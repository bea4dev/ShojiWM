use std::{collections::HashMap, path::Path, time::{Duration, Instant}};
use std::hash::{Hash, Hasher};

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
                Element,
                memory::MemoryRenderBuffer,
                solid::SolidColorRenderElement,
                surface::WaylandSurfaceRenderElement,
                texture::TextureRenderElement,
                utils::{Relocate, RelocateRenderElement, RescaleRenderElement},
                AsRenderElements,
            },
            gles::{GlesRenderer, GlesTexture},
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
        wayland_server::Resource,
        wayland_protocols::wp::presentation_time::server::wp_presentation_feedback,
    },
    render_elements,
    utils::{DeviceFd, IsAlive, Logical, Monotonic, Point, Rectangle, Scale, Transform},
    wayland::{
        background_effect::BackgroundEffectSurfaceCachedState,
        compositor,
        dmabuf::DmabufFeedbackBuilder,
    },
};
use smithay_drm_extras::drm_scanner::{DrmScanEvent, DrmScanner};
use tracing::{debug, info, trace, warn};

use crate::{
    backend::damage, backend::damage_blink, backend::decoration, backend::snapshot, backend::window as window_render,
    backend::visual::{WindowVisualState, window_visual_state},
    config::DisplayModePreference,
    presentation::{take_presentation_feedback, update_primary_scanout_output},
    drawing::PointerRenderElement, state::ShojiWM,
};
use smithay::wayland::presentation::Refresh;

const CLEAR_COLOR: [f32; 4] = [0.08, 0.10, 0.13, 1.0];
const TTY_FRAME_FLAGS: FrameFlags = FrameFlags::DEFAULT;

fn high_refresh_debug_enabled() -> bool {
    std::env::var_os("SHOJI_HIGH_REFRESH_DEBUG")
        .is_some_and(|value| value != "0" && !value.is_empty())
}

fn damage_profile_debug_enabled() -> bool {
    std::env::var_os("SHOJI_DAMAGE_PROFILE")
        .is_some_and(|value| value != "0" && !value.is_empty())
}

fn element_damage_debug_enabled() -> bool {
    std::env::var_os("SHOJI_ELEMENT_DAMAGE_DEBUG")
        .is_some_and(|value| value != "0" && !value.is_empty())
}

fn frame_callback_debug_enabled() -> bool {
    std::env::var_os("SHOJI_FRAME_CALLBACK_DEBUG")
        .is_some_and(|value| value != "0" && !value.is_empty())
}

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
    frame_callback_timer_armed: bool,
    frame_callback_timer_generation: u64,
    frame_callback_sequence: u32,
    redraw_state: TtyRedrawState,
    frame_duration: Duration,
    next_frame_target: Option<Duration>,
    estimated_render_duration: Duration,
    last_presented_at: Option<Duration>,
    last_frame_callback_at: Option<Duration>,
}

enum RenderSurfaceOutcome {
    Skipped,
    Processed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TtyRedrawState {
    Idle,
    Queued,
    WaitingForVBlank { redraw_needed: bool },
    WaitingForEstimatedVBlank { queued: bool, generation: u64 },
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
    _loop_handle: &LoopHandle<'_, ShojiWM>,
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

    if high_refresh_debug_enabled() {
        let repaint_budget = repaint_delay(surface);
        trace!(
            ?node,
            ?crtc,
            output = %surface.output.name(),
            presentation_clock_ms = presentation_clock.as_secs_f64() * 1000.0,
            frame_duration_ms = surface.frame_duration.as_secs_f64() * 1000.0,
            next_frame_target_ms = surface.next_frame_target.map(|tp| tp.as_secs_f64() * 1000.0),
            repaint_delay_ms = repaint_budget.map(|delay| delay.as_secs_f64() * 1000.0),
            estimated_render_ms = surface.estimated_render_duration.as_secs_f64() * 1000.0,
            "tty high refresh frame_finish"
        );
    }

    surface.frame_pending = false;
    let redraw_needed = match surface.redraw_state {
        TtyRedrawState::WaitingForVBlank { redraw_needed } => redraw_needed,
        _ => false,
    };
    surface.redraw_state = if redraw_needed {
        TtyRedrawState::Queued
    } else {
        TtyRedrawState::Idle
    };
    if redraw_needed {
        state.schedule_redraw();
    }
}

pub fn render_if_needed(
    state: &mut ShojiWM,
    loop_handle: &LoopHandle<'_, ShojiWM>,
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
    let mut processed_outputs: Vec<String> = Vec::new();

    queue_tty_redraws(state);

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
            match render_surface(state, loop_handle, node, crtc)? {
                RenderSurfaceOutcome::Skipped => {}
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

    if !processed_outputs.is_empty() {
        state.pending_decoration_damage.clear();
        state.clear_source_damage();
        state.finish_damage_blink_for_outputs(processed_outputs.iter().map(String::as_str));
    }

    Ok(())
}

render_elements! {
    pub TtyRenderElements<=GlesRenderer>;
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
    Backdrop=crate::backend::shader_effect::StableBackdropTextureElement,
    TransformedBackdrop=RelocateRenderElement<RescaleRenderElement<crate::backend::shader_effect::StableBackdropTextureElement>>,
    Cursor=PointerRenderElement<GlesRenderer>,
}

fn tty_render_element_kind_name(element: &TtyRenderElements) -> &'static str {
    match element {
        TtyRenderElements::Window(_) => "window",
        TtyRenderElements::TransformedWindow(_) => "transformed-window",
        TtyRenderElements::Clipped(_) => "clipped",
        TtyRenderElements::TransformedClipped(_) => "transformed-clipped",
        TtyRenderElements::Text(_) => "text",
        TtyRenderElements::TransformedText(_) => "transformed-text",
        TtyRenderElements::Snapshot(_) => "snapshot",
        TtyRenderElements::TransformedSnapshot(_) => "transformed-snapshot",
        TtyRenderElements::Damage(_) => "damage",
        TtyRenderElements::Blink(_) => "blink",
        TtyRenderElements::Decoration(_) => "decoration",
        TtyRenderElements::TransformedDecoration(_) => "transformed-decoration",
        TtyRenderElements::Backdrop(_) => "backdrop",
        TtyRenderElements::TransformedBackdrop(_) => "transformed-backdrop",
        TtyRenderElements::Cursor(_) => "cursor",
        TtyRenderElements::_GenericCatcher(_) => "generic",
    }
}

fn tty_render_element_signature(
    element: &TtyRenderElements,
    scale: Scale<f64>,
) -> String {
    let debug_label = match element {
        TtyRenderElements::Backdrop(element) => format!("|label={}", element.debug_label()),
        _ => String::new(),
    };
    format!(
        "{}|id={:?}|commit={:?}|geom={:?}{}",
        tty_render_element_kind_name(element),
        element.id(),
        element.current_commit(),
        element.geometry(scale),
        debug_label,
    )
}

fn queue_tty_redraws(state: &mut ShojiWM) {
    for backend in state.tty_backends.values_mut() {
        for surface in backend.surfaces.values_mut() {
            match &mut surface.redraw_state {
                TtyRedrawState::Idle => {
                    surface.redraw_state = TtyRedrawState::Queued;
                }
                TtyRedrawState::Queued => {}
                TtyRedrawState::WaitingForVBlank { redraw_needed } => {
                    *redraw_needed = true;
                }
                TtyRedrawState::WaitingForEstimatedVBlank { queued, .. } => {
                    *queued = true;
                }
            }
        }
    }
}

fn render_surface(
    state: &mut ShojiWM,
    loop_handle: &LoopHandle<'_, ShojiWM>,
    node: DrmNode,
    crtc: crtc::Handle,
) -> Result<RenderSurfaceOutcome, Box<dyn std::error::Error>> {
    let output = state
        .tty_backends
        .get(&node)
        .and_then(|backend| backend.surfaces.get(&crtc))
        .map(|surface| surface.output.clone())
        .unwrap();

    let redraw_state = state
        .tty_backends
        .get(&node)
        .and_then(|backend| backend.surfaces.get(&crtc))
        .map(|surface| surface.redraw_state)
        .unwrap_or(TtyRedrawState::Idle);

    if redraw_state != TtyRedrawState::Queued {
        if high_refresh_debug_enabled() {
            let surface = state
                .tty_backends
                .get(&node)
                .and_then(|backend| backend.surfaces.get(&crtc))
                .unwrap();
            trace!(
                ?node,
                ?crtc,
                output = %output.name(),
                next_frame_target_ms = surface.next_frame_target.map(|tp| tp.as_secs_f64() * 1000.0),
                frame_duration_ms = surface.frame_duration.as_secs_f64() * 1000.0,
                estimated_render_ms = surface.estimated_render_duration.as_secs_f64() * 1000.0,
                "tty high refresh skipped pending frame"
            );
        }
        return Ok(RenderSurfaceOutcome::Skipped);
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
    if state.force_full_damage {
        extra_damage.push(crate::ssd::LogicalRect::new(
            output_geo.loc.x,
            output_geo.loc.y,
            output_geo.size.w,
            output_geo.size.h,
        ));
    }
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
            window_decorations,
            windows_ready_for_decoration,
            live_window_snapshots,
            complete_window_snapshots,
            closing_window_snapshots,
            snapshot_dirty_window_ids,
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
        let windows_top_to_bottom: Vec<_> = windows.iter().rev().cloned().collect();
        let all_windows: Vec<_> = space.elements().cloned().collect();
        let window_count = all_windows.len();
        if frame_callback_debug_enabled() {
            let window_stack = windows_top_to_bottom
                .iter()
                .filter_map(|window| {
                    window_decorations.get(window).map(|decoration| {
                        format!(
                            "{}:{}:{:?}",
                            decoration.snapshot.id,
                            decoration.snapshot.title,
                            decoration.snapshot.app_id
                        )
                    })
                })
                .collect::<Vec<_>>();
            trace!(
                output = %output.name(),
                window_stack = ?window_stack,
                "tty render window stack snapshot"
            );
        }
        let closing_snapshots = closing_window_snapshots
            .values()
            .cloned()
            .collect::<Vec<_>>();
        let (_, _lower_layer_elements) =
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
        scene_elements.extend(upper_layer_scene_elements(
            &mut backend.renderer,
            space,
            window_decorations,
            &state.window_source_damage,
            &state.lower_layer_source_damage,
            state.configured_background_effect.as_ref(),
            &output,
            output_geo,
            scale,
            &windows_top_to_bottom,
            &mut state.layer_backdrop_cache,
        )?);

        for (_window_index, window) in windows_top_to_bottom.iter().enumerate() {
            let Some(window_location) = space.element_location(window) else {
                continue;
            };
            let Some(window_id) = window_decorations
                .get(window)
                .map(|decoration| decoration.snapshot.id.clone())
            else {
                continue;
            };
            if closing_window_snapshots.contains_key(&window_id) {
                continue;
            }
            let physical_location =
                (window_location - output_geo.loc).to_physical_precise_round(scale);
            let direct_surface_count = window_render::surface_elements(
                window,
                &mut backend.renderer,
                physical_location,
                scale,
                1.0,
            )
            .len();
            if direct_surface_count == 0 {
                continue;
            }
            let has_backdrop_source = direct_surface_count > 0
                || live_window_snapshots.contains_key(&window_id)
                || complete_window_snapshots.contains_key(&window_id);
            let decoration_ready = windows_ready_for_decoration.contains(&window_id);
            if !has_backdrop_source {
                continue;
            }
            let visual_state = window_decorations
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
            let mut ordered_ui_elements: Vec<(usize, TtyRenderElements)> = Vec::new();
            let mut ordered_backdrop_elements: Vec<(usize, TtyRenderElements)> = Vec::new();
            if decoration_ready {
                let mut backdrop_items = backdrop_shader_elements_for_window(
                    &mut backend.renderer,
                    space,
                    window_decorations,
                    &state.window_commit_times,
                    &state.window_source_damage,
                    &state.lower_layer_source_damage,
                    &output,
                    output_geo,
                    scale,
                    &windows_top_to_bottom,
                    _window_index,
                    window,
                    visual_state.opacity,
                    decoration_ready,
                );
                if let Some(effect_config) = state.configured_background_effect.as_ref() {
                    backdrop_items.extend(configured_background_effect_elements_for_window(
                        &mut backend.renderer,
                        space,
                        window_decorations,
                        &state.window_commit_times,
                        &state.window_source_damage,
                        &state.lower_layer_source_damage,
                        &output,
                        output_geo,
                        scale,
                        &windows_top_to_bottom,
                        _window_index,
                        window,
                        visual_state.opacity,
                        effect_config,
                    ));
                }
                for (order, element) in backdrop_items.drain(..) {
                    ordered_backdrop_elements.extend(
                        transform_backdrop_elements(vec![element], visual_state)?
                            .into_iter()
                            .map(|item| (order, item)),
                    );
                }
                if let Some(decoration_state) = window_decorations.get_mut(window) {
                    let mut ordered_background_items = decoration::ordered_background_elements_for_window(
                        &mut backend.renderer,
                        decoration_state,
                        output_geo,
                        scale,
                        visual_state.opacity,
                    )
                    .inspect_err(|error| {
                        warn!(?error, "failed to build decoration background elements");
                    })
                    .unwrap_or_default();
                    ordered_background_items.sort_by_key(|(order, _)| *order);
                    for (order, element) in ordered_background_items {
                        ordered_ui_elements.extend(
                            transform_decoration_elements(vec![element], visual_state)?
                                .into_iter()
                                .map(|item| (order, item)),
                        );
                    }
                }

                for (order, element) in decoration::ordered_icon_elements_for_window(
                    &mut backend.renderer,
                    space,
                    window_decorations,
                    &output,
                    window,
                    visual_state.opacity,
                )? {
                    ordered_ui_elements.extend(
                        transform_text_elements(vec![element], visual_state)?
                            .into_iter()
                            .map(|item| (order, item)),
                    );
                }

                for (order, element) in decoration::ordered_text_elements_for_window(
                    &mut backend.renderer,
                    space,
                    window_decorations,
                    &output,
                    window,
                    visual_state.opacity,
                )? {
                    ordered_ui_elements.extend(
                        transform_text_elements(vec![element], visual_state)?
                            .into_iter()
                            .map(|item| (order, item)),
                    );
                }

                ordered_ui_elements.sort_by_key(|(order, _)| *order);
                ordered_backdrop_elements.sort_by_key(|(order, _)| *order);
            }

            let content_clip = window_decorations
                .get(window)
                .and_then(|decoration| decoration.content_clip);

            let client_elements = if let Some(content_clip) = content_clip {
                let clipped = window_render::clipped_surface_elements(
                    window,
                    &mut backend.renderer,
                    physical_location,
                    scale,
                    visual_state.opacity,
                    Some(content_clip),
                )
                .inspect_err(|error| {
                    warn!(?error, "failed to build clipped surface elements");
                })
                .unwrap_or_default();
                transform_clipped_elements(clipped, visual_state)
            } else {
                let surfaces = window_render::surface_elements(
                    window,
                    &mut backend.renderer,
                    physical_location,
                    scale,
                    visual_state.opacity,
                );
                transform_window_elements(
                    surfaces,
                    visual_state,
                    TtyRenderElements::Window,
                    TtyRenderElements::TransformedWindow,
                )
            };

            let popup_elements = transform_window_elements(
                window_render::popup_elements(
                    window,
                    &mut backend.renderer,
                    physical_location,
                    scale,
                    visual_state.opacity,
                ),
                visual_state,
                TtyRenderElements::Window,
                TtyRenderElements::TransformedWindow,
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

            windows_ready_for_decoration.insert(window_id.clone());

            let should_refresh_snapshot = window_decorations
                .get(window)
                .map(|decoration| {
                    snapshot_dirty_window_ids.contains(&decoration.snapshot.id)
                        || live_window_snapshots
                            .get(&decoration.snapshot.id)
                            .map(|snapshot| snapshot.rect != decoration.client_rect)
                            .unwrap_or(true)
                })
                .unwrap_or(false);
            if should_refresh_snapshot {
                if capture_live_snapshot_for_window(
                    &mut backend.renderer,
                    window,
                    window_location,
                    scale,
                    0,
                    window_decorations,
                    live_window_snapshots,
                    complete_window_snapshots,
                )
                .is_ok()
                {
                    if let Some(window_id) = window_decorations
                        .get(window)
                        .map(|decoration| decoration.snapshot.id.clone())
                    {
                        snapshot_dirty_window_ids.remove(&window_id);
                    }
                }
            }
        }
        scene_elements.extend(
            closing_snapshot_elements(&mut backend.renderer, &closing_snapshots, output_geo, scale)
                .into_iter(),
        );
        scene_elements.extend(lower_layer_scene_elements(
            &mut backend.renderer,
            &output,
            output_geo,
            scale,
            state.configured_background_effect.as_ref(),
            &state.lower_layer_source_damage,
            &mut state.layer_backdrop_cache,
        )?);

        if element_damage_debug_enabled() {
            let output_key = format!("tty:{}", output.name());
            let signatures = scene_elements
                .iter()
                .map(|element| tty_render_element_signature(element, scale))
                .collect::<Vec<_>>();
            let previous = state
                .debug_previous_scene_signatures
                .insert(output_key, signatures.clone())
                .unwrap_or_default();
            let previous_set = previous.iter().cloned().collect::<std::collections::HashSet<_>>();
            let current_set = signatures
                .iter()
                .cloned()
                .collect::<std::collections::HashSet<_>>();
            let added = current_set
                .difference(&previous_set)
                .take(12)
                .cloned()
                .collect::<Vec<_>>();
            let removed = previous_set
                .difference(&current_set)
                .take(12)
                .cloned()
                .collect::<Vec<_>>();
            trace!(
                output = %output.name(),
                previous_count = previous.len(),
                current_count = signatures.len(),
                added_count = current_set.difference(&previous_set).count(),
                removed_count = previous_set.difference(&current_set).count(),
                added = ?added,
                removed = ?removed,
                "tty scene element damage audit"
            );
        }

        let should_profile_damage = should_capture_blink || damage_profile_debug_enabled();
        let computed_damage = if should_profile_damage {
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

        if damage_profile_debug_enabled() {
            let rect_count = computed_damage.as_ref().map(|damage| damage.len()).unwrap_or(0);
            let damage_area = computed_damage
                .as_ref()
                .map(|damage| {
                    damage
                        .iter()
                        .map(|rect| i64::from(rect.size.w.max(0)) * i64::from(rect.size.h.max(0)))
                        .sum::<i64>()
                })
                .unwrap_or(0);
            let output_area = i64::from(output_geo.size.w.max(0)) * i64::from(output_geo.size.h.max(0));
            trace!(
                ?node,
                ?crtc,
                output = %output.name(),
                window_count,
                scene_element_count = scene_elements.len(),
                extra_damage_rect_count = extra_damage.len(),
                computed_damage_rect_count = rect_count,
                computed_damage_area = damage_area,
                output_area,
                damage_to_output_ratio = if output_area > 0 { damage_area as f64 / output_area as f64 } else { 0.0 },
                "tty damage profile"
            );
        }

        let captured_blink_damage = if should_capture_blink {
            computed_damage
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
            surface.frame_callback_timer_armed = false;
            surface.frame_callback_timer_generation =
                surface.frame_callback_timer_generation.wrapping_add(1);
            surface.frame_callback_sequence = surface.frame_callback_sequence.wrapping_add(1);
            surface.redraw_state = TtyRedrawState::WaitingForVBlank {
                redraw_needed: false,
            };
            let frame_callback_sequence = surface.frame_callback_sequence;
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
            if high_refresh_debug_enabled() {
                let frame_budget = surface.frame_duration.as_secs_f64() * 1000.0;
                let render_ms = render_elapsed.as_secs_f64() * 1000.0;
                trace!(
                    ?node,
                    ?crtc,
                    output = %output.name(),
                    frame_budget_ms = frame_budget,
                    render_elapsed_ms = render_ms,
                    estimated_render_ms = surface.estimated_render_duration.as_secs_f64() * 1000.0,
                    budget_utilization = if frame_budget > 0.0 { render_ms / frame_budget } else { 0.0 },
                    element_count = elements.len(),
                    "tty high refresh queued frame stats"
                );
            }
            let _ = surface;
            let _ = backend;
            state.post_repaint_with_sequence(
                &output,
                frame_time,
                &result.states,
                Some(frame_callback_sequence),
            );
        } else {
            trace!(output = %output.name(), "tty frame had no damage");
            if high_refresh_debug_enabled() {
                let frame_budget = surface.frame_duration.as_secs_f64() * 1000.0;
                let render_ms = render_elapsed.as_secs_f64() * 1000.0;
                trace!(
                    ?node,
                    ?crtc,
                    output = %output.name(),
                    frame_budget_ms = frame_budget,
                    render_elapsed_ms = render_ms,
                    estimated_render_ms = surface.estimated_render_duration.as_secs_f64() * 1000.0,
                    budget_utilization = if frame_budget > 0.0 { render_ms / frame_budget } else { 0.0 },
                    element_count = elements.len(),
                    "tty high refresh no-damage stats"
                );
            }
            let generation = surface.frame_callback_timer_generation.wrapping_add(1);
            surface.frame_callback_timer_generation = generation;
            surface.redraw_state = TtyRedrawState::WaitingForEstimatedVBlank {
                queued: false,
                generation,
            };
            schedule_estimated_vblank_callback(loop_handle, state, node, crtc, frame_time);
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
    transformed: fn(RelocateRenderElement<RescaleRenderElement<WaylandSurfaceRenderElement<GlesRenderer>>>) -> TtyRenderElements,
) -> Vec<TtyRenderElements> {
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
) -> Vec<TtyRenderElements> {
    if is_identity_visual(visual) {
        return elements.into_iter().map(TtyRenderElements::Clipped).collect();
    }

    elements
        .into_iter()
        .map(|element| {
            TtyRenderElements::TransformedClipped(RelocateRenderElement::from_element(
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
) -> Result<Vec<TtyRenderElements>, Box<dyn std::error::Error>> {
    if is_identity_visual(visual) {
        return Ok(elements.into_iter().map(TtyRenderElements::Text).collect());
    }

    Ok(elements
        .into_iter()
        .map(|element| {
            TtyRenderElements::TransformedText(RelocateRenderElement::from_element(
                RescaleRenderElement::from_element(
                    element,
                    visual.origin,
                    visual.scale,
                ),
                visual.translation,
                Relocate::Relative,
            ))
        })
        .collect())
}

fn transform_snapshot_elements(
    elements: Vec<TextureRenderElement<GlesTexture>>,
    visual: WindowVisualState,
) -> Result<Vec<TtyRenderElements>, Box<dyn std::error::Error>> {
    if is_identity_visual(visual) {
        return Ok(elements.into_iter().map(TtyRenderElements::Snapshot).collect());
    }

    Ok(elements
        .into_iter()
        .map(|element| {
            TtyRenderElements::TransformedSnapshot(RelocateRenderElement::from_element(
                RescaleRenderElement::from_element(
                    element,
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
    elements: Vec<crate::backend::decoration::DecorationSceneElements>,
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
                    element,
                    visual.origin,
                    visual.scale,
                ),
                visual.translation,
                Relocate::Relative,
            ))
        })
        .collect())
}

fn transform_backdrop_elements(
    elements: Vec<crate::backend::shader_effect::StableBackdropTextureElement>,
    _visual: WindowVisualState,
) -> Result<Vec<TtyRenderElements>, Box<dyn std::error::Error>> {
    Ok(elements.into_iter().map(TtyRenderElements::Backdrop).collect())
}

fn backdrop_shader_elements_for_window(
    renderer: &mut GlesRenderer,
    space: &smithay::desktop::Space<smithay::desktop::Window>,
    window_decorations: &mut std::collections::HashMap<smithay::desktop::Window, crate::ssd::WindowDecorationState>,
    _window_commit_times: &std::collections::HashMap<smithay::desktop::Window, std::time::Duration>,
    window_source_damage: &[crate::state::OwnedDamageRect],
    lower_layer_source_damage: &[crate::state::OwnedDamageRect],
    output: &Output,
    output_geo: smithay::utils::Rectangle<i32, Logical>,
    scale: smithay::utils::Scale<f64>,
    windows_top_to_bottom: &[smithay::desktop::Window],
    window_index: usize,
    window: &smithay::desktop::Window,
    alpha: f32,
    has_backdrop_source: bool,
) -> Vec<(usize, crate::backend::shader_effect::StableBackdropTextureElement)> {
    if !has_backdrop_source {
        return Vec::new();
    }
    let Some(decoration) = window_decorations.get(window).cloned() else {
        return Vec::new();
    };
    let lower_windows = windows_top_to_bottom
        .iter()
        .skip(window_index + 1)
        .cloned()
        .collect::<Vec<_>>();
    let (_, lower_layers) = window_render::layer_surfaces_for_output(output);
    let relevant_source_damage = {
        let mut entries = collect_window_source_damage(
            window_decorations,
            lower_windows.iter().cloned(),
            window_source_damage,
        );
        entries.extend(collect_layer_source_damage(
            lower_layers.iter().cloned(),
            lower_layer_source_damage,
        ));
        entries
    };

    decoration
        .shader_buffers
        .clone()
        .iter()
        .filter(|cached| cached.shader.is_backdrop())
        .filter_map(|cached| {
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            cached.stable_key.hash(&mut hasher);
            let effect_rect = crate::backend::visual::transformed_rect(
                cached.rect,
                decoration.layout.root.rect,
                decoration.visual_transform,
            );
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
            format!("{:?}", cached.shader).hash(&mut hasher);
            let capture_geo = smithay::utils::Rectangle::new(
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
            for lower_window in &lower_windows {
                if let Some(lower_decoration) = window_decorations.get(lower_window) {
                    lower_decoration.snapshot.id.hash(&mut hasher);
                    lower_decoration.visual_transform.translate_x.to_bits().hash(&mut hasher);
                    lower_decoration.visual_transform.translate_y.to_bits().hash(&mut hasher);
                    lower_decoration.visual_transform.scale_x.to_bits().hash(&mut hasher);
                    lower_decoration.visual_transform.scale_y.to_bits().hash(&mut hasher);
                    lower_decoration.visual_transform.opacity.to_bits().hash(&mut hasher);
                }
            }
            let signature = hasher.finish();
            let source_damage_hit = crate::backend::shader_effect::source_damage_intersects_rect(
                &cached.shader,
                smithay::utils::Rectangle::new(
                    smithay::utils::Point::from((effect_rect.x, effect_rect.y)),
                    (effect_rect.width, effect_rect.height).into(),
                ),
                &relevant_source_damage,
            );

            if !matches!(
                cached.shader.invalidate_policy(),
                crate::ssd::EffectInvalidationPolicy::Always
            ) && !source_damage_hit
            {
                if let Some(existing) = window_decorations
                    .get(window)
                    .and_then(|d| d.backdrop_cache.get(&cached.stable_key))
                    .filter(|existing| existing.signature == signature)
                    .cloned()
                {
                    let local_rect = smithay::utils::Rectangle::new(
                        smithay::utils::Point::from((
                            effect_rect.x - output_geo.loc.x,
                            effect_rect.y - output_geo.loc.y,
                        )),
                        (effect_rect.width, effect_rect.height).into(),
                    );
                    let clip_rect = cached.clip_rect.map(|clip_rect| {
                        let transformed_clip = crate::backend::visual::transformed_rect(
                            clip_rect,
                            decoration.layout.root.rect,
                            decoration.visual_transform,
                        );
                        smithay::utils::Rectangle::new(
                            smithay::utils::Point::from((
                                transformed_clip.x - output_geo.loc.x,
                                transformed_clip.y - output_geo.loc.y,
                            )),
                            (transformed_clip.width, transformed_clip.height).into(),
                        )
                    });
                    let local_sample_rect = local_rect;
                    let local_capture_rect = local_rect;
                    return crate::backend::shader_effect::backdrop_shader_element(
                        renderer,
                        existing.id.clone(),
                        existing.commit_counter,
                        existing.texture,
                        local_rect,
                        local_sample_rect,
                        local_capture_rect,
                        &cached.shader,
                        alpha,
                        scale.x as f32,
                        clip_rect,
                        cached.clip_radius,
                        format!("window-backdrop:{}:{}", decoration.snapshot.id, cached.stable_key),
                    )
                    .ok()
                    .map(|element| (cached.order, element));
                }
            }
            let mut backdrop_scene: Vec<TtyRenderElements> = Vec::new();
            for lower_window in &lower_windows {
                if let Ok(mut elements) = window_scene_elements_for_capture(
                    renderer,
                    space,
                    window_decorations,
                    capture_geo,
                    scale,
                    lower_window,
                ) {
                    backdrop_scene.append(&mut elements);
                }
            }
            let (_, lower_layer_elements) =
                window_render::layer_elements_for_output(renderer, output, scale, 1.0);
            let capture_offset = capture_geo.loc - output_geo.loc;
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
                    TtyRenderElements::Window,
                    TtyRenderElements::TransformedWindow,
                )
                .into_iter(),
            );
            if backdrop_scene.is_empty() {
                return None;
            }
            let snapshot = crate::backend::snapshot::capture_snapshot(
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
            let texture = crate::backend::shader_effect::apply_effect_pipeline(
                renderer,
                snapshot.texture,
                (capture_geo.size.w, capture_geo.size.h),
                Some(Rectangle::new(
                    Point::from((
                        effect_rect.x - capture_geo.loc.x,
                        effect_rect.y - capture_geo.loc.y,
                    )),
                    (effect_rect.width, effect_rect.height).into(),
                )),
                Some((effect_rect.width, effect_rect.height)),
                &cached.shader,
            )
            .ok()?;
            let commit_counter = window_decorations
                .get(window)
                .and_then(|d| d.backdrop_cache.get(&cached.stable_key))
                .map(|existing| {
                    let mut counter = existing.commit_counter;
                    counter.increment();
                    counter
                })
                .unwrap_or_default();
            if let Some(window_decoration) = window_decorations.get_mut(window) {
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
            let local_rect = smithay::utils::Rectangle::new(
                smithay::utils::Point::from((
                    effect_rect.x - output_geo.loc.x,
                    effect_rect.y - output_geo.loc.y,
                )),
                (effect_rect.width, effect_rect.height).into(),
            );
            let clip_rect = cached.clip_rect.map(|clip_rect| {
                let transformed_clip = crate::backend::visual::transformed_rect(
                    clip_rect,
                    decoration.layout.root.rect,
                    decoration.visual_transform,
                );
                smithay::utils::Rectangle::new(
                    smithay::utils::Point::from((
                        transformed_clip.x - output_geo.loc.x,
                        transformed_clip.y - output_geo.loc.y,
                    )),
                    (transformed_clip.width, transformed_clip.height).into(),
                )
            });
            let local_sample_rect = local_rect;
            let local_capture_rect = local_rect;
            crate::backend::shader_effect::backdrop_shader_element(
                renderer,
                window_decorations
                    .get(window)
                    .and_then(|d| d.backdrop_cache.get(&cached.stable_key))
                    .map(|cached| cached.id.clone())
                    .unwrap_or_else(smithay::backend::renderer::element::Id::new),
                window_decorations
                    .get(window)
                    .and_then(|d| d.backdrop_cache.get(&cached.stable_key))
                    .map(|cached| cached.commit_counter)
                    .unwrap_or_default(),
                texture,
                local_rect,
                local_sample_rect,
                local_capture_rect,
                &cached.shader,
                alpha,
                scale.x as f32,
                clip_rect,
                cached.clip_radius,
                format!("window-backdrop:{}:{}", decoration.snapshot.id, cached.stable_key),
            )
            .ok()
            .map(|element| (cached.order, element))
        })
        .collect()
}

fn protocol_background_effect_rects_for_window(
    window: &smithay::desktop::Window,
    decoration: &crate::ssd::WindowDecorationState,
) -> Vec<crate::ssd::LogicalRect> {
    let smithay::desktop::WindowSurface::Wayland(surface) = window.underlying_surface() else {
        return Vec::new();
    };
    let wl_surface = surface.wl_surface();
    let blur_region = compositor::with_states(wl_surface, |states| {
        let mut cached = states.cached_state.get::<BackgroundEffectSurfaceCachedState>();
        cached.current().blur_region.clone()
    });
    let Some(region) = blur_region else {
        return Vec::new();
    };

    crate::backend::window::region_rects_within_bounds(
        &region,
        crate::ssd::LogicalRect::new(0, 0, decoration.client_rect.width, decoration.client_rect.height),
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
        let mut cached = states.cached_state.get::<BackgroundEffectSurfaceCachedState>();
        cached.current().blur_region.clone()
    });
    let Some(region) = blur_region else {
        return Vec::new();
    };
    let map = layer_map_for_output(output);
    let Some(layer_geo) = map.layer_geometry(layer_surface) else {
        return Vec::new();
    };
    drop(map);

    crate::backend::window::region_rects_within_bounds(
        &region,
        crate::ssd::LogicalRect::new(0, 0, layer_geo.size.w, layer_geo.size.h),
    )
    .into_iter()
    .map(|rect| {
        crate::ssd::LogicalRect::new(
            layer_geo.loc.x + rect.x,
            layer_geo.loc.y + rect.y,
            rect.width,
            rect.height,
        )
    })
    .collect()
}

fn collect_window_source_damage(
    window_decorations: &std::collections::HashMap<smithay::desktop::Window, crate::ssd::WindowDecorationState>,
    windows: impl IntoIterator<Item = smithay::desktop::Window>,
    source_damage: &[crate::state::OwnedDamageRect],
) -> Vec<crate::state::OwnedDamageRect> {
    let owners = windows
        .into_iter()
        .filter_map(|window| window_decorations.get(&window).map(|decoration| decoration.snapshot.id.clone()))
        .collect::<std::collections::HashSet<_>>();
    source_damage
        .iter()
        .filter(|entry| owners.contains(&entry.owner))
        .cloned()
        .collect()
}

fn collect_layer_source_damage(
    layers: impl IntoIterator<Item = smithay::desktop::LayerSurface>,
    source_damage: &[crate::state::OwnedDamageRect],
) -> Vec<crate::state::OwnedDamageRect> {
    let owners = layers
        .into_iter()
        .map(|layer| layer.wl_surface().id().protocol_id().to_string())
        .collect::<std::collections::HashSet<_>>();
    source_damage
        .iter()
        .filter(|entry| owners.contains(&entry.owner))
        .cloned()
        .collect()
}

fn layer_surface_scene_elements_for_capture(
    renderer: &mut GlesRenderer,
    output: &Output,
    capture_geo: smithay::utils::Rectangle<i32, Logical>,
    scale: smithay::utils::Scale<f64>,
    layer_surface: &smithay::desktop::LayerSurface,
) -> Result<Vec<TtyRenderElements>, Box<dyn std::error::Error>> {
    let capture_visual = WindowVisualState {
        origin: smithay::utils::Point::from((0, 0)),
        scale: smithay::utils::Scale::from((1.0, 1.0)),
        translation: smithay::utils::Point::from((-capture_geo.loc.x, -capture_geo.loc.y))
            .to_f64()
            .to_physical_precise_round(scale),
        opacity: 1.0,
    };
    Ok(transform_window_elements(
        window_render::layer_surface_elements(renderer, output, layer_surface, scale, 1.0),
        capture_visual,
        TtyRenderElements::Window,
        TtyRenderElements::TransformedWindow,
    ))
}

fn configured_background_effect_elements_for_layer(
    renderer: &mut GlesRenderer,
    space: &smithay::desktop::Space<smithay::desktop::Window>,
    window_decorations: &mut std::collections::HashMap<smithay::desktop::Window, crate::ssd::WindowDecorationState>,
    window_source_damage: &[crate::state::OwnedDamageRect],
    lower_layer_source_damage: &[crate::state::OwnedDamageRect],
    output: &Output,
    output_geo: smithay::utils::Rectangle<i32, Logical>,
    scale: smithay::utils::Scale<f64>,
    windows_top_to_bottom: &[smithay::desktop::Window],
    layer_surface: &smithay::desktop::LayerSurface,
    alpha: f32,
    effect_config: &crate::ssd::BackgroundEffectConfig,
    layer_backdrop_cache: &mut std::collections::HashMap<String, crate::backend::shader_effect::CachedBackdropTexture>,
) -> Result<Vec<TtyRenderElements>, Box<dyn std::error::Error>> {
    let rects = protocol_background_effect_rects_for_layer(output, layer_surface);
    if rects.is_empty() {
        return Ok(Vec::new());
    }

    let Some(effect_rect) = crate::backend::window::bounding_box_for_rects(&rects) else {
        return Ok(Vec::new());
    };
    let blur_padding = effect_config
        .effect
        .blur_stage()
        .map(|blur| {
            let radius = blur.radius.max(1);
            let passes = blur.passes.max(1);
            (radius * passes * 24 + 32).max(32)
        })
        .unwrap_or(0);
    let capture_geo = smithay::utils::Rectangle::new(
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
    let (_, lower_layers) = window_render::layer_surfaces_for_output(output);
    let relevant_source_damage = {
        let mut entries = collect_window_source_damage(
            window_decorations,
            windows_top_to_bottom.iter().cloned(),
            window_source_damage,
        );
        entries.extend(collect_layer_source_damage(
            lower_layers.iter().cloned(),
            lower_layer_source_damage,
        ));
        entries
    };

    let mut backdrop_scene: Vec<TtyRenderElements> = Vec::new();
    for lower_window in windows_top_to_bottom {
        if let Ok(mut window_elements) = window_scene_elements_for_capture(
            renderer,
            space,
            window_decorations,
            capture_geo,
            scale,
            lower_window,
        ) {
            backdrop_scene.append(&mut window_elements);
        }
    }
    for lower_layer in &lower_layers {
        if let Ok(mut layer_elements) = layer_surface_scene_elements_for_capture(
            renderer,
            output,
            capture_geo,
            scale,
            lower_layer,
        ) {
            backdrop_scene.append(&mut layer_elements);
        }
    }
    if backdrop_scene.is_empty() {
        return Ok(Vec::new());
    }
    let snapshot = crate::backend::snapshot::capture_snapshot(
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
    )?
    .ok_or("missing backdrop snapshot")?;
    let texture = crate::backend::shader_effect::apply_effect_pipeline(
        renderer,
        snapshot.texture,
        (capture_geo.size.w, capture_geo.size.h),
        Some(Rectangle::new(
            Point::from((
                effect_rect.x - capture_geo.loc.x,
                effect_rect.y - capture_geo.loc.y,
            )),
            (effect_rect.width, effect_rect.height).into(),
        )),
        Some((effect_rect.width, effect_rect.height)),
        &effect_config.effect,
    )?;
    let _captured_local_rect: smithay::utils::Rectangle<i32, smithay::utils::Logical> = smithay::utils::Rectangle::new(
        smithay::utils::Point::from((
            effect_rect.x - output_geo.loc.x,
            effect_rect.y - output_geo.loc.y,
        )),
        (effect_rect.width, effect_rect.height).into(),
    );

    let mut elements = Vec::new();
    let stable_key = format!(
        "__layer_background_effect_{}_{}_top_{}x{}",
        output.name(),
        layer_surface.wl_surface().id().protocol_id(),
        effect_rect.width,
        effect_rect.height
    );
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    stable_key.hash(&mut hasher);
    for lower_window in windows_top_to_bottom {
        if let Some(lower_decoration) = window_decorations.get(lower_window) {
            lower_decoration.snapshot.id.hash(&mut hasher);
            lower_decoration.visual_transform.translate_x.to_bits().hash(&mut hasher);
            lower_decoration.visual_transform.translate_y.to_bits().hash(&mut hasher);
            lower_decoration.visual_transform.scale_x.to_bits().hash(&mut hasher);
            lower_decoration.visual_transform.scale_y.to_bits().hash(&mut hasher);
            lower_decoration.visual_transform.opacity.to_bits().hash(&mut hasher);
        }
    }
    format!("{:?}", effect_config.effect).hash(&mut hasher);
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
        &effect_config.effect,
        smithay::utils::Rectangle::new(
            smithay::utils::Point::from((effect_rect.x, effect_rect.y)),
            (effect_rect.width, effect_rect.height).into(),
        ),
        &relevant_source_damage,
    );
    let captured_local_rect = smithay::utils::Rectangle::new(
        smithay::utils::Point::from((
            effect_rect.x - output_geo.loc.x,
            effect_rect.y - output_geo.loc.y,
        )),
        (effect_rect.width, effect_rect.height).into(),
    );
    if !matches!(
        effect_config.effect.invalidate_policy(),
        crate::ssd::EffectInvalidationPolicy::Always
    ) && !source_damage_hit
    {
        if let Some(existing) = layer_backdrop_cache
            .get(&stable_key)
            .filter(|existing| existing.signature == signature)
            .cloned()
        {
            for rect in rects {
            let rect_key = format!("{}:{}:{}:{}:{}", stable_key, rect.x, rect.y, rect.width, rect.height);
            let rect_local = smithay::utils::Rectangle::new(
                smithay::utils::Point::from((
                    rect.x - output_geo.loc.x,
                    rect.y - output_geo.loc.y,
                )),
                (rect.width, rect.height).into(),
            );
            elements.push(TtyRenderElements::Backdrop(
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
                    &effect_config.effect,
                    alpha,
                    scale.x as f32,
                    None,
                    0,
                    format!("layer-top:{}:{}", output.name(), rect_key),
                )?,
            ));
        }
            return Ok(elements);
        }
    }
    let mut sub_elements = layer_backdrop_cache
        .get(&stable_key)
        .map(|existing| existing.sub_elements.clone())
        .unwrap_or_default();
    let had_existing = layer_backdrop_cache.contains_key(&stable_key);
    for rect in &rects {
        let rect_key = format!("{}:{}:{}:{}:{}", stable_key, rect.x, rect.y, rect.width, rect.height);
        let entry = sub_elements.entry(rect_key).or_default();
        if had_existing {
            entry.commit_counter.increment();
        }
    }
    layer_backdrop_cache.insert(
        stable_key.clone(),
        crate::backend::shader_effect::CachedBackdropTexture {
            signature,
            texture: texture.clone(),
            id: layer_backdrop_cache
                .get(&stable_key)
                .map(|cached| cached.id.clone())
                .unwrap_or_else(smithay::backend::renderer::element::Id::new),
            commit_counter: layer_backdrop_cache
                .get(&stable_key)
                .map(|existing| {
                    let mut counter = existing.commit_counter;
                    counter.increment();
                    counter
                })
                .unwrap_or_default(),
            sub_elements,
        },
    );
    for rect in rects {
        let rect_key = format!("{}:{}:{}:{}:{}", stable_key, rect.x, rect.y, rect.width, rect.height);
        let rect_local = smithay::utils::Rectangle::new(
            smithay::utils::Point::from((
                rect.x - output_geo.loc.x,
                rect.y - output_geo.loc.y,
            )),
            (rect.width, rect.height).into(),
        );
        elements.push(TtyRenderElements::Backdrop(
            crate::backend::shader_effect::backdrop_shader_element(
                renderer,
                layer_backdrop_cache
                    .get(&stable_key)
                    .and_then(|cached| cached.sub_elements.get(&rect_key))
                    .map(|entry| entry.id.clone())
                    .unwrap_or_else(smithay::backend::renderer::element::Id::new),
                layer_backdrop_cache
                    .get(&stable_key)
                    .and_then(|cached| cached.sub_elements.get(&rect_key))
                    .map(|entry| entry.commit_counter)
                    .unwrap_or_default(),
                texture.clone(),
                rect_local,
                rect_local,
                captured_local_rect,
                &effect_config.effect,
                alpha,
                scale.x as f32,
                None,
                0,
                format!("layer-top:{}:{}", output.name(), rect_key),
            )?,
        ));
    }
    Ok(elements)
}

fn lower_layer_scene_elements(
    renderer: &mut GlesRenderer,
    output: &Output,
    output_geo: smithay::utils::Rectangle<i32, Logical>,
    scale: smithay::utils::Scale<f64>,
    effect_config: Option<&crate::ssd::BackgroundEffectConfig>,
    lower_layer_source_damage: &[crate::state::OwnedDamageRect],
    layer_backdrop_cache: &mut std::collections::HashMap<String, crate::backend::shader_effect::CachedBackdropTexture>,
) -> Result<Vec<TtyRenderElements>, Box<dyn std::error::Error>> {
    let (_, lower_layers) = window_render::layer_surfaces_for_output(output);
    let mut elements = Vec::new();
    for (index, layer_surface) in lower_layers.iter().enumerate() {
        let layer_id = layer_surface.wl_surface().id().protocol_id();
        elements.extend(
            window_render::layer_surface_elements(renderer, output, layer_surface, scale, 1.0)
                .into_iter()
                .map(TtyRenderElements::Window),
        );
        let Some(effect_config) = effect_config else {
            continue;
        };
        let rects = protocol_background_effect_rects_for_layer(output, layer_surface);
        let Some(effect_rect) = crate::backend::window::bounding_box_for_rects(&rects) else {
            continue;
        };
        {
            let stable_key = format!(
                "__layer_background_effect_{}_{}_{}_{}x{}",
                output.name(),
                layer_id,
                index,
                effect_rect.width,
                effect_rect.height
            );
            let blur_padding = effect_config
                .effect
                .blur_stage()
                .map(|blur| {
                    let radius = blur.radius.max(1);
                    let passes = blur.passes.max(1);
                    (radius * passes * 24 + 32).max(32)
                })
                .unwrap_or(0);
            let capture_geo = smithay::utils::Rectangle::new(
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
            format!("{:?}", effect_config.effect).hash(&mut hasher);
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
            let relevant_source_damage = collect_layer_source_damage(
                lower_layers.iter().skip(index + 1).cloned(),
                lower_layer_source_damage,
            );
            let source_damage_hit = crate::backend::shader_effect::source_damage_intersects_rect(
                &effect_config.effect,
                smithay::utils::Rectangle::new(
                    smithay::utils::Point::from((effect_rect.x, effect_rect.y)),
                    (effect_rect.width, effect_rect.height).into(),
                ),
                &relevant_source_damage,
            );
            let captured_local_rect = smithay::utils::Rectangle::new(
                smithay::utils::Point::from((
                    effect_rect.x - output_geo.loc.x,
                    effect_rect.y - output_geo.loc.y,
                )),
                (effect_rect.width, effect_rect.height).into(),
            );
            if !matches!(
                effect_config.effect.invalidate_policy(),
                crate::ssd::EffectInvalidationPolicy::Always
            ) && !source_damage_hit
            {
                if let Some(existing) = layer_backdrop_cache
                    .get(&stable_key)
                    .filter(|existing| existing.signature == signature)
                    .cloned()
                {
                    for rect in rects {
                    let rect_key = format!("{}:{}:{}:{}:{}", stable_key, rect.x, rect.y, rect.width, rect.height);
                    let rect_local = smithay::utils::Rectangle::new(
                        smithay::utils::Point::from((
                            rect.x - output_geo.loc.x,
                            rect.y - output_geo.loc.y,
                        )),
                        (rect.width, rect.height).into(),
                    );
                    elements.push(TtyRenderElements::Backdrop(
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
                            &effect_config.effect,
                            1.0,
                            scale.x as f32,
                            None,
                            0,
                            format!("layer-lower:{}:{}", output.name(), rect_key),
                        )?,
                    ));
                }
                    continue;
                }
            }
            let mut backdrop_scene: Vec<TtyRenderElements> = Vec::new();
            for lower_layer in lower_layers.iter().skip(index + 1) {
                if let Ok(mut layer_elements) = layer_surface_scene_elements_for_capture(
                    renderer,
                    output,
                    capture_geo,
                    scale,
                    lower_layer,
                ) {
                    backdrop_scene.append(&mut layer_elements);
                }
            }
            if backdrop_scene.is_empty() {
                continue;
            }
            let snapshot = crate::backend::snapshot::capture_snapshot(
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
            )?
            .ok_or("missing backdrop snapshot")?;
            let texture = crate::backend::shader_effect::apply_effect_pipeline(
                renderer,
                snapshot.texture,
                (capture_geo.size.w, capture_geo.size.h),
                Some(Rectangle::new(
                    Point::from((
                        effect_rect.x - capture_geo.loc.x,
                        effect_rect.y - capture_geo.loc.y,
                    )),
                    (effect_rect.width, effect_rect.height).into(),
                )),
                Some((effect_rect.width, effect_rect.height)),
                &effect_config.effect,
            )?;
            let mut sub_elements = layer_backdrop_cache
                .get(&stable_key)
                .map(|existing| existing.sub_elements.clone())
                .unwrap_or_default();
            let had_existing = layer_backdrop_cache.contains_key(&stable_key);
            for rect in &rects {
                let rect_key = format!("{}:{}:{}:{}:{}", stable_key, rect.x, rect.y, rect.width, rect.height);
                let entry = sub_elements.entry(rect_key).or_default();
                if had_existing {
                    entry.commit_counter.increment();
                }
            }
            layer_backdrop_cache.insert(
                stable_key.clone(),
                crate::backend::shader_effect::CachedBackdropTexture {
                    signature,
                    texture: texture.clone(),
                    id: layer_backdrop_cache
                        .get(&stable_key)
                        .map(|cached| cached.id.clone())
                        .unwrap_or_else(smithay::backend::renderer::element::Id::new),
                    commit_counter: layer_backdrop_cache
                        .get(&stable_key)
                        .map(|existing| {
                            let mut counter = existing.commit_counter;
                            counter.increment();
                            counter
                        })
                        .unwrap_or_default(),
                    sub_elements,
                },
            );
            for rect in rects {
                let rect_key = format!("{}:{}:{}:{}:{}", stable_key, rect.x, rect.y, rect.width, rect.height);
                let rect_local = smithay::utils::Rectangle::new(
                    smithay::utils::Point::from((
                        rect.x - output_geo.loc.x,
                        rect.y - output_geo.loc.y,
                    )),
                    (rect.width, rect.height).into(),
                );
                elements.push(TtyRenderElements::Backdrop(
                    crate::backend::shader_effect::backdrop_shader_element(
                        renderer,
                        layer_backdrop_cache
                            .get(&stable_key)
                            .and_then(|cached| cached.sub_elements.get(&rect_key))
                            .map(|entry| entry.id.clone())
                            .unwrap_or_else(smithay::backend::renderer::element::Id::new),
                        layer_backdrop_cache
                            .get(&stable_key)
                            .and_then(|cached| cached.sub_elements.get(&rect_key))
                            .map(|entry| entry.commit_counter)
                            .unwrap_or_default(),
                        texture.clone(),
                        rect_local,
                        rect_local,
                        captured_local_rect,
                        &effect_config.effect,
                        1.0,
                        scale.x as f32,
                        None,
                        0,
                        format!("layer-lower:{}:{}", output.name(), rect_key),
                    )?,
                ));
            }
        }
    }
    Ok(elements)
}

fn upper_layer_scene_elements(
    renderer: &mut GlesRenderer,
    space: &smithay::desktop::Space<smithay::desktop::Window>,
    window_decorations: &mut std::collections::HashMap<smithay::desktop::Window, crate::ssd::WindowDecorationState>,
    window_source_damage: &[crate::state::OwnedDamageRect],
    lower_layer_source_damage: &[crate::state::OwnedDamageRect],
    effect_config: Option<&crate::ssd::BackgroundEffectConfig>,
    output: &Output,
    output_geo: smithay::utils::Rectangle<i32, Logical>,
    scale: smithay::utils::Scale<f64>,
    windows_top_to_bottom: &[smithay::desktop::Window],
    layer_backdrop_cache: &mut std::collections::HashMap<String, crate::backend::shader_effect::CachedBackdropTexture>,
) -> Result<Vec<TtyRenderElements>, Box<dyn std::error::Error>> {
    let map = layer_map_for_output(output);
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
                .map(TtyRenderElements::Window),
        );
        if let Some(effect_config) = effect_config {
            elements.extend(configured_background_effect_elements_for_layer(
                renderer,
                space,
                window_decorations,
                window_source_damage,
                lower_layer_source_damage,
                output,
                output_geo,
                scale,
                windows_top_to_bottom,
                &layer_surface,
                1.0,
                effect_config,
                layer_backdrop_cache,
            )?);
        }
    }
    Ok(elements)
}

fn configured_background_effect_elements_for_window(
    renderer: &mut GlesRenderer,
    space: &smithay::desktop::Space<smithay::desktop::Window>,
    window_decorations: &mut std::collections::HashMap<smithay::desktop::Window, crate::ssd::WindowDecorationState>,
    _window_commit_times: &std::collections::HashMap<smithay::desktop::Window, std::time::Duration>,
    window_source_damage: &[crate::state::OwnedDamageRect],
    lower_layer_source_damage: &[crate::state::OwnedDamageRect],
    output: &Output,
    output_geo: smithay::utils::Rectangle<i32, Logical>,
    scale: smithay::utils::Scale<f64>,
    windows_top_to_bottom: &[smithay::desktop::Window],
    window_index: usize,
    window: &smithay::desktop::Window,
    alpha: f32,
    effect_config: &crate::ssd::BackgroundEffectConfig,
) -> Vec<(usize, crate::backend::shader_effect::StableBackdropTextureElement)> {
    let Some(decoration) = window_decorations.get(window).cloned() else {
        return Vec::new();
    };
    let rects = protocol_background_effect_rects_for_window(window, &decoration);
    if rects.is_empty() {
        return Vec::new();
    }
    let lower_windows = windows_top_to_bottom
        .iter()
        .skip(window_index + 1)
        .cloned()
        .collect::<Vec<_>>();
    let (_, lower_layers) = window_render::layer_surfaces_for_output(output);
    let relevant_source_damage = {
        let mut entries = collect_window_source_damage(
            window_decorations,
            lower_windows.iter().cloned(),
            window_source_damage,
        );
        entries.extend(collect_layer_source_damage(
            lower_layers.iter().cloned(),
            lower_layer_source_damage,
        ));
        entries
    };

    rects
        .into_iter()
        .enumerate()
        .filter_map(|(index, rect)| {
            let stable_key = format!("__protocol_background_effect_{}", index);
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            stable_key.hash(&mut hasher);
            let effect_rect = crate::backend::visual::transformed_rect(
                rect,
                decoration.layout.root.rect,
                decoration.visual_transform,
            );
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
            let blur_padding = effect_config
                .effect
                .blur_stage()
                .map(|blur| {
                    let radius = blur.radius.max(1);
                    let passes = blur.passes.max(1);
                    (radius * passes * 24 + 32).max(32)
                })
                .unwrap_or(0);
            blur_padding.hash(&mut hasher);
            format!("{:?}", effect_config.effect).hash(&mut hasher);
            let capture_geo = smithay::utils::Rectangle::new(
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
            for lower_window in &lower_windows {
                if let Some(lower_decoration) = window_decorations.get(lower_window) {
                    lower_decoration.snapshot.id.hash(&mut hasher);
                    lower_decoration.visual_transform.translate_x.to_bits().hash(&mut hasher);
                    lower_decoration.visual_transform.translate_y.to_bits().hash(&mut hasher);
                    lower_decoration.visual_transform.scale_x.to_bits().hash(&mut hasher);
                    lower_decoration.visual_transform.scale_y.to_bits().hash(&mut hasher);
                    lower_decoration.visual_transform.opacity.to_bits().hash(&mut hasher);
                }
            }
            let signature = hasher.finish();
            let source_damage_hit = crate::backend::shader_effect::source_damage_intersects_rect(
                &effect_config.effect,
                smithay::utils::Rectangle::new(
                    smithay::utils::Point::from((effect_rect.x, effect_rect.y)),
                    (effect_rect.width, effect_rect.height).into(),
                ),
                &relevant_source_damage,
            );

            if !matches!(
                effect_config.effect.invalidate_policy(),
                crate::ssd::EffectInvalidationPolicy::Always
            ) && !source_damage_hit
            {
                if let Some(existing) = window_decorations
                    .get(window)
                    .and_then(|d| d.backdrop_cache.get(&stable_key))
                    .filter(|existing| existing.signature == signature)
                    .cloned()
                {
                    let local_rect = smithay::utils::Rectangle::new(
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
                        &effect_config.effect,
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

            let mut backdrop_scene: Vec<TtyRenderElements> = Vec::new();
            for lower_window in &lower_windows {
                if let Ok(mut elements) = window_scene_elements_for_capture(
                    renderer,
                    space,
                    window_decorations,
                    capture_geo,
                    scale,
                    lower_window,
                ) {
                    backdrop_scene.append(&mut elements);
                }
            }
            let (_, lower_layer_elements) =
                window_render::layer_elements_for_output(renderer, output, scale, 1.0);
            let capture_offset = capture_geo.loc - output_geo.loc;
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
                    TtyRenderElements::Window,
                    TtyRenderElements::TransformedWindow,
                )
                .into_iter(),
            );
            if backdrop_scene.is_empty() {
                return None;
            }
            let snapshot = crate::backend::snapshot::capture_snapshot(
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
            let texture = crate::backend::shader_effect::apply_effect_pipeline(
                renderer,
                snapshot.texture,
                (capture_geo.size.w, capture_geo.size.h),
                Some(Rectangle::new(
                    Point::from((
                        effect_rect.x - capture_geo.loc.x,
                        effect_rect.y - capture_geo.loc.y,
                    )),
                    (effect_rect.width, effect_rect.height).into(),
                )),
                Some((effect_rect.width, effect_rect.height)),
                &effect_config.effect,
            )
            .ok()?;
            let commit_counter = window_decorations
                .get(window)
                .and_then(|d| d.backdrop_cache.get(&stable_key))
                .map(|existing| {
                    let mut counter = existing.commit_counter;
                    counter.increment();
                    counter
                })
                .unwrap_or_default();
            if let Some(window_decoration) = window_decorations.get_mut(window) {
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
            let local_rect = smithay::utils::Rectangle::new(
                smithay::utils::Point::from((
                    effect_rect.x - output_geo.loc.x,
                    effect_rect.y - output_geo.loc.y,
                )),
                (effect_rect.width, effect_rect.height).into(),
            );
            crate::backend::shader_effect::backdrop_shader_element(
                renderer,
                window_decorations
                    .get(window)
                    .and_then(|d| d.backdrop_cache.get(&stable_key))
                    .map(|cached| cached.id.clone())
                    .unwrap_or_else(smithay::backend::renderer::element::Id::new),
                window_decorations
                    .get(window)
                    .and_then(|d| d.backdrop_cache.get(&stable_key))
                    .map(|cached| cached.commit_counter)
                    .unwrap_or_default(),
                texture,
                local_rect,
                local_rect,
                local_rect,
                &effect_config.effect,
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
    space: &smithay::desktop::Space<smithay::desktop::Window>,
    window_decorations: &std::collections::HashMap<smithay::desktop::Window, crate::ssd::WindowDecorationState>,
    capture_geo: smithay::utils::Rectangle<i32, Logical>,
    scale: smithay::utils::Scale<f64>,
    window: &smithay::desktop::Window,
) -> Result<Vec<TtyRenderElements>, Box<dyn std::error::Error>> {
    let Some(window_location) = space.element_location(window) else {
        return Ok(Vec::new());
    };
    let physical_location =
        (window_location - capture_geo.loc).to_physical_precise_round(scale);
    let visual_state = window_decorations
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

    if let Some(decoration) = window_decorations.get(window) {
        let mut ordered_ui_elements: Vec<(usize, TtyRenderElements)> = Vec::new();
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
                    transform_decoration_elements(vec![element], visual_state)?
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
                    transform_text_elements(vec![element], visual_state)?
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
                    transform_text_elements(vec![element], visual_state)?
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
                TtyRenderElements::Window,
                TtyRenderElements::TransformedWindow,
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
            TtyRenderElements::Window,
            TtyRenderElements::TransformedWindow,
        )
        .into_iter(),
    );

    Ok(elements)
}

fn is_identity_visual(visual: WindowVisualState) -> bool {
    visual.translation.x == 0
        && visual.translation.y == 0
        && (visual.scale.x - 1.0).abs() < f64::EPSILON
        && (visual.scale.y - 1.0).abs() < f64::EPSILON
        && (visual.opacity - 1.0).abs() < f32::EPSILON
}

fn capture_live_snapshot_for_window(
    renderer: &mut GlesRenderer,
    window: &smithay::desktop::Window,
    window_location: smithay::utils::Point<i32, Logical>,
    scale: smithay::utils::Scale<f64>,
    z_index: usize,
    window_decorations: &mut std::collections::HashMap<smithay::desktop::Window, crate::ssd::WindowDecorationState>,
    live_window_snapshots: &mut std::collections::HashMap<String, crate::backend::snapshot::LiveWindowSnapshot>,
    complete_window_snapshots: &mut std::collections::HashMap<String, crate::backend::snapshot::LiveWindowSnapshot>,
) -> Result<(), smithay::backend::renderer::gles::GlesError> {
    let Some(decoration) = window_decorations.get_mut(window) else {
        return Ok(());
    };
    let client_rect = decoration.client_rect;
    let snapshot_geo = smithay::utils::Rectangle::new(
        smithay::utils::Point::from((client_rect.x, client_rect.y)),
        (client_rect.width, client_rect.height).into(),
    );
    let physical_location =
        (window_location - snapshot_geo.loc).to_physical_precise_round(scale);

    let surface_elements =
        window_render::surface_elements(window, renderer, physical_location, scale, 1.0);
    let has_client_content = !surface_elements.is_empty();
    let elements = surface_elements
        .into_iter()
        .map(TtyRenderElements::Window)
        .collect::<Vec<_>>();

    let existing = live_window_snapshots.remove(&decoration.snapshot.id);
    if let Some(snapshot) = snapshot::capture_snapshot(
        renderer,
        existing,
        client_rect,
        z_index,
        has_client_content,
        scale,
        &elements,
    )? {
        live_window_snapshots.insert(decoration.snapshot.id.clone(), snapshot);
        if has_client_content {
            if let Some(snapshot) = live_window_snapshots.get(&decoration.snapshot.id) {
                if let Ok(complete_snapshot) = snapshot::duplicate_snapshot(renderer, snapshot) {
                    complete_window_snapshots.insert(decoration.snapshot.id.clone(), complete_snapshot);
                }
            }
        }
    }

    Ok(())
}

fn closing_snapshot_elements(
    renderer: &mut GlesRenderer,
    closing_snapshots: &[crate::backend::snapshot::ClosingWindowSnapshot],
    output_geo: smithay::utils::Rectangle<i32, Logical>,
    scale: smithay::utils::Scale<f64>,
) -> Vec<TtyRenderElements> {
    closing_snapshots
        .iter()
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
                if let Ok(transformed) = transform_text_elements(icon_elements, visual) {
                    elements.extend(transformed);
                }
            }
            if let Ok(text_elements) = crate::backend::text::text_elements_for_decoration(
                renderer,
                &snapshot.decoration,
                output_geo,
                scale,
                visual.opacity,
            ) {
                if let Ok(transformed) = transform_text_elements(text_elements, visual) {
                    elements.extend(transformed);
                }
            }
            let mut decoration = snapshot.decoration.clone();
            if let Ok(background_elements) = decoration::background_elements_for_window(
                renderer,
                &mut decoration,
                output_geo,
                scale,
                visual.opacity,
            ) {
                if let Ok(transformed) = transform_decoration_elements(background_elements, visual) {
                    elements.extend(transformed);
                }
            }

            if let Some(element) =
                snapshot::closing_snapshot_element(renderer, snapshot, output_geo, scale)
            {
                if let Ok(transformed) = transform_snapshot_elements(vec![element], visual) {
                    elements.extend(transformed);
                }
            }
            elements
        })
        .collect()
}

fn connector_connected(
    state: &mut ShojiWM,
    node: DrmNode,
    crtc: crtc::Handle,
    connector: connector::Info,
) -> Result<(), Box<dyn std::error::Error>> {
    let output_name = format!("{}-{}", connector.interface().as_str(), connector.interface_id());
    if !state.display_config.tty_output_allowed(&output_name) {
        info!(
            ?node,
            ?crtc,
            output = %output_name,
            "skipping tty output because it is filtered out"
        );
        return Ok(());
    }

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
        output_name,
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
        frame_callback_timer_armed: false,
        frame_callback_timer_generation: 0,
        frame_callback_sequence: 0,
        redraw_state: TtyRedrawState::Idle,
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
        surface.redraw_state = TtyRedrawState::WaitingForVBlank {
            redraw_needed: false,
        };
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

fn schedule_estimated_vblank_callback(
    loop_handle: &LoopHandle<'_, ShojiWM>,
    state: &mut ShojiWM,
    node: DrmNode,
    crtc: crtc::Handle,
    frame_time: Duration,
) {
    let Some(backend) = state.tty_backends.get_mut(&node) else {
        return;
    };
    let Some(surface) = backend.surfaces.get_mut(&crtc) else {
        return;
    };
    let generation = match surface.redraw_state {
        TtyRedrawState::WaitingForEstimatedVBlank { generation, .. } => generation,
        _ => return,
    };
    if surface.frame_callback_timer_armed {
        return;
    }

    let delay = surface.frame_duration;
    surface.frame_callback_timer_armed = true;
    let output = surface.output.clone();

    if loop_handle
        .insert_source(Timer::from_duration(delay), move |_, _, state| {
            let outcome = {
                let Some(backend) = state.tty_backends.get_mut(&node) else {
                    return TimeoutAction::Drop;
                };
                let Some(surface) = backend.surfaces.get_mut(&crtc) else {
                    return TimeoutAction::Drop;
                };
                match surface.redraw_state {
                    TtyRedrawState::WaitingForEstimatedVBlank {
                        queued,
                        generation: current_generation,
                    } if surface.frame_callback_timer_armed && current_generation == generation => {
                        surface.frame_callback_timer_armed = false;
                        surface.frame_callback_sequence =
                            surface.frame_callback_sequence.wrapping_add(1);
                        let sequence = surface.frame_callback_sequence;
                        if queued {
                            surface.redraw_state = TtyRedrawState::Queued;
                            Some((sequence, true))
                        } else {
                            surface.redraw_state = TtyRedrawState::Idle;
                            Some((sequence, false))
                        }
                    }
                    _ => None,
                }
            };
            let Some((sequence, should_redraw)) = outcome else {
                return TimeoutAction::Drop;
            };
            let callback_time = frame_time.saturating_add(delay);
            if should_redraw {
                state.schedule_redraw();
            } else {
                state.send_frame_callbacks_for_output(&output, callback_time, Some(sequence));
                state.signal_post_repaint_barriers(&output);
            }
            TimeoutAction::Drop
        })
        .is_err()
    {
        surface.frame_callback_timer_armed = false;
        warn!(?node, ?crtc, "failed to schedule tty estimated vblank callback");
    }
}

fn blend_render_duration(previous: Duration, current: Duration) -> Duration {
    if previous.is_zero() {
        return current;
    }

    Duration::from_secs_f64(previous.as_secs_f64() * 0.75 + current.as_secs_f64() * 0.25)
}
