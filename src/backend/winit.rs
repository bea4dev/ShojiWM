use std::hash::{Hash, Hasher};
use std::time::Duration;

use smithay::{
    backend::{
        renderer::{
            ImportEgl, ImportMemWl,
            damage::OutputDamageTracker,
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
        WindowVisualState, relative_physical_rect_from_root,
        relative_physical_rect_from_root_precise, root_physical_origin, transformed_root_rect,
        window_visual_state,
    },
    backend::{damage, damage_blink, decoration, snapshot, window as window_render},
    presentation::{take_presentation_feedback, update_primary_scanout_output},
};
use smithay::wayland::presentation::Refresh;

fn manual_invalidate_debug_enabled() -> bool {
    std::env::var_os("SHOJI_MANUAL_INVALIDATE_DEBUG")
        .is_some_and(|value| value != "0" && !value.is_empty())
}

fn capture_scene_texture_for_effect(
    renderer: &mut GlesRenderer,
    capture_geo: Rectangle<i32, Logical>,
    scale: smithay::utils::Scale<f64>,
    scene: &[WinitRenderElements],
) -> Option<GlesTexture> {
    if scene.is_empty() {
        return None;
    }
    snapshot::capture_snapshot(
        renderer,
        None,
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
    .ok()
    .flatten()
    .map(|snapshot| snapshot.texture)
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
    let _global = output.create_global::<ShojiWM>(&state.display_handle);
    output.change_current_state(
        Some(mode),
        Some(Transform::Flipped180),
        None,
        Some((0, 0).into()),
    );
    output.set_preferred(mode);

    state.space.map_output(&output, (0, 0));

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
                    if let Err(err) = state.refresh_window_decorations_for_output(Some(output.name().as_str())) {
                        warn!(error = ?err, "failed to refresh window decorations for winit");
                    }
                    if let Err(err) = state.refresh_layer_effects_for_output(output.name().as_str()) {
                        warn!(error = ?err, "failed to refresh layer effects for winit");
                    }

                    let size = backend.window_size();
                    let damage = Rectangle::from_size(size);

                    let mut should_submit_frame = false;
                    {
                        let (renderer, mut framebuffer) = backend.bind().unwrap();
                        let output_geo = state.space.output_geometry(&output).unwrap();
                        let scale =
                            smithay::utils::Scale::from(output.current_scale().fractional_scale());
                        let windows: Vec<_> =
                            state.space.elements_for_output(&output).cloned().collect();
                        let windows_top_to_bottom: Vec<_> =
                            windows.iter().rev().cloned().collect();
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

                        let mut scene_elements: Vec<WinitRenderElements> = Vec::new();
                        scene_elements.extend(upper_layer_scene_elements(
                            renderer,
                            state,
                            &output,
                            output_geo,
                            scale,
                            &windows_top_to_bottom,
                        ));
                        for (_window_index, window) in windows_top_to_bottom.iter().enumerate() {
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
                                            relative_physical_rect_from_root_precise(
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
                                    visual_state.opacity,
                                    decoration_ready,
                                    false,
                                );
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
                                        visual_state.opacity,
                                        false,
                                    )
                                    .into_iter()
                                    .map(|(order, element)| (order, element, true)),
                                );
                                for (order, element, render_as_backdrop) in backdrop_items.drain(..) {
                                    if let Some(root_origin) = root_origin {
                                        let transformed = transform_backdrop_elements(
                                            vec![element],
                                            root_origin,
                                            visual_state,
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
                                if let Some(decoration_state) =
                                    state.window_decorations.get_mut(window)
                                {
                                    let mut background_items = decoration::ordered_background_elements_for_window(
                                        renderer,
                                        decoration_state,
                                        output_geo,
                                        snap_scale,
                                        visual_state.opacity,
                                    )
                                    .inspect_err(|error| {
                                        warn!(?error, "failed to build decoration background elements");
                                    })
                                    .unwrap_or_default();
                                    background_items.sort_by_key(|(order, _)| *order);
                                    for (order, element) in background_items {
                                        if let Some(root_origin) = root_origin {
                                            ordered_ui_elements.extend(
                                                transform_decoration_elements(vec![element], root_origin, visual_state)
                                                    .into_iter()
                                                    .map(|item| (order, item)),
                                            );
                                        }
                                    }
                                }

                                for (order, element) in decoration::ordered_icon_elements_for_window(
                                    renderer,
                                    &state.space,
                                    &state.window_decorations,
                                    &output,
                                    window,
                                    visual_state.opacity,
                                )
                                .unwrap_or_default()
                                {
                                    if let Some(root_origin) = root_origin {
                                        ordered_ui_elements.extend(
                                            transform_text_elements(vec![element], root_origin, visual_state)
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
                                    visual_state.opacity,
                                )
                                .unwrap_or_default()
                                {
                                    if let Some(root_origin) = root_origin {
                                        ordered_ui_elements.extend(
                                            transform_text_elements(vec![element], root_origin, visual_state)
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

                            let client_elements = if let Some(content_clip) = content_clip {
                                if std::env::var_os("SHOJI_GAP_DEBUG").is_some() {
                                    if let Some(decoration) = state.window_decorations.get(window) {
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
                                    snap_scale,
                                    visual_state.opacity,
                                    Some(content_clip),
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
                                        .map(|element| smithay::backend::renderer::element::Element::geometry(element, scale));
                                    let window_geometry = window.geometry();
                                    let decoration_client_rect = state
                                        .window_decorations
                                        .get(window)
                                        .map(|decoration| decoration.client_rect);
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
                                        visual_state,
                                        WinitRenderElements::Window,
                                        WinitRenderElements::TransformedWindow,
                                    )
                                } else {
                                    transform_clipped_elements(clipped, visual_state)
                                }
                            } else {
                                let surfaces = window_render::surface_elements(
                                    window,
                                    renderer,
                                    physical_location,
                                    scale,
                                    visual_state.opacity,
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
                                    visual_state,
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
                                    visual_state.opacity,
                                ),
                                visual_state,
                                WinitRenderElements::Window,
                                WinitRenderElements::TransformedWindow,
                            );

                            scene_elements.extend(popup_elements.into_iter());
                            scene_elements.extend(client_elements.into_iter());
                            scene_elements.extend(
                                ordered_ui_elements.into_iter().map(|(_, element)| element),
                            );
                            scene_elements.extend(
                                ordered_backdrop_elements
                                    .into_iter()
                                    .map(|(_, element)| element),
                            );

                            state
                                .windows_ready_for_decoration
                                .insert(window_id.clone());

                            let should_refresh_snapshot = state
                                .window_decorations
                                .get(window)
                                .map(|decoration| {
                                    state.snapshot_dirty_window_ids.contains(&decoration.snapshot.id)
                                        || state
                                            .live_window_snapshots
                                            .get(&decoration.snapshot.id)
                                            .map(|snapshot| snapshot.rect != decoration.client_rect)
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

                        }
                        scene_elements.extend(
                            closing_snapshot_elements(renderer, state, &output, scale)
                                .into_iter(),
                        );
                        scene_elements.extend(lower_layer_scene_elements(
                            renderer,
                            state,
                            &output,
                            output_geo,
                            scale,
                            &windows_top_to_bottom,
                        ));

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
                        elements.extend(
                            damage_blink::elements_for_output(
                                state.damage_blink_rects_for_output(&output),
                                output_geo,
                                scale,
                            )
                            .into_iter()
                            .map(WinitRenderElements::Blink),
                        );
                        elements.extend(content_elements);

                        trace!(
                            output = %output.name(),
                            window_count = state.space.elements().count(),
                            render_element_count = elements.len(),
                            "rendering winit frame"
                        );

                        if !elements.is_empty() {
                            let frame_target = state.clock.now()
                                + output
                                    .current_mode()
                                    .map(|mode| Duration::from_secs_f64(1_000f64 / mode.refresh as f64))
                                    .unwrap_or(Duration::ZERO);
                            state.pre_repaint(&output, frame_target);

                            let render_output_result = damage_tracker.render_output(
                                renderer,
                                &mut framebuffer,
                                0,
                                &elements,
                                [0.1, 0.1, 0.1, 1.0],
                            );
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
                                    &render_output_result.states,
                                );

                                let frame_time = Duration::from(state.clock.now())
                                    + output
                                        .current_mode()
                                        .map(|mode| Duration::from_secs_f64(1_000f64 / mode.refresh as f64))
                                        .unwrap_or(Duration::ZERO);

                                if render_output_result.damage.is_some() {
                                    let mut output_presentation_feedback =
                                        take_presentation_feedback(&output, &state.space, &render_output_result.states);
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
                            }
                        }
                    }
                    if should_submit_frame {
                        backend.submit(Some(&[damage])).unwrap();
                    }

                    state.space.refresh();
                    state.popups.cleanup();
                    state.pending_decoration_damage.clear();
                    state.clear_source_damage();
                    state.finish_damage_blink_frame();
                    let _ = state.display_handle.flush_clients();

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
    if is_identity_visual(visual) {
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
    if is_identity_visual(visual) {
        return elements
            .into_iter()
            .map(WinitRenderElements::Clipped)
            .collect();
    }

    elements
        .into_iter()
        .map(|element| {
            WinitRenderElements::TransformedClipped(RelocateRenderElement::from_element(
                RescaleRenderElement::from_element(element, visual.origin, visual.scale),
                visual.translation,
                Relocate::Relative,
            ))
        })
        .collect()
}

fn transform_text_elements(
    elements: Vec<crate::backend::text::DecorationTextureElements>,
    root_origin: Point<i32, smithay::utils::Physical>,
    visual: WindowVisualState,
) -> Vec<WinitRenderElements> {
    if is_identity_visual(visual) {
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
    if is_identity_visual(visual) {
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
    if is_identity_visual(visual) {
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
    if is_identity_visual(visual) {
        return elements
            .into_iter()
            .map(|element| {
                WinitRenderElements::RelocatedBackdrop(RelocateRenderElement::from_element(
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
            WinitRenderElements::TransformedBackdrop(RelocateRenderElement::from_element(
                RescaleRenderElement::from_element(relocated, visual.origin, visual.scale),
                visual.translation,
                Relocate::Relative,
            ))
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
) -> Vec<(
    usize,
    crate::backend::shader_effect::StableBackdropTextureElement,
    bool,
)> {
    if !has_backdrop_source {
        let Some(decoration) = state.window_decorations.get(window) else {
            return Vec::new();
        };
        if !decoration
            .shader_buffers
            .iter()
            .any(|cached| cached.shader.is_texture_backed())
        {
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
        .filter(|cached| cached.shader.is_texture_backed())
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
            let display_rect_precise = cached.clip_rect_precise.and_then(|_| {
                cached.rect_precise.map(|rect| {
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
            });
            let source_effect_rect = crate::backend::visual::transformed_rect(
                cached.rect,
                decoration.layout.root.rect,
                decoration.visual_transform,
            );
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
            let blur_padding = cached
                .shader
                .blur_stage()
                .map(|blur| {
                    let radius = blur.radius.max(1);
                    let passes = blur.passes.max(1);
                    (radius * passes * 24 + 32).max(32)
                })
                .unwrap_or(0);
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
            (
                capture_geo.loc.x,
                capture_geo.loc.y,
                capture_geo.size.w,
                capture_geo.size.h,
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
                    let clip_rect = cached
                        .clip_rect
                        .map(|clip_rect| {
                            let transformed_clip = if apply_visual_transform {
                                crate::backend::visual::transformed_rect(
                                    clip_rect,
                                    decoration.layout.root.rect,
                                    decoration.visual_transform,
                                )
                            } else {
                                clip_rect
                            };
                            crate::backend::visual::SnappedLogicalRect {
                                x: (transformed_clip.x - display_rect.x) as f32,
                                y: (transformed_clip.y - display_rect.y) as f32,
                                width: transformed_clip.width.max(0) as f32,
                                height: transformed_clip.height.max(0) as f32,
                            }
                        })
                        .or_else(|| {
                            display_rect_precise
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
                                    crate::backend::visual::precise_logical_rect_in_element_space(
                                        clip, rect,
                                    )
                                })
                        });
                    let local_sample_rect = Rectangle::new(
                        smithay::utils::Point::from((
                            source_effect_rect.x - output_geo.loc.x,
                            source_effect_rect.y - output_geo.loc.y,
                        )),
                        (source_effect_rect.width, source_effect_rect.height).into(),
                    );
                    let local_capture_rect = local_sample_rect;
                    let geometry = relative_physical_rect_from_root(
                        display_rect,
                        root_rect,
                        output_geo,
                        scale,
                        cached.clip_rect,
                    );
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
                        clip_rect,
                        cached.clip_radius,
                        format!(
                            "window-backdrop:{}:{}",
                            decoration.snapshot.id, cached.stable_key
                        ),
                    )
                    .ok()
                    .map(|element| (cached.order, element, render_as_backdrop));
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
                        actual_capture_geo,
                        scale,
                        lower_window,
                    ));
                }
                let (_, lower_layer_elements) =
                    window_render::layer_elements_for_output(renderer, output, scale, 1.0);
                let capture_offset = actual_capture_geo.loc - output_geo.loc;
                let capture_visual = WindowVisualState {
                    origin: smithay::utils::Point::from((0, 0)),
                    scale: smithay::utils::Scale::from((1.0, 1.0)),
                    translation: smithay::utils::Point::from((
                        -capture_offset.x,
                        -capture_offset.y,
                    ))
                    .to_f64()
                    .to_physical_precise_round(scale),
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
                        scale,
                        lower_layer,
                    ));
                }
                capture_scene_texture_for_effect(renderer, actual_capture_geo, scale, &xray_scene)
            } else {
                None
            };
            let input_texture = backdrop_texture
                .clone()
                .or_else(|| xray_texture.clone())
                .or_else(|| crate::backend::shader_effect::solid_white_texture(renderer).ok())?;
            let sample_region = crate::backend::visual::logical_rect_to_physical_buffer_rect(
                source_effect_rect,
                actual_capture_geo.loc,
                scale,
            );
            let output_size = crate::backend::visual::logical_size_to_physical_buffer_size(
                source_effect_rect.width,
                source_effect_rect.height,
                scale,
            );
            let texture = crate::backend::shader_effect::apply_effect_pipeline(
                renderer,
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
            let clip_rect = cached
                .clip_rect
                .map(|clip_rect| {
                    let transformed_clip = if apply_visual_transform {
                        crate::backend::visual::transformed_rect(
                            clip_rect,
                            decoration.layout.root.rect,
                            decoration.visual_transform,
                        )
                    } else {
                        clip_rect
                    };
                    crate::backend::visual::SnappedLogicalRect {
                        x: (transformed_clip.x - display_rect.x) as f32,
                        y: (transformed_clip.y - display_rect.y) as f32,
                        width: transformed_clip.width.max(0) as f32,
                        height: transformed_clip.height.max(0) as f32,
                    }
                })
                .or_else(|| {
                    display_rect_precise
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
                            crate::backend::visual::precise_logical_rect_in_element_space(
                                clip, rect,
                            )
                        })
                });
            let local_sample_rect = Rectangle::new(
                smithay::utils::Point::from((
                    source_effect_rect.x - output_geo.loc.x,
                    source_effect_rect.y - output_geo.loc.y,
                )),
                (source_effect_rect.width, source_effect_rect.height).into(),
            );
            let local_capture_rect = local_sample_rect;
            let geometry = relative_physical_rect_from_root(
                display_rect,
                root_rect,
                output_geo,
                scale,
                cached.clip_rect,
            );
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
                clip_rect,
                cached.clip_radius,
                format!(
                    "window-backdrop:{}:{}",
                    decoration.snapshot.id, cached.stable_key
                ),
            )
            .ok()
            .map(|element| (cached.order, element, render_as_backdrop))
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

    crate::backend::window::region_rects_within_bounds(
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
    .collect()
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

    crate::backend::window::region_rects_within_bounds(
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
    .collect()
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
    capture_geo: Rectangle<i32, Logical>,
    scale: smithay::utils::Scale<f64>,
    layer_surface: &smithay::desktop::LayerSurface,
) -> Vec<WinitRenderElements> {
    let capture_offset = capture_geo.loc - output.current_location();
    let capture_visual = WindowVisualState {
        origin: smithay::utils::Point::from((0, 0)),
        scale: smithay::utils::Scale::from((1.0, 1.0)),
        translation: smithay::utils::Point::from((-capture_offset.x, -capture_offset.y))
            .to_f64()
            .to_physical_precise_round(scale),
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
    let Some(config) = state.configured_background_effect.clone() else {
        return lower_layers
            .into_iter()
            .flat_map(|layer_surface| {
                window_render::layer_surface_elements(renderer, output, &layer_surface, scale, 1.0)
                    .into_iter()
                    .map(WinitRenderElements::Window)
                    .collect::<Vec<_>>()
            })
            .collect();
    };

    let mut elements = Vec::new();
    for (index, layer_surface) in lower_layers.iter().enumerate() {
        let layer_id = layer_surface.wl_surface().id().protocol_id();
        elements.extend(
            window_render::layer_surface_elements(renderer, output, layer_surface, scale, 1.0)
                .into_iter()
                .map(WinitRenderElements::Window),
        );
        let rects = protocol_background_effect_rects_for_layer(output, layer_surface);
        let Some(effect_rect) = crate::backend::window::bounding_box_for_rects(&rects) else {
            continue;
        };
        let stable_key = format!(
            "__layer_background_effect_{}_{}_{}_{}x{}",
            output.name(),
            layer_id,
            index,
            effect_rect.width,
            effect_rect.height
        );
        let blur_padding = config
            .effect
            .blur_stage()
            .map(|blur| {
                let radius = blur.radius.max(1);
                let passes = blur.passes.max(1);
                (radius * passes * 24 + 32).max(32)
            })
            .unwrap_or(0);
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
        let relevant_source_damage =
            collect_layer_source_damage(state, lower_layers.iter().skip(index + 1).cloned(), true);
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
                        0,
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
                scale,
                lower_layer,
            ));
        }
        if backdrop_scene.is_empty() {
            continue;
        }
        let snapshot = snapshot::capture_snapshot(
            renderer,
            None,
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
        let texture = crate::backend::shader_effect::apply_effect_pipeline(
            renderer,
            backdrop_texture
                .clone()
                .or_else(|| xray_texture.clone())
                .unwrap_or(snapshot.texture),
            xray_texture,
            crate::backend::visual::logical_size_to_physical_buffer_size(
                capture_geo.size.w,
                capture_geo.size.h,
                scale,
            ),
            Some(crate::backend::visual::logical_rect_to_physical_buffer_rect(
                effect_rect,
                capture_geo.loc,
                scale,
            )),
            Some(crate::backend::visual::logical_size_to_physical_buffer_size(
                effect_rect.width,
                effect_rect.height,
                scale,
            )),
            &config.effect,
        )
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
                0,
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
    layer_surface: &smithay::desktop::LayerSurface,
    alpha: f32,
) -> Vec<WinitRenderElements> {
    let layer_id = crate::ssd::layer_runtime_id(layer_surface);
    let Some(config) = state
        .configured_layer_effects
        .get(&layer_id)
        .cloned()
        .or_else(|| state.configured_background_effect.clone())
    else {
        return Vec::new();
    };
    let rects = protocol_background_effect_rects_for_layer(output, layer_surface);
    if rects.is_empty() {
        return Vec::new();
    }

    let Some(effect_rect) = crate::backend::window::bounding_box_for_rects(&rects) else {
        return Vec::new();
    };
    let blur_padding = config
        .effect
        .blur_stage()
        .map(|blur| {
            let radius = blur.radius.max(1);
            let passes = blur.passes.max(1);
            (radius * passes * 24 + 32).max(32)
        })
        .unwrap_or(0);
    let capture_geo = Rectangle::new(
        smithay::utils::Point::from((effect_rect.x - blur_padding, effect_rect.y - blur_padding)),
        (
            effect_rect.width + blur_padding * 2,
            effect_rect.height + blur_padding * 2,
        )
            .into(),
    );
    let actual_capture_geo = capture_geo.intersection(output_geo).unwrap_or(capture_geo);
    let (_, lower_layers) = window_render::layer_surfaces_for_output(output);
    let uses_backdrop = config.effect.uses_backdrop_input();
    let uses_xray = config.effect.uses_xray_backdrop_input();
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
        entries
    };

    let backdrop_texture = if config.effect.uses_backdrop_input() {
        let mut backdrop_scene: Vec<WinitRenderElements> = Vec::new();
        for lower_window in windows_top_to_bottom {
            backdrop_scene.extend(window_scene_elements_for_capture(
                renderer,
                state,
                actual_capture_geo,
                scale,
                lower_window,
            ));
        }
        let (_, lower_layer_elements) =
            window_render::layer_elements_for_output(renderer, output, scale, 1.0);
        let capture_offset = actual_capture_geo.loc - output_geo.loc;
        let capture_visual = WindowVisualState {
            origin: smithay::utils::Point::from((0, 0)),
            scale: smithay::utils::Scale::from((1.0, 1.0)),
            translation: smithay::utils::Point::from((-capture_offset.x, -capture_offset.y))
                .to_f64()
                .to_physical_precise_round(scale),
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
        capture_scene_texture_for_effect(renderer, actual_capture_geo, scale, &backdrop_scene)
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
                scale,
                lower_layer,
            ));
        }
        capture_scene_texture_for_effect(renderer, actual_capture_geo, scale, &xray_scene)
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
                        0,
                        format!("layer-lower:{}:{}", output.name(), rect_key),
                    )
                    .ok()
                    .map(WinitRenderElements::Backdrop)
                })
                .collect();
        }
    }
    let texture = crate::backend::shader_effect::apply_effect_pipeline(
        renderer,
        input_texture,
        xray_texture,
        crate::backend::visual::logical_size_to_physical_buffer_size(
            actual_capture_geo.size.w,
            actual_capture_geo.size.h,
            scale,
        ),
        Some(crate::backend::visual::logical_rect_to_physical_buffer_rect(
            effect_rect,
            actual_capture_geo.loc,
            scale,
        )),
        Some(crate::backend::visual::logical_size_to_physical_buffer_size(
            effect_rect.width,
            effect_rect.height,
            scale,
        )),
        &config.effect,
    )
    .ok();
    let Some(texture) = texture else {
        return Vec::new();
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
                0,
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
) -> Vec<WinitRenderElements> {
    let map = smithay::desktop::layer_map_for_output(output);
    let upper_layers: Vec<_> = [
        smithay::wayland::shell::wlr_layer::Layer::Overlay,
        smithay::wayland::shell::wlr_layer::Layer::Top,
    ]
    .into_iter()
    .flat_map(|layer| map.layers_on(layer).rev().cloned())
    .collect();
    drop(map);

    let mut elements = Vec::new();
    for layer_surface in upper_layers {
        elements.extend(
            window_render::layer_surface_elements(renderer, output, &layer_surface, scale, 1.0)
                .into_iter()
                .map(WinitRenderElements::Window),
        );
        elements.extend(configured_background_effect_elements_for_layer(
            renderer,
            state,
            output,
            output_geo,
            scale,
            windows_top_to_bottom,
            &layer_surface,
            1.0,
        ));
    }
    elements
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
            let blur_padding = config
                .effect
                .blur_stage()
                .map(|blur| {
                    let radius = blur.radius.max(1);
                    let passes = blur.passes.max(1);
                    (radius * passes * 24 + 32).max(32)
                })
                .unwrap_or(0);
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
                            effect_rect.x - output_geo.loc.x,
                            effect_rect.y - output_geo.loc.y,
                        )),
                        (effect_rect.width, effect_rect.height).into(),
                    );
                    return crate::backend::shader_effect::backdrop_shader_element(
                        renderer,
                        existing.id.clone(),
                        existing.commit_counter,
                        existing.texture,
                        local_rect,
                        local_rect,
                        local_rect,
                        &config.effect,
                        alpha,
                        scale.x as f32,
                        None,
                        0,
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
                        actual_capture_geo,
                        scale,
                        lower_window,
                    ));
                }
                let (_, lower_layer_elements) =
                    window_render::layer_elements_for_output(renderer, output, scale, 1.0);
                let capture_offset = actual_capture_geo.loc - output_geo.loc;
                let capture_visual = WindowVisualState {
                    origin: smithay::utils::Point::from((0, 0)),
                    scale: smithay::utils::Scale::from((1.0, 1.0)),
                    translation: smithay::utils::Point::from((
                        -capture_offset.x,
                        -capture_offset.y,
                    ))
                    .to_f64()
                    .to_physical_precise_round(scale),
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
                        scale,
                        lower_layer,
                    ));
                }
                capture_scene_texture_for_effect(renderer, actual_capture_geo, scale, &xray_scene)
            } else {
                None
            };
            let input_texture = backdrop_texture
                .clone()
                .or_else(|| xray_texture.clone())
                .or_else(|| crate::backend::shader_effect::solid_white_texture(renderer).ok())?;
            let sample_region = crate::backend::visual::logical_rect_to_physical_buffer_rect(
                effect_rect,
                actual_capture_geo.loc,
                scale,
            );
            let output_size = crate::backend::visual::logical_size_to_physical_buffer_size(
                effect_rect.width,
                effect_rect.height,
                scale,
            );
            let texture = crate::backend::shader_effect::apply_effect_pipeline(
                renderer,
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
                    effect_rect.x - output_geo.loc.x,
                    effect_rect.y - output_geo.loc.y,
                )),
                (effect_rect.width, effect_rect.height).into(),
            );
            crate::backend::shader_effect::backdrop_shader_element(
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
                local_rect,
                local_rect,
                &config.effect,
                alpha,
                scale.x as f32,
                None,
                0,
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
    capture_geo: Rectangle<i32, Logical>,
    scale: smithay::utils::Scale<f64>,
    window: &smithay::desktop::Window,
) -> Vec<WinitRenderElements> {
    let Some(window_location) = state.space.element_location(window) else {
        return Vec::new();
    };

    let physical_location = (window_location - capture_geo.loc).to_physical_precise_round(scale);
    let visual_state = state
        .window_decorations
        .get(window)
        .map(|decoration| {
            window_visual_state(
                decoration.layout.root.rect,
                decoration.visual_transform,
                capture_geo,
                scale,
            )
        })
        .unwrap_or(WindowVisualState {
            origin: physical_location,
            scale: smithay::utils::Scale::from((1.0, 1.0)),
            translation: (0, 0).into(),
            opacity: 1.0,
        });

    let mut elements = Vec::new();

    if let Some(decoration) = state.window_decorations.get(window) {
        let root_origin = root_physical_origin(decoration.layout.root.rect, capture_geo, scale);
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
    window_location: smithay::utils::Point<i32, Logical>,
    scale: smithay::utils::Scale<f64>,
    z_index: usize,
) -> Result<(), smithay::backend::renderer::gles::GlesError> {
    let Some(decoration) = state.window_decorations.get_mut(window) else {
        return Ok(());
    };
    let client_rect = decoration.client_rect;
    let snapshot_geo = Rectangle::new(
        smithay::utils::Point::from((client_rect.x, client_rect.y)),
        (client_rect.width, client_rect.height).into(),
    );
    let physical_location = (window_location - snapshot_geo.loc).to_physical_precise_round(scale);

    let mut elements: Vec<WinitRenderElements> = Vec::new();
    let surface_elements =
        window_render::surface_elements(window, renderer, physical_location, scale, 1.0);
    let has_client_content = !surface_elements.is_empty();
    elements.extend(
        surface_elements
            .into_iter()
            .map(WinitRenderElements::Window),
    );

    let existing = state.live_window_snapshots.remove(&decoration.snapshot.id);
    if let Some(snapshot) = snapshot::capture_snapshot(
        renderer,
        existing,
        client_rect,
        z_index,
        has_client_content,
        scale,
        &elements,
    )? {
        state
            .live_window_snapshots
            .insert(decoration.snapshot.id.clone(), snapshot);
        if has_client_content {
            if let Some(snapshot) = state.live_window_snapshots.get(&decoration.snapshot.id) {
                if let Ok(complete_snapshot) = snapshot::duplicate_snapshot(renderer, snapshot) {
                    state
                        .complete_window_snapshots
                        .insert(decoration.snapshot.id.clone(), complete_snapshot);
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

            if let Some(element) =
                snapshot::closing_snapshot_element(renderer, snapshot, output_geo, scale)
            {
                elements.extend(transform_snapshot_elements(vec![element], visual));
            }
            elements
        })
        .collect()
}

fn is_identity_visual(visual: WindowVisualState) -> bool {
    visual.translation.x == 0
        && visual.translation.y == 0
        && (visual.scale.x - 1.0).abs() < f64::EPSILON
        && (visual.scale.y - 1.0).abs() < f64::EPSILON
        && (visual.opacity - 1.0).abs() < f32::EPSILON
}
