use std::time::Duration;

use smithay::{
    backend::{
        renderer::{
            damage::OutputDamageTracker, element::solid::SolidColorRenderElement, element::surface::WaylandSurfaceRenderElement,
            element::texture::TextureRenderElement,
            element::utils::{Relocate, RelocateRenderElement, RescaleRenderElement},
            gles::{GlesRenderer, GlesTexture}, ImportEgl, ImportMemWl,
        },
        winit::{self, WinitEvent},
    },
    output::{Mode, Output, PhysicalProperties, Subpixel},
    reexports::calloop::EventLoop,
    reexports::wayland_protocols::wp::presentation_time::server::wp_presentation_feedback,
    utils::{Logical, Monotonic, Rectangle, Transform},
};
use tracing::{trace, warn};

use crate::{
    backend::{damage, damage_blink, decoration, snapshot, window as window_render},
    backend::visual::{WindowVisualState, transformed_rect, window_visual_state},
    presentation::{take_presentation_feedback, update_primary_scanout_output},
    ShojiWM,
};
use smithay::wayland::presentation::Refresh;

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
                    if let Err(err) = state.refresh_window_decorations() {
                        warn!(error = ?err, "failed to refresh window decorations for winit");
                    }

                    let size = backend.window_size();
                    let damage = Rectangle::from_size(size);

                    {
                        let (renderer, mut framebuffer) = backend.bind().unwrap();
                        let output_geo = state.space.output_geometry(&output).unwrap();
                        let scale =
                            smithay::utils::Scale::from(output.current_scale().fractional_scale());
                        let windows: Vec<_> =
                            state.space.elements_for_output(&output).cloned().collect();
                        let windows_top_to_bottom: Vec<_> =
                            windows.iter().rev().cloned().collect();
                        let extra_damage = state.pending_decoration_damage.clone();
                        let (upper_layer_elements, lower_layer_elements) =
                            window_render::layer_elements_for_output(renderer, &output, scale, 1.0);

                        let mut scene_elements: Vec<WinitRenderElements> = Vec::new();
                        scene_elements.extend(
                            upper_layer_elements
                                .into_iter()
                                .map(WinitRenderElements::Window),
                        );
                        for (window_index, window) in windows_top_to_bottom.iter().enumerate() {
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
                            let physical_location =
                                (window_location - output_geo.loc).to_physical_precise_round(scale);
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
                                    origin: physical_location,
                                    scale: smithay::utils::Scale::from((1.0, 1.0)),
                                    translation: (0, 0).into(),
                                    opacity: 1.0,
                                });
                            if decoration_ready {
                                let mut ordered_ui_elements: Vec<(usize, WinitRenderElements)> = Vec::new();
                                let mut ordered_ui_debug: Vec<(usize, &'static str)> = Vec::new();
                                let mut backdrop_items = backdrop_shader_elements_for_window(
                                    renderer,
                                    state,
                                    output_geo,
                                    scale,
                                    &windows_top_to_bottom,
                                    window_index,
                                    window,
                                    visual_state.opacity,
                                    decoration_ready,
                                );
                                if let Some(decoration_state) =
                                    state.window_decorations.get_mut(window)
                                {
                                    let mut background_items = decoration::ordered_background_elements_for_window(
                                        renderer,
                                        decoration_state,
                                        output_geo,
                                        scale,
                                        visual_state.opacity,
                                    )
                                    .inspect_err(|error| {
                                        warn!(?error, "failed to build decoration background elements");
                                    })
                                    .unwrap_or_default();
                                    for cached in decoration_state.buffers.iter() {
                                        ordered_ui_debug.push((cached.order, cached.source_kind));
                                    }
                                    for cached in decoration_state.shader_buffers.iter() {
                                        let kind = match cached.shader.shader_type {
                                            crate::ssd::ShaderType::Pixel => "shader",
                                            crate::ssd::ShaderType::Backdrop => "backdrop",
                                        };
                                        ordered_ui_debug.push((cached.order, kind));
                                    }
                                    background_items.append(&mut backdrop_items);
                                    background_items.sort_by_key(|(order, _)| *order);
                                    for (order, element) in background_items {
                                        ordered_ui_elements.extend(
                                            transform_decoration_elements(vec![element], visual_state)
                                                .into_iter()
                                                .map(|item| (order, item)),
                                        );
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
                                    ordered_ui_debug.push((order, "icon"));
                                    ordered_ui_elements.extend(
                                        transform_text_elements(vec![element], visual_state)
                                            .into_iter()
                                            .map(|item| (order, item)),
                                    );
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
                                    ordered_ui_debug.push((order, "label"));
                                    ordered_ui_elements.extend(
                                        transform_text_elements(vec![element], visual_state)
                                            .into_iter()
                                            .map(|item| (order, item)),
                                    );
                                }

                                ordered_ui_elements.sort_by_key(|(order, _)| *order);
                                ordered_ui_debug.sort_by_key(|(order, _)| *order);
                                trace!(
                                    window_id = window_id,
                                    ?ordered_ui_debug,
                                    "ordered ui elements for winit window"
                                );
                                scene_elements.extend(
                                    ordered_ui_elements.into_iter().map(|(_, element)| element),
                                );
                            }

                            let content_clip = state
                                .window_decorations
                                .get(window)
                                .and_then(|decoration| decoration.content_clip);

                            if let Some(content_clip) = content_clip {
                                scene_elements.extend(
                                    transform_clipped_elements(
                                        window_render::clipped_surface_elements(
                                            window,
                                            renderer,
                                            physical_location,
                                            scale,
                                            visual_state.opacity,
                                            Some(content_clip),
                                        )
                                        .inspect_err(|error| {
                                            warn!(?error, "failed to build clipped surface elements");
                                        })
                                        .unwrap_or_default(),
                                        visual_state,
                                    )
                                    .into_iter(),
                                );
                            } else {
                                scene_elements.extend(
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

                            scene_elements.extend(
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
                        scene_elements.extend(
                            lower_layer_elements
                                .into_iter()
                                .map(WinitRenderElements::Window),
                        );

                        if state.damage_blink_enabled {
                            if let Ok((damage, _)) =
                                blink_damage_tracker.damage_output(1, &scene_elements)
                            {
                                if let Some(damage) = damage {
                                    state.record_damage_blink(&output, damage);
                                }
                            }
                        }

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
                        elements.extend(
                            damage::elements_for_output(&extra_damage, output_geo)
                                .into_iter()
                                .map(WinitRenderElements::Damage),
                        );
                        elements.extend(scene_elements);

                        trace!(
                            output = %output.name(),
                            window_count = state.space.elements().count(),
                            render_element_count = elements.len(),
                            "rendering winit frame"
                        );

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
                    backend.submit(Some(&[damage])).unwrap();

                    state.space.refresh();
                    state.popups.cleanup();
                    state.pending_decoration_damage.clear();
                    state.finish_damage_blink_frame();
                    let _ = state.display_handle.flush_clients();

                    // Ask for redraw to schedule new frame.
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
    TransformedText=RelocateRenderElement<RescaleRenderElement<crate::backend::text::DecorationTextureElements>>,
    Snapshot=TextureRenderElement<GlesTexture>,
    TransformedSnapshot=RelocateRenderElement<RescaleRenderElement<TextureRenderElement<GlesTexture>>>,
    Damage=crate::backend::damage::DamageOnlyElement,
    Blink=SolidColorRenderElement,
    Decoration=crate::backend::decoration::DecorationSceneElements,
    TransformedDecoration=RelocateRenderElement<RescaleRenderElement<crate::backend::decoration::DecorationSceneElements>>,
}

fn transform_window_elements(
    elements: Vec<WaylandSurfaceRenderElement<GlesRenderer>>,
    visual: WindowVisualState,
    direct: fn(WaylandSurfaceRenderElement<GlesRenderer>) -> WinitRenderElements,
    transformed: fn(RelocateRenderElement<RescaleRenderElement<WaylandSurfaceRenderElement<GlesRenderer>>>) -> WinitRenderElements,
) -> Vec<WinitRenderElements> {
    if is_identity_visual(visual) {
        return elements.into_iter().map(direct).collect();
    }

    elements
        .into_iter()
        .map(|element| {
            transformed(RelocateRenderElement::from_element(
                RescaleRenderElement::from_element(
                    element,
                    visual.origin,
                    visual.scale,
                ),
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
        return elements.into_iter().map(WinitRenderElements::Clipped).collect();
    }

    elements
        .into_iter()
        .map(|element| {
            WinitRenderElements::TransformedClipped(RelocateRenderElement::from_element(
                RescaleRenderElement::from_element(
                    element,
                    visual.origin,
                    visual.scale,
                ),
                visual.translation,
                Relocate::Relative,
            ))
        })
        .collect()
}

fn transform_text_elements(
    elements: Vec<crate::backend::text::DecorationTextureElements>,
    visual: WindowVisualState,
) -> Vec<WinitRenderElements> {
    if is_identity_visual(visual) {
        return elements.into_iter().map(WinitRenderElements::Text).collect();
    }

    elements
        .into_iter()
        .map(|element| {
            WinitRenderElements::TransformedText(RelocateRenderElement::from_element(
                RescaleRenderElement::from_element(
                    element,
                    visual.origin,
                    visual.scale,
                ),
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
        return elements.into_iter().map(WinitRenderElements::Snapshot).collect();
    }

    elements
        .into_iter()
        .map(|element| {
            WinitRenderElements::TransformedSnapshot(RelocateRenderElement::from_element(
                RescaleRenderElement::from_element(
                    element,
                    visual.origin,
                    visual.scale,
                ),
                visual.translation,
                Relocate::Relative,
            ))
        })
        .collect()
}

fn transform_decoration_elements(
    elements: Vec<crate::backend::decoration::DecorationSceneElements>,
    visual: WindowVisualState,
) -> Vec<WinitRenderElements> {
    if is_identity_visual(visual) {
        return elements
            .into_iter()
            .map(WinitRenderElements::Decoration)
            .collect();
    }

    elements
        .into_iter()
        .map(|element| {
            WinitRenderElements::TransformedDecoration(RelocateRenderElement::from_element(
                RescaleRenderElement::from_element(
                    element,
                    visual.origin,
                    visual.scale,
                ),
                visual.translation,
                Relocate::Relative,
            ))
        })
        .collect()
}

fn backdrop_shader_elements_for_window(
    renderer: &mut GlesRenderer,
    state: &ShojiWM,
    output_geo: Rectangle<i32, Logical>,
    scale: smithay::utils::Scale<f64>,
    windows_top_to_bottom: &[smithay::desktop::Window],
    window_index: usize,
    window: &smithay::desktop::Window,
    alpha: f32,
    has_backdrop_source: bool,
) -> Vec<(usize, crate::backend::decoration::DecorationSceneElements)> {
    if !has_backdrop_source {
        return Vec::new();
    }
    let Some(decoration) = state.window_decorations.get(window) else {
        return Vec::new();
    };

    decoration
        .shader_buffers
        .iter()
        .filter(|cached| matches!(cached.shader.shader_type, crate::ssd::ShaderType::Backdrop))
        .filter_map(|cached| {
            let effect_rect = transformed_rect(
                cached.rect,
                decoration.layout.root.rect,
                decoration.visual_transform,
            );
            trace!(
                window_id = decoration.snapshot.id,
                effect_rect = ?effect_rect,
                alpha,
                has_backdrop_source,
                shader_type = ?cached.shader.shader_type,
                "considering backdrop shader effect for winit window"
            );
            let blur_padding = cached
                .shader
                .blur
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
            let mut backdrop_scene: Vec<WinitRenderElements> = Vec::new();
            for lower_window in windows_top_to_bottom.iter().skip(window_index + 1) {
                backdrop_scene.extend(window_scene_elements_for_capture(
                    renderer,
                    state,
                    capture_geo,
                    scale,
                    lower_window,
                ));
            }
            if backdrop_scene.is_empty() {
                trace!(
                    window_id = decoration.snapshot.id,
                    effect_rect = ?effect_rect,
                    "skipping backdrop shader effect because backdrop scene is empty"
                );
                return None;
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
            .flatten()?;
            let texture = if let Some(blur) = cached.shader.blur {
                crate::backend::shader_effect::preblur_backdrop_texture(
                    renderer,
                    snapshot.texture,
                    (capture_geo.size.w, capture_geo.size.h),
                    blur.radius,
                    blur.passes,
                )
                .ok()?
            } else {
                snapshot.texture
            };

            let local_rect = Rectangle::new(
                smithay::utils::Point::from((
                    cached.rect.x - output_geo.loc.x,
                    cached.rect.y - output_geo.loc.y,
                )),
                (cached.rect.width, cached.rect.height).into(),
            );
            let clip_rect = cached.clip_rect.map(|clip_rect| {
                Rectangle::new(
                    smithay::utils::Point::from((
                        clip_rect.x - output_geo.loc.x,
                        clip_rect.y - output_geo.loc.y,
                    )),
                    (clip_rect.width, clip_rect.height).into(),
                )
            });
            let local_sample_rect = Rectangle::new(
                smithay::utils::Point::from((
                    effect_rect.x - output_geo.loc.x,
                    effect_rect.y - output_geo.loc.y,
                )),
                (effect_rect.width, effect_rect.height).into(),
            );
            let local_capture_rect = Rectangle::new(
                smithay::utils::Point::from((
                    capture_geo.loc.x - output_geo.loc.x,
                    capture_geo.loc.y - output_geo.loc.y,
                )),
                (capture_geo.size.w, capture_geo.size.h).into(),
            );

            crate::backend::shader_effect::backdrop_shader_element(
                renderer,
                texture,
                local_rect,
                local_sample_rect,
                local_capture_rect,
                &cached.shader,
                alpha,
                scale.x as f32,
                clip_rect,
                cached.clip_radius,
            )
            .ok()
            .map(|element| {
                trace!(
                    window_id = decoration.snapshot.id,
                    effect_rect = ?effect_rect,
                    alpha,
                    "built backdrop shader effect element for winit window"
                );
                (
                    cached.order,
                    crate::backend::decoration::DecorationSceneElements::Backdrop(element),
                )
            })
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

    let physical_location =
        (window_location - capture_geo.loc).to_physical_precise_round(scale);
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
                    transform_decoration_elements(vec![element], visual_state)
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
                    transform_text_elements(vec![element], visual_state)
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
                    transform_text_elements(vec![element], visual_state)
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
    let physical_location =
        (window_location - snapshot_geo.loc).to_physical_precise_round(scale);

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

            let mut elements = Vec::new();
            if let Ok(icon_elements) = crate::backend::icon::icon_elements_for_decoration(
                renderer,
                &snapshot.decoration,
                output_geo,
                scale,
                visual.opacity,
            ) {
                elements.extend(transform_text_elements(icon_elements, visual));
            }
            if let Ok(text_elements) = crate::backend::text::text_elements_for_decoration(
                renderer,
                &snapshot.decoration,
                output_geo,
                scale,
                visual.opacity,
            ) {
                elements.extend(transform_text_elements(text_elements, visual));
            }
            let mut decoration = snapshot.decoration.clone();
            if let Ok(background_elements) = decoration::background_elements_for_window(
                renderer,
                &mut decoration,
                output_geo,
                scale,
                visual.opacity,
            ) {
                elements.extend(transform_decoration_elements(background_elements, visual));
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
