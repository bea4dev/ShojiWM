use std::{collections::HashMap, path::Path, time::{Duration, Instant}};

use smithay::{
    backend::{
        allocator::{gbm::GbmAllocator, Fourcc},
        drm::{
            compositor::FrameFlags,
            exporter::gbm::{GbmFramebufferExporter, NodeFilter},
            output::{DrmOutput, DrmOutputManager, DrmOutputRenderElements},
            DrmDevice, DrmDeviceFd, DrmEvent, DrmEventMetadata, DrmEventTime, DrmNode,
        },
        egl::{context::ContextPriority, EGLContext, EGLDisplay},
        renderer::{
            damage::OutputDamageTracker,
            element::{
                memory::{MemoryRenderBuffer, MemoryRenderBufferRenderElement},
                solid::SolidColorRenderElement,
                surface::WaylandSurfaceRenderElement,
                utils::{Relocate, RelocateRenderElement, RescaleRenderElement},
                AsRenderElements,
            },
            gles::GlesRenderer,
            ImportDma, ImportEgl, ImportMemWl,
        },
        session::{libseat::LibSeatSession, Session},
    },
    desktop::layer_map_for_output,
    input::pointer::{CursorImageAttributes, CursorImageStatus},
    output::{Mode as WlMode, Output, PhysicalProperties},
    reexports::{
        calloop::{timer::{TimeoutAction, Timer}, EventLoop, LoopHandle},
        drm::control::{connector, crtc},
        gbm::{BufferObjectFlags, Device, Format},
        rustix::fs::OFlags,
        wayland_protocols::wp::presentation_time::server::wp_presentation_feedback,
    },
    render_elements,
    utils::{DeviceFd, IsAlive, Monotonic, Scale, Transform},
    wayland::{compositor, dmabuf::DmabufFeedbackBuilder},
};
use smithay_drm_extras::drm_scanner::{DrmScanEvent, DrmScanner};
use tracing::{debug, info, trace, warn};

use crate::{
    backend::damage, backend::damage_blink, backend::decoration, backend::window as window_render,
    backend::visual::{AlphaRenderElement, WindowVisualState, window_visual_state},
    config::DisplayModePreference,
    presentation::{take_presentation_feedback, update_primary_scanout_output},
    drawing::PointerRenderElement, state::ShojiWM,
};
use smithay::wayland::presentation::Refresh;

const CLEAR_COLOR: [f32; 4] = [0.08, 0.10, 0.13, 1.0];
const TTY_FRAME_FLAGS: FrameFlags = FrameFlags::DEFAULT;

type GbmDrmOutput =
    DrmOutput<
        GbmAllocator<DrmDeviceFd>,
        GbmFramebufferExporter<DrmDeviceFd>,
        Option<smithay::desktop::utils::OutputPresentationFeedback>,
        DrmDeviceFd,
    >;

struct SurfaceData {
    output: Output,
    drm_output: GbmDrmOutput,
    blink_damage_tracker: OutputDamageTracker,
    frame_pending: bool,
    repaint_scheduled: bool,
    frame_duration: Duration,
    next_frame_target: Option<Duration>,
    estimated_render_duration: Duration,
    last_presented_at: Option<Duration>,
    last_frame_callback_at: Option<Duration>,
}

enum RenderSurfaceOutcome {
    SkippedPending,
    Processed,
}

pub struct BackendData {
    pub drm_scanner: DrmScanner,
    pub drm_output_manager: DrmOutputManager<
        GbmAllocator<DrmDeviceFd>,
        GbmFramebufferExporter<DrmDeviceFd>,
        Option<smithay::desktop::utils::OutputPresentationFeedback>,
        DrmDeviceFd,
    >,
    pub renderer: GlesRenderer,
    surfaces: HashMap<crtc::Handle, SurfaceData>,
}

pub fn device_added(
    state: &mut ShojiWM,
    event_loop: &EventLoop<ShojiWM>,
    session: &mut LibSeatSession,
    node: DrmNode,
    path: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    info!(?node, path = ?path, "opening drm device");
    let fd = session.open(
        path,
        OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOCTTY | OFlags::NONBLOCK,
    )?;
    let fd = DrmDeviceFd::new(DeviceFd::from(fd));

    let (drm, drm_events) = DrmDevice::new(fd.clone(), true)?;
    let gbm = Device::new(fd.clone())?;

    let egl = unsafe { EGLDisplay::new(gbm.clone())? };
    let ctx = EGLContext::new_with_priority(&egl, ContextPriority::High)?;
    let mut renderer = unsafe { GlesRenderer::new(ctx)? };
    match renderer.bind_wl_display(&state.display_handle) {
        Ok(()) => info!(?node, "bound wl_display for tty EGL clients"),
        Err(error) => warn!(
            ?node,
            ?error,
            "failed to bind wl_display for tty EGL clients"
        ),
    }
    state.shm_state.update_formats(renderer.shm_formats());
    if state.dmabuf_global.is_none() {
        let default_feedback = DmabufFeedbackBuilder::new(node.dev_id(), renderer.dmabuf_formats())
            .build()
            .unwrap();
        let global = state
            .dmabuf_state
            .create_global_with_default_feedback::<ShojiWM>(
                &state.display_handle,
                &default_feedback,
            );
        state.dmabuf_global = Some(global);
        info!(?node, "initialized linux-dmabuf global");
    }

    let allocator = GbmAllocator::new(
        gbm.clone(),
        BufferObjectFlags::RENDERING | BufferObjectFlags::SCANOUT,
    );
    let exporter = GbmFramebufferExporter::new(gbm.clone(), NodeFilter::from(node));

    let render_formats = renderer.egl_context().dmabuf_render_formats().clone();
    let drm_output_manager = DrmOutputManager::new(
        drm,
        allocator,
        exporter,
        Some(gbm),
        [Format::Argb8888],
        render_formats,
    );

    let backend = BackendData {
        drm_scanner: DrmScanner::new(),
        drm_output_manager,
        renderer,
        surfaces: HashMap::new(),
    };
    state.tty_backends.insert(node.clone(), backend);
    info!(?node, "drm backend stored in state");

    let backend = state.tty_backends.get_mut(&node).unwrap();

    let loop_handle = event_loop.handle();
    event_loop
        .handle()
        .insert_source(drm_events, move |event, metadata, state| {
            if let DrmEvent::VBlank(crtc) = event {
                trace!(?node, ?crtc, "received drm vblank");
                frame_finish(state, &loop_handle, node, crtc, metadata);
            }
        })?;

    for scan in backend
        .drm_scanner
        .scan_connectors(backend.drm_output_manager.device())?
    {
        debug!(?node, ?scan, "connector scan event");
        if let DrmScanEvent::Connected {
            connector,
            crtc: Some(crtc),
        } = scan
        {
            connector_connected(state, node, crtc, connector)?;
        }
    }

    Ok(())
}

fn frame_finish(
    state: &mut ShojiWM,
    loop_handle: &LoopHandle<'_, ShojiWM>,
    node: DrmNode,
    crtc: crtc::Handle,
    metadata: &mut Option<DrmEventMetadata>,
) {
    let Some(backend) = state.tty_backends.get_mut(&node) else {
        warn!(?node, ?crtc, "frame_finish without backend");
        return;
    };
    let Some(surface) = backend.surfaces.get_mut(&crtc) else {
        warn!(?node, ?crtc, "frame_finish without surface");
        return;
    };

    trace!(?node, ?crtc, "marking frame submitted");
    let submit_result = surface.drm_output.frame_submitted();
    let presentation_clock = metadata
        .as_ref()
        .and_then(|metadata| match metadata.time {
            DrmEventTime::Monotonic(tp) => Some(tp),
            DrmEventTime::Realtime(_) => None,
        })
        .unwrap_or_else(|| Duration::from(state.clock.now()));
    surface.next_frame_target = Some(presentation_clock + surface.frame_duration);

    if let Ok(user_data) = submit_result {
        let clock = presentation_clock;
        let sequence = metadata.as_ref().map(|metadata| metadata.sequence).unwrap_or(0);
        let flags = if metadata
            .as_ref()
            .is_some_and(|metadata| matches!(metadata.time, DrmEventTime::Monotonic(_)))
        {
            wp_presentation_feedback::Kind::Vsync
                | wp_presentation_feedback::Kind::HwClock
                | wp_presentation_feedback::Kind::HwCompletion
        } else {
            wp_presentation_feedback::Kind::Vsync
        };

        if let Some(mut feedback) = user_data.flatten() {
            feedback.presented::<Duration, Monotonic>(
                clock,
                Refresh::fixed(surface.frame_duration),
                sequence as u64,
                flags,
            );
        }
    }

    if let Some(previous_presented) = surface.last_presented_at.replace(presentation_clock) {
        trace!(
            ?node,
            ?crtc,
            output = %surface.output.name(),
            presented_delta_ms = (presentation_clock.saturating_sub(previous_presented).as_secs_f64() * 1000.0),
            refresh_mhz = surface.output.current_mode().map(|mode| mode.refresh),
            "tty presentation cadence"
        );
    }

    surface.frame_pending = false;

    if surface.repaint_scheduled {
        return;
    }

    let Some(repaint_delay) = repaint_delay(surface) else {
        return;
    };
    let repaint_target = presentation_clock + repaint_delay;
    let repaint_delay = repaint_target.saturating_sub(Duration::from(state.clock.now()));

    surface.repaint_scheduled = true;
    if loop_handle
        .insert_source(Timer::from_duration(repaint_delay), move |_, _, state| {
            if let Some(backend) = state.tty_backends.get_mut(&node)
                && let Some(surface) = backend.surfaces.get_mut(&crtc)
            {
                surface.repaint_scheduled = false;
            }
            state.schedule_redraw();
            TimeoutAction::Drop
        })
        .is_err()
    {
        if let Some(backend) = state.tty_backends.get_mut(&node)
            && let Some(surface) = backend.surfaces.get_mut(&crtc)
        {
            surface.repaint_scheduled = false;
        }
        warn!(?node, ?crtc, "failed to schedule tty repaint timer");
    }
}

pub fn render_if_needed(
    state: &mut ShojiWM,
) -> Result<(), Box<dyn std::error::Error>> {
    if !state.needs_redraw {
        return Ok(());
    }

    state.refresh_window_decorations()?;

    trace!(
        backend_count = state.tty_backends.len(),
        window_count = state.space.elements().count(),
        "rendering pending redraw"
    );
    state.needs_redraw = false;
    let mut skipped_for_pending_frame = false;
    let mut processed_outputs: Vec<String> = Vec::new();

    let nodes: Vec<_> = state.tty_backends.keys().copied().collect();
    for node in nodes {
        let crtcs: Vec<_> = state
            .tty_backends
            .get(&node)
            .unwrap()
            .surfaces
            .keys()
            .copied()
            .collect();

        for crtc in crtcs {
            match render_surface(state, node, crtc)? {
                RenderSurfaceOutcome::SkippedPending => skipped_for_pending_frame = true,
                RenderSurfaceOutcome::Processed => {
                    let output_name = state
                        .tty_backends
                        .get(&node)
                        .and_then(|backend| backend.surfaces.get(&crtc))
                        .map(|surface| surface.output.name())
                        .unwrap();
                    processed_outputs.push(output_name);
                }
            }
        }
    }

    if skipped_for_pending_frame {
        state.needs_redraw = true;
    }

    state.pending_decoration_damage.clear();
    state.finish_damage_blink_for_outputs(processed_outputs.iter().map(String::as_str));

    Ok(())
}

render_elements! {
    pub TtyRenderElements<=GlesRenderer>;
    Window=WaylandSurfaceRenderElement<GlesRenderer>,
    TransformedWindow=RelocateRenderElement<RescaleRenderElement<AlphaRenderElement<WaylandSurfaceRenderElement<GlesRenderer>>>>,
    Clipped=crate::backend::clipped_surface::ClippedSurfaceElement,
    TransformedClipped=RelocateRenderElement<RescaleRenderElement<AlphaRenderElement<crate::backend::clipped_surface::ClippedSurfaceElement>>>,
    Text=MemoryRenderBufferRenderElement<GlesRenderer>,
    TransformedText=RelocateRenderElement<RescaleRenderElement<AlphaRenderElement<MemoryRenderBufferRenderElement<GlesRenderer>>>>,
    Damage=crate::backend::damage::DamageOnlyElement,
    Blink=SolidColorRenderElement,
    Decoration=crate::backend::rounded::StableRoundedElement,
    TransformedDecoration=RelocateRenderElement<RescaleRenderElement<AlphaRenderElement<crate::backend::rounded::StableRoundedElement>>>,
    Cursor=PointerRenderElement<GlesRenderer>,
}

fn render_surface(
    state: &mut ShojiWM,
    node: DrmNode,
    crtc: crtc::Handle,
) -> Result<RenderSurfaceOutcome, Box<dyn std::error::Error>> {
    let output = state
        .tty_backends
        .get(&node)
        .and_then(|backend| backend.surfaces.get(&crtc))
        .map(|surface| surface.output.clone())
        .unwrap();

    if state
        .tty_backends
        .get(&node)
        .and_then(|backend| backend.surfaces.get(&crtc))
        .is_some_and(|surface| surface.frame_pending)
    {
        trace!(
            ?node,
            ?crtc,
            output = %output.name(),
            "skipping tty render while previous frame is pending"
        );
        return Ok(RenderSurfaceOutcome::SkippedPending);
    }

    let frame_duration = state
        .tty_backends
        .get(&node)
        .and_then(|backend| backend.surfaces.get(&crtc))
        .map(|surface| surface.frame_duration)
        .unwrap_or(Duration::ZERO);
    let fallback_frame_time = Duration::from(state.clock.now()) + frame_duration;
    let frame_target = state
        .tty_backends
        .get(&node)
        .and_then(|backend| backend.surfaces.get(&crtc))
        .and_then(|surface| surface.next_frame_target)
        .unwrap_or(fallback_frame_time);
    state.pre_repaint(&output, frame_target.into());

    let should_capture_blink = state.damage_blink_enabled;
    let blink_visible = state.damage_blink_rects_for_output(&output).to_vec();
    let output_geo = state.space.output_geometry(&output).unwrap();
    let mut extra_damage = state.pending_decoration_damage.clone();
    if should_capture_blink && !blink_visible.is_empty() {
        extra_damage.push(crate::ssd::LogicalRect::new(
            output_geo.loc.x,
            output_geo.loc.y,
            output_geo.size.w,
            output_geo.size.h,
        ));
    }
    let captured_blink_damage = {
        let ShojiWM {
            space,
            tty_backends,
            start_time,
            cursor_status,
            cursor_override,
            cursor_theme,
            pointer_images,
            current_pointer_image,
            pointer_element,
            seat,
            ..
        } = state;

        let backend = tty_backends.get_mut(&node).unwrap();
        let surface = backend.surfaces.get_mut(&crtc).unwrap();
        let render_started_at = Instant::now();
        let frame_time = surface
            .next_frame_target
            .take()
            .unwrap_or(fallback_frame_time);
        if let Some(previous_callback) = surface.last_frame_callback_at.replace(frame_time) {
            trace!(
                ?node,
                ?crtc,
                output = %output.name(),
                callback_delta_ms = (frame_time.saturating_sub(previous_callback).as_secs_f64() * 1000.0),
                frame_time_ms = frame_time.as_secs_f64() * 1000.0,
                target_refresh_mhz = output.current_mode().map(|mode| mode.refresh),
                "tty frame callback cadence"
            );
        }

        let mut cursor_elements: Vec<TtyRenderElements> = Vec::new();

        let pointer_pos = seat.get_pointer().unwrap().current_location();
        let output_geo = space.output_geometry(&output).unwrap();
        let scale = Scale::from(output.current_scale().fractional_scale());
        let windows: Vec<_> = space.elements_for_output(&output).cloned().collect();
        let all_windows: Vec<_> = space.elements().cloned().collect();
        let window_count = all_windows.len();
        let (upper_layer_elements, lower_layer_elements) =
            window_render::layer_elements_for_output(&mut backend.renderer, &output, scale, 1.0);

        if output_geo.to_f64().contains(pointer_pos) {
            let reset =
                matches!(cursor_status, CursorImageStatus::Surface(surface) if !surface.alive());
            if reset {
                *cursor_status = CursorImageStatus::default_named();
            }

            let effective_cursor_status = cursor_override
                .map(CursorImageStatus::Named)
                .unwrap_or_else(|| cursor_status.clone());

            let hotspot = if let CursorImageStatus::Surface(surface) = &effective_cursor_status {
                *current_pointer_image = None;
                compositor::with_states(surface, |states| {
                    states
                        .data_map
                        .get::<std::sync::Mutex<CursorImageAttributes>>()
                        .unwrap()
                        .lock()
                        .unwrap()
                        .hotspot
                })
            } else {
                let icon = match &effective_cursor_status {
                    CursorImageStatus::Named(icon) => *icon,
                    _ => smithay::input::pointer::CursorIcon::Default,
                };
                let frame = cursor_theme.get_image(icon, 1, start_time.elapsed());
                let buffer = pointer_images
                    .iter()
                    .find_map(|(image, buffer)| (image == &frame).then_some(buffer.clone()))
                    .unwrap_or_else(|| {
                        let buffer = MemoryRenderBuffer::from_slice(
                            &frame.pixels_rgba,
                            Fourcc::Argb8888,
                            (frame.width as i32, frame.height as i32),
                            1,
                            Transform::Normal,
                            None,
                        );
                        pointer_images.push((frame.clone(), buffer.clone()));
                        buffer
                    });
                if current_pointer_image.as_ref() != Some(&frame) {
                    pointer_element.set_buffer(buffer);
                    *current_pointer_image = Some(frame.clone());
                }
                (frame.xhot as i32, frame.yhot as i32).into()
            };

            pointer_element.set_status(effective_cursor_status);

            let cursor_location = (pointer_pos - output_geo.loc.to_f64() - hotspot.to_f64())
                .to_physical(scale)
                .to_i32_round();

            cursor_elements.extend(
                pointer_element
                    .render_elements(&mut backend.renderer, cursor_location, scale, 1.0)
                    .into_iter()
                    .map(TtyRenderElements::Cursor),
            );
        }

        let mut scene_elements: Vec<TtyRenderElements> = Vec::new();
        scene_elements.extend(
            upper_layer_elements
                .into_iter()
                .map(TtyRenderElements::Window),
        );

        for window in windows.iter().rev() {
            let Some(window_location) = space.element_location(window) else {
                continue;
            };
            let physical_location =
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
                    origin: physical_location,
                    scale: smithay::utils::Scale::from((1.0, 1.0)),
                    translation: (0, 0).into(),
                    opacity: 1.0,
                });

            scene_elements.extend(
                transform_window_elements(
                    window_render::popup_elements(
                        window,
                        &mut backend.renderer,
                        physical_location,
                        scale,
                        1.0,
                    ),
                    visual_state,
                    TtyRenderElements::Window,
                    TtyRenderElements::TransformedWindow,
                )
                .into_iter(),
            );

            scene_elements.extend(
                transform_text_elements(
                    decoration::icon_elements_for_window(
                        &mut backend.renderer,
                        space,
                        &state.window_decorations,
                        &output,
                        window,
                    )?,
                    visual_state,
                )?
                .into_iter()
                .collect::<Vec<_>>(),
            );

            scene_elements.extend(
                transform_text_elements(
                    decoration::text_elements_for_window(
                        &mut backend.renderer,
                        space,
                        &state.window_decorations,
                        &output,
                        window,
                    )?,
                    visual_state,
                )?
                .into_iter()
                .collect::<Vec<_>>(),
            );

            scene_elements.extend(
                transform_decoration_elements(
                    decoration::rounded_elements_for_window(
                        &mut backend.renderer,
                        state.window_decorations.get_mut(window).unwrap(),
                        output_geo,
                        scale,
                        window,
                    )?,
                    visual_state,
                )?
                .into_iter()
                .collect::<Vec<_>>(),
            );

            scene_elements.extend(
                transform_clipped_elements(
                    window_render::clipped_surface_elements(
                        window,
                        &mut backend.renderer,
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
                    .unwrap_or_default(),
                    visual_state,
                )
                .into_iter(),
            );
        }
        scene_elements.extend(
            lower_layer_elements
                .into_iter()
                .map(TtyRenderElements::Window),
        );

        let captured_blink_damage = if should_capture_blink {
            match surface
                .blink_damage_tracker
                .damage_output(1, &scene_elements)
            {
                Ok((damage, _)) => damage.cloned(),
                Err(_) => None,
            }
        } else {
            None
        };

        let cursor_status_for_log = cursor_override
            .map(CursorImageStatus::Named)
            .unwrap_or_else(|| cursor_status.clone());
        let mut elements: Vec<TtyRenderElements> = Vec::new();
        elements.extend(
            damage_blink::elements_for_output(&blink_visible, output_geo, scale)
                .into_iter()
                .map(TtyRenderElements::Blink),
        );
        elements.extend(cursor_elements);
        elements.extend(
            damage::elements_for_output(&extra_damage, output_geo)
                .into_iter()
                .map(TtyRenderElements::Damage),
        );
        elements.extend(scene_elements);

        trace!(
            ?node,
            ?crtc,
            output = %output.name(),
            window_count,
            render_element_count = elements.len(),
            cursor_status = ?cursor_status_for_log,
            "rendering tty surface"
        );

        let result = surface.drm_output.render_frame(
            &mut backend.renderer,
            &elements,
            CLEAR_COLOR,
            TTY_FRAME_FLAGS,
        )?;
        let render_elapsed = render_started_at.elapsed();
        surface.estimated_render_duration =
            blend_render_duration(surface.estimated_render_duration, render_elapsed);

        if !result.is_empty {
            trace!(output = %output.name(), "queueing tty frame");
            // Update primary-scanout metadata before collecting presentation feedback.
            //
            // Chrome on the TTY backend would otherwise frequently stick to ~60 fps on a 66 Hz
            // output. Keeping this metadata current made Chrome observe the real output cadence.
            update_primary_scanout_output(&state.space, &output, &cursor_status_for_log, &result.states);
            let output_presentation_feedback =
                take_presentation_feedback(&output, &state.space, &result.states);
            surface
                .drm_output
                .queue_frame(Some(output_presentation_feedback))?;
            surface.frame_pending = true;
            trace!(
                ?node,
                ?crtc,
                output = %output.name(),
                frame_time_ms = frame_time.as_secs_f64() * 1000.0,
                frame_duration_ms = surface.frame_duration.as_secs_f64() * 1000.0,
                render_elapsed_ms = render_elapsed.as_secs_f64() * 1000.0,
                estimated_render_ms = surface.estimated_render_duration.as_secs_f64() * 1000.0,
                next_frame_target_ms = surface.next_frame_target.map(|tp| tp.as_secs_f64() * 1000.0),
                "queued tty frame"
            );
            all_windows.iter().for_each(|window| {
                window.send_frame(
                    &output,
                    frame_time,
                    Some(Duration::from_secs(1)),
                    |_, _| Some(output.clone()),
                );
            });
            {
                let map = layer_map_for_output(&output);
                for layer_surface in map.layers() {
                    layer_surface.send_frame(
                        &output,
                        frame_time,
                        Some(Duration::from_secs(1)),
                        |_, _| Some(output.clone()),
                    );
                }
            }
            state.signal_post_repaint_barriers(&output);
        } else {
            trace!(output = %output.name(), "tty frame had no damage");
        }

        captured_blink_damage
    };

    if let Some(damage) = captured_blink_damage.as_deref() {
        state.record_damage_blink(&output, damage);
    }

    Ok(RenderSurfaceOutcome::Processed)
}

fn transform_window_elements(
    elements: Vec<WaylandSurfaceRenderElement<GlesRenderer>>,
    visual: WindowVisualState,
    direct: fn(WaylandSurfaceRenderElement<GlesRenderer>) -> TtyRenderElements,
    transformed: fn(RelocateRenderElement<RescaleRenderElement<AlphaRenderElement<WaylandSurfaceRenderElement<GlesRenderer>>>>) -> TtyRenderElements,
) -> Vec<TtyRenderElements> {
    if is_identity_visual(visual) {
        return elements.into_iter().map(direct).collect();
    }

    elements
        .into_iter()
        .map(|element| {
            transformed(RelocateRenderElement::from_element(
                RescaleRenderElement::from_element(
                    AlphaRenderElement::from_element(element, visual.opacity),
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
) -> Vec<TtyRenderElements> {
    if is_identity_visual(visual) {
        return elements.into_iter().map(TtyRenderElements::Clipped).collect();
    }

    elements
        .into_iter()
        .map(|element| {
            TtyRenderElements::TransformedClipped(RelocateRenderElement::from_element(
                RescaleRenderElement::from_element(
                    AlphaRenderElement::from_element(element, visual.opacity),
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
    elements: Vec<MemoryRenderBufferRenderElement<GlesRenderer>>,
    visual: WindowVisualState,
) -> Result<Vec<TtyRenderElements>, Box<dyn std::error::Error>> {
    if is_identity_visual(visual) {
        return Ok(elements.into_iter().map(TtyRenderElements::Text).collect());
    }

    Ok(elements
        .into_iter()
        .map(|element| {
            TtyRenderElements::TransformedText(RelocateRenderElement::from_element(
                RescaleRenderElement::from_element(
                    AlphaRenderElement::from_element(element, visual.opacity),
                    visual.origin,
                    visual.scale,
                ),
                visual.translation,
                Relocate::Relative,
            ))
        })
        .collect())
}

fn transform_decoration_elements(
    elements: Vec<crate::backend::rounded::StableRoundedElement>,
    visual: WindowVisualState,
) -> Result<Vec<TtyRenderElements>, Box<dyn std::error::Error>> {
    if is_identity_visual(visual) {
        return Ok(elements.into_iter().map(TtyRenderElements::Decoration).collect());
    }

    Ok(elements
        .into_iter()
        .map(|element| {
            TtyRenderElements::TransformedDecoration(RelocateRenderElement::from_element(
                RescaleRenderElement::from_element(
                    AlphaRenderElement::from_element(element, visual.opacity),
                    visual.origin,
                    visual.scale,
                ),
                visual.translation,
                Relocate::Relative,
            ))
        })
        .collect())
}

fn is_identity_visual(visual: WindowVisualState) -> bool {
    visual.translation.x == 0
        && visual.translation.y == 0
        && (visual.scale.x - 1.0).abs() < f64::EPSILON
        && (visual.scale.y - 1.0).abs() < f64::EPSILON
        && (visual.opacity - 1.0).abs() < f32::EPSILON
}

fn connector_connected(
    state: &mut ShojiWM,
    node: DrmNode,
    crtc: crtc::Handle,
    connector: connector::Info,
) -> Result<(), Box<dyn std::error::Error>> {
    let mode = select_output_mode(&connector, &state.display_config.default_mode);
    let available_modes = connector
        .modes()
        .iter()
        .map(|candidate| {
            let wl_mode = WlMode::from(*candidate);
            format!(
                "{}x{}@{}(drm:{} wl:{})",
                candidate.size().0,
                candidate.size().1,
                candidate.name().to_string_lossy(),
                candidate.vrefresh(),
                wl_mode.refresh,
            )
        })
        .collect::<Vec<_>>();

    let output = Output::new(
        format!(
            "{}-{}",
            connector.interface().as_str(),
            connector.interface_id()
        ),
        PhysicalProperties {
            size: (0, 0).into(),
            subpixel: connector.subpixel().into(),
            make: "Unknown".into(),
            model: "Unknown".into(),
            serial_number: "Unknown".into(),
        },
    );
    let wl_mode = WlMode::from(mode);
    let frame_duration = Duration::from_secs_f64(1_000f64 / wl_mode.refresh as f64);
    output.set_preferred(wl_mode);
    output.change_current_state(Some(wl_mode), None, None, Some((0, 0).into()));
    output.create_global::<ShojiWM>(&state.display_handle);
    state.space.map_output(&output, (0, 0));
    info!(
        ?node,
        ?crtc,
        output = %output.name(),
        size = ?wl_mode.size,
        refresh_mhz = wl_mode.refresh,
        available_modes = ?available_modes,
        "connected tty output"
    );

    let backend = state.tty_backends.get_mut(&node).unwrap();

    let drm_output = backend
        .drm_output_manager
        .lock()
        .initialize_output::<_, WaylandSurfaceRenderElement<GlesRenderer>>(
            crtc,
            mode,
            &[connector.handle()],
            &output,
            None,
            &mut backend.renderer,
            &DrmOutputRenderElements::default(),
        )?;

    let surface = SurfaceData {
        output: output.clone(),
        drm_output,
        blink_damage_tracker: OutputDamageTracker::from_output(&output),
        frame_pending: false,
        repaint_scheduled: false,
        frame_duration,
        next_frame_target: None,
        estimated_render_duration: Duration::from_millis(4),
        last_presented_at: None,
        last_frame_callback_at: None,
    };
    backend.surfaces.insert(crtc, surface);
    debug!(?node, ?crtc, "stored tty surface");

    render_now(
        backend.surfaces.get_mut(&crtc).unwrap(),
        &mut backend.renderer,
    )?;
    Ok(())
}

fn select_output_mode(
    connector: &connector::Info,
    preference: &DisplayModePreference,
) -> smithay::reexports::drm::control::Mode {
    match preference {
        DisplayModePreference::Auto => connector
            .modes()
            .iter()
            .copied()
            .max_by_key(|mode| {
                let wl_mode = WlMode::from(*mode);
                (
                    i64::from(wl_mode.size.w) * i64::from(wl_mode.size.h),
                    mode.vrefresh(),
                    wl_mode.refresh,
                )
            })
            .unwrap_or(connector.modes()[0]),
        DisplayModePreference::Exact {
            width,
            height,
            refresh_mhz,
        } => {
            let exact = connector
                .modes()
                .iter()
                .copied()
                .filter(|mode| mode.size() == (*width, *height))
                .collect::<Vec<_>>();

            if exact.is_empty() {
                return select_output_mode(connector, &DisplayModePreference::Auto);
            }

            match refresh_mhz {
                Some(refresh_mhz) => exact
                    .into_iter()
                    .min_by_key(|mode| {
                        (i64::from(WlMode::from(*mode).refresh) - i64::from(*refresh_mhz)).abs()
                    })
                    .unwrap_or(connector.modes()[0]),
                None => exact
                    .into_iter()
                    .max_by_key(|mode| (mode.vrefresh(), WlMode::from(*mode).refresh))
                    .unwrap_or(connector.modes()[0]),
            }
        }
    }
}

fn render_now(
    surface: &mut SurfaceData,
    renderer: &mut GlesRenderer,
) -> Result<(), Box<dyn std::error::Error>> {
    let elements: Vec<crate::backend::rounded::StableRoundedElement> = Vec::new();

    debug!(output = %surface.output.name(), "rendering initial tty frame");
    let result =
        surface
            .drm_output
            .render_frame(renderer, &elements, CLEAR_COLOR, TTY_FRAME_FLAGS)?;

    if !result.is_empty {
        surface.drm_output.queue_frame(None)?;
        surface.frame_pending = true;
    }

    Ok(())
}

fn repaint_delay(surface: &SurfaceData) -> Option<Duration> {
    let refresh_mhz = surface.output.current_mode()?.refresh;
    if refresh_mhz <= 0 {
        return None;
    }

    let frame_duration = Duration::from_secs_f64(1_000f64 / refresh_mhz as f64);
    let min_budget = Duration::from_millis(2);
    let max_budget = Duration::from_millis(3);
    let compositor_budget = std::cmp::max(
        surface
            .estimated_render_duration
            .saturating_add(Duration::from_millis(1)),
        min_budget,
    );
    let compositor_budget = std::cmp::min(compositor_budget, max_budget);

    Some(frame_duration.saturating_sub(compositor_budget))
}

fn blend_render_duration(previous: Duration, current: Duration) -> Duration {
    if previous.is_zero() {
        return current;
    }

    Duration::from_secs_f64(previous.as_secs_f64() * 0.75 + current.as_secs_f64() * 0.25)
}
