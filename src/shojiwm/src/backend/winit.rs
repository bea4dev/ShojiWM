use std::hash::{Hash, Hasher};
use std::time::{Duration, Instant};

use smithay::{
    backend::{
        renderer::{
            ImportEgl, ImportMemWl, Texture,
            damage::OutputDamageTracker,
            element::Element,
            element::solid::SolidColorRenderElement,
            element::surface::WaylandSurfaceRenderElement,
            element::texture::TextureRenderElement,
            element::utils::{Relocate, RelocateRenderElement, RescaleRenderElement},
            gles::{GlesRenderer, GlesTexture},
        },
        winit::{self, WinitEvent},
    },
    desktop::{WindowSurface, layer_map_for_output},
    output::{Mode, Output, PhysicalProperties, Subpixel},
    reexports::calloop::EventLoop,
    reexports::wayland_protocols::wp::presentation_time::server::wp_presentation_feedback,
    reexports::wayland_server::Resource,
    utils::{Logical, Monotonic, Point, Rectangle, Transform},
    wayland::{background_effect::BackgroundEffectSurfaceCachedState, compositor},
};
use tracing::{info, trace, warn};

use crate::{
    ShojiWM,
    backend::visual::{
        WindowVisualState, is_identity_visual_geometry, requires_full_window_snapshot,
        root_physical_origin, transformed_root_rect, window_visual_state,
    },
    backend::{damage, damage_blink, decoration, snapshot, window as window_render},
    presentation::{take_presentation_feedback, update_primary_scanout_output},
};
use smithay::wayland::presentation::Refresh;

fn manual_invalidate_debug_enabled() -> bool {
    std::env::var_os("SHOJI_MANUAL_INVALIDATE_DEBUG")
        .is_some_and(|value| value != "0" && !value.is_empty())
}

fn animation_timing_debug_enabled() -> bool {
    std::env::var_os("SHOJI_ANIMATION_TIMING_DEBUG")
        .is_some_and(|value| value != "0" && !value.is_empty())
}

fn animation_spike_threshold_ms() -> f64 {
    std::env::var("SHOJI_ANIMATION_SPIKE_THRESHOLD_MS")
        .ok()
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|value| *value > 0.0)
        .unwrap_or(12.0)
}

fn clipped_transform_debug_enabled() -> bool {
    std::env::var_os("SHOJI_CLIPPED_TRANSFORM_DEBUG")
        .is_some_and(|value| value != "0" && !value.is_empty())
}

#[derive(Default)]
struct WinitAnimationTimingMetrics {
    render_element_count: usize,
    transform_snapshot_window_count: usize,
    closing_snapshot_count: usize,
    scene_build_elapsed_ms: f64,
    render_elapsed_ms: f64,
}

fn capture_scene_texture_for_effect(
    renderer: &mut GlesRenderer,
    source: &'static str,
    capture_geo: Rectangle<i32, Logical>,
    scale: smithay::utils::Scale<f64>,
    scene: &[WinitRenderElements],
) -> Option<GlesTexture> {
    if scene.is_empty() {
        return None;
    }
    let mut tracker = smithay::backend::renderer::damage::OutputDamageTracker::new(
        (0, 0),
        1.0,
        smithay::utils::Transform::Normal,
    );
    let capture_size = crate::backend::visual::logical_size_to_physical_buffer_size(
        capture_geo.size.w,
        capture_geo.size.h,
        scale,
    );
    crate::backend::shader_effect::record_snapshot_fallback(source, capture_size, scene.len());
    crate::backend::shader_effect::with_gpu_timing_renderer_span(
        renderer,
        "backdrop-scene-capture",
        capture_size,
        |renderer| {
            renderer.with_deferred_frame_flushes(|renderer| {
                snapshot::capture_snapshot(
                    renderer,
                    None,
                    &mut tracker,
                    crate::ssd::LogicalRect::new(
                        capture_geo.loc.x,
                        capture_geo.loc.y,
                        capture_geo.size.w,
                        capture_geo.size.h,
                    ),
                    0,
                    true,
                    scale,
                    scene,
                )
            })
        },
    )
    .ok()
    .flatten()
    .map(|snapshot| snapshot.texture)
}

fn capture_snapshot_from_output_elements(
    renderer: &mut GlesRenderer,
    output_geo: Rectangle<i32, Logical>,
    rect: crate::ssd::LogicalRect,
    scale: smithay::utils::Scale<f64>,
    existing: Option<crate::backend::snapshot::LiveWindowSnapshot>,
    tracker: &mut smithay::backend::renderer::damage::OutputDamageTracker,
    elements: &[WinitRenderElements],
) -> Result<
    Option<crate::backend::snapshot::LiveWindowSnapshot>,
    smithay::backend::renderer::gles::GlesError,
> {
    let capture_origin: Point<i32, smithay::utils::Physical> = (Point::from((rect.x, rect.y))
        - output_geo.loc)
        .to_f64()
        .to_physical_precise_round(scale);
    let relocated = elements
        .iter()
        .map(|element| {
            RelocateRenderElement::from_element(
                element,
                Point::from((-capture_origin.x, -capture_origin.y)),
                Relocate::Relative,
            )
        })
        .collect::<Vec<_>>();
    snapshot::capture_snapshot(
        renderer, existing, tracker, rect, 0, true, scale, &relocated,
    )
}

fn layer_surface_logical_rect(
    output: &Output,
    layer_surface: &smithay::desktop::LayerSurface,
) -> Option<crate::ssd::LogicalRect> {
    let map = layer_map_for_output(output);
    let geometry = map.layer_geometry(layer_surface)?;
    let output_location = output.current_location();
    Some(crate::ssd::LogicalRect::new(
        output_location.x + geometry.loc.x,
        output_location.y + geometry.loc.y,
        geometry.size.w,
        geometry.size.h,
    ))
}

fn expand_effect_rect(
    rect: crate::ssd::LogicalRect,
    outsets: crate::ssd::EffectOutsets,
) -> crate::ssd::LogicalRect {
    crate::ssd::LogicalRect::new(
        rect.x - outsets.left,
        rect.y - outsets.top,
        rect.width + outsets.left + outsets.right,
        rect.height + outsets.top + outsets.bottom,
    )
}

fn layer_source_effect_element(
    renderer: &mut GlesRenderer,
    output: &Output,
    output_geo: Rectangle<i32, Logical>,
    scale: smithay::utils::Scale<f64>,
    layer_id: &str,
    placement: &'static str,
    layer_rect: crate::ssd::LogicalRect,
    effect: &crate::ssd::WindowEffectSlot,
    source_elements: &[WinitRenderElements],
    cache: &mut std::collections::HashMap<
        String,
        crate::backend::shader_effect::WindowEffectElementState,
    >,
) -> Vec<WinitRenderElements> {
    if !matches!(effect.effect.input, crate::ssd::EffectInput::LayerSource(_))
        || source_elements.is_empty()
    {
        return Vec::new();
    }
    let rect = expand_effect_rect(layer_rect, effect.outsets);
    if rect.width <= 0 || rect.height <= 0 {
        return Vec::new();
    }
    let logical = Rectangle::new(
        Point::from((rect.x, rect.y)),
        (rect.width, rect.height).into(),
    );
    if logical.intersection(output_geo).is_none() {
        return Vec::new();
    }

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    placement.hash(&mut hasher);
    layer_id.hash(&mut hasher);
    output.name().hash(&mut hasher);
    format!("{:?}", effect).hash(&mut hasher);
    scale.x.to_bits().hash(&mut hasher);
    scale.y.to_bits().hash(&mut hasher);
    crate::backend::snapshot::render_element_scene_signature(source_elements, scale)
        .hash(&mut hasher);
    let signature = hasher.finish();
    let state = cache
        .entry(format!("{}@{}@{}", layer_id, placement, output.name()))
        .or_default();
    if state.signature != signature {
        state.signature = signature;
        state.commit_counter.increment();
    }

    let mut tracker = OutputDamageTracker::new((0, 0), 1.0, Transform::Normal);
    let Ok(Some(source)) = capture_snapshot_from_output_elements(
        renderer,
        output_geo,
        rect,
        scale,
        None,
        &mut tracker,
        source_elements,
    ) else {
        return Vec::new();
    };
    let texture_size = source.texture.size();
    let Ok(texture) = crate::backend::shader_effect::apply_effect_pipeline_cached_for_key(
        renderer,
        format!(
            "winit:layer-effect:{}:{}:{}",
            output.name(),
            layer_id,
            placement
        ),
        source.texture,
        None,
        (texture_size.w, texture_size.h),
        None,
        Some((texture_size.w, texture_size.h)),
        &effect.effect,
    ) else {
        return Vec::new();
    };
    let texture_size = texture.size();
    let capture_origin: Point<i32, smithay::utils::Physical> = (Point::from((rect.x, rect.y))
        - output_geo.loc)
        .to_f64()
        .to_physical_precise_round(scale);
    let geometry = Rectangle::new(capture_origin, (texture_size.w, texture_size.h).into());
    crate::backend::shader_effect::backdrop_shader_element_with_geometry(
        renderer,
        state.id.clone(),
        state.commit_counter,
        texture,
        logical,
        geometry,
        logical,
        logical,
        &effect.effect,
        1.0,
        scale.x as f32,
        [0.0, 0.0],
        None,
        0.0,
        format!("layer-effect:{}:{}:{}", placement, output.name(), layer_id),
    )
    .ok()
    .map(WinitRenderElements::Backdrop)
    .into_iter()
    .collect()
}

/// Popup counterpart of `layer_source_effect_element`: captures the popup's
/// own elements and runs the slot's pipeline (popupSource input).
fn popup_source_effect_element(
    renderer: &mut GlesRenderer,
    output: &Output,
    output_geo: Rectangle<i32, Logical>,
    scale: smithay::utils::Scale<f64>,
    popup_id: &str,
    placement: &'static str,
    popup_rect: crate::ssd::LogicalRect,
    effect: &crate::ssd::WindowEffectSlot,
    source_elements: &[WinitRenderElements],
    cache: &mut std::collections::HashMap<
        String,
        crate::backend::shader_effect::WindowEffectElementState,
    >,
) -> Vec<WinitRenderElements> {
    if !matches!(effect.effect.input, crate::ssd::EffectInput::PopupSource(_))
        || source_elements.is_empty()
    {
        return Vec::new();
    }
    let rect = expand_effect_rect(popup_rect, effect.outsets);
    if rect.width <= 0 || rect.height <= 0 {
        return Vec::new();
    }
    let logical = Rectangle::new(
        Point::from((rect.x, rect.y)),
        (rect.width, rect.height).into(),
    );
    if logical.intersection(output_geo).is_none() {
        return Vec::new();
    }

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    placement.hash(&mut hasher);
    popup_id.hash(&mut hasher);
    output.name().hash(&mut hasher);
    format!("{:?}", effect).hash(&mut hasher);
    scale.x.to_bits().hash(&mut hasher);
    scale.y.to_bits().hash(&mut hasher);
    crate::backend::snapshot::render_element_scene_signature(source_elements, scale)
        .hash(&mut hasher);
    let signature = hasher.finish();
    let state = cache
        .entry(format!("{}@{}@{}", popup_id, placement, output.name()))
        .or_default();
    if state.signature != signature {
        state.signature = signature;
        state.commit_counter.increment();
    }

    let mut tracker = OutputDamageTracker::new((0, 0), 1.0, Transform::Normal);
    let Ok(Some(source)) = capture_snapshot_from_output_elements(
        renderer,
        output_geo,
        rect,
        scale,
        None,
        &mut tracker,
        source_elements,
    ) else {
        return Vec::new();
    };
    let texture_size = source.texture.size();
    let Ok(texture) = crate::backend::shader_effect::apply_effect_pipeline_cached_for_key(
        renderer,
        format!(
            "winit:popup-effect:{}:{}:{}",
            output.name(),
            popup_id,
            placement
        ),
        source.texture,
        None,
        (texture_size.w, texture_size.h),
        None,
        Some((texture_size.w, texture_size.h)),
        &effect.effect,
    ) else {
        return Vec::new();
    };
    let texture_size = texture.size();
    let capture_origin: Point<i32, smithay::utils::Physical> = (Point::from((rect.x, rect.y))
        - output_geo.loc)
        .to_f64()
        .to_physical_precise_round(scale);
    let geometry = Rectangle::new(capture_origin, (texture_size.w, texture_size.h).into());
    crate::backend::shader_effect::backdrop_shader_element_with_geometry(
        renderer,
        state.id.clone(),
        state.commit_counter,
        texture,
        logical,
        geometry,
        logical,
        logical,
        &effect.effect,
        1.0,
        scale.x as f32,
        [0.0, 0.0],
        None,
        0.0,
        format!("popup-effect:{}:{}:{}", placement, output.name(), popup_id),
    )
    .ok()
    .map(WinitRenderElements::Backdrop)
    .into_iter()
    .collect()
}

fn compose_layer_source_effects(
    renderer: &mut GlesRenderer,
    output: &Output,
    output_geo: Rectangle<i32, Logical>,
    scale: smithay::utils::Scale<f64>,
    layer_id: &str,
    layer_rect: crate::ssd::LogicalRect,
    effects: &crate::ssd::WindowEffectConfig,
    // Shown when no `replace` slot is active. Excludes the layer's popups:
    // those are displayed (and effect-composed) separately.
    display_elements: Vec<WinitRenderElements>,
    // What layerSource() captures sample. Includes popups ("full" semantics).
    capture_elements: &[WinitRenderElements],
    cache: &mut std::collections::HashMap<
        String,
        crate::backend::shader_effect::WindowEffectElementState,
    >,
) -> Vec<WinitRenderElements> {
    let mut render = |placement, effect| {
        layer_source_effect_element(
            renderer,
            output,
            output_geo,
            scale,
            layer_id,
            placement,
            layer_rect,
            effect,
            capture_elements,
            cache,
        )
    };
    let in_front = effects
        .in_front
        .as_ref()
        .map(|effect| render("layer-in-front", effect))
        .unwrap_or_default();
    let replacement = effects
        .replace
        .as_ref()
        .map(|effect| render("layer-replace", effect))
        .unwrap_or_default();
    let behind_root = effects
        .behind_root_surface
        .as_ref()
        .map(|effect| render("layer-behind-root-surface", effect))
        .unwrap_or_default();
    let behind = effects
        .behind
        .as_ref()
        .map(|effect| render("layer-behind", effect))
        .unwrap_or_default();

    let mut elements = Vec::new();
    elements.extend(in_front);
    if replacement.is_empty() {
        elements.extend(display_elements);
    } else {
        elements.extend(replacement);
    }
    elements.extend(behind_root);
    elements.extend(behind);
    elements
}

/// Display elements for all popups of a layer surface, each composed with its
/// configured popup effects. See the tty counterpart for details.
fn composed_popup_scene_elements(
    renderer: &mut GlesRenderer,
    output: &Output,
    output_geo: Rectangle<i32, Logical>,
    scale: smithay::utils::Scale<f64>,
    layer_surface: &smithay::desktop::LayerSurface,
    configured_popup_effects: &std::collections::HashMap<String, crate::ssd::WindowEffectConfig>,
    surface_policies: &std::collections::HashMap<String, crate::ssd::SurfacePolicy>,
    popup_effect_cache: &mut std::collections::HashMap<
        String,
        crate::backend::shader_effect::WindowEffectElementState,
    >,
    popup_framebuffer_effect_states: &mut std::collections::HashMap<
        String,
        crate::backend::shader_effect::ShaderEffectElementState,
    >,
) -> Vec<WinitRenderElements> {
    let groups =
        window_render::layer_surface_popup_groups(renderer, output, layer_surface, scale, 1.0);
    if groups.is_empty() {
        return Vec::new();
    }
    let output_loc = output.current_location();
    let mut elements = Vec::new();
    for (popup_id, local_rect, raw_elements) in groups {
        // Surface policy applies to the popup's raw surface elements whether
        // or not an effect is configured for it.
        let ignore_opaque = matches!(
            surface_policies
                .get(&popup_id)
                .map(|policy| policy.opaque_region),
            Some(crate::ssd::OpaqueRegionPolicy::Ignore)
        );
        let popup_elements = raw_elements
            .into_iter()
            .map(|element| {
                if ignore_opaque {
                    WinitRenderElements::PolicyWindow(
                        crate::backend::visual::IgnoredOpaqueRegionElement::from_element(element),
                    )
                } else {
                    WinitRenderElements::Window(element)
                }
            })
            .collect::<Vec<_>>();
        let Some(effects) = configured_popup_effects.get(&popup_id) else {
            elements.extend(popup_elements);
            continue;
        };
        let popup_rect = crate::ssd::LogicalRect::new(
            output_loc.x + local_rect.x,
            output_loc.y + local_rect.y,
            local_rect.width,
            local_rect.height,
        );
        elements.extend(compose_one_popup_elements(
            renderer,
            output,
            output_geo,
            scale,
            &popup_id,
            popup_rect,
            effects,
            popup_elements,
            popup_effect_cache,
            popup_framebuffer_effect_states,
        ));
    }
    elements
}

/// Winit counterpart of the TTY layer-popup pass. Layer-shell popup surfaces
/// need their own front-most pass so a Bottom-layer Waybar tooltip is not
/// occluded by ordinary toplevel windows.
fn layer_popup_scene_elements(
    renderer: &mut GlesRenderer,
    output: &Output,
    output_geo: Rectangle<i32, Logical>,
    scale: smithay::utils::Scale<f64>,
    overlay_only: bool,
    configured_popup_effects: &std::collections::HashMap<String, crate::ssd::WindowEffectConfig>,
    surface_policies: &std::collections::HashMap<String, crate::ssd::SurfacePolicy>,
    popup_effect_cache: &mut std::collections::HashMap<
        String,
        crate::backend::shader_effect::WindowEffectElementState,
    >,
    popup_framebuffer_effect_states: &mut std::collections::HashMap<
        String,
        crate::backend::shader_effect::ShaderEffectElementState,
    >,
) -> Vec<WinitRenderElements> {
    crate::backend::window::layer_surfaces_for_popup_pass(output, overlay_only)
        .into_iter()
        .flat_map(|layer_surface| {
            composed_popup_scene_elements(
                renderer,
                output,
                output_geo,
                scale,
                &layer_surface,
                configured_popup_effects,
                surface_policies,
                popup_effect_cache,
                popup_framebuffer_effect_states,
            )
        })
        .collect()
}

/// Display elements for all popups of a toplevel window, each composed with
/// its configured popup effects. Only used while the window's visual
/// transform is identity (see the tty counterpart).
fn composed_window_popup_scene_elements(
    renderer: &mut GlesRenderer,
    output: &Output,
    output_geo: Rectangle<i32, Logical>,
    scale: smithay::utils::Scale<f64>,
    window: &smithay::desktop::Window,
    location: Point<i32, smithay::utils::Physical>,
    alpha: f32,
    configured_popup_effects: &std::collections::HashMap<String, crate::ssd::WindowEffectConfig>,
    popup_effect_cache: &mut std::collections::HashMap<
        String,
        crate::backend::shader_effect::WindowEffectElementState,
    >,
    popup_framebuffer_effect_states: &mut std::collections::HashMap<
        String,
        crate::backend::shader_effect::ShaderEffectElementState,
    >,
) -> Vec<WinitRenderElements> {
    let groups =
        window_render::window_popup_groups(window, renderer, location, output_geo, scale, alpha);
    if groups.is_empty() {
        return Vec::new();
    }
    let mut elements = Vec::new();
    for (popup_id, popup_rect, raw_elements) in groups {
        let popup_elements = raw_elements
            .into_iter()
            .map(WinitRenderElements::Window)
            .collect::<Vec<_>>();
        let Some(effects) = configured_popup_effects.get(&popup_id) else {
            elements.extend(popup_elements);
            continue;
        };
        elements.extend(compose_one_popup_elements(
            renderer,
            output,
            output_geo,
            scale,
            &popup_id,
            popup_rect,
            effects,
            popup_elements,
            popup_effect_cache,
            popup_framebuffer_effect_states,
        ));
    }
    elements
}

/// Compose a single popup's display elements with its assigned effects.
/// Shared by the layer-popup and window-popup paths.
fn compose_one_popup_elements(
    renderer: &mut GlesRenderer,
    output: &Output,
    output_geo: Rectangle<i32, Logical>,
    scale: smithay::utils::Scale<f64>,
    popup_id: &str,
    popup_rect: crate::ssd::LogicalRect,
    effects: &crate::ssd::WindowEffectConfig,
    popup_elements: Vec<WinitRenderElements>,
    popup_effect_cache: &mut std::collections::HashMap<
        String,
        crate::backend::shader_effect::WindowEffectElementState,
    >,
    popup_framebuffer_effect_states: &mut std::collections::HashMap<
        String,
        crate::backend::shader_effect::ShaderEffectElementState,
    >,
) -> Vec<WinitRenderElements> {
    {
        let mut render_slot = |placement: &'static str,
                               effect: &crate::ssd::WindowEffectSlot|
         -> Vec<WinitRenderElements> {
            if !matches!(effect.effect.input, crate::ssd::EffectInput::PopupSource(_)) {
                return Vec::new();
            }
            popup_source_effect_element(
                renderer,
                output,
                output_geo,
                scale,
                popup_id,
                placement,
                popup_rect,
                effect,
                &popup_elements,
                popup_effect_cache,
            )
        };

        let in_front = effects
            .in_front
            .as_ref()
            .map(|effect| render_slot("popup-in-front", effect))
            .unwrap_or_default();
        let replacement = effects
            .replace
            .as_ref()
            .map(|effect| render_slot("popup-replace", effect))
            .unwrap_or_default();
        let behind_root = effects
            .behind_root_surface
            .as_ref()
            .map(|effect| render_slot("popup-behind-root-surface", effect))
            .unwrap_or_default();
        let behind_popup_source = effects
            .behind
            .as_ref()
            .filter(|effect| matches!(effect.effect.input, crate::ssd::EffectInput::PopupSource(_)))
            .map(|effect| render_slot("popup-behind", effect))
            .unwrap_or_default();
        // Backdrop-input behind effects resolve from the framebuffer below the
        // popup at draw time (no offline capture path for popups).
        let behind_backdrop = effects
            .behind
            .as_ref()
            .filter(|effect| {
                !matches!(effect.effect.input, crate::ssd::EffectInput::PopupSource(_))
            })
            .filter(|effect| effect.effect.supports_popup_framebuffer_backdrop())
            .and_then(|effect| {
                let rect = expand_effect_rect(popup_rect, effect.outsets);
                let stable_key =
                    format!("{}@popup-behind-framebuffer@{}", popup_id, output.name());
                let popup_source = if effect.effect.uses_popup_source_input() {
                    let mut tracker = OutputDamageTracker::new((0, 0), 1.0, Transform::Normal);
                    capture_snapshot_from_output_elements(
                        renderer,
                        output_geo,
                        rect,
                        scale,
                        None,
                        &mut tracker,
                        &popup_elements,
                    )
                    .ok()
                    .flatten()
                    .map(|snapshot| snapshot.texture)
                } else {
                    None
                };
                if effect.effect.uses_popup_source_input() && popup_source.is_none() {
                    return None;
                }
                crate::backend::shader_effect::framebuffer_backdrop_element_for_output_rects_with_popup_source(
                    renderer,
                    popup_framebuffer_effect_states
                        .entry(stable_key)
                        .or_default(),
                    &[rect],
                    effect.effect.clone(),
                    output_geo,
                    scale,
                    1.0,
                    popup_source,
                )
                .ok()
                .flatten()
            })
            .map(|element| {
                vec![WinitRenderElements::Decoration(
                    decoration::DecorationSceneElements::Backdrop(element),
                )]
            })
            .unwrap_or_default();

        let mut elements = Vec::new();
        elements.extend(in_front);
        if replacement.is_empty() {
            elements.extend(popup_elements);
        } else {
            elements.extend(replacement);
        }
        elements.extend(behind_root);
        elements.extend(behind_popup_source);
        elements.extend(behind_backdrop);
        elements
    }
}

pub fn init_winit(
    event_loop: &mut EventLoop<ShojiWM>,
    state: &mut ShojiWM,
) -> Result<(), Box<dyn std::error::Error>> {
    let (mut backend, winit) = winit::init::<GlesRenderer>()?;
    match backend.renderer().bind_wl_display(&state.display_handle) {
        Ok(()) => trace!("winit renderer bound wl_display for EGL clients"),
        Err(error) => warn!(?error, "failed to bind wl_display for winit EGL clients"),
    }
    state
        .shm_state
        .update_formats(backend.renderer().shm_formats());

    let mode = Mode {
        size: backend.window_size(),
        refresh: 60_000,
    };

    let output = Output::new(
        "winit".to_string(),
        PhysicalProperties {
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: "Smithay".into(),
            model: "Winit".into(),
            serial_number: "Unknown".into(),
        },
    );
    output.change_current_state(
        Some(mode),
        Some(Transform::Flipped180),
        None,
        Some((0, 0).into()),
    );
    output.set_preferred(mode);
    state.seed_xwayland_refresh_override_from_output(&output, "winit-output-connected");
    let _global = state.create_output_global(&output);

    state.space.map_output(&output, (0, 0));
    state.warp_cursor_to_output_center(&output);

    let mut damage_tracker = OutputDamageTracker::from_output(&output);
    let mut blink_damage_tracker = OutputDamageTracker::from_output(&output);
    event_loop
        .handle()
        .insert_source(winit, move |event, _, state| {
            match event {
                WinitEvent::Resized { size, .. } => {
                    output.change_current_state(
                        Some(Mode {
                            size,
                            refresh: 60_000,
                        }),
                        None,
                        None,
                        None,
                    );
                }
                WinitEvent::Input(event) => state.process_input_event(event),
                WinitEvent::Redraw => {
                    timescope::scope!("winit redraw");
                    let redraw_started_at = Instant::now();
                    let spike_threshold_ms = animation_spike_threshold_ms();
                    let decorations_refresh_started_at = Instant::now();
                    {
                        timescope::scope!("winit refresh_window_decorations");
                        if let Err(err) =
                            state.refresh_window_decorations_for_output(Some(output.name().as_str()))
                        {
                            warn!(error = ?err, "failed to refresh window decorations for winit");
                        }
                    }
                    let decorations_refresh_elapsed_ms =
                        decorations_refresh_started_at.elapsed().as_secs_f64() * 1000.0;
                    let layer_effects_started_at = Instant::now();
                    {
                        timescope::scope!("winit refresh_layer_and_popup_effects");
                        if let Err(err) =
                            state.refresh_layer_effects_for_output(output.name().as_str())
                        {
                            warn!(error = ?err, "failed to refresh layer effects for winit");
                        }
                        if let Err(err) =
                            state.refresh_popup_effects_for_output(output.name().as_str())
                        {
                            warn!(error = ?err, "failed to refresh popup effects for winit");
                        }
                    }
                    let layer_effects_elapsed_ms =
                        layer_effects_started_at.elapsed().as_secs_f64() * 1000.0;

                    let size = backend.window_size();
                    let damage = Rectangle::from_size(size);

                    let mut should_submit_frame = false;
                    let mut timing = WinitAnimationTimingMetrics::default();
                    {
                        timescope::scope!("winit scene build");
                        let scene_build_started_at = Instant::now();
                        let (renderer, mut framebuffer) = backend.bind().unwrap();
                        let output_geo = state.space.output_geometry(&output).unwrap();
                        let scale =
                            smithay::utils::Scale::from(output.current_scale().fractional_scale());
                        let windows_top_to_bottom: Vec<_> = state
                            .windows_for_output_top_to_bottom(&output)
                            .into_iter()
                            .cloned()
                            .collect();
                        let mut extra_damage = state.pending_decoration_damage.clone();
                        if state.force_full_damage {
                            extra_damage.push(crate::ssd::LogicalRect::new(
                                output_geo.loc.x,
                                output_geo.loc.y,
                                output_geo.size.w,
                                output_geo.size.h,
                            ));
                        }
                        let (_, _lower_layer_elements) =
                            window_render::layer_elements_for_output(renderer, &output, scale, 1.0);

                        // Fullscreen fast path (mirrors tty): collapse the
                        // scene to "overlay layers above one raw client
                        // surface". Winit has no DRM planes so no scanout,
                        // but stacking policy and perf behavior stay aligned.
                        let fullscreen_scanout = crate::backend::tty::fullscreen_scanout_window(
                            &state.space,
                            &state.window_decorations,
                            &windows_top_to_bottom,
                            state.closing_window_snapshots.len(),
                            &output,
                            output_geo,
                            scale,
                        );
                        let fullscreen_backdrop_windows: Vec<smithay::desktop::Window>;
                        let upper_layer_backdrop_windows: &[smithay::desktop::Window] =
                            if let Some(window) = fullscreen_scanout.as_ref() {
                                fullscreen_backdrop_windows = vec![window.clone()];
                                &fullscreen_backdrop_windows
                            } else {
                                &windows_top_to_bottom
                            };

                        let mut scene_elements: Vec<WinitRenderElements> = Vec::new();
                        scene_elements.extend(upper_layer_scene_elements(
                            renderer,
                            state,
                            &output,
                            output_geo,
                            scale,
                            upper_layer_backdrop_windows,
                            fullscreen_scanout.is_some(),
                        ));
                        scene_elements.extend(
                            closing_snapshot_elements(renderer, state, &output, scale)
                                .into_iter(),
                        );
                        for (_window_index, window) in windows_top_to_bottom.iter().enumerate() {
                            // Fullscreen fast path: everything but the
                            // fullscreen window is occluded; render it as a
                            // bare surface tree + popups.
                            if let Some(fullscreen_window) = fullscreen_scanout.as_ref() {
                                if window != fullscreen_window {
                                    continue;
                                }
                                let Some(window_location) =
                                    state.space.element_location(window)
                                else {
                                    continue;
                                };
                                let physical_location = (window_location - output_geo.loc)
                                    .to_physical_precise_round(scale);
                                scene_elements.extend(
                                    window_render::popup_elements(
                                        window,
                                        renderer,
                                        physical_location,
                                        scale,
                                        1.0,
                                    )
                                    .into_iter()
                                    .map(WinitRenderElements::Window),
                                );
                                scene_elements.extend(
                                    window_render::surface_elements(
                                        window,
                                        renderer,
                                        physical_location,
                                        scale,
                                        1.0,
                                    )
                                    .into_iter()
                                    .map(WinitRenderElements::Window),
                                );
                                continue;
                            }
                            let Some(window_location) = state.space.element_location(window) else {
                                continue;
                            };
                            let Some(window_id) = state
                                .window_decorations
                                .get(window)
                                .map(|decoration| decoration.snapshot.id.clone())
                            else {
                                continue;
                            };
                            if state
                                .window_decorations
                                .get(window)
                                .is_some_and(|decoration| {
                                    !decoration
                                        .managed_window_allows_render_on_output(output.name().as_str())
                                })
                            {
                                continue;
                            }
                            if state.closing_window_snapshots.contains_key(&window_id) {
                                continue;
                            }
                            let preliminary_physical_location =
                                (window_location - output_geo.loc).to_physical_precise_round(scale);
                            let visual_state = state
                                .window_decorations
                                .get(window)
                                .map(|decoration| {
                                    window_visual_state(
                                        decoration.layout.root.rect,
                                        decoration.visual_transform,
                                        output_geo,
                                        scale,
                                    )
                                })
                                .unwrap_or(WindowVisualState {
                                    origin: preliminary_physical_location,
                                    scale: smithay::utils::Scale::from((1.0, 1.0)),
                                    translation: (0, 0).into(),
                                    opacity: 1.0,
                                });
                            let snap_scale = smithay::utils::Scale::from((
                                scale.x * visual_state.scale.x.max(0.0),
                                scale.y * visual_state.scale.y.max(0.0),
                            ));
                            let client_physical_geometry = state
                                .window_decorations
                                .get(window)
                                .and_then(|decoration| {
                                    decoration.content_clip.map(|clip| {
                                        let root_origin = root_physical_origin(
                                            decoration.layout.root.rect,
                                            output_geo,
                                            scale,
                                        );
                                        let local_geometry =
                                            crate::backend::visual::relative_physical_rect_from_root_precise(
                                                clip.rect_precise,
                                                decoration.layout.root.rect,
                                                output_geo,
                                                scale,
                                            );
                                        smithay::utils::Rectangle::new(
                                            smithay::utils::Point::from((
                                                root_origin.x + local_geometry.loc.x,
                                                root_origin.y + local_geometry.loc.y,
                                            )),
                                            local_geometry.size,
                                        )
                                    })
                                });
                            let physical_location = client_physical_geometry
                                .map(|geometry| geometry.loc)
                                .unwrap_or(preliminary_physical_location);
                            let direct_surface_count = window_render::surface_elements(
                                window,
                                renderer,
                                physical_location,
                                scale,
                                1.0,
                            )
                            .len();
                            if direct_surface_count == 0 {
                                if let Some(decoration) =
                                    state.window_decorations.get(window).cloned()
                                {
                                    let now_ms = Duration::from(state.clock.now()).as_millis() as u64;
                                    if state
                                        .promote_window_to_closing_snapshot(
                                            &window_id,
                                            &decoration,
                                            now_ms,
                                        )
                                        .unwrap_or(false)
                                    {
                                        continue;
                                    }
                                }
                                continue;
                            }
                            let has_backdrop_source = direct_surface_count > 0
                                || state.live_window_snapshots.contains_key(&window_id)
                                || state.complete_window_snapshots.contains_key(&window_id);
                            let decoration_ready =
                                state.windows_ready_for_decoration.contains(&window_id);
                            if !has_backdrop_source {
                                continue;
                            }
                            let use_full_window_snapshot =
                                requires_full_window_snapshot(visual_state);
                            let used_transform_snapshot_last_frame = state
                                .transform_snapshot_window_ids
                                .contains(&window_id);
                            let snapshot_id = state
                                .window_decorations
                                .get(window)
                                .map(|decoration| decoration.snapshot.id.clone());
                            let window_has_snapshot_damage = snapshot_id.as_ref().is_some_and(
                                |snapshot_id| {
                                    state.snapshot_dirty_window_ids.contains(snapshot_id)
                                },
                            );
                            if ((use_full_window_snapshot != used_transform_snapshot_last_frame)
                                || (use_full_window_snapshot && window_has_snapshot_damage))
                                && let Some(decoration) = state.window_decorations.get(window)
                            {
                                extra_damage.push(transformed_root_rect(
                                    decoration.layout.root.rect,
                                    decoration.visual_transform,
                                ));
                            }
                            if use_full_window_snapshot {
                                state
                                    .transform_snapshot_window_ids
                                    .insert(window_id.clone());
                            } else {
                                state.transform_snapshot_window_ids.remove(&window_id);
                                state.complete_window_snapshot_trackers.remove(&window_id);
                            }
                            let composition_visual = if use_full_window_snapshot {
                                WindowVisualState {
                                    origin: Point::from((0, 0)),
                                    scale: smithay::utils::Scale::from((1.0, 1.0)),
                                    translation: (0, 0).into(),
                                    opacity: 1.0,
                                }
                            } else {
                                visual_state
                            };
                            let root_origin = state
                                .window_decorations
                                .get(window)
                                .map(|decoration| root_physical_origin(decoration.layout.root.rect, output_geo, scale));
                            let mut ordered_ui_elements: Vec<(usize, WinitRenderElements)> = Vec::new();
                            let mut ordered_backdrop_elements: Vec<(usize, WinitRenderElements)> =
                                Vec::new();
                            if decoration_ready {
                                let mut backdrop_items = backdrop_shader_elements_for_window(
                                    renderer,
                                    state,
                                    &output,
                                    output_geo,
                                    scale,
                                    &windows_top_to_bottom,
                                    _window_index,
                                    window,
                                    if use_full_window_snapshot {
                                        1.0
                                    } else {
                                        visual_state.opacity
                                    },
                                    decoration_ready,
                                    false,
                                    !use_full_window_snapshot,
                                );
                                let use_configured_framebuffer_backdrop = !use_full_window_snapshot
                                    && state
                                        .configured_background_effect
                                        .as_ref()
                                        .is_some_and(|config| {
                                            config.effect.supports_framebuffer_backdrop()
                                        });
                                if !use_configured_framebuffer_backdrop {
                                    backdrop_items.extend(
                                        configured_background_effect_elements_for_window(
                                            renderer,
                                            state,
                                            &output,
                                            output_geo,
                                            scale,
                                            &windows_top_to_bottom,
                                            _window_index,
                                            window,
                                            if use_full_window_snapshot {
                                                1.0
                                            } else {
                                                visual_state.opacity
                                            },
                                            false,
                                        )
                                        .into_iter()
                                        .map(|(order, element)| (order, element, true)),
                                    );
                                }
                                for (order, element, render_as_backdrop) in backdrop_items.drain(..) {
                                    if let Some(root_origin) = root_origin {
                                        let transformed = transform_backdrop_elements(
                                            vec![element],
                                            root_origin,
                                            composition_visual,
                                        )
                                        .into_iter()
                                        .map(|item| (order, item));
                                        if render_as_backdrop {
                                            ordered_backdrop_elements.extend(transformed);
                                        } else {
                                            ordered_ui_elements.extend(transformed);
                                        }
                                    }
                                }
                                if use_configured_framebuffer_backdrop {
                                    for (order, element) in
                                        configured_background_framebuffer_effect_elements_for_window(
                                            renderer,
                                            state,
                                            window,
                                            output_geo,
                                            scale,
                                            visual_state.opacity,
                                        )
                                    {
                                        if let Some(root_origin) = root_origin {
                                            ordered_backdrop_elements.extend(
                                                transform_decoration_elements(
                                                    vec![decoration::DecorationSceneElements::Backdrop(element)],
                                                    root_origin,
                                                    composition_visual,
                                                )
                                                .into_iter()
                                                .map(|item| (order, item)),
                                            );
                                        }
                                    }
                                }
                                if let Some(decoration_state) =
                                    state.window_decorations.get_mut(window)
                                {
                                    let mut background_items = decoration::ordered_background_elements_for_window_with_framebuffer_backdrops(
                                        renderer,
                                        decoration_state,
                                        output_geo,
                                        if use_full_window_snapshot { scale } else { snap_scale },
                                        if use_full_window_snapshot {
                                            1.0
                                        } else {
                                            visual_state.opacity
                                        },
                                        !use_full_window_snapshot,
                                    )
                                    .inspect_err(|error| {
                                        warn!(?error, "failed to build decoration background elements");
                                    })
                                    .unwrap_or_default();
                                    background_items.sort_by_key(|(order, _)| *order);
                                    for (order, element) in background_items {
                                        if let Some(root_origin) = root_origin {
                                            let render_as_backdrop = matches!(
                                                element,
                                                decoration::DecorationSceneElements::Backdrop(_)
                                            );
                                            let transformed =
                                                transform_decoration_elements(vec![element], root_origin, composition_visual)
                                                    .into_iter()
                                                    .map(|item| (order, item));
                                            if render_as_backdrop {
                                                ordered_backdrop_elements.extend(transformed);
                                            } else {
                                                ordered_ui_elements.extend(transformed);
                                            }
                                        }
                                    }
                                }

                                for (order, element) in decoration::ordered_icon_elements_for_window(
                                    renderer,
                                    &state.space,
                                    &state.window_decorations,
                                    &output,
                                    window,
                                    if use_full_window_snapshot {
                                        1.0
                                    } else {
                                        visual_state.opacity
                                    },
                                )
                                .unwrap_or_default()
                                {
                                    if let Some(root_origin) = root_origin {
                                        ordered_ui_elements.extend(
                                            transform_text_elements(vec![element], root_origin, composition_visual)
                                                .into_iter()
                                                .map(|item| (order, item)),
                                        );
                                    }
                                }

                                for (order, element) in decoration::ordered_text_elements_for_window(
                                    renderer,
                                    &state.space,
                                    &state.window_decorations,
                                    &output,
                                    window,
                                    if use_full_window_snapshot {
                                        1.0
                                    } else {
                                        visual_state.opacity
                                    },
                                )
                                .unwrap_or_default()
                                {
                                    if let Some(root_origin) = root_origin {
                                        ordered_ui_elements.extend(
                                            transform_text_elements(vec![element], root_origin, composition_visual)
                                                .into_iter()
                                                .map(|item| (order, item)),
                                        );
                                    }
                                }

                                ordered_ui_elements.sort_by_key(|(order, _)| *order);
                                ordered_backdrop_elements.sort_by_key(|(order, _)| *order);
                            }

                            let content_clip = state
                                .window_decorations
                                .get(window)
                                .and_then(|decoration| decoration.content_clip);
                            let clip_all_client_surfaces = state
                                .window_decorations
                                .get(window)
                                .is_some_and(|decoration| decoration.managed_window.force_rect_size);

                            let client_elements = if let Some(content_clip) = content_clip {
                                if std::env::var_os("SHOJI_GAP_DEBUG").is_some() {
                                    if let Some(decoration) = state.window_decorations.get(window) {
                                        let snapshot_title = decoration.snapshot.title.clone();
                                        let snapshot_app_id = decoration.snapshot.app_id.clone();
                                        let snap_scale = smithay::utils::Scale::from((
                                            scale.x * visual_state.scale.x.max(0.0),
                                            scale.y * visual_state.scale.y.max(0.0),
                                        ));
                                        let border_width = (decoration.layout.root.rect.x + decoration.layout.root.rect.width)
                                            - (content_clip.rect.loc.x + content_clip.rect.size.w);
                                        let border_rect = Some(crate::ssd::LogicalRect::new(
                                            content_clip.rect.loc.x - border_width,
                                            content_clip.rect.loc.y - border_width,
                                            content_clip.rect.size.w + border_width * 2,
                                            content_clip.rect.size.h + border_width * 2,
                                        ));
                                        let snapped_inner = Some(
                                            crate::backend::visual::snapped_logical_rect_relative(
                                                crate::ssd::LogicalRect::new(
                                                    content_clip.rect.loc.x,
                                                    content_clip.rect.loc.y,
                                                    content_clip.rect.size.w,
                                                    content_clip.rect.size.h,
                                                ),
                                                output_geo.loc,
                                                snap_scale,
                                            )
                                        );
                                        let snapped_clip = crate::backend::visual::snapped_logical_rect_relative_with_mode(
                                            crate::ssd::LogicalRect::new(
                                                content_clip.rect.loc.x,
                                                content_clip.rect.loc.y,
                                                content_clip.rect.size.w,
                                                content_clip.rect.size.h,
                                            ),
                                            output_geo.loc,
                                            snap_scale,
                                            content_clip.snap_mode,
                                        );
                                        let expected_left =
                                            (snapped_clip.x as f64 * scale.x).round() as i32;
                                        let expected_top =
                                            (snapped_clip.y as f64 * scale.y).round() as i32;
                                        let expected_right =
                                            ((snapped_clip.x + snapped_clip.width) as f64 * scale.x).round() as i32;
                                        let expected_bottom =
                                            ((snapped_clip.y + snapped_clip.height) as f64 * scale.y).round() as i32;
                                        tracing::info!(
                                            output = %output.name(),
                                            window_id = %window_id,
                                            title = %snapshot_title,
                                            app_id = ?snapshot_app_id,
                                            window_location = ?window_location,
                                            output_scale = scale.x,
                                            window_scale_x = visual_state.scale.x,
                                            window_scale_y = visual_state.scale.y,
                                            physical_location = ?physical_location,
                                            border_rect = ?border_rect,
                                            snapped_inner = ?snapped_inner,
                                            content_clip = ?content_clip,
                                            snapped_clip = ?snapped_clip,
                                            expected_left,
                                            expected_top,
                                            expected_right,
                                            expected_bottom,
                                            "gap debug winit border/client geometry"
                                        );
                                    }
                                }
                                let clipped = window_render::clipped_surface_elements(
                                    window,
                                    renderer,
                                    physical_location,
                                    client_physical_geometry,
                                    output_geo.loc,
                                    scale,
                                    if use_full_window_snapshot { scale } else { snap_scale },
                                    if use_full_window_snapshot {
                                        1.0
                                    } else {
                                        visual_state.opacity
                                    },
                                    Some(content_clip),
                                    clip_all_client_surfaces,
                                )
                                .inspect_err(|error| {
                                    warn!(?error, "failed to build clipped surface elements");
                                })
                                .unwrap_or_default();
                                let bypass_clip =
                                    std::env::var_os("SHOJI_GAP_BYPASS_CLIP").is_some();
                                if std::env::var_os("SHOJI_GAP_DEBUG").is_some() {
                                    let first_geometry = clipped
                                        .first()
                                        .and_then(|element| match element {
                                            window_render::WindowClipElement::Clipped(element) => Some(
                                                smithay::backend::renderer::element::Element::geometry(element, scale),
                                            ),
                                            window_render::WindowClipElement::Raw(element) => Some(
                                                smithay::backend::renderer::element::Element::geometry(element, scale),
                                            ),
                                        });
                                    let window_geometry = window.geometry();
                                    let decoration_client_rect = state
                                        .window_decorations
                                        .get(window)
                                        .map(|decoration| decoration.client_rect);
                                    let snapshot = state.window_decorations.get(window).map(|decoration| {
                                        (decoration.snapshot.title.clone(), decoration.snapshot.app_id.clone())
                                    });
                                    let edge_delta = if let (Some(_decoration), Some(first_geometry)) =
                                        (state.window_decorations.get(window), first_geometry)
                                    {
                                        let snapped_clip = crate::backend::visual::snapped_logical_rect_relative(
                                            crate::ssd::LogicalRect::new(
                                                content_clip.rect.loc.x,
                                                content_clip.rect.loc.y,
                                                content_clip.rect.size.w,
                                                content_clip.rect.size.h,
                                            ),
                                            output_geo.loc,
                                            snap_scale,
                                        );
                                        let expected_left =
                                            (snapped_clip.x as f64 * scale.x).round() as i32;
                                        let expected_top =
                                            (snapped_clip.y as f64 * scale.y).round() as i32;
                                        let expected_right =
                                            ((snapped_clip.x + snapped_clip.width) as f64 * scale.x).round() as i32;
                                        let expected_bottom =
                                            ((snapped_clip.y + snapped_clip.height) as f64 * scale.y).round() as i32;
                                        Some((
                                            first_geometry.loc.x - expected_left,
                                            first_geometry.loc.y - expected_top,
                                            (first_geometry.loc.x + first_geometry.size.w) - expected_right,
                                            (first_geometry.loc.y + first_geometry.size.h) - expected_bottom,
                                        ))
                                    } else {
                                        None
                                    };
                                    tracing::info!(
                                        output = %output.name(),
                                        window_id = %window_id,
                                        title = %snapshot.as_ref().map(|(title, _)| title.as_str()).unwrap_or(""),
                                        app_id = ?snapshot.as_ref().and_then(|(_, app_id)| app_id.clone()),
                                        window_geometry = ?window_geometry,
                                        decoration_client_rect = ?decoration_client_rect,
                                        window_bbox = ?window.bbox(),
                                        physical_location = ?physical_location,
                                        clipped_count = clipped.len(),
                                        first_geometry = ?first_geometry,
                                        edge_delta = ?edge_delta,
                                        "gap debug winit clipped surface elements"
                                    );
                                }
                                if bypass_clip {
                                    let raw_elements = window_render::surface_elements(
                                        window,
                                        renderer,
                                        physical_location,
                                        scale,
                                        visual_state.opacity,
                                    );
                                    if std::env::var_os("SHOJI_GAP_DEBUG").is_some() {
                                        let first_geometry = raw_elements.first().map(|element| {
                                            smithay::backend::renderer::element::Element::geometry(element, scale)
                                        });
                                        let first_src = raw_elements.first().map(|element| {
                                            smithay::backend::renderer::element::Element::src(element)
                                        });
                                        let first_transform = raw_elements.first().map(|element| {
                                            smithay::backend::renderer::element::Element::transform(element)
                                        });
                                        tracing::info!(
                                            output = %output.name(),
                                            window_id = %window_id,
                                            physical_location = ?physical_location,
                                            raw_count = raw_elements.len(),
                                            first_geometry = ?first_geometry,
                                            first_src = ?first_src,
                                            first_transform = ?first_transform,
                                            "gap debug winit raw surface elements"
                                        );
                                    }
                                    transform_window_elements(
                                        raw_elements,
                                        composition_visual,
                                        WinitRenderElements::Window,
                                        WinitRenderElements::TransformedWindow,
                                    )
                                } else {
                                    clipped
                                        .into_iter()
                                        .flat_map(|element| match element {
                                            window_render::WindowClipElement::Clipped(element) => {
                                                transform_clipped_elements(vec![element], composition_visual)
                                            }
                                            window_render::WindowClipElement::Raw(element) => {
                                                transform_window_elements(
                                                    vec![element],
                                                    composition_visual,
                                                    WinitRenderElements::Window,
                                                    WinitRenderElements::TransformedWindow,
                                                )
                                            }
                                        })
                                        .collect()
                                }
                            } else {
                                let surfaces = window_render::surface_elements(
                                    window,
                                    renderer,
                                    physical_location,
                                    scale,
                                    if use_full_window_snapshot {
                                        1.0
                                    } else {
                                        visual_state.opacity
                                    },
                                );
                                if std::env::var_os("SHOJI_GAP_DEBUG").is_some() {
                                    let first_geometry = surfaces
                                        .first()
                                        .map(|element| smithay::backend::renderer::element::Element::geometry(element, scale));
                                    let window_geometry = window.geometry();
                                    let decoration_client_rect = state
                                        .window_decorations
                                        .get(window)
                                        .map(|decoration| decoration.client_rect);
                                    tracing::info!(
                                        output = %output.name(),
                                        window_id = %window_id,
                                        window_geometry = ?window_geometry,
                                        decoration_client_rect = ?decoration_client_rect,
                                        window_bbox = ?window.bbox(),
                                        physical_location = ?physical_location,
                                        surface_count = surfaces.len(),
                                        first_geometry = ?first_geometry,
                                        "gap debug winit raw surface elements"
                                    );
                                }
                                transform_window_elements(
                                    surfaces,
                                    composition_visual,
                                    WinitRenderElements::Window,
                                    WinitRenderElements::TransformedWindow,
                                )
                            };
                            let popup_elements = transform_window_elements(
                                window_render::popup_elements(
                                    window,
                                    renderer,
                                    physical_location,
                                    scale,
                                    if use_full_window_snapshot {
                                        1.0
                                    } else {
                                        visual_state.opacity
                                    },
                                ),
                                composition_visual,
                                WinitRenderElements::Window,
                                WinitRenderElements::TransformedWindow,
                            );
                            if use_full_window_snapshot {
                                let full_rect = state.window_decorations.get(window).map(
                                    |decoration| {
                                        window_render::snapshot_bounds(
                                            window,
                                            window_location,
                                            decoration.layout.root.rect,
                                            decoration.content_clip,
                                        )
                                    },
                                );
                                let mut snapshot_scene = Vec::new();
                                snapshot_scene.extend(popup_elements.into_iter());
                                snapshot_scene.extend(client_elements.into_iter());
                                snapshot_scene.extend(
                                    ordered_ui_elements.into_iter().map(|(_, element)| element),
                                );
                                snapshot_scene.extend(
                                    ordered_backdrop_elements
                                        .into_iter()
                                        .map(|(_, element)| element),
                                );
                                let snapshot_scene_signature =
                                    crate::backend::snapshot::render_element_scene_signature(
                                        &snapshot_scene,
                                        scale,
                                    );
                                let snapshot_element = full_rect
                                    .and_then(|full_rect| {
                                        if !window_has_snapshot_damage {
                                            if let Some(mut existing) = state
                                                .complete_window_snapshots
                                                .get(&window_id)
                                                .cloned()
                                                .filter(|snapshot| {
                                                    snapshot.scene_signature
                                                        == snapshot_scene_signature
                                                })
                                            {
                                                existing.rect = full_rect;
                                                state.complete_window_snapshots.insert(
                                                    window_id.clone(),
                                                    existing.clone(),
                                                );
                                                return Some(existing);
                                            }
                                        }
                                        let existing_complete =
                                            state.complete_window_snapshots.remove(&window_id);
                                        let tracker = state
                                            .complete_window_snapshot_trackers
                                            .entry(window_id.clone())
                                            .or_insert_with(|| {
                                                OutputDamageTracker::new(
                                                    (0, 0),
                                                    1.0,
                                                    smithay::utils::Transform::Normal,
                                                )
                                            });
                                        capture_snapshot_from_output_elements(
                                            renderer,
                                            output_geo,
                                            full_rect,
                                            scale,
                                            existing_complete,
                                            tracker,
                                            &snapshot_scene,
                                        )
                                        .ok()
                                        .flatten()
                                        .map(|mut snapshot| {
                                            snapshot.scene_signature = snapshot_scene_signature;
                                            state.complete_window_snapshots.insert(
                                                window_id.clone(),
                                                snapshot.clone(),
                                            );
                                            snapshot
                                        })
                                    })
                                    .and_then(|snapshot| {
                                        snapshot::live_snapshot_element(
                                            renderer,
                                            &snapshot,
                                            output_geo,
                                            scale,
                                            visual_state.opacity,
                                        )
                                    })
                                    .map(|element| transform_snapshot_elements(vec![element], visual_state))
                                    .and_then(|mut elements| elements.pop());
                                if let Some(element) = snapshot_element {
                                    scene_elements.push(element);
                                }
                            } else {
                                if is_identity_visual_geometry(composition_visual) {
                                    // Steady state: replace the raw popup
                                    // pass-through with per-popup effect
                                    // composition (effect elements cannot ride
                                    // the window animation transform).
                                    drop(popup_elements);
                                    let configured_popup_effects =
                                        state.configured_popup_effects.clone();
                                    scene_elements.extend(composed_window_popup_scene_elements(
                                        renderer,
                                        &output,
                                        output_geo,
                                        scale,
                                        window,
                                        physical_location,
                                        visual_state.opacity,
                                        &configured_popup_effects,
                                        &mut state.popup_effect_cache,
                                        &mut state.popup_framebuffer_effect_states,
                                    ));
                                } else {
                                    scene_elements.extend(popup_elements.into_iter());
                                }
                                scene_elements.extend(client_elements.into_iter());
                                scene_elements.extend(
                                    ordered_ui_elements.into_iter().map(|(_, element)| element),
                                );
                                scene_elements.extend(
                                    ordered_backdrop_elements
                                        .into_iter()
                                        .map(|(_, element)| element),
                                );
                            }

                            if state.windows_ready_for_decoration.insert(window_id.clone()) {
                                state.apply_pending_initial_focus_for_window(&window_id, window);
                            }

                            if let Some(decoration) = state.window_decorations.get(window)
                                && let Some(live_snapshot) = state
                                    .live_window_snapshots
                                    .get_mut(&decoration.snapshot.id)
                            {
                                snapshot::retarget_snapshot_rect(
                                    live_snapshot,
                                    decoration.client_rect,
                                );
                            }
                            let should_refresh_snapshot = state
                                .window_decorations
                                .get(window)
                                .map(|decoration| {
                                    state
                                        .live_window_snapshots
                                        .get(&decoration.snapshot.id)
                                        .map(|snapshot| {
                                            snapshot.rect.width != decoration.client_rect.width
                                                || snapshot.rect.height
                                                    != decoration.client_rect.height
                                        })
                                        .unwrap_or(true)
                                })
                                .unwrap_or(false);
                            if should_refresh_snapshot {
                                if capture_live_snapshot_for_window(
                                    renderer,
                                    state,
                                    &output,
                                    window,
                                    window_location,
                                    scale,
                                    0,
                                )
                                .is_ok()
                                {
                                    if let Some(window_id) = state
                                        .window_decorations
                                        .get(window)
                                        .map(|decoration| decoration.snapshot.id.clone())
                                    {
                                        state.snapshot_dirty_window_ids.remove(&window_id);
                                    }
                                }
                            }
                            if let Some(snapshot_id) = snapshot_id.as_ref() {
                                state.snapshot_dirty_window_ids.remove(snapshot_id);
                            }

                        }
                        // Fullscreen fast path: Bottom/Background layers are
                        // fully occluded.
                        if fullscreen_scanout.is_none() {
                            scene_elements.extend(lower_layer_scene_elements(
                                renderer,
                                state,
                                &output,
                                output_geo,
                                scale,
                                &windows_top_to_bottom,
                            ));
                        }

                        // Render layer-shell popups after all layer roots and
                        // windows. GTK uses zwlr_layer_surface_v1.get_popup
                        // for Waybar tooltips, so keeping them next to a
                        // Bottom-layer parent would wrongly place them behind
                        // regular toplevels.
                        let configured_popup_effects = state.configured_popup_effects.clone();
                        let configured_popup_surface_policies =
                            state.configured_popup_surface_policies.clone();
                        let mut layer_popup_elements = layer_popup_scene_elements(
                            renderer,
                            &output,
                            output_geo,
                            scale,
                            fullscreen_scanout.is_some(),
                            &configured_popup_effects,
                            &configured_popup_surface_policies,
                            &mut state.popup_effect_cache,
                            &mut state.popup_framebuffer_effect_states,
                        );
                        layer_popup_elements.append(&mut scene_elements);
                        scene_elements = layer_popup_elements;

                        let computed_damage = if state.damage_blink_enabled {
                            match blink_damage_tracker.damage_output(1, &scene_elements) {
                                Ok((damage, _)) => damage.cloned(),
                                Err(_) => None,
                            }
                        } else {
                            None
                        };

                        if state.damage_blink_enabled {
                            if let Some(damage) = computed_damage.as_deref() {
                                state.record_damage_blink(&output, damage);
                            }
                            if manual_invalidate_debug_enabled() {
                                info!(
                                    output = %output.name(),
                                    extra_damage = ?extra_damage,
                                    blink_visible = ?state.damage_blink_rects_for_output(&output),
                                    "manual invalidate blink inputs"
                                );
                            }
                        }

                        let mut content_elements: Vec<WinitRenderElements> = Vec::new();
                        content_elements.extend(
                            damage::elements_for_output(&extra_damage, output_geo)
                                .into_iter()
                                .map(WinitRenderElements::Damage),
                        );
                        content_elements.extend(scene_elements);

                        let mut elements: Vec<WinitRenderElements> = Vec::new();
                        let error_text_elements = crate::config_error::text_elements_for_output(
                            renderer,
                            &mut state.text_rasterizer,
                            state.config_error_report.as_ref(),
                            output_geo,
                            scale,
                        )
                        .unwrap_or_default()
                        .into_iter()
                        .map(WinitRenderElements::Text);
                        let error_background_elements =
                            crate::config_error::background_elements_for_output(
                                state.config_error_report.as_ref(),
                                output_geo,
                                scale,
                            )
                            .into_iter()
                            .map(WinitRenderElements::Blink);
                        // FPS overlay sits in front of everything else; build
                        // before the blink + content layers so it ends up at
                        // index 0 (top-most under smithay's element model).
                        let fps_overlay_elements: Vec<WinitRenderElements> = state
                            .fps_counter
                            .render_elements(renderer, output.name().as_str(), output_geo, scale)
                            .into_iter()
                            .map(WinitRenderElements::Text)
                            .collect();
                        elements.extend(error_text_elements);
                        elements.extend(error_background_elements);
                        elements.extend(fps_overlay_elements);
                        elements.extend(
                            damage_blink::elements_for_output(
                                state.damage_blink_rects_for_output(&output),
                                output_geo,
                                scale,
                            )
                            .into_iter()
                            .map(WinitRenderElements::Blink),
                        );
                        if let Some(lock_surface) = state.session_lock_surface_for_output(&output)
                        {
                            elements.extend(
                                crate::backend::window::lock_surface_elements(
                                    renderer,
                                    &lock_surface,
                                    scale,
                                    1.0,
                                )
                                .into_iter()
                                .map(WinitRenderElements::Window),
                            );
                        }
                        elements.extend(content_elements);

                        trace!(
                            output = %output.name(),
                            window_count = state.space.elements().count(),
                            render_element_count = elements.len(),
                            "rendering winit frame"
                        );
                        timing.render_element_count = elements.len();
                        timing.transform_snapshot_window_count =
                            state.transform_snapshot_window_ids.len();
                        timing.closing_snapshot_count = state.closing_window_snapshots.len();
                        timing.scene_build_elapsed_ms =
                            scene_build_started_at.elapsed().as_secs_f64() * 1000.0;

                        if !elements.is_empty() {
                            let frame_target = state.clock.now()
                                + output
                                    .current_mode()
                                    .map(|mode| Duration::from_secs_f64(1_000f64 / mode.refresh as f64))
                                    .unwrap_or(Duration::ZERO);
                            state.pre_repaint(&output, frame_target);

                            let render_started_at = Instant::now();
                            let render_output_result = {
                                timescope::scope!("winit render_output");
                                damage_tracker.render_output(
                                    renderer,
                                    &mut framebuffer,
                                    0,
                                    &elements,
                                    [0.1, 0.1, 0.1, 1.0],
                                )
                            };
                            timing.render_elapsed_ms =
                                render_started_at.elapsed().as_secs_f64() * 1000.0;
                            if let Ok(render_output_result) = render_output_result {
                                if manual_invalidate_debug_enabled() {
                                    info!(
                                        output = %output.name(),
                                        final_damage = ?render_output_result.damage,
                                        "manual invalidate render output damage"
                                    );
                                }
                                should_submit_frame = true;
                                update_primary_scanout_output(
                                    &state.space,
                                    &output,
                                    &state.cursor_status,
                                    state.session_lock_surface_for_output(&output).as_ref(),
                                    &render_output_result.states,
                                    &state.window_decorations,
                                );

                                let frame_time = Duration::from(state.clock.now())
                                    + output
                                        .current_mode()
                                        .map(|mode| Duration::from_secs_f64(1_000f64 / mode.refresh as f64))
                                        .unwrap_or(Duration::ZERO);

                                if render_output_result.damage.is_some() {
                                    let mut output_presentation_feedback =
                                        take_presentation_feedback(
                                            &output,
                                            &state.space,
                                            state
                                                .session_lock_surface_for_output(&output)
                                                .as_ref(),
                                            &render_output_result.states,
                                        );
                                    output_presentation_feedback.presented::<Duration, Monotonic>(
                                        frame_time,
                                        output
                                            .current_mode()
                                            .map(|mode| Refresh::fixed(Duration::from_secs_f64(1_000f64 / mode.refresh as f64)))
                                            .unwrap_or(Refresh::Unknown),
                                        0,
                                        wp_presentation_feedback::Kind::Vsync,
                                    );
                                }

                                state.post_repaint(&output, frame_time, &render_output_result.states);
                                state.fps_counter.record_present(output.name().as_str());
                            }
                        }
                    }
                    let submit_started_at = Instant::now();
                    if should_submit_frame {
                        backend.submit(Some(&[damage])).unwrap();
                    }
                    let submit_elapsed_ms =
                        submit_started_at.elapsed().as_secs_f64() * 1000.0;

                    state.space.refresh();
                    state.cleanup_popups_with_debug("winit-post-render");
                    state.pending_decoration_damage.clear();
                    state.clear_source_damage();
                    state.finish_damage_blink_frame();
                    let _ = state.display_handle.flush_clients();
                    let total_redraw_elapsed_ms =
                        redraw_started_at.elapsed().as_secs_f64() * 1000.0;

                    if animation_timing_debug_enabled()
                        && (timing.transform_snapshot_window_count > 0
                            || timing.closing_snapshot_count > 0
                            || decorations_refresh_elapsed_ms >= spike_threshold_ms
                            || layer_effects_elapsed_ms >= spike_threshold_ms
                            || timing.render_elapsed_ms >= spike_threshold_ms
                            || total_redraw_elapsed_ms >= spike_threshold_ms)
                    {
                        if total_redraw_elapsed_ms >= spike_threshold_ms
                            || timing.render_elapsed_ms >= spike_threshold_ms
                            || decorations_refresh_elapsed_ms >= spike_threshold_ms
                            || layer_effects_elapsed_ms >= spike_threshold_ms
                        {
                            warn!(
                                output = %output.name(),
                                decorations_refresh_elapsed_ms,
                                layer_effects_elapsed_ms,
                                scene_build_elapsed_ms = timing.scene_build_elapsed_ms,
                                render_elapsed_ms = timing.render_elapsed_ms,
                                submit_elapsed_ms,
                                total_redraw_elapsed_ms,
                                render_element_count = timing.render_element_count,
                                transform_snapshot_window_count =
                                    timing.transform_snapshot_window_count,
                                closing_snapshot_count = timing.closing_snapshot_count,
                                should_submit_frame,
                                spike_threshold_ms,
                                "animation timing: winit frame spike"
                            );
                        } else {
                            info!(
                                output = %output.name(),
                                decorations_refresh_elapsed_ms,
                                layer_effects_elapsed_ms,
                                scene_build_elapsed_ms = timing.scene_build_elapsed_ms,
                                render_elapsed_ms = timing.render_elapsed_ms,
                                submit_elapsed_ms,
                                total_redraw_elapsed_ms,
                                render_element_count = timing.render_element_count,
                                transform_snapshot_window_count =
                                    timing.transform_snapshot_window_count,
                                closing_snapshot_count = timing.closing_snapshot_count,
                                should_submit_frame,
                                spike_threshold_ms,
                                "animation timing: winit frame"
                            );
                        }
                    }

                    backend.window().request_redraw();
                }
                WinitEvent::CloseRequested => {
                    state.shutdown();
                }
                _ => (),
            };
        })?;

    Ok(())
}

smithay::render_elements! {
    pub WinitRenderElements<=GlesRenderer>;
    Window=WaylandSurfaceRenderElement<GlesRenderer>,
    PolicyWindow=crate::backend::visual::IgnoredOpaqueRegionElement<WaylandSurfaceRenderElement<GlesRenderer>>,
    TransformedWindow=RelocateRenderElement<RescaleRenderElement<WaylandSurfaceRenderElement<GlesRenderer>>>,
    Clipped=crate::backend::clipped_surface::ClippedSurfaceElement,
    TransformedClipped=RelocateRenderElement<RescaleRenderElement<crate::backend::clipped_surface::ClippedSurfaceElement>>,
    Text=crate::backend::text::DecorationTextureElements,
    RelocatedText=RelocateRenderElement<crate::backend::text::DecorationTextureElements>,
    TransformedText=RelocateRenderElement<RescaleRenderElement<RelocateRenderElement<crate::backend::text::DecorationTextureElements>>>,
    Snapshot=TextureRenderElement<GlesTexture>,
    TransformedSnapshot=RelocateRenderElement<RescaleRenderElement<TextureRenderElement<GlesTexture>>>,
    Damage=crate::backend::damage::DamageOnlyElement,
    Blink=SolidColorRenderElement,
    Decoration=crate::backend::decoration::DecorationSceneElements,
    RelocatedDecoration=RelocateRenderElement<crate::backend::decoration::DecorationSceneElements>,
    TransformedDecoration=RelocateRenderElement<RescaleRenderElement<RelocateRenderElement<crate::backend::decoration::DecorationSceneElements>>>,
    Backdrop=crate::backend::shader_effect::StableBackdropTextureElement,
    RelocatedBackdrop=RelocateRenderElement<crate::backend::shader_effect::StableBackdropTextureElement>,
    TransformedBackdrop=RelocateRenderElement<RescaleRenderElement<RelocateRenderElement<crate::backend::shader_effect::StableBackdropTextureElement>>>,
}

fn transform_window_elements(
    elements: Vec<WaylandSurfaceRenderElement<GlesRenderer>>,
    visual: WindowVisualState,
    direct: fn(WaylandSurfaceRenderElement<GlesRenderer>) -> WinitRenderElements,
    transformed: fn(
        RelocateRenderElement<RescaleRenderElement<WaylandSurfaceRenderElement<GlesRenderer>>>,
    ) -> WinitRenderElements,
) -> Vec<WinitRenderElements> {
    if is_identity_visual_geometry(visual) {
        return elements.into_iter().map(direct).collect();
    }

    elements
        .into_iter()
        .map(|element| {
            transformed(RelocateRenderElement::from_element(
                RescaleRenderElement::from_element(element, visual.origin, visual.scale),
                visual.translation,
                Relocate::Relative,
            ))
        })
        .collect()
}

fn transform_clipped_elements(
    elements: Vec<crate::backend::clipped_surface::ClippedSurfaceElement>,
    visual: WindowVisualState,
) -> Vec<WinitRenderElements> {
    if is_identity_visual_geometry(visual) {
        if clipped_transform_debug_enabled() {
            for element in &elements {
                info!(
                    debug_label = element.debug_label(),
                    visual_origin = ?visual.origin,
                    visual_scale = ?visual.scale,
                    visual_translation = ?visual.translation,
                    pre_transform_geometry = ?element.geometry(smithay::utils::Scale::from((1.0, 1.0))),
                    post_transform_geometry = ?element.geometry(smithay::utils::Scale::from((1.0, 1.0))),
                    "gap debug winit transformed clipped geometry"
                );
            }
        }
        return elements
            .into_iter()
            .map(WinitRenderElements::Clipped)
            .collect();
    }

    elements
        .into_iter()
        .map(|element| {
            let debug_label = element.debug_label().map(|label| label.to_owned());
            let pre_transform_geometry = element.geometry(smithay::utils::Scale::from((1.0, 1.0)));
            let transformed = RelocateRenderElement::from_element(
                RescaleRenderElement::from_element(element, visual.origin, visual.scale),
                visual.translation,
                Relocate::Relative,
            );
            if clipped_transform_debug_enabled() {
                info!(
                    debug_label = debug_label.as_deref(),
                    visual_origin = ?visual.origin,
                    visual_scale = ?visual.scale,
                    visual_translation = ?visual.translation,
                    pre_transform_geometry = ?pre_transform_geometry,
                    post_transform_geometry = ?transformed.geometry(smithay::utils::Scale::from((1.0, 1.0))),
                    "gap debug winit transformed clipped geometry"
                );
            }
            WinitRenderElements::TransformedClipped(transformed)
        })
        .collect()
}

fn transform_text_elements(
    elements: Vec<crate::backend::text::DecorationTextureElements>,
    root_origin: Point<i32, smithay::utils::Physical>,
    visual: WindowVisualState,
) -> Vec<WinitRenderElements> {
    if is_identity_visual_geometry(visual) {
        return elements
            .into_iter()
            .map(|element| {
                WinitRenderElements::RelocatedText(RelocateRenderElement::from_element(
                    element,
                    root_origin,
                    Relocate::Relative,
                ))
            })
            .collect();
    }

    elements
        .into_iter()
        .map(|element| {
            let relocated =
                RelocateRenderElement::from_element(element, root_origin, Relocate::Relative);
            WinitRenderElements::TransformedText(RelocateRenderElement::from_element(
                RescaleRenderElement::from_element(relocated, visual.origin, visual.scale),
                visual.translation,
                Relocate::Relative,
            ))
        })
        .collect()
}

fn transform_snapshot_elements(
    elements: Vec<TextureRenderElement<GlesTexture>>,
    visual: WindowVisualState,
) -> Vec<WinitRenderElements> {
    if is_identity_visual_geometry(visual) {
        return elements
            .into_iter()
            .map(WinitRenderElements::Snapshot)
            .collect();
    }

    elements
        .into_iter()
        .map(|element| {
            WinitRenderElements::TransformedSnapshot(RelocateRenderElement::from_element(
                RescaleRenderElement::from_element(element, visual.origin, visual.scale),
                visual.translation,
                Relocate::Relative,
            ))
        })
        .collect()
}

fn transform_decoration_elements(
    elements: Vec<crate::backend::decoration::DecorationSceneElements>,
    root_origin: Point<i32, smithay::utils::Physical>,
    visual: WindowVisualState,
) -> Vec<WinitRenderElements> {
    if is_identity_visual_geometry(visual) {
        return elements
            .into_iter()
            .map(|element| {
                WinitRenderElements::RelocatedDecoration(RelocateRenderElement::from_element(
                    element,
                    root_origin,
                    Relocate::Relative,
                ))
            })
            .collect();
    }

    elements
        .into_iter()
        .map(|element| {
            let relocated =
                RelocateRenderElement::from_element(element, root_origin, Relocate::Relative);
            WinitRenderElements::TransformedDecoration(RelocateRenderElement::from_element(
                RescaleRenderElement::from_element(relocated, visual.origin, visual.scale),
                visual.translation,
                Relocate::Relative,
            ))
        })
        .collect()
}

fn transform_backdrop_elements(
    elements: Vec<crate::backend::shader_effect::StableBackdropTextureElement>,
    root_origin: Point<i32, smithay::utils::Physical>,
    visual: WindowVisualState,
) -> Vec<WinitRenderElements> {
    if is_identity_visual_geometry(visual) {
        return elements
            .into_iter()
            .map(|element| {
                let debug_label = if std::env::var_os("SHOJI_GAP_DEBUG").is_some() {
                    Some(element.debug_label().to_string())
                } else {
                    None
                };
                let pre_transform_geometry = if std::env::var_os("SHOJI_GAP_DEBUG").is_some() {
                    Some(smithay::backend::renderer::element::Element::geometry(
                        &element,
                        smithay::utils::Scale::from((1.0, 1.0)),
                    ))
                } else {
                    None
                };
                let relocated = WinitRenderElements::RelocatedBackdrop(
                    RelocateRenderElement::from_element(element, root_origin, Relocate::Relative),
                );
                if let (Some(debug_label), Some(pre_transform_geometry)) =
                    (debug_label, pre_transform_geometry)
                {
                    let post_transform_geometry =
                        smithay::backend::renderer::element::Element::geometry(
                            &relocated,
                            smithay::utils::Scale::from((1.0, 1.0)),
                        );
                    tracing::info!(
                        backdrop = %debug_label,
                        root_origin = ?root_origin,
                        visual_origin = ?visual.origin,
                        visual_scale = ?visual.scale,
                        visual_translation = ?visual.translation,
                        pre_transform_geometry = ?pre_transform_geometry,
                        post_transform_geometry = ?post_transform_geometry,
                        "gap debug winit transformed backdrop geometry"
                    );
                }
                relocated
            })
            .collect();
    }

    elements
        .into_iter()
        .map(|element| {
            let debug_label = if std::env::var_os("SHOJI_GAP_DEBUG").is_some() {
                Some(element.debug_label().to_string())
            } else {
                None
            };
            let pre_transform_geometry = if std::env::var_os("SHOJI_GAP_DEBUG").is_some() {
                Some(smithay::backend::renderer::element::Element::geometry(
                    &element,
                    smithay::utils::Scale::from((1.0, 1.0)),
                ))
            } else {
                None
            };
            let relocated =
                RelocateRenderElement::from_element(element, root_origin, Relocate::Relative);
            let transformed =
                WinitRenderElements::TransformedBackdrop(RelocateRenderElement::from_element(
                    RescaleRenderElement::from_element(relocated, visual.origin, visual.scale),
                    visual.translation,
                    Relocate::Relative,
                ));
            if let (Some(debug_label), Some(pre_transform_geometry)) =
                (debug_label, pre_transform_geometry)
            {
                let post_transform_geometry =
                    smithay::backend::renderer::element::Element::geometry(
                        &transformed,
                        smithay::utils::Scale::from((1.0, 1.0)),
                    );
                tracing::info!(
                    backdrop = %debug_label,
                    root_origin = ?root_origin,
                    visual_origin = ?visual.origin,
                    visual_scale = ?visual.scale,
                    visual_translation = ?visual.translation,
                    pre_transform_geometry = ?pre_transform_geometry,
                    post_transform_geometry = ?post_transform_geometry,
                    "gap debug winit transformed backdrop geometry"
                );
            }
            transformed
        })
        .collect()
}

fn backdrop_shader_elements_for_window(
    renderer: &mut GlesRenderer,
    state: &mut ShojiWM,
    output: &Output,
    output_geo: Rectangle<i32, Logical>,
    scale: smithay::utils::Scale<f64>,
    windows_top_to_bottom: &[smithay::desktop::Window],
    window_index: usize,
    window: &smithay::desktop::Window,
    alpha: f32,
    has_backdrop_source: bool,
    apply_visual_transform: bool,
    prefer_framebuffer_backdrops: bool,
) -> Vec<(
    usize,
    crate::backend::shader_effect::StableBackdropTextureElement,
    bool,
)> {
    if !has_backdrop_source {
        let Some(decoration) = state.window_decorations.get(window) else {
            return Vec::new();
        };
        if !decoration.shader_buffers.iter().any(|cached| {
            cached.shader.is_texture_backed()
                && (!prefer_framebuffer_backdrops || !cached.shader.supports_framebuffer_backdrop())
        }) {
            return Vec::new();
        }
    }
    let Some(decoration) = state.window_decorations.get(window).cloned() else {
        return Vec::new();
    };
    let lower_windows = windows_top_to_bottom
        .iter()
        .skip(window_index + 1)
        .cloned()
        .collect::<Vec<_>>();
    let (_, lower_layers) = window_render::layer_surfaces_for_output(output);

    decoration
        .shader_buffers
        .clone()
        .iter()
        .filter(|cached| {
            cached.shader.is_texture_backed()
                && (!prefer_framebuffer_backdrops
                    || !cached.shader.supports_framebuffer_backdrop())
        })
        .filter_map(|cached| {
            let uses_backdrop = cached.shader.uses_backdrop_input();
            let uses_xray = cached.shader.uses_xray_backdrop_input();
            let render_as_backdrop = uses_backdrop || uses_xray;
            let root_rect = decoration.layout.root.rect;
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            cached.stable_key.hash(&mut hasher);
            let display_rect = if apply_visual_transform {
                crate::backend::visual::transformed_rect(
                    cached.rect,
                    decoration.layout.root.rect,
                    decoration.visual_transform,
                )
            } else {
                cached.rect
            };
            let display_rect_precise = cached
                .rect_precise
                .map(|rect| {
                    if apply_visual_transform {
                        crate::backend::visual::transformed_precise_rect(
                            rect,
                            decoration.layout.root.rect,
                            decoration.visual_transform,
                        )
                    } else {
                        rect
                    }
                })
                .or_else(|| {
                    Some(crate::backend::visual::precise_rect_from_logical(
                        display_rect,
                    ))
                });
            // Snapshot mode renders the whole window into a raw, untransformed offscreen texture
            // and applies the visual transform exactly once to that final texture. If the shader
            // effect capture/sample rect is still derived from the transformed display rect here,
            // backdrop-based effects are sampled from a second, stale coordinate space and drift
            // slightly during scale animations even though the final snapshot is otherwise correct.
            let source_effect_rect = display_rect;
            let _source_effect_rect_precise = display_rect_precise.unwrap_or_else(|| {
                crate::backend::visual::precise_rect_from_logical(source_effect_rect)
            });
            (
                source_effect_rect.x,
                source_effect_rect.y,
                source_effect_rect.width,
                source_effect_rect.height,
                output_geo.loc.x,
                output_geo.loc.y,
                output_geo.size.w,
                output_geo.size.h,
            )
                .hash(&mut hasher);
            let blur_padding = cached.shader.capture_padding.max(0);
            (blur_padding, cached.clip_radius).hash(&mut hasher);
            if uses_backdrop || uses_xray {
                state.lower_layer_scene_generation.hash(&mut hasher);
            }
            format!("{:?}", cached.shader).hash(&mut hasher);
            let capture_geo = Rectangle::new(
                smithay::utils::Point::from((
                    source_effect_rect.x - blur_padding,
                    source_effect_rect.y - blur_padding,
                )),
                (
                    source_effect_rect.width + blur_padding * 2,
                    source_effect_rect.height + blur_padding * 2,
                )
                    .into(),
            );
            let actual_capture_geo = capture_geo.intersection(output_geo).unwrap_or(capture_geo);
            let capture_origin_physical =
                crate::backend::visual::logical_point_to_physical_point_global_edges(
                    actual_capture_geo.loc,
                    output_geo.loc,
                    scale,
                );
            (
                actual_capture_geo.loc.x,
                actual_capture_geo.loc.y,
                actual_capture_geo.size.w,
                actual_capture_geo.size.h,
                capture_origin_physical.x,
                capture_origin_physical.y,
            )
                .hash(&mut hasher);
            if uses_backdrop {
                hash_window_scene_contributors(
                    &mut hasher,
                    state,
                    &lower_windows,
                    source_effect_rect,
                );
            }
            if uses_backdrop || uses_xray {
                hash_layer_scene_contributors(
                    &mut hasher,
                    output,
                    &lower_layers,
                    source_effect_rect,
                );
            }
            let signature = hasher.finish();
            let source_damage_hit = crate::backend::shader_effect::source_damage_intersects_rect(
                &cached.shader,
                Rectangle::new(
                    smithay::utils::Point::from((source_effect_rect.x, source_effect_rect.y)),
                    (source_effect_rect.width, source_effect_rect.height).into(),
                ),
                &{
                    let mut entries = Vec::new();
                    if uses_backdrop {
                        entries.extend(collect_window_source_damage(
                            state,
                            lower_windows.iter().cloned(),
                        ));
                    }
                    if uses_backdrop || uses_xray {
                        entries.extend(collect_layer_source_damage(
                            state,
                            lower_layers.iter().cloned(),
                            true,
                        ));
                    }
                    entries
                },
            );

            if std::env::var_os("SHOJI_FIREFOX_BACKDROP_DEBUG").is_some() {
                tracing::info!(
                    window_id = %decoration.snapshot.id,
                    title = %decoration.snapshot.title,
                    app_id = ?decoration.snapshot.app_id,
                    stable_key = %cached.stable_key,
                    source_effect_rect = ?source_effect_rect,
                    display_rect = ?display_rect,
                    display_rect_precise = ?display_rect_precise,
                    root_rect = ?root_rect,
                    output_geo = ?output_geo,
                    blur_padding,
                    capture_geo = ?capture_geo,
                    actual_capture_geo = ?actual_capture_geo,
                    capture_origin_physical = ?capture_origin_physical,
                    apply_visual_transform,
                    uses_backdrop,
                    uses_xray,
                    "backdrop debug: window shader rects"
                );
            }

            if !matches!(
                cached.shader.invalidate_policy(),
                crate::ssd::EffectInvalidationPolicy::Always
            ) && !source_damage_hit
            {
                if let Some(existing) = state
                    .window_decorations
                    .get(window)
                    .and_then(|d| d.backdrop_cache.get(&cached.stable_key))
                    .filter(|existing| existing.signature == signature)
                    .cloned()
                {
                    let local_rect = Rectangle::new(
                        smithay::utils::Point::from((
                            display_rect.x - root_rect.x,
                            display_rect.y - root_rect.y,
                        )),
                        (display_rect.width, display_rect.height).into(),
                    );
                    let clip_rect = {
                        let precise_clip = display_rect_precise
                            .zip(cached.clip_rect_precise.map(|clip| {
                                if apply_visual_transform {
                                    crate::backend::visual::transformed_precise_rect(
                                        clip,
                                        decoration.layout.root.rect,
                                        decoration.visual_transform,
                                    )
                                } else {
                                    clip
                                }
                            }))
                            .map(|(rect, clip)| {
                                crate::backend::visual::snapped_precise_logical_rect_in_root_frame_area_space(
                                    clip,
                                    rect,
                                    display_rect.width,
                                    display_rect.height,
                                    root_rect,
                                    output_geo,
                                    scale,
                                )
                            });
                        precise_clip.or_else(|| {
                            cached.clip_rect.map(|clip_rect| {
                                let transformed_clip = if apply_visual_transform {
                                    crate::backend::visual::transformed_rect(
                                        clip_rect,
                                        decoration.layout.root.rect,
                                        decoration.visual_transform,
                                    )
                                } else {
                                    clip_rect
                                };
                                let rect = display_rect_precise.unwrap_or_else(|| {
                                    crate::backend::visual::precise_rect_from_logical(display_rect)
                                });
                                crate::backend::visual::snapped_precise_logical_rect_in_root_frame_area_space(
                                    crate::backend::visual::precise_rect_from_logical(
                                        transformed_clip,
                                    ),
                                    rect,
                                    display_rect.width,
                                    display_rect.height,
                                    root_rect,
                                    output_geo,
                                    scale,
                                )
                            })
                        })
                    };
                    let local_sample_rect = Rectangle::new(
                        smithay::utils::Point::from((
                            source_effect_rect.x - output_geo.loc.x,
                            source_effect_rect.y - output_geo.loc.y,
                        )),
                        (source_effect_rect.width, source_effect_rect.height).into(),
                    );
                    let local_capture_rect = local_sample_rect;
                    let geometry = display_rect_precise
                        .map(|rect| {
                            crate::backend::visual::relative_physical_rect_from_root_precise(
                                rect,
                                root_rect,
                                output_geo,
                                scale,
                            )
                        })
                        .unwrap_or_else(|| {
                            crate::backend::visual::relative_physical_rect_from_root_global_origin_size(
                                display_rect,
                                root_rect,
                                output_geo,
                                scale,
                            )
                        });
                    return crate::backend::shader_effect::backdrop_shader_element_with_geometry(
                        renderer,
                        existing.id.clone(),
                        existing.commit_counter,
                        existing.texture,
                        local_rect,
                        geometry,
                        local_sample_rect,
                        local_capture_rect,
                        &cached.shader,
                        alpha,
                        scale.x as f32,
                        [0.0, 0.0],
                        clip_rect,
                        cached
                            .clip_radius_precise
                            .unwrap_or(cached.clip_radius as f32),
                        format!(
                            "window-backdrop:{}:{}",
                            decoration.snapshot.id, cached.stable_key
                        ),
                    )
                    .ok()
                    .map(|element| {
                        if std::env::var_os("SHOJI_FIREFOX_BACKDROP_DEBUG").is_some() {
                            tracing::info!(
                                window_id = %decoration.snapshot.id,
                                title = %decoration.snapshot.title,
                                app_id = ?decoration.snapshot.app_id,
                                stable_key = %cached.stable_key,
                                local_rect = ?local_rect,
                                local_sample_rect = ?local_sample_rect,
                                local_capture_rect = ?local_capture_rect,
                                geometry = ?geometry,
                                from_cache = true,
                                "backdrop debug: window shader element"
                            );
                        }
                        (cached.order, element, render_as_backdrop)
                    });
                }
            }
            let backdrop_texture = if uses_backdrop {
                let mut backdrop_scene: Vec<WinitRenderElements> = Vec::new();
                let actual_capture_geo =
                    capture_geo.intersection(output_geo).unwrap_or(capture_geo);
                for lower_window in &lower_windows {
                    backdrop_scene.extend(window_scene_elements_for_capture(
                        renderer,
                        state,
                        output_geo.loc,
                        actual_capture_geo,
                        capture_origin_physical,
                        scale,
                        lower_window,
                    ));
                }
                let (_, lower_layer_elements) =
                    window_render::layer_elements_for_output(renderer, output, scale, 1.0);
                let capture_visual = WindowVisualState {
                    origin: smithay::utils::Point::from((0, 0)),
                    scale: smithay::utils::Scale::from((1.0, 1.0)),
                    translation: Point::from((-capture_origin_physical.x, -capture_origin_physical.y)),
                    opacity: 1.0,
                };
                backdrop_scene.extend(
                    transform_window_elements(
                        lower_layer_elements,
                        capture_visual,
                        WinitRenderElements::Window,
                        WinitRenderElements::TransformedWindow,
                    )
                    .into_iter(),
                );
                capture_scene_texture_for_effect(
                    renderer,
                    "winit-window-backdrop",
                    actual_capture_geo,
                    scale,
                    &backdrop_scene,
                )
            } else {
                None
            };
            let xray_texture = if uses_xray {
                let mut xray_scene: Vec<WinitRenderElements> = Vec::new();
                for lower_layer in &lower_layers {
                    xray_scene.extend(layer_surface_scene_elements_for_capture(
                        renderer,
                        output,
                        actual_capture_geo,
                        capture_origin_physical,
                        scale,
                        lower_layer,
                    ));
                }
                capture_scene_texture_for_effect(
                    renderer,
                    "winit-window-xray",
                    actual_capture_geo,
                    scale,
                    &xray_scene,
                )
            } else {
                None
            };
            let input_texture = backdrop_texture
                .clone()
                .or_else(|| xray_texture.clone())
                .or_else(|| crate::backend::shader_effect::solid_white_texture(renderer).ok())?;
            let geometry = display_rect_precise
                .map(|rect| {
                    crate::backend::visual::relative_physical_rect_from_root_precise(
                        rect,
                        root_rect,
                        output_geo,
                        scale,
                    )
                })
                .unwrap_or_else(|| {
                    crate::backend::visual::relative_physical_rect_from_root_global_origin_size(
                        display_rect,
                        root_rect,
                        output_geo,
                        scale,
                    )
                });
            let root_origin_physical =
                crate::backend::visual::root_physical_origin(root_rect, output_geo, scale);
            let final_backdrop_screen_rect = Rectangle::new(
                smithay::utils::Point::from((
                    root_origin_physical.x + geometry.loc.x,
                    root_origin_physical.y + geometry.loc.y,
                )),
                geometry.size,
            );
            let sample_region = Rectangle::new(
                smithay::utils::Point::from((
                    (final_backdrop_screen_rect.loc.x - capture_origin_physical.x) as f64,
                    (final_backdrop_screen_rect.loc.y - capture_origin_physical.y) as f64,
                )),
                (
                    final_backdrop_screen_rect.size.w as f64,
                    final_backdrop_screen_rect.size.h as f64,
                )
                    .into(),
            );
            let output_size = (
                final_backdrop_screen_rect.size.w,
                final_backdrop_screen_rect.size.h,
            );
            let texture = crate::backend::shader_effect::apply_effect_pipeline_cached_for_key(
                renderer,
                format!(
                    "winit:window-backdrop:{}:{}",
                    decoration.snapshot.id, cached.stable_key
                ),
                input_texture,
                xray_texture,
                crate::backend::visual::logical_size_to_physical_buffer_size(
                    actual_capture_geo.size.w,
                    actual_capture_geo.size.h,
                    scale,
                ),
                Some(sample_region),
                Some(output_size),
                &cached.shader,
            )
            .ok();
            if texture.is_none() {
                return None;
            }
            let texture = texture?;
            let commit_counter = state
                .window_decorations
                .get(window)
                .and_then(|d| d.backdrop_cache.get(&cached.stable_key))
                .map(|existing| {
                    let mut counter = existing.commit_counter;
                    counter.increment();
                    counter
                })
                .unwrap_or_default();
            if let Some(window_decoration) = state.window_decorations.get_mut(window) {
                window_decoration.backdrop_cache.insert(
                    cached.stable_key.clone(),
                    crate::backend::shader_effect::CachedBackdropTexture {
                        signature,
                        texture: texture.clone(),
                        id: smithay::backend::renderer::element::Id::new(),
                        commit_counter,
                        sub_elements: std::collections::HashMap::new(),
                    },
                );
            }
            let local_rect = Rectangle::new(
                smithay::utils::Point::from((
                    display_rect.x - root_rect.x,
                    display_rect.y - root_rect.y,
                )),
                (display_rect.width, display_rect.height).into(),
            );
            let clip_rect = {
                let precise_clip = display_rect_precise
                    .zip(cached.clip_rect_precise.map(|clip| {
                        if apply_visual_transform {
                            crate::backend::visual::transformed_precise_rect(
                                clip,
                                decoration.layout.root.rect,
                                decoration.visual_transform,
                            )
                        } else {
                            clip
                        }
                    }))
                    .map(|(rect, clip)| {
                        crate::backend::visual::snapped_precise_logical_rect_in_root_frame_area_space(
                            clip,
                            rect,
                            display_rect.width,
                            display_rect.height,
                            root_rect,
                            output_geo,
                            scale,
                        )
                    });
                precise_clip.or_else(|| {
                    cached.clip_rect.map(|clip_rect| {
                        let transformed_clip = if apply_visual_transform {
                            crate::backend::visual::transformed_rect(
                                clip_rect,
                                decoration.layout.root.rect,
                                decoration.visual_transform,
                            )
                        } else {
                            clip_rect
                        };
                        let rect = display_rect_precise.unwrap_or_else(|| {
                            crate::backend::visual::precise_rect_from_logical(display_rect)
                        });
                        crate::backend::visual::snapped_precise_logical_rect_in_root_frame_area_space(
                            crate::backend::visual::precise_rect_from_logical(transformed_clip),
                            rect,
                            display_rect.width,
                            display_rect.height,
                            root_rect,
                            output_geo,
                            scale,
                        )
                    })
                })
            };
            let local_sample_rect = Rectangle::new(
                smithay::utils::Point::from((
                    source_effect_rect.x - output_geo.loc.x,
                    source_effect_rect.y - output_geo.loc.y,
                )),
                (source_effect_rect.width, source_effect_rect.height).into(),
            );
            let local_capture_rect = local_sample_rect;
            crate::backend::shader_effect::backdrop_shader_element_with_geometry(
                renderer,
                state
                    .window_decorations
                    .get(window)
                    .and_then(|d| d.backdrop_cache.get(&cached.stable_key))
                    .map(|cached| cached.id.clone())
                    .unwrap_or_else(smithay::backend::renderer::element::Id::new),
                state
                    .window_decorations
                    .get(window)
                    .and_then(|d| d.backdrop_cache.get(&cached.stable_key))
                    .map(|cached| cached.commit_counter)
                    .unwrap_or_default(),
                texture,
                local_rect,
                geometry,
                local_sample_rect,
                local_capture_rect,
                &cached.shader,
                alpha,
                scale.x as f32,
                [0.0, 0.0],
                clip_rect,
                cached
                    .clip_radius_precise
                    .unwrap_or(cached.clip_radius as f32),
                format!(
                    "window-backdrop:{}:{}",
                    decoration.snapshot.id, cached.stable_key
                ),
            )
            .ok()
            .map(|element| {
                if std::env::var_os("SHOJI_FIREFOX_BACKDROP_DEBUG").is_some() {
                    tracing::info!(
                        window_id = %decoration.snapshot.id,
                        title = %decoration.snapshot.title,
                        app_id = ?decoration.snapshot.app_id,
                        stable_key = %cached.stable_key,
                        local_rect = ?local_rect,
                        local_sample_rect = ?local_sample_rect,
                        local_capture_rect = ?local_capture_rect,
                        sample_region = ?sample_region,
                        final_backdrop_screen_rect = ?final_backdrop_screen_rect,
                        geometry = ?geometry,
                        from_cache = false,
                        "backdrop debug: window shader element"
                    );
                }
                (cached.order, element, render_as_backdrop)
            })
        })
        .collect()
}

fn protocol_background_effect_rects_for_window(
    state: &ShojiWM,
    window: &smithay::desktop::Window,
) -> Vec<crate::ssd::LogicalRect> {
    let Some(decoration) = state.window_decorations.get(window) else {
        return Vec::new();
    };
    let WindowSurface::Wayland(surface) = window.underlying_surface() else {
        return Vec::new();
    };
    let wl_surface = surface.wl_surface();
    let blur_region = compositor::with_states(wl_surface, |states| {
        let mut cached = states
            .cached_state
            .get::<BackgroundEffectSurfaceCachedState>();
        cached.current().blur_region.clone()
    });
    let Some(region) = blur_region else {
        return Vec::new();
    };

    let rects = crate::backend::window::region_rects_within_bounds(
        &region,
        crate::ssd::LogicalRect::new(
            0,
            0,
            decoration.client_rect.width,
            decoration.client_rect.height,
        ),
    )
    .into_iter()
    .map(|rect| {
        crate::ssd::LogicalRect::new(
            decoration.client_rect.x + rect.x,
            decoration.client_rect.y + rect.y,
            rect.width,
            rect.height,
        )
    })
    .collect::<Vec<_>>();

    if std::env::var_os("SHOJI_FIREFOX_BACKDROP_DEBUG").is_some() {
        let (surface_geometry, buffer_scale, buffer_delta) =
            compositor::with_states(wl_surface, |states| {
                let geometry = states
                    .cached_state
                    .get::<smithay::wayland::shell::xdg::SurfaceCachedState>()
                    .current()
                    .geometry;
                let mut attrs = states
                    .cached_state
                    .get::<smithay::wayland::compositor::SurfaceAttributes>();
                let attrs = attrs.current();
                (geometry, attrs.buffer_scale, attrs.buffer_delta)
            });
        tracing::info!(
            window_id = %decoration.snapshot.id,
            title = %decoration.snapshot.title,
            app_id = ?decoration.snapshot.app_id,
            client_rect = ?decoration.client_rect,
            root_rect = ?decoration.layout.root.rect,
            surface_geometry = ?surface_geometry,
            buffer_scale,
            buffer_delta = ?buffer_delta,
            blur_region_rects = ?rects,
            "backdrop debug: protocol window rects"
        );
    }

    rects
}

fn protocol_background_effect_rects_for_layer(
    output: &Output,
    layer_surface: &smithay::desktop::LayerSurface,
) -> Vec<crate::ssd::LogicalRect> {
    let wl_surface = layer_surface.wl_surface();
    let blur_region = compositor::with_states(wl_surface, |states| {
        let mut cached = states
            .cached_state
            .get::<BackgroundEffectSurfaceCachedState>();
        cached.current().blur_region.clone()
    });
    let Some(region) = blur_region else {
        return Vec::new();
    };
    let map = smithay::desktop::layer_map_for_output(output);
    let Some(layer_geo) = map.layer_geometry(layer_surface) else {
        return Vec::new();
    };
    drop(map);
    let output_loc = output.current_location();

    let rects = crate::backend::window::region_rects_within_bounds(
        &region,
        crate::ssd::LogicalRect::new(0, 0, layer_geo.size.w, layer_geo.size.h),
    )
    .into_iter()
    .map(|rect| {
        crate::ssd::LogicalRect::new(
            output_loc.x + layer_geo.loc.x + rect.x,
            output_loc.y + layer_geo.loc.y + rect.y,
            rect.width,
            rect.height,
        )
    })
    .collect::<Vec<_>>();

    if std::env::var_os("SHOJI_FIREFOX_BACKDROP_DEBUG").is_some() {
        tracing::info!(
            layer_surface = ?layer_surface.wl_surface().id(),
            output = %output.name(),
            layer_geo = ?layer_geo,
            output_loc = ?output_loc,
            blur_region_rects = ?rects,
            "backdrop debug: protocol layer rects"
        );
    }

    rects
}

fn collect_window_source_damage(
    state: &ShojiWM,
    windows: impl IntoIterator<Item = smithay::desktop::Window>,
) -> Vec<crate::state::OwnedDamageRect> {
    let owners = windows
        .into_iter()
        .filter_map(|window| {
            state
                .window_decorations
                .get(&window)
                .map(|decoration| decoration.snapshot.id.clone())
        })
        .collect::<std::collections::HashSet<_>>();
    state
        .window_source_damage
        .iter()
        .filter(|entry| owners.contains(&entry.owner))
        .cloned()
        .collect()
}

fn collect_layer_source_damage(
    state: &ShojiWM,
    layers: impl IntoIterator<Item = smithay::desktop::LayerSurface>,
    lower: bool,
) -> Vec<crate::state::OwnedDamageRect> {
    let owners = layers
        .into_iter()
        .map(|layer| layer.wl_surface().id().protocol_id().to_string())
        .collect::<std::collections::HashSet<_>>();
    let entries = if lower {
        &state.lower_layer_source_damage
    } else {
        &state.upper_layer_source_damage
    };
    entries
        .iter()
        .filter(|entry| owners.contains(&entry.owner))
        .cloned()
        .collect()
}

fn logical_rects_intersect(lhs: crate::ssd::LogicalRect, rhs: crate::ssd::LogicalRect) -> bool {
    let left = lhs.x.max(rhs.x);
    let top = lhs.y.max(rhs.y);
    let right = (lhs.x + lhs.width).min(rhs.x + rhs.width);
    let bottom = (lhs.y + lhs.height).min(rhs.y + rhs.height);
    right > left && bottom > top
}

fn contributor_window_scene_rect(
    state: &ShojiWM,
    window: &smithay::desktop::Window,
) -> Option<(String, crate::ssd::LogicalRect)> {
    if let Some(decoration) = state.window_decorations.get(window) {
        return Some((
            decoration.snapshot.id.clone(),
            transformed_root_rect(decoration.layout.root.rect, decoration.visual_transform),
        ));
    }
    let location = state.space.element_location(window)?;
    let bbox = window.bbox();
    Some((
        window
            .toplevel()
            .map(|surface| surface.wl_surface().id().protocol_id().to_string())
            .unwrap_or_else(|| "unknown".into()),
        crate::ssd::LogicalRect::new(
            location.x + bbox.loc.x,
            location.y + bbox.loc.y,
            bbox.size.w,
            bbox.size.h,
        ),
    ))
}

fn hash_window_scene_contributors(
    hasher: &mut std::collections::hash_map::DefaultHasher,
    state: &ShojiWM,
    windows: &[smithay::desktop::Window],
    effect_rect: crate::ssd::LogicalRect,
) {
    for window in windows {
        let Some((window_id, rect)) = contributor_window_scene_rect(state, window) else {
            continue;
        };
        if !logical_rects_intersect(rect, effect_rect) {
            continue;
        }
        window_id.hash(hasher);
        (rect.x, rect.y, rect.width, rect.height).hash(hasher);
    }
}

fn hash_layer_scene_contributors(
    hasher: &mut std::collections::hash_map::DefaultHasher,
    output: &Output,
    layers: &[smithay::desktop::LayerSurface],
    effect_rect: crate::ssd::LogicalRect,
) {
    let map = layer_map_for_output(output);
    let output_loc = output.current_location();
    for layer in layers {
        let Some(geo) = map.layer_geometry(layer) else {
            continue;
        };
        let rect = crate::ssd::LogicalRect::new(
            output_loc.x + geo.loc.x,
            output_loc.y + geo.loc.y,
            geo.size.w,
            geo.size.h,
        );
        if !logical_rects_intersect(rect, effect_rect) {
            continue;
        }
        layer.wl_surface().id().protocol_id().hash(hasher);
        (rect.x, rect.y, rect.width, rect.height).hash(hasher);
    }
}

fn layer_surface_scene_elements_for_capture(
    renderer: &mut GlesRenderer,
    output: &Output,
    _capture_geo: Rectangle<i32, Logical>,
    capture_origin_physical: Point<i32, smithay::utils::Physical>,
    scale: smithay::utils::Scale<f64>,
    layer_surface: &smithay::desktop::LayerSurface,
) -> Vec<WinitRenderElements> {
    let capture_visual = WindowVisualState {
        origin: smithay::utils::Point::from((0, 0)),
        scale: smithay::utils::Scale::from((1.0, 1.0)),
        translation: crate::backend::visual::logical_point_to_relative_physical_point_from_origin(
            output.current_location(),
            output.current_location(),
            capture_origin_physical,
            scale,
        ),
        opacity: 1.0,
    };
    transform_window_elements(
        window_render::layer_surface_elements(renderer, output, layer_surface, scale, 1.0),
        capture_visual,
        WinitRenderElements::Window,
        WinitRenderElements::TransformedWindow,
    )
}

fn lower_layer_scene_elements(
    renderer: &mut GlesRenderer,
    state: &mut ShojiWM,
    output: &Output,
    output_geo: Rectangle<i32, Logical>,
    scale: smithay::utils::Scale<f64>,
    _windows_top_to_bottom: &[smithay::desktop::Window],
) -> Vec<WinitRenderElements> {
    let (_, lower_layers) = window_render::layer_surfaces_for_output(output);

    let mut elements = Vec::new();
    for (index, layer_surface) in lower_layers.iter().enumerate() {
        let layer_id = crate::ssd::layer_runtime_id(layer_surface);
        let root_elements =
            window_render::layer_surface_root_elements(renderer, output, layer_surface, scale, 1.0)
                .into_iter()
                .map(WinitRenderElements::Window)
                .collect::<Vec<_>>();
        let effects = state.configured_layer_effects.get(&layer_id).cloned();
        if let (Some(effects), Some(layer_rect)) =
            (effects, layer_surface_logical_rect(output, layer_surface))
        {
            let capture_elements =
                window_render::layer_surface_elements(renderer, output, layer_surface, scale, 1.0)
                    .into_iter()
                    .map(WinitRenderElements::Window)
                    .collect::<Vec<_>>();
            elements.extend(compose_layer_source_effects(
                renderer,
                output,
                output_geo,
                scale,
                &layer_id,
                layer_rect,
                &effects,
                root_elements,
                &capture_elements,
                &mut state.layer_effect_cache,
            ));
        } else {
            elements.extend(root_elements);
        }
        let custom_background = state
            .configured_layer_effects
            .get(&layer_id)
            .and_then(|effects| effects.behind.as_ref())
            .filter(|effect| effect.effect.is_backdrop())
            .cloned();
        let custom_config =
            custom_background
                .as_ref()
                .map(|effect| crate::ssd::BackgroundEffectConfig {
                    effect: effect.effect.clone(),
                });
        let config = custom_config.or_else(|| state.configured_background_effect.clone());
        let Some(config) = config.as_ref() else {
            continue;
        };
        let rects = if let Some(effect) = custom_background.as_ref() {
            layer_surface_logical_rect(output, layer_surface)
                .map(|rect| vec![expand_effect_rect(rect, effect.outsets)])
                .unwrap_or_default()
        } else {
            protocol_background_effect_rects_for_layer(output, layer_surface)
        };
        let Some(effect_rect) = crate::backend::window::bounding_box_for_rects(&rects) else {
            continue;
        };
        let stable_key = format!(
            "__layer_background_effect_{}_{}_{}_{}x{}",
            output.name(),
            layer_surface.wl_surface().id().protocol_id(),
            index,
            effect_rect.width,
            effect_rect.height
        );
        let blur_padding = config.effect.capture_padding.max(0);
        let capture_geo = Rectangle::new(
            smithay::utils::Point::from((
                effect_rect.x - blur_padding,
                effect_rect.y - blur_padding,
            )),
            (
                effect_rect.width + blur_padding * 2,
                effect_rect.height + blur_padding * 2,
            )
                .into(),
        );
        let capture_origin_physical =
            crate::backend::visual::logical_point_to_physical_point_global_edges(
                capture_geo.loc,
                output_geo.loc,
                scale,
            );
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        stable_key.hash(&mut hasher);
        state.lower_layer_scene_generation.hash(&mut hasher);
        format!("{:?}", config.effect).hash(&mut hasher);
        (
            effect_rect.x,
            effect_rect.y,
            effect_rect.width,
            effect_rect.height,
            capture_geo.loc.x,
            capture_geo.loc.y,
            capture_geo.size.w,
            capture_geo.size.h,
        )
            .hash(&mut hasher);
        let signature = hasher.finish();
        let mut relevant_source_damage =
            collect_layer_source_damage(state, lower_layers.iter().skip(index + 1).cloned(), true);
        if config.effect.uses_layer_source_input() {
            relevant_source_damage.extend(collect_layer_source_damage(
                state,
                std::iter::once(layer_surface.clone()),
                true,
            ));
        }
        let source_damage_hit = crate::backend::shader_effect::source_damage_intersects_rect(
            &config.effect,
            Rectangle::new(
                smithay::utils::Point::from((effect_rect.x, effect_rect.y)),
                (effect_rect.width, effect_rect.height).into(),
            ),
            &relevant_source_damage,
        );
        let captured_local_rect = Rectangle::new(
            smithay::utils::Point::from((
                effect_rect.x - output_geo.loc.x,
                effect_rect.y - output_geo.loc.y,
            )),
            (effect_rect.width, effect_rect.height).into(),
        );
        if !matches!(
            config.effect.invalidate_policy(),
            crate::ssd::EffectInvalidationPolicy::Always
        ) && !source_damage_hit
        {
            if let Some(existing) = state
                .layer_backdrop_cache
                .get(&stable_key)
                .filter(|existing| existing.signature == signature)
                .cloned()
            {
                for rect in rects {
                    let rect_key = format!(
                        "{}:{}:{}:{}:{}",
                        layer_id, rect.x, rect.y, rect.width, rect.height
                    );
                    let rect_local = Rectangle::new(
                        smithay::utils::Point::from((
                            rect.x - output_geo.loc.x,
                            rect.y - output_geo.loc.y,
                        )),
                        (rect.width, rect.height).into(),
                    );
                    if let Ok(element) = crate::backend::shader_effect::backdrop_shader_element(
                        renderer,
                        existing
                            .sub_elements
                            .get(&rect_key)
                            .map(|entry| entry.id.clone())
                            .unwrap_or_else(smithay::backend::renderer::element::Id::new),
                        existing
                            .sub_elements
                            .get(&rect_key)
                            .map(|entry| entry.commit_counter)
                            .unwrap_or_default(),
                        existing.texture.clone(),
                        rect_local,
                        rect_local,
                        captured_local_rect,
                        &config.effect,
                        1.0,
                        scale.x as f32,
                        None,
                        0.0,
                        format!("layer-lower:{}:{}", output.name(), rect_key),
                    ) {
                        elements.push(WinitRenderElements::Backdrop(element));
                    }
                }
                continue;
            }
        }
        let mut backdrop_scene: Vec<WinitRenderElements> = Vec::new();
        for lower_layer in lower_layers.iter().skip(index + 1) {
            backdrop_scene.extend(layer_surface_scene_elements_for_capture(
                renderer,
                output,
                capture_geo,
                capture_origin_physical,
                scale,
                lower_layer,
            ));
        }
        if backdrop_scene.is_empty() {
            continue;
        }
        let mut backdrop_tracker = smithay::backend::renderer::damage::OutputDamageTracker::new(
            (0, 0),
            1.0,
            smithay::utils::Transform::Normal,
        );
        let capture_size = crate::backend::visual::logical_size_to_physical_buffer_size(
            capture_geo.size.w,
            capture_geo.size.h,
            scale,
        );
        crate::backend::shader_effect::record_snapshot_fallback(
            "winit-layer-lower",
            capture_size,
            backdrop_scene.len(),
        );
        let snapshot = crate::backend::shader_effect::with_gpu_timing_renderer_span(
            renderer,
            "backdrop-scene-capture",
            capture_size,
            |renderer| {
                snapshot::capture_snapshot(
                    renderer,
                    None,
                    &mut backdrop_tracker,
                    crate::ssd::LogicalRect::new(
                        capture_geo.loc.x,
                        capture_geo.loc.y,
                        capture_geo.size.w,
                        capture_geo.size.h,
                    ),
                    0,
                    true,
                    scale,
                    &backdrop_scene,
                )
            },
        )
        .ok()
        .flatten();
        let Some(snapshot) = snapshot else {
            continue;
        };
        let backdrop_texture = if config.effect.uses_backdrop_input() {
            Some(snapshot.texture.clone())
        } else {
            None
        };
        let xray_texture = if config.effect.uses_xray_backdrop_input() {
            Some(snapshot.texture.clone())
        } else {
            None
        };
        let layer_source_texture = config
            .effect
            .uses_layer_source_input()
            .then(|| {
                let layer_source_geo = Rectangle::new(
                    smithay::utils::Point::from((effect_rect.x, effect_rect.y)),
                    (effect_rect.width, effect_rect.height).into(),
                );
                let layer_source_origin =
                    crate::backend::visual::logical_point_to_physical_point_global_edges(
                        layer_source_geo.loc,
                        output_geo.loc,
                        scale,
                    );
                let scene = layer_surface_scene_elements_for_capture(
                    renderer,
                    output,
                    layer_source_geo,
                    layer_source_origin,
                    scale,
                    layer_surface,
                );
                capture_scene_texture_for_effect(
                    renderer,
                    "winit-layer-lower-source",
                    layer_source_geo,
                    scale,
                    &scene,
                )
            })
            .flatten();
        // Skip the effect this frame when the layer source could not be
        // captured (empty scene / zero-sized geometry); running the pipeline
        // without it would fail inside resolve_effect_input.
        if config.effect.uses_layer_source_input() && layer_source_texture.is_none() {
            continue;
        }
        let input_texture = backdrop_texture
            .clone()
            .or_else(|| xray_texture.clone())
            .unwrap_or(snapshot.texture);
        let input_size = crate::backend::visual::logical_size_to_physical_buffer_size(
            capture_geo.size.w,
            capture_geo.size.h,
            scale,
        );
        let sample_region = Some(
            crate::backend::visual::logical_rect_to_physical_buffer_rect_f64(
                effect_rect,
                capture_geo.loc,
                scale,
            ),
        );
        let output_size = Some(
            crate::backend::visual::logical_size_to_physical_buffer_size(
                effect_rect.width,
                effect_rect.height,
                scale,
            ),
        );
        let texture = if let Some(layer_source_texture) = layer_source_texture {
            crate::backend::shader_effect::apply_effect_pipeline_cached_for_key_with_layer_source(
                renderer,
                format!("winit:layer-lower:{}", stable_key),
                input_texture,
                xray_texture,
                layer_source_texture,
                input_size,
                sample_region,
                output_size,
                &config.effect,
            )
        } else {
            crate::backend::shader_effect::apply_effect_pipeline_cached_for_key(
                renderer,
                format!("winit:layer-lower:{}", stable_key),
                input_texture,
                xray_texture,
                input_size,
                sample_region,
                output_size,
                &config.effect,
            )
        }
        .ok();
        let Some(texture) = texture else {
            continue;
        };
        let mut sub_elements = state
            .layer_backdrop_cache
            .get(&stable_key)
            .map(|existing| existing.sub_elements.clone())
            .unwrap_or_default();
        let had_existing = state.layer_backdrop_cache.contains_key(&stable_key);
        for rect in &rects {
            let rect_key = format!(
                "{}:{}:{}:{}:{}",
                layer_id, rect.x, rect.y, rect.width, rect.height
            );
            let entry = sub_elements.entry(rect_key).or_default();
            if had_existing {
                entry.commit_counter.increment();
            }
        }
        state.layer_backdrop_cache.insert(
            stable_key.clone(),
            crate::backend::shader_effect::CachedBackdropTexture {
                signature,
                texture: texture.clone(),
                id: state
                    .layer_backdrop_cache
                    .get(&stable_key)
                    .map(|cached| cached.id.clone())
                    .unwrap_or_else(smithay::backend::renderer::element::Id::new),
                commit_counter: state
                    .layer_backdrop_cache
                    .get(&stable_key)
                    .map(|existing| {
                        let mut counter = existing.commit_counter;
                        counter.increment();
                        counter
                    })
                    .unwrap_or_default(),
                sub_elements: state
                    .layer_backdrop_cache
                    .get(&stable_key)
                    .map(|_| sub_elements.clone())
                    .unwrap_or(sub_elements),
            },
        );
        for rect in rects {
            let rect_key = format!(
                "{}:{}:{}:{}:{}",
                layer_id, rect.x, rect.y, rect.width, rect.height
            );
            let rect_local = Rectangle::new(
                smithay::utils::Point::from((rect.x - output_geo.loc.x, rect.y - output_geo.loc.y)),
                (rect.width, rect.height).into(),
            );
            if let Ok(element) = crate::backend::shader_effect::backdrop_shader_element(
                renderer,
                state
                    .layer_backdrop_cache
                    .get(&stable_key)
                    .and_then(|cached| cached.sub_elements.get(&rect_key))
                    .map(|entry| entry.id.clone())
                    .unwrap_or_else(smithay::backend::renderer::element::Id::new),
                state
                    .layer_backdrop_cache
                    .get(&stable_key)
                    .and_then(|cached| cached.sub_elements.get(&rect_key))
                    .map(|entry| entry.commit_counter)
                    .unwrap_or_default(),
                texture.clone(),
                rect_local,
                rect_local,
                captured_local_rect,
                &config.effect,
                1.0,
                scale.x as f32,
                None,
                0.0,
                format!("layer-lower:{}:{}", output.name(), rect_key),
            ) {
                elements.push(WinitRenderElements::Backdrop(element));
            }
        }
    }
    elements
}

fn configured_background_effect_elements_for_layer(
    renderer: &mut GlesRenderer,
    state: &mut ShojiWM,
    output: &Output,
    output_geo: Rectangle<i32, Logical>,
    scale: smithay::utils::Scale<f64>,
    windows_top_to_bottom: &[smithay::desktop::Window],
    // Top/Overlay layers stacked below `layer_surface` (front-to-back). They
    // sit above all toplevel windows, so backdrop captures must include them
    // or an overlay's blur would miss any overlay/top layer behind it.
    upper_layers_below: &[smithay::desktop::LayerSurface],
    layer_surface: &smithay::desktop::LayerSurface,
    alpha: f32,
    custom_background: Option<&crate::ssd::WindowEffectSlot>,
) -> Vec<WinitRenderElements> {
    let layer_id = crate::ssd::layer_runtime_id(layer_surface);
    let custom_config = custom_background.map(|effect| crate::ssd::BackgroundEffectConfig {
        effect: effect.effect.clone(),
    });
    let Some(config) = custom_config.or_else(|| state.configured_background_effect.clone()) else {
        return Vec::new();
    };
    let rects = if let Some(effect) = custom_background {
        layer_surface_logical_rect(output, layer_surface)
            .map(|rect| vec![expand_effect_rect(rect, effect.outsets)])
            .unwrap_or_default()
    } else {
        protocol_background_effect_rects_for_layer(output, layer_surface)
    };
    if rects.is_empty() {
        return Vec::new();
    }

    let Some(effect_rect) = crate::backend::window::bounding_box_for_rects(&rects) else {
        return Vec::new();
    };
    let blur_padding = config.effect.capture_padding.max(0);
    let capture_geo = Rectangle::new(
        smithay::utils::Point::from((effect_rect.x - blur_padding, effect_rect.y - blur_padding)),
        (
            effect_rect.width + blur_padding * 2,
            effect_rect.height + blur_padding * 2,
        )
            .into(),
    );
    let actual_capture_geo = capture_geo.intersection(output_geo).unwrap_or(capture_geo);
    let capture_origin_physical =
        crate::backend::visual::logical_point_to_physical_point_global_edges(
            actual_capture_geo.loc,
            output_geo.loc,
            scale,
        );
    let (_, lower_layers) = window_render::layer_surfaces_for_output(output);
    let uses_backdrop = config.effect.uses_backdrop_input();
    let uses_xray = config.effect.uses_xray_backdrop_input();
    if std::env::var_os("SHOJI_FIREFOX_BACKDROP_DEBUG").is_some() {
        tracing::info!(
            layer_surface = ?layer_surface.wl_surface().id(),
            layer_id = %layer_id,
            output = %output.name(),
            effect_rect = ?effect_rect,
            output_geo = ?output_geo,
            blur_padding,
            capture_geo = ?capture_geo,
            actual_capture_geo = ?actual_capture_geo,
            capture_origin_physical = ?capture_origin_physical,
            scale = ?scale,
            uses_backdrop,
            uses_xray,
            "backdrop debug: layer effect rects"
        );
    }
    let relevant_source_damage = {
        let mut entries = Vec::new();
        if uses_backdrop {
            entries.extend(collect_window_source_damage(
                state,
                windows_top_to_bottom.iter().cloned(),
            ));
        }
        if uses_backdrop || uses_xray {
            entries.extend(collect_layer_source_damage(
                state,
                lower_layers.iter().cloned(),
                true,
            ));
        }
        if uses_backdrop {
            entries.extend(collect_layer_source_damage(
                state,
                upper_layers_below.iter().cloned(),
                false,
            ));
        }
        if config.effect.uses_layer_source_input() {
            entries.extend(collect_layer_source_damage(
                state,
                std::iter::once(layer_surface.clone()),
                false,
            ));
        }
        entries
    };

    let backdrop_texture = if config.effect.uses_backdrop_input() {
        let mut backdrop_scene: Vec<WinitRenderElements> = Vec::new();
        // Upper layers below this one render above every toplevel window, so
        // they go first in the front-to-back capture scene.
        for upper_layer in upper_layers_below {
            backdrop_scene.extend(layer_surface_scene_elements_for_capture(
                renderer,
                output,
                actual_capture_geo,
                capture_origin_physical,
                scale,
                upper_layer,
            ));
        }
        for lower_window in windows_top_to_bottom {
            backdrop_scene.extend(window_scene_elements_for_capture(
                renderer,
                state,
                output_geo.loc,
                actual_capture_geo,
                capture_origin_physical,
                scale,
                lower_window,
            ));
        }
        let (_, lower_layer_elements) =
            window_render::layer_elements_for_output(renderer, output, scale, 1.0);
        let capture_visual = WindowVisualState {
            origin: smithay::utils::Point::from((0, 0)),
            scale: smithay::utils::Scale::from((1.0, 1.0)),
            translation: Point::from((-capture_origin_physical.x, -capture_origin_physical.y)),
            opacity: 1.0,
        };
        backdrop_scene.extend(
            transform_window_elements(
                lower_layer_elements,
                capture_visual,
                WinitRenderElements::Window,
                WinitRenderElements::TransformedWindow,
            )
            .into_iter(),
        );
        capture_scene_texture_for_effect(
            renderer,
            "winit-layer-top-backdrop",
            actual_capture_geo,
            scale,
            &backdrop_scene,
        )
    } else {
        None
    };
    let xray_texture = if config.effect.uses_xray_backdrop_input() {
        let mut xray_scene: Vec<WinitRenderElements> = Vec::new();
        for lower_layer in &lower_layers {
            xray_scene.extend(layer_surface_scene_elements_for_capture(
                renderer,
                output,
                actual_capture_geo,
                capture_origin_physical,
                scale,
                lower_layer,
            ));
        }
        capture_scene_texture_for_effect(
            renderer,
            "winit-layer-top-xray",
            actual_capture_geo,
            scale,
            &xray_scene,
        )
    } else {
        None
    };
    let Some(input_texture) = backdrop_texture
        .clone()
        .or_else(|| xray_texture.clone())
        .or_else(|| crate::backend::shader_effect::solid_white_texture(renderer).ok())
    else {
        return Vec::new();
    };
    let layer_id = layer_surface.wl_surface().id().protocol_id();
    let stable_key = format!(
        "__layer_background_effect_{}_{}_top_{}x{}",
        output.name(),
        layer_id,
        effect_rect.width,
        effect_rect.height
    );
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    stable_key.hash(&mut hasher);
    if uses_backdrop || uses_xray {
        state.lower_layer_scene_generation.hash(&mut hasher);
    }
    if uses_backdrop {
        hash_window_scene_contributors(&mut hasher, state, windows_top_to_bottom, effect_rect);
    }
    if uses_backdrop || uses_xray {
        hash_layer_scene_contributors(&mut hasher, output, &lower_layers, effect_rect);
    }
    if uses_backdrop {
        hash_layer_scene_contributors(&mut hasher, output, upper_layers_below, effect_rect);
    }
    format!("{:?}", config.effect).hash(&mut hasher);
    (
        effect_rect.x,
        effect_rect.y,
        effect_rect.width,
        effect_rect.height,
        capture_geo.loc.x,
        capture_geo.loc.y,
        capture_geo.size.w,
        capture_geo.size.h,
    )
        .hash(&mut hasher);
    let signature = hasher.finish();
    let source_damage_hit = crate::backend::shader_effect::source_damage_intersects_rect(
        &config.effect,
        Rectangle::new(
            smithay::utils::Point::from((effect_rect.x, effect_rect.y)),
            (effect_rect.width, effect_rect.height).into(),
        ),
        &relevant_source_damage,
    );
    let captured_local_rect = Rectangle::new(
        smithay::utils::Point::from((
            effect_rect.x - output_geo.loc.x,
            effect_rect.y - output_geo.loc.y,
        )),
        (effect_rect.width, effect_rect.height).into(),
    );
    if !matches!(
        config.effect.invalidate_policy(),
        crate::ssd::EffectInvalidationPolicy::Always
    ) && !source_damage_hit
    {
        if let Some(existing) = state
            .layer_backdrop_cache
            .get(&stable_key)
            .filter(|existing| existing.signature == signature)
            .cloned()
        {
            return rects
                .into_iter()
                .filter_map(|rect| {
                    let rect_key = format!(
                        "{}:{}:{}:{}:{}",
                        layer_id, rect.x, rect.y, rect.width, rect.height
                    );
                    let rect_local = Rectangle::new(
                        smithay::utils::Point::from((
                            rect.x - output_geo.loc.x,
                            rect.y - output_geo.loc.y,
                        )),
                        (rect.width, rect.height).into(),
                    );
                    if std::env::var_os("SHOJI_FIREFOX_BACKDROP_DEBUG").is_some() {
                        tracing::info!(
                            layer_surface = ?layer_surface.wl_surface().id(),
                            output = %output.name(),
                            rect = ?rect,
                            rect_local = ?rect_local,
                            captured_local_rect = ?captured_local_rect,
                            from_cache = true,
                            "backdrop debug: layer effect element"
                        );
                    }
                    crate::backend::shader_effect::backdrop_shader_element(
                        renderer,
                        existing
                            .sub_elements
                            .get(&rect_key)
                            .map(|entry| entry.id.clone())
                            .unwrap_or_else(smithay::backend::renderer::element::Id::new),
                        existing
                            .sub_elements
                            .get(&rect_key)
                            .map(|entry| entry.commit_counter)
                            .unwrap_or_default(),
                        existing.texture.clone(),
                        rect_local,
                        rect_local,
                        captured_local_rect,
                        &config.effect,
                        alpha,
                        scale.x as f32,
                        None,
                        0.0,
                        format!("layer-lower:{}:{}", output.name(), rect_key),
                    )
                    .ok()
                    .map(WinitRenderElements::Backdrop)
                })
                .collect();
        }
    }
    let layer_source_texture = config
        .effect
        .uses_layer_source_input()
        .then(|| {
            let layer_source_geo = Rectangle::new(
                smithay::utils::Point::from((effect_rect.x, effect_rect.y)),
                (effect_rect.width, effect_rect.height).into(),
            );
            let layer_source_origin =
                crate::backend::visual::logical_point_to_physical_point_global_edges(
                    layer_source_geo.loc,
                    output_geo.loc,
                    scale,
                );
            let scene = layer_surface_scene_elements_for_capture(
                renderer,
                output,
                layer_source_geo,
                layer_source_origin,
                scale,
                layer_surface,
            );
            capture_scene_texture_for_effect(
                renderer,
                "winit-layer-top-source",
                layer_source_geo,
                scale,
                &scene,
            )
        })
        .flatten();
    // Skip the effect this frame when the layer source could not be captured
    // (empty scene / zero-sized geometry); running the pipeline without it
    // would fail inside resolve_effect_input.
    if config.effect.uses_layer_source_input() && layer_source_texture.is_none() {
        return Vec::new();
    }
    let input_size = crate::backend::visual::logical_size_to_physical_buffer_size(
        actual_capture_geo.size.w,
        actual_capture_geo.size.h,
        scale,
    );
    let sample_region = Some(
        crate::backend::visual::logical_rect_to_physical_buffer_rect_f64(
            effect_rect,
            actual_capture_geo.loc,
            scale,
        ),
    );
    let output_size = Some(
        crate::backend::visual::logical_size_to_physical_buffer_size(
            effect_rect.width,
            effect_rect.height,
            scale,
        ),
    );
    let texture = if let Some(layer_source_texture) = layer_source_texture {
        crate::backend::shader_effect::apply_effect_pipeline_cached_for_key_with_layer_source(
            renderer,
            format!("winit:layer-top:{}", stable_key),
            input_texture,
            xray_texture,
            layer_source_texture,
            input_size,
            sample_region,
            output_size,
            &config.effect,
        )
    } else {
        crate::backend::shader_effect::apply_effect_pipeline_cached_for_key(
            renderer,
            format!("winit:layer-top:{}", stable_key),
            input_texture,
            xray_texture,
            input_size,
            sample_region,
            output_size,
            &config.effect,
        )
    }
    .ok();
    let Some(texture) = texture else {
        return Vec::new();
    };
    if std::env::var_os("SHOJI_FIREFOX_BACKDROP_DEBUG").is_some() {
        tracing::info!(
            layer_surface = ?layer_surface.wl_surface().id(),
            output = %output.name(),
            effect_rect = ?effect_rect,
            sample_region = ?crate::backend::visual::logical_rect_to_physical_buffer_rect_f64(
                effect_rect,
                actual_capture_geo.loc,
                scale,
            ),
            output_size = ?crate::backend::visual::logical_size_to_physical_buffer_size(
                effect_rect.width,
                effect_rect.height,
                scale,
            ),
            captured_local_rect = ?Rectangle::<i32, Logical>::new(
                smithay::utils::Point::from((
                    effect_rect.x - output_geo.loc.x,
                    effect_rect.y - output_geo.loc.y,
                )),
                (effect_rect.width, effect_rect.height).into(),
            ),
            from_cache = false,
            "backdrop debug: layer effect texture"
        );
    }
    let mut sub_elements = state
        .layer_backdrop_cache
        .get(&stable_key)
        .map(|existing| existing.sub_elements.clone())
        .unwrap_or_default();
    let had_existing = state.layer_backdrop_cache.contains_key(&stable_key);
    for rect in &rects {
        let rect_key = format!(
            "{}:{}:{}:{}:{}",
            layer_id, rect.x, rect.y, rect.width, rect.height
        );
        let entry = sub_elements.entry(rect_key).or_default();
        if had_existing {
            entry.commit_counter.increment();
        }
    }
    state.layer_backdrop_cache.insert(
        stable_key.clone(),
        crate::backend::shader_effect::CachedBackdropTexture {
            signature,
            texture: texture.clone(),
            id: state
                .layer_backdrop_cache
                .get(&stable_key)
                .map(|cached| cached.id.clone())
                .unwrap_or_else(smithay::backend::renderer::element::Id::new),
            commit_counter: state
                .layer_backdrop_cache
                .get(&stable_key)
                .map(|existing| {
                    let mut counter = existing.commit_counter;
                    counter.increment();
                    counter
                })
                .unwrap_or_default(),
            sub_elements: state
                .layer_backdrop_cache
                .get(&stable_key)
                .map(|_| sub_elements.clone())
                .unwrap_or(sub_elements),
        },
    );
    rects
        .into_iter()
        .filter_map(|rect| {
            let rect_key = format!(
                "{}:{}:{}:{}:{}",
                layer_id, rect.x, rect.y, rect.width, rect.height
            );
            let rect_local = Rectangle::new(
                smithay::utils::Point::from((rect.x - output_geo.loc.x, rect.y - output_geo.loc.y)),
                (rect.width, rect.height).into(),
            );
            if std::env::var_os("SHOJI_FIREFOX_BACKDROP_DEBUG").is_some() {
                tracing::info!(
                    layer_surface = ?layer_surface.wl_surface().id(),
                    output = %output.name(),
                    rect = ?rect,
                    rect_local = ?rect_local,
                    captured_local_rect = ?captured_local_rect,
                    from_cache = false,
                    "backdrop debug: layer effect element"
                );
            }
            crate::backend::shader_effect::backdrop_shader_element(
                renderer,
                state
                    .layer_backdrop_cache
                    .get(&stable_key)
                    .and_then(|cached| cached.sub_elements.get(&rect_key))
                    .map(|entry| entry.id.clone())
                    .unwrap_or_else(smithay::backend::renderer::element::Id::new),
                state
                    .layer_backdrop_cache
                    .get(&stable_key)
                    .and_then(|cached| cached.sub_elements.get(&rect_key))
                    .map(|entry| entry.commit_counter)
                    .unwrap_or_default(),
                texture.clone(),
                rect_local,
                rect_local,
                captured_local_rect,
                &config.effect,
                alpha,
                scale.x as f32,
                None,
                0.0,
                format!("layer-lower:{}:{}", output.name(), rect_key),
            )
            .ok()
            .map(WinitRenderElements::Backdrop)
        })
        .collect()
}

fn upper_layer_scene_elements(
    renderer: &mut GlesRenderer,
    state: &mut ShojiWM,
    output: &Output,
    output_geo: Rectangle<i32, Logical>,
    scale: smithay::utils::Scale<f64>,
    windows_top_to_bottom: &[smithay::desktop::Window],
    // Fullscreen fast path: a fullscreen window stacks above the Top layer
    // but below Overlay, so only Overlay surfaces stay visible.
    overlay_only: bool,
) -> Vec<WinitRenderElements> {
    let map = smithay::desktop::layer_map_for_output(output);
    let layer_kinds: &[smithay::wayland::shell::wlr_layer::Layer] = if overlay_only {
        &[smithay::wayland::shell::wlr_layer::Layer::Overlay]
    } else {
        &[
            smithay::wayland::shell::wlr_layer::Layer::Overlay,
            smithay::wayland::shell::wlr_layer::Layer::Top,
        ]
    };
    let upper_layers: Vec<_> = layer_kinds
        .iter()
        .flat_map(|layer| map.layers_on(*layer).rev().cloned())
        .filter(crate::backend::window::layer_surface_is_mapped)
        .collect();
    drop(map);

    let mut elements = Vec::new();
    for (layer_index, layer_surface) in upper_layers.iter().enumerate() {
        let layer_surface = layer_surface.clone();
        // Upper layers stacked below this one (the list is front-to-back);
        // backdrop captures must see them since they draw above all windows.
        let upper_layers_below = &upper_layers[layer_index + 1..];
        let layer_id = crate::ssd::layer_runtime_id(&layer_surface);
        let root_elements = window_render::layer_surface_root_elements(
            renderer,
            output,
            &layer_surface,
            scale,
            1.0,
        )
        .into_iter()
        .map(WinitRenderElements::Window)
        .collect::<Vec<_>>();
        let effects = state.configured_layer_effects.get(&layer_id).cloned();
        if let (Some(effects), Some(layer_rect)) =
            (effects, layer_surface_logical_rect(output, &layer_surface))
        {
            let capture_elements =
                window_render::layer_surface_elements(renderer, output, &layer_surface, scale, 1.0)
                    .into_iter()
                    .map(WinitRenderElements::Window)
                    .collect::<Vec<_>>();
            elements.extend(compose_layer_source_effects(
                renderer,
                output,
                output_geo,
                scale,
                &layer_id,
                layer_rect,
                &effects,
                root_elements,
                &capture_elements,
                &mut state.layer_effect_cache,
            ));
        } else {
            elements.extend(root_elements);
        }
        let custom_background = state
            .configured_layer_effects
            .get(&layer_id)
            .and_then(|effects| effects.behind.as_ref())
            .filter(|effect| effect.effect.is_backdrop())
            .cloned();
        let effect_config = state.configured_background_effect.clone();
        if custom_background.is_none()
            && let Some(effect_config) =
                effect_config.filter(|config| config.effect.supports_framebuffer_backdrop())
        {
            elements.extend(configured_background_framebuffer_effect_elements_for_layer(
                renderer,
                state,
                output,
                output_geo,
                scale,
                &layer_surface,
                1.0,
                &effect_config,
            ));
        } else {
            elements.extend(configured_background_effect_elements_for_layer(
                renderer,
                state,
                output,
                output_geo,
                scale,
                windows_top_to_bottom,
                upper_layers_below,
                &layer_surface,
                1.0,
                custom_background.as_ref(),
            ));
        }
    }
    elements
}

fn configured_background_framebuffer_effect_elements_for_layer(
    renderer: &mut GlesRenderer,
    state: &mut ShojiWM,
    output: &Output,
    output_geo: Rectangle<i32, Logical>,
    scale: smithay::utils::Scale<f64>,
    layer_surface: &smithay::desktop::LayerSurface,
    alpha: f32,
    effect_config: &crate::ssd::BackgroundEffectConfig,
) -> Vec<WinitRenderElements> {
    let layer_id = crate::ssd::layer_runtime_id(layer_surface);
    let stable_key = format!("winit:layer-top-framebuffer:{}:{}", output.name(), layer_id);
    crate::backend::shader_effect::framebuffer_backdrop_element_for_output_rects(
        renderer,
        state
            .layer_framebuffer_effect_states
            .entry(stable_key)
            .or_default(),
        &protocol_background_effect_rects_for_layer(output, layer_surface),
        effect_config.effect.clone(),
        output_geo,
        scale,
        alpha,
    )
    .ok()
    .flatten()
    .map(|element| {
        vec![WinitRenderElements::Decoration(
            decoration::DecorationSceneElements::Backdrop(element),
        )]
    })
    .unwrap_or_default()
}

fn configured_background_framebuffer_effect_elements_for_window(
    renderer: &mut GlesRenderer,
    state: &mut ShojiWM,
    window: &smithay::desktop::Window,
    output_geo: Rectangle<i32, Logical>,
    scale: smithay::utils::Scale<f64>,
    alpha: f32,
) -> Vec<(
    usize,
    crate::backend::shader_effect::StableBackdropFramebufferElement,
)> {
    let Some(config) = state.configured_background_effect.clone() else {
        return Vec::new();
    };
    let rects = protocol_background_effect_rects_for_window(state, window);
    let Some(decoration) = state.window_decorations.get_mut(window) else {
        return Vec::new();
    };
    rects
        .into_iter()
        .enumerate()
        .filter_map(|(index, rect)| {
            crate::backend::decoration::framebuffer_backdrop_element_for_window_rect(
                renderer,
                decoration,
                format!("__protocol_background_effect_framebuffer_{}", index),
                rect,
                config.effect.clone(),
                output_geo,
                scale,
                alpha,
            )
            .ok()
            .flatten()
            .map(|element| (index, element))
        })
        .collect()
}

fn configured_background_effect_elements_for_window(
    renderer: &mut GlesRenderer,
    state: &mut ShojiWM,
    output: &Output,
    output_geo: Rectangle<i32, Logical>,
    scale: smithay::utils::Scale<f64>,
    windows_top_to_bottom: &[smithay::desktop::Window],
    window_index: usize,
    window: &smithay::desktop::Window,
    alpha: f32,
    apply_visual_transform: bool,
) -> Vec<(
    usize,
    crate::backend::shader_effect::StableBackdropTextureElement,
)> {
    let Some(config) = state.configured_background_effect.clone() else {
        return Vec::new();
    };
    let Some(decoration) = state.window_decorations.get(window).cloned() else {
        return Vec::new();
    };
    let rects = protocol_background_effect_rects_for_window(state, window);
    if rects.is_empty() {
        return Vec::new();
    }
    let lower_windows = windows_top_to_bottom
        .iter()
        .skip(window_index + 1)
        .cloned()
        .collect::<Vec<_>>();
    let (_, lower_layers) = window_render::layer_surfaces_for_output(output);

    rects
        .into_iter()
        .enumerate()
        .filter_map(|(index, rect)| {
            let uses_backdrop = config.effect.uses_backdrop_input();
            let uses_xray = config.effect.uses_xray_backdrop_input();
            let stable_key = format!("__protocol_background_effect_{}", index);
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            stable_key.hash(&mut hasher);
            let effect_rect = if apply_visual_transform {
                crate::backend::visual::transformed_rect(
                    rect,
                    decoration.layout.root.rect,
                    decoration.visual_transform,
                )
            } else {
                rect
            };
            (
                effect_rect.x,
                effect_rect.y,
                effect_rect.width,
                effect_rect.height,
                output_geo.loc.x,
                output_geo.loc.y,
                output_geo.size.w,
                output_geo.size.h,
            )
                .hash(&mut hasher);
            let blur_padding = config.effect.capture_padding.max(0);
            blur_padding.hash(&mut hasher);
            if uses_backdrop || uses_xray {
                state.lower_layer_scene_generation.hash(&mut hasher);
            }
            format!("{:?}", config.effect).hash(&mut hasher);
            let capture_geo = Rectangle::new(
                smithay::utils::Point::from((
                    effect_rect.x - blur_padding,
                    effect_rect.y - blur_padding,
                )),
                (
                    effect_rect.width + blur_padding * 2,
                    effect_rect.height + blur_padding * 2,
                )
                    .into(),
            );
            (
                capture_geo.loc.x,
                capture_geo.loc.y,
                capture_geo.size.w,
                capture_geo.size.h,
            )
                .hash(&mut hasher);
            if uses_backdrop {
                hash_window_scene_contributors(&mut hasher, state, &lower_windows, effect_rect);
            }
            if uses_backdrop || uses_xray {
                hash_layer_scene_contributors(&mut hasher, output, &lower_layers, effect_rect);
            }
            let signature = hasher.finish();
            let source_damage_hit = crate::backend::shader_effect::source_damage_intersects_rect(
                &config.effect,
                Rectangle::new(
                    smithay::utils::Point::from((effect_rect.x, effect_rect.y)),
                    (effect_rect.width, effect_rect.height).into(),
                ),
                &{
                    let mut entries = Vec::new();
                    if uses_backdrop {
                        entries.extend(collect_window_source_damage(
                            state,
                            lower_windows.iter().cloned(),
                        ));
                    }
                    if uses_backdrop || uses_xray {
                        entries.extend(collect_layer_source_damage(
                            state,
                            lower_layers.iter().cloned(),
                            true,
                        ));
                    }
                    entries
                },
            );
            let actual_capture_geo = capture_geo.intersection(output_geo).unwrap_or(capture_geo);
            let capture_origin_physical =
                crate::backend::visual::logical_point_to_physical_point_global_edges(
                    actual_capture_geo.loc,
                    output_geo.loc,
                    scale,
                );

            if !matches!(
                config.effect.invalidate_policy(),
                crate::ssd::EffectInvalidationPolicy::Always
            ) && !source_damage_hit
            {
                if let Some(existing) = state
                    .window_decorations
                    .get(window)
                    .and_then(|d| d.backdrop_cache.get(&stable_key))
                    .filter(|existing| existing.signature == signature)
                    .cloned()
                {
                    let local_rect = Rectangle::new(
                        smithay::utils::Point::from((
                            effect_rect.x - decoration.layout.root.rect.x,
                            effect_rect.y - decoration.layout.root.rect.y,
                        )),
                        (effect_rect.width, effect_rect.height).into(),
                    );
                    let sample_rect = Rectangle::new(
                        smithay::utils::Point::from((
                            effect_rect.x - output_geo.loc.x,
                            effect_rect.y - output_geo.loc.y,
                        )),
                        (effect_rect.width, effect_rect.height).into(),
                    );
                    let geometry =
                        crate::backend::visual::relative_physical_rect_from_root_global_origin_size(
                            effect_rect,
                            decoration.layout.root.rect,
                            output_geo,
                            scale,
                        );
                    return crate::backend::shader_effect::backdrop_shader_element_with_geometry(
                        renderer,
                        existing.id.clone(),
                        existing.commit_counter,
                        existing.texture,
                        local_rect,
                        geometry,
                        sample_rect,
                        sample_rect,
                        &config.effect,
                        alpha,
                        scale.x as f32,
                        [0.0, 0.0],
                        None,
                        0.0,
                        format!("protocol-window:{}:{}", decoration.snapshot.id, stable_key),
                    )
                    .ok()
                    .map(|element| (index, element));
                }
            }

            let backdrop_texture = if uses_backdrop {
                let mut backdrop_scene: Vec<WinitRenderElements> = Vec::new();
                for lower_window in &lower_windows {
                    backdrop_scene.extend(window_scene_elements_for_capture(
                        renderer,
                        state,
                        output_geo.loc,
                        actual_capture_geo,
                        capture_origin_physical,
                        scale,
                        lower_window,
                    ));
                }
                let (_, lower_layer_elements) =
                    window_render::layer_elements_for_output(renderer, output, scale, 1.0);
                let capture_visual = WindowVisualState {
                    origin: smithay::utils::Point::from((0, 0)),
                    scale: smithay::utils::Scale::from((1.0, 1.0)),
                    translation: Point::from((
                        -capture_origin_physical.x,
                        -capture_origin_physical.y,
                    )),
                    opacity: 1.0,
                };
                backdrop_scene.extend(
                    transform_window_elements(
                        lower_layer_elements,
                        capture_visual,
                        WinitRenderElements::Window,
                        WinitRenderElements::TransformedWindow,
                    )
                    .into_iter(),
                );
                capture_scene_texture_for_effect(
                    renderer,
                    "winit-protocol-window-backdrop",
                    actual_capture_geo,
                    scale,
                    &backdrop_scene,
                )
            } else {
                None
            };
            let xray_texture = if uses_xray {
                let mut xray_scene: Vec<WinitRenderElements> = Vec::new();
                for lower_layer in &lower_layers {
                    xray_scene.extend(layer_surface_scene_elements_for_capture(
                        renderer,
                        output,
                        actual_capture_geo,
                        capture_origin_physical,
                        scale,
                        lower_layer,
                    ));
                }
                capture_scene_texture_for_effect(
                    renderer,
                    "winit-protocol-window-xray",
                    actual_capture_geo,
                    scale,
                    &xray_scene,
                )
            } else {
                None
            };
            let input_texture = backdrop_texture
                .clone()
                .or_else(|| xray_texture.clone())
                .or_else(|| crate::backend::shader_effect::solid_white_texture(renderer).ok())?;
            let sample_region = crate::backend::visual::logical_rect_to_physical_buffer_rect_f64(
                effect_rect,
                actual_capture_geo.loc,
                scale,
            );
            let output_size = crate::backend::visual::logical_size_to_physical_buffer_size(
                effect_rect.width,
                effect_rect.height,
                scale,
            );
            if std::env::var_os("SHOJI_FIREFOX_BACKDROP_DEBUG").is_some() {
                tracing::info!(
                    window_id = %decoration.snapshot.id,
                    title = %decoration.snapshot.title,
                    app_id = ?decoration.snapshot.app_id,
                    stable_key = %stable_key,
                    effect_rect = ?effect_rect,
                    root_rect = ?decoration.layout.root.rect,
                    output_geo = ?output_geo,
                    blur_padding,
                    capture_geo = ?capture_geo,
                    actual_capture_geo = ?actual_capture_geo,
                    capture_origin_physical = ?capture_origin_physical,
                    sample_region = ?sample_region,
                    output_size = ?output_size,
                    local_rect = ?Rectangle::<i32, Logical>::new(
                        smithay::utils::Point::from((
                            effect_rect.x - decoration.layout.root.rect.x,
                            effect_rect.y - decoration.layout.root.rect.y,
                        )),
                        (effect_rect.width, effect_rect.height).into(),
                    ),
                    sample_rect = ?Rectangle::<i32, Logical>::new(
                        smithay::utils::Point::from((
                            effect_rect.x - output_geo.loc.x,
                            effect_rect.y - output_geo.loc.y,
                        )),
                        (effect_rect.width, effect_rect.height).into(),
                    ),
                    geometry = ?crate::backend::visual::relative_physical_rect_from_root_global_origin_size(
                        effect_rect,
                        decoration.layout.root.rect,
                        output_geo,
                        scale,
                    ),
                    "backdrop debug: protocol window element"
                );
            }
            let texture = crate::backend::shader_effect::apply_effect_pipeline_cached_for_key(
                renderer,
                format!(
                    "winit:protocol-window:{}:{}",
                    decoration.snapshot.id, stable_key
                ),
                input_texture,
                xray_texture,
                crate::backend::visual::logical_size_to_physical_buffer_size(
                    actual_capture_geo.size.w,
                    actual_capture_geo.size.h,
                    scale,
                ),
                Some(sample_region),
                Some(output_size),
                &config.effect,
            )
            .ok()?;
            let commit_counter = state
                .window_decorations
                .get(window)
                .and_then(|d| d.backdrop_cache.get(&stable_key))
                .map(|existing| {
                    let mut counter = existing.commit_counter;
                    counter.increment();
                    counter
                })
                .unwrap_or_default();
            if let Some(window_decoration) = state.window_decorations.get_mut(window) {
                window_decoration.backdrop_cache.insert(
                    stable_key.clone(),
                    crate::backend::shader_effect::CachedBackdropTexture {
                        signature,
                        texture: texture.clone(),
                        id: smithay::backend::renderer::element::Id::new(),
                        commit_counter,
                        sub_elements: std::collections::HashMap::new(),
                    },
                );
            }
            let local_rect = Rectangle::new(
                smithay::utils::Point::from((
                    effect_rect.x - decoration.layout.root.rect.x,
                    effect_rect.y - decoration.layout.root.rect.y,
                )),
                (effect_rect.width, effect_rect.height).into(),
            );
            let sample_rect = Rectangle::new(
                smithay::utils::Point::from((
                    effect_rect.x - output_geo.loc.x,
                    effect_rect.y - output_geo.loc.y,
                )),
                (effect_rect.width, effect_rect.height).into(),
            );
            let geometry =
                crate::backend::visual::relative_physical_rect_from_root_global_origin_size(
                    effect_rect,
                    decoration.layout.root.rect,
                    output_geo,
                    scale,
                );
            crate::backend::shader_effect::backdrop_shader_element_with_geometry(
                renderer,
                state
                    .window_decorations
                    .get(window)
                    .and_then(|d| d.backdrop_cache.get(&stable_key))
                    .map(|cached| cached.id.clone())
                    .unwrap_or_else(smithay::backend::renderer::element::Id::new),
                state
                    .window_decorations
                    .get(window)
                    .and_then(|d| d.backdrop_cache.get(&stable_key))
                    .map(|cached| cached.commit_counter)
                    .unwrap_or_default(),
                texture,
                local_rect,
                geometry,
                sample_rect,
                sample_rect,
                &config.effect,
                alpha,
                scale.x as f32,
                [0.0, 0.0],
                None,
                0.0,
                format!("protocol-window:{}:{}", decoration.snapshot.id, stable_key),
            )
            .ok()
            .map(|element| (index, element))
        })
        .collect()
}

fn window_scene_elements_for_capture(
    renderer: &mut GlesRenderer,
    state: &ShojiWM,
    output_origin: Point<i32, Logical>,
    capture_geo: Rectangle<i32, Logical>,
    capture_origin_physical: Point<i32, smithay::utils::Physical>,
    scale: smithay::utils::Scale<f64>,
    window: &smithay::desktop::Window,
) -> Vec<WinitRenderElements> {
    let Some(window_location) = state.space.element_location(window) else {
        return Vec::new();
    };
    let preliminary_physical_location =
        crate::backend::visual::logical_point_to_relative_physical_point_from_origin(
            window_location,
            output_origin,
            capture_origin_physical,
            scale,
        );
    let client_physical_geometry = state.window_decorations.get(window).and_then(|decoration| {
        decoration.content_clip.map(|clip| {
            let root_origin =
                crate::backend::visual::logical_point_to_relative_physical_point_from_origin(
                    Point::from((decoration.layout.root.rect.x, decoration.layout.root.rect.y)),
                    output_origin,
                    capture_origin_physical,
                    scale,
                );
            let local_geometry = crate::backend::visual::relative_physical_rect_from_root_precise(
                clip.rect_precise,
                decoration.layout.root.rect,
                Rectangle::new(output_origin, (0, 0).into()),
                scale,
            );
            Rectangle::new(
                smithay::utils::Point::from((
                    root_origin.x + local_geometry.loc.x,
                    root_origin.y + local_geometry.loc.y,
                )),
                local_geometry.size,
            )
        })
    });
    let physical_location = client_physical_geometry
        .map(|geometry| geometry.loc)
        .unwrap_or(preliminary_physical_location);
    let visual_state = state
        .window_decorations
        .get(window)
        .map(|decoration| {
            let transform = decoration.visual_transform;
            let rect = decoration.layout.root.rect;
            let logical_origin = Point::<f64, Logical>::from((
                rect.x as f64 + rect.width as f64 * transform.origin.x,
                rect.y as f64 + rect.height as f64 * transform.origin.y,
            ));
            WindowVisualState {
                origin: crate::backend::visual::precise_logical_point_to_relative_physical_point_from_origin(
                    logical_origin,
                    output_origin,
                    capture_origin_physical,
                    scale,
                ),
                scale: smithay::utils::Scale::from((
                    transform.scale_x.max(0.0),
                    transform.scale_y.max(0.0),
                )),
                translation: Point::<f64, Logical>::from((
                    transform.translate_x,
                    transform.translate_y,
                ))
                .to_physical_precise_round(scale),
                opacity: transform.opacity,
            }
        })
        .unwrap_or(WindowVisualState {
            origin: physical_location,
            scale: smithay::utils::Scale::from((1.0, 1.0)),
            translation: (0, 0).into(),
            opacity: 1.0,
        });

    let mut elements = Vec::new();

    if let Some(decoration) = state.window_decorations.get(window) {
        let root_origin =
            crate::backend::visual::logical_point_to_relative_physical_point_from_origin(
                Point::from((decoration.layout.root.rect.x, decoration.layout.root.rect.y)),
                output_origin,
                capture_origin_physical,
                scale,
            );
        let mut ordered_ui_elements: Vec<(usize, WinitRenderElements)> = Vec::new();
        let mut decoration = decoration.clone();
        if let Ok(backgrounds) = crate::backend::decoration::ordered_background_elements_for_window(
            renderer,
            &mut decoration,
            capture_geo,
            scale,
            visual_state.opacity,
        ) {
            for (order, element) in backgrounds {
                ordered_ui_elements.extend(
                    transform_decoration_elements(vec![element], root_origin, visual_state)
                        .into_iter()
                        .map(|item| (order, item)),
                );
            }
        }
        if let Ok(icon_elements) = crate::backend::decoration::ordered_icon_elements_for_decoration(
            renderer,
            &decoration,
            capture_geo,
            scale,
            visual_state.opacity,
        ) {
            for (order, element) in icon_elements {
                ordered_ui_elements.extend(
                    transform_text_elements(vec![element], root_origin, visual_state)
                        .into_iter()
                        .map(|item| (order, item)),
                );
            }
        }
        if let Ok(text_elements) = crate::backend::decoration::ordered_text_elements_for_decoration(
            renderer,
            &decoration,
            capture_geo,
            scale,
            visual_state.opacity,
        ) {
            for (order, element) in text_elements {
                ordered_ui_elements.extend(
                    transform_text_elements(vec![element], root_origin, visual_state)
                        .into_iter()
                        .map(|item| (order, item)),
                );
            }
        }
        ordered_ui_elements.sort_by_key(|(order, _)| *order);
        elements.extend(ordered_ui_elements.into_iter().map(|(_, element)| element));
        if let Some(content_clip) = decoration.content_clip {
            let clipped = window_render::clipped_surface_elements(
                window,
                renderer,
                physical_location,
                client_physical_geometry,
                output_origin,
                scale,
                scale,
                visual_state.opacity,
                Some(content_clip),
                decoration.managed_window.force_rect_size,
            )
            .unwrap_or_default();
            let mut clipped_elements = Vec::new();
            let mut raw_elements = Vec::new();
            for element in clipped {
                match element {
                    window_render::WindowClipElement::Clipped(element) => {
                        clipped_elements.push(element);
                    }
                    window_render::WindowClipElement::Raw(element) => {
                        raw_elements.push(element);
                    }
                }
            }
            elements.extend(transform_clipped_elements(clipped_elements, visual_state));
            elements.extend(
                transform_window_elements(
                    raw_elements,
                    visual_state,
                    WinitRenderElements::Window,
                    WinitRenderElements::TransformedWindow,
                )
                .into_iter(),
            );
        } else {
            elements.extend(
                transform_window_elements(
                    window_render::surface_elements(
                        window,
                        renderer,
                        physical_location,
                        scale,
                        visual_state.opacity,
                    ),
                    visual_state,
                    WinitRenderElements::Window,
                    WinitRenderElements::TransformedWindow,
                )
                .into_iter(),
            );
        }
    }

    elements.extend(
        transform_window_elements(
            window_render::popup_elements(
                window,
                renderer,
                physical_location,
                scale,
                visual_state.opacity,
            ),
            visual_state,
            WinitRenderElements::Window,
            WinitRenderElements::TransformedWindow,
        )
        .into_iter(),
    );

    elements
}

fn capture_live_snapshot_for_window(
    renderer: &mut GlesRenderer,
    state: &mut ShojiWM,
    _output: &Output,
    window: &smithay::desktop::Window,
    _window_location: smithay::utils::Point<i32, Logical>,
    scale: smithay::utils::Scale<f64>,
    z_index: usize,
) -> Result<(), smithay::backend::renderer::gles::GlesError> {
    let Some(decoration) = state.window_decorations.get(window) else {
        return Ok(());
    };
    let client_rect = decoration.client_rect;
    let snapshot_id = decoration.snapshot.id.clone();
    // The close snapshot texture is client-rect-local. `surface_elements`
    // expects the same client-slot location that live rendering uses and
    // subtracts `window.geometry().loc` internally to place the root surface.
    // Passing `window_location - client_rect.loc` here applies that geometry
    // offset twice for CSD/GTK windows, which shifts the frozen client image.
    let physical_location = smithay::utils::Point::<i32, smithay::utils::Physical>::from((0, 0));

    let mut elements: Vec<WinitRenderElements> = Vec::new();
    let surface_elements =
        window_render::surface_elements(window, renderer, physical_location, scale, 1.0);
    if surface_elements.is_empty() {
        return Ok(());
    }
    let has_client_content = !surface_elements.is_empty();
    elements.extend(
        surface_elements
            .into_iter()
            .map(WinitRenderElements::Window),
    );

    let existing = state.live_window_snapshots.remove(&snapshot_id);
    let live_tracker = state
        .live_window_snapshot_trackers
        .entry(snapshot_id.clone())
        .or_insert_with(|| {
            smithay::backend::renderer::damage::OutputDamageTracker::new(
                (0, 0),
                1.0,
                smithay::utils::Transform::Normal,
            )
        });
    if let Some(snapshot) = snapshot::capture_snapshot(
        renderer,
        existing,
        live_tracker,
        client_rect,
        z_index,
        has_client_content,
        scale,
        &elements,
    )? {
        state
            .live_window_snapshots
            .insert(snapshot_id.clone(), snapshot);
        if has_client_content {
            if let Some(snapshot) = state.live_window_snapshots.get(&snapshot_id) {
                if let Ok(complete_snapshot) = snapshot::duplicate_snapshot(renderer, snapshot) {
                    state
                        .complete_window_snapshots
                        .insert(snapshot_id, complete_snapshot);
                }
            }
        }
    }

    Ok(())
}

fn closing_snapshot_elements(
    renderer: &mut GlesRenderer,
    state: &ShojiWM,
    output: &Output,
    scale: smithay::utils::Scale<f64>,
) -> Vec<WinitRenderElements> {
    let Some(output_geo) = state.space.output_geometry(output) else {
        return Vec::new();
    };

    state
        .closing_window_snapshots
        .values()
        .flat_map(|snapshot| {
            let visual = window_visual_state(
                snapshot.decoration.layout.root.rect,
                snapshot.transform,
                output_geo,
                scale,
            );
            let root_origin =
                root_physical_origin(snapshot.decoration.layout.root.rect, output_geo, scale);

            let mut elements = Vec::new();
            // Render compositor-drawn decorations through the normal pipeline. The snapshot
            // texture contains only the client area (live_window_snapshot), so decorations
            // are always rendered separately here — same as the live loop.
            if let Ok(icon_elements) = crate::backend::icon::icon_elements_for_decoration(
                renderer,
                &snapshot.decoration,
                output_geo,
                scale,
                visual.opacity,
            ) {
                elements.extend(transform_text_elements(icon_elements, root_origin, visual));
            }
            if let Ok(text_elements) = crate::backend::text::text_elements_for_decoration(
                renderer,
                &snapshot.decoration,
                output_geo,
                scale,
                visual.opacity,
            ) {
                elements.extend(transform_text_elements(text_elements, root_origin, visual));
            }
            let mut decoration = snapshot.decoration.clone();
            if let Ok(background_elements) = decoration::background_elements_for_window(
                renderer,
                &mut decoration,
                output_geo,
                scale,
                visual.opacity,
            ) {
                elements.extend(transform_decoration_elements(
                    background_elements,
                    root_origin,
                    visual,
                ));
            }
            // Render the frozen client-area snapshot as the window content.
            if let Some(element) = snapshot::live_snapshot_element(
                renderer,
                &snapshot.live,
                output_geo,
                scale,
                visual.opacity,
            ) {
                elements.extend(transform_snapshot_elements(vec![element], visual));
            }
            elements
        })
        .collect()
}
