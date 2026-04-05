use std::hash::{Hash, Hasher};
use std::{
    collections::HashMap,
    path::Path,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

use smithay::{
    backend::{
        allocator::{Fourcc, gbm::GbmAllocator},
        drm::{
            DrmDevice, DrmDeviceFd, DrmEvent, DrmEventMetadata, DrmEventTime, DrmNode,
            compositor::FrameFlags,
            exporter::gbm::{GbmFramebufferExporter, NodeFilter},
            output::{DrmOutput, DrmOutputManager, DrmOutputRenderElements},
        },
        egl::{EGLContext, EGLDisplay, context::ContextPriority},
        renderer::{
            Bind, ExportMem, ImportDma, ImportEgl, ImportMemWl, Offscreen,
            damage::OutputDamageTracker,
            element::{
                AsRenderElements,
                memory::MemoryRenderBuffer,
                solid::SolidColorRenderElement,
                surface::WaylandSurfaceRenderElement,
                texture::TextureRenderElement,
                utils::{Relocate, RelocateRenderElement, RescaleRenderElement},
            },
            gles::{GlesRenderer, GlesTexture},
        },
        session::{Session, libseat::LibSeatSession},
    },
    desktop::layer_map_for_output,
    input::pointer::{CursorImageAttributes, CursorImageStatus},
    output::{Mode as WlMode, Output, PhysicalProperties},
    reexports::{
        calloop::{
            EventLoop, LoopHandle,
            timer::{TimeoutAction, Timer},
        },
        drm::control::{connector, crtc},
        gbm::{BufferObjectFlags, Device, Format},
        rustix::fs::OFlags,
        wayland_protocols::wp::presentation_time::server::wp_presentation_feedback,
        wayland_server::Resource,
    },
    render_elements,
    utils::{DeviceFd, IsAlive, Logical, Monotonic, Point, Rectangle, Scale, Transform},
    wayland::{
        background_effect::BackgroundEffectSurfaceCachedState, compositor,
        dmabuf::DmabufFeedbackBuilder,
    },
};
use smithay_drm_extras::drm_scanner::{DrmScanEvent, DrmScanner};
use tracing::{debug, info, trace, warn};

use crate::{
    backend::damage,
    backend::damage_blink,
    backend::decoration,
    backend::snapshot,
    backend::visual::{
        WindowVisualState, relative_physical_rect_from_root_precise, root_physical_origin,
        transformed_root_rect, window_visual_state,
    },
    backend::window as window_render,
    config::DisplayModePreference,
    drawing::PointerRenderElement,
    presentation::{take_presentation_feedback, update_primary_scanout_output},
    state::ShojiWM,
};
use smithay::wayland::presentation::Refresh;

const CLEAR_COLOR: [f32; 4] = [0.08, 0.10, 0.13, 1.0];
const TTY_FRAME_FLAGS: FrameFlags = FrameFlags::DEFAULT;

type GbmDrmOutput = DrmOutput<
    GbmAllocator<DrmDeviceFd>,
    GbmFramebufferExporter<DrmDeviceFd>,
    Option<smithay::desktop::utils::OutputPresentationFeedback>,
    DrmDeviceFd,
>;

#[derive(Debug, Clone, Copy, Default)]
struct TitlebarFillFrameState {
    first_pre_fill: Option<Rectangle<i32, smithay::utils::Physical>>,
    second_pre_fill: Option<Rectangle<i32, smithay::utils::Physical>>,
}

fn previous_titlebar_fill_state(
    key: &str,
    current: TitlebarFillFrameState,
) -> Option<TitlebarFillFrameState> {
    static STATE: OnceLock<Mutex<HashMap<String, TitlebarFillFrameState>>> = OnceLock::new();
    let state = STATE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = state.lock().ok()?;
    guard.insert(key.to_string(), current)
}

#[derive(Debug, Clone, Copy, Default)]
struct ClientFrameState {
    client_geometry: Option<Rectangle<i32, smithay::utils::Physical>>,
    content_clip_physical: Option<Rectangle<i32, smithay::utils::Physical>>,
    fill_client_edge_delta: Option<(i32, i32, i32, i32)>,
}

fn previous_client_frame_state(key: &str, current: ClientFrameState) -> Option<ClientFrameState> {
    static STATE: OnceLock<Mutex<HashMap<String, ClientFrameState>>> = OnceLock::new();
    let state = STATE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = state.lock().ok()?;
    guard.insert(key.to_string(), current)
}

#[derive(Debug, Clone, Copy, Default)]
struct BackdropSampleFrameState {
    sample_screen_rect: Option<(f64, f64, f64, f64)>,
}

fn backdrop_sample_state_map() -> &'static Mutex<HashMap<String, BackdropSampleFrameState>> {
    static STATE: OnceLock<Mutex<HashMap<String, BackdropSampleFrameState>>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn previous_backdrop_sample_state(
    key: &str,
    current: BackdropSampleFrameState,
) -> Option<BackdropSampleFrameState> {
    let mut guard = backdrop_sample_state_map().lock().ok()?;
    guard.insert(key.to_string(), current)
}

fn latest_backdrop_sample_rect(key: &str) -> Option<(f64, f64, f64, f64)> {
    let guard = backdrop_sample_state_map().lock().ok()?;
    guard.get(key).and_then(|state| state.sample_screen_rect)
}

struct SurfaceData {
    output: Output,
    drm_output: GbmDrmOutput,
    available_modes: Vec<smithay::reexports::drm::control::Mode>,
    blink_damage_tracker: OutputDamageTracker,
    frame_pending: bool,
    queued_at: Option<Instant>,
    queued_cpu_duration: Duration,
    skipped_while_pending_count: u32,
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
        let sequence = metadata
            .as_ref()
            .map(|metadata| metadata.sequence)
            .unwrap_or(0);
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

    surface.last_presented_at = Some(presentation_clock);

    surface.frame_pending = false;
    surface.queued_at = None;
    surface.queued_cpu_duration = Duration::ZERO;
    surface.skipped_while_pending_count = 0;
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
    Cursor=PointerRenderElement<GlesRenderer>,
}

fn capture_scene_texture_for_effect(
    renderer: &mut GlesRenderer,
    capture_geo: smithay::utils::Rectangle<i32, Logical>,
    scale: smithay::utils::Scale<f64>,
    scene: &[TtyRenderElements],
) -> Option<GlesTexture> {
    if scene.is_empty() {
        return None;
    }
    crate::backend::snapshot::capture_snapshot(
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

fn queue_tty_redraws(state: &mut ShojiWM) {
    for backend in state.tty_backends.values_mut() {
        for surface in backend.surfaces.values_mut() {
            let previous_state = surface.redraw_state;
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
            if std::env::var_os("SHOJI_TRANSFORM_SNAPSHOT_DEBUG").is_some()
                && previous_state != surface.redraw_state
            {
                tracing::info!(
                    output = %surface.output.name(),
                    previous_state = ?previous_state,
                    next_state = ?surface.redraw_state,
                    frame_pending = surface.frame_pending,
                    skipped_while_pending_count = surface.skipped_while_pending_count,
                    "transform snapshot tty queue redraw transition"
                );
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
    let frame_started_at = Instant::now();
    let output = state
        .tty_backends
        .get(&node)
        .and_then(|backend| backend.surfaces.get(&crtc))
        .map(|surface| surface.output.clone())
        .unwrap();

    state.refresh_window_decorations_for_output(Some(output.name().as_str()))?;
    state.refresh_layer_effects_for_output(output.name().as_str())?;

    let redraw_state = state
        .tty_backends
        .get(&node)
        .and_then(|backend| backend.surfaces.get(&crtc))
        .map(|surface| surface.redraw_state)
        .unwrap_or(TtyRedrawState::Idle);

    if redraw_state != TtyRedrawState::Queued {
        if let Some(surface) = state
            .tty_backends
            .get_mut(&node)
            .and_then(|backend| backend.surfaces.get_mut(&crtc))
        {
            if surface.frame_pending {
                surface.skipped_while_pending_count =
                    surface.skipped_while_pending_count.saturating_add(1);
            }
            if std::env::var_os("SHOJI_TRANSFORM_SNAPSHOT_DEBUG").is_some() {
                tracing::info!(
                    output = %output.name(),
                    redraw_state = ?redraw_state,
                    frame_pending = surface.frame_pending,
                    skipped_while_pending_count = surface.skipped_while_pending_count,
                    "transform snapshot tty skipped render_surface"
                );
            }
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
        let window_source_damage_snapshot = state.window_source_damage.clone();
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
            transform_snapshot_window_ids,
            ..
        } = state;

        let backend = tty_backends.get_mut(&node).unwrap();
        let surface = backend.surfaces.get_mut(&crtc).unwrap();
        let render_started_at = Instant::now();
        let frame_time = surface
            .next_frame_target
            .take()
            .unwrap_or(fallback_frame_time);
        surface.last_frame_callback_at = Some(frame_time);

        let mut cursor_elements: Vec<TtyRenderElements> = Vec::new();
        let mut frame_had_transform_snapshot_damage = false;
        let mut frame_transform_snapshot_window_count = 0usize;
        let cursor_started_at = Instant::now();

        let pointer_pos = seat.get_pointer().unwrap().current_location();
        let output_geo = space.output_geometry(&output).unwrap();
        let scale = Scale::from(output.current_scale().fractional_scale());
        let windows: Vec<_> = space.elements_for_output(&output).cloned().collect();
        let windows_top_to_bottom: Vec<_> = windows.iter().rev().cloned().collect();
        let all_windows: Vec<_> = space.elements().cloned().collect();
        let window_count = all_windows.len();
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
        let _cursor_elapsed_ms = cursor_started_at.elapsed().as_secs_f64() * 1000.0;

        let mut scene_elements: Vec<TtyRenderElements> = Vec::new();
        let upper_layers_started_at = Instant::now();
        scene_elements.extend(upper_layer_scene_elements(
            &mut backend.renderer,
            space,
            window_decorations,
            &state.window_source_damage,
            &state.lower_layer_source_damage,
            state.lower_layer_scene_generation,
            &state.configured_layer_effects,
            state.configured_background_effect.as_ref(),
            &output,
            output_geo,
            scale,
            &windows_top_to_bottom,
            &mut state.layer_backdrop_cache,
        )?);
        let _upper_layers_elapsed_ms = upper_layers_started_at.elapsed().as_secs_f64() * 1000.0;
        let mut _window_loop_elapsed_ms = 0.0f64;
        let mut max_window_elapsed_ms = 0.0f64;
        let mut _max_window_id: Option<String> = None;
        let mut _snapshot_capture_elapsed_ms = 0.0f64;
        let mut snapshot_capture_count = 0usize;

        for (_window_index, window) in windows_top_to_bottom.iter().enumerate() {
            let window_started_at = Instant::now();
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
            let preliminary_physical_location =
                (window_location - output_geo.loc).to_physical_precise_round(scale);
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
                    origin: preliminary_physical_location,
                    scale: smithay::utils::Scale::from((1.0, 1.0)),
                    translation: (0, 0).into(),
                    opacity: 1.0,
                });
            let snap_scale = Scale::from((
                scale.x * visual_state.scale.x.max(0.0),
                scale.y * visual_state.scale.y.max(0.0),
            ));
            let client_physical_geometry = window_decorations.get(window).and_then(|decoration| {
                decoration.content_clip.map(|clip| {
                    let root_origin =
                        root_physical_origin(decoration.layout.root.rect, output_geo, scale);
                    let local_geometry = relative_physical_rect_from_root_precise(
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
            let use_full_window_snapshot = !is_identity_visual(visual_state);
            let used_transform_snapshot_last_frame =
                transform_snapshot_window_ids.contains(&window_id);
            if use_full_window_snapshot {
                frame_transform_snapshot_window_count =
                    frame_transform_snapshot_window_count.saturating_add(1);
            }
            let snapshot_id = window_decorations
                .get(window)
                .map(|decoration| decoration.snapshot.id.clone());
            let window_has_snapshot_damage = snapshot_id.as_ref().is_some_and(|snapshot_id| {
                snapshot_dirty_window_ids.contains(snapshot_id)
                    || window_source_damage_snapshot
                        .iter()
                        .any(|damage| damage.owner == *snapshot_id)
            });
            if ((use_full_window_snapshot != used_transform_snapshot_last_frame)
                || (use_full_window_snapshot && window_has_snapshot_damage))
                && let Some(decoration) = window_decorations.get(window)
            {
                if use_full_window_snapshot && window_has_snapshot_damage {
                    frame_had_transform_snapshot_damage = true;
                }
                extra_damage.push(transformed_root_rect(
                    decoration.layout.root.rect,
                    decoration.visual_transform,
                ));
            }
            if use_full_window_snapshot {
                transform_snapshot_window_ids.insert(window_id.clone());
            } else {
                transform_snapshot_window_ids.remove(&window_id);
            }
            let mut ordered_ui_elements: Vec<(usize, TtyRenderElements)> = Vec::new();
            let mut ordered_backdrop_elements: Vec<(usize, TtyRenderElements)> = Vec::new();
            let mut snapshot_ui_items: Vec<(usize, TtyRenderElements)> = Vec::new();
            let mut snapshot_backdrop_items: Vec<(usize, TtyRenderElements)> = Vec::new();
            let mut debug_background_geometries: Vec<(
                usize,
                String,
                &'static str,
                smithay::utils::Rectangle<i32, smithay::utils::Physical>,
            )> = Vec::new();
            let mut debug_background_pre_geometries: Vec<(
                usize,
                String,
                &'static str,
                smithay::utils::Rectangle<i32, smithay::utils::Physical>,
            )> = Vec::new();
            let mut debug_ui_geometries: Vec<(
                usize,
                String,
                &'static str,
                smithay::utils::Rectangle<i32, smithay::utils::Physical>,
            )> = Vec::new();
            let mut debug_ui_pre_geometries: Vec<(
                usize,
                String,
                &'static str,
                smithay::utils::Rectangle<i32, smithay::utils::Physical>,
            )> = Vec::new();
            let root_origin = window_decorations.get(window).map(|decoration| {
                root_physical_origin(decoration.layout.root.rect, output_geo, scale)
            });
            let composition_visual = if use_full_window_snapshot {
                WindowVisualState {
                    origin: Point::from((0, 0)),
                    scale: Scale::from((1.0, 1.0)),
                    translation: Point::from((0, 0)),
                    opacity: 1.0,
                }
            } else {
                visual_state
            };
            if decoration_ready {
                let mut backdrop_items = backdrop_shader_elements_for_window(
                    &mut backend.renderer,
                    space,
                    window_decorations,
                    &state.window_commit_times,
                    &state.window_source_damage,
                    &state.lower_layer_source_damage,
                    state.lower_layer_scene_generation,
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
                );
                if let Some(effect_config) = state.configured_background_effect.as_ref() {
                    backdrop_items.extend(
                        configured_background_effect_elements_for_window(
                            &mut backend.renderer,
                            space,
                            window_decorations,
                            &state.window_commit_times,
                            &state.window_source_damage,
                            &state.lower_layer_source_damage,
                            state.lower_layer_scene_generation,
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
                            effect_config,
                            false,
                        )
                        .into_iter()
                        .map(|(order, element)| (order, element, true)),
                    );
                }
                for (order, element, render_as_backdrop) in backdrop_items.drain(..) {
                    if let Some(root_origin) = root_origin {
                        let items = transform_backdrop_elements(
                            vec![element],
                            root_origin,
                            composition_visual,
                        )?;
                        if std::env::var_os("SHOJI_GAP_READBACK_DEBUG").is_some()
                            && !use_full_window_snapshot
                            && let Some(first_geometry) = items.first().map(|item| {
                                smithay::backend::renderer::element::Element::geometry(item, scale)
                            })
                        {
                            log_gap_readback_edge_probes(
                                &mut backend.renderer,
                                scale,
                                &items,
                                first_geometry,
                                "decoration-backdrop",
                                &output.name(),
                                &window_id,
                            );
                        }
                        if use_full_window_snapshot {
                            if render_as_backdrop {
                                snapshot_backdrop_items
                                    .extend(items.into_iter().map(|item| (order, item)));
                            } else {
                                snapshot_ui_items
                                    .extend(items.into_iter().map(|item| (order, item)));
                            }
                        } else {
                            let transformed = items.into_iter().map(|item| (order, item));
                            if render_as_backdrop {
                                ordered_backdrop_elements.extend(transformed);
                            } else {
                                ordered_ui_elements.extend(transformed);
                            }
                        }
                    }
                }
                if let Some(decoration_state) = window_decorations.get_mut(window) {
                    let mut ordered_background_items =
                        decoration::ordered_background_elements_for_window(
                            &mut backend.renderer,
                            decoration_state,
                            output_geo,
                            if use_full_window_snapshot {
                                scale
                            } else {
                                snap_scale
                            },
                            if use_full_window_snapshot {
                                1.0
                            } else {
                                visual_state.opacity
                            },
                        )
                        .inspect_err(|error| {
                            warn!(?error, "failed to build decoration background elements");
                        })
                        .unwrap_or_default();
                    ordered_background_items.sort_by_key(|(order, _)| *order);
                    for (order, element) in ordered_background_items {
                        if let Some(root_origin) = root_origin {
                            let debug_stable = if std::env::var_os("SHOJI_GAP_DEBUG").is_some() {
                                decoration_state
                                    .buffers
                                    .iter()
                                    .find(|buffer| buffer.order == order)
                                    .map(|buffer| (buffer.stable_key.clone(), buffer.source_kind))
                            } else {
                                None
                            };
                            let pre_transform_geometry =
                                if std::env::var_os("SHOJI_GAP_DEBUG").is_some() {
                                    Some(smithay::backend::renderer::element::Element::geometry(
                                        &element, scale,
                                    ))
                                } else {
                                    None
                                };
                            let items = transform_decoration_elements(
                                vec![element],
                                root_origin,
                                composition_visual,
                            )?;
                            if let (Some((stable_key, source_kind)), Some(pre_transform_geometry)) =
                                (debug_stable, pre_transform_geometry)
                            {
                                let post_transform_geometry = items.first().map(|item| {
                                    smithay::backend::renderer::element::Element::geometry(
                                        item, scale,
                                    )
                                });
                                debug_background_pre_geometries.push((
                                    order,
                                    stable_key.clone(),
                                    source_kind,
                                    pre_transform_geometry,
                                ));
                                if let Some(post_transform_geometry) = post_transform_geometry {
                                    debug_background_geometries.push((
                                        order,
                                        stable_key.clone(),
                                        source_kind,
                                        post_transform_geometry,
                                    ));
                                }
                                tracing::info!(
                                    output = %output.name(),
                                    window_id = %window_id,
                                    stable_key = %stable_key,
                                    source_kind = %source_kind,
                                    order,
                                    root_origin = ?root_origin,
                                    visual_origin = ?composition_visual.origin,
                                    visual_scale = ?composition_visual.scale,
                                    visual_translation = ?composition_visual.translation,
                                    pre_transform_geometry = ?pre_transform_geometry,
                                    post_transform_geometry = ?post_transform_geometry,
                                    "gap debug tty transformed decoration geometry"
                                );
                            }
                            if std::env::var_os("SHOJI_GAP_READBACK_DEBUG").is_some()
                                && !use_full_window_snapshot
                                && let Some(first_geometry) = items.first().map(|item| {
                                    smithay::backend::renderer::element::Element::geometry(
                                        item, scale,
                                    )
                                })
                            {
                                log_gap_readback_edge_probes(
                                    &mut backend.renderer,
                                    scale,
                                    &items,
                                    first_geometry,
                                    "decoration-background",
                                    &output.name(),
                                    &window_id,
                                );
                            }
                            if use_full_window_snapshot {
                                snapshot_ui_items
                                    .extend(items.into_iter().map(|item| (order, item)));
                            } else {
                                ordered_ui_elements
                                    .extend(items.into_iter().map(|item| (order, item)));
                            }
                        }
                    }
                }

                for (order, element) in decoration::ordered_icon_elements_for_window(
                    &mut backend.renderer,
                    space,
                    window_decorations,
                    &output,
                    window,
                    if use_full_window_snapshot {
                        1.0
                    } else {
                        visual_state.opacity
                    },
                )? {
                    if let Some(root_origin) = root_origin {
                        let stable_key = window_decorations
                            .get(window)
                            .and_then(|decoration| {
                                decoration
                                    .icon_buffers
                                    .iter()
                                    .find(|buffer| buffer.order == order)
                                    .map(|buffer| buffer.stable_key.clone())
                            })
                            .unwrap_or_else(|| format!("icon-order-{order}"));
                        let pre_transform_geometry =
                            smithay::backend::renderer::element::Element::geometry(&element, scale);
                        let items = transform_text_elements(
                            vec![element],
                            root_origin,
                            composition_visual,
                        )?;
                        let post_transform_geometry = items.first().map(|item| {
                            smithay::backend::renderer::element::Element::geometry(item, scale)
                        });
                        debug_ui_pre_geometries.push((
                            order,
                            stable_key.clone(),
                            "app-icon",
                            pre_transform_geometry,
                        ));
                        if let Some(post_transform_geometry) = post_transform_geometry {
                            debug_ui_geometries.push((
                                order,
                                stable_key.clone(),
                                "app-icon",
                                post_transform_geometry,
                            ));
                        }
                        tracing::info!(
                            output = %output.name(),
                            window_id = %window_id,
                            stable_key = %stable_key,
                            source_kind = %"app-icon",
                            order,
                            root_origin = ?root_origin,
                            visual_origin = ?composition_visual.origin,
                            visual_scale = ?composition_visual.scale,
                            visual_translation = ?composition_visual.translation,
                            pre_transform_geometry = ?pre_transform_geometry,
                            post_transform_geometry = ?post_transform_geometry,
                            "gap debug tty transformed decoration geometry"
                        );
                        if use_full_window_snapshot {
                            snapshot_ui_items.extend(items.into_iter().map(|item| (order, item)));
                        } else {
                            ordered_ui_elements.extend(items.into_iter().map(|item| (order, item)));
                        }
                    }
                }

                for (order, element) in decoration::ordered_text_elements_for_window(
                    &mut backend.renderer,
                    space,
                    window_decorations,
                    &output,
                    window,
                    if use_full_window_snapshot {
                        1.0
                    } else {
                        visual_state.opacity
                    },
                )? {
                    if let Some(root_origin) = root_origin {
                        let stable_key = window_decorations
                            .get(window)
                            .and_then(|decoration| {
                                decoration
                                    .text_buffers
                                    .iter()
                                    .find(|buffer| buffer.order == order)
                                    .map(|buffer| buffer.stable_key.clone())
                            })
                            .unwrap_or_else(|| format!("label-order-{order}"));
                        let pre_transform_geometry =
                            smithay::backend::renderer::element::Element::geometry(&element, scale);
                        let items = transform_text_elements(
                            vec![element],
                            root_origin,
                            composition_visual,
                        )?;
                        let post_transform_geometry = items.first().map(|item| {
                            smithay::backend::renderer::element::Element::geometry(item, scale)
                        });
                        debug_ui_pre_geometries.push((
                            order,
                            stable_key.clone(),
                            "label",
                            pre_transform_geometry,
                        ));
                        if let Some(post_transform_geometry) = post_transform_geometry {
                            debug_ui_geometries.push((
                                order,
                                stable_key.clone(),
                                "label",
                                post_transform_geometry,
                            ));
                        }
                        tracing::info!(
                            output = %output.name(),
                            window_id = %window_id,
                            stable_key = %stable_key,
                            source_kind = %"label",
                            order,
                            root_origin = ?root_origin,
                            visual_origin = ?composition_visual.origin,
                            visual_scale = ?composition_visual.scale,
                            visual_translation = ?composition_visual.translation,
                            pre_transform_geometry = ?pre_transform_geometry,
                            post_transform_geometry = ?post_transform_geometry,
                            "gap debug tty transformed decoration geometry"
                        );
                        if use_full_window_snapshot {
                            snapshot_ui_items.extend(items.into_iter().map(|item| (order, item)));
                        } else {
                            ordered_ui_elements.extend(items.into_iter().map(|item| (order, item)));
                        }
                    }
                }

                ordered_ui_elements.sort_by_key(|(order, _)| *order);
                ordered_backdrop_elements.sort_by_key(|(order, _)| *order);
                snapshot_ui_items.sort_by_key(|(order, _)| *order);
                snapshot_backdrop_items.sort_by_key(|(order, _)| *order);
                if std::env::var_os("SHOJI_TRANSFORM_SNAPSHOT_DEBUG").is_some() {
                    let first_backdrop = ordered_backdrop_elements.first().map(|(_, element)| {
                        smithay::backend::renderer::element::Element::geometry(element, scale)
                    });
                    let first_snapshot_backdrop =
                        snapshot_backdrop_items.first().map(|(_, element)| {
                            smithay::backend::renderer::element::Element::geometry(element, scale)
                        });
                    let first_ui = ordered_ui_elements.first().map(|(_, element)| {
                        smithay::backend::renderer::element::Element::geometry(element, scale)
                    });
                    let first_snapshot_item = snapshot_ui_items.first().map(|(_, element)| {
                        smithay::backend::renderer::element::Element::geometry(element, scale)
                    });
                    tracing::info!(
                        window_id = %window_id,
                        use_full_window_snapshot,
                        visual_state = ?visual_state,
                        backdrop_count = ordered_backdrop_elements.len(),
                        ui_count = ordered_ui_elements.len(),
                        snapshot_scene_count = snapshot_ui_items.len() + snapshot_backdrop_items.len(),
                        first_backdrop = ?first_backdrop,
                        first_snapshot_backdrop = ?first_snapshot_backdrop,
                        first_ui = ?first_ui,
                        first_snapshot_item = ?first_snapshot_item,
                        "transform snapshot tty branch composition"
                    );
                }
                if std::env::var_os("SHOJI_GAP_DEBUG").is_some() {
                    let first_backdrop = ordered_backdrop_elements.first().map(|(_, element)| {
                        smithay::backend::renderer::element::Element::geometry(element, scale)
                    });
                    let first_fill = debug_background_geometries
                        .iter()
                        .filter(|(_, stable_key, source_kind, geometry)| {
                            *source_kind == "fill"
                                && stable_key.ends_with(":fill")
                                && geometry.size.w > 200
                        })
                        .min_by_key(|(order, _, _, _)| *order)
                        .map(|(_, _, _, geometry)| *geometry);
                    let backdrop_fill_delta = first_backdrop.zip(first_fill).map(|(backdrop, fill)| {
                        (
                            backdrop.loc.x - fill.loc.x,
                            backdrop.loc.y - fill.loc.y,
                            backdrop.size.w - fill.size.w,
                            backdrop.size.h - fill.size.h,
                        )
                    });
                    tracing::info!(
                        window_id = %window_id,
                        first_backdrop = ?first_backdrop,
                        first_fill = ?first_fill,
                        backdrop_fill_delta = ?backdrop_fill_delta,
                        "gap debug tty backdrop/fill compare"
                    );
                }
            }

            let content_clip = window_decorations
                .get(window)
                .and_then(|decoration| decoration.content_clip);

            let client_elements = if use_full_window_snapshot {
                let mut snapshot_scene = Vec::new();
                snapshot_scene.extend(
                    window_render::popup_elements(
                        window,
                        &mut backend.renderer,
                        physical_location,
                        scale,
                        1.0,
                    )
                    .into_iter()
                    .map(TtyRenderElements::Window),
                );
                if let Some(content_clip) = content_clip {
                    let clipped = window_render::clipped_surface_elements(
                        window,
                        &mut backend.renderer,
                        physical_location,
                        client_physical_geometry,
                        output_geo.loc,
                        scale,
                        scale,
                        1.0,
                        Some(content_clip),
                    )
                    .unwrap_or_default();
                    snapshot_scene.extend(clipped.into_iter().map(TtyRenderElements::Clipped));
                } else {
                    snapshot_scene.extend(
                        window_render::surface_elements(
                            window,
                            &mut backend.renderer,
                            physical_location,
                            scale,
                            1.0,
                        )
                        .into_iter()
                        .map(TtyRenderElements::Window),
                    );
                }
                let _client_end_len = snapshot_scene.len();
                snapshot_scene.extend(snapshot_ui_items.into_iter().map(|(_, element)| element));
                snapshot_scene.extend(
                    snapshot_backdrop_items
                        .into_iter()
                        .map(|(_, element)| element),
                );
                let full_rect = window_decorations
                    .get(window)
                    .map(|decoration| decoration.layout.root.rect);
                let snapshot_scene_signature =
                    crate::backend::snapshot::render_element_scene_signature(&snapshot_scene, scale);
                full_rect
                    .and_then(|full_rect| {
                        if std::env::var_os("SHOJI_TRANSFORM_SNAPSHOT_DEBUG").is_some() {
                            let existing_signature = complete_window_snapshots
                                .get(&window_id)
                                .map(|snapshot| snapshot.scene_signature);
                            tracing::info!(
                                window_id = %window_id,
                                full_rect = ?full_rect,
                                use_full_window_snapshot,
                                window_has_snapshot_damage,
                                snapshot_scene_signature,
                                existing_signature = ?existing_signature,
                                "transform snapshot tty complete snapshot decision"
                            );
                        }
                        if !window_has_snapshot_damage {
                            if let Some(existing) = complete_window_snapshots
                                .get(&window_id)
                                .cloned()
                                .filter(|snapshot| {
                                    snapshot.scene_signature == snapshot_scene_signature
                                })
                            {
                                return Some(existing);
                            }
                        }
                        let existing_complete = complete_window_snapshots.remove(&window_id);
                        if std::env::var_os("SHOJI_TRANSFORM_SNAPSHOT_DEBUG").is_some() {
                            let first_snapshot_geometry = snapshot_scene.first().map(|element| {
                                smithay::backend::renderer::element::Element::geometry(
                                    element, scale,
                                )
                            });
                            tracing::info!(
                                window_id = %window_id,
                                full_rect = ?full_rect,
                                snapshot_scene_count = snapshot_scene.len(),
                                first_snapshot_geometry = ?first_snapshot_geometry,
                                "transform snapshot tty assembled current-frame scene"
                            );
                        }
                        capture_snapshot_from_output_elements(
                            &mut backend.renderer,
                            output_geo,
                            full_rect,
                            scale,
                            existing_complete,
                            &snapshot_scene,
                        )
                        .ok()
                        .flatten()
                        .map(|mut snapshot| {
                            snapshot.scene_signature = snapshot_scene_signature;
                            complete_window_snapshots.insert(window_id.clone(), snapshot.clone());
                            snapshot
                        })
                    })
                    .and_then(|snapshot| {
                        snapshot::live_snapshot_element(
                            &backend.renderer,
                            &snapshot,
                            output_geo,
                            scale,
                            visual_state.opacity,
                        )
                    })
                    .and_then(|element| {
                        transform_snapshot_elements(vec![element], visual_state).ok()
                    })
                    .unwrap_or_default()
            } else if let Some(content_clip) = content_clip {
                if std::env::var_os("SHOJI_GAP_DEBUG").is_some() {
                    if let Some(decoration) = window_decorations.get(window) {
                        let border_buffer = decoration.buffers.iter().find(|buffer| {
                            buffer.source_kind == "window-border" && buffer.border_width > 0.0
                        });
                        let border_fill = decoration.buffers.iter().find(|buffer| {
                            buffer.source_kind == "fill" && buffer.hole_rect.is_some()
                        });
                        let snap_scale = Scale::from((
                            scale.x * visual_state.scale.x.max(0.0),
                            scale.y * visual_state.scale.y.max(0.0),
                        ));
                        let border_width = (decoration.layout.root.rect.x
                            + decoration.layout.root.rect.width)
                            - (content_clip.rect.loc.x + content_clip.rect.size.w);
                        let border_rect = Some(crate::ssd::LogicalRect::new(
                            content_clip.rect.loc.x - border_width,
                            content_clip.rect.loc.y - border_width,
                            content_clip.rect.size.w + border_width * 2,
                            content_clip.rect.size.h + border_width * 2,
                        ));
                        let snapped_inner = Some(
                            crate::backend::visual::snapped_logical_rect_relative_with_mode(
                                crate::ssd::LogicalRect::new(
                                    content_clip.rect.loc.x,
                                    content_clip.rect.loc.y,
                                    content_clip.rect.size.w,
                                    content_clip.rect.size.h,
                                ),
                                output_geo.loc,
                                snap_scale,
                                content_clip.snap_mode,
                            ),
                        );
                        let snapped_clip =
                            crate::backend::visual::snapped_logical_rect_relative_with_mode(
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
                        let expected_left = (snapped_clip.x as f64 * scale.x).round() as i32;
                        let expected_top = (snapped_clip.y as f64 * scale.y).round() as i32;
                        let expected_right =
                            ((snapped_clip.x + snapped_clip.width) as f64 * scale.x).round() as i32;
                        let expected_bottom = ((snapped_clip.y + snapped_clip.height) as f64
                            * scale.y)
                            .round() as i32;
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
                            "gap debug tty border/client geometry"
                        );
                        tracing::info!(
                            output = %output.name(),
                            window_id = %window_id,
                            border_buffer_rect = ?border_buffer.map(|buffer| buffer.rect),
                            border_buffer_width = ?border_buffer.map(|buffer| buffer.border_width),
                            border_buffer_hole = ?border_buffer.and_then(|buffer| buffer.hole_rect),
                            border_buffer_hole_precise = ?border_buffer.and_then(|buffer| buffer.hole_rect_precise),
                            border_buffer_hole_radius_precise = ?border_buffer.and_then(|buffer| buffer.hole_radius_precise),
                            border_fill_rect = ?border_fill.map(|buffer| buffer.rect),
                            border_fill_hole = ?border_fill.and_then(|buffer| buffer.hole_rect),
                            decoration_root_rect = ?decoration.layout.root.rect,
                            decoration_slot_rect = ?decoration.layout.window_slot_rect(),
                            "gap debug tty border buffers"
                        );
                        if let Some(decoration) = window_decorations.get(window) {
                            let border_outer_physical = border_buffer.and_then(|buffer| {
                                buffer
                                    .rect_precise
                                    .map(|rect| crate::backend::visual::relative_physical_rect_from_root_precise(
                                        rect,
                                        decoration.layout.root.rect,
                                        output_geo,
                                        scale,
                                    ))
                                    .or_else(|| Some(crate::backend::visual::relative_physical_rect_from_root(
                                        buffer.rect,
                                        decoration.layout.root.rect,
                                        output_geo,
                                        scale,
                                        buffer.clip_rect,
                                    )))
                            });
                            let border_inner_physical = border_buffer.and_then(|buffer| {
                                buffer
                                    .hole_rect_precise
                                    .map(|rect| crate::backend::visual::relative_physical_rect_from_root_precise(
                                        rect,
                                        decoration.layout.root.rect,
                                        output_geo,
                                        scale,
                                    ))
                                    .or_else(|| buffer.hole_rect.map(|rect| {
                                        crate::backend::visual::relative_physical_rect_from_root(
                                            rect,
                                            decoration.layout.root.rect,
                                            output_geo,
                                            scale,
                                            Some(rect),
                                        )
                                    }))
                            });
                            let titlebar_fill = decoration.buffers.iter().find(|buffer| {
                                buffer.source_kind == "fill" && buffer.rect.height == 30
                            });
                            let titlebar_fill_physical = titlebar_fill.map(|buffer| {
                                buffer
                                    .rect_precise
                                    .map(|rect| crate::backend::visual::relative_physical_rect_from_root_precise(
                                        rect,
                                        decoration.layout.root.rect,
                                        output_geo,
                                        scale,
                                    ))
                                    .unwrap_or_else(|| {
                                        crate::backend::visual::relative_physical_rect_from_root(
                                            buffer.rect,
                                            decoration.layout.root.rect,
                                            output_geo,
                                            scale,
                                            buffer.clip_rect,
                                        )
                                    })
                            });
                            let titlebar_shader = decoration
                                .shader_buffers
                                .iter()
                                .find(|buffer| buffer.rect.height == 30);
                            let titlebar_shader_precise = titlebar_shader.and_then(|buffer| {
                                buffer.rect_precise.map(|rect| {
                                    crate::backend::visual::PreciseLogicalRect {
                                        x: rect.x - decoration.layout.root.rect.x as f32,
                                        y: rect.y - decoration.layout.root.rect.y as f32,
                                        width: rect.width,
                                        height: rect.height,
                                    }
                                })
                            });
                            let titlebar_shader_clip_precise = titlebar_shader.and_then(|buffer| {
                                buffer.clip_rect_precise.map(|rect| {
                                    crate::backend::visual::PreciseLogicalRect {
                                        x: rect.x - decoration.layout.root.rect.x as f32,
                                        y: rect.y - decoration.layout.root.rect.y as f32,
                                        width: rect.width,
                                        height: rect.height,
                                    }
                                })
                            });
                            let titlebar_shader_physical = titlebar_shader.map(|buffer| {
                                buffer
                                    .rect_precise
                                    .map(|rect| crate::backend::visual::relative_physical_rect_from_root_precise(
                                        rect,
                                        decoration.layout.root.rect,
                                        output_geo,
                                        scale,
                                    ))
                                    .unwrap_or_else(|| {
                                        crate::backend::visual::relative_physical_rect_from_root(
                                            buffer.rect,
                                            decoration.layout.root.rect,
                                            output_geo,
                                            scale,
                                            buffer.clip_rect,
                                        )
                                    })
                            });
                            let titlebar_shader_clip_physical_precise =
                                titlebar_shader_clip_precise.map(|clip| {
                                    let scale_x = scale.x.abs().max(0.0001) as f32;
                                    let scale_y = scale.y.abs().max(0.0001) as f32;
                                    (
                                        clip.x * scale_x,
                                        clip.y * scale_y,
                                        clip.width * scale_x,
                                        clip.height * scale_y,
                                    )
                                });
                            let titlebar_shader_clip_physical_global_precise =
                                titlebar_shader_clip_physical_precise;
                            let border_expected_inner_precise = border_buffer
                                .and_then(|buffer| buffer.hole_rect_precise)
                                .map(|rect| crate::backend::visual::PreciseLogicalRect {
                                    x: rect.x - decoration.layout.root.rect.x as f32,
                                    y: rect.y - decoration.layout.root.rect.y as f32,
                                    width: rect.width,
                                    height: rect.height,
                                });
                            let border_expected_inner_physical_precise =
                                border_expected_inner_precise.map(|rect| {
                                    let scale_x = scale.x.abs().max(0.0001) as f32;
                                    let scale_y = scale.y.abs().max(0.0001) as f32;
                                    (
                                        rect.x * scale_x,
                                        rect.y * scale_y,
                                        rect.width * scale_x,
                                        rect.height * scale_y,
                                    )
                                });
                            let shader_clip_vs_border_inner_precise =
                                titlebar_shader_clip_physical_global_precise
                                    .zip(border_expected_inner_physical_precise)
                                    .map(|(shader, border)| {
                                        (
                                            shader.0 - border.0,
                                            shader.1 - border.1,
                                            (shader.0 + shader.2) - (border.0 + border.2),
                                            (shader.1 + shader.3) - (border.1 + border.3),
                                        )
                                    });
                            let content_clip_physical =
                                smithay::utils::Rectangle::<i32, smithay::utils::Physical>::new(
                                    smithay::utils::Point::from((expected_left, expected_top)),
                                    (
                                        (expected_right - expected_left).max(0),
                                        (expected_bottom - expected_top).max(0),
                                    )
                                        .into(),
                                );
                            let first_button = decoration.buffers.iter().find(|buffer| {
                                buffer.source_kind == "button" && buffer.border_width > 0.0
                            });
                            let first_button_physical = first_button.map(|buffer| {
                                crate::backend::visual::relative_physical_rect_from_root(
                                    buffer.rect,
                                    decoration.layout.root.rect,
                                    output_geo,
                                    scale,
                                    buffer.clip_rect,
                                )
                            });
                            let button_delta = match (border_inner_physical, first_button_physical)
                            {
                                (Some(inner), Some(button)) => Some((
                                    button.loc.x - inner.loc.x,
                                    button.loc.y - inner.loc.y,
                                    (inner.loc.x + inner.size.w) - (button.loc.x + button.size.w),
                                    (inner.loc.y + inner.size.h) - (button.loc.y + button.size.h),
                                )),
                                _ => None,
                            };
                            tracing::info!(
                                output = %output.name(),
                                window_id = %window_id,
                                border_outer_physical = ?border_outer_physical,
                                border_inner_physical = ?border_inner_physical,
                                titlebar_shader_physical = ?titlebar_shader_physical,
                                titlebar_fill_physical = ?titlebar_fill_physical,
                                content_clip_physical = ?content_clip_physical,
                                border_expected_inner_precise = ?border_expected_inner_precise,
                                border_expected_inner_physical_precise = ?border_expected_inner_physical_precise,
                                titlebar_shader_precise = ?titlebar_shader_precise,
                                titlebar_shader_clip_precise = ?titlebar_shader_clip_precise,
                                titlebar_shader_clip_physical_precise = ?titlebar_shader_clip_physical_precise,
                                titlebar_shader_clip_physical_global_precise = ?titlebar_shader_clip_physical_global_precise,
                                shader_clip_vs_border_inner_precise = ?shader_clip_vs_border_inner_precise,
                                first_button_physical = ?first_button_physical,
                                button_delta = ?button_delta,
                                "gap debug tty border physical compare"
                            );
                        }
                    }
                }
                let clipped = window_render::clipped_surface_elements(
                    window,
                    &mut backend.renderer,
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
                let bypass_clip = std::env::var_os("SHOJI_GAP_BYPASS_CLIP").is_some();
                if std::env::var_os("SHOJI_GAP_DEBUG").is_some() {
                    let first_geometry = clipped.first().map(|element| {
                        smithay::backend::renderer::element::Element::geometry(element, scale)
                    });
                    let window_geometry = window.geometry();
                    let decoration_client_rect = window_decorations
                        .get(window)
                        .map(|decoration| decoration.client_rect);
                    let edge_delta = if let (Some(decoration), Some(first_geometry)) =
                        (window_decorations.get(window), first_geometry)
                    {
                        let snap_scale = Scale::from((
                            scale.x * visual_state.scale.x.max(0.0),
                            scale.y * visual_state.scale.y.max(0.0),
                        ));
                        let snapped_clip =
                            crate::backend::visual::snapped_logical_rect_relative_with_mode(
                                crate::ssd::LogicalRect::new(
                                    decoration.content_clip.unwrap().rect.loc.x,
                                    decoration.content_clip.unwrap().rect.loc.y,
                                    decoration.content_clip.unwrap().rect.size.w,
                                    decoration.content_clip.unwrap().rect.size.h,
                                ),
                                output_geo.loc,
                                snap_scale,
                                decoration.content_clip.unwrap().snap_mode,
                            );
                        let expected_left = (snapped_clip.x as f64 * scale.x).round() as i32;
                        let expected_top = (snapped_clip.y as f64 * scale.y).round() as i32;
                        let expected_right =
                            ((snapped_clip.x + snapped_clip.width) as f64 * scale.x).round() as i32;
                        let expected_bottom = ((snapped_clip.y + snapped_clip.height) as f64
                            * scale.y)
                            .round() as i32;
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
                        "gap debug tty clipped surface elements"
                    );
                    if !debug_background_geometries.is_empty() {
                        let mut titlebar_fills = debug_background_geometries
                            .iter()
                            .filter(|(_, stable_key, source_kind, geometry)| {
                                *source_kind == "fill"
                                    && stable_key.ends_with(":fill")
                                    && geometry.size.w > 200
                            })
                            .cloned()
                            .collect::<Vec<_>>();
                        titlebar_fills.sort_by_key(|(order, _, _, _)| *order);
                        let mut titlebar_pre_fills = debug_background_pre_geometries
                            .iter()
                            .filter(|(_, stable_key, source_kind, geometry)| {
                                *source_kind == "fill"
                                    && stable_key.ends_with(":fill")
                                    && geometry.size.w > 200
                            })
                            .cloned()
                            .collect::<Vec<_>>();
                        titlebar_pre_fills.sort_by_key(|(order, _, _, _)| *order);
                        let first_fill = titlebar_fills.first().cloned();
                        let second_fill = titlebar_fills.get(1).cloned();
                        let first_pre_fill = titlebar_pre_fills.first().cloned();
                        let second_pre_fill = titlebar_pre_fills.get(1).cloned();
                        let first_pre_fill_geometry =
                            first_pre_fill.as_ref().map(|(_, _, _, geometry)| *geometry);
                        let second_pre_fill_geometry = second_pre_fill
                            .as_ref()
                            .map(|(_, _, _, geometry)| *geometry);
                        let fill_frame_key = format!("{}:{}", output.name(), window_id);
                        let previous_fill_state = previous_titlebar_fill_state(
                            &fill_frame_key,
                            TitlebarFillFrameState {
                                first_pre_fill: first_pre_fill_geometry,
                                second_pre_fill: second_pre_fill_geometry,
                            },
                        );
                        let fill_delta = |current: Option<
                            Rectangle<i32, smithay::utils::Physical>,
                        >,
                                          previous: Option<
                            Rectangle<i32, smithay::utils::Physical>,
                        >| {
                            current.zip(previous).map(|(current, previous)| {
                                (
                                    current.loc.x - previous.loc.x,
                                    current.loc.y - previous.loc.y,
                                    current.size.w - previous.size.w,
                                    current.size.h - previous.size.h,
                                )
                            })
                        };
                        let first_pre_fill_delta = fill_delta(
                            first_pre_fill_geometry,
                            previous_fill_state.and_then(|state| state.first_pre_fill),
                        );
                        let second_pre_fill_delta = fill_delta(
                            second_pre_fill_geometry,
                            previous_fill_state.and_then(|state| state.second_pre_fill),
                        );
                        let sibling_gap = |upper: smithay::utils::Rectangle<
                            i32,
                            smithay::utils::Physical,
                        >,
                                           lower: smithay::utils::Rectangle<
                            i32,
                            smithay::utils::Physical,
                        >| {
                            (
                                lower.loc.x - upper.loc.x,
                                lower.loc.y - (upper.loc.y + upper.size.h),
                                (lower.loc.x + lower.size.w) - (upper.loc.x + upper.size.w),
                            )
                        };
                        let shader_to_shader_gap = first_fill
                            .as_ref()
                            .zip(second_fill.as_ref())
                            .map(|((_, _, _, first), (_, _, _, second))| {
                                sibling_gap(*first, *second)
                            });
                        let shader_to_client_gap =
                            second_fill.as_ref().and_then(|(_, _, _, second)| {
                                first_geometry.map(|client| sibling_gap(*second, client))
                            });
                        let fill_client_edge_delta =
                            second_fill.as_ref().and_then(|(_, _, _, fill)| {
                                first_geometry.map(|client| {
                                    (
                                        client.loc.x - fill.loc.x,
                                        client.loc.y - (fill.loc.y + fill.size.h),
                                        (client.loc.x + client.size.w) - (fill.loc.x + fill.size.w),
                                        client.size.w - fill.size.w,
                                    )
                                })
                            });
                        let content_clip_physical =
                            window_decorations.get(window).and_then(|decoration| {
                                let content_clip = decoration.content_clip?;
                                let root_origin = root_physical_origin(
                                    decoration.layout.root.rect,
                                    output_geo,
                                    scale,
                                );
                                let local_geometry = relative_physical_rect_from_root_precise(
                                    content_clip.rect_precise,
                                    decoration.layout.root.rect,
                                    output_geo,
                                    scale,
                                );
                                Some(smithay::utils::Rectangle::new(
                                    smithay::utils::Point::from((
                                        root_origin.x + local_geometry.loc.x,
                                        root_origin.y + local_geometry.loc.y,
                                    )),
                                    local_geometry.size,
                                ))
                            });
                        let frame_key = format!("{}:{}", output.name(), window_id);
                        let previous_client_state = previous_client_frame_state(
                            &frame_key,
                            ClientFrameState {
                                client_geometry: first_geometry,
                                content_clip_physical,
                                fill_client_edge_delta,
                            },
                        );
                        let rect_delta = |current: Option<
                            Rectangle<i32, smithay::utils::Physical>,
                        >,
                                          previous: Option<
                            Rectangle<i32, smithay::utils::Physical>,
                        >| {
                            current.zip(previous).map(|(current, previous)| {
                                (
                                    current.loc.x - previous.loc.x,
                                    current.loc.y - previous.loc.y,
                                    current.size.w - previous.size.w,
                                    current.size.h - previous.size.h,
                                )
                            })
                        };
                        let client_geometry_delta = rect_delta(
                            first_geometry,
                            previous_client_state.and_then(|state| state.client_geometry),
                        );
                        let content_clip_physical_delta = rect_delta(
                            content_clip_physical,
                            previous_client_state.and_then(|state| state.content_clip_physical),
                        );
                        let fill_client_edge_delta_delta = fill_client_edge_delta
                            .zip(
                                previous_client_state
                                    .and_then(|state| state.fill_client_edge_delta),
                            )
                            .map(|(current, previous)| {
                                (
                                    current.0 - previous.0,
                                    current.1 - previous.1,
                                    current.2 - previous.2,
                                    current.3 - previous.3,
                                )
                            });
                        let matching_fill = |ui_key: &str,
                                             fills: &Vec<(
                            usize,
                            String,
                            &'static str,
                            smithay::utils::Rectangle<i32, smithay::utils::Physical>,
                        )>| {
                            fills
                                .iter()
                                .filter_map(|(order, fill_key, source_kind, geometry)| {
                                    let fill_base = fill_key.strip_suffix(":fill")?;
                                    (ui_key.starts_with(fill_base)
                                        && ui_key.as_bytes().get(fill_base.len()) == Some(&b'/'))
                                    .then_some((*order, fill_key.clone(), *source_kind, *geometry))
                                })
                                .max_by_key(|(_, fill_key, _, _)| fill_key.len())
                        };
                        let titlebar_ui_pre_transform_relative = Some(
                            debug_ui_pre_geometries
                                .iter()
                                .filter_map(|(order, key, source_kind, geometry)| {
                                    let (_, fill_key, _, fill) =
                                        matching_fill(key, &titlebar_pre_fills)?;
                                    Some((
                                        *order,
                                        key.clone(),
                                        *source_kind,
                                        fill_key,
                                        (
                                            geometry.loc.x - fill.loc.x,
                                            geometry.loc.y - fill.loc.y,
                                            geometry.size.w - fill.size.w,
                                            geometry.size.h - fill.size.h,
                                        ),
                                    ))
                                })
                                .collect::<Vec<_>>(),
                        );
                        let titlebar_ui_relative = Some(
                            debug_ui_geometries
                                .iter()
                                .filter_map(|(order, key, source_kind, geometry)| {
                                    let (_, fill_key, _, fill) =
                                        matching_fill(key, &titlebar_fills)?;
                                    Some((
                                        *order,
                                        key.clone(),
                                        *source_kind,
                                        fill_key,
                                        (
                                            geometry.loc.x - fill.loc.x,
                                            geometry.loc.y - fill.loc.y,
                                            geometry.size.w - fill.size.w,
                                            geometry.size.h - fill.size.h,
                                        ),
                                    ))
                                })
                                .collect::<Vec<_>>(),
                        );
                        tracing::info!(
                            output = %output.name(),
                            window_id = %window_id,
                            background_pre_geometries = ?debug_background_pre_geometries,
                            background_geometries = ?debug_background_geometries,
                            ui_pre_geometries = ?debug_ui_pre_geometries,
                            ui_geometries = ?debug_ui_geometries,
                            titlebar_pre_fills = ?titlebar_pre_fills,
                            titlebar_fills = ?titlebar_fills,
                            first_pre_fill = ?first_pre_fill,
                            first_pre_fill_delta = ?first_pre_fill_delta,
                            first_fill = ?first_fill,
                            second_pre_fill = ?second_pre_fill,
                            second_pre_fill_delta = ?second_pre_fill_delta,
                            second_fill = ?second_fill,
                            client_geometry = ?first_geometry,
                            client_geometry_delta = ?client_geometry_delta,
                            content_clip_physical = ?content_clip_physical,
                            content_clip_physical_delta = ?content_clip_physical_delta,
                            shader_to_shader_gap = ?shader_to_shader_gap,
                            shader_to_client_gap = ?shader_to_client_gap,
                            fill_client_edge_delta = ?fill_client_edge_delta,
                            fill_client_edge_delta_delta = ?fill_client_edge_delta_delta,
                            titlebar_ui_pre_transform_relative = ?titlebar_ui_pre_transform_relative,
                            titlebar_ui_relative = ?titlebar_ui_relative,
                            "gap debug tty sibling geometry summary"
                        );
                        tracing::info!(
                            output = %output.name(),
                            window_id = %window_id,
                            first_fill = ?first_fill.as_ref().map(|(_, stable_key, _, geometry)| (stable_key, geometry)),
                            second_fill = ?second_fill.as_ref().map(|(_, stable_key, _, geometry)| (stable_key, geometry)),
                            client_geometry = ?first_geometry,
                            client_geometry_delta = ?client_geometry_delta,
                            content_clip_physical = ?content_clip_physical,
                            content_clip_physical_delta = ?content_clip_physical_delta,
                            fill_client_edge_delta = ?fill_client_edge_delta,
                            fill_client_edge_delta_delta = ?fill_client_edge_delta_delta,
                            edge_delta = ?edge_delta,
                            "gap debug tty frame summary"
                        );
                    }
                }
                let transformed = if bypass_clip {
                    window_render::debug_surface_elements(
                        window,
                        &mut backend.renderer,
                        physical_location,
                        scale,
                        visual_state.opacity,
                    );
                    let raw_elements = window_render::surface_elements(
                        window,
                        &mut backend.renderer,
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
                            "gap debug tty raw surface elements"
                        );
                    }
                    let expand_px = std::env::var_os("SHOJI_GAP_EXPAND_RAW_EDGE")
                        .and_then(|value| {
                            value.to_str().and_then(|value| value.parse::<i32>().ok())
                        })
                        .unwrap_or(0)
                        .max(0);
                    if expand_px == 0 {
                        raw_elements
                            .into_iter()
                            .map(TtyRenderElements::Window)
                            .collect()
                    } else {
                        raw_elements
                            .into_iter()
                            .map(|element| {
                                let geometry =
                                    smithay::backend::renderer::element::Element::geometry(
                                        &element, scale,
                                    );
                                let scale_x = (geometry.size.w.saturating_add(expand_px).max(1)
                                    as f64)
                                    / geometry.size.w.max(1) as f64;
                                let scale_y = (geometry.size.h.saturating_add(expand_px).max(1)
                                    as f64)
                                    / geometry.size.h.max(1) as f64;
                                TtyRenderElements::TransformedWindow(
                                    RelocateRenderElement::from_element(
                                        RescaleRenderElement::from_element(
                                            element,
                                            geometry.loc,
                                            smithay::utils::Scale::from((scale_x, scale_y)),
                                        ),
                                        smithay::utils::Point::from((0, 0)),
                                        Relocate::Relative,
                                    ),
                                )
                            })
                            .collect()
                    }
                } else {
                    transform_clipped_elements(clipped, visual_state)
                };
                if std::env::var_os("SHOJI_GAP_READBACK_DEBUG").is_some() {
                    if let Some(first_geometry) = transformed.first().map(|element| {
                        smithay::backend::renderer::element::Element::geometry(element, scale)
                    }) {
                        log_gap_readback_edge_probes(
                            &mut backend.renderer,
                            scale,
                            &transformed,
                            first_geometry,
                            "client",
                            &output.name(),
                            &window_id,
                        );
                    }
                }
                transformed
            } else {
                let surfaces = window_render::surface_elements(
                    window,
                    &mut backend.renderer,
                    physical_location,
                    scale,
                    visual_state.opacity,
                );
                if std::env::var_os("SHOJI_GAP_DEBUG").is_some() {
                    let first_geometry = surfaces.first().map(|element| {
                        smithay::backend::renderer::element::Element::geometry(element, scale)
                    });
                    let window_geometry = window.geometry();
                    let decoration_client_rect = window_decorations
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
                        "gap debug tty raw surface elements"
                    );
                }
                let transformed = transform_window_elements(
                    surfaces,
                    visual_state,
                    TtyRenderElements::Window,
                    TtyRenderElements::TransformedWindow,
                );
                if std::env::var_os("SHOJI_GAP_READBACK_DEBUG").is_some() {
                    if let Some(first_geometry) = transformed.first().map(|element| {
                        smithay::backend::renderer::element::Element::geometry(element, scale)
                    }) {
                        log_gap_readback_edge_probes(
                            &mut backend.renderer,
                            scale,
                            &transformed,
                            first_geometry,
                            "client",
                            &output.name(),
                            &window_id,
                        );
                    }
                }
                transformed
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
            scene_elements.extend(ordered_ui_elements.into_iter().map(|(_, element)| element));
            scene_elements.extend(
                ordered_backdrop_elements
                    .into_iter()
                    .map(|(_, element)| element),
            );

            windows_ready_for_decoration.insert(window_id.clone());

            let should_refresh_snapshot = window_decorations
                .get(window)
                .map(|decoration| {
                    window_has_snapshot_damage
                        || live_window_snapshots
                            .get(&decoration.snapshot.id)
                            .map(|snapshot| snapshot.rect != decoration.client_rect)
                            .unwrap_or(true)
                })
                .unwrap_or(false);
            if std::env::var_os("SHOJI_TRANSFORM_SNAPSHOT_DEBUG").is_some() {
                let live_rect = window_decorations
                    .get(window)
                    .and_then(|decoration| {
                        live_window_snapshots
                            .get(&decoration.snapshot.id)
                            .map(|snapshot| snapshot.rect)
                    });
                let client_rect = window_decorations.get(window).map(|decoration| decoration.client_rect);
                tracing::info!(
                    window_id = %window_id,
                    use_full_window_snapshot,
                    window_has_snapshot_damage,
                    should_refresh_snapshot,
                    live_rect = ?live_rect,
                    client_rect = ?client_rect,
                    "transform snapshot tty refresh decision"
                );
            }
            if should_refresh_snapshot {
                let snapshot_capture_started_at = Instant::now();
                if capture_live_snapshot_for_window(
                    &mut backend.renderer,
                    space,
                    window,
                    window_location,
                    scale,
                    0,
                    window_decorations,
                    live_window_snapshots,
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
                _snapshot_capture_elapsed_ms +=
                    snapshot_capture_started_at.elapsed().as_secs_f64() * 1000.0;
                snapshot_capture_count = snapshot_capture_count.saturating_add(1);
            }
            let window_elapsed_ms = window_started_at.elapsed().as_secs_f64() * 1000.0;
            _window_loop_elapsed_ms += window_elapsed_ms;
            if window_elapsed_ms > max_window_elapsed_ms {
                max_window_elapsed_ms = window_elapsed_ms;
                _max_window_id = Some(window_id);
            }
        }
        let closing_snapshots_started_at = Instant::now();
        scene_elements.extend(
            closing_snapshot_elements(&mut backend.renderer, &closing_snapshots, output_geo, scale)
                .into_iter(),
        );
        let _closing_snapshots_elapsed_ms =
            closing_snapshots_started_at.elapsed().as_secs_f64() * 1000.0;
        let lower_layers_started_at = Instant::now();
        scene_elements.extend(lower_layer_scene_elements(
            &mut backend.renderer,
            &output,
            output_geo,
            scale,
            state.configured_background_effect.as_ref(),
            &state.lower_layer_source_damage,
            state.lower_layer_scene_generation,
            &mut state.layer_backdrop_cache,
        )?);
        let _lower_layers_elapsed_ms = lower_layers_started_at.elapsed().as_secs_f64() * 1000.0;

        let should_profile_damage = should_capture_blink;
        let damage_profile_started_at = Instant::now();
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
        let _damage_profile_elapsed_ms = damage_profile_started_at.elapsed().as_secs_f64() * 1000.0;

        let captured_blink_damage = if should_capture_blink {
            computed_damage
        } else {
            None
        };

        let mut content_elements: Vec<TtyRenderElements> = Vec::new();
        content_elements.extend(
            damage::elements_for_output(&extra_damage, output_geo)
                .into_iter()
                .map(TtyRenderElements::Damage),
        );
        content_elements.extend(scene_elements);

        let cursor_status_for_log = cursor_override
            .map(CursorImageStatus::Named)
            .unwrap_or_else(|| cursor_status.clone());
        let _cursor_element_count = cursor_elements.len();
        let _content_element_count = content_elements.len();
        let mut elements: Vec<TtyRenderElements> = Vec::new();
        elements.extend(
            damage_blink::elements_for_output(&blink_visible, output_geo, scale)
                .into_iter()
                .map(TtyRenderElements::Blink),
        );
        elements.extend(cursor_elements);
        elements.extend(content_elements);

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
        if std::env::var_os("SHOJI_TRANSFORM_SNAPSHOT_DEBUG").is_some()
            && (frame_transform_snapshot_window_count > 0 || frame_had_transform_snapshot_damage)
        {
            tracing::info!(
                output = %output.name(),
                frame_transform_snapshot_window_count,
                frame_had_transform_snapshot_damage,
                extra_damage_count = extra_damage.len(),
                result_is_empty = result.is_empty,
                "transform snapshot tty render result"
            );
        }
        let render_elapsed = render_started_at.elapsed();
        let total_cpu_elapsed = frame_started_at.elapsed();
        surface.estimated_render_duration =
            blend_render_duration(surface.estimated_render_duration, render_elapsed);
        if !result.is_empty {
            trace!(output = %output.name(), "queueing tty frame");
            // Update primary-scanout metadata before collecting presentation feedback.
            //
            // Chrome on the TTY backend would otherwise frequently stick to ~60 fps on a 66 Hz
            // output. Keeping this metadata current made Chrome observe the real output cadence.
            update_primary_scanout_output(
                &state.space,
                &output,
                &cursor_status_for_log,
                &result.states,
            );
            let output_presentation_feedback =
                take_presentation_feedback(&output, &state.space, &result.states);
            surface
                .drm_output
                .queue_frame(Some(output_presentation_feedback))?;
            surface.frame_pending = true;
            surface.queued_at = Some(Instant::now());
            surface.queued_cpu_duration = total_cpu_elapsed;
            surface.skipped_while_pending_count = 0;
            surface.frame_callback_timer_armed = false;
            surface.frame_callback_timer_generation =
                surface.frame_callback_timer_generation.wrapping_add(1);
            surface.frame_callback_sequence = surface.frame_callback_sequence.wrapping_add(1);
            surface.redraw_state = TtyRedrawState::WaitingForVBlank {
                redraw_needed: true,
            };
            let frame_callback_sequence = surface.frame_callback_sequence;
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
    transformed: fn(
        RelocateRenderElement<RescaleRenderElement<WaylandSurfaceRenderElement<GlesRenderer>>>,
    ) -> TtyRenderElements,
) -> Vec<TtyRenderElements> {
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
) -> Vec<TtyRenderElements> {
    if is_identity_visual(visual) {
        return elements
            .into_iter()
            .map(TtyRenderElements::Clipped)
            .collect();
    }

    elements
        .into_iter()
        .map(|element| {
            TtyRenderElements::TransformedClipped(RelocateRenderElement::from_element(
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
) -> Result<Vec<TtyRenderElements>, Box<dyn std::error::Error>> {
    if is_identity_visual(visual) {
        return Ok(elements
            .into_iter()
            .map(|element| {
                TtyRenderElements::RelocatedText(RelocateRenderElement::from_element(
                    element,
                    root_origin,
                    Relocate::Relative,
                ))
            })
            .collect());
    }

    Ok(elements
        .into_iter()
        .map(|element| {
            let relocated =
                RelocateRenderElement::from_element(element, root_origin, Relocate::Relative);
            TtyRenderElements::TransformedText(RelocateRenderElement::from_element(
                RescaleRenderElement::from_element(relocated, visual.origin, visual.scale),
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
        return Ok(elements
            .into_iter()
            .map(TtyRenderElements::Snapshot)
            .collect());
    }

    Ok(elements
        .into_iter()
        .map(|element| {
            TtyRenderElements::TransformedSnapshot(RelocateRenderElement::from_element(
                RescaleRenderElement::from_element(element, visual.origin, visual.scale),
                visual.translation,
                Relocate::Relative,
            ))
        })
        .collect())
}

fn transform_decoration_elements(
    elements: Vec<crate::backend::decoration::DecorationSceneElements>,
    root_origin: Point<i32, smithay::utils::Physical>,
    visual: WindowVisualState,
) -> Result<Vec<TtyRenderElements>, Box<dyn std::error::Error>> {
    if is_identity_visual(visual) {
        return Ok(elements
            .into_iter()
            .map(|element| {
                TtyRenderElements::RelocatedDecoration(RelocateRenderElement::from_element(
                    element,
                    root_origin,
                    Relocate::Relative,
                ))
            })
            .collect());
    }

    Ok(elements
        .into_iter()
        .map(|element| {
            let relocated =
                RelocateRenderElement::from_element(element, root_origin, Relocate::Relative);
            TtyRenderElements::TransformedDecoration(RelocateRenderElement::from_element(
                RescaleRenderElement::from_element(relocated, visual.origin, visual.scale),
                visual.translation,
                Relocate::Relative,
            ))
        })
        .collect())
}

fn transform_backdrop_elements(
    elements: Vec<crate::backend::shader_effect::StableBackdropTextureElement>,
    root_origin: Point<i32, smithay::utils::Physical>,
    visual: WindowVisualState,
) -> Result<Vec<TtyRenderElements>, Box<dyn std::error::Error>> {
    if is_identity_visual(visual) {
        return Ok(elements
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
                        Scale::from((1.0, 1.0)),
                    ))
                } else {
                    None
                };
                let relocated = TtyRenderElements::RelocatedBackdrop(
                    RelocateRenderElement::from_element(element, root_origin, Relocate::Relative),
                );
                if let (Some(debug_label), Some(pre_transform_geometry)) =
                    (debug_label, pre_transform_geometry)
                {
                    let post_transform_geometry =
                        smithay::backend::renderer::element::Element::geometry(
                            &relocated,
                            Scale::from((1.0, 1.0)),
                        );
                    let sample_screen_rect = latest_backdrop_sample_rect(&debug_label);
                    let backdrop_vs_sample_screen = sample_screen_rect.map(|sample| {
                        (
                            post_transform_geometry.loc.x as f64 - sample.0,
                            post_transform_geometry.loc.y as f64 - sample.1,
                            post_transform_geometry.size.w as f64 - sample.2,
                            post_transform_geometry.size.h as f64 - sample.3,
                        )
                    });
                    tracing::info!(
                        backdrop = %debug_label,
                        root_origin = ?root_origin,
                        visual_origin = ?visual.origin,
                        visual_scale = ?visual.scale,
                        visual_translation = ?visual.translation,
                        pre_transform_geometry = ?pre_transform_geometry,
                        post_transform_geometry = ?post_transform_geometry,
                        sample_screen_rect = ?sample_screen_rect,
                        backdrop_vs_sample_screen = ?backdrop_vs_sample_screen,
                        "gap debug tty transformed backdrop geometry"
                    );
                }
                relocated
            })
            .collect());
    }

    Ok(elements
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
                    Scale::from((1.0, 1.0)),
                ))
            } else {
                None
            };
            let relocated =
                RelocateRenderElement::from_element(element, root_origin, Relocate::Relative);
            let transformed = TtyRenderElements::TransformedBackdrop(RelocateRenderElement::from_element(
                RescaleRenderElement::from_element(relocated, visual.origin, visual.scale),
                visual.translation,
                Relocate::Relative,
            ));
            if let (Some(debug_label), Some(pre_transform_geometry)) =
                (debug_label, pre_transform_geometry)
            {
                let post_transform_geometry = smithay::backend::renderer::element::Element::geometry(
                    &transformed,
                    Scale::from((1.0, 1.0)),
                );
                let sample_screen_rect = latest_backdrop_sample_rect(&debug_label);
                let backdrop_vs_sample_screen = sample_screen_rect.map(|sample| {
                    (
                        post_transform_geometry.loc.x as f64 - sample.0,
                        post_transform_geometry.loc.y as f64 - sample.1,
                        post_transform_geometry.size.w as f64 - sample.2,
                        post_transform_geometry.size.h as f64 - sample.3,
                    )
                });
                tracing::info!(
                    backdrop = %debug_label,
                    root_origin = ?root_origin,
                    visual_origin = ?visual.origin,
                    visual_scale = ?visual.scale,
                    visual_translation = ?visual.translation,
                    pre_transform_geometry = ?pre_transform_geometry,
                    post_transform_geometry = ?post_transform_geometry,
                    sample_screen_rect = ?sample_screen_rect,
                    backdrop_vs_sample_screen = ?backdrop_vs_sample_screen,
                    "gap debug tty transformed backdrop geometry"
                );
            }
            transformed
        })
        .collect())
}

fn debug_scene_geometry_snapshot(
    elements: &[TtyRenderElements],
    scale: Scale<f64>,
) -> (
    Option<smithay::utils::Rectangle<i32, smithay::utils::Physical>>,
    Vec<smithay::utils::Rectangle<i32, smithay::utils::Physical>>,
) {
    let geometries = elements
        .iter()
        .map(|element| smithay::backend::renderer::element::Element::geometry(element, scale))
        .collect::<Vec<_>>();

    let union = geometries.iter().copied().reduce(|current, rect| {
        let left = current.loc.x.min(rect.loc.x);
        let top = current.loc.y.min(rect.loc.y);
        let right = (current.loc.x + current.size.w).max(rect.loc.x + rect.size.w);
        let bottom = (current.loc.y + current.size.h).max(rect.loc.y + rect.size.h);
        smithay::utils::Rectangle::new(
            smithay::utils::Point::from((left, top)),
            ((right - left).max(0), (bottom - top).max(0)).into(),
        )
    });

    (union, geometries.into_iter().take(8).collect())
}

fn log_gap_readback_probe(
    renderer: &mut GlesRenderer,
    output_scale: Scale<f64>,
    elements: &[TtyRenderElements],
    probe_rect: smithay::utils::Rectangle<i32, smithay::utils::Physical>,
    side: &str,
    subject: &str,
    output_name: &str,
    window_id: &str,
) {
    if probe_rect.size.w <= 0 || probe_rect.size.h <= 0 || elements.is_empty() {
        return;
    }
    let probe_size = smithay::utils::Size::<i32, smithay::utils::Buffer>::from((
        probe_rect.size.w,
        probe_rect.size.h,
    ));

    let Ok(mut offscreen) =
        Offscreen::<GlesTexture>::create_buffer(renderer, Fourcc::Abgr8888, probe_size)
    else {
        return;
    };
    let Ok(mut framebuffer) = renderer.bind(&mut offscreen) else {
        return;
    };

    let relocated = elements
        .iter()
        .map(|element| {
            RelocateRenderElement::from_element(
                element,
                smithay::utils::Point::from((-probe_rect.loc.x, -probe_rect.loc.y)),
                Relocate::Relative,
            )
        })
        .collect::<Vec<_>>();
    let mut damage_tracker = OutputDamageTracker::new(probe_rect.size, 1.0, Transform::Normal);
    let Ok(_) = damage_tracker.render_output(
        renderer,
        &mut framebuffer,
        0,
        &relocated,
        [0.0, 0.0, 0.0, 0.0],
    ) else {
        return;
    };

    let Ok(mapping) = renderer.copy_framebuffer(
        &framebuffer,
        smithay::utils::Rectangle::from_size(probe_size),
        Fourcc::Abgr8888,
    ) else {
        return;
    };
    let Ok(bytes) = renderer.map_texture(&mapping) else {
        return;
    };

    let mut transparent = 0usize;
    let mut opaque = 0usize;
    let mut min_alpha = u8::MAX;
    let mut max_alpha = 0u8;
    for px in bytes.chunks_exact(4) {
        let alpha = px[3];
        min_alpha = min_alpha.min(alpha);
        max_alpha = max_alpha.max(alpha);
        if alpha == 0 {
            transparent += 1;
        } else {
            opaque += 1;
        }
    }

    tracing::info!(
        output = output_name,
        window_id,
        subject,
        side,
        probe_rect = ?probe_rect,
        output_scale = output_scale.x,
        transparent_pixels = transparent,
        opaque_pixels = opaque,
        min_alpha,
        max_alpha,
        "gap readback tty client edge probe"
    );
}

fn log_gap_readback_edge_probes(
    renderer: &mut GlesRenderer,
    output_scale: Scale<f64>,
    elements: &[TtyRenderElements],
    first_geometry: smithay::utils::Rectangle<i32, smithay::utils::Physical>,
    subject: &str,
    output_name: &str,
    window_id: &str,
) {
    for probe in [1, 2, 4] {
        let probe_width = first_geometry.size.w.min(probe);
        let probe_height = first_geometry.size.h.min(probe);

        let left_probe = smithay::utils::Rectangle::new(
            first_geometry.loc,
            smithay::utils::Size::from((probe_width, first_geometry.size.h)),
        );
        let top_probe = smithay::utils::Rectangle::new(
            first_geometry.loc,
            smithay::utils::Size::from((first_geometry.size.w, probe_height)),
        );
        let right_probe = smithay::utils::Rectangle::new(
            smithay::utils::Point::from((
                first_geometry.loc.x + first_geometry.size.w.saturating_sub(probe_width),
                first_geometry.loc.y,
            )),
            smithay::utils::Size::from((probe_width, first_geometry.size.h)),
        );
        let bottom_probe = smithay::utils::Rectangle::new(
            smithay::utils::Point::from((
                first_geometry.loc.x,
                first_geometry.loc.y + first_geometry.size.h.saturating_sub(probe_height),
            )),
            smithay::utils::Size::from((first_geometry.size.w, probe_height)),
        );

        let left_side = format!("left-{probe}px");
        let top_side = format!("top-{probe}px");
        let right_side = format!("right-{probe}px");
        let bottom_side = format!("bottom-{probe}px");

        log_gap_readback_probe(
            renderer,
            output_scale,
            elements,
            left_probe,
            &left_side,
            subject,
            output_name,
            window_id,
        );
        log_gap_readback_probe(
            renderer,
            output_scale,
            elements,
            top_probe,
            &top_side,
            subject,
            output_name,
            window_id,
        );
        log_gap_readback_probe(
            renderer,
            output_scale,
            elements,
            right_probe,
            &right_side,
            subject,
            output_name,
            window_id,
        );
        log_gap_readback_probe(
            renderer,
            output_scale,
            elements,
            bottom_probe,
            &bottom_side,
            subject,
            output_name,
            window_id,
        );
    }

    for inset in [1, 2, 4, 8] {
        if first_geometry.size.w > inset {
            let right_inset_probe = smithay::utils::Rectangle::new(
                smithay::utils::Point::from((
                    first_geometry.loc.x + first_geometry.size.w - 1 - inset,
                    first_geometry.loc.y,
                )),
                smithay::utils::Size::from((1, first_geometry.size.h)),
            );
            let right_side = format!("right-inset-{inset}px");
            log_gap_readback_probe(
                renderer,
                output_scale,
                elements,
                right_inset_probe,
                &right_side,
                subject,
                output_name,
                window_id,
            );
        }

        if first_geometry.size.h > inset {
            let bottom_inset_probe = smithay::utils::Rectangle::new(
                smithay::utils::Point::from((
                    first_geometry.loc.x,
                    first_geometry.loc.y + first_geometry.size.h - 1 - inset,
                )),
                smithay::utils::Size::from((first_geometry.size.w, 1)),
            );
            let bottom_side = format!("bottom-inset-{inset}px");
            log_gap_readback_probe(
                renderer,
                output_scale,
                elements,
                bottom_inset_probe,
                &bottom_side,
                subject,
                output_name,
                window_id,
            );
        }
    }
}

#[allow(dead_code)]
fn capture_snapshot_from_output_elements(
    renderer: &mut GlesRenderer,
    output_geo: smithay::utils::Rectangle<i32, Logical>,
    rect: crate::ssd::LogicalRect,
    scale: smithay::utils::Scale<f64>,
    existing: Option<crate::backend::snapshot::LiveWindowSnapshot>,
    elements: &[TtyRenderElements],
) -> Result<
    Option<crate::backend::snapshot::LiveWindowSnapshot>,
    smithay::backend::renderer::gles::GlesError,
> {
    let capture_origin: smithay::utils::Point<i32, smithay::utils::Physical> =
        (smithay::utils::Point::from((rect.x, rect.y)) - output_geo.loc)
            .to_f64()
            .to_physical_precise_round(scale);
    let relocated = elements
        .iter()
        .map(|element| {
            RelocateRenderElement::from_element(
                element,
                smithay::utils::Point::from((-capture_origin.x, -capture_origin.y)),
                Relocate::Relative,
            )
        })
        .collect::<Vec<_>>();
    snapshot::capture_snapshot(renderer, existing, rect, 0, true, scale, &relocated)
}

fn backdrop_shader_elements_for_window(
    renderer: &mut GlesRenderer,
    space: &smithay::desktop::Space<smithay::desktop::Window>,
    window_decorations: &mut std::collections::HashMap<
        smithay::desktop::Window,
        crate::ssd::WindowDecorationState,
    >,
    _window_commit_times: &std::collections::HashMap<smithay::desktop::Window, std::time::Duration>,
    window_source_damage: &[crate::state::OwnedDamageRect],
    lower_layer_source_damage: &[crate::state::OwnedDamageRect],
    lower_layer_scene_generation: u64,
    output: &Output,
    output_geo: smithay::utils::Rectangle<i32, Logical>,
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
        let Some(decoration) = window_decorations.get(window) else {
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
        .filter(|cached| cached.shader.is_texture_backed())
        .filter_map(|cached| {
            let cache_key = format!(
                "{}@{}@{}",
                cached.stable_key,
                output.name(),
                if apply_visual_transform {
                    "visual"
                } else {
                    "raw"
                }
            );
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
            let source_effect_rect = crate::backend::visual::transformed_rect(
                cached.rect,
                decoration.layout.root.rect,
                decoration.visual_transform,
            );
            let source_effect_rect_precise = cached
                .rect_precise
                .map(|rect| {
                    crate::backend::visual::transformed_precise_rect(
                        rect,
                        decoration.layout.root.rect,
                        decoration.visual_transform,
                    )
                })
                .unwrap_or_else(|| crate::backend::visual::precise_rect_from_logical(source_effect_rect));
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
                lower_layer_scene_generation.hash(&mut hasher);
            }
            format!("{:?}", cached.shader).hash(&mut hasher);
            let capture_geo = smithay::utils::Rectangle::new(
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
                    space,
                    window_decorations,
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
                smithay::utils::Rectangle::new(
                    smithay::utils::Point::from((source_effect_rect.x, source_effect_rect.y)),
                    (source_effect_rect.width, source_effect_rect.height).into(),
                ),
                &{
                    let mut entries = Vec::new();
                    if uses_backdrop {
                        entries.extend(
                            relevant_source_damage
                                .iter()
                                .filter(|entry| entry.owner.starts_with("window:"))
                                .cloned(),
                        );
                    }
                    if uses_backdrop || uses_xray {
                        entries.extend(
                            relevant_source_damage
                                .iter()
                                .filter(|entry| entry.owner.starts_with("layer:"))
                                .cloned(),
                        );
                    }
                    entries
                },
            );
            let existing_cache = window_decorations
                .get(window)
                .and_then(|d| d.backdrop_cache.get(&cache_key))
                .cloned();

            if !matches!(
                cached.shader.invalidate_policy(),
                crate::ssd::EffectInvalidationPolicy::Always
            ) && !source_damage_hit
            {
                if let Some(existing) = existing_cache
                    .clone()
                    .filter(|existing| existing.signature == signature)
                {
                    let local_rect = smithay::utils::Rectangle::new(
                        smithay::utils::Point::from((
                            display_rect.x - root_rect.x,
                            display_rect.y - root_rect.y,
                        )),
                        (display_rect.width, display_rect.height).into(),
                    );
                    let clip_rect = cached
                        .clip_rect
                        .map(|clip_rect| {
                            let clip = if apply_visual_transform {
                                crate::backend::visual::transformed_rect(
                                    clip_rect,
                                    decoration.layout.root.rect,
                                    decoration.visual_transform,
                                )
                            } else {
                                clip_rect
                            };
                            crate::backend::visual::SnappedLogicalRect {
                                x: (clip.x - display_rect.x) as f32,
                                y: (clip.y - display_rect.y) as f32,
                                width: clip.width.max(0) as f32,
                                height: clip.height.max(0) as f32,
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
                    let local_sample_rect = smithay::utils::Rectangle::new(
                        smithay::utils::Point::from((
                            source_effect_rect.x - output_geo.loc.x,
                            source_effect_rect.y - output_geo.loc.y,
                        )),
                        (source_effect_rect.width, source_effect_rect.height).into(),
                    );
                    let local_capture_rect = local_sample_rect;
                    let sample_region =
                        crate::backend::visual::precise_logical_rect_to_physical_buffer_rect(
                            source_effect_rect_precise,
                            actual_capture_geo.loc,
                            scale,
                        );
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
                    let element =
                        crate::backend::shader_effect::backdrop_shader_element_with_geometry(
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
                            cached.clip_radius,
                            format!(
                                "window-backdrop:{}:{}",
                                decoration.snapshot.id, cached.stable_key
                            ),
                        )
                        .ok()?;
                    if std::env::var_os("SHOJI_GAP_DEBUG").is_some() {
                        let geometry =
                            smithay::backend::renderer::element::Element::geometry(&element, scale);
                        let sample_region_screen = (
                            capture_origin_physical.x as f64 + sample_region.loc.x,
                            capture_origin_physical.y as f64 + sample_region.loc.y,
                            sample_region.size.w,
                            sample_region.size.h,
                        );
                        let backdrop_sample_key =
                            format!("{}:{}:{}", output.name(), decoration.snapshot.id, cached.stable_key);
                        let previous_backdrop_sample =
                            previous_backdrop_sample_state(
                                &backdrop_sample_key,
                                BackdropSampleFrameState {
                                    sample_screen_rect: Some(sample_region_screen),
                                },
                            );
                        let sample_screen_delta = previous_backdrop_sample
                            .and_then(|state| state.sample_screen_rect)
                            .map(|previous| {
                                (
                                    sample_region_screen.0 - previous.0,
                                    sample_region_screen.1 - previous.1,
                                    sample_region_screen.2 - previous.2,
                                    sample_region_screen.3 - previous.3,
                                )
                            });
                        tracing::info!(
                            stable_key = %cached.stable_key,
                            rect = ?cached.rect,
                            display_rect = ?display_rect,
                            local_rect = ?local_rect,
                            local_sample_rect = ?local_sample_rect,
                            local_capture_rect = ?local_capture_rect,
                            sample_region = ?sample_region,
                            sample_region_screen = ?sample_region_screen,
                            sample_screen_delta = ?sample_screen_delta,
                            clip_rect = ?cached.clip_rect,
                            geometry = ?geometry,
                            "gap debug window backdrop element"
                        );
                    }
                    return Some((cached.order, element, render_as_backdrop));
                }
            }
            let mut backdrop_scene: Vec<TtyRenderElements> = Vec::new();
            let backdrop_texture = if uses_backdrop {
                for lower_window in &lower_windows {
                    if let Ok(mut elements) = window_scene_elements_for_capture(
                        renderer,
                        space,
                        window_decorations,
                        output_geo.loc,
                        actual_capture_geo,
                        capture_origin_physical,
                        scale,
                        lower_window,
                    ) {
                        backdrop_scene.append(&mut elements);
                    }
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
                        TtyRenderElements::Window,
                        TtyRenderElements::TransformedWindow,
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
            let mut xray_scene: Vec<TtyRenderElements> = Vec::new();
            let xray_texture = if uses_xray {
                for lower_layer in &lower_layers {
                    if let Ok(mut layer_elements) = layer_surface_scene_elements_for_capture(
                        renderer,
                        output,
                        actual_capture_geo,
                        capture_origin_physical,
                        scale,
                        lower_layer,
                    ) {
                        xray_scene.append(&mut layer_elements);
                    }
                }
                capture_scene_texture_for_effect(renderer, actual_capture_geo, scale, &xray_scene)
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
            let final_backdrop_screen_rect = smithay::utils::Rectangle::new(
                smithay::utils::Point::from((
                    root_origin_physical.x + geometry.loc.x,
                    root_origin_physical.y + geometry.loc.y,
                )),
                geometry.size,
            );
            let sample_region = smithay::utils::Rectangle::new(
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
            if std::env::var_os("SHOJI_GAP_SHADER_READBACK_DEBUG").is_some() {
                crate::backend::shader_effect::log_gap_texture_region_readback(
                    renderer,
                    &input_texture,
                    None,
                    crate::backend::visual::logical_size_to_physical_buffer_size(
                        actual_capture_geo.size.w,
                        actual_capture_geo.size.h,
                        scale,
                    ),
                    "shader-effect-capture-full",
                    &cached.stable_key,
                    &output.name(),
                    &cached.stable_key,
                );
            }
            if std::env::var_os("SHOJI_GAP_DEBUG").is_some() {
                let (backdrop_union, backdrop_first) =
                    debug_scene_geometry_snapshot(&backdrop_scene, scale);
                let (xray_union, xray_first) = debug_scene_geometry_snapshot(&xray_scene, scale);
                tracing::info!(
                    window_id = %decoration.snapshot.id,
                    stable_key = %cached.stable_key,
                    source_effect_rect = ?source_effect_rect,
                    source_effect_rect_precise = ?source_effect_rect_precise,
                    actual_capture_geo = ?actual_capture_geo,
                    capture_origin_physical = ?capture_origin_physical,
                    final_backdrop_screen_rect = ?final_backdrop_screen_rect,
                    sample_region = ?sample_region,
                    backdrop_union = ?backdrop_union,
                    backdrop_first = ?backdrop_first,
                    xray_union = ?xray_union,
                    xray_first = ?xray_first,
                    "gap debug tty backdrop capture scene"
                );
            }
            let output_size = (
                final_backdrop_screen_rect.size.w,
                final_backdrop_screen_rect.size.h,
            );
            if std::env::var_os("SHOJI_GAP_SHADER_READBACK_DEBUG").is_some() {
                crate::backend::shader_effect::log_gap_texture_region_readback(
                    renderer,
                    &input_texture,
                    Some(sample_region),
                    output_size,
                    "shader-effect-input",
                    &cached.stable_key,
                    &output.name(),
                    &cached.stable_key,
                );
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
                Some(sample_region),
                Some(output_size),
                &cached.shader,
            )
            .ok()?;
            if std::env::var_os("SHOJI_GAP_SHADER_READBACK_DEBUG").is_some() {
                crate::backend::shader_effect::log_gap_texture_region_readback(
                    renderer,
                    &texture,
                    None,
                    output_size,
                    "shader-effect-output",
                    &cached.stable_key,
                    &output.name(),
                    &cached.stable_key,
                );
            }
            let commit_counter = window_decorations
                .get(window)
                .and_then(|d| d.backdrop_cache.get(&cache_key))
                .map(|existing| {
                    let mut counter = existing.commit_counter;
                    counter.increment();
                    counter
                })
                .unwrap_or_default();
            if let Some(window_decoration) = window_decorations.get_mut(window) {
                window_decoration.backdrop_cache.insert(
                    cache_key.clone(),
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
                    display_rect.x - root_rect.x,
                    display_rect.y - root_rect.y,
                )),
                (display_rect.width, display_rect.height).into(),
            );
            let clip_rect = cached
                .clip_rect
                .map(|clip_rect| {
                    let clip = if apply_visual_transform {
                        crate::backend::visual::transformed_rect(
                            clip_rect,
                            decoration.layout.root.rect,
                            decoration.visual_transform,
                        )
                    } else {
                        clip_rect
                    };
                    crate::backend::visual::SnappedLogicalRect {
                        x: (clip.x - display_rect.x) as f32,
                        y: (clip.y - display_rect.y) as f32,
                        width: clip.width.max(0) as f32,
                        height: clip.height.max(0) as f32,
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
            let local_sample_rect = smithay::utils::Rectangle::new(
                smithay::utils::Point::from((
                    source_effect_rect.x - output_geo.loc.x,
                    source_effect_rect.y - output_geo.loc.y,
                )),
                (source_effect_rect.width, source_effect_rect.height).into(),
            );
            let local_capture_rect = local_sample_rect;
            let element = crate::backend::shader_effect::backdrop_shader_element_with_geometry(
                renderer,
                window_decorations
                    .get(window)
                    .and_then(|d| d.backdrop_cache.get(&cache_key))
                    .map(|cached| cached.id.clone())
                    .unwrap_or_else(smithay::backend::renderer::element::Id::new),
                window_decorations
                    .get(window)
                    .and_then(|d| d.backdrop_cache.get(&cache_key))
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
                cached.clip_radius,
                format!(
                    "window-backdrop:{}:{}",
                    decoration.snapshot.id, cached.stable_key
                ),
            )
            .ok()?;
            if std::env::var_os("SHOJI_GAP_DEBUG").is_some() {
                let geometry =
                    smithay::backend::renderer::element::Element::geometry(&element, scale);
                let sample_region_screen = (
                    capture_origin_physical.x as f64 + sample_region.loc.x,
                    capture_origin_physical.y as f64 + sample_region.loc.y,
                    sample_region.size.w,
                    sample_region.size.h,
                );
                let backdrop_sample_key =
                    format!("{}:{}:{}", output.name(), decoration.snapshot.id, cached.stable_key);
                let previous_backdrop_sample = previous_backdrop_sample_state(
                    &backdrop_sample_key,
                    BackdropSampleFrameState {
                        sample_screen_rect: Some(sample_region_screen),
                    },
                );
                let sample_screen_delta = previous_backdrop_sample
                    .and_then(|state| state.sample_screen_rect)
                    .map(|previous| {
                        (
                            sample_region_screen.0 - previous.0,
                            sample_region_screen.1 - previous.1,
                            sample_region_screen.2 - previous.2,
                            sample_region_screen.3 - previous.3,
                        )
                    });
                tracing::info!(
                    stable_key = %cached.stable_key,
                    rect = ?cached.rect,
                    display_rect = ?display_rect,
                    local_rect = ?local_rect,
                    local_sample_rect = ?local_sample_rect,
                    local_capture_rect = ?local_capture_rect,
                    sample_region = ?sample_region,
                    sample_region_screen = ?sample_region_screen,
                    sample_screen_delta = ?sample_screen_delta,
                    clip_rect = ?cached.clip_rect,
                    geometry = ?geometry,
                    "gap debug window backdrop element"
                );
            }
            Some((cached.order, element, render_as_backdrop))
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
    let map = layer_map_for_output(output);
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
    window_decorations: &std::collections::HashMap<
        smithay::desktop::Window,
        crate::ssd::WindowDecorationState,
    >,
    windows: impl IntoIterator<Item = smithay::desktop::Window>,
    source_damage: &[crate::state::OwnedDamageRect],
) -> Vec<crate::state::OwnedDamageRect> {
    let owners = windows
        .into_iter()
        .filter_map(|window| {
            window_decorations
                .get(&window)
                .map(|decoration| decoration.snapshot.id.clone())
        })
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

fn logical_rects_intersect(lhs: crate::ssd::LogicalRect, rhs: crate::ssd::LogicalRect) -> bool {
    let left = lhs.x.max(rhs.x);
    let top = lhs.y.max(rhs.y);
    let right = (lhs.x + lhs.width).min(rhs.x + rhs.width);
    let bottom = (lhs.y + lhs.height).min(rhs.y + rhs.height);
    right > left && bottom > top
}

fn contributor_window_scene_rect(
    space: &smithay::desktop::Space<smithay::desktop::Window>,
    window_decorations: &std::collections::HashMap<
        smithay::desktop::Window,
        crate::ssd::WindowDecorationState,
    >,
    window: &smithay::desktop::Window,
) -> Option<(String, crate::ssd::LogicalRect)> {
    if let Some(decoration) = window_decorations.get(window) {
        return Some((
            decoration.snapshot.id.clone(),
            transformed_root_rect(decoration.layout.root.rect, decoration.visual_transform),
        ));
    }
    let location = space.element_location(window)?;
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
    space: &smithay::desktop::Space<smithay::desktop::Window>,
    window_decorations: &std::collections::HashMap<
        smithay::desktop::Window,
        crate::ssd::WindowDecorationState,
    >,
    windows: &[smithay::desktop::Window],
    effect_rect: crate::ssd::LogicalRect,
) {
    for window in windows {
        let Some((window_id, rect)) =
            contributor_window_scene_rect(space, window_decorations, window)
        else {
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
    _capture_geo: smithay::utils::Rectangle<i32, Logical>,
    capture_origin_physical: Point<i32, smithay::utils::Physical>,
    scale: smithay::utils::Scale<f64>,
    layer_surface: &smithay::desktop::LayerSurface,
) -> Result<Vec<TtyRenderElements>, Box<dyn std::error::Error>> {
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
    window_decorations: &mut std::collections::HashMap<
        smithay::desktop::Window,
        crate::ssd::WindowDecorationState,
    >,
    window_source_damage: &[crate::state::OwnedDamageRect],
    lower_layer_source_damage: &[crate::state::OwnedDamageRect],
    lower_layer_scene_generation: u64,
    output: &Output,
    output_geo: smithay::utils::Rectangle<i32, Logical>,
    scale: smithay::utils::Scale<f64>,
    windows_top_to_bottom: &[smithay::desktop::Window],
    layer_surface: &smithay::desktop::LayerSurface,
    alpha: f32,
    layer_backdrop_cache: &mut std::collections::HashMap<
        String,
        crate::backend::shader_effect::CachedBackdropTexture,
    >,
    configured_layer_effects: &std::collections::HashMap<
        String,
        crate::ssd::BackgroundEffectConfig,
    >,
    configured_background_effect: Option<&crate::ssd::BackgroundEffectConfig>,
) -> Result<Vec<TtyRenderElements>, Box<dyn std::error::Error>> {
    let layer_id = crate::ssd::layer_runtime_id(layer_surface);
    let Some(effect_config) = configured_layer_effects
        .get(&layer_id)
        .or(configured_background_effect)
    else {
        return Ok(Vec::new());
    };
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
    let uses_backdrop = effect_config.effect.uses_backdrop_input();
    let uses_xray = effect_config.effect.uses_xray_backdrop_input();
    let relevant_source_damage = {
        let mut entries = Vec::new();
        if uses_backdrop {
            entries.extend(collect_window_source_damage(
                window_decorations,
                windows_top_to_bottom.iter().cloned(),
                window_source_damage,
            ));
        }
        if uses_backdrop || uses_xray {
            entries.extend(collect_layer_source_damage(
                lower_layers.iter().cloned(),
                lower_layer_source_damage,
            ));
        }
        entries
    };
    let backdrop_texture = if effect_config.effect.uses_backdrop_input() {
        let mut backdrop_scene: Vec<TtyRenderElements> = Vec::new();
        for lower_window in windows_top_to_bottom {
            if let Ok(mut window_elements) = window_scene_elements_for_capture(
                renderer,
                space,
                window_decorations,
                output_geo.loc,
                actual_capture_geo,
                capture_origin_physical,
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
                actual_capture_geo,
                capture_origin_physical,
                scale,
                lower_layer,
            ) {
                backdrop_scene.append(&mut layer_elements);
            }
        }
        capture_scene_texture_for_effect(renderer, actual_capture_geo, scale, &backdrop_scene)
    } else {
        None
    };
    let xray_texture = if effect_config.effect.uses_xray_backdrop_input() {
        let mut xray_scene: Vec<TtyRenderElements> = Vec::new();
        for lower_layer in &lower_layers {
            if let Ok(mut layer_elements) = layer_surface_scene_elements_for_capture(
                renderer,
                output,
                actual_capture_geo,
                capture_origin_physical,
                scale,
                lower_layer,
            ) {
                xray_scene.append(&mut layer_elements);
            }
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
        return Ok(Vec::new());
    };
    let texture = crate::backend::shader_effect::apply_effect_pipeline(
        renderer,
        input_texture,
        xray_texture,
        crate::backend::visual::logical_size_to_physical_buffer_size(
            actual_capture_geo.size.w,
            actual_capture_geo.size.h,
            scale,
        ),
        Some(crate::backend::visual::logical_rect_to_physical_buffer_rect_f64(
            effect_rect,
            actual_capture_geo.loc,
            scale,
        )),
        Some(crate::backend::visual::logical_size_to_physical_buffer_size(
            effect_rect.width,
            effect_rect.height,
            scale,
        )),
        &effect_config.effect,
    )?;
    let _captured_local_rect: smithay::utils::Rectangle<i32, smithay::utils::Logical> =
        smithay::utils::Rectangle::new(
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
    if uses_backdrop || uses_xray {
        lower_layer_scene_generation.hash(&mut hasher);
    }
    if uses_backdrop {
        hash_window_scene_contributors(
            &mut hasher,
            space,
            window_decorations,
            windows_top_to_bottom,
            effect_rect,
        );
    }
    if uses_backdrop || uses_xray {
        hash_layer_scene_contributors(&mut hasher, output, &lower_layers, effect_rect);
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
                let rect_key = format!(
                    "{}:{}:{}:{}:{}",
                    stable_key, rect.x, rect.y, rect.width, rect.height
                );
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
        let rect_key = format!(
            "{}:{}:{}:{}:{}",
            stable_key, rect.x, rect.y, rect.width, rect.height
        );
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
        let rect_key = format!(
            "{}:{}:{}:{}:{}",
            stable_key, rect.x, rect.y, rect.width, rect.height
        );
        let rect_local = smithay::utils::Rectangle::new(
            smithay::utils::Point::from((rect.x - output_geo.loc.x, rect.y - output_geo.loc.y)),
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
    lower_layer_scene_generation: u64,
    layer_backdrop_cache: &mut std::collections::HashMap<
        String,
        crate::backend::shader_effect::CachedBackdropTexture,
    >,
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
            lower_layer_scene_generation.hash(&mut hasher);
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
            let actual_capture_geo = capture_geo.intersection(output_geo).unwrap_or(capture_geo);
            let capture_origin_physical =
                crate::backend::visual::logical_point_to_physical_point_global_edges(
                    actual_capture_geo.loc,
                    output_geo.loc,
                    scale,
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
                        let rect_key = format!(
                            "{}:{}:{}:{}:{}",
                            stable_key, rect.x, rect.y, rect.width, rect.height
                        );
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
                    actual_capture_geo,
                    capture_origin_physical,
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
                    actual_capture_geo.loc.x,
                    actual_capture_geo.loc.y,
                    actual_capture_geo.size.w,
                    actual_capture_geo.size.h,
                ),
                0,
                true,
                scale,
                &backdrop_scene,
            )?
            .ok_or("missing backdrop snapshot")?;
            let backdrop_texture = if effect_config.effect.uses_backdrop_input() {
                Some(snapshot.texture.clone())
            } else {
                None
            };
            let xray_texture = if effect_config.effect.uses_xray_backdrop_input() {
                Some(snapshot.texture.clone())
            } else {
                None
            };
            let texture = crate::backend::shader_effect::apply_effect_pipeline(
                renderer,
                backdrop_texture
                    .clone()
                    .or_else(|| xray_texture.clone())
                    .ok_or("missing backdrop snapshot")?,
                xray_texture,
                crate::backend::visual::logical_size_to_physical_buffer_size(
                    actual_capture_geo.size.w,
                    actual_capture_geo.size.h,
                    scale,
                ),
                Some(crate::backend::visual::logical_rect_to_physical_buffer_rect_f64(
                    effect_rect,
                    actual_capture_geo.loc,
                    scale,
                )),
                Some(crate::backend::visual::logical_size_to_physical_buffer_size(
                    effect_rect.width,
                    effect_rect.height,
                    scale,
                )),
                &effect_config.effect,
            )?;
            let mut sub_elements = layer_backdrop_cache
                .get(&stable_key)
                .map(|existing| existing.sub_elements.clone())
                .unwrap_or_default();
            let had_existing = layer_backdrop_cache.contains_key(&stable_key);
            for rect in &rects {
                let rect_key = format!(
                    "{}:{}:{}:{}:{}",
                    stable_key, rect.x, rect.y, rect.width, rect.height
                );
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
                let rect_key = format!(
                    "{}:{}:{}:{}:{}",
                    stable_key, rect.x, rect.y, rect.width, rect.height
                );
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
    window_decorations: &mut std::collections::HashMap<
        smithay::desktop::Window,
        crate::ssd::WindowDecorationState,
    >,
    window_source_damage: &[crate::state::OwnedDamageRect],
    lower_layer_source_damage: &[crate::state::OwnedDamageRect],
    lower_layer_scene_generation: u64,
    configured_layer_effects: &std::collections::HashMap<
        String,
        crate::ssd::BackgroundEffectConfig,
    >,
    configured_background_effect: Option<&crate::ssd::BackgroundEffectConfig>,
    output: &Output,
    output_geo: smithay::utils::Rectangle<i32, Logical>,
    scale: smithay::utils::Scale<f64>,
    windows_top_to_bottom: &[smithay::desktop::Window],
    layer_backdrop_cache: &mut std::collections::HashMap<
        String,
        crate::backend::shader_effect::CachedBackdropTexture,
    >,
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
        elements.extend(configured_background_effect_elements_for_layer(
            renderer,
            space,
            window_decorations,
            window_source_damage,
            lower_layer_source_damage,
            lower_layer_scene_generation,
            output,
            output_geo,
            scale,
            windows_top_to_bottom,
            &layer_surface,
            1.0,
            layer_backdrop_cache,
            configured_layer_effects,
            configured_background_effect,
        )?);
    }
    Ok(elements)
}

fn configured_background_effect_elements_for_window(
    renderer: &mut GlesRenderer,
    space: &smithay::desktop::Space<smithay::desktop::Window>,
    window_decorations: &mut std::collections::HashMap<
        smithay::desktop::Window,
        crate::ssd::WindowDecorationState,
    >,
    _window_commit_times: &std::collections::HashMap<smithay::desktop::Window, std::time::Duration>,
    window_source_damage: &[crate::state::OwnedDamageRect],
    lower_layer_source_damage: &[crate::state::OwnedDamageRect],
    lower_layer_scene_generation: u64,
    output: &Output,
    output_geo: smithay::utils::Rectangle<i32, Logical>,
    scale: smithay::utils::Scale<f64>,
    windows_top_to_bottom: &[smithay::desktop::Window],
    window_index: usize,
    window: &smithay::desktop::Window,
    alpha: f32,
    effect_config: &crate::ssd::BackgroundEffectConfig,
    apply_visual_transform: bool,
) -> Vec<(
    usize,
    crate::backend::shader_effect::StableBackdropTextureElement,
)> {
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

    rects
        .into_iter()
        .enumerate()
        .filter_map(|(index, rect)| {
            let uses_backdrop = effect_config.effect.uses_backdrop_input();
            let uses_xray = effect_config.effect.uses_xray_backdrop_input();
            let stable_key = format!("__protocol_background_effect_{}", index);
            let cache_key = format!("{}@{}", stable_key, output.name());
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
            if uses_backdrop || uses_xray {
                lower_layer_scene_generation.hash(&mut hasher);
            }
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
            if uses_backdrop {
                hash_window_scene_contributors(
                    &mut hasher,
                    space,
                    window_decorations,
                    &lower_windows,
                    effect_rect,
                );
            }
            if uses_backdrop || uses_xray {
                hash_layer_scene_contributors(&mut hasher, output, &lower_layers, effect_rect);
            }
            let signature = hasher.finish();
            let source_damage_hit = crate::backend::shader_effect::source_damage_intersects_rect(
                &effect_config.effect,
                smithay::utils::Rectangle::new(
                    smithay::utils::Point::from((effect_rect.x, effect_rect.y)),
                    (effect_rect.width, effect_rect.height).into(),
                ),
                &{
                    let mut entries = Vec::new();
                    if uses_backdrop {
                        entries.extend(collect_window_source_damage(
                            window_decorations,
                            lower_windows.iter().cloned(),
                            window_source_damage,
                        ));
                    }
                    if uses_backdrop || uses_xray {
                        entries.extend(collect_layer_source_damage(
                            lower_layers.iter().cloned(),
                            lower_layer_source_damage,
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
                effect_config.effect.invalidate_policy(),
                crate::ssd::EffectInvalidationPolicy::Always
            ) && !source_damage_hit
            {
                if let Some(existing) = window_decorations
                    .get(window)
                    .and_then(|d| d.backdrop_cache.get(&cache_key))
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

            let backdrop_texture = if uses_backdrop {
                let mut backdrop_scene: Vec<TtyRenderElements> = Vec::new();
                for lower_window in &lower_windows {
                    if let Ok(mut elements) = window_scene_elements_for_capture(
                        renderer,
                        space,
                        window_decorations,
                        output_geo.loc,
                        actual_capture_geo,
                        capture_origin_physical,
                        scale,
                        lower_window,
                    ) {
                        backdrop_scene.append(&mut elements);
                    }
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
                        TtyRenderElements::Window,
                        TtyRenderElements::TransformedWindow,
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
                let mut xray_scene: Vec<TtyRenderElements> = Vec::new();
                for lower_layer in &lower_layers {
                    if let Ok(mut layer_elements) = layer_surface_scene_elements_for_capture(
                        renderer,
                        output,
                        actual_capture_geo,
                        capture_origin_physical,
                        scale,
                        lower_layer,
                    ) {
                        xray_scene.append(&mut layer_elements);
                    }
                }
                capture_scene_texture_for_effect(renderer, actual_capture_geo, scale, &xray_scene)
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
            if std::env::var_os("SHOJI_GAP_SHADER_READBACK_DEBUG").is_some() {
                crate::backend::shader_effect::log_gap_texture_region_readback(
                    renderer,
                    &input_texture,
                    Some(sample_region),
                    output_size,
                    "shader-effect-input",
                    &cache_key,
                    &output.name(),
                    &cache_key,
                );
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
                Some(sample_region),
                Some(output_size),
                &effect_config.effect,
            )
            .ok()?;
            if std::env::var_os("SHOJI_GAP_SHADER_READBACK_DEBUG").is_some() {
                crate::backend::shader_effect::log_gap_texture_region_readback(
                    renderer,
                    &texture,
                    None,
                    output_size,
                    "shader-effect-output",
                    &cache_key,
                    &output.name(),
                    &cache_key,
                );
            }
            let commit_counter = window_decorations
                .get(window)
                .and_then(|d| d.backdrop_cache.get(&cache_key))
                .map(|existing| {
                    let mut counter = existing.commit_counter;
                    counter.increment();
                    counter
                })
                .unwrap_or_default();
            if let Some(window_decoration) = window_decorations.get_mut(window) {
                window_decoration.backdrop_cache.insert(
                    cache_key.clone(),
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
                    .and_then(|d| d.backdrop_cache.get(&cache_key))
                    .map(|cached| cached.id.clone())
                    .unwrap_or_else(smithay::backend::renderer::element::Id::new),
                window_decorations
                    .get(window)
                    .and_then(|d| d.backdrop_cache.get(&cache_key))
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
    window_decorations: &std::collections::HashMap<
        smithay::desktop::Window,
        crate::ssd::WindowDecorationState,
    >,
    output_origin: Point<i32, Logical>,
    capture_geo: smithay::utils::Rectangle<i32, Logical>,
    capture_origin_physical: Point<i32, smithay::utils::Physical>,
    scale: smithay::utils::Scale<f64>,
    window: &smithay::desktop::Window,
) -> Result<Vec<TtyRenderElements>, Box<dyn std::error::Error>> {
    let Some(window_location) = space.element_location(window) else {
        return Ok(Vec::new());
    };
    let physical_location =
        crate::backend::visual::logical_point_to_relative_physical_point_from_origin(
            window_location,
            output_origin,
            capture_origin_physical,
            scale,
        );
    let visual_state = window_decorations
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

    if let Some(decoration) = window_decorations.get(window) {
        let root_origin = crate::backend::visual::logical_point_to_relative_physical_point_from_origin(
            Point::from((decoration.layout.root.rect.x, decoration.layout.root.rect.y)),
            output_origin,
            capture_origin_physical,
            scale,
        );
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
                    transform_decoration_elements(vec![element], root_origin, visual_state)?
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
                    transform_text_elements(vec![element], root_origin, visual_state)?
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
                    transform_text_elements(vec![element], root_origin, visual_state)?
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
    _space: &smithay::desktop::Space<smithay::desktop::Window>,
    window: &smithay::desktop::Window,
    window_location: smithay::utils::Point<i32, Logical>,
    scale: smithay::utils::Scale<f64>,
    z_index: usize,
    window_decorations: &mut std::collections::HashMap<
        smithay::desktop::Window,
        crate::ssd::WindowDecorationState,
    >,
    live_window_snapshots: &mut std::collections::HashMap<
        String,
        crate::backend::snapshot::LiveWindowSnapshot,
    >,
) -> Result<(), smithay::backend::renderer::gles::GlesError> {
    let Some((snapshot_id, client_rect)) = window_decorations
        .get(window)
        .map(|decoration| (decoration.snapshot.id.clone(), decoration.client_rect))
    else {
        return Ok(());
    };
    let snapshot_geo = smithay::utils::Rectangle::new(
        smithay::utils::Point::from((client_rect.x, client_rect.y)),
        (client_rect.width, client_rect.height).into(),
    );
    let physical_location = (window_location - snapshot_geo.loc).to_physical_precise_round(scale);

    let surface_elements =
        window_render::surface_elements(window, renderer, physical_location, scale, 1.0);
    let has_client_content = !surface_elements.is_empty();
    let elements = surface_elements
        .into_iter()
        .map(TtyRenderElements::Window)
        .collect::<Vec<_>>();

    let existing = live_window_snapshots.remove(&snapshot_id);
    if let Some(snapshot) = snapshot::capture_snapshot(
        renderer,
        existing,
        client_rect,
        z_index,
        has_client_content,
        scale,
        &elements,
    )? {
        live_window_snapshots.insert(snapshot_id.clone(), snapshot);
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
                if let Ok(transformed) = transform_text_elements(icon_elements, root_origin, visual)
                {
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
                if let Ok(transformed) = transform_text_elements(text_elements, root_origin, visual)
                {
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
                if let Ok(transformed) =
                    transform_decoration_elements(background_elements, root_origin, visual)
                {
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
    let output_name = format!(
        "{}-{}",
        connector.interface().as_str(),
        connector.interface_id()
    );
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
        available_modes: connector.modes().to_vec(),
        blink_damage_tracker: OutputDamageTracker::from_output(&output),
        frame_pending: false,
        queued_at: None,
        queued_cpu_duration: Duration::ZERO,
        skipped_while_pending_count: 0,
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
    if let Some(surface) = backend.surfaces.get_mut(&crtc) {
        surface.redraw_state = TtyRedrawState::Queued;
    }
    state.apply_runtime_display_configuration();
    state.schedule_redraw();
    Ok(())
}

pub fn tty_output_available_modes(
    state: &crate::state::ShojiWM,
    output_name: &str,
) -> Option<Vec<WlMode>> {
    for backend in state.tty_backends.values() {
        for surface in backend.surfaces.values() {
            if surface.output.name() == output_name {
                return Some(
                    surface
                        .available_modes
                        .iter()
                        .copied()
                        .map(WlMode::from)
                        .collect(),
                );
            }
        }
    }
    None
}

pub fn apply_tty_output_mode(
    state: &mut crate::state::ShojiWM,
    output_name: &str,
    mode: WlMode,
) -> Result<bool, Box<dyn std::error::Error>> {
    for backend in state.tty_backends.values_mut() {
        for surface in backend.surfaces.values_mut() {
            if surface.output.name() != output_name {
                continue;
            }
            let Some(drm_mode) = surface.available_modes.iter().copied().find(|candidate| {
                let candidate_mode = WlMode::from(*candidate);
                candidate_mode.size == mode.size && candidate_mode.refresh == mode.refresh
            }) else {
                return Ok(false);
            };
            surface
                .drm_output
                .use_mode::<GlesRenderer, WaylandSurfaceRenderElement<GlesRenderer>>(
                    drm_mode,
                    &mut backend.renderer,
                    &DrmOutputRenderElements::default(),
                )?;
            surface.frame_duration = Duration::from_secs_f64(1_000f64 / mode.refresh as f64);
            surface.redraw_state = TtyRedrawState::Queued;
            return Ok(true);
        }
    }
    Ok(false)
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
                if std::env::var_os("SHOJI_TRANSFORM_SNAPSHOT_DEBUG").is_some() {
                    tracing::info!(
                        output = %output.name(),
                        queued = true,
                        generation,
                        callback_time = ?callback_time,
                        "transform snapshot tty estimated vblank fired"
                    );
                }
                state.schedule_redraw();
            } else {
                if std::env::var_os("SHOJI_TRANSFORM_SNAPSHOT_DEBUG").is_some() {
                    tracing::info!(
                        output = %output.name(),
                        queued = false,
                        generation,
                        callback_time = ?callback_time,
                        "transform snapshot tty estimated vblank fired"
                    );
                }
                state.send_frame_callbacks_for_output(&output, callback_time, Some(sequence));
                state.signal_post_repaint_barriers(&output);
            }
            TimeoutAction::Drop
        })
        .is_err()
    {
        surface.frame_callback_timer_armed = false;
        warn!(
            ?node,
            ?crtc,
            "failed to schedule tty estimated vblank callback"
        );
    }
}

fn blend_render_duration(previous: Duration, current: Duration) -> Duration {
    if previous.is_zero() {
        return current;
    }

    Duration::from_secs_f64(previous.as_secs_f64() * 0.75 + current.as_secs_f64() * 0.25)
}
