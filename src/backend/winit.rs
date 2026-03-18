use std::time::Duration;

use smithay::{
    backend::{
        renderer::{
            damage::OutputDamageTracker,
            element::solid::SolidColorRenderElement,
            element::memory::MemoryRenderBufferRenderElement,
            element::surface::WaylandSurfaceRenderElement,
            gles::GlesRenderer,
            ImportEgl, ImportMemWl,
        },
        winit::{self, WinitEvent},
    },
    desktop::layer_map_for_output,
    output::{Mode, Output, PhysicalProperties, Subpixel},
    reexports::calloop::EventLoop,
    utils::{Rectangle, Transform},
};
use tracing::{trace, warn};

use crate::{
    backend::{damage, damage_blink, decoration, window as window_render},
    ShojiWM,
};

pub fn init_winit(
    event_loop: &mut EventLoop<ShojiWM>,
    state: &mut ShojiWM,
) -> Result<(), Box<dyn std::error::Error>> {
    let (mut backend, winit) = winit::init::<GlesRenderer>()?;
    match backend.renderer().bind_wl_display(&state.display_handle) {
        Ok(()) => trace!("winit renderer bound wl_display for EGL clients"),
        Err(error) => warn!(?error, "failed to bind wl_display for winit EGL clients"),
    }
    state.shm_state.update_formats(backend.renderer().shm_formats());

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
                        let windows: Vec<_> = state.space.elements_for_output(&output).cloned().collect();
                        let extra_damage = state.pending_decoration_damage.clone();
                        let (upper_layer_elements, lower_layer_elements) =
                            window_render::layer_elements_for_output(renderer, &output, scale, 1.0);

                        let mut scene_elements: Vec<WinitRenderElements> = Vec::new();
                        scene_elements.extend(
                            upper_layer_elements
                                .into_iter()
                                .map(WinitRenderElements::Window),
                        );
                        for window in windows.iter().rev() {
                            let Some(window_location) = state.space.element_location(window) else {
                                continue;
                            };
                            let render_location = window_location - window.geometry().loc;
                            let physical_location =
                                (render_location - output_geo.loc).to_physical_precise_round(scale);

                            scene_elements.extend(
                                window_render::popup_elements(window, renderer, physical_location, scale, 1.0)
                                    .into_iter()
                                    .map(WinitRenderElements::Window),
                            );

                            scene_elements.extend(
                                decoration::text_elements_for_window(
                                    renderer,
                                    &state.space,
                                    &state.window_decorations,
                                    &output,
                                    window,
                                )
                                .unwrap_or_default()
                                    .into_iter()
                                    .map(WinitRenderElements::Text),
                            );

                            scene_elements.extend(
                                decoration::rounded_elements_for_window(
                                    renderer,
                                    state.window_decorations.get_mut(window).unwrap(),
                                    output_geo,
                                    scale,
                                    window,
                                )
                                .unwrap_or_default()
                                    .into_iter()
                                    .map(WinitRenderElements::Decoration),
                            );

                            scene_elements.extend(
                                window_render::clipped_surface_elements(
                                    window,
                                    renderer,
                                    physical_location,
                                    scale,
                                    1.0,
                                    state
                                        .window_decorations
                                        .get(window)
                                        .and_then(|decoration| decoration.content_clip),
                                )
                                .inspect_err(|error| {
                                    warn!(?error, "failed to build clipped surface elements");
                                })
                                .unwrap_or_default()
                                .into_iter()
                                .map(WinitRenderElements::Clipped),
                            );
                        }
                        scene_elements.extend(
                            lower_layer_elements
                                .into_iter()
                                .map(WinitRenderElements::Window),
                        );

                        if state.damage_blink_enabled {
                            if let Ok((damage, _)) = blink_damage_tracker.damage_output(1, &scene_elements) {
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

                        let _ = damage_tracker.render_output(
                            renderer,
                            &mut framebuffer,
                            0,
                            &elements,
                            [0.1, 0.1, 0.1, 1.0],
                        );
                    }
                    backend.submit(Some(&[damage])).unwrap();

                    state.space.elements().for_each(|window| {
                        window.send_frame(
                            &output,
                            state.start_time.elapsed(),
                            Some(Duration::ZERO),
                            |_, _| Some(output.clone()),
                        )
                    });
                    {
                        let map = layer_map_for_output(&output);
                        for layer_surface in map.layers() {
                            layer_surface.send_frame(
                                &output,
                                state.start_time.elapsed(),
                                Some(Duration::ZERO),
                                |_, _| Some(output.clone()),
                            );
                        }
                    }

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
    Clipped=crate::backend::clipped_surface::ClippedSurfaceElement,
    Text=MemoryRenderBufferRenderElement<GlesRenderer>,
    Damage=crate::backend::damage::DamageOnlyElement,
    Blink=SolidColorRenderElement,
    Decoration=crate::backend::rounded::StableRoundedElement,
}
