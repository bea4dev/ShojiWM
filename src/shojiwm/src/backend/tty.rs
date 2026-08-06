use std::hash::{DefaultHasher, Hash, Hasher};
use std::{
    collections::HashMap,
    os::fd::AsRawFd,
    path::Path,
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use smithay::{
    backend::{
        allocator::{Fourcc, format::FormatSet, gbm::GbmAllocator},
        drm::{
            DrmDevice,
            DrmDeviceFd, 
            DrmError,
            DrmEvent,
            DrmEventMetadata,
            DrmEventTime,
            DrmNode,
            compositor::{FrameFlags, PrimaryPlaneElement},
            exporter::gbm::{GbmFramebufferExporter, NodeFilter},
            output::{DrmOutput, DrmOutputManager, DrmOutputRenderElements},
        },
        egl::{EGLContext, EGLDisplay, context::ContextPriority},
        renderer::{
            Bind, Color32F, ExportMem, ImportDma, ImportEgl, ImportMemWl, Offscreen, Renderer,
            Texture,
            damage::OutputDamageTracker,
            element::{
                AsRenderElements, Element, Id, Kind, RenderElement, RenderElementPresentationState,
                RenderElementStates, RenderingReason, UnderlyingStorage,
                memory::MemoryRenderBuffer,
                solid::SolidColorRenderElement,
                surface::WaylandSurfaceRenderElement,
                texture::TextureRenderElement,
                utils::{
                    Relocate, RelocateRenderElement, RescaleRenderElement, select_dmabuf_feedback,
                },
            },
            gles::{GlesError, GlesFrame, GlesRenderer, GlesTexture},
            utils::{CommitCounter, DamageSet, OpaqueRegions},
        },
        session::{Session, libseat::LibSeatSession},
    },
    desktop::{layer_map_for_output, utils::send_dmabuf_feedback_surface_tree},
    input::pointer::{CursorImageAttributes, CursorImageStatus},
    output::{Mode as WlMode, Output, PhysicalProperties},
    reexports::{
        calloop::{
            LoopHandle,
            timer::{TimeoutAction, Timer},
        },
        drm::control::{ModeTypeFlags, connector, crtc},
        gbm::{BufferObjectFlags, Device, Format},
        rustix::fs::OFlags,
        wayland_protocols::wp::{
            linux_dmabuf::zv1::server::zwp_linux_dmabuf_feedback_v1::TrancheFlags,
            presentation_time::server::wp_presentation_feedback,
        },
        wayland_server::Resource,
    },
    render_elements,
    utils::{
        Buffer, DeviceFd, IsAlive, Logical, Monotonic, Physical, Point, Rectangle, Scale, Size,
        Transform, user_data::UserDataMap,
    },
    wayland::{
        background_effect::BackgroundEffectSurfaceCachedState,
        compositor,
        dmabuf::{DmabufFeedback, DmabufFeedbackBuilder},
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
        WindowVisualState, is_identity_visual_geometry, requires_full_window_snapshot,
        root_physical_origin, transformed_rect, transformed_root_rect, window_visual_state,
    },
    backend::window as window_render,
    config::DisplayModePreference,
    drawing::PointerRenderElement,
    presentation::{take_presentation_feedback, update_primary_scanout_output},
    ssd::{EffectInput, WindowSourceInclude},
    state::ShojiWM,
};
use smithay::wayland::presentation::Refresh;

const CLEAR_COLOR: [f32; 4] = [0.08, 0.10, 0.13, 1.0];
// Keep hardware cursor updates on the cursor plane, but force window/layer content through
// the compositor for now.
//
// Keep primary, overlay, and cursor plane scanout enabled. Unlike niri's
// default, overlay assignment measurably improves performance on the target
// NVIDIA system.
//
// The earlier worry that primary scanout would skip our SSD/decorations is
// handled by smithay's `DrmCompositor`: plane assignment only picks the
// primary plane when a single element can realize the whole frame, i.e.
// nothing else (no decorations, no layer-shell, no shader effects) needs to
// be drawn on top. Multi-element frames fall back to the GL compositing
// path automatically, so decorations cannot disappear under this flag.
const TTY_FRAME_FLAGS: FrameFlags = FrameFlags::ALLOW_PRIMARY_PLANE_SCANOUT_ANY
    .union(FrameFlags::ALLOW_OVERLAY_PLANE_SCANOUT)
    .union(FrameFlags::ALLOW_CURSOR_PLANE_SCANOUT);

fn frame_liveness_debug_enabled() -> bool {
    std::env::var_os("SHOJI_FRAME_LIVENESS_DEBUG")
        .is_some_and(|value| value != "0" && !value.is_empty())
}

fn output_render_debug_enabled() -> bool {
    std::env::var_os("SHOJI_OUTPUT_RENDER_DEBUG")
        .is_some_and(|value| value != "0" && !value.is_empty())
}

fn overlay_planes_disabled() -> bool {
    std::env::var_os("SHOJI_DISABLE_OVERLAY_PLANES")
        .is_some_and(|value| value != "0" && !value.is_empty())
}

fn direct_scanout_debug_enabled() -> bool {
    std::env::var_os("SHOJI_DIRECT_SCANOUT_DEBUG")
        .is_some_and(|value| value != "0" && !value.is_empty())
}

fn direct_scanout_debug_log_allowed(output_name: &str) -> bool {
    static LAST_LOGGED: OnceLock<Mutex<HashMap<String, Instant>>> = OnceLock::new();
    let Ok(mut last_logged) = LAST_LOGGED
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
    else {
        return false;
    };
    let now = Instant::now();
    if last_logged
        .get(output_name)
        .is_some_and(|last| now.duration_since(*last) < Duration::from_secs(1))
    {
        return false;
    }
    last_logged.insert(output_name.to_string(), now);
    true
}

fn window_effect_debug_enabled() -> bool {
    std::env::var_os("SHOJI_WINDOW_EFFECT_DEBUG")
        .is_some_and(|value| value != "0" && !value.is_empty())
}

fn managed_rect_debug_enabled() -> bool {
    std::env::var_os("SHOJI_MANAGED_RECT_DEBUG")
        .is_some_and(|value| value != "0" && !value.is_empty())
}

fn kinetic_scroll_trace_debug_enabled() -> bool {
    std::env::var_os("SHOJI_KINETIC_SCROLL_TRACE")
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

fn animation_gap_debug_enabled() -> bool {
    std::env::var_os("SHOJI_ANIMATION_GAP_DEBUG")
        .is_some_and(|value| value != "0" && !value.is_empty())
}

fn animation_gap_threshold_ms() -> f64 {
    std::env::var("SHOJI_ANIMATION_GAP_THRESHOLD_MS")
        .ok()
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|value| *value > 0.0)
        .unwrap_or(80.0)
}

fn browser_cpu_debug_enabled() -> bool {
    std::env::var_os("SHOJI_BROWSER_CPU_DEBUG")
        .is_some_and(|value| value != "0" && !value.is_empty())
}

fn mpv_frame_debug_enabled() -> bool {
    std::env::var_os("SHOJI_MPV_FRAME_DEBUG").is_some_and(|value| value != "0" && !value.is_empty())
}

fn sanitize_next_frame_target(
    next_frame_target: Option<Duration>,
    fallback_frame_time: Duration,
    frame_duration: Duration,
) -> (Duration, bool) {
    let stale_before = fallback_frame_time
        .checked_sub(frame_duration)
        .unwrap_or(Duration::ZERO);
    match next_frame_target {
        Some(target) if target < stale_before => (fallback_frame_time, true),
        Some(target) => (target, false),
        None => (fallback_frame_time, false),
    }
}

pub(super) fn error_chain_has_permission_denied(
    error: &(dyn std::error::Error + 'static),
) -> bool {
    let mut current = Some(error);
    while let Some(error) = current {
        if error
            .downcast_ref::<std::io::Error>()
            .is_some_and(|source| source.kind() == std::io::ErrorKind::PermissionDenied)
        {
            return true;
        }
        current = error.source();
    }
    false
}

/// Whether a render/commit error chain bottoms out in a failed DRM atomic
/// test (`DrmError::TestFailed`). That means the kernel rejected the screen
/// configuration itself — expected after suspend/resume when the connector
/// topology changed during sleep — and the surface should be rebuilt rather
/// than treated as a fatal render failure.
fn error_chain_has_drm_test_failed(error: &(dyn std::error::Error + 'static)) -> bool {
    let mut current = Some(
        error,
    );
    while let Some(
        error,
    ) = current {
        if matches!(
            error.downcast_ref::<DrmError>(),
            Some(
                DrmError::TestFailed(_),
            )
        ) {
            return true;
        }
        current = error
            .source();
    }
    false
}

fn browser_cpu_debug_allowed(output_name: &str) -> bool {
    if !browser_cpu_debug_enabled() {
        return false;
    }
    static STATE: OnceLock<Mutex<HashMap<String, Instant>>> = OnceLock::new();
    let state = STATE.get_or_init(|| Mutex::new(HashMap::new()));
    let Ok(mut guard) = state.lock() else {
        return false;
    };
    let now = Instant::now();
    let should_log = guard.get(output_name).is_none_or(|previous| {
        now.saturating_duration_since(*previous) >= Duration::from_millis(250)
    });
    if should_log {
        guard.insert(output_name.to_string(), now);
    }
    should_log
}

fn output_has_visible_x11_chrome(state: &ShojiWM, output: &Output) -> bool {
    state.space.elements_for_output(output).any(|window| {
        window
            .x11_surface()
            .map(|x11| x11.class().to_ascii_lowercase())
            .is_some_and(|class| {
                class == "google-chrome" || class.contains("chromium") || class.contains("chrome")
            })
    })
}

fn output_has_visible_mpv(state: &ShojiWM, output: &Output) -> bool {
    state.space.elements_for_output(output).any(|window| {
        state
            .window_decorations
            .get(window)
            .and_then(|decoration| decoration.snapshot.app_id.as_deref())
            == Some("mpv")
    })
}

fn record_animation_gap(label: &str, key: &str, now: Instant) -> Option<f64> {
    static STATE: OnceLock<Mutex<HashMap<String, Instant>>> = OnceLock::new();
    let state = STATE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = state.lock().ok()?;
    let map_key = format!("{label}:{key}");
    let previous = guard.insert(map_key, now);
    previous.map(|previous| now.saturating_duration_since(previous).as_secs_f64() * 1000.0)
}

fn clipped_transform_debug_enabled() -> bool {
    std::env::var_os("SHOJI_CLIPPED_TRANSFORM_DEBUG")
        .is_some_and(|value| value != "0" && !value.is_empty())
}

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

/// Stores the visual transform for a snapshot window from the previous render frame and returns
/// the previous value. Used to detect whether the transform is actively changing (animation
/// in progress) vs. stable (stationary snapshot window that should be throttled).
///
/// The key is `snapshot_id + ":" + output_name` so that each output independently tracks
/// the previous transform.  Without the output suffix, a multi-output setup would have the
/// second output to render always see "unchanged" because the first output already stored the
/// current-frame value into the shared slot.
fn previous_snapshot_visual_transform(
    snapshot_id: &str,
    output_name: &str,
    current: crate::ssd::WindowTransform,
) -> Option<crate::ssd::WindowTransform> {
    static STATE: OnceLock<Mutex<HashMap<String, crate::ssd::WindowTransform>>> = OnceLock::new();
    let state = STATE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = state.lock().ok()?;
    let key = format!("{snapshot_id}:{output_name}");
    guard.insert(key, current)
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

fn direct_scanout_state_map() -> &'static Mutex<HashMap<String, bool>> {
    static STATE: OnceLock<Mutex<HashMap<String, bool>>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn fullscreen_fast_path_state_map() -> &'static Mutex<HashMap<String, bool>> {
    static STATE: OnceLock<Mutex<HashMap<String, bool>>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn overlay_plane_state_map() -> &'static Mutex<HashMap<String, usize>> {
    static STATE: OnceLock<Mutex<HashMap<String, usize>>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Edge-triggered log of overlay plane assignments (SHOJI_DIRECT_SCANOUT_DEBUG).
/// Overlay promotion of translucent client surfaces is a suspect for
/// "transparent regions render black / flicker, but screencopy looks fine",
/// so log which elements sit on overlay planes whenever the count changes.
fn note_overlay_plane_transition(output_name: &str, count: usize, details: &[String]) {
    let Ok(mut guard) = overlay_plane_state_map().lock() else {
        return;
    };
    let previous = guard.insert(output_name.to_string(), count);
    if previous == Some(count) {
        return;
    }
    tracing::info!(
        output = %output_name,
        overlay_plane_count = count,
        overlay_elements = ?details,
        "overlay plane assignment changed"
    );
}

/// Logs fullscreen-fast-path transitions edge-triggered. Separate from the
/// scanout-plane log so we can distinguish "fast path engaged but the buffer
/// was not promoted to a plane (e.g. shm or unsupported modifier)" from "fast
/// path never engaged".
fn note_fullscreen_fast_path_transition(output_name: &str, active: bool) {
    let Ok(mut guard) = fullscreen_fast_path_state_map().lock() else {
        return;
    };
    let previous = guard.insert(output_name.to_string(), active);
    if previous == Some(active) {
        return;
    }
    if active {
        tracing::info!(
            output = %output_name,
            "fullscreen fast path engaged: scene collapsed to overlay layers + bare client surface"
        );
    } else {
        tracing::info!(
            output = %output_name,
            "fullscreen fast path disengaged: full scene compositing resumed"
        );
    }
}

/// Logs direct-scanout transitions edge-triggered: one line when a client
/// buffer first lands on the primary plane and one line when compositing
/// resumes, instead of spamming per frame.
fn note_direct_scanout_transition(
    output_name: &str,
    active: bool,
    fullscreen_fast_path: bool,
    fullscreen_root_buffer: Option<&str>,
) {
    let Ok(mut guard) = direct_scanout_state_map().lock() else {
        return;
    };
    let previous = guard.insert(output_name.to_string(), active);
    if previous == Some(active) {
        return;
    }
    if active {
        tracing::info!(
            output = %output_name,
            fullscreen_fast_path,
            fullscreen_root_buffer,
            "direct scanout engaged: client buffer assigned to primary plane (zero-copy)"
        );
    } else {
        tracing::info!(
            output = %output_name,
            fullscreen_fast_path,
            fullscreen_root_buffer,
            "direct scanout disengaged: GL compositing resumed"
        );
    }
}

fn describe_underlying_storage(storage: Option<UnderlyingStorage<'_>>) -> String {
    let Some(storage) = storage else {
        return "none".to_string();
    };
    match storage {
        UnderlyingStorage::Wayland(buffer) => {
            let wl_buffer_id = buffer.id();
            let Ok(dmabuf) = smithay::wayland::dmabuf::get_dmabuf(buffer) else {
                return format!("wayland wl_buffer={wl_buffer_id:?} storage=non-dmabuf");
            };
            let mut hasher = DefaultHasher::new();
            dmabuf.hash(&mut hasher);
            let dmabuf_id = hasher.finish();
            let size = smithay::backend::allocator::Buffer::size(dmabuf);
            let format = smithay::backend::allocator::Buffer::format(dmabuf);
            let handles = dmabuf
                .handles()
                .map(|handle| handle.as_raw_fd())
                .collect::<Vec<_>>();
            let offsets = dmabuf.offsets().collect::<Vec<_>>();
            let strides = dmabuf.strides().collect::<Vec<_>>();
            format!(
                "wayland wl_buffer={wl_buffer_id:?} dmabuf_id={dmabuf_id:#018x} \
                 size={size:?} fourcc={:?} modifier={:?} planes={} fds={handles:?} \
                 offsets={offsets:?} strides={strides:?} y_inverted={} node={:?}",
                format.code,
                format.modifier,
                dmabuf.num_planes(),
                dmabuf.y_inverted(),
                dmabuf.node(),
            )
        }
        UnderlyingStorage::Memory(memory) => format!("memory {memory:?}"),
    }
}

/// When set, forces the fullscreen tearing fast path on regardless of the
/// client's `wp_tearing_control` hint. Intended for testing tearing on clients
/// (or XWayland proxies) that don't advertise the hint.
fn tearing_force_enabled() -> bool {
    std::env::var_os("SHOJI_FORCE_TEARING").is_some_and(|value| value != "0" && !value.is_empty())
}

#[allow(clippy::type_complexity)]
fn present_rate_state_map() -> &'static Mutex<HashMap<String, (u32, Instant)>> {
    static STATE: OnceLock<Mutex<HashMap<String, (u32, Instant)>>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Counts presents per output and logs the rate roughly once per second when
/// `SHOJI_PRESENT_RATE_DEBUG` is set. Lets us see the actual on-screen present
/// rate (vs. the output mode's refresh) to tell apart "compositor presents at
/// 60 Hz" from "client renders at 60 fps".
fn note_present_rate(output_name: &str) {
    if !std::env::var_os("SHOJI_PRESENT_RATE_DEBUG")
        .is_some_and(|value| value != "0" && !value.is_empty())
    {
        return;
    }
    let Ok(mut guard) = present_rate_state_map().lock() else {
        return;
    };
    let now = Instant::now();
    let entry = guard.entry(output_name.to_string()).or_insert((0, now));
    entry.0 += 1;
    let elapsed = now.duration_since(entry.1);
    if elapsed >= Duration::from_secs(1) {
        let rate = entry.0 as f64 / elapsed.as_secs_f64();
        tracing::info!(
            output = %output_name,
            present_rate_hz = rate,
            presents = entry.0,
            window_ms = elapsed.as_secs_f64() * 1000.0,
            "present rate"
        );
        entry.0 = 0;
        entry.1 = now;
    }
}

fn tearing_state_map() -> &'static Mutex<HashMap<String, bool>> {
    static STATE: OnceLock<Mutex<HashMap<String, bool>>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Logs tearing-path transitions edge-triggered: one line when a frame is first
/// submitted as an immediate (async) page flip and one when synced flips resume.
fn note_tearing_transition(output_name: &str, active: bool, forced: bool) {
    let Ok(mut guard) = tearing_state_map().lock() else {
        return;
    };
    if guard.insert(output_name.to_string(), active) == Some(active) {
        return;
    }
    if active {
        tracing::info!(
            output = %output_name,
            forced,
            "tearing engaged: submitting immediate (async) page flips on the fullscreen surface"
        );
    } else {
        tracing::info!(
            output = %output_name,
            "tearing disengaged: synced (vblank) page flips resumed"
        );
    }
}

/// Detects the window that should take the fullscreen fast path on this
/// output: the topmost rendered window carries the client-acked xdg
/// Fullscreen state, its committed geometry covers the whole output, and no
/// compositor-side visual transform or fade is in flight.
///
/// While this holds, scene assembly collapses to "overlay layers above one
/// raw client surface" (Top/Bottom/Background layers, other windows,
/// decorations and effects are all occluded and skipped). With nothing else
/// in the element list, smithay's DrmCompositor can promote the client's
/// dmabuf straight to the primary plane — zero-copy direct scanout.
pub(crate) fn fullscreen_scanout_window(
    space: &smithay::desktop::Space<smithay::desktop::Window>,
    window_decorations: &std::collections::HashMap<
        smithay::desktop::Window,
        crate::ssd::WindowDecorationState,
    >,
    windows_top_to_bottom: &[smithay::desktop::Window],
    closing_snapshot_count: usize,
    output: &Output,
    output_geo: smithay::utils::Rectangle<i32, Logical>,
    scale: smithay::utils::Scale<f64>,
) -> Option<smithay::desktop::Window> {
    // Closing-window animations draw above live windows; let the normal
    // pipeline run while one is active.
    if closing_snapshot_count != 0 {
        return None;
    }
    let output_name = output.name();
    // Topmost window that actually renders on this output. If that is not
    // the fullscreen window (e.g. a floating window stacked above it), the
    // fast path must stay off so the upper window remains visible.
    let window = windows_top_to_bottom.iter().find(|window| {
        let Some(decoration) = window_decorations.get(window) else {
            return false;
        };
        if !decoration.managed_window_allows_render_on_output(output_name.as_str()) {
            return false;
        }
        space
            .element_geometry(window)
            .is_some_and(|geometry| geometry.intersection(output_geo).is_some())
    })?;
    let toplevel = window.toplevel()?;
    let fullscreen = toplevel.with_committed_state(|state| {
        state.is_some_and(|state| {
            state
                .states
                .contains(smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::State::Fullscreen)
        })
    });
    if !fullscreen {
        return None;
    }
    // The committed window must actually cover the output; during the
    // fullscreen transition (configure sent, buffer not resized yet) the
    // normal pipeline keeps rendering the scene below.
    let geometry = space.element_geometry(window)?;
    if geometry.intersection(output_geo) != Some(output_geo) {
        return None;
    }
    // No animation transform/fade: effect and transform elements cannot ride
    // direct scanout, so fall back to compositing until the window settles.
    let decoration = window_decorations.get(window)?;
    let visual_state = window_visual_state(
        decoration.layout.root.rect,
        decoration.visual_transform,
        output_geo,
        scale,
    );
    if !is_identity_visual_geometry(visual_state) || visual_state.opacity < 1.0 {
        return None;
    }
    Some(window.clone())
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
    commit_timing_timer_armed: bool,
    commit_timing_timer_generation: u64,
    frame_callback_sequence: u32,
    redraw_state: TtyRedrawState,
    frame_duration: Duration,
    next_frame_target: Option<Duration>,
    estimated_render_duration: Duration,
    last_presented_at: Option<Duration>,
    last_frame_callback_at: Option<Duration>,
    /// Whether the driver supports immediate (async) page flips on this
    /// surface, i.e. tearing. Queried once at surface creation since it is a
    /// constant device capability. Gates the fullscreen tearing fast path.
    supports_async_flip: bool,
    /// Whether this output is currently using the fullscreen tearing path. Updated on every
    /// real render and consulted by the redraw state machine: while tearing is active a fresh
    /// client commit must not be parked behind the estimated-vblank timer (see
    /// `queue_tty_redraws`).
    tearing_active: bool,
    dmabuf_feedback: SurfaceDmabufFeedback,
}

struct SurfaceDmabufFeedback {
    render: DmabufFeedback,
    scanout: DmabufFeedback,
}

pub fn pause_tty_session(state: &mut ShojiWM) {
    for backend in state.tty_backends.values_mut() {
        backend.drm_output_manager.pause();
        for surface in backend.surfaces.values_mut() {
            reset_surface_after_tty_pause(surface);
        }
    }
}

pub fn resume_tty_session(state: &mut ShojiWM) {
    for (node, backend) in state.tty_backends.iter_mut() {
        if let Err(err) = backend.drm_output_manager.lock().activate(false) {
            warn!(
                ?node,
                ?err,
                "failed to activate drm backend after tty resume"
            );
        }
        for surface in backend.surfaces.values_mut() {
            reset_surface_after_tty_resume(surface);
        }
    }
    // Connector changes that arrived while the session was paused were
    // deferred (scanning a paused device half-applies them and leaves stale
    // output state); re-run them now that the devices accept commits again.
    //
    // Rescan *every* device though, not only the ones a deferral queued.
    // Detecting mid-suspend master loss is structurally incomplete: smithay
    // maps EACCES during a `TEST_ONLY` commit to `Error::TestFailed` and drops
    // the errno, so a loss can go unnoticed, nothing is queued, and the
    // half-applied topology is never repaired — panels stay dark for the rest
    // of the session. Recovery must not depend on having noticed.
    //
    // This is cheap because `DrmScanner` is diff-based: a device whose
    // topology is unchanged emits no events, so the pass costs one connector
    // scan and touches no surfaces. The deferral queue is still drained and
    // still worth keeping — it records which nodes were *known* to need this,
    // which is the difference between a handled race and a silent one.
    let deferred = std::mem::take(&mut state.pending_tty_device_changes);
    let nodes: Vec<DrmNode> = state
        .tty_backends
        .keys()
        .copied()
        .collect();
    for node in nodes {
        if deferred.contains(&node) {
            info!(
                ?node,
                "processing deferred drm device change after tty resume",
            );
        }
        if let Err(
            err,
        ) = device_changed(
            state,
            node,
        ) {
            warn!(
                ?node,
                ?err,
                "failed to rescan drm device after tty resume",
            );
        }
    }
    state.force_full_damage = true;
    state.request_tty_maintenance("tty-session-resume");
    state.schedule_redraw();
}

/// Record a mid-session config evaluation failure for the error overlay.
///
/// The render path retries every frame, so a persistently broken config would
/// rebuild this report ~60 times a second. Keep the first failure instead: it is
/// the one that names the original cause, later frames usually repeat it, and
/// the existing reload paths clear the field once a config loads cleanly again.
/// That mirrors `scheduler_tick`, which also reports once rather than per tick.
fn report_tty_config_error(state: &mut ShojiWM, error: impl ToString) {
    if state.config_error_report.is_none() {
        state.config_error_report = Some(crate::config_error::ConfigErrorReport::runtime(error));
        state.schedule_redraw();
    }
}

fn reset_surface_after_tty_pause(surface: &mut SurfaceData) {
    surface.frame_pending = false;
    surface.queued_at = None;
    surface.queued_cpu_duration = Duration::ZERO;
    surface.skipped_while_pending_count = 0;
    surface.frame_callback_timer_armed = false;
    surface.commit_timing_timer_armed = false;
    surface.next_frame_target = None;
    surface.tearing_active = false;
    surface.redraw_state = TtyRedrawState::Idle;
}

fn reset_surface_after_tty_resume(surface: &mut SurfaceData) {
    reset_surface_after_tty_pause(surface);
    surface.redraw_state = TtyRedrawState::Queued;
}

enum RenderSurfaceOutcome {
    Skipped,
    Processed,
    /// The DRM atomic test rejected the current screen configuration
    /// (`DrmError::TestFailed`). The caller must rebuild this CRTC's surface
    /// via `reset_surface_after_commit_failure` instead of exiting.
    CommitFailed,
}

struct TtyRenderFrameResult {
    is_empty: bool,
    /// True when DrmCompositor assigned a client buffer directly to the
    /// primary plane (zero-copy direct scanout) instead of GL-compositing
    /// into the swapchain.
    primary_scanout: bool,
    primary_plane_kind: &'static str,
    overlay_plane_count: usize,
    /// Id/geometry/alpha of elements assigned to overlay planes. Only
    /// populated when SHOJI_DIRECT_SCANOUT_DEBUG is set.
    overlay_details: Vec<String>,
    cursor_plane_assigned: bool,
    states: RenderElementStates,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TtyRedrawState {
    Idle,
    Queued,
    WaitingForVBlank { redraw_needed: bool },
    WaitingForEstimatedVBlank { queued: bool, generation: u64 },
}

#[derive(Default)]
struct TtyAnimationTimingMetrics {
    closing_snapshot_count: usize,
    transform_snapshot_window_count: usize,
    snapshot_capture_count: usize,
    render_element_count: usize,
    result_is_empty: bool,
    cursor_elapsed_ms: f64,
    upper_layers_elapsed_ms: f64,
    closing_snapshots_elapsed_ms: f64,
    window_loop_elapsed_ms: f64,
    snapshot_capture_elapsed_ms: f64,
    lower_layers_elapsed_ms: f64,
    damage_profile_elapsed_ms: f64,
    render_elapsed_ms: f64,
    total_cpu_elapsed_ms: f64,
    max_window_elapsed_ms: f64,
    max_window_id: Option<String>,
}

#[derive(Default)]
struct TtyWindowTimingMetrics {
    direct_surface_lookup_ms: f64,
    decoration_phase_ms: f64,
    backdrop_ms: f64,
    background_ms: f64,
    icon_ms: f64,
    text_ms: f64,
    client_phase_ms: f64,
    popup_phase_ms: f64,
    full_snapshot_scene_ms: f64,
    full_snapshot_capture_ms: f64,
    live_snapshot_refresh_ms: f64,
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
    /// Per-CRTC (window start, count) of recent surface rebuilds after failed
    /// atomic commit tests. Bounds the recovery path: a configuration the
    /// kernel keeps rejecting must not turn into an endless reset loop.
    surface_reset_attempts: HashMap<
        crtc::Handle, 
        (
            Instant, 
            u32,
        ),
    >,
}

pub fn device_added(
    state: &mut ShojiWM,
    loop_handle: &LoopHandle<'_, ShojiWM>,
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
        let all_formats = renderer.dmabuf_formats();
        // PipeWire/OBS consume DMA-BUF much faster from uncompressed buffers.
        // When the Intel render-compressed modifier is selected by the
        // wlr-screencopy → PipeWire → OBS chain, throughput drops to ~19 fps on
        // a 60-fps target. Setting SHOJI_DMABUF_FEEDBACK_LINEAR_ONLY=1 narrows
        // the advertised modifiers to LINEAR/INVALID so consumers cannot
        // negotiate a compressed format.
        let formats: smithay::backend::allocator::format::FormatSet =
            if std::env::var_os("SHOJI_DMABUF_FEEDBACK_LINEAR_ONLY").is_some() {
                use smithay::backend::allocator::Modifier;
                all_formats
                    .iter()
                    .filter(|fmt| matches!(fmt.modifier, Modifier::Linear | Modifier::Invalid))
                    .copied()
                    .collect()
            } else {
                all_formats
            };
        let modifier_count = formats.iter().count();
        // List unique modifiers (across fourccs) so we can verify the filter.
        let mut unique_mods: std::collections::BTreeSet<u64> = std::collections::BTreeSet::new();
        for fmt in formats.iter() {
            unique_mods.insert(Into::<u64>::into(fmt.modifier));
        }
        let unique_mods_hex: Vec<String> =
            unique_mods.iter().map(|m| format!("0x{:x}", m)).collect();
        info!(
            ?node,
            modifier_count,
            unique_modifier_count = unique_mods.len(),
            unique_modifiers = ?unique_mods_hex,
            "advertising linux-dmabuf modifiers"
        );
        let default_feedback = DmabufFeedbackBuilder::new(node.dev_id(), formats)
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
    // NodeFilter gates whether a client dmabuf is even considered for direct
    // scanout (GbmFramebufferExporter::can_add_framebuffer). It compares the
    // filter against the dmabuf's recorded source node — but smithay's
    // zwp_linux_dmabuf import path hardcodes that node to `None`
    // (wayland/dmabuf/dispatch.rs create_dmabuf(..., None)). So any
    // `NodeFilter::Node(_)` — including the primary node we used before —
    // never matches, silently disabling client direct scanout for every
    // buffer (it always fell back to GL compositing with no reason).
    //
    // This exporter is per-GPU: `device_added` runs once per DRM node and each
    // `BackendData` owns its own renderer/allocator/exporter, so this serves
    // exactly the outputs driven by `node`. `NodeFilter::All` lets
    // can_add_framebuffer return true for any client buffer and defers the real
    // decision to `add_framebuffer`, which imports the dmabuf into THIS GPU's
    // gbm device. Buffers this GPU cannot import (e.g. a foreign-GPU buffer with
    // an incompatible modifier in a multi-GPU setup) fail there and fall back to
    // GL compositing — no garbage, no crash, just no zero-copy for that buffer.
    // (We cannot filter by source node precisely because smithay leaves it None,
    // so "attempt and let the import decide" is the only workable policy.)
    let exporter = GbmFramebufferExporter::new(gbm.clone(), NodeFilter::All);

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
        surface_reset_attempts: HashMap::new(),
    };
    state.tty_backends.insert(node.clone(), backend);
    info!(?node, "drm backend stored in state");

    let backend = state.tty_backends.get_mut(&node).unwrap();

    let drm_loop_handle = loop_handle.clone();
    loop_handle.insert_source(drm_events, move |event, metadata, state| {
        if let DrmEvent::VBlank(crtc) = event {
            trace!(?node, ?crtc, "received drm vblank");
            frame_finish(state, &drm_loop_handle, node, crtc, metadata);
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
    let output_name = surface.output.name();
    // Present-rate diagnostic (SHOJI_PRESENT_RATE_DEBUG): frame_finish runs once
    // per completed flip = once per present, so counting it gives the real
    // present rate per output — the ground truth for "is the compositor showing
    // 120 Hz or 60 Hz".
    note_present_rate(output_name.as_str());
    let gap_threshold_ms = animation_gap_threshold_ms();
    if animation_gap_debug_enabled()
        && let Some(gap_ms) =
            record_animation_gap("tty-frame-finish", output_name.as_str(), Instant::now())
        && gap_ms >= gap_threshold_ms
    {
        let queued_wait_ms = surface
            .queued_at
            .map(|queued_at| queued_at.elapsed().as_secs_f64() * 1000.0);
        warn!(
            output = %output_name,
            gap_ms,
            redraw_state = ?surface.redraw_state,
            frame_pending = surface.frame_pending,
            queued_wait_ms,
            queued_cpu_duration_ms = surface.queued_cpu_duration.as_secs_f64() * 1000.0,
            skipped_while_pending_count = surface.skipped_while_pending_count,
            last_presented_at = ?surface.last_presented_at,
            next_frame_target = ?surface.next_frame_target,
            frame_duration_ms = surface.frame_duration.as_secs_f64() * 1000.0,
            closing_snapshot_count = state.closing_window_snapshots.len(),
            gap_threshold_ms,
            "animation gap: tty frame_finish cadence gap"
        );
    }

    trace!(?node, ?crtc, "marking frame submitted");
    let submit_result = surface.drm_output.frame_submitted();
    let present_sequence = metadata
        .as_ref()
        .map(|metadata| metadata.sequence)
        .unwrap_or(0);
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
        let sequence = present_sequence;
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
    let queued_wait_ms = surface
        .queued_at
        .map(|queued_at| queued_at.elapsed().as_secs_f64() * 1000.0);
    if animation_gap_debug_enabled()
        && queued_wait_ms.is_some_and(|queued_wait_ms| queued_wait_ms >= gap_threshold_ms)
    {
        info!(
            output = %surface.output.name(),
            queued_wait_ms,
            queued_cpu_duration_ms = surface.queued_cpu_duration.as_secs_f64() * 1000.0,
            redraw_state = ?surface.redraw_state,
            skipped_while_pending_count = surface.skipped_while_pending_count,
            sequence = present_sequence,
            "animation gap: tty frame_finish queue wait"
        );
    }
    if std::env::var_os("SHOJI_XDG_POPUP_LATENCY_DEBUG").is_some() {
        if let Some(popup_debug) = state.popup_latency_debug.take() {
            tracing::info!(
                surface_id = popup_debug.surface_id,
                created_to_frame_finish_ms = presentation_clock
                    .checked_sub(popup_debug.created_at)
                    .map(|delta| delta.as_secs_f64() * 1000.0),
                commit_to_frame_finish_ms = popup_debug
                    .committed_at
                    .and_then(|commit| presentation_clock.checked_sub(commit))
                    .map(|delta| delta.as_secs_f64() * 1000.0),
                output = %surface.output.name(),
                "xdg popup latency: frame finish"
            );
        }
    }

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
    if !redraw_needed {
        // `next_frame_target` predicts the next vblank for already-queued follow-up work.
        // Once the output goes idle, keeping it would make the next unrelated commit reuse
        // a stale presentation target, sometimes more than a second in the past.
        surface.next_frame_target = None;
    }
    if mpv_frame_debug_enabled() {
        info!(
            output = %surface.output.name(),
            presentation_clock_ms = presentation_clock.as_secs_f64() * 1000.0,
            frame_duration_ms = surface.frame_duration.as_secs_f64() * 1000.0,
            redraw_needed,
            idle_callback_sent = false,
            next_redraw_state = ?surface.redraw_state,
            frame_callback_sequence = surface.frame_callback_sequence,
            queued_wait_ms,
            pending_window_damage = state.window_source_damage.len(),
            pending_lower_layer_damage = state.lower_layer_source_damage.len(),
            pending_upper_layer_damage = state.upper_layer_source_damage.len(),
            pending_decoration_damage = state.pending_decoration_damage.len(),
            "mpv frame debug: frame_finish"
        );
    }
    if redraw_needed {
        // Once a flip completes, any redraw requested while that output was pending is local to
        // this output. Calling the global `schedule_redraw()` here re-queues every output; with
        // multiple monitors that makes outputs continually mark each other as `redraw_needed`
        // while they wait for vblank, creating a 120Hz-per-output idle render loop.
        //
        // Render this CRTC directly instead. This preserves the low-latency tearing path and also
        // handles normal vblank-synced follow-up frames without waking unrelated outputs.
        render_queued_surface_after_frame_finish(state, loop_handle, node, crtc);
    } else {
        schedule_commit_timing_timer(loop_handle, state, node, crtc);
    }
}

fn render_queued_surface_after_frame_finish(
    state: &mut ShojiWM,
    loop_handle: &LoopHandle<'_, ShojiWM>,
    node: DrmNode,
    crtc: crtc::Handle,
) {
    match render_surface(state, loop_handle, node, crtc) {
        Ok(RenderSurfaceOutcome::Processed) => {
            let output_name = state
                .tty_backends
                .get(&node)
                .and_then(|backend| backend.surfaces.get(&crtc))
                .map(|surface| surface.output.name());
            if let Some(output_name) = output_name {
                finish_processed_outputs(state, &[output_name]);
            }
        }
        Ok(RenderSurfaceOutcome::Skipped) => {
            trace!(
                ?node,
                ?crtc,
                "queued tty follow-up redraw was skipped after frame_finish"
            );
        }
        Ok(RenderSurfaceOutcome::CommitFailed) => {
            if let Err(
                err,
            ) = reset_surface_after_commit_failure(
                state,
                node,
                crtc,
            ) {
                // This path was never fatal; stay consistent and let the
                // main render loop decide when a reset storm ends the session.
                warn!(
                    ?node,
                    ?crtc,
                    error = ?err,
                    "surface reset after follow-up commit failure did not recover",
                );
            }
        }
        Err(err) => {
            warn!(
                ?node,
                ?crtc,
                error = ?err,
                "queued tty follow-up redraw failed; falling back to scheduled redraw"
            );
            state.schedule_redraw();
        }
    }
}

pub fn render_if_needed(
    state: &mut ShojiWM,
    loop_handle: &LoopHandle<'_, ShojiWM>,
) -> Result<(), Box<dyn std::error::Error>> {
    if !state.needs_redraw {
        return Ok(());
    }
    if !state.tty_session_active {
        trace!("skipping tty redraw while session is inactive");
        return Ok(());
    }

    let gap_threshold_ms = animation_gap_threshold_ms();
    if animation_gap_debug_enabled()
        && let Some(gap_ms) = record_animation_gap("tty-render-if-needed", "global", Instant::now())
        && gap_ms >= gap_threshold_ms
    {
        warn!(
            gap_ms,
            closing_snapshot_count = state.closing_window_snapshots.len(),
            window_count = state.space.elements().count(),
            needs_redraw = state.needs_redraw,
            gap_threshold_ms,
            "animation gap: render_if_needed cadence gap"
        );
    }

    if std::env::var_os("SHOJI_XDG_POPUP_LATENCY_DEBUG").is_some() {
        if let Some(popup_debug) = state.popup_latency_debug {
            let now = Duration::from(state.clock.now());
            tracing::info!(
                surface_id = popup_debug.surface_id,
                created_to_render_start_ms = now
                    .checked_sub(popup_debug.created_at)
                    .map(|delta| delta.as_secs_f64() * 1000.0),
                commit_to_render_start_ms = popup_debug
                    .committed_at
                    .and_then(|commit| now.checked_sub(commit))
                    .map(|delta| delta.as_secs_f64() * 1000.0),
                "xdg popup latency: render_if_needed start"
            );
        }
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
                RenderSurfaceOutcome::CommitFailed => {
                    // An invalid screen configuration is expected right after
                    // resume when the connector topology changed during
                    // sleep. Rebuild the surface instead of exiting the
                    // session; only a repeated reset storm propagates as
                    // fatal.
                    reset_surface_after_commit_failure(
                        state,
                        node,
                        crtc,
                    )?;
                }
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

    finish_processed_outputs(state, &processed_outputs);

    Ok(())
}

fn finish_processed_outputs(state: &mut ShojiWM, processed_outputs: &[String]) {
    if processed_outputs.is_empty() {
        return;
    }

    // In multi-monitor configurations the outputs run at independent refresh rates.
    // Retain source damage while any output that can consume it still has a render queued.
    let pending_output_names: std::collections::HashSet<String> = state
        .tty_backends
        .values()
        .flat_map(|backend| backend.surfaces.values())
        .filter(|surface| {
            matches!(
                surface.redraw_state,
                TtyRedrawState::Queued
                    | TtyRedrawState::WaitingForVBlank {
                        redraw_needed: true
                    }
                    | TtyRedrawState::WaitingForEstimatedVBlank { queued: true, .. }
            )
        })
        .map(|surface| surface.output.name())
        .collect();

    if !pending_output_names.is_empty() {
        let window_outputs: std::collections::HashMap<String, Vec<String>> = state
            .space
            .elements()
            .filter_map(|window| {
                let id = state
                    .window_decorations
                    .get(window)
                    .map(|decoration| decoration.snapshot.id.clone())?;
                let outputs = state
                    .space
                    .outputs_for_element(window)
                    .into_iter()
                    .map(|output| output.name())
                    .collect();
                Some((id, outputs))
            })
            .collect();
        let mut layer_outputs: std::collections::HashMap<String, Vec<String>> =
            std::collections::HashMap::new();
        for output in state.space.outputs() {
            let output_name = output.name();
            let map = smithay::desktop::layer_map_for_output(output);
            for layer in map
                .layers()
                .filter(|layer| crate::backend::window::layer_surface_is_mapped(layer))
            {
                layer_outputs
                    .entry(layer.wl_surface().id().protocol_id().to_string())
                    .or_default()
                    .push(output_name.clone());
            }
        }

        let owner_still_needed =
            |owner: &str, visibility: &std::collections::HashMap<String, Vec<String>>| -> bool {
                visibility
                    .get(owner)
                    .map(|outputs| {
                        outputs
                            .iter()
                            .any(|name| pending_output_names.contains(name))
                    })
                    .unwrap_or(false)
            };

        state
            .window_source_damage
            .retain(|entry| owner_still_needed(&entry.owner, &window_outputs));
        state
            .lower_layer_source_damage
            .retain(|entry| owner_still_needed(&entry.owner, &layer_outputs));
        state
            .upper_layer_source_damage
            .retain(|entry| owner_still_needed(&entry.owner, &layer_outputs));
        state.pending_decoration_damage.clear();
    } else {
        state.pending_decoration_damage.clear();
        state.clear_source_damage();
    }
    state.finish_damage_blink_for_outputs(processed_outputs.iter().map(String::as_str));
}

render_elements! {
    pub TtyRenderElements<=GlesRenderer>;
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
    Cursor=PointerRenderElement<GlesRenderer>,
}

fn tty_render_element_name(element: &TtyRenderElements) -> &'static str {
    match element {
        TtyRenderElements::Window(_) => "Window",
        TtyRenderElements::PolicyWindow(_) => "PolicyWindow",
        TtyRenderElements::TransformedWindow(_) => "TransformedWindow",
        TtyRenderElements::Clipped(_) => "Clipped",
        TtyRenderElements::TransformedClipped(_) => "TransformedClipped",
        TtyRenderElements::Text(_) => "Text",
        TtyRenderElements::RelocatedText(_) => "RelocatedText",
        TtyRenderElements::TransformedText(_) => "TransformedText",
        TtyRenderElements::Snapshot(_) => "Snapshot",
        TtyRenderElements::TransformedSnapshot(_) => "TransformedSnapshot",
        TtyRenderElements::Damage(_) => "Damage",
        TtyRenderElements::Blink(_) => "Blink",
        TtyRenderElements::Decoration(_) => "Decoration",
        TtyRenderElements::RelocatedDecoration(_) => "RelocatedDecoration",
        TtyRenderElements::TransformedDecoration(_) => "TransformedDecoration",
        TtyRenderElements::Backdrop(_) => "Backdrop",
        TtyRenderElements::RelocatedBackdrop(_) => "RelocatedBackdrop",
        TtyRenderElements::TransformedBackdrop(_) => "TransformedBackdrop",
        TtyRenderElements::Cursor(_) => "Cursor",
        _ => "Generic",
    }
}

pub struct OutputCaptureMirror {
    texture: GlesTexture,
    damage_tracker: OutputDamageTracker,
    size: Size<i32, Physical>,
    scale: Scale<f64>,
    transform: Transform,
}

fn render_output_capture_mirror(
    renderer: &mut GlesRenderer,
    mirror: &mut Option<OutputCaptureMirror>,
    output: &Output,
    elements: &[TtyRenderElements],
) -> Result<
    Option<(TtyRenderElements, TtyRenderElements, RenderElementStates)>,
    Box<dyn std::error::Error>,
> {
    let Some(mode) = output.current_mode() else {
        return Ok(None);
    };
    let size = mode.size;
    let scale: Scale<f64> = output.current_scale().fractional_scale().into();
    let transform = output.current_transform();

    let recreate = mirror.as_ref().is_none_or(|mirror| {
        mirror.size != size || mirror.scale != scale || mirror.transform != transform
    });
    if recreate {
        let buffer_size = size.to_logical(1).to_buffer(1, Transform::Normal);
        let texture =
            Offscreen::<GlesTexture>::create_buffer(renderer, Fourcc::Abgr8888, buffer_size)?;
        *mirror = Some(OutputCaptureMirror {
            texture,
            damage_tracker: OutputDamageTracker::new(size, scale, transform),
            size,
            scale,
            transform,
        });
    }

    let Some(mirror) = mirror.as_mut() else {
        return Ok(None);
    };
    let mirror_logical_size: Size<i32, Logical> = (
        ((size.w as f64) / scale.x.abs().max(0.0001)).round() as i32,
        ((size.h as f64) / scale.y.abs().max(0.0001)).round() as i32,
    )
        .into();
    let mirror_src = Rectangle::<f64, Logical>::from_size((size.w as f64, size.h as f64).into());
    let mirror_render_states = {
        let mut target = renderer.bind(&mut mirror.texture)?;
        mirror
            .damage_tracker
            .render_output(
                renderer,
                &mut target,
                0,
                elements,
                Color32F::new(
                    CLEAR_COLOR[0],
                    CLEAR_COLOR[1],
                    CLEAR_COLOR[2],
                    CLEAR_COLOR[3],
                ),
            )?
            .states
    };

    let make_element = || {
        TextureRenderElement::from_static_texture(
            Id::new(),
            renderer.context_id(),
            (0.0, 0.0),
            mirror.texture.clone(),
            1,
            Transform::Normal,
            None,
            Some(mirror_src),
            Some(mirror_logical_size),
            None,
            Kind::Unspecified,
        )
    };
    Ok(Some((
        TtyRenderElements::Snapshot(make_element()),
        TtyRenderElements::Snapshot(make_element()),
        mirror_render_states,
    )))
}

struct ProfiledTtyRenderElement<'a> {
    inner: &'a TtyRenderElements,
    draw_label: &'static str,
}

impl<'a> ProfiledTtyRenderElement<'a> {
    fn new(inner: &'a TtyRenderElements) -> Self {
        let draw_label = match inner {
            TtyRenderElements::Window(_) | TtyRenderElements::TransformedWindow(_) => {
                "tty-element-window-draw"
            }
            TtyRenderElements::Clipped(_) | TtyRenderElements::TransformedClipped(_) => {
                "tty-element-clipped-draw"
            }
            TtyRenderElements::Text(_)
            | TtyRenderElements::RelocatedText(_)
            | TtyRenderElements::TransformedText(_) => "tty-element-text-draw",
            TtyRenderElements::Snapshot(_) | TtyRenderElements::TransformedSnapshot(_) => {
                "tty-element-snapshot-draw"
            }
            TtyRenderElements::Damage(_) => "tty-element-damage-draw",
            TtyRenderElements::Blink(_) => "tty-element-blink-draw",
            TtyRenderElements::Decoration(_)
            | TtyRenderElements::RelocatedDecoration(_)
            | TtyRenderElements::TransformedDecoration(_) => "tty-element-decoration-draw",
            TtyRenderElements::Backdrop(_)
            | TtyRenderElements::RelocatedBackdrop(_)
            | TtyRenderElements::TransformedBackdrop(_) => "tty-element-backdrop-draw",
            TtyRenderElements::Cursor(_) => "tty-element-cursor-draw",
            _ => "tty-element-generic-draw",
        };
        Self { inner, draw_label }
    }
}

impl Element for ProfiledTtyRenderElement<'_> {
    fn id(&self) -> &Id {
        self.inner.id()
    }

    fn current_commit(&self) -> CommitCounter {
        self.inner.current_commit()
    }

    fn location(&self, scale: Scale<f64>) -> Point<i32, Physical> {
        self.inner.location(scale)
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        self.inner.src()
    }

    fn transform(&self) -> Transform {
        self.inner.transform()
    }

    fn geometry(&self, scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.inner.geometry(scale)
    }

    fn damage_since(
        &self,
        scale: Scale<f64>,
        commit: Option<CommitCounter>,
    ) -> DamageSet<i32, Physical> {
        self.inner.damage_since(scale, commit)
    }

    fn opaque_regions(&self, scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        self.inner.opaque_regions(scale)
    }

    fn alpha(&self) -> f32 {
        self.inner.alpha()
    }

    fn kind(&self) -> Kind {
        self.inner.kind()
    }

    fn is_framebuffer_effect(&self) -> bool {
        self.inner.is_framebuffer_effect()
    }

    fn framebuffer_capture_region(&self, scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.inner.framebuffer_capture_region(scale)
    }
}

impl RenderElement<GlesRenderer> for ProfiledTtyRenderElement<'_> {
    fn draw(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
        cache: Option<&UserDataMap>,
    ) -> Result<(), GlesError> {
        let timing = crate::backend::shader_effect::begin_gpu_timing_frame_span(
            frame,
            self.draw_label,
            (dst.size.w, dst.size.h),
        );
        let result = RenderElement::<GlesRenderer>::draw(
            self.inner,
            frame,
            src,
            dst,
            damage,
            opaque_regions,
            cache,
        );
        crate::backend::shader_effect::end_gpu_timing_frame_span(frame, timing);
        result
    }

    fn underlying_storage(&self, renderer: &mut GlesRenderer) -> Option<UnderlyingStorage<'_>> {
        RenderElement::<GlesRenderer>::underlying_storage(self.inner, renderer)
    }

    fn capture_framebuffer(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        cache: &UserDataMap,
    ) -> Result<(), GlesError> {
        let timing = crate::backend::shader_effect::begin_gpu_timing_frame_span(
            frame,
            "tty-element-framebuffer-capture",
            (dst.size.w, dst.size.h),
        );
        let result =
            RenderElement::<GlesRenderer>::capture_framebuffer(self.inner, frame, src, dst, cache);
        crate::backend::shader_effect::end_gpu_timing_frame_span(frame, timing);
        result
    }
}

fn capture_scene_texture_for_effect(
    renderer: &mut GlesRenderer,
    source: &'static str,
    capture_geo: smithay::utils::Rectangle<i32, Logical>,
    scale: smithay::utils::Scale<f64>,
    scene: &[TtyRenderElements],
) -> Option<GlesTexture> {
    if scene.is_empty() {
        return None;
    }
    let mut tracker = smithay::backend::renderer::damage::OutputDamageTracker::new(
        (0, 0),
        1.0,
        Transform::Normal,
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
                crate::backend::snapshot::capture_snapshot(
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

fn queue_tty_redraws(state: &mut ShojiWM) {
    let gap_threshold_ms = animation_gap_threshold_ms();
    let renderable_outputs = state
        .space
        .outputs()
        .filter(|output| state.runtime_output_render_enabled(&output.name()))
        .map(Output::name)
        .collect::<std::collections::HashSet<_>>();
    for backend in state.tty_backends.values_mut() {
        for surface in backend.surfaces.values_mut() {
            if !renderable_outputs.contains(&surface.output.name()) {
                if !surface.frame_pending {
                    surface.redraw_state = TtyRedrawState::Idle;
                    surface.queued_at = None;
                    surface.next_frame_target = None;
                }
                continue;
            }
            let previous_state = surface.redraw_state;
            let tearing_active = surface.tearing_active;
            match surface.redraw_state {
                TtyRedrawState::Idle => {
                    surface.redraw_state = TtyRedrawState::Queued;
                }
                TtyRedrawState::Queued => {}
                TtyRedrawState::WaitingForVBlank { .. } => {
                    surface.redraw_state = TtyRedrawState::WaitingForVBlank {
                        redraw_needed: true,
                    };
                }
                TtyRedrawState::WaitingForEstimatedVBlank { generation, .. } => {
                    // Tearing fast-path fix: do not let a fresh client commit get stuck behind the
                    // previous estimated-vblank timer.
                    //
                    // Background: `frame_finish` re-renders immediately after each async (tearing)
                    // flip completes. That re-render frequently runs in the sub-millisecond gap
                    // *before* the client's next buffer commit, so it produces a no-damage frame.
                    // The no-damage path parks the surface in `WaitingForEstimatedVBlank` and arms a
                    // ~one-refresh-period timer (`schedule_estimated_vblank_callback`). For a normal
                    // desktop surface that is correct throttling, but a tearing client commits fresh
                    // buffers far above the refresh rate. With the default arm below, every such
                    // commit only sets `queued = true` and waits for that timer to fire — so the
                    // effective present cadence collapses to ~refresh rate and frames bunch up
                    // unevenly (this was the root cause of the uneven view-motion / afterimage
                    // spacing during fullscreen Direct Scanout + tearing; the present rate measured
                    // ~2 flips clustered then a ~one-refresh gap).
                    //
                    // Fix: while tearing is active, promote straight to `Queued` so the very next
                    // commit tears immediately instead of waiting a whole refresh period. The
                    // already-armed estimated-vblank timer becomes a no-op: queuing the real frame
                    // bumps `frame_callback_timer_generation`, and the timer's generation guard then
                    // drops the stale callback. Keeping the timer armed still provides the
                    // frame-callback safety net for the case where the client genuinely stops
                    // committing (e.g. a paused game).
                    //
                    // Possible second-stage improvement (not yet implemented): gate the
                    // `frame_finish` re-render on whether the fullscreen window actually has a new
                    // buffer since the last present — e.g. compare
                    // `with_renderer_surface_state(surface, |s| s.current_commit())` against a stored
                    // `last_present_window_commit`. That would skip the redundant no-damage
                    // re-render entirely (and the no-damage churn that non-buffer redraws such as
                    // cursor motion cause), tightening the cadence further and cutting wasted GPU/CPU
                    // work. The promotion here is enough to remove the refresh-rate cap; the gate
                    // would mostly be an efficiency/evenness refinement.
                    surface.redraw_state = if tearing_active {
                        TtyRedrawState::Queued
                    } else {
                        TtyRedrawState::WaitingForEstimatedVBlank {
                            queued: true,
                            generation,
                        }
                    };
                }
            }
            if animation_gap_debug_enabled() && previous_state != surface.redraw_state {
                let queued_wait_ms = surface
                    .queued_at
                    .map(|queued_at| queued_at.elapsed().as_secs_f64() * 1000.0);
                if queued_wait_ms.is_some_and(|gap_ms| gap_ms >= gap_threshold_ms)
                    || matches!(
                        (&previous_state, &surface.redraw_state),
                        (
                            TtyRedrawState::WaitingForVBlank { .. },
                            TtyRedrawState::WaitingForVBlank {
                                redraw_needed: true
                            }
                        )
                    )
                {
                    warn!(
                        output = %surface.output.name(),
                        previous_state = ?previous_state,
                        next_state = ?surface.redraw_state,
                        frame_pending = surface.frame_pending,
                        queued_wait_ms,
                        skipped_while_pending_count = surface.skipped_while_pending_count,
                        gap_threshold_ms,
                        "animation gap: tty queue_redraw transition"
                    );
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

fn retire_unmapped_surface(state: &mut ShojiWM, node: DrmNode, crtc: crtc::Handle) {
    let Some(surface) = state
        .tty_backends
        .get_mut(&node)
        .and_then(|backend| backend.surfaces.get_mut(&crtc))
    else {
        return;
    };
    if surface.frame_pending {
        return;
    }
    surface.redraw_state = TtyRedrawState::Idle;
    surface.queued_at = None;
    surface.next_frame_target = None;
}

fn render_surface(
    state: &mut ShojiWM,
    loop_handle: &LoopHandle<'_, ShojiWM>,
    node: DrmNode,
    crtc: crtc::Handle,
) -> Result<RenderSurfaceOutcome, Box<dyn std::error::Error>> {
    let frame_started_at = Instant::now();
    timescope::scope!("tty render_surface");
    let spike_threshold_ms = animation_spike_threshold_ms();
    let Some(output) = state
        .tty_backends
        .get(&node)
        .and_then(|backend| backend.surfaces.get(&crtc))
        .map(|surface| surface.output.clone())
    else {
        return Ok(RenderSurfaceOutcome::Skipped);
    };
    if !state.runtime_output_render_enabled(&output.name())
        || state.space.output_geometry(&output).is_none()
    {
        retire_unmapped_surface(state, node, crtc);
        return Ok(RenderSurfaceOutcome::Skipped);
    }
    {
        timescope::scope!("tty render_surface debug gates");
        if std::env::var_os("SHOJI_SCREENCOPY_PROFILE").is_some() {
            let frame_pending = state
                .tty_backends
                .get(&node)
                .and_then(|backend| backend.surfaces.get(&crtc))
                .map(|surface| surface.frame_pending)
                .unwrap_or(false);
            let redraw_state = state
                .tty_backends
                .get(&node)
                .and_then(|backend| backend.surfaces.get(&crtc))
                .map(|surface| surface.redraw_state)
                .unwrap_or(TtyRedrawState::Idle);
            tracing::info!(
                output = %output.name(),
                frame_pending,
                redraw_state = ?redraw_state,
                "screencopy: render_surface called"
            );
        }
        let gap_threshold_ms = animation_gap_threshold_ms();
        if animation_gap_debug_enabled()
            && let Some(gap_ms) =
                record_animation_gap("tty-render-surface", output.name().as_str(), Instant::now())
            && gap_ms >= gap_threshold_ms
        {
            let redraw_state = state
                .tty_backends
                .get(&node)
                .and_then(|backend| backend.surfaces.get(&crtc))
                .map(|surface| surface.redraw_state)
                .unwrap_or(TtyRedrawState::Idle);
            let frame_pending = state
                .tty_backends
                .get(&node)
                .and_then(|backend| backend.surfaces.get(&crtc))
                .map(|surface| surface.frame_pending)
                .unwrap_or(false);
            warn!(
                output = %output.name(),
                gap_ms,
                redraw_state = ?redraw_state,
                frame_pending,
                queued_wait_ms = state
                    .tty_backends
                    .get(&node)
                    .and_then(|backend| backend.surfaces.get(&crtc))
                    .and_then(|surface| surface.queued_at)
                    .map(|queued_at| queued_at.elapsed().as_secs_f64() * 1000.0),
                closing_snapshot_count = state.closing_window_snapshots.len(),
                gap_threshold_ms,
                "animation gap: tty render_surface cadence gap"
            );
        }
    }
    let gap_threshold_ms = animation_gap_threshold_ms();

    let decoration_refresh_started_at = Instant::now();
    {
        timescope::scope!("tty refresh_window_decorations");
        // A config that fails to evaluate mid-session must not end the session.
        // This `?` propagated out of `render_if_needed` into `main() -> Result`,
        // so one bad evaluation tore the whole compositor down and dropped the
        // user back at the display manager. That is a live tripwire whenever
        // `$SHOJI_CONFIG` points into a working tree: an ordinary `git checkout`
        // across a revision that changed the config is enough to trigger it,
        // with no way to read the error afterwards. Degrade the way the winit
        // backend already does — keep the previous decorations for this frame —
        // and surface the failure through the config-error overlay so it is
        // legible and recoverable in place.
        if let Err(err) =
            state.refresh_window_decorations_for_output(Some(output.name().as_str()))
        {
            warn!(
                output = %output.name(),
                error = ?err,
                "failed to refresh window decorations for tty; keeping previous decorations"
            );
            report_tty_config_error(state, err);
        }
    }
    // Output configuration is reactive. Evaluating the TypeScript runtime above can unmap this
    // output (for example, disabling the internal panel when a dock output appears). Do not carry
    // the stale DRM render request into scene preparation after that transition.
    if !state.runtime_output_render_enabled(&output.name())
        || state.space.output_geometry(&output).is_none()
    {
        retire_unmapped_surface(state, node, crtc);
        return Ok(RenderSurfaceOutcome::Skipped);
    }
    let decoration_refresh_elapsed_ms =
        decoration_refresh_started_at.elapsed().as_secs_f64() * 1000.0;
    let layer_effects_started_at = Instant::now();
    {
        timescope::scope!("tty refresh_layer_and_popup_effects");
        // Same reasoning as the decoration refresh above: layer and popup
        // effects come from the same runtime evaluation, so a broken config
        // fails here too and must not be fatal either.
        if let Err(err) = state.refresh_layer_effects_for_output(output.name().as_str()) {
            warn!(
                output = %output.name(),
                error = ?err,
                "failed to refresh layer effects for tty; keeping previous effects"
            );
            report_tty_config_error(state, err);
        }
        if let Err(err) = state.refresh_popup_effects_for_output(output.name().as_str()) {
            warn!(
                output = %output.name(),
                error = ?err,
                "failed to refresh popup effects for tty; keeping previous effects"
            );
            report_tty_config_error(state, err);
        }
    }
    let layer_effects_elapsed_ms = layer_effects_started_at.elapsed().as_secs_f64() * 1000.0;

    let Some(output_geo) = state.space.output_geometry(&output) else {
        retire_unmapped_surface(state, node, crtc);
        return Ok(RenderSurfaceOutcome::Skipped);
    };

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
            if animation_gap_debug_enabled() {
                let queued_wait_ms = surface
                    .queued_at
                    .map(|queued_at| queued_at.elapsed().as_secs_f64() * 1000.0);
                if queued_wait_ms.is_some_and(|queued_wait_ms| queued_wait_ms >= gap_threshold_ms)
                    || surface.frame_pending
                {
                    warn!(
                        output = %output.name(),
                        redraw_state = ?redraw_state,
                        frame_pending = surface.frame_pending,
                        queued_wait_ms,
                        skipped_while_pending_count = surface.skipped_while_pending_count,
                        frame_callback_timer_armed = surface.frame_callback_timer_armed,
                        last_presented_at = ?surface.last_presented_at,
                        next_frame_target = ?surface.next_frame_target,
                        "animation gap: tty render_surface skipped"
                    );
                }
            }
        }
        return Ok(RenderSurfaceOutcome::Skipped);
    }

    let has_visible_mpv = {
        timescope::scope!("tty visible mpv check");
        mpv_frame_debug_enabled() && output_has_visible_mpv(state, &output)
    };
    let (frame_duration, fallback_frame_time, raw_frame_target, frame_target, stale_frame_target) = {
        timescope::scope!("tty frame timing setup");
        let frame_duration = state
            .tty_backends
            .get(&node)
            .and_then(|backend| backend.surfaces.get(&crtc))
            .map(|surface| surface.frame_duration)
            .unwrap_or(Duration::ZERO);
        let fallback_frame_time = Duration::from(state.clock.now()) + frame_duration;
        let raw_frame_target = state
            .tty_backends
            .get(&node)
            .and_then(|backend| backend.surfaces.get(&crtc))
            .and_then(|surface| surface.next_frame_target);
        let (frame_target, stale_frame_target) =
            sanitize_next_frame_target(raw_frame_target, fallback_frame_time, frame_duration);
        (
            frame_duration,
            fallback_frame_time,
            raw_frame_target,
            frame_target,
            stale_frame_target,
        )
    };
    if stale_frame_target && mpv_frame_debug_enabled() && output_has_visible_mpv(state, &output) {
        info!(
            output = %output.name(),
            raw_frame_target_ms = raw_frame_target.map(|target| target.as_secs_f64() * 1000.0),
            fallback_frame_time_ms = fallback_frame_time.as_secs_f64() * 1000.0,
            frame_duration_ms = frame_duration.as_secs_f64() * 1000.0,
            "mpv frame debug: stale next_frame_target ignored before pre_repaint"
        );
    }
    {
        timescope::scope!("tty pre_repaint");
        state.pre_repaint(&output, frame_target.into());
    }

    let mut timing = TtyAnimationTimingMetrics::default();

    let (
        should_capture_blink,
        blink_visible,
        has_visible_x11_chrome,
        mut extra_damage,
        windows_top_to_bottom_for_output,
        session_lock_surface_for_output,
    ) = {
        timescope::scope!("tty render prep");
        let should_capture_blink = state.damage_blink_enabled;
        let blink_visible = state.damage_blink_rects_for_output(&output).to_vec();
        let has_visible_x11_chrome = output_has_visible_x11_chrome(state, &output);
        let mut extra_damage = state.pending_decoration_damage.clone();
        if std::env::var_os("SHOJI_TRANSFORM_SNAPSHOT_DEBUG").is_some() && !extra_damage.is_empty()
        {
            tracing::info!(
                output = %output.name(),
                pending_decoration_damage_count = extra_damage.len(),
                "transform snapshot tty pending decoration damage at render start"
            );
        }
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
        let windows_top_to_bottom_for_output: Vec<_> = state
            .windows_for_output_top_to_bottom(&output)
            .into_iter()
            .cloned()
            .collect();
        let session_lock_surface_for_output = state.session_lock_surface_for_output(&output);
        (
            should_capture_blink,
            blink_visible,
            has_visible_x11_chrome,
            extra_damage,
            windows_top_to_bottom_for_output,
            session_lock_surface_for_output,
        )
    };
    let mut newly_ready_initial_focus_window_ids = Vec::new();
    let captured_blink_damage = {
        timescope::scope!("tty render mutable section");
        let window_source_damage_snapshot = state.window_source_damage.clone();
        let ShojiWM {
            space,
            tty_session_active,
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
            live_window_snapshot_trackers,
            complete_window_snapshots,
            complete_window_snapshot_trackers,
            closing_window_snapshots,
            snapshot_dirty_window_ids,
            transform_snapshot_window_ids,
            screencopy_state,
            image_copy_capture_pending,
            fps_counter,
            text_rasterizer,
            config_error_report,
            ..
        } = state;

        let backend = tty_backends.get_mut(&node).unwrap();
        let surface = backend.surfaces.get_mut(&crtc).unwrap();
        let render_started_at = Instant::now();
        let raw_frame_time = surface.next_frame_target.take();
        let (frame_time, stale_frame_time) =
            sanitize_next_frame_target(raw_frame_time, fallback_frame_time, frame_duration);
        surface.last_frame_callback_at = Some(frame_time);
        if has_visible_mpv {
            info!(
                output = %output.name(),
                frame_time_ms = frame_time.as_secs_f64() * 1000.0,
                raw_frame_time_ms = raw_frame_time.map(|target| target.as_secs_f64() * 1000.0),
                stale_frame_time,
                fallback_frame_time_ms = fallback_frame_time.as_secs_f64() * 1000.0,
                frame_duration_ms = frame_duration.as_secs_f64() * 1000.0,
                pending_window_damage = window_source_damage_snapshot.len(),
                pending_decoration_damage = extra_damage.len(),
                redraw_state = ?surface.redraw_state,
                frame_pending = surface.frame_pending,
                frame_callback_sequence = surface.frame_callback_sequence,
                "mpv frame debug: render_start"
            );
        }
        // The raw `PointerRenderElement` Vec is kept separately so the
        // toplevel image-copy-capture path can wrap each cursor with a
        // window-local `RelocateRenderElement` before rendering. After the
        // capture pass we move the same elements into the unified
        // `TtyRenderElements::Cursor` list used by the DRM render and the
        // other capture queues.
        let mut cursor_pointer_elements: Vec<PointerRenderElement<GlesRenderer>> = Vec::new();
        let mut frame_had_transform_snapshot_damage = false;
        let mut frame_transform_snapshot_window_count = 0usize;
        let mut frame_snapshot_damage_window_count = 0usize;
        let cursor_started_at = Instant::now();

        let pointer_pos = seat.get_pointer().unwrap().current_location();
        let scale = Scale::from(output.current_scale().fractional_scale());
        let windows_top_to_bottom = windows_top_to_bottom_for_output;
        let all_windows: Vec<_> = space.elements().cloned().collect();
        let window_count = all_windows.len();
        let closing_snapshots = closing_window_snapshots
            .values()
            .cloned()
            .collect::<Vec<_>>();
        timing.closing_snapshot_count = closing_snapshots.len();
        let (_, _lower_layer_elements) =
            window_render::layer_elements_for_output(&mut backend.renderer, &output, scale, 1.0);

        {
            timescope::scope!("tty cursor elements");
            if output_geo.to_f64().contains(pointer_pos) {
                let reset = matches!(cursor_status, CursorImageStatus::Surface(surface) if !surface.alive());
                if reset {
                    *cursor_status = CursorImageStatus::default_named();
                }

                let effective_cursor_status = cursor_override
                    .map(CursorImageStatus::Named)
                    .unwrap_or_else(|| cursor_status.clone());

                let hotspot = if let CursorImageStatus::Surface(surface) = &effective_cursor_status
                {
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
                    let cursor_scale =
                        output.current_scale().fractional_scale().ceil().max(1.0) as u32;
                    let frame = cursor_theme.get_image(icon, cursor_scale, start_time.elapsed());
                    let buffer = pointer_images
                        .iter()
                        .find_map(|(image, buffer)| (image == &frame).then_some(buffer.clone()))
                        .unwrap_or_else(|| {
                            let buffer = MemoryRenderBuffer::from_slice(
                                &frame.pixels_rgba,
                                Fourcc::Argb8888,
                                (frame.width as i32, frame.height as i32),
                                cursor_scale as i32,
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
                    (
                        (frame.xhot / cursor_scale) as i32,
                        (frame.yhot / cursor_scale) as i32,
                    )
                        .into()
                };

                pointer_element.set_status(effective_cursor_status);

                let cursor_location = (pointer_pos - output_geo.loc.to_f64() - hotspot.to_f64())
                    .to_physical(scale)
                    .to_i32_round();

                cursor_pointer_elements.extend(
                    pointer_element.render_elements::<PointerRenderElement<GlesRenderer>>(
                        &mut backend.renderer,
                        cursor_location,
                        scale,
                        1.0,
                    ),
                );
            }
        }
        let cursor_elapsed_ms = cursor_started_at.elapsed().as_secs_f64() * 1000.0;
        timing.cursor_elapsed_ms = cursor_elapsed_ms;

        // Fullscreen fast path: when the topmost window is settled fullscreen
        // on this output, only overlay layers and the raw client surface are
        // rendered (Hyprland-style stacking: fullscreen above Top, below
        // Overlay). Keep this separate from the direct-scanout decision below:
        // a notification/OSD overlay must not unfullscreen the window, but it
        // must temporarily force compositing and synced flips until it goes
        // away.
        let fullscreen_window = fullscreen_scanout_window(
            space,
            window_decorations,
            &windows_top_to_bottom,
            closing_snapshots.len(),
            &output,
            output_geo,
            scale,
        );
        note_fullscreen_fast_path_transition(output.name().as_str(), fullscreen_window.is_some());
        // Overlay-layer backdrop effects must sample the fullscreen window
        // instead of the regular window stack while the fast path is active.
        let fullscreen_backdrop_windows: Vec<smithay::desktop::Window>;
        let upper_layer_backdrop_windows: &[smithay::desktop::Window] =
            if let Some(window) = fullscreen_window.as_ref() {
                fullscreen_backdrop_windows = vec![window.clone()];
                &fullscreen_backdrop_windows
            } else {
                &windows_top_to_bottom
            };

        let mut scene_elements: Vec<TtyRenderElements> = Vec::new();
        let upper_layers_started_at = Instant::now();
        let upper_layer_elements = {
            timescope::scope!("tty upper layer scene");
            upper_layer_scene_elements(
                &mut backend.renderer,
                space,
                window_decorations,
                &state.window_source_damage,
                &state.lower_layer_source_damage,
                &state.upper_layer_source_damage,
                state.lower_layer_scene_generation,
                &state.configured_layer_effects,
                state.configured_background_effect.as_ref(),
                &output,
                output_geo,
                scale,
                upper_layer_backdrop_windows,
                fullscreen_window.is_some(),
                &mut state.layer_backdrop_cache,
                &mut state.layer_framebuffer_effect_states,
                &mut state.layer_effect_cache,
            )?
        };
        let mut fullscreen_overlay_visible =
            fullscreen_window.is_some() && !upper_layer_elements.is_empty();
        scene_elements.extend(upper_layer_elements);
        let upper_layers_elapsed_ms = upper_layers_started_at.elapsed().as_secs_f64() * 1000.0;
        timing.upper_layers_elapsed_ms = upper_layers_elapsed_ms;
        let closing_snapshots_started_at = Instant::now();
        {
            timescope::scope!("tty closing snapshots");
            scene_elements.extend(
                closing_snapshot_elements(
                    &mut backend.renderer,
                    &output,
                    &closing_snapshots,
                    output_geo,
                    scale,
                )
                .into_iter(),
            );
        }
        let closing_snapshots_elapsed_ms =
            closing_snapshots_started_at.elapsed().as_secs_f64() * 1000.0;
        timing.closing_snapshots_elapsed_ms = closing_snapshots_elapsed_ms;
        let mut window_loop_elapsed_ms = 0.0f64;
        let mut max_window_elapsed_ms = 0.0f64;
        let mut max_window_id: Option<String> = None;
        let mut snapshot_capture_elapsed_ms = 0.0f64;
        let mut snapshot_capture_count = 0usize;
        // Windows in snapshot mode (scaled visual_transform) whose transform changed
        // since the previous frame — these are actively animating and need full-rate callbacks.
        // Windows whose transform is unchanged are stationary in snapshot mode and can be
        // throttled (see snapshot fix below).
        let mut snapshot_transform_changed_ids: std::collections::HashSet<String> =
            std::collections::HashSet::new();

        let close_debug = std::env::var_os("SHOJI_CLOSE_DEBUG").is_some();
        if close_debug && !closing_window_snapshots.is_empty() {
            tracing::info!(
                output = %output.name(),
                closing_count = closing_window_snapshots.len(),
                closing_ids = ?closing_window_snapshots.keys().collect::<Vec<_>>(),
                "close debug: closing snapshots active"
            );
        }
        {
            timescope::scope!("tty window loop");
            for (_window_index, window) in windows_top_to_bottom.iter().enumerate() {
                timescope::scope!("tty window");
                // Fullscreen fast path: every other window is fully occluded;
                // the fullscreen window itself renders as a bare surface tree +
                // popups (no decorations, no effects) so the frame can collapse
                // to a single scanout-capable element.
                if let Some(fullscreen_fast_path_window) = fullscreen_window.as_ref() {
                    if window != fullscreen_fast_path_window {
                        continue;
                    }
                    let Some(window_location) = space.element_location(window) else {
                        continue;
                    };
                    let physical_location =
                        (window_location - output_geo.loc).to_physical_precise_round(scale);
                    let popup_elements = window_render::popup_elements(
                        window,
                        &mut backend.renderer,
                        physical_location,
                        scale,
                        1.0,
                    );
                    let surface_elements = window_render::surface_elements(
                        window,
                        &mut backend.renderer,
                        physical_location,
                        scale,
                        1.0,
                    );
                    scene_elements
                        .extend(popup_elements.into_iter().map(TtyRenderElements::Window));
                    scene_elements
                        .extend(surface_elements.into_iter().map(TtyRenderElements::Window));
                    continue;
                }
                let window_started_at = Instant::now();
                let mut window_timing = TtyWindowTimingMetrics::default();
                let Some(window_location) = space.element_location(window) else {
                    continue;
                };
                let Some(window_id) = window_decorations
                    .get(window)
                    .map(|decoration| decoration.snapshot.id.clone())
                else {
                    continue;
                };
                if let Some(decoration) = window_decorations.get(window) {
                    let render_allowed =
                        decoration.managed_window_allows_render_on_output(output.name().as_str());
                    if !render_allowed {
                        log_kinetic_window_render_state_debug(
                            "live-skip-render",
                            &output.name(),
                            decoration,
                            output_geo,
                            scale,
                            None,
                            render_allowed,
                        );
                        continue;
                    }
                    log_kinetic_window_render_state_debug(
                        "live-before-visual",
                        &output.name(),
                        decoration,
                        output_geo,
                        scale,
                        None,
                        render_allowed,
                    );
                } else {
                    continue;
                }
                if closing_window_snapshots.contains_key(&window_id) {
                    if close_debug {
                        tracing::info!(
                            output = %output.name(),
                            window_id = %window_id,
                            "close debug: live loop skipping window (in closing_window_snapshots)"
                        );
                    }
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
                if let Some(decoration) = window_decorations.get(window) {
                    log_kinetic_window_render_state_debug(
                        "live-after-visual",
                        &output.name(),
                        decoration,
                        output_geo,
                        scale,
                        Some(visual_state),
                        true,
                    );
                }
                let snap_scale = Scale::from((
                    scale.x * visual_state.scale.x.max(0.0),
                    scale.y * visual_state.scale.y.max(0.0),
                ));
                let client_physical_geometry =
                    window_decorations.get(window).and_then(|decoration| {
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
                let direct_surface_lookup_started_at = Instant::now();
                let direct_surface_count = window_render::surface_elements(
                    window,
                    &mut backend.renderer,
                    physical_location,
                    scale,
                    1.0,
                )
                .len();
                window_timing.direct_surface_lookup_ms =
                    direct_surface_lookup_started_at.elapsed().as_secs_f64() * 1000.0;
                if std::env::var_os("SHOJI_SOURCE_DAMAGE_DEBUG").is_some() {
                    let title = window_decorations
                        .get(window)
                        .map(|d| d.snapshot.title.clone())
                        .unwrap_or_default();
                    let has_backdrop_source_probe = direct_surface_count > 0
                        || live_window_snapshots.contains_key(&window_id)
                        || complete_window_snapshots.contains_key(&window_id);
                    let decoration_ready_probe = windows_ready_for_decoration.contains(&window_id);
                    tracing::info!(
                        output = %output.name(),
                        window_id = %window_id,
                        title = %title,
                        direct_surface_count,
                        skipping = direct_surface_count == 0,
                        has_backdrop_source = has_backdrop_source_probe,
                        decoration_ready = decoration_ready_probe,
                        "per-window loop direct_surface_count probe"
                    );
                }
                if direct_surface_count == 0 {
                    if close_debug {
                        tracing::info!(
                            output = %output.name(),
                            window_id = %window_id,
                            "close debug: live loop skipping window (direct_surface_count==0)"
                        );
                    }
                    if output_render_debug_enabled() {
                        let title = window_decorations
                            .get(window)
                            .map(|d| d.snapshot.title.as_str());
                        tracing::info!(
                            output = %output.name(),
                            window_id = %window_id,
                            title = ?title,
                            physical_location = ?physical_location,
                            "output_render_debug: SKIPPED (direct_surface_count==0)"
                        );
                    }
                    continue;
                }
                if close_debug {
                    tracing::info!(
                        output = %output.name(),
                        window_id = %window_id,
                        direct_surface_count,
                        "close debug: live loop rendering window"
                    );
                }
                let has_backdrop_source = direct_surface_count > 0
                    || live_window_snapshots.contains_key(&window_id)
                    || complete_window_snapshots.contains_key(&window_id);
                let decoration_ready = windows_ready_for_decoration.contains(&window_id);
                if !has_backdrop_source {
                    continue;
                }
                let use_full_window_snapshot = requires_full_window_snapshot(visual_state);
                let used_transform_snapshot_last_frame =
                    transform_snapshot_window_ids.contains(&window_id);
                if use_full_window_snapshot {
                    frame_transform_snapshot_window_count =
                        frame_transform_snapshot_window_count.saturating_add(1);
                }
                let snapshot_id = window_decorations
                    .get(window)
                    .map(|decoration| decoration.snapshot.id.clone());
                let window_has_snapshot_damage = snapshot_id
                    .as_ref()
                    .is_some_and(|snapshot_id| snapshot_dirty_window_ids.contains(snapshot_id));
                if frame_liveness_debug_enabled() {
                    let snapshot = window_decorations
                        .get(window)
                        .map(|decoration| &decoration.snapshot);
                    tracing::info!(
                        output = %output.name(),
                        window_id = %window_id,
                        title = ?snapshot.map(|snapshot| snapshot.title.as_str()),
                        app_id = ?snapshot.and_then(|snapshot| snapshot.app_id.as_deref()),
                        direct_surface_count,
                        use_full_window_snapshot,
                        used_transform_snapshot_last_frame,
                        window_has_snapshot_damage,
                        translation_x = visual_state.translation.x,
                        translation_y = visual_state.translation.y,
                        scale_x = visual_state.scale.x,
                        scale_y = visual_state.scale.y,
                        opacity = visual_state.opacity,
                        "tty frame liveness: window snapshot decision",
                    );
                }
                if use_full_window_snapshot && window_has_snapshot_damage {
                    frame_snapshot_damage_window_count =
                        frame_snapshot_damage_window_count.saturating_add(1);
                }
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
                // Track whether this window's visual_transform changed since the previous frame.
                // If it changed the window is actively animating; if not, it is stationary in
                // snapshot mode.  The snapshot fix below only restores primary_scanout_output for
                // animating windows so that stationary snapshot windows remain throttled.
                if use_full_window_snapshot {
                    if let Some(sid) = snapshot_id.as_deref() {
                        if let Some(decoration) = window_decorations.get(window) {
                            let prev = previous_snapshot_visual_transform(
                                sid,
                                output.name().as_str(),
                                decoration.visual_transform,
                            );
                            let changed = prev
                                .map(|p| p != decoration.visual_transform)
                                .unwrap_or(true); // first frame → assume changed
                            if changed {
                                snapshot_transform_changed_ids.insert(sid.to_string());
                            }
                        }
                    }
                }
                if use_full_window_snapshot {
                    transform_snapshot_window_ids.insert(window_id.clone());
                } else {
                    transform_snapshot_window_ids.remove(&window_id);
                    complete_window_snapshot_trackers.remove(&window_id);
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
                let decoration_phase_started_at = Instant::now();
                if decoration_ready {
                    let backdrop_started_at = Instant::now();
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
                        !use_full_window_snapshot,
                    );
                    if let Some(effect_config) = state.configured_background_effect.as_ref() {
                        if use_full_window_snapshot
                            || !effect_config.effect.supports_framebuffer_backdrop()
                        {
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
                    }
                    window_timing.backdrop_ms =
                        backdrop_started_at.elapsed().as_secs_f64() * 1000.0;
                    let background_started_at = Instant::now();
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
                    if !use_full_window_snapshot
                        && let Some(effect_config) = state.configured_background_effect.as_ref()
                        && effect_config.effect.supports_framebuffer_backdrop()
                    {
                        for (order, element) in
                            configured_background_framebuffer_effect_elements_for_window(
                                &mut backend.renderer,
                                window_decorations,
                                window,
                                output_geo,
                                scale,
                                visual_state.opacity,
                                effect_config,
                            )
                        {
                            if let Some(root_origin) = root_origin {
                                ordered_backdrop_elements.extend(
                                    transform_decoration_elements(
                                        vec![decoration::DecorationSceneElements::Backdrop(
                                            element,
                                        )],
                                        root_origin,
                                        composition_visual,
                                    )?
                                    .into_iter()
                                    .map(|item| (order, item)),
                                );
                            }
                        }
                    }
                    if let Some(decoration_state) = window_decorations.get_mut(window) {
                        let mut ordered_background_items =
                        decoration::ordered_background_elements_for_window_with_framebuffer_backdrops(
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
                            !use_full_window_snapshot,
                        )
                        .inspect_err(|error| {
                            warn!(?error, "failed to build decoration background elements");
                        })
                        .unwrap_or_default();
                        ordered_background_items.sort_by_key(|(order, _)| *order);
                        for (order, element) in ordered_background_items {
                            if let Some(root_origin) = root_origin {
                                let render_as_backdrop = matches!(
                                    element,
                                    decoration::DecorationSceneElements::Backdrop(_)
                                );
                                let debug_stable = if std::env::var_os("SHOJI_GAP_DEBUG").is_some()
                                {
                                    decoration_state
                                        .buffers
                                        .iter()
                                        .find(|buffer| buffer.order == order)
                                        .map(|buffer| {
                                            (buffer.stable_key.clone(), buffer.source_kind)
                                        })
                                } else {
                                    None
                                };
                                let pre_transform_geometry = if std::env::var_os("SHOJI_GAP_DEBUG")
                                    .is_some()
                                {
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
                                if let (
                                    Some((stable_key, source_kind)),
                                    Some(pre_transform_geometry),
                                ) = (debug_stable, pre_transform_geometry)
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
                                    if render_as_backdrop {
                                        snapshot_backdrop_items
                                            .extend(items.into_iter().map(|item| (order, item)));
                                    } else {
                                        snapshot_ui_items
                                            .extend(items.into_iter().map(|item| (order, item)));
                                    }
                                } else if render_as_backdrop {
                                    ordered_backdrop_elements
                                        .extend(items.into_iter().map(|item| (order, item)));
                                } else {
                                    ordered_ui_elements
                                        .extend(items.into_iter().map(|item| (order, item)));
                                }
                            }
                        }
                    }
                    window_timing.background_ms =
                        background_started_at.elapsed().as_secs_f64() * 1000.0;

                    let icon_started_at = Instant::now();
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
                            let debug_stable = if std::env::var_os("SHOJI_GAP_DEBUG").is_some() {
                                Some(
                                    window_decorations
                                        .get(window)
                                        .and_then(|decoration| {
                                            decoration
                                                .icon_buffers
                                                .iter()
                                                .find(|buffer| buffer.order == order)
                                                .map(|buffer| buffer.stable_key.clone())
                                        })
                                        .unwrap_or_else(|| format!("icon-order-{order}")),
                                )
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
                            let items = transform_text_elements(
                                vec![element],
                                root_origin,
                                composition_visual,
                            )?;
                            if let (Some(stable_key), Some(pre_transform_geometry)) =
                                (debug_stable, pre_transform_geometry)
                            {
                                let post_transform_geometry = items.first().map(|item| {
                                    smithay::backend::renderer::element::Element::geometry(
                                        item, scale,
                                    )
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
                    window_timing.icon_ms = icon_started_at.elapsed().as_secs_f64() * 1000.0;

                    let text_started_at = Instant::now();
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
                            let debug_stable = if std::env::var_os("SHOJI_GAP_DEBUG").is_some() {
                                Some(
                                    window_decorations
                                        .get(window)
                                        .and_then(|decoration| {
                                            decoration
                                                .text_buffers
                                                .iter()
                                                .find(|buffer| buffer.order == order)
                                                .map(|buffer| buffer.stable_key.clone())
                                        })
                                        .unwrap_or_else(|| format!("label-order-{order}")),
                                )
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
                            let items = transform_text_elements(
                                vec![element],
                                root_origin,
                                composition_visual,
                            )?;
                            if let (Some(stable_key), Some(pre_transform_geometry)) =
                                (debug_stable, pre_transform_geometry)
                            {
                                let post_transform_geometry = items.first().map(|item| {
                                    smithay::backend::renderer::element::Element::geometry(
                                        item, scale,
                                    )
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
                    window_timing.text_ms = text_started_at.elapsed().as_secs_f64() * 1000.0;

                    ordered_ui_elements.sort_by_key(|(order, _)| *order);
                    ordered_backdrop_elements.sort_by_key(|(order, _)| *order);
                    snapshot_ui_items.sort_by_key(|(order, _)| *order);
                    snapshot_backdrop_items.sort_by_key(|(order, _)| *order);
                    if std::env::var_os("SHOJI_TRANSFORM_SNAPSHOT_DEBUG").is_some() {
                        let first_backdrop =
                            ordered_backdrop_elements.first().map(|(_, element)| {
                                smithay::backend::renderer::element::Element::geometry(
                                    element, scale,
                                )
                            });
                        let first_snapshot_backdrop =
                            snapshot_backdrop_items.first().map(|(_, element)| {
                                smithay::backend::renderer::element::Element::geometry(
                                    element, scale,
                                )
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
                        let first_backdrop =
                            ordered_backdrop_elements.first().map(|(_, element)| {
                                smithay::backend::renderer::element::Element::geometry(
                                    element, scale,
                                )
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
                        let backdrop_fill_delta =
                            first_backdrop.zip(first_fill).map(|(backdrop, fill)| {
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
                window_timing.decoration_phase_ms =
                    decoration_phase_started_at.elapsed().as_secs_f64() * 1000.0;

                let content_clip = window_decorations
                    .get(window)
                    .and_then(|decoration| decoration.content_clip);
                let clip_all_client_surfaces = window_decorations
                    .get(window)
                    .is_some_and(|decoration| decoration.managed_window.force_rect_size);
                if let Some(decoration) = window_decorations.get(window) {
                    log_managed_rect_physical_debug(
                        &window_id,
                        &output.name(),
                        decoration.layout.root.rect,
                        content_clip,
                        output_geo,
                        scale,
                    );
                }

                let client_phase_started_at = Instant::now();
                let client_elements = if use_full_window_snapshot {
                    let full_snapshot_scene_started_at = Instant::now();
                    let mut snapshot_scene = Vec::new();
                    if let Some(content_clip) = content_clip.filter(|clip| clip.clips_surface) {
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
                            clip_all_client_surfaces,
                        )
                        .unwrap_or_default();
                        let mut root_raw_element = None;
                        for element in clipped {
                            match element {
                                window_render::WindowClipElement::Clipped(element) => {
                                    snapshot_scene.push(TtyRenderElements::Clipped(element));
                                    if !clip_all_client_surfaces {
                                        break;
                                    }
                                }
                                window_render::WindowClipElement::Raw(element)
                                    if root_raw_element.is_none() =>
                                {
                                    root_raw_element = Some(element);
                                }
                                window_render::WindowClipElement::Raw(_) => {}
                            }
                        }
                        if snapshot_scene.is_empty()
                            && let Some(element) = root_raw_element
                        {
                            snapshot_scene.push(TtyRenderElements::Window(element));
                        }
                    } else {
                        snapshot_scene.extend(
                            window_render::root_surface_elements(
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
                    snapshot_scene
                        .extend(snapshot_ui_items.into_iter().map(|(_, element)| element));
                    snapshot_scene.extend(
                        snapshot_backdrop_items
                            .into_iter()
                            .map(|(_, element)| element),
                    );
                    let full_rect = window_decorations.get(window).map(|decoration| {
                        window_render::snapshot_bounds(
                            window,
                            window_location,
                            decoration.layout.root.rect,
                            decoration.content_clip,
                        )
                    });
                    let snapshot_scene_signature =
                        crate::backend::snapshot::render_element_scene_signature(
                            &snapshot_scene,
                            scale,
                        );
                    window_timing.full_snapshot_scene_ms =
                        full_snapshot_scene_started_at.elapsed().as_secs_f64() * 1000.0;
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
                            if let Some(mut existing) = complete_window_snapshots
                                .get(&window_id)
                                .cloned()
                                .filter(|snapshot| {
                                    snapshot.scene_signature == snapshot_scene_signature
                                })
                            {
                                // The texture content is still valid (same scene), but the
                                // window may have moved to a different output since the snapshot
                                // was captured. Update the rect so that live_snapshot_element
                                // can compute the correct position relative to the new output_geo
                                // and passes the intersection check.
                                existing.rect = full_rect;
                                complete_window_snapshots.insert(window_id.clone(), existing.clone());
                                if std::env::var_os("SHOJI_TRANSFORM_SNAPSHOT_DEBUG").is_some() {
                                    let commit = existing.damage.lock().unwrap().current_commit();
                                    tracing::info!(
                                        window_id = %window_id,
                                        commit = ?commit,
                                        rect = ?full_rect,
                                        "transform snapshot tty complete snapshot cache hit (rect updated)"
                                    );
                                }
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
                            let prev_commit = existing_complete.as_ref()
                                .map(|s| s.damage.lock().unwrap().current_commit());
                            tracing::info!(
                                window_id = %window_id,
                                full_rect = ?full_rect,
                                snapshot_scene_count = snapshot_scene.len(),
                                first_snapshot_geometry = ?first_snapshot_geometry,
                                prev_commit = ?prev_commit,
                                window_has_snapshot_damage,
                                "transform snapshot tty assembled current-frame scene"
                            );
                        }
                        let capture_started_at = Instant::now();
                        let captured = {
                            let tracker = complete_window_snapshot_trackers
                                .entry(window_id.clone())
                                .or_insert_with(|| {
                                    smithay::backend::renderer::damage::OutputDamageTracker::new(
                                        (0, 0),
                                        1.0,
                                        Transform::Normal,
                                    )
                                });
                            capture_snapshot_from_output_elements(
                                &mut backend.renderer,
                                output_geo,
                                full_rect,
                                scale,
                                existing_complete,
                                tracker,
                                &snapshot_scene,
                            )
                        };
                        window_timing.full_snapshot_capture_ms +=
                            capture_started_at.elapsed().as_secs_f64() * 1000.0;
                        captured.ok().flatten().map(|mut snapshot| {
                            if std::env::var_os("SHOJI_TRANSFORM_SNAPSHOT_DEBUG").is_some() {
                                let commit = snapshot.damage.lock().unwrap().current_commit();
                                tracing::info!(
                                    window_id = %window_id,
                                    commit = ?commit,
                                    "transform snapshot tty complete snapshot rebuilt"
                                );
                            }
                            snapshot.scene_signature = snapshot_scene_signature;
                            complete_window_snapshots.insert(window_id.clone(), snapshot.clone());
                            snapshot
                        })
                    })
                    .and_then(|snapshot| {
                        if std::env::var_os("SHOJI_TRANSFORM_SNAPSHOT_DEBUG").is_some() {
                            let commit = snapshot.damage.lock().unwrap().current_commit();
                            tracing::info!(
                                window_id = %window_id,
                                commit = ?commit,
                                "transform snapshot tty creating live element from snapshot"
                            );
                        }
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
                            let expected_right = ((snapped_clip.x + snapped_clip.width) as f64
                                * scale.x)
                                .round() as i32;
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
                                let titlebar_shader_clip_precise =
                                    titlebar_shader.and_then(|buffer| {
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
                                let content_clip_physical = smithay::utils::Rectangle::<
                                    i32,
                                    smithay::utils::Physical,
                                >::new(
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
                                let button_delta =
                                    match (border_inner_physical, first_button_physical) {
                                        (Some(inner), Some(button)) => Some((
                                            button.loc.x - inner.loc.x,
                                            button.loc.y - inner.loc.y,
                                            (inner.loc.x + inner.size.w)
                                                - (button.loc.x + button.size.w),
                                            (inner.loc.y + inner.size.h)
                                                - (button.loc.y + button.size.h),
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
                        clip_all_client_surfaces,
                    )
                    .inspect_err(|error| {
                        warn!(?error, "failed to build clipped surface elements");
                    })
                    .unwrap_or_default();
                    let bypass_clip = std::env::var_os("SHOJI_GAP_BYPASS_CLIP").is_some();
                    if std::env::var_os("SHOJI_GAP_DEBUG").is_some() {
                        let first_geometry = clipped.first().map(|element| match element {
                            window_render::WindowClipElement::Clipped(element) => {
                                smithay::backend::renderer::element::Element::geometry(
                                    element, scale,
                                )
                            }
                            window_render::WindowClipElement::Raw(element) => {
                                smithay::backend::renderer::element::Element::geometry(
                                    element, scale,
                                )
                            }
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
                            let expected_right = ((snapped_clip.x + snapped_clip.width) as f64
                                * scale.x)
                                .round() as i32;
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
                                            (client.loc.x + client.size.w)
                                                - (fill.loc.x + fill.size.w),
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
                                let local_geometry =
                                    crate::backend::visual::relative_physical_rect_from_root_precise(
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
                                            && ui_key.as_bytes().get(fill_base.len())
                                                == Some(&b'/'))
                                        .then_some((
                                            *order,
                                            fill_key.clone(),
                                            *source_kind,
                                            *geometry,
                                        ))
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
                    let transformed: Vec<TtyRenderElements> = if bypass_clip {
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
                                smithay::backend::renderer::element::Element::geometry(
                                    element, scale,
                                )
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
                        clipped
                            .into_iter()
                            .flat_map(|element| match element {
                                window_render::WindowClipElement::Clipped(element) => {
                                    transform_clipped_elements(vec![element], visual_state)
                                }
                                window_render::WindowClipElement::Raw(element) => {
                                    transform_window_elements(
                                        vec![element],
                                        visual_state,
                                        TtyRenderElements::Window,
                                        TtyRenderElements::TransformedWindow,
                                    )
                                }
                            })
                            .collect()
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
                window_timing.client_phase_ms =
                    client_phase_started_at.elapsed().as_secs_f64() * 1000.0;
                let mut current_window_elements: Vec<TtyRenderElements> = Vec::new();
                let popup_phase_started_at = Instant::now();
                if is_identity_visual_geometry(visual_state) {
                    // Steady state: compose per-popup effects. Effect elements
                    // cannot ride the window animation transform, so this path is
                    // only taken when the visual transform is identity.
                    current_window_elements.extend(composed_window_popup_scene_elements(
                        &mut backend.renderer,
                        &output,
                        output_geo,
                        scale,
                        window,
                        physical_location,
                        visual_state.opacity,
                        &state.configured_popup_effects,
                        &mut state.popup_effect_cache,
                        &mut state.popup_framebuffer_effect_states,
                        state.configured_background_effect.as_ref(),
                    ));
                } else {
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
                    current_window_elements.extend(popup_elements);
                }
                if use_full_window_snapshot {
                    current_window_elements.extend(non_root_surface_elements_for_window(
                        window,
                        &mut backend.renderer,
                        physical_location,
                        client_physical_geometry,
                        output_geo.loc,
                        scale,
                        scale,
                        visual_state,
                        visual_state.opacity,
                        content_clip,
                    ));
                }
                window_timing.popup_phase_ms =
                    popup_phase_started_at.elapsed().as_secs_f64() * 1000.0;
                if output_render_debug_enabled() {
                    let title = window_decorations
                        .get(window)
                        .map(|d| d.snapshot.title.as_str());
                    let root_rect = window_decorations.get(window).map(|d| d.layout.root.rect);
                    let content_clip_rect = window_decorations
                        .get(window)
                        .and_then(|d| d.content_clip)
                        .map(|c| c.rect);
                    tracing::info!(
                        output = %output.name(),
                        window_id = %window_id,
                        title = ?title,
                        use_full_window_snapshot,
                        direct_surface_count,
                        client_elements_count = client_elements.len(),
                        physical_location = ?physical_location,
                        output_geo = ?output_geo,
                        root_rect = ?root_rect,
                        content_clip_rect = ?content_clip_rect,
                        scale_x = scale.x,
                        scale_y = scale.y,
                        visual_scale_x = visual_state.scale.x,
                        visual_scale_y = visual_state.scale.y,
                        "output_render_debug: window rendered"
                    );
                }
                // Window-source effects sample only the top-level window. `Full` means
                // root surface plus SSD decoration, but not popup/subsurface content.
                let source_clip_scale = if use_full_window_snapshot {
                    scale
                } else {
                    snap_scale
                };
                let (needs_root_surface_source, needs_full_window_source) = window_decorations
                    .get(window)
                    .and_then(|decoration| decoration.window_effects.as_ref())
                    .map(|effects| {
                        let slots = [
                            effects.behind.as_ref(),
                            effects.behind_root_surface.as_ref(),
                            effects.in_front.as_ref(),
                            effects.replace.as_ref(),
                        ];
                        let needs_full =
                            slots.iter().flatten().any(|effect| {
                                matches!(
                                    &effect.effect.input,
                                    EffectInput::WindowSource(WindowSourceInclude::Full)
                                )
                            }) || effects.behind_root_surface.as_ref().is_some_and(|effect| {
                                !matches!(
                                    &effect.effect.input,
                                    EffectInput::WindowSource(WindowSourceInclude::RootSurface)
                                )
                            });
                        let needs_root = needs_full
                            || slots.iter().flatten().any(|effect| {
                                matches!(
                                    &effect.effect.input,
                                    EffectInput::WindowSource(WindowSourceInclude::RootSurface)
                                )
                            });
                        (needs_root, needs_full)
                    })
                    .unwrap_or_default();
                let root_surface_source_elements = if needs_root_surface_source {
                    root_surface_source_elements_for_window(
                        window,
                        &mut backend.renderer,
                        physical_location,
                        client_physical_geometry,
                        output_geo.loc,
                        scale,
                        source_clip_scale,
                        visual_state,
                        visual_state.opacity,
                        content_clip,
                    )
                } else {
                    Vec::new()
                };
                let mut full_window_source_elements = if needs_full_window_source {
                    root_surface_source_elements_for_window(
                        window,
                        &mut backend.renderer,
                        physical_location,
                        client_physical_geometry,
                        output_geo.loc,
                        scale,
                        source_clip_scale,
                        visual_state,
                        visual_state.opacity,
                        content_clip,
                    )
                } else {
                    Vec::new()
                };
                if needs_full_window_source
                    && decoration_ready
                    && let Some(root_origin) = root_origin
                {
                    let source_alpha = visual_state.opacity;
                    let mut source_ui_elements: Vec<(usize, TtyRenderElements)> = Vec::new();
                    let mut source_backdrop_elements: Vec<(usize, TtyRenderElements)> = Vec::new();

                    let mut source_backdrop_items = backdrop_shader_elements_for_window(
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
                        source_alpha,
                        decoration_ready,
                        false,
                        false,
                    );
                    if let Some(effect_config) = state.configured_background_effect.as_ref() {
                        source_backdrop_items.extend(
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
                                source_alpha,
                                effect_config,
                                false,
                            )
                            .into_iter()
                            .map(|(order, element)| (order, element, true)),
                        );
                    }
                    for (order, element, render_as_backdrop) in source_backdrop_items.drain(..) {
                        let items =
                            transform_backdrop_elements(vec![element], root_origin, visual_state)?;
                        if render_as_backdrop {
                            source_backdrop_elements
                                .extend(items.into_iter().map(|item| (order, item)));
                        } else {
                            source_ui_elements.extend(items.into_iter().map(|item| (order, item)));
                        }
                    }

                    if let Some(decoration_state) = window_decorations.get_mut(window) {
                        let mut source_background_items =
                            decoration::ordered_background_elements_for_window(
                                &mut backend.renderer,
                                decoration_state,
                                output_geo,
                                source_clip_scale,
                                source_alpha,
                            )
                            .inspect_err(|error| {
                                warn!(
                                    ?error,
                                    "failed to build window effect source decoration backgrounds"
                                );
                            })
                            .unwrap_or_default();
                        source_background_items.sort_by_key(|(order, _)| *order);
                        for (order, element) in source_background_items {
                            source_ui_elements.extend(
                                transform_decoration_elements(
                                    vec![element],
                                    root_origin,
                                    visual_state,
                                )?
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
                        source_alpha,
                    )? {
                        source_ui_elements.extend(
                            transform_text_elements(vec![element], root_origin, visual_state)?
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
                        source_alpha,
                    )? {
                        source_ui_elements.extend(
                            transform_text_elements(vec![element], root_origin, visual_state)?
                                .into_iter()
                                .map(|item| (order, item)),
                        );
                    }

                    source_ui_elements.sort_by_key(|(order, _)| *order);
                    source_backdrop_elements.sort_by_key(|(order, _)| *order);
                    full_window_source_elements
                        .extend(source_ui_elements.into_iter().map(|(_, element)| element));
                    full_window_source_elements.extend(
                        source_backdrop_elements
                            .into_iter()
                            .map(|(_, element)| element),
                    );
                }

                let mut original_window_body_elements: Vec<TtyRenderElements> = Vec::new();
                original_window_body_elements.extend(client_elements.into_iter());
                original_window_body_elements
                    .extend(ordered_ui_elements.into_iter().map(|(_, element)| element));
                original_window_body_elements.extend(
                    ordered_backdrop_elements
                        .into_iter()
                        .map(|(_, element)| element),
                );
                if let Some(decoration) = window_decorations.get(window) {
                    log_gap_final_composite_readback(
                        &mut backend.renderer,
                        scale,
                        &original_window_body_elements,
                        decoration,
                        output_geo,
                        visual_state,
                        &output.name(),
                        &window_id,
                    );
                }
                let replace_effect_slot = window_decorations
                    .get(window)
                    .and_then(|decoration| decoration.window_effects.as_ref())
                    .and_then(|effects| effects.replace.as_ref())
                    .cloned();
                let replace_effects = replace_effect_slot.as_ref()
                .and_then(|effect| {
                    let decoration = window_decorations.get(window)?;
                    let (rect, source_elements): (_, &[TtyRenderElements]) =
                        match &effect.effect.input {
                            EffectInput::WindowSource(WindowSourceInclude::Full) => (
                                transformed_root_rect(
                                    decoration.layout.root.rect,
                                    decoration.visual_transform,
                                ),
                                &full_window_source_elements,
                            ),
                            EffectInput::WindowSource(WindowSourceInclude::RootSurface) => (
                                transformed_rect(
                                    decoration.client_rect,
                                    decoration.layout.root.rect,
                                    decoration.visual_transform,
                                ),
                                &root_surface_source_elements,
                            ),
                            _ => (
                                transformed_root_rect(
                                    decoration.layout.root.rect,
                                    decoration.visual_transform,
                                ),
                                &original_window_body_elements,
                            ),
                        };
                    let signature = window_effect_signature(
                        "replace",
                        rect,
                        effect,
                        scale,
                        source_elements,
                    );
                    let (element_id, commit_counter) = window_decorations
                        .get_mut(window)
                        .map(|decoration| {
                            window_effect_element_state(
                                decoration,
                                format!("replace@{}", output.name()),
                                signature,
                            )
                        })?;
                    window_effect_elements(
                        &mut backend.renderer,
                        &output,
                        output_geo,
                        scale,
                        &window_id,
                        "replace",
                        element_id,
                        commit_counter,
                        rect,
                        effect,
                        source_elements,
                    )
                    .inspect_err(|error| {
                        warn!(window_id = %window_id, ?error, "failed to build replacement window effect");
                    })
                    .ok()
            });
                if let Some(replace_effects) = replace_effects {
                    if !use_full_window_snapshot {
                        current_window_elements.extend(non_root_surface_elements_for_window(
                            window,
                            &mut backend.renderer,
                            physical_location,
                            client_physical_geometry,
                            output_geo.loc,
                            scale,
                            source_clip_scale,
                            visual_state,
                            visual_state.opacity,
                            content_clip,
                        ));
                    }
                    current_window_elements.extend(replace_effects);
                } else {
                    current_window_elements.extend(original_window_body_elements);
                }
                if window_effect_debug_enabled() {
                    let effect_summary = window_decorations.get(window).map(|decoration| {
                        decoration.window_effects.as_ref().map(|effects| {
                            (
                                effects.behind.is_some(),
                                effects.behind_root_surface.is_some(),
                                effects.in_front.is_some(),
                                effects.replace.is_some(),
                                effects
                                    .behind_root_surface
                                    .as_ref()
                                    .or(effects.behind.as_ref())
                                    .map(|effect| effect.outsets),
                            )
                        })
                    });
                    info!(
                        output = %output.name(),
                        window_id = %window_id,
                        use_full_window_snapshot,
                        current_window_element_count = current_window_elements.len(),
                        effect_summary = ?effect_summary,
                        "window effect debug: window render effect state"
                    );
                }
                let behind_root_surface_effect_slot = window_decorations
                    .get(window)
                    .and_then(|decoration| decoration.window_effects.as_ref())
                    .and_then(|effects| effects.behind_root_surface.as_ref())
                    .cloned();
                let behind_root_surface_effects =
                    behind_root_surface_effect_slot.as_ref().and_then(|effect| {
                        let decoration = window_decorations.get(window)?;
                        let (rect, source_elements): (_, &[TtyRenderElements]) =
                            match &effect.effect.input {
                                EffectInput::WindowSource(WindowSourceInclude::Full) => (
                                    transformed_root_rect(
                                        decoration.layout.root.rect,
                                        decoration.visual_transform,
                                    ),
                                    &full_window_source_elements,
                                ),
                                EffectInput::WindowSource(WindowSourceInclude::RootSurface) => (
                                    transformed_rect(
                                        decoration.client_rect,
                                        decoration.layout.root.rect,
                                        decoration.visual_transform,
                                    ),
                                    &root_surface_source_elements,
                                ),
                                _ => (
                                    transformed_root_rect(
                                        decoration.layout.root.rect,
                                        decoration.visual_transform,
                                    ),
                                    &full_window_source_elements,
                                ),
                            };
                        let signature = window_effect_signature(
                            "behind-root-surface",
                            rect,
                            effect,
                            scale,
                            source_elements,
                        );
                        let (element_id, commit_counter) =
                            window_decorations.get_mut(window).map(|decoration| {
                                window_effect_element_state(
                                    decoration,
                                    format!("behind-root-surface@{}", output.name()),
                                    signature,
                                )
                            })?;
                        window_effect_elements(
                            &mut backend.renderer,
                            &output,
                            output_geo,
                            scale,
                            &window_id,
                            "behind-root-surface",
                            element_id,
                            commit_counter,
                            rect,
                            effect,
                            source_elements,
                        )
                        .inspect_err(|error| {
                            warn!(
                                window_id = %window_id,
                                ?error,
                                "failed to build root-surface window behind effect"
                            );
                        })
                        .ok()
                    });
                let behind_effect_slot = window_decorations
                    .get(window)
                    .and_then(|decoration| decoration.window_effects.as_ref())
                    .and_then(|effects| effects.behind.as_ref())
                    .cloned();
                let behind_effects = behind_effect_slot.as_ref().and_then(|effect| {
                    let decoration = window_decorations.get(window)?;
                    let (rect, source_elements): (_, &[TtyRenderElements]) =
                        match &effect.effect.input {
                            EffectInput::WindowSource(WindowSourceInclude::Full) => (
                                transformed_root_rect(
                                    decoration.layout.root.rect,
                                    decoration.visual_transform,
                                ),
                                &full_window_source_elements,
                            ),
                            EffectInput::WindowSource(WindowSourceInclude::RootSurface) => (
                                transformed_rect(
                                    decoration.client_rect,
                                    decoration.layout.root.rect,
                                    decoration.visual_transform,
                                ),
                                &root_surface_source_elements,
                            ),
                            _ => (
                                transformed_root_rect(
                                    decoration.layout.root.rect,
                                    decoration.visual_transform,
                                ),
                                &current_window_elements,
                            ),
                        };
                    let signature =
                        window_effect_signature("behind", rect, effect, scale, source_elements);
                    let (element_id, commit_counter) =
                        window_decorations.get_mut(window).map(|decoration| {
                            window_effect_element_state(
                                decoration,
                                format!("behind@{}", output.name()),
                                signature,
                            )
                        })?;
                    window_effect_elements(
                    &mut backend.renderer,
                    &output,
                    output_geo,
                    scale,
                    &window_id,
                    "behind",
                    element_id,
                    commit_counter,
                    rect,
                    effect,
                    source_elements,
                )
                .inspect_err(|error| {
                    warn!(window_id = %window_id, ?error, "failed to build window behind effect");
                })
                .ok()
                });
                let in_front_effect_slot = window_decorations
                    .get(window)
                    .and_then(|decoration| decoration.window_effects.as_ref())
                    .and_then(|effects| effects.in_front.as_ref())
                    .cloned();
                let in_front_effects = in_front_effect_slot.as_ref().and_then(|effect| {
                    let decoration = window_decorations.get(window)?;
                    let (rect, source_elements): (_, &[TtyRenderElements]) =
                        match &effect.effect.input {
                            EffectInput::WindowSource(WindowSourceInclude::Full) => (
                                transformed_root_rect(
                                    decoration.layout.root.rect,
                                    decoration.visual_transform,
                                ),
                                &full_window_source_elements,
                            ),
                            EffectInput::WindowSource(WindowSourceInclude::RootSurface) => (
                                transformed_rect(
                                    decoration.client_rect,
                                    decoration.layout.root.rect,
                                    decoration.visual_transform,
                                ),
                                &root_surface_source_elements,
                            ),
                            _ => (
                                transformed_root_rect(
                                    decoration.layout.root.rect,
                                    decoration.visual_transform,
                                ),
                                &current_window_elements,
                            ),
                        };
                    let signature =
                        window_effect_signature("in-front", rect, effect, scale, source_elements);
                    let (element_id, commit_counter) =
                        window_decorations.get_mut(window).map(|decoration| {
                            window_effect_element_state(
                                decoration,
                                format!("in-front@{}", output.name()),
                                signature,
                            )
                        })?;
                    window_effect_elements(
                        &mut backend.renderer,
                        &output,
                        output_geo,
                        scale,
                        &window_id,
                        "in-front",
                        element_id,
                        commit_counter,
                        rect,
                        effect,
                        source_elements,
                    )
                    .inspect_err(|error| {
                        warn!(
                            window_id = %window_id,
                            ?error,
                            "failed to build in-front window effect"
                        );
                    })
                    .ok()
                });

                // Smithay renders render elements in reverse order, so this vector is
                // ordered front-to-back. Front effects go before the window elements;
                // behind effects go after them but still above lower windows.
                if let Some(in_front_effects) = in_front_effects {
                    if window_effect_debug_enabled() {
                        info!(
                            output = %output.name(),
                            window_id = %window_id,
                            in_front_effect_count = in_front_effects.len(),
                            "window effect debug: appending in-front effects"
                        );
                    }
                    scene_elements.extend(in_front_effects);
                }
                scene_elements.extend(current_window_elements.into_iter());
                if let Some(behind_effects) = behind_root_surface_effects {
                    if window_effect_debug_enabled() {
                        info!(
                            output = %output.name(),
                            window_id = %window_id,
                            behind_effect_count = behind_effects.len(),
                            "window effect debug: appending root-surface behind effects"
                        );
                    }
                    scene_elements.extend(behind_effects);
                }
                if let Some(behind_effects) = behind_effects {
                    if window_effect_debug_enabled() {
                        info!(
                            output = %output.name(),
                            window_id = %window_id,
                            behind_effect_count = behind_effects.len(),
                            "window effect debug: appending behind effects"
                        );
                    }
                    scene_elements.extend(behind_effects);
                }

                if windows_ready_for_decoration.insert(window_id.clone()) {
                    newly_ready_initial_focus_window_ids.push(window_id.clone());
                }

                if let Some(decoration) = window_decorations.get(window)
                    && let Some(live_snapshot) =
                        live_window_snapshots.get_mut(&decoration.snapshot.id)
                {
                    snapshot::retarget_snapshot_rect(live_snapshot, decoration.client_rect);
                }
                let should_seed_close_snapshot = window_decorations
                    .get(window)
                    .map(|decoration| {
                        live_window_snapshots
                            .get(&decoration.snapshot.id)
                            .map(|snapshot| {
                                snapshot.rect.width != decoration.client_rect.width
                                    || snapshot.rect.height != decoration.client_rect.height
                            })
                            .unwrap_or(true)
                    })
                    .unwrap_or(false);
                if should_seed_close_snapshot {
                    let snapshot_capture_started_at = Instant::now();
                    timescope::scope!("tty live snapshot capture");
                    if capture_live_snapshot_for_window(
                        &mut backend.renderer,
                        window,
                        window_location,
                        scale,
                        0,
                        window_decorations,
                        live_window_snapshots,
                        live_window_snapshot_trackers,
                    )
                    .is_ok()
                    {
                        snapshot_capture_count = snapshot_capture_count.saturating_add(1);
                    }
                    let elapsed_ms = snapshot_capture_started_at.elapsed().as_secs_f64() * 1000.0;
                    snapshot_capture_elapsed_ms += elapsed_ms;
                    window_timing.live_snapshot_refresh_ms += elapsed_ms;
                }

                // Content dirtiness is consumed by the transform-snapshot path above. The close
                // backup is seeded only when missing or resized, so client commits and workspace
                // movement do not maintain a second full client texture.
                if let Some(snapshot_id) = snapshot_id.as_ref() {
                    snapshot_dirty_window_ids.remove(snapshot_id);
                }
                let window_elapsed_ms = window_started_at.elapsed().as_secs_f64() * 1000.0;
                if animation_timing_debug_enabled() && window_elapsed_ms >= spike_threshold_ms {
                    warn!(
                        output = %output.name(),
                        window_id = %window_id,
                        use_full_window_snapshot,
                        direct_surface_count,
                        direct_surface_lookup_ms = window_timing.direct_surface_lookup_ms,
                        decoration_phase_ms = window_timing.decoration_phase_ms,
                        backdrop_ms = window_timing.backdrop_ms,
                        background_ms = window_timing.background_ms,
                        icon_ms = window_timing.icon_ms,
                        text_ms = window_timing.text_ms,
                        client_phase_ms = window_timing.client_phase_ms,
                        popup_phase_ms = window_timing.popup_phase_ms,
                        full_snapshot_scene_ms = window_timing.full_snapshot_scene_ms,
                        full_snapshot_capture_ms = window_timing.full_snapshot_capture_ms,
                        live_snapshot_refresh_ms = window_timing.live_snapshot_refresh_ms,
                        window_elapsed_ms,
                        window_has_snapshot_damage,
                        "animation timing: tty window spike"
                    );
                }
                window_loop_elapsed_ms += window_elapsed_ms;
                if window_elapsed_ms > max_window_elapsed_ms {
                    max_window_elapsed_ms = window_elapsed_ms;
                    max_window_id = Some(window_id);
                }
            }
        }
        timing.transform_snapshot_window_count = frame_transform_snapshot_window_count;
        timing.snapshot_capture_count = snapshot_capture_count;
        timing.window_loop_elapsed_ms = window_loop_elapsed_ms;
        timing.snapshot_capture_elapsed_ms = snapshot_capture_elapsed_ms;
        timing.max_window_elapsed_ms = max_window_elapsed_ms;
        timing.max_window_id = max_window_id;
        let lower_layers_started_at = Instant::now();
        // Fullscreen fast path: Bottom/Background layers are fully occluded.
        if fullscreen_window.is_none() {
            timescope::scope!("tty lower layer scene");
            scene_elements.extend(lower_layer_scene_elements(
                &mut backend.renderer,
                &output,
                output_geo,
                scale,
                state.configured_background_effect.as_ref(),
                &state.lower_layer_source_damage,
                state.lower_layer_scene_generation,
                &mut state.layer_backdrop_cache,
                &state.configured_layer_effects,
                &mut state.layer_effect_cache,
            )?);
        }
        let lower_layers_elapsed_ms = lower_layers_started_at.elapsed().as_secs_f64() * 1000.0;
        timing.lower_layers_elapsed_ms = lower_layers_elapsed_ms;

        // Keep layer roots in their requested layer, but draw popups in a
        // final front-most pass. GTK uses zwlr_layer_surface_v1.get_popup for
        // Waybar tooltips, which must remain visible above normal windows even
        // when the parent bar lives on Bottom.
        let layer_popup_elements = {
            timescope::scope!("tty layer popup scene");
            layer_popup_scene_elements(
                &mut backend.renderer,
                &output,
                output_geo,
                scale,
                fullscreen_window.is_some(),
                &state.configured_popup_effects,
                &state.configured_popup_surface_policies,
                &mut state.popup_effect_cache,
                &mut state.popup_framebuffer_effect_states,
                state.configured_background_effect.as_ref(),
            )
        };
        fullscreen_overlay_visible |=
            fullscreen_window.is_some() && !layer_popup_elements.is_empty();
        let mut front_to_back_scene = layer_popup_elements;
        front_to_back_scene.append(&mut scene_elements);
        scene_elements = front_to_back_scene;

        let should_profile_damage = should_capture_blink;
        let damage_profile_started_at = Instant::now();
        let computed_damage = if should_profile_damage {
            timescope::scope!("tty damage profile");
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
        let damage_profile_elapsed_ms = damage_profile_started_at.elapsed().as_secs_f64() * 1000.0;
        timing.damage_profile_elapsed_ms = damage_profile_elapsed_ms;

        let captured_blink_damage = if should_capture_blink {
            computed_damage
        } else {
            None
        };

        let mut content_elements: Vec<TtyRenderElements> = Vec::new();
        // Synthetic DamageOnly elements force the OutputDamageTracker to repaint
        // regions (decorations, manual invalidation). During the fullscreen fast
        // path they are actively harmful: the fullscreen surface occludes every
        // region they target, but as non-scanout elements stacked above the
        // client surface they knock it off the primary plane. Worse, scanning
        // out skips the swapchain render, which feeds back as fresh decoration
        // damage the next frame — so the damage element reappears every other
        // frame and direct scanout flaps engaged/disengaged at ~30 Hz. The
        // client surface carries its own damage for smithay's tracker, so
        // dropping these here is safe and keeps scanout steady.
        if fullscreen_window.is_none() {
            content_elements.extend(
                damage::elements_for_output(&extra_damage, output_geo)
                    .into_iter()
                    .map(TtyRenderElements::Damage),
            );
        }
        content_elements.extend(scene_elements);

        let cursor_status_for_log = cursor_override
            .map(CursorImageStatus::Named)
            .unwrap_or_else(|| cursor_status.clone());
        // Capture paths receive content_for_capture (everything but cursor)
        // and cursor_elements separately so they can honour the client's
        // cursor-visibility preference. The normal render to DRM still
        // composites them together below.
        let mut content_for_capture: Vec<TtyRenderElements> = Vec::new();
        content_for_capture.extend(
            damage_blink::elements_for_output(&blink_visible, output_geo, scale)
                .into_iter()
                .map(TtyRenderElements::Blink),
        );
        if let Some(lock_surface) = session_lock_surface_for_output.as_ref() {
            content_for_capture.extend(
                crate::backend::window::lock_surface_elements(
                    &mut backend.renderer,
                    lock_surface,
                    scale,
                    1.0,
                )
                .into_iter()
                .map(TtyRenderElements::Window),
            );
        }
        content_for_capture.extend(content_elements);
        // The element count for diagnostics — final elements is built below
        // after capture has run against the by-reference slices.
        timing.render_element_count = cursor_pointer_elements.len() + content_for_capture.len();

        trace!(
            ?node,
            ?crtc,
            output = %output.name(),
            window_count,
            render_element_count = timing.render_element_count,
            cursor_status = ?cursor_status_for_log,
            "rendering tty surface"
        );

        // Phase 5b-iii / 5f: drain toplevel captures first while we still
        // have the raw cursor PointerRenderElement Vec (those need window-
        // local translation that we can't apply after wrapping into
        // TtyRenderElements::Cursor).
        let presented = start_time.elapsed();
        {
            timescope::scope!("tty toplevel image capture");
            crate::backend::image_copy_capture_render::process_image_copy_capture_for_toplevels(
                image_copy_capture_pending,
                space,
                &mut backend.renderer,
                &cursor_pointer_elements,
                presented,
            );
        }

        // Now wrap cursor elements into the unified TtyRenderElements list
        // used by the output capture paths and the DRM render.
        let cursor_elements: Vec<TtyRenderElements> = cursor_pointer_elements
            .into_iter()
            .map(TtyRenderElements::Cursor)
            .collect();
        let cursor_visible = !cursor_elements.is_empty();

        let use_capture_mirror = screencopy_state.has_screencopy_for_output(&output)
            || crate::backend::image_copy_capture_render::has_pending_output_capture(
                image_copy_capture_pending,
                &output,
            );
        let mut mirrored_capture_content = None;
        let mut mirrored_display_content = None;
        let mut mirrored_render_states = None;
        if use_capture_mirror {
            timescope::scope!("tty capture mirror");
            let output_name = output.name();
            let mut mirror = state.output_capture_mirrors.remove(&output_name);
            match render_output_capture_mirror(
                &mut backend.renderer,
                &mut mirror,
                &output,
                &content_for_capture,
            ) {
                Ok(Some((capture_element, display_element, render_states))) => {
                    mirrored_capture_content = Some(vec![capture_element]);
                    mirrored_display_content = Some(vec![display_element]);
                    mirrored_render_states = Some(render_states);
                    if let Some(mirror) = mirror {
                        state.output_capture_mirrors.insert(output_name, mirror);
                    }
                }
                Ok(None) => {
                    if let Some(mirror) = mirror {
                        state.output_capture_mirrors.insert(output_name, mirror);
                    }
                }
                Err(err) => {
                    warn!(
                        output = %output.name(),
                        ?err,
                        "screencopy mirror: falling back to direct content render"
                    );
                }
            }
        } else {
            state.output_capture_mirrors.remove(output.name().as_str());
        }
        let capture_content_for_output = mirrored_capture_content
            .as_deref()
            .unwrap_or(content_for_capture.as_slice());

        {
            timescope::scope!("tty screencopy output");
            crate::backend::screencopy_render::process_screencopy_queue_for_output(
                screencopy_state,
                loop_handle,
                &mut backend.renderer,
                &output,
                capture_content_for_output,
                &cursor_elements,
            );
        }
        // Phase 5b-ii: serve ext-image-copy-capture-v1 frames for this output
        // alongside the existing wlr-screencopy queue.
        {
            timescope::scope!("tty output image capture");
            crate::backend::image_copy_capture_render::process_image_copy_capture_for_output(
                image_copy_capture_pending,
                &mut backend.renderer,
                &output,
                capture_content_for_output,
                &cursor_elements,
                presented,
            );
        }

        // FPS overlay: pre-rasterized glyph buffers composed at the top-left
        // of this output. Built before render_frame so it sits in the
        // top-most position (smithay treats index 0 as front-most).
        let error_text_elements: Vec<TtyRenderElements> =
            crate::config_error::text_elements_for_output(
                &mut backend.renderer,
                text_rasterizer,
                config_error_report.as_ref(),
                output_geo,
                scale,
            )
            .unwrap_or_default()
            .into_iter()
            .map(TtyRenderElements::Text)
            .collect();
        let error_background_elements: Vec<TtyRenderElements> =
            crate::config_error::background_elements_for_output(
                config_error_report.as_ref(),
                output_geo,
                scale,
            )
            .into_iter()
            .map(TtyRenderElements::Blink)
            .collect();
        let fps_overlay_elements: Vec<TtyRenderElements> = fps_counter
            .render_elements(
                &mut backend.renderer,
                output.name().as_str(),
                output_geo,
                scale,
            )
            .into_iter()
            .map(TtyRenderElements::Text)
            .collect();

        // After capture has run against the cursor / content slices by
        // reference, move them into a single element list for the DRM render.
        let mut elements: Vec<TtyRenderElements> = Vec::with_capacity(
            error_text_elements.len()
                + error_background_elements.len()
                + fps_overlay_elements.len()
                + cursor_elements.len()
                + content_for_capture.len(),
        );
        elements.extend(error_text_elements);
        elements.extend(error_background_elements);
        elements.extend(fps_overlay_elements);
        elements.extend(cursor_elements);
        if let Some(mirrored_display_content) = mirrored_display_content {
            elements.extend(mirrored_display_content);
        } else {
            elements.extend(content_for_capture);
        }

        let fullscreen_scanout_candidate = if fullscreen_overlay_visible {
            None
        } else {
            fullscreen_window.as_ref()
        };

        // Direct scanout only kicks in if the CRTC background (clear color) is
        // black/transparent OR the topmost element reports itself opaque and
        // spans the output (smithay DrmCompositor::render_frame). Many clients
        // (Chrome, Minecraft) never set a wl_surface opaque region, so the
        // opaque check fails and the non-black desktop clear color blocks the
        // primary-plane promotion. During the fullscreen fast path the window
        // covers the whole output, so the clear color is never visible — swap
        // in black there to satisfy the easy scanout path.
        let frame_clear_color: [f32; 4] = if fullscreen_window.is_some() {
            [0.0, 0.0, 0.0, 1.0]
        } else {
            CLEAR_COLOR
        };
        // An async page flip may only touch the primary plane; changing the cursor plane in the
        // same commit is rejected by the kernel. Match Hyprland's policy and block tearing while
        // a cursor is visible, allowing the hardware cursor to update independently at the output
        // refresh rate. Fullscreen games normally hide/lock the pointer, so their frames still use
        // direct scanout with async flips.
        let tearing_forced = tearing_force_enabled();
        let fullscreen_window_id = fullscreen_scanout_candidate
            .and_then(|window| window_decorations.get(window))
            .map(|decoration| decoration.snapshot.id.clone());
        let fullscreen_root_element_id = fullscreen_scanout_candidate.and_then(|window| {
            window
                .toplevel()
                .map(|toplevel| Id::from_wayland_resource(toplevel.wl_surface()))
                .or_else(|| {
                    window
                        .x11_surface()
                        .and_then(|surface| surface.wl_surface())
                        .map(|surface| Id::from_wayland_resource(&surface))
                })
        });
        // Tearing only ever happens for the fullscreen direct-scanout window with the pointer
        // hidden (an async flip may not touch the cursor plane). Within that gate the per-window
        // `allowTearing` config prop is the source of truth (Model B): when the config sets it we
        // honor that value directly; when it leaves it unset we fall back to the client's
        // `wp_tearing_control` hint. Driving the decision from config (rather than requiring the
        // client hint) lets X11/Xwayland games — which never send `wp_tearing_control` — tear too.
        // `SHOJI_FORCE_TEARING` still forces it on for testing.
        let should_tear = surface.supports_async_flip
            && !cursor_visible
            && fullscreen_scanout_candidate.is_some_and(|window| {
                if tearing_forced {
                    return true;
                }
                window_decorations
                    .get(window)
                    .and_then(|decoration| decoration.managed_window.allow_tearing)
                    .unwrap_or_else(|| {
                        window
                            .toplevel()
                            .map(|toplevel| toplevel.wl_surface())
                            .is_some_and(crate::protocols::tearing_control::surface_prefers_tearing)
                    })
            });
        surface.tearing_active = should_tear;
        let mut frame_flags = TTY_FRAME_FLAGS;
        if overlay_planes_disabled() {
            frame_flags = frame_flags.difference(FrameFlags::ALLOW_OVERLAY_PLANE_SCANOUT);
        }
        if fullscreen_overlay_visible {
            frame_flags = frame_flags.difference(FrameFlags::ALLOW_PRIMARY_PLANE_SCANOUT_ANY);
        }
        if should_tear {
            frame_flags = frame_flags.difference(FrameFlags::ALLOW_CURSOR_PLANE_SCANOUT);
        }
        // Keep every real damage frame asynchronous for the whole tearing period. In
        // particular, a visible software-cursor update must not fall back to a synced flip:
        // alternating async game frames with vblank-bound cursor frames produces visibly uneven
        // cursor motion even when both the game and the output are otherwise running fast.
        let mut fullscreen_root_buffer_details = None;
        let render_frame_result = {
            timescope::scope!("tty render_frame");
            crate::backend::shader_effect::with_gpu_timing_renderer_span(
                &mut backend.renderer,
                "tty-render-frame",
                (output_geo.size.w, output_geo.size.h),
                |renderer| {
                    if direct_scanout_debug_enabled()
                        && let Some(root_id) = fullscreen_root_element_id.as_ref()
                        && let Some(element) =
                            elements.iter().find(|element| element.id() == root_id)
                    {
                        let src = element.src();
                        let geometry = element.geometry(scale);
                        let transform = element.transform();
                        let storage =
                            describe_underlying_storage(element.underlying_storage(renderer));
                        fullscreen_root_buffer_details = Some(format!(
                            "element={} id={:?} src={src:?} geometry={geometry:?} \
                         transform={transform:?} storage=[{storage}]",
                            tty_render_element_name(element),
                            element.id(),
                        ));
                    }
                    if crate::backend::shader_effect::gpu_element_timing_debug_enabled() {
                        let profiled_elements = elements
                            .iter()
                            .map(ProfiledTtyRenderElement::new)
                            .collect::<Vec<_>>();
                        surface
                            .drm_output
                            .render_frame(
                                renderer,
                                &profiled_elements,
                                frame_clear_color,
                                frame_flags,
                            )
                            .map(|result| {
                                // NVIDIA: IN_FENCE_FD is broken on nvidia-drm (see
                                // https://github.com/NVIDIA/open-gpu-kernel-modules/issues/622),
                                // so smithay cannot attach a fence to the atomic commit. When
                                // needs_sync() is true the page flip would be submitted before
                                // the GPU finishes rendering, causing the display to scan out a
                                // partially-rendered buffer and producing visible microstutter
                                // during scroll/animation. Block on the swapchain sync point
                                // manually before queue_frame commits the flip.
                                if result.needs_sync() {
                                    if let PrimaryPlaneElement::Swapchain(ref element) =
                                        result.primary_element
                                    {
                                        let _ = element.sync.wait();
                                    }
                                }
                                let primary_scanout = matches!(
                                    result.primary_element,
                                    PrimaryPlaneElement::Element(_)
                                );
                                let primary_plane_kind = match &result.primary_element {
                                    PrimaryPlaneElement::Element(_) => "element",
                                    PrimaryPlaneElement::Swapchain(_) => "swapchain",
                                };
                                let overlay_details = if direct_scanout_debug_enabled() {
                                    result
                                        .overlay_elements
                                        .iter()
                                        .map(|element| {
                                            format!(
                                                "id={:?} geo={:?} alpha={:.3}",
                                                element.id(),
                                                element.geometry(scale),
                                                element.alpha(),
                                            )
                                        })
                                        .collect()
                                } else {
                                    Vec::new()
                                };
                                TtyRenderFrameResult {
                                    is_empty: result.is_empty,
                                    primary_scanout,
                                    primary_plane_kind,
                                    overlay_plane_count: result.overlay_elements.len(),
                                    overlay_details,
                                    cursor_plane_assigned: result.cursor_element.is_some(),
                                    states: result.states,
                                }
                            })
                    } else {
                        surface
                            .drm_output
                            .render_frame(renderer, &elements, frame_clear_color, frame_flags)
                            .map(|result| {
                                // NVIDIA: IN_FENCE_FD is broken on nvidia-drm (see
                                // https://github.com/NVIDIA/open-gpu-kernel-modules/issues/622),
                                // so smithay cannot attach a fence to the atomic commit. When
                                // needs_sync() is true the page flip would be submitted before
                                // the GPU finishes rendering, causing the display to scan out a
                                // partially-rendered buffer and producing visible microstutter
                                // during scroll/animation. Block on the swapchain sync point
                                // manually before queue_frame commits the flip.
                                if result.needs_sync() {
                                    if let PrimaryPlaneElement::Swapchain(ref element) =
                                        result.primary_element
                                    {
                                        let _ = element.sync.wait();
                                    }
                                }
                                let primary_scanout = matches!(
                                    result.primary_element,
                                    PrimaryPlaneElement::Element(_)
                                );
                                let primary_plane_kind = match &result.primary_element {
                                    PrimaryPlaneElement::Element(_) => "element",
                                    PrimaryPlaneElement::Swapchain(_) => "swapchain",
                                };
                                let overlay_details = if direct_scanout_debug_enabled() {
                                    result
                                        .overlay_elements
                                        .iter()
                                        .map(|element| {
                                            format!(
                                                "id={:?} geo={:?} alpha={:.3}",
                                                element.id(),
                                                element.geometry(scale),
                                                element.alpha(),
                                            )
                                        })
                                        .collect()
                                } else {
                                    Vec::new()
                                };
                                TtyRenderFrameResult {
                                    is_empty: result.is_empty,
                                    primary_scanout,
                                    primary_plane_kind,
                                    overlay_plane_count: result.overlay_elements.len(),
                                    overlay_details,
                                    cursor_plane_assigned: result.cursor_element.is_some(),
                                    states: result.states,
                                }
                            })
                    }
                },
            )
        };
        let result = match render_frame_result {
            Ok(
                result,
            ) => result,
            Err(
                err,
            ) => {
                if error_chain_has_drm_test_failed(
                    &err,
                ) {
                    warn!(
                        output = %output.name(),
                        ?err,
                        "tty render_frame failed its atomic test; requesting surface reset",
                    );
                    return Ok(
                        RenderSurfaceOutcome::CommitFailed,
                    );
                }
                return Err(
                    err
                        .into(),
                );
            }
        };
        fps_counter.record_present(output.name().as_str());
        if direct_scanout_debug_enabled() {
            note_overlay_plane_transition(
                output.name().as_str(),
                result.overlay_plane_count,
                &result.overlay_details,
            );
        }
        note_direct_scanout_transition(
            output.name().as_str(),
            result.primary_scanout,
            fullscreen_scanout_candidate.is_some(),
            fullscreen_root_buffer_details.as_deref(),
        );
        if direct_scanout_debug_enabled()
            && fullscreen_scanout_candidate.is_some()
            && direct_scanout_debug_log_allowed(output.name().as_str())
        {
            let mut zero_copy_count = 0usize;
            let mut rendered_count = 0usize;
            let mut skipped_count = 0usize;
            let mut format_unsupported_count = 0usize;
            let mut scanout_failed_count = 0usize;
            for state in result.states.states.values() {
                match state.presentation_state {
                    RenderElementPresentationState::ZeroCopy => zero_copy_count += 1,
                    RenderElementPresentationState::Skipped => skipped_count += 1,
                    RenderElementPresentationState::Rendering { reason } => {
                        rendered_count += 1;
                        match reason {
                            Some(RenderingReason::FormatUnsupported) => {
                                format_unsupported_count += 1
                            }
                            Some(RenderingReason::ScanoutFailed) => scanout_failed_count += 1,
                            None => {}
                        }
                    }
                }
            }
            let fullscreen_root_state = fullscreen_root_element_id
                .as_ref()
                .and_then(|id| result.states.states.get(id))
                .map(|state| format!("{:?}", state));
            let element_details = elements
                .iter()
                .enumerate()
                .map(|(index, element)| {
                    let element_state = result.states.states.get(element.id());
                    format!(
                        "#{index}:{} id={:?} kind={:?} src={:?} geo={:?} transform={:?} \
                         alpha={:.3} opaque={:?} state={:?}",
                        tty_render_element_name(element),
                        element.id(),
                        element.kind(),
                        element.src(),
                        element.geometry(scale),
                        element.transform(),
                        element.alpha(),
                        element.opaque_regions(scale),
                        element_state,
                    )
                })
                .collect::<Vec<_>>();
            info!(
                output = %output.name(),
                fullscreen_window_id,
                should_tear,
                result_is_empty = result.is_empty,
                primary_scanout = result.primary_scanout,
                primary_plane_kind = result.primary_plane_kind,
                overlay_plane_count = result.overlay_plane_count,
                cursor_plane_assigned = result.cursor_plane_assigned,
                element_count = elements.len(),
                zero_copy_count,
                rendered_count,
                skipped_count,
                format_unsupported_count,
                scanout_failed_count,
                fullscreen_root_element_id = ?fullscreen_root_element_id,
                fullscreen_root_state,
                fullscreen_root_buffer_details,
                element_details = ?element_details,
                "direct scanout debug: fullscreen frame result"
            );
        }
        if std::env::var_os("SHOJI_TRANSFORM_SNAPSHOT_DEBUG").is_some()
            && (frame_transform_snapshot_window_count > 0 || frame_had_transform_snapshot_damage)
        {
            tracing::info!(
                output = %output.name(),
                frame_transform_snapshot_window_count,
                frame_snapshot_damage_window_count,
                frame_had_transform_snapshot_damage,
                extra_damage_count = extra_damage.len(),
                result_is_empty = result.is_empty,
                "transform snapshot tty render result"
            );
        }
        let render_elapsed = render_started_at.elapsed();
        let total_cpu_elapsed = frame_started_at.elapsed();
        timing.result_is_empty = result.is_empty;
        timing.render_elapsed_ms = render_elapsed.as_secs_f64() * 1000.0;
        timing.total_cpu_elapsed_ms = total_cpu_elapsed.as_secs_f64() * 1000.0;
        if has_visible_x11_chrome && browser_cpu_debug_allowed(output.name().as_str()) {
            info!(
                output = %output.name(),
                result_is_empty = result.is_empty,
                window_count,
                render_element_count = timing.render_element_count,
                decoration_refresh_elapsed_ms,
                layer_effects_elapsed_ms,
                upper_layers_elapsed_ms = timing.upper_layers_elapsed_ms,
                closing_snapshots_elapsed_ms = timing.closing_snapshots_elapsed_ms,
                window_loop_elapsed_ms = timing.window_loop_elapsed_ms,
                lower_layers_elapsed_ms = timing.lower_layers_elapsed_ms,
                damage_profile_elapsed_ms = timing.damage_profile_elapsed_ms,
                render_elapsed_ms = timing.render_elapsed_ms,
                total_cpu_elapsed_ms = timing.total_cpu_elapsed_ms,
                snapshot_capture_elapsed_ms = timing.snapshot_capture_elapsed_ms,
                frame_transform_snapshot_window_count,
                frame_snapshot_damage_window_count,
                frame_had_transform_snapshot_damage,
                pending_decoration_damage_count = state.pending_decoration_damage.len(),
                window_source_damage_count = state.window_source_damage.len(),
                transform_snapshot_window_ids_count = state.transform_snapshot_window_ids.len(),
                "browser cpu: tty frame summary"
            );
        }
        surface.estimated_render_duration =
            blend_render_duration(surface.estimated_render_duration, render_elapsed);
        let effective_render_states_storage = mirrored_render_states.map(|mut states| {
            states.states.extend(
                result
                    .states
                    .states
                    .iter()
                    .map(|(id, state)| (id.clone(), *state)),
            );
            states
        });
        let effective_render_states = effective_render_states_storage
            .as_ref()
            .unwrap_or(&result.states);
        // Update primary-scanout metadata unconditionally — even for no-damage frames.
        //
        // Firefox's root wl_surface commits without a buffer (pure frame-callback registration).
        // Surfaces without a buffer produce no render elements and are therefore absent from
        // result.states. Calling update_primary_scanout_output here clears those surfaces'
        // primary_scanout_output. Combined with throttle = Some(1s) in send_frame_callbacks,
        // this limits Firefox frame callbacks to ~1/second when idle, dropping its CPU from
        // ~8% to ~0.1%. (Anvil does the same: its update_primary_scanout_output call is
        // unconditional, outside the `if rendered` branch.)
        update_primary_scanout_output(
            &state.space,
            &output,
            &cursor_status_for_log,
            session_lock_surface_for_output.as_ref(),
            effective_render_states,
            window_decorations,
        );
        for window in state.space.elements_for_output(&output) {
            window.send_dmabuf_feedback(
                &output,
                |_, _| Some(output.clone()),
                |wl_surface, _| {
                    select_dmabuf_feedback(
                        wl_surface,
                        effective_render_states,
                        &surface.dmabuf_feedback.render,
                        &surface.dmabuf_feedback.scanout,
                    )
                },
            );
        }
        for layer in layer_map_for_output(&output)
            .layers()
            .filter(|layer| crate::backend::window::layer_surface_is_mapped(layer))
        {
            layer.send_dmabuf_feedback(
                &output,
                |_, _| Some(output.clone()),
                |wl_surface, _| {
                    select_dmabuf_feedback(
                        wl_surface,
                        effective_render_states,
                        &surface.dmabuf_feedback.render,
                        &surface.dmabuf_feedback.scanout,
                    )
                },
            );
        }
        if let Some(lock_surface) = session_lock_surface_for_output.as_ref() {
            send_dmabuf_feedback_surface_tree(
                lock_surface.wl_surface(),
                &output,
                |_, _| Some(output.clone()),
                |wl_surface, _| {
                    select_dmabuf_feedback(
                        wl_surface,
                        effective_render_states,
                        &surface.dmabuf_feedback.render,
                        &surface.dmabuf_feedback.scanout,
                    )
                },
            );
        }
        if !result.is_empty {
            restore_presented_window_surface_primary_outputs(
                &state.space,
                &output,
                effective_render_states,
            );
            if frame_liveness_debug_enabled() {
                tracing::info!(
                    output = %output.name(),
                    result_is_empty = false,
                    frame_callback_sequence = surface.frame_callback_sequence,
                    "tty frame liveness: damage frame rendered",
                );
            }
            if std::env::var_os("SHOJI_FRAME_THROTTLE_DEBUG").is_some() {
                tracing::info!(
                    output = %output.name(),
                    states_count = effective_render_states.states.len(),
                    states_ids = ?effective_render_states.states.keys().map(|id| format!("{:?}", id)).collect::<Vec<_>>(),
                    "tty DAMAGE frame",
                );
            }
            trace!(output = %output.name(), "queueing tty frame");
            // Windows rendered via full-window snapshot have their wl_surfaces composited
            // into an offscreen texture rather than the DRM framebuffer. Those surfaces are
            // therefore absent from result.states, so update_primary_scanout_output above
            // clears their SurfacePrimaryScanoutOutput to None. That in turn makes
            // send_frame_callbacks_for_output skip them entirely, causing the client (e.g.
            // Chrome at visual scale=0.9) to fall back to a ~0.6 fps background rate.
            //
            // Fix: for every window currently rendered via snapshot, synthesise a
            // RenderElementStates that marks all its wl_surfaces as "presented on this
            // output" and call update_surface_primary_scanout_output again so that their
            // primary-scanout assignment is restored before frame callbacks are sent.
            if !state.transform_snapshot_window_ids.is_empty() {
                use smithay::backend::renderer::element::{
                    Id, RenderElementPresentationState, RenderElementState, RenderElementStates,
                };
                use smithay::desktop::utils::update_surface_primary_scanout_output;

                if std::env::var_os("SHOJI_FRAME_THROTTLE_DEBUG").is_some() {
                    tracing::info!(
                        output = %output.name(),
                        snapshot_ids_count = state.transform_snapshot_window_ids.len(),
                        "snapshot fix: transform_snapshot_window_ids non-empty",
                    );
                }

                let snapshot_windows: Vec<_> = state
                    .space
                    .elements_for_output(&output)
                    .filter(|w| {
                        state
                            .window_decorations
                            .get(*w)
                            .map(|d| {
                                state.transform_snapshot_window_ids.contains(&d.snapshot.id)
                                    && snapshot_transform_changed_ids.contains(&d.snapshot.id)
                            })
                            .unwrap_or(false)
                    })
                    .cloned()
                    .collect();

                if std::env::var_os("SHOJI_FRAME_THROTTLE_DEBUG").is_some() {
                    for window in &snapshot_windows {
                        let app_id = window
                            .toplevel()
                            .and_then(|t| {
                                smithay::wayland::compositor::with_states(
                                    t.wl_surface(),
                                    |states| {
                                        states
                                            .data_map
                                            .get::<smithay::wayland::shell::xdg::XdgToplevelSurfaceData>()
                                            .map(|d| d.lock().ok()?.app_id.clone())
                                    },
                                )
                            })
                            .flatten();
                        tracing::info!(
                            output = %output.name(),
                            app_id = ?app_id,
                            "snapshot fix: window in snapshot_windows — synthetic primary will be SET",
                        );
                    }
                }

                for window in snapshot_windows {
                    // Restore primary scanout output only for snapshot windows whose transform is
                    // still actively changing this frame. Stationary snapshot windows remain
                    // throttled so visible-idle clients such as X11 Chrome do not get refresh-rate
                    // frame callbacks solely because the compositor is compositing an offscreen
                    // texture for them.
                    let mut synthetic_states = RenderElementStates::default();
                    window.with_surfaces(|surface, _| {
                        synthetic_states.states.insert(
                            Id::from_wayland_resource(surface),
                            // Use usize::MAX so that area_primary_scanout_compare (used below)
                            // always assigns this output as primary, regardless of the stored
                            // area from the previous output. The goal here is to force the
                            // primary back to the current output for snapshot-rendered windows.
                            RenderElementState {
                                visible_area: usize::MAX,
                                presentation_state: RenderElementPresentationState::Rendering {
                                    reason: None,
                                },
                                needs_capture: false,
                            },
                        );
                    });
                    window.with_surfaces(|surface, states| {
                        update_surface_primary_scanout_output(
                            surface,
                            &output,
                            states,
                            None,
                            &synthetic_states,
                            crate::presentation::area_primary_scanout_compare,
                        );
                    });
                }
            }
            let replace_effect_windows: Vec<_> = state
                .space
                .elements_for_output(&output)
                .filter(|window| {
                    state
                        .window_decorations
                        .get(*window)
                        .and_then(|decoration| decoration.window_effects.as_ref())
                        .and_then(|effects| effects.replace.as_ref())
                        .is_some()
                })
                .cloned()
                .collect();
            if !replace_effect_windows.is_empty() {
                use smithay::backend::renderer::element::{
                    Id, RenderElementPresentationState, RenderElementState, RenderElementStates,
                };
                use smithay::desktop::utils::update_surface_primary_scanout_output;

                for window in replace_effect_windows {
                    // Replacement effects render the client into an offscreen texture and then
                    // present that texture instead of the original surface element. Without this
                    // synthetic state, primary-scanout bookkeeping is cleared and visible clients
                    // only receive throttled frame callbacks, making text input appear late.
                    let mut synthetic_states = RenderElementStates::default();
                    window.with_surfaces(|surface, _| {
                        synthetic_states.states.insert(
                            Id::from_wayland_resource(surface),
                            RenderElementState {
                                visible_area: usize::MAX,
                                presentation_state: RenderElementPresentationState::Rendering {
                                    reason: None,
                                },
                                needs_capture: false,
                            },
                        );
                    });
                    window.with_surfaces(|surface, states| {
                        update_surface_primary_scanout_output(
                            surface,
                            &output,
                            states,
                            None,
                            &synthetic_states,
                            crate::presentation::area_primary_scanout_compare,
                        );
                    });
                }
            }
            let output_presentation_feedback = take_presentation_feedback(
                &output,
                &state.space,
                session_lock_surface_for_output.as_ref(),
                effective_render_states,
            );
            // Edge-triggered "tearing engaged/disengaged" log (kept as a normal operational
            // log; see `note_tearing_transition`).
            note_tearing_transition(output.name().as_str(), should_tear, tearing_forced);
            let queue_started_at = Instant::now();
            // `should_tear` selects an immediate (async) page flip when the fullscreen
            // direct-scanout tearing fast path is active, and a normal vblank-synced flip
            // otherwise. See `should_tear` / the tearing fast-path block above.
            {
                timescope::scope!("tty queue_frame");
                if let Err(err) = surface
                    .drm_output
                    .queue_frame_tearing(Some(output_presentation_feedback), should_tear)
                {
                    if error_chain_has_permission_denied(&err) {
                        warn!(
                            output = %output.name(),
                            ?err,
                            "tty queue_frame lost drm access; waiting for session resume"
                        );
                        // A permission-denied commit means DRM master is already gone,
                        // but logind's `PauseSession` has not been dispatched yet —
                        // measured ~20ms wide on resume from suspend. Until the flag
                        // flips, `run_tty_udev` still treats `UdevEvent::Changed` as a
                        // live hotplug, so a connector event landing inside that window
                        // half-applies exactly as its comment warns: the scanner records
                        // the new topology while every CRTC commit fails with EACCES, and
                        // the post-resume replay then skips the output because it is
                        // already recorded as connected. Nothing re-arms it and the panel
                        // stays dark for the rest of the session. Drop the flag where the
                        // loss is first observed so the existing deferral covers the gap;
                        // `ActivateSession` restores it.
                        *tty_session_active = false;
                        reset_surface_after_tty_pause(surface);
                        return Ok(RenderSurfaceOutcome::Skipped);
                    }
                    if error_chain_has_drm_test_failed(&err) {
                        warn!(
                            output = %output.name(),
                            ?err,
                            "tty queue_frame failed its atomic test; requesting surface reset",
                        );
                        return Ok(
                            RenderSurfaceOutcome::CommitFailed,
                        );
                    }
                    return Err(Box::new(err));
                }
            }
            if animation_gap_debug_enabled() {
                info!(
                    output = %output.name(),
                    redraw_state_before = ?surface.redraw_state,
                    frame_pending_before = surface.frame_pending,
                    frame_callback_timer_armed = surface.frame_callback_timer_armed,
                    next_frame_target = ?surface.next_frame_target,
                    estimated_render_duration_ms =
                        surface.estimated_render_duration.as_secs_f64() * 1000.0,
                    result_is_empty = result.is_empty,
                    "animation gap: tty queue_frame submitted"
                );
            }
            surface.frame_pending = true;
            surface.queued_at = Some(queue_started_at);
            surface.queued_cpu_duration = total_cpu_elapsed;
            surface.skipped_while_pending_count = 0;
            surface.frame_callback_timer_armed = false;
            surface.frame_callback_timer_generation =
                surface.frame_callback_timer_generation.wrapping_add(1);
            surface.frame_callback_sequence = surface.frame_callback_sequence.wrapping_add(1);
            surface.redraw_state = TtyRedrawState::WaitingForVBlank {
                redraw_needed: false,
            };
            let frame_callback_sequence = surface.frame_callback_sequence;
            let callback_time = Duration::from(state.clock.now());
            if has_visible_mpv {
                info!(
                    output = %output.name(),
                    callback_time_ms = callback_time.as_secs_f64() * 1000.0,
                    frame_time_ms = frame_time.as_secs_f64() * 1000.0,
                    result_states_count = effective_render_states.states.len(),
                    total_cpu_elapsed_ms = total_cpu_elapsed.as_secs_f64() * 1000.0,
                    render_elapsed_ms = timing.render_elapsed_ms,
                    frame_callback_sequence,
                    "mpv frame debug: queue_frame"
                );
            }
            let _ = surface;
            let _ = backend;
            state.post_repaint_with_sequence(
                &output,
                callback_time,
                effective_render_states,
                Some(frame_callback_sequence),
            );
            let _ = state.display_handle.flush_clients();
        } else {
            if frame_liveness_debug_enabled() {
                tracing::info!(
                    output = %output.name(),
                    result_is_empty = true,
                    frame_callback_sequence = surface.frame_callback_sequence,
                    "tty frame liveness: no-damage frame rendered",
                );
            }
            if std::env::var_os("SHOJI_FRAME_THROTTLE_DEBUG").is_some() {
                tracing::info!(
                    output = %output.name(),
                    states_count = result.states.states.len(),
                    "tty no-damage frame: update_primary_scanout_output already called unconditionally",
                );
            }
            trace!(output = %output.name(), "tty frame had no damage");
            // A completely idle no-damage frame still advances the logical refresh cycle.
            // If we drop straight to Idle here, visible clients such as Kitty can end up waiting
            // forever for the next frame callback after an animation finishes, and their next
            // input-driven repaint only appears once some unrelated event (for example pointer
            // motion) forces another redraw.
            //
            // We therefore keep a lightweight estimated-vblank timer, but unlike the earlier
            // version we only send callbacks to surfaces whose primary scanout output is this
            // output. That preserves the "next callback after no-damage" behaviour that visible
            // clients need without reintroducing the Firefox `primary=None` callback burst.
            let generation = surface.frame_callback_timer_generation.wrapping_add(1);
            surface.frame_callback_timer_generation = generation;
            surface.redraw_state = TtyRedrawState::WaitingForEstimatedVBlank {
                queued: false,
                generation,
            };
            if animation_gap_debug_enabled() {
                info!(
                    output = %output.name(),
                    generation,
                    frame_duration_ms = surface.frame_duration.as_secs_f64() * 1000.0,
                    frame_callback_timer_armed = surface.frame_callback_timer_armed,
                    next_frame_target = ?surface.next_frame_target,
                    "animation gap: tty estimated-vblank armed"
                );
            }
            schedule_estimated_vblank_callback(loop_handle, state, node, crtc, frame_time);
        }

        captured_blink_damage
    };

    for window_id in newly_ready_initial_focus_window_ids {
        if !state.pending_initial_focus_window_ids.contains(&window_id) {
            continue;
        }
        let window = state
            .space
            .elements()
            .find(|window| state.snapshot_window(window).id == window_id)
            .cloned();
        if let Some(window) = window {
            state.apply_pending_initial_focus_for_window(&window_id, &window);
        }
    }

    if let Some(damage) = captured_blink_damage.as_deref() {
        state.record_damage_blink(&output, damage);
    }

    if animation_timing_debug_enabled()
        && (timing.transform_snapshot_window_count > 0
            || timing.closing_snapshot_count > 0
            || decoration_refresh_elapsed_ms >= spike_threshold_ms
            || layer_effects_elapsed_ms >= spike_threshold_ms
            || timing.render_elapsed_ms >= spike_threshold_ms
            || timing.total_cpu_elapsed_ms >= spike_threshold_ms)
    {
        if timing.total_cpu_elapsed_ms >= spike_threshold_ms
            || timing.render_elapsed_ms >= spike_threshold_ms
            || decoration_refresh_elapsed_ms >= spike_threshold_ms
            || layer_effects_elapsed_ms >= spike_threshold_ms
        {
            warn!(
                output = %output.name(),
                decoration_refresh_elapsed_ms,
                layer_effects_elapsed_ms,
                cursor_elapsed_ms = timing.cursor_elapsed_ms,
                upper_layers_elapsed_ms = timing.upper_layers_elapsed_ms,
                closing_snapshots_elapsed_ms = timing.closing_snapshots_elapsed_ms,
                window_loop_elapsed_ms = timing.window_loop_elapsed_ms,
                snapshot_capture_elapsed_ms = timing.snapshot_capture_elapsed_ms,
                lower_layers_elapsed_ms = timing.lower_layers_elapsed_ms,
                damage_profile_elapsed_ms = timing.damage_profile_elapsed_ms,
                render_elapsed_ms = timing.render_elapsed_ms,
                total_cpu_elapsed_ms = timing.total_cpu_elapsed_ms,
                render_element_count = timing.render_element_count,
                transform_snapshot_window_count = timing.transform_snapshot_window_count,
                closing_snapshot_count = timing.closing_snapshot_count,
                snapshot_capture_count = timing.snapshot_capture_count,
                result_is_empty = timing.result_is_empty,
                max_window_elapsed_ms = timing.max_window_elapsed_ms,
                max_window_id = timing.max_window_id.as_deref(),
                spike_threshold_ms,
                "animation timing: tty frame spike"
            );
        } else {
            info!(
                output = %output.name(),
                decoration_refresh_elapsed_ms,
                layer_effects_elapsed_ms,
                cursor_elapsed_ms = timing.cursor_elapsed_ms,
                upper_layers_elapsed_ms = timing.upper_layers_elapsed_ms,
                closing_snapshots_elapsed_ms = timing.closing_snapshots_elapsed_ms,
                window_loop_elapsed_ms = timing.window_loop_elapsed_ms,
                snapshot_capture_elapsed_ms = timing.snapshot_capture_elapsed_ms,
                lower_layers_elapsed_ms = timing.lower_layers_elapsed_ms,
                damage_profile_elapsed_ms = timing.damage_profile_elapsed_ms,
                render_elapsed_ms = timing.render_elapsed_ms,
                total_cpu_elapsed_ms = timing.total_cpu_elapsed_ms,
                render_element_count = timing.render_element_count,
                transform_snapshot_window_count = timing.transform_snapshot_window_count,
                closing_snapshot_count = timing.closing_snapshot_count,
                snapshot_capture_count = timing.snapshot_capture_count,
                result_is_empty = timing.result_is_empty,
                max_window_elapsed_ms = timing.max_window_elapsed_ms,
                max_window_id = timing.max_window_id.as_deref(),
                spike_threshold_ms,
                "animation timing: tty frame"
            );
        }
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
) -> Vec<TtyRenderElements> {
    if is_identity_visual_geometry(visual) {
        if clipped_transform_debug_enabled() {
            for element in &elements {
                info!(
                    debug_label = element.debug_label(),
                    visual_origin = ?visual.origin,
                    visual_scale = ?visual.scale,
                    visual_translation = ?visual.translation,
                    pre_transform_geometry = ?element.geometry(Scale::from((1.0, 1.0))),
                    post_transform_geometry = ?element.geometry(Scale::from((1.0, 1.0))),
                    "gap debug tty transformed clipped geometry"
                );
            }
        }
        return elements
            .into_iter()
            .map(TtyRenderElements::Clipped)
            .collect();
    }

    elements
        .into_iter()
        .map(|element| {
            let debug_label = element.debug_label().map(|label| label.to_owned());
            let pre_transform_geometry = element.geometry(Scale::from((1.0, 1.0)));
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
                    post_transform_geometry = ?transformed.geometry(Scale::from((1.0, 1.0))),
                    "gap debug tty transformed clipped geometry"
                );
            }
            TtyRenderElements::TransformedClipped(transformed)
        })
        .collect()
}

fn root_surface_source_elements_for_window(
    window: &smithay::desktop::Window,
    renderer: &mut GlesRenderer,
    physical_location: Point<i32, smithay::utils::Physical>,
    client_physical_geometry: Option<Rectangle<i32, smithay::utils::Physical>>,
    output_origin: Point<i32, Logical>,
    output_scale: Scale<f64>,
    clip_scale: Scale<f64>,
    visual: WindowVisualState,
    alpha: f32,
    content_clip: Option<crate::ssd::ContentClip>,
) -> Vec<TtyRenderElements> {
    if let Some(content_clip) = content_clip.filter(|clip| clip.clips_surface) {
        let clipped = window_render::clipped_surface_elements(
            window,
            renderer,
            physical_location,
            client_physical_geometry,
            output_origin,
            output_scale,
            clip_scale,
            alpha,
            Some(content_clip),
            false,
        )
        .inspect_err(|error| {
            warn!(
                ?error,
                "failed to build clipped root surface source elements"
            );
        })
        .unwrap_or_default();

        let mut root_raw_element = None;
        for element in clipped {
            match element {
                window_render::WindowClipElement::Clipped(element) => {
                    return transform_clipped_elements(vec![element], visual);
                }
                window_render::WindowClipElement::Raw(element) if root_raw_element.is_none() => {
                    root_raw_element = Some(element);
                }
                window_render::WindowClipElement::Raw(_) => {}
            }
        }
        if let Some(element) = root_raw_element {
            return transform_window_elements(
                vec![element],
                visual,
                TtyRenderElements::Window,
                TtyRenderElements::TransformedWindow,
            );
        }
    }

    transform_window_elements(
        window_render::root_surface_elements(
            window,
            renderer,
            physical_location,
            output_scale,
            alpha,
        ),
        visual,
        TtyRenderElements::Window,
        TtyRenderElements::TransformedWindow,
    )
}

fn non_root_surface_elements_for_window(
    window: &smithay::desktop::Window,
    renderer: &mut GlesRenderer,
    physical_location: Point<i32, smithay::utils::Physical>,
    client_physical_geometry: Option<Rectangle<i32, smithay::utils::Physical>>,
    output_origin: Point<i32, Logical>,
    output_scale: Scale<f64>,
    clip_scale: Scale<f64>,
    visual: WindowVisualState,
    alpha: f32,
    content_clip: Option<crate::ssd::ContentClip>,
) -> Vec<TtyRenderElements> {
    if let Some(content_clip) = content_clip.filter(|clip| clip.clips_surface) {
        let clipped = window_render::clipped_surface_elements(
            window,
            renderer,
            physical_location,
            client_physical_geometry,
            output_origin,
            output_scale,
            clip_scale,
            alpha,
            Some(content_clip),
            false,
        )
        .inspect_err(|error| {
            warn!(?error, "failed to build clipped non-root surface elements");
        })
        .unwrap_or_default();

        let mut saw_root = false;
        let mut raw_elements = Vec::new();
        for element in clipped {
            match element {
                window_render::WindowClipElement::Clipped(_) => {
                    saw_root = true;
                }
                window_render::WindowClipElement::Raw(element) => raw_elements.push(element),
            }
        }
        if !saw_root && !raw_elements.is_empty() {
            raw_elements.remove(0);
        }

        return transform_window_elements(
            raw_elements,
            visual,
            TtyRenderElements::Window,
            TtyRenderElements::TransformedWindow,
        );
    }

    transform_window_elements(
        window_render::non_root_surface_elements(
            window,
            renderer,
            physical_location,
            output_scale,
            alpha,
        ),
        visual,
        TtyRenderElements::Window,
        TtyRenderElements::TransformedWindow,
    )
}

fn transform_text_elements(
    elements: Vec<crate::backend::text::DecorationTextureElements>,
    root_origin: Point<i32, smithay::utils::Physical>,
    visual: WindowVisualState,
) -> Result<Vec<TtyRenderElements>, Box<dyn std::error::Error>> {
    if is_identity_visual_geometry(visual) {
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
    if is_identity_visual_geometry(visual) {
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
    if is_identity_visual_geometry(visual) {
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
    if is_identity_visual_geometry(visual) {
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
            let transformed =
                TtyRenderElements::TransformedBackdrop(RelocateRenderElement::from_element(
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
    let stride = probe_rect.size.w.max(1) as usize * 4;
    let sample_raw = |x: i32, y: i32| -> Option<[u8; 4]> {
        if x < 0 || y < 0 || x >= probe_rect.size.w || y >= probe_rect.size.h {
            return None;
        }
        let index = y as usize * stride + x as usize * 4;
        bytes
            .get(index..index + 4)
            .map(|px| [px[0], px[1], px[2], px[3]])
    };
    let sample_points = [
        (0, 0),
        (probe_rect.size.w / 2, probe_rect.size.h / 2),
        (probe_rect.size.w.saturating_sub(1), 0),
        (probe_rect.size.w.saturating_sub(1), probe_rect.size.h / 2),
        (
            probe_rect.size.w.saturating_sub(1),
            probe_rect.size.h.saturating_sub(1),
        ),
    ];
    let raw_samples = sample_points
        .into_iter()
        .filter_map(|(x, y)| sample_raw(x, y).map(|raw| format!("({},{}):{:?}", x, y, raw)))
        .collect::<Vec<_>>();

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
        raw_samples = ?raw_samples,
        "gap readback tty client edge probe"
    );
}

static GAP_FINAL_READBACK_COUNT: AtomicUsize = AtomicUsize::new(0);

fn gap_final_readback_debug_enabled() -> bool {
    std::env::var_os("SHOJI_GAP_FINAL_READBACK_DEBUG").is_some()
}

fn translate_physical_rect(
    rect: smithay::utils::Rectangle<i32, smithay::utils::Physical>,
    offset: smithay::utils::Point<i32, smithay::utils::Physical>,
) -> smithay::utils::Rectangle<i32, smithay::utils::Physical> {
    smithay::utils::Rectangle::new(rect.loc + offset, rect.size)
}

fn transform_physical_rect_for_visual(
    rect: smithay::utils::Rectangle<i32, smithay::utils::Physical>,
    visual: WindowVisualState,
) -> smithay::utils::Rectangle<i32, smithay::utils::Physical> {
    if is_identity_visual_geometry(visual) {
        return rect;
    }

    let left = visual.origin.x as f64
        + (rect.loc.x - visual.origin.x) as f64 * visual.scale.x
        + visual.translation.x as f64;
    let top = visual.origin.y as f64
        + (rect.loc.y - visual.origin.y) as f64 * visual.scale.y
        + visual.translation.y as f64;
    let right = visual.origin.x as f64
        + (rect.loc.x + rect.size.w - visual.origin.x) as f64 * visual.scale.x
        + visual.translation.x as f64;
    let bottom = visual.origin.y as f64
        + (rect.loc.y + rect.size.h - visual.origin.y) as f64 * visual.scale.y
        + visual.translation.y as f64;

    let x = left.min(right).floor() as i32;
    let y = top.min(bottom).floor() as i32;
    let width = (left.max(right) - left.min(right)).ceil().max(0.0) as i32;
    let height = (top.max(bottom) - top.min(bottom)).ceil().max(0.0) as i32;
    smithay::utils::Rectangle::new(
        smithay::utils::Point::from((x, y)),
        smithay::utils::Size::from((width, height)),
    )
}

fn log_managed_rect_physical_debug(
    window_id: &str,
    output_name: &str,
    root_rect: crate::ssd::LogicalRect,
    content_clip: Option<crate::ssd::ContentClip>,
    output_geo: smithay::utils::Rectangle<i32, Logical>,
    scale: Scale<f64>,
) {
    if !managed_rect_debug_enabled() {
        return;
    }

    let scale_x = scale.x.abs().max(0.0001);
    let scale_y = scale.y.abs().max(0.0001);
    let root_origin = root_physical_origin(root_rect, output_geo, scale);
    let root_size_independent = smithay::utils::Size::<i32, smithay::utils::Physical>::from((
        ((root_rect.width as f64) * scale_x).round().max(0.0) as i32,
        ((root_rect.height as f64) * scale_y).round().max(0.0) as i32,
    ));
    let root_global_edges =
        crate::backend::visual::logical_rect_to_physical_rect(root_rect, output_geo.loc, scale);

    let client = content_clip.map(|clip| {
        let local = crate::backend::visual::relative_physical_rect_from_root_precise(
            clip.rect_precise,
            root_rect,
            output_geo,
            scale,
        );
        let independent = smithay::utils::Rectangle::new(
            smithay::utils::Point::from((root_origin.x + local.loc.x, root_origin.y + local.loc.y)),
            local.size,
        );
        let global = crate::backend::visual::relative_physical_rect_from_root_global_edges_precise(
            clip.rect_precise,
            root_rect,
            output_geo,
            scale,
        );
        (
            independent.loc.x,
            independent.loc.y,
            independent.size.w,
            independent.size.h,
            independent.loc.x + independent.size.w,
            independent.loc.y + independent.size.h,
            global.loc.x,
            global.loc.y,
            global.size.w,
            global.size.h,
            global.loc.x + global.size.w,
            global.loc.y + global.size.h,
        )
    });

    tracing::info!(
        output = %output_name,
        window_id = %window_id,
        scale_x,
        scale_y,
        logical_root_x = root_rect.x,
        logical_root_y = root_rect.y,
        logical_root_width = root_rect.width,
        logical_root_height = root_rect.height,
        logical_root_right = root_rect.x + root_rect.width,
        logical_root_bottom = root_rect.y + root_rect.height,
        root_origin_x = root_origin.x,
        root_origin_y = root_origin.y,
        root_size_independent_width = root_size_independent.w,
        root_size_independent_height = root_size_independent.h,
        root_independent_right = root_origin.x + root_size_independent.w,
        root_independent_bottom = root_origin.y + root_size_independent.h,
        root_global_x = root_global_edges.loc.x,
        root_global_y = root_global_edges.loc.y,
        root_global_width = root_global_edges.size.w,
        root_global_height = root_global_edges.size.h,
        root_global_right = root_global_edges.loc.x + root_global_edges.size.w,
        root_global_bottom = root_global_edges.loc.y + root_global_edges.size.h,
        client_physical = ?client,
        "managed rect debug: physical geometry"
    );
}

fn log_kinetic_window_render_state_debug(
    phase: &str,
    output_name: &str,
    decoration: &crate::ssd::WindowDecorationState,
    output_geo: smithay::utils::Rectangle<i32, Logical>,
    scale: Scale<f64>,
    visual_state: Option<WindowVisualState>,
    render_allowed: bool,
) {
    if !managed_rect_debug_enabled() && !kinetic_scroll_trace_debug_enabled() {
        return;
    }

    let root_rect = decoration.layout.root.rect;
    let visual_root = transformed_root_rect(root_rect, decoration.visual_transform);
    let root_physical =
        crate::backend::visual::logical_rect_to_physical_rect(root_rect, output_geo.loc, scale);
    let visual_physical =
        crate::backend::visual::logical_rect_to_physical_rect(visual_root, output_geo.loc, scale);
    let managed_rect = decoration.managed_window.rect.map(|rect| {
        (
            rect.x,
            rect.y,
            rect.width,
            rect.height,
            rect.x + rect.width,
            rect.y + rect.height,
        )
    });
    let static_managed_rect = decoration.static_managed_window.rect.map(|rect| {
        (
            rect.x,
            rect.y,
            rect.width,
            rect.height,
            rect.x + rect.width,
            rect.y + rect.height,
        )
    });
    let visual_origin = visual_state.map(|visual| (visual.origin.x, visual.origin.y));
    let visual_scale = visual_state.map(|visual| (visual.scale.x, visual.scale.y));
    let visual_translation =
        visual_state.map(|visual| (visual.translation.x, visual.translation.y));
    let visual_opacity = visual_state.map(|visual| visual.opacity);

    tracing::info!(
        phase,
        output = %output_name,
        window_id = %decoration.snapshot.id,
        title = %decoration.snapshot.title,
        render_allowed,
        managed = decoration.managed_window.managed,
        visible = decoration.managed_window.visible,
        idle = decoration.managed_window.idle,
        interactive = decoration.managed_window.interactive,
        animation_active = decoration.managed_window_animation_active,
        client_rect_stale = decoration.client_rect_potentially_stale,
        managed_rect = ?managed_rect,
        static_managed_rect = ?static_managed_rect,
        layout_root = ?(
            root_rect.x,
            root_rect.y,
            root_rect.width,
            root_rect.height,
            root_rect.x + root_rect.width,
            root_rect.y + root_rect.height,
        ),
        visual_root = ?(
            visual_root.x,
            visual_root.y,
            visual_root.width,
            visual_root.height,
            visual_root.x + visual_root.width,
            visual_root.y + visual_root.height,
        ),
        root_physical = ?(
            root_physical.loc.x,
            root_physical.loc.y,
            root_physical.size.w,
            root_physical.size.h,
            root_physical.loc.x + root_physical.size.w,
            root_physical.loc.y + root_physical.size.h,
        ),
        visual_physical = ?(
            visual_physical.loc.x,
            visual_physical.loc.y,
            visual_physical.size.w,
            visual_physical.size.h,
            visual_physical.loc.x + visual_physical.size.w,
            visual_physical.loc.y + visual_physical.size.h,
        ),
        transform_translate_x = decoration.visual_transform.translate_x,
        transform_translate_y = decoration.visual_transform.translate_y,
        transform_scale_x = decoration.visual_transform.scale_x,
        transform_scale_y = decoration.visual_transform.scale_y,
        transform_opacity = decoration.visual_transform.opacity,
        visual_origin = ?visual_origin,
        visual_scale = ?visual_scale,
        visual_translation = ?visual_translation,
        visual_opacity = ?visual_opacity,
        "kinetic-scroll render-state"
    );
}

fn log_gap_final_composite_readback(
    renderer: &mut GlesRenderer,
    output_scale: Scale<f64>,
    elements: &[TtyRenderElements],
    decoration: &crate::ssd::WindowDecorationState,
    output_geo: smithay::utils::Rectangle<i32, Logical>,
    visual: WindowVisualState,
    output_name: &str,
    window_id: &str,
) {
    if !gap_final_readback_debug_enabled() {
        return;
    }
    if visual.opacity < 0.99 || elements.len() <= 1 {
        return;
    }
    // Keep the debug log bounded. This readback renders several offscreen
    // probes and is meant for one short reproduction run.
    if GAP_FINAL_READBACK_COUNT.fetch_add(1, Ordering::Relaxed) >= 12 {
        return;
    }

    let root_rect = decoration.layout.root.rect;
    let root_origin =
        crate::backend::visual::root_physical_origin(root_rect, output_geo, output_scale);
    let titlebar_shader = decoration
        .shader_buffers
        .iter()
        .find(|buffer| buffer.stable_key.ends_with(":shader") && buffer.rect.height == 30)
        .or_else(|| decoration.shader_buffers.first());
    let Some(titlebar_shader) = titlebar_shader else {
        return;
    };

    let titlebar_pre = titlebar_shader
        .rect_precise
        .map(|rect| {
            crate::backend::visual::relative_physical_rect_from_root_precise(
                rect,
                root_rect,
                output_geo,
                output_scale,
            )
        })
        .unwrap_or_else(|| {
            crate::backend::visual::relative_physical_rect_from_root_snapped_edges(
                titlebar_shader.rect,
                root_rect,
                output_geo,
                output_scale,
            )
        });
    let titlebar_geometry = transform_physical_rect_for_visual(
        translate_physical_rect(titlebar_pre, root_origin),
        visual,
    );

    let parent_box_border = decoration.buffers.iter().find(|buffer| {
        buffer.source_kind == "box"
            && buffer.border_width > 0.0
            && buffer.hole_rect_precise.is_some_and(|hole| {
                let shader = titlebar_shader.rect_precise.unwrap_or_else(|| {
                    crate::backend::visual::PreciseLogicalRect {
                        x: titlebar_shader.rect.x as f32,
                        y: titlebar_shader.rect.y as f32,
                        width: titlebar_shader.rect.width as f32,
                        height: titlebar_shader.rect.height as f32,
                    }
                });
                shader.x >= hole.x - 1.0
                    && shader.y >= hole.y - 1.0
                    && shader.x + shader.width <= hole.x + hole.width + 1.0
            })
    });

    let parent_hole_geometry = parent_box_border.and_then(|buffer| {
        buffer.hole_rect_precise.map(|hole| {
            let pre = crate::backend::visual::relative_physical_rect_from_root_precise(
                hole,
                root_rect,
                output_geo,
                output_scale,
            );
            transform_physical_rect_for_visual(translate_physical_rect(pre, root_origin), visual)
        })
    });
    let parent_border_geometry = parent_box_border.map(|buffer| {
        let pre = buffer
            .rect_precise
            .map(|rect| {
                crate::backend::visual::relative_physical_rect_from_root_precise(
                    rect,
                    root_rect,
                    output_geo,
                    output_scale,
                )
            })
            .unwrap_or_else(|| {
                crate::backend::visual::relative_physical_rect_from_root_snapped_edges(
                    buffer.rect,
                    root_rect,
                    output_geo,
                    output_scale,
                )
            });
        transform_physical_rect_for_visual(translate_physical_rect(pre, root_origin), visual)
    });
    let titlebar_vs_parent_hole_delta = parent_hole_geometry.map(|hole| {
        (
            titlebar_geometry.loc.x - hole.loc.x,
            titlebar_geometry.loc.y - hole.loc.y,
            (titlebar_geometry.loc.x + titlebar_geometry.size.w) - (hole.loc.x + hole.size.w),
            (titlebar_geometry.loc.y + titlebar_geometry.size.h) - (hole.loc.y + hole.size.h),
        )
    });

    tracing::info!(
        output = output_name,
        window_id,
        element_count = elements.len(),
        visual = ?visual,
        root_rect = ?root_rect,
        root_origin = ?root_origin,
        titlebar_key = %titlebar_shader.stable_key,
        titlebar_rect = ?titlebar_shader.rect,
        titlebar_rect_precise = ?titlebar_shader.rect_precise,
        titlebar_geometry = ?titlebar_geometry,
        parent_box_border_key = ?parent_box_border.map(|buffer| buffer.stable_key.as_str()),
        parent_border_geometry = ?parent_border_geometry,
        parent_hole_geometry = ?parent_hole_geometry,
        titlebar_vs_parent_hole_delta = ?titlebar_vs_parent_hole_delta,
        "gap final composite geometry"
    );

    let mut probes = Vec::new();
    for inset in [0, 1, 2, 3, 4, 8] {
        if titlebar_geometry.size.w > inset {
            probes.push((
                format!("titlebar-right-inset-{inset}px"),
                smithay::utils::Rectangle::new(
                    smithay::utils::Point::from((
                        titlebar_geometry.loc.x + titlebar_geometry.size.w - 1 - inset,
                        titlebar_geometry.loc.y,
                    )),
                    smithay::utils::Size::from((1, titlebar_geometry.size.h)),
                ),
            ));
        }
    }
    if let Some(hole) = parent_hole_geometry {
        for offset in [-2, -1, 0, 1, 2] {
            probes.push((
                format!("parent-hole-right-offset-{offset}px"),
                smithay::utils::Rectangle::new(
                    smithay::utils::Point::from((
                        hole.loc.x + hole.size.w + offset,
                        titlebar_geometry.loc.y,
                    )),
                    smithay::utils::Size::from((1, titlebar_geometry.size.h)),
                ),
            ));
        }
    }

    for (side, probe_rect) in probes {
        log_gap_readback_probe(
            renderer,
            output_scale,
            elements,
            probe_rect,
            &side,
            "final-window-composite",
            output_name,
            window_id,
        );
    }
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
    tracker: &mut smithay::backend::renderer::damage::OutputDamageTracker,
    elements: &[TtyRenderElements],
) -> Result<
    Option<crate::backend::snapshot::LiveWindowSnapshot>,
    smithay::backend::renderer::gles::GlesError,
> {
    let capture_origin = capture_origin_for_logical_rect(output_geo, rect, scale);
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
    snapshot::capture_snapshot(
        renderer, existing, tracker, rect, 0, true, scale, &relocated,
    )
}

fn capture_origin_for_logical_rect(
    output_geo: smithay::utils::Rectangle<i32, Logical>,
    rect: crate::ssd::LogicalRect,
    scale: smithay::utils::Scale<f64>,
) -> smithay::utils::Point<i32, smithay::utils::Physical> {
    (smithay::utils::Point::from((rect.x, rect.y)) - output_geo.loc)
        .to_f64()
        .to_physical_precise_round(scale)
}

fn window_effect_signature(
    placement: &'static str,
    window_rect: crate::ssd::LogicalRect,
    effect: &crate::ssd::WindowEffectSlot,
    scale: smithay::utils::Scale<f64>,
    window_elements: &[TtyRenderElements],
) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    placement.hash(&mut hasher);
    (
        window_rect.x,
        window_rect.y,
        window_rect.width,
        window_rect.height,
    )
        .hash(&mut hasher);
    (
        effect.outsets.left,
        effect.outsets.right,
        effect.outsets.top,
        effect.outsets.bottom,
    )
        .hash(&mut hasher);
    format!("{:?}", effect.effect).hash(&mut hasher);
    scale.x.to_bits().hash(&mut hasher);
    scale.y.to_bits().hash(&mut hasher);
    snapshot::render_element_scene_signature(window_elements, scale).hash(&mut hasher);
    hasher.finish()
}

fn window_effect_element_state(
    decoration: &mut crate::ssd::WindowDecorationState,
    cache_key: String,
    signature: u64,
) -> (
    smithay::backend::renderer::element::Id,
    smithay::backend::renderer::utils::CommitCounter,
) {
    let state = decoration.window_effect_cache.entry(cache_key).or_default();
    if state.signature != signature {
        state.signature = signature;
        state.commit_counter.increment();
    }
    (state.id.clone(), state.commit_counter)
}

fn layer_effect_element_state(
    cache: &mut std::collections::HashMap<
        String,
        crate::backend::shader_effect::WindowEffectElementState,
    >,
    cache_key: String,
    signature: u64,
) -> (
    smithay::backend::renderer::element::Id,
    smithay::backend::renderer::utils::CommitCounter,
) {
    let state = cache.entry(cache_key).or_default();
    if state.signature != signature {
        state.signature = signature;
        state.commit_counter.increment();
    }
    (state.id.clone(), state.commit_counter)
}

fn restore_presented_window_surface_primary_outputs(
    space: &smithay::desktop::Space<smithay::desktop::Window>,
    output: &Output,
    render_element_states: &smithay::backend::renderer::element::RenderElementStates,
) {
    use smithay::backend::renderer::element::{
        Id, RenderElementPresentationState, RenderElementState, RenderElementStates,
    };
    use smithay::desktop::utils::update_surface_primary_scanout_output;

    for window in space.elements_for_output(output) {
        let mut window_had_presented_surface = false;
        window.with_surfaces(|surface, _| {
            if render_element_states.element_was_presented(Id::from_wayland_resource(surface)) {
                window_had_presented_surface = true;
            }
        });
        if !window_had_presented_surface {
            continue;
        }

        let mut synthetic_states = RenderElementStates::default();
        window.with_surfaces(|surface, _| {
            synthetic_states.states.insert(
                Id::from_wayland_resource(surface),
                RenderElementState {
                    visible_area: usize::MAX,
                    presentation_state: RenderElementPresentationState::Rendering { reason: None },
                    needs_capture: false,
                },
            );
        });
        window.with_surfaces(|surface, states| {
            update_surface_primary_scanout_output(
                surface,
                output,
                states,
                None,
                &synthetic_states,
                crate::presentation::area_primary_scanout_compare,
            );
        });
    }
}

fn window_effect_elements(
    renderer: &mut GlesRenderer,
    output: &Output,
    output_geo: smithay::utils::Rectangle<i32, Logical>,
    scale: smithay::utils::Scale<f64>,
    window_id: &str,
    placement: &'static str,
    element_id: smithay::backend::renderer::element::Id,
    commit_counter: smithay::backend::renderer::utils::CommitCounter,
    window_rect: crate::ssd::LogicalRect,
    effect: &crate::ssd::WindowEffectSlot,
    window_elements: &[TtyRenderElements],
) -> Result<Vec<TtyRenderElements>, crate::backend::shader_effect::ShaderEffectError> {
    if window_elements.is_empty() || window_rect.width <= 0 || window_rect.height <= 0 {
        if window_effect_debug_enabled() {
            info!(
                output = %output.name(),
                window_id,
                placement,
                window_rect = ?window_rect,
                window_element_count = window_elements.len(),
                "window effect debug: skipping effect due empty input"
            );
        }
        return Ok(Vec::new());
    }

    let rect = expand_logical_rect(window_rect, effect.outsets);
    if rect.width <= 0 || rect.height <= 0 {
        if window_effect_debug_enabled() {
            info!(
                output = %output.name(),
                window_id,
                placement,
                window_rect = ?window_rect,
                expanded_rect = ?rect,
                outsets = ?effect.outsets,
                "window effect debug: skipping effect due invalid expanded rect"
            );
        }
        return Ok(Vec::new());
    }
    let logical = Rectangle::new(
        Point::from((rect.x, rect.y)),
        (rect.width, rect.height).into(),
    );
    if logical.intersection(output_geo).is_none() {
        if window_effect_debug_enabled() {
            info!(
                output = %output.name(),
                window_id,
                placement,
                output_geo = ?output_geo,
                expanded_rect = ?rect,
                "window effect debug: skipping effect outside output"
            );
        }
        return Ok(Vec::new());
    }
    if window_effect_debug_enabled() {
        let first_geometry = window_elements
            .first()
            .map(|element| smithay::backend::renderer::element::Element::geometry(element, scale));
        info!(
            output = %output.name(),
            window_id,
            placement,
            window_rect = ?window_rect,
            expanded_rect = ?rect,
            outsets = ?effect.outsets,
            window_element_count = window_elements.len(),
            first_window_element_geometry = ?first_geometry,
            "window effect debug: capturing source"
        );
    }

    let mut tracker = smithay::backend::renderer::damage::OutputDamageTracker::new(
        (0, 0),
        1.0,
        Transform::Normal,
    );
    let Some(source) = capture_snapshot_from_output_elements(
        renderer,
        output_geo,
        rect,
        scale,
        None,
        &mut tracker,
        window_elements,
    )
    .map_err(crate::backend::shader_effect::ShaderEffectError::Gles)?
    else {
        if window_effect_debug_enabled() {
            info!(
                output = %output.name(),
                window_id,
                placement,
                expanded_rect = ?rect,
                "window effect debug: source capture returned none"
            );
        }
        return Ok(Vec::new());
    };
    let texture_size = source.texture.size();
    if window_effect_debug_enabled() {
        info!(
            output = %output.name(),
            window_id,
            placement,
            source_texture_size = ?texture_size,
            source_rect = ?source.rect,
            "window effect debug: source captured"
        );
    }
    // The captured subject feeds every subject alias (backdrop/windowSource,
    // layerSource, popupSource) so the slot's declared input kind — and any
    // extra subject-sampling shader textures — resolve inside these slots.
    let texture =
        crate::backend::shader_effect::apply_effect_pipeline_cached_for_key_with_captured_subject(
            renderer,
            format!(
                "tty:window-effect:{}:{}:{}",
                output.name(),
                window_id,
                placement
            ),
            source.texture,
            (texture_size.w, texture_size.h),
            None,
            Some((texture_size.w, texture_size.h)),
            &effect.effect,
        )?;
    if window_effect_debug_enabled() {
        info!(
            output = %output.name(),
            window_id,
            placement,
            effect_texture_size = ?texture.size(),
            display_rect = ?logical,
            "window effect debug: pipeline applied"
        );
    }
    let texture_size = texture.size();
    let capture_origin = capture_origin_for_logical_rect(output_geo, rect, scale);
    let geometry = Rectangle::new(capture_origin, (texture_size.w, texture_size.h).into());
    let element = crate::backend::shader_effect::backdrop_shader_element_with_geometry(
        renderer,
        element_id,
        commit_counter,
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
        format!(
            "window-effect:{}:{}:{}",
            placement,
            output.name(),
            window_id
        ),
    )?;
    if window_effect_debug_enabled() {
        let geometry = smithay::backend::renderer::element::Element::geometry(&element, scale);
        info!(
            output = %output.name(),
            window_id,
            placement,
            geometry = ?geometry,
            "window effect debug: render element created"
        );
    }
    Ok(vec![TtyRenderElements::Backdrop(element)])
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

fn layer_source_effect_elements(
    renderer: &mut GlesRenderer,
    output: &Output,
    output_geo: smithay::utils::Rectangle<i32, Logical>,
    scale: smithay::utils::Scale<f64>,
    layer_id: &str,
    layer_rect: crate::ssd::LogicalRect,
    effects: &crate::ssd::WindowEffectConfig,
    // What gets shown when no `replace` slot is active. Excludes the layer's
    // popups: those are displayed (and effect-composed) separately so popup
    // effects don't double-render them.
    display_elements: Vec<TtyRenderElements>,
    // What layerSource() captures sample. Includes popups ("full" semantics).
    capture_elements: &[TtyRenderElements],
    cache: &mut std::collections::HashMap<
        String,
        crate::backend::shader_effect::WindowEffectElementState,
    >,
) -> Vec<TtyRenderElements> {
    let mut render_slot = |placement: &'static str,
                           effect: &crate::ssd::WindowEffectSlot|
     -> Vec<TtyRenderElements> {
        if !matches!(effect.effect.input, EffectInput::LayerSource(_)) {
            return Vec::new();
        }
        let signature =
            window_effect_signature(placement, layer_rect, effect, scale, capture_elements);
        let (element_id, commit_counter) = layer_effect_element_state(
            cache,
            format!("{}@{}@{}", layer_id, placement, output.name()),
            signature,
        );
        window_effect_elements(
            renderer,
            output,
            output_geo,
            scale,
            layer_id,
            placement,
            element_id,
            commit_counter,
            layer_rect,
            effect,
            capture_elements,
        )
        .inspect_err(|error| {
            warn!(
                layer_id,
                placement,
                ?error,
                "failed to build layer source effect"
            );
        })
        .unwrap_or_default()
    };

    let in_front = effects
        .in_front
        .as_ref()
        .map(|effect| render_slot("layer-in-front", effect))
        .unwrap_or_default();
    let replacement = effects
        .replace
        .as_ref()
        .map(|effect| render_slot("layer-replace", effect))
        .unwrap_or_default();
    let behind_root = effects
        .behind_root_surface
        .as_ref()
        .map(|effect| render_slot("layer-behind-root-surface", effect))
        .unwrap_or_default();
    let behind = effects
        .behind
        .as_ref()
        .map(|effect| render_slot("layer-behind", effect))
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
/// configured popup effects (`COMPOSITOR.effect.popup`). Popups without
/// an assignment pass through unchanged. Returned front-to-back, in the same
/// order the popups would have rendered inside `layer_surface_elements`.
fn composed_popup_scene_elements(
    renderer: &mut GlesRenderer,
    output: &Output,
    output_geo: smithay::utils::Rectangle<i32, Logical>,
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
    configured_background_effect: Option<&crate::ssd::BackgroundEffectConfig>,
) -> Vec<TtyRenderElements> {
    let groups =
        window_render::layer_surface_popup_groups(renderer, output, layer_surface, scale, 1.0);
    if groups.is_empty() {
        return Vec::new();
    }
    let output_loc = output.current_location();
    let mut elements = Vec::new();
    for (popup_id, local_rect, local_buffer_origin, popup_surface, raw_elements) in groups {
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
                    TtyRenderElements::PolicyWindow(
                        crate::backend::visual::IgnoredOpaqueRegionElement::from_element(element),
                    )
                } else {
                    TtyRenderElements::Window(element)
                }
            })
            .collect::<Vec<_>>();
        let protocol_background = popup_protocol_background_elements(
            renderer,
            output,
            output_geo,
            scale,
            &popup_id,
            &popup_surface,
            smithay::utils::Point::from((
                output_loc.x + local_buffer_origin.x,
                output_loc.y + local_buffer_origin.y,
            )),
            configured_background_effect,
            popup_framebuffer_effect_states,
        );
        let Some(effects) = configured_popup_effects.get(&popup_id) else {
            elements.extend(popup_elements);
            elements.extend(protocol_background);
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
        elements.extend(protocol_background);
    }
    elements
}

/// Backdrop-effect elements for a popup that requested background blur via
/// `ext-background-effect-v1` (fcitx5's candidate window does this). The
/// configured `COMPOSITOR.effect.background_effect` pipeline renders behind
/// the popup, clipped to the client's requested blur region. Only
/// framebuffer-resolvable pipelines are supported — popups render inline
/// with their parent's element stream, so there is no offline "scene below
/// the popup" capture (same restriction as popup `behind` effects).
#[allow(clippy::too_many_arguments)]
fn popup_protocol_background_elements(
    renderer: &mut GlesRenderer,
    output: &Output,
    output_geo: smithay::utils::Rectangle<i32, Logical>,
    scale: smithay::utils::Scale<f64>,
    popup_id: &str,
    popup_surface: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
    buffer_origin: smithay::utils::Point<i32, Logical>,
    configured_background_effect: Option<&crate::ssd::BackgroundEffectConfig>,
    popup_framebuffer_effect_states: &mut std::collections::HashMap<
        String,
        crate::backend::shader_effect::ShaderEffectElementState,
    >,
) -> Vec<TtyRenderElements> {
    let Some(config) = configured_background_effect else {
        return Vec::new();
    };
    let rects =
        window_render::protocol_background_effect_rects_for_popup(popup_surface, buffer_origin);
    if rects.is_empty() {
        return Vec::new();
    }
    if !config.effect.supports_popup_framebuffer_backdrop() {
        return Vec::new();
    }
    let stable_key = format!("{}@popup-protocol-background@{}", popup_id, output.name());
    match crate::backend::shader_effect::framebuffer_backdrop_element_for_output_rects_with_popup_source(
        renderer,
        popup_framebuffer_effect_states
            .entry(stable_key)
            .or_default(),
        &rects,
        config.effect.clone(),
        output_geo,
        scale,
        1.0,
        None,
    ) {
        Ok(Some(element)) => vec![TtyRenderElements::Decoration(
            decoration::DecorationSceneElements::Backdrop(element),
        )],
        Ok(None) => Vec::new(),
        Err(error) => {
            warn!(
                popup_id,
                ?error,
                "failed to build popup protocol background effect"
            );
            Vec::new()
        }
    }
}

/// Render all layer-shell popups in one front-most pass.
///
/// `zwlr_layer_surface_v1.get_popup` makes the popup a child of its layer
/// surface, but placing it directly next to a Bottom-layer parent would leave
/// it behind ordinary toplevels. This mirrors Hyprland's separate layer-popup
/// pass while preserving the normal layer ordering for the parent roots.
fn layer_popup_scene_elements(
    renderer: &mut GlesRenderer,
    output: &Output,
    output_geo: smithay::utils::Rectangle<i32, Logical>,
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
    configured_background_effect: Option<&crate::ssd::BackgroundEffectConfig>,
) -> Vec<TtyRenderElements> {
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
                configured_background_effect,
            )
        })
        .collect()
}

/// Display elements for all popups of a toplevel window, each composed with
/// its configured popup effects. `location` is the window's output-local
/// physical render location (same as `popup_elements` receives). Used only
/// while the window's visual transform is identity: effect elements cannot
/// ride the window animation transform, so animating windows fall back to the
/// raw popup pass-through.
fn composed_window_popup_scene_elements(
    renderer: &mut GlesRenderer,
    output: &Output,
    output_geo: smithay::utils::Rectangle<i32, Logical>,
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
    configured_background_effect: Option<&crate::ssd::BackgroundEffectConfig>,
) -> Vec<TtyRenderElements> {
    let groups =
        window_render::window_popup_groups(window, renderer, location, output_geo, scale, alpha);
    if groups.is_empty() {
        return Vec::new();
    }
    let mut elements = Vec::new();
    for (popup_id, popup_rect, buffer_origin, popup_surface, raw_elements) in groups {
        let popup_elements = raw_elements
            .into_iter()
            .map(TtyRenderElements::Window)
            .collect::<Vec<_>>();
        let protocol_background = popup_protocol_background_elements(
            renderer,
            output,
            output_geo,
            scale,
            &popup_id,
            &popup_surface,
            buffer_origin,
            configured_background_effect,
            popup_framebuffer_effect_states,
        );
        let Some(effects) = configured_popup_effects.get(&popup_id) else {
            elements.extend(popup_elements);
            elements.extend(protocol_background);
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
        elements.extend(protocol_background);
    }
    elements
}

/// Compose a single popup's display elements with its assigned effects.
/// Shared by the layer-popup and window-popup paths.
fn compose_one_popup_elements(
    renderer: &mut GlesRenderer,
    output: &Output,
    output_geo: smithay::utils::Rectangle<i32, Logical>,
    scale: smithay::utils::Scale<f64>,
    popup_id: &str,
    popup_rect: crate::ssd::LogicalRect,
    effects: &crate::ssd::WindowEffectConfig,
    popup_elements: Vec<TtyRenderElements>,
    popup_effect_cache: &mut std::collections::HashMap<
        String,
        crate::backend::shader_effect::WindowEffectElementState,
    >,
    popup_framebuffer_effect_states: &mut std::collections::HashMap<
        String,
        crate::backend::shader_effect::ShaderEffectElementState,
    >,
) -> Vec<TtyRenderElements> {
    {
        let mut render_slot = |placement: &'static str,
                               effect: &crate::ssd::WindowEffectSlot|
         -> Vec<TtyRenderElements> {
            if !matches!(effect.effect.input, EffectInput::PopupSource(_)) {
                return Vec::new();
            }
            let signature =
                window_effect_signature(placement, popup_rect, effect, scale, &popup_elements);
            let (element_id, commit_counter) = layer_effect_element_state(
                popup_effect_cache,
                format!("{}@{}@{}", popup_id, placement, output.name()),
                signature,
            );
            window_effect_elements(
                renderer,
                output,
                output_geo,
                scale,
                popup_id,
                placement,
                element_id,
                commit_counter,
                popup_rect,
                effect,
                &popup_elements,
            )
            .inspect_err(|error| {
                warn!(
                    popup_id,
                    placement,
                    ?error,
                    "failed to build popup source effect"
                );
            })
            .unwrap_or_default()
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
            .filter(|effect| matches!(effect.effect.input, EffectInput::PopupSource(_)))
            .map(|effect| render_slot("popup-behind", effect))
            .unwrap_or_default();
        // `behind` with a backdrop input: popups render inline with their
        // parent's element stream, so there is no offline "scene below the
        // popup" capture. Framebuffer-resolvable effects sample whatever has
        // already been drawn below them at draw time, which is exactly the
        // content behind the popup (including its parent).
        let behind_backdrop = effects
            .behind
            .as_ref()
            .filter(|effect| !matches!(effect.effect.input, EffectInput::PopupSource(_)))
            .filter(|effect| effect.effect.supports_popup_framebuffer_backdrop())
            .and_then(|effect| {
                let rect = expand_logical_rect(popup_rect, effect.outsets);
                let stable_key =
                    format!("{}@popup-behind-framebuffer@{}", popup_id, output.name());
                let popup_source = if effect.effect.uses_popup_source_input() {
                    let mut tracker =
                        smithay::backend::renderer::damage::OutputDamageTracker::new(
                            (0, 0),
                            1.0,
                            Transform::Normal,
                        );
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
                .inspect_err(|error| {
                    warn!(popup_id, ?error, "failed to build popup behind effect");
                })
                .ok()
                .flatten()
            })
            .map(|element| {
                vec![TtyRenderElements::Decoration(
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

fn expand_logical_rect(
    rect: crate::ssd::LogicalRect,
    outsets: crate::ssd::EffectOutsets,
) -> crate::ssd::LogicalRect {
    crate::ssd::LogicalRect::new(
        rect.x.saturating_sub(outsets.left),
        rect.y.saturating_sub(outsets.top),
        rect.width
            .saturating_add(outsets.left)
            .saturating_add(outsets.right),
        rect.height
            .saturating_add(outsets.top)
            .saturating_add(outsets.bottom),
    )
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
    prefer_framebuffer_backdrops: bool,
) -> Vec<(
    usize,
    crate::backend::shader_effect::StableBackdropTextureElement,
    bool,
)> {
    if !has_backdrop_source {
        let Some(decoration) = window_decorations.get(window) else {
            return Vec::new();
        };
        if !decoration.shader_buffers.iter().any(|cached| {
            cached.shader.is_texture_backed()
                && (!prefer_framebuffer_backdrops || !cached.shader.supports_framebuffer_backdrop())
        }) {
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
            // Keep the effect sampling space aligned with the geometry space used for the element
            // itself. In snapshot mode `display_rect` is intentionally left raw so the complete
            // window can be captured once and transformed once; continuing to sample backdrop
            // input from a separately transformed rect here reintroduces the old per-element
            // transform path and causes subtle wobble/tearing on shader-backed nodes.
            let source_effect_rect = display_rect;
            let source_effect_rect_precise = display_rect_precise.unwrap_or_else(|| {
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
            let source_damage_entries = {
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
            };
            if std::env::var_os("SHOJI_SOURCE_DAMAGE_DEBUG").is_some() && (uses_backdrop || uses_xray) && !source_damage_entries.is_empty() {
                tracing::info!(
                    stable_key = %cached.stable_key,
                    source_effect_rect = ?source_effect_rect,
                    uses_backdrop,
                    uses_xray,
                    source_damage_count = source_damage_entries.len(),
                    owners = ?source_damage_entries.iter().map(|e| &e.owner).collect::<Vec<_>>(),
                    rects = ?source_damage_entries.iter().map(|e| &e.rect).collect::<Vec<_>>(),
                    "window-backdrop on-source-damage-box check [tty]"
                );
            }
            let source_damage_hit = crate::backend::shader_effect::source_damage_intersects_rect(
                &cached.shader,
                smithay::utils::Rectangle::new(
                    smithay::utils::Point::from((source_effect_rect.x, source_effect_rect.y)),
                    (source_effect_rect.width, source_effect_rect.height).into(),
                ),
                &source_damage_entries,
            );
            if std::env::var_os("SHOJI_SOURCE_DAMAGE_DEBUG").is_some() && (uses_backdrop || uses_xray) && !source_damage_entries.is_empty() {
                tracing::info!(
                    stable_key = %cached.stable_key,
                    source_damage_hit,
                    "window-backdrop on-source-damage-box result [tty]"
                );
            }
            let existing_cache = window_decorations
                .get(window)
                .and_then(|d| d.backdrop_cache.get(&cache_key))
                .cloned();

            if std::env::var_os("SHOJI_FIREFOX_BACKDROP_DEBUG").is_some() {
                tracing::info!(
                    window_id = %decoration.snapshot.id,
                    title = %decoration.snapshot.title,
                    app_id = ?decoration.snapshot.app_id,
                    stable_key = %cached.stable_key,
                    source_effect_rect = ?source_effect_rect,
                    source_effect_rect_precise = ?source_effect_rect_precise,
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
                    has_existing_cache = existing_cache.is_some(),
                    "backdrop debug: window shader rects"
                );
            }

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
                                let clip = if apply_visual_transform {
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
                                    crate::backend::visual::precise_rect_from_logical(clip),
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
                            cached
                                .clip_radius_precise
                                .unwrap_or(cached.clip_radius as f32),
                            format!(
                                "window-backdrop:{}:{}",
                                decoration.snapshot.id, cached.stable_key
                            ),
                        )
                        .ok()?;
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
                            geometry = ?geometry,
                            from_cache = true,
                            "backdrop debug: window shader element"
                        );
                    }
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
                    "tty-window-backdrop",
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
                capture_scene_texture_for_effect(
                    renderer,
                    "tty-window-xray",
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
            let texture = crate::backend::shader_effect::apply_effect_pipeline_cached_for_key(
                renderer,
                format!(
                    "tty:window-backdrop:{}:{}",
                    decoration.snapshot.id, cache_key
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
                        let clip = if apply_visual_transform {
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
                            crate::backend::visual::precise_rect_from_logical(clip),
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
                cached
                    .clip_radius_precise
                    .unwrap_or(cached.clip_radius as f32),
                format!(
                    "window-backdrop:{}:{}",
                    decoration.snapshot.id, cached.stable_key
                ),
            )
            .ok()?;
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
    let map = layer_map_for_output(output);
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
    upper_layer_source_damage: &[crate::state::OwnedDamageRect],
    lower_layer_scene_generation: u64,
    output: &Output,
    output_geo: smithay::utils::Rectangle<i32, Logical>,
    scale: smithay::utils::Scale<f64>,
    windows_top_to_bottom: &[smithay::desktop::Window],
    // Top/Overlay layers stacked below `layer_surface` (front-to-back). They
    // sit above all toplevel windows, so backdrop captures must include them
    // or an overlay's blur would miss any overlay/top layer behind it.
    upper_layers_below: &[smithay::desktop::LayerSurface],
    layer_surface: &smithay::desktop::LayerSurface,
    alpha: f32,
    layer_backdrop_cache: &mut std::collections::HashMap<
        String,
        crate::backend::shader_effect::CachedBackdropTexture,
    >,
    configured_background_effect: Option<&crate::ssd::BackgroundEffectConfig>,
    custom_background: Option<&crate::ssd::WindowEffectSlot>,
) -> Result<Vec<TtyRenderElements>, Box<dyn std::error::Error>> {
    let layer_id = crate::ssd::layer_runtime_id(layer_surface);
    let custom_config = custom_background.map(|effect| crate::ssd::BackgroundEffectConfig {
        effect: effect.effect.clone(),
    });
    let selected_effect_config = custom_config.or_else(|| configured_background_effect.cloned());
    let Some(effect_config) = selected_effect_config.as_ref() else {
        return Ok(Vec::new());
    };
    let rects = if let Some(effect) = custom_background {
        layer_surface_logical_rect(output, layer_surface)
            .map(|rect| vec![expand_logical_rect(rect, effect.outsets)])
            .unwrap_or_default()
    } else {
        protocol_background_effect_rects_for_layer(output, layer_surface)
    };
    if rects.is_empty() {
        return Ok(Vec::new());
    }

    let Some(effect_rect) = crate::backend::window::bounding_box_for_rects(&rects) else {
        return Ok(Vec::new());
    };
    let blur_padding = effect_config.effect.capture_padding.max(0);
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
        if uses_backdrop {
            entries.extend(collect_layer_source_damage(
                upper_layers_below.iter().cloned(),
                upper_layer_source_damage,
            ));
        }
        if effect_config.effect.uses_layer_source_input() {
            entries.extend(collect_layer_source_damage(
                std::iter::once(layer_surface.clone()),
                upper_layer_source_damage,
            ));
        }
        entries
    };
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
    if uses_backdrop {
        hash_layer_scene_contributors(&mut hasher, output, upper_layers_below, effect_rect);
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
    let mut elements = Vec::new();
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
                        0.0,
                        format!("layer-top:{}:{}", output.name(), rect_key),
                    )?,
                ));
            }
            return Ok(elements);
        }
    }
    let backdrop_texture = if effect_config.effect.uses_backdrop_input() {
        let mut backdrop_scene: Vec<TtyRenderElements> = Vec::new();
        // Upper layers below this one render above every toplevel window, so
        // they go first in the front-to-back capture scene.
        for upper_layer in upper_layers_below {
            if let Ok(mut layer_elements) = layer_surface_scene_elements_for_capture(
                renderer,
                output,
                actual_capture_geo,
                capture_origin_physical,
                scale,
                upper_layer,
            ) {
                backdrop_scene.append(&mut layer_elements);
            }
        }
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
        capture_scene_texture_for_effect(
            renderer,
            "tty-layer-top-backdrop",
            actual_capture_geo,
            scale,
            &backdrop_scene,
        )
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
        capture_scene_texture_for_effect(
            renderer,
            "tty-layer-top-xray",
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
        return Ok(Vec::new());
    };
    let layer_source_texture = if effect_config.effect.uses_layer_source_input() {
        let layer_source_geo = smithay::utils::Rectangle::new(
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
        )?;
        capture_scene_texture_for_effect(
            renderer,
            "tty-layer-top-source",
            layer_source_geo,
            scale,
            &scene,
        )
    } else {
        None
    };
    // Layer source capture can legitimately come up empty (first frame after
    // the surface maps, zero-sized geometry during animations, capture
    // failure). Running the pipeline without it would fail inside
    // resolve_effect_input, so skip the effect for this frame instead.
    if effect_config.effect.uses_layer_source_input() && layer_source_texture.is_none() {
        return Ok(Vec::new());
    }
    let cache_key = format!(
        "tty:layer-top:{}:{}:{}x{}",
        output.name(),
        layer_id,
        effect_rect.width,
        effect_rect.height
    );
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
            cache_key,
            input_texture,
            xray_texture,
            layer_source_texture,
            input_size,
            sample_region,
            output_size,
            &effect_config.effect,
        )?
    } else {
        crate::backend::shader_effect::apply_effect_pipeline_cached_for_key(
            renderer,
            cache_key,
            input_texture,
            xray_texture,
            input_size,
            sample_region,
            output_size,
            &effect_config.effect,
        )?
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
            captured_local_rect = ?smithay::utils::Rectangle::<i32, Logical>::new(
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
                0.0,
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
    configured_layer_effects: &std::collections::HashMap<String, crate::ssd::WindowEffectConfig>,
    layer_effect_cache: &mut std::collections::HashMap<
        String,
        crate::backend::shader_effect::WindowEffectElementState,
    >,
) -> Result<Vec<TtyRenderElements>, Box<dyn std::error::Error>> {
    let (_, lower_layers) = window_render::layer_surfaces_for_output(output);
    let mut elements = Vec::new();
    for (index, layer_surface) in lower_layers.iter().enumerate() {
        let layer_id = crate::ssd::layer_runtime_id(layer_surface);
        let root_elements =
            window_render::layer_surface_root_elements(renderer, output, layer_surface, scale, 1.0)
                .into_iter()
                .map(TtyRenderElements::Window)
                .collect::<Vec<_>>();
        if let (Some(effects), Some(layer_rect)) = (
            configured_layer_effects.get(&layer_id),
            layer_surface_logical_rect(output, layer_surface),
        ) {
            let capture_elements =
                window_render::layer_surface_elements(renderer, output, layer_surface, scale, 1.0)
                    .into_iter()
                    .map(TtyRenderElements::Window)
                    .collect::<Vec<_>>();
            elements.extend(layer_source_effect_elements(
                renderer,
                output,
                output_geo,
                scale,
                &layer_id,
                layer_rect,
                effects,
                root_elements,
                &capture_elements,
                layer_effect_cache,
            ));
        } else {
            elements.extend(root_elements);
        }
        let custom_background = configured_layer_effects
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
        let selected_effect_config = custom_config.or_else(|| effect_config.cloned());
        let Some(effect_config) = selected_effect_config.as_ref() else {
            continue;
        };
        let rects = if let Some(effect) = custom_background.as_ref() {
            layer_surface_logical_rect(output, layer_surface)
                .map(|rect| vec![expand_logical_rect(rect, effect.outsets)])
                .unwrap_or_default()
        } else {
            protocol_background_effect_rects_for_layer(output, layer_surface)
        };
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
            let blur_padding = effect_config.effect.capture_padding.max(0);
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
            let mut relevant_source_damage = collect_layer_source_damage(
                lower_layers.iter().skip(index + 1).cloned(),
                lower_layer_source_damage,
            );
            if effect_config.effect.uses_layer_source_input() {
                relevant_source_damage.extend(collect_layer_source_damage(
                    std::iter::once(layer_surface.clone()),
                    lower_layer_source_damage,
                ));
            }
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
                                0.0,
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
            let mut backdrop_tracker = smithay::backend::renderer::damage::OutputDamageTracker::new(
                (0, 0),
                1.0,
                Transform::Normal,
            );
            let capture_size = crate::backend::visual::logical_size_to_physical_buffer_size(
                actual_capture_geo.size.w,
                actual_capture_geo.size.h,
                scale,
            );
            crate::backend::shader_effect::record_snapshot_fallback(
                "tty-layer-lower",
                capture_size,
                backdrop_scene.len(),
            );
            let snapshot = crate::backend::shader_effect::with_gpu_timing_renderer_span(
                renderer,
                "backdrop-scene-capture",
                capture_size,
                |renderer| {
                    crate::backend::snapshot::capture_snapshot(
                        renderer,
                        None,
                        &mut backdrop_tracker,
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
                    )
                },
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
            let layer_source_texture = if effect_config.effect.uses_layer_source_input() {
                let layer_source_geo = smithay::utils::Rectangle::new(
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
                )?;
                capture_scene_texture_for_effect(
                    renderer,
                    "tty-layer-lower-source",
                    layer_source_geo,
                    scale,
                    &scene,
                )
            } else {
                None
            };
            // Skip the effect this frame when the layer source could not be
            // captured (empty scene / zero-sized geometry); running the
            // pipeline without it would fail inside resolve_effect_input.
            if effect_config.effect.uses_layer_source_input() && layer_source_texture.is_none() {
                continue;
            }
            let input_texture = backdrop_texture
                .clone()
                .or_else(|| xray_texture.clone())
                .ok_or("missing backdrop snapshot")?;
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
                    format!("tty:layer-lower:{}", stable_key),
                    input_texture,
                    xray_texture,
                    layer_source_texture,
                    input_size,
                    sample_region,
                    output_size,
                    &effect_config.effect,
                )?
            } else {
                crate::backend::shader_effect::apply_effect_pipeline_cached_for_key(
                    renderer,
                    format!("tty:layer-lower:{}", stable_key),
                    input_texture,
                    xray_texture,
                    input_size,
                    sample_region,
                    output_size,
                    &effect_config.effect,
                )?
            };
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
                        0.0,
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
    upper_layer_source_damage: &[crate::state::OwnedDamageRect],
    lower_layer_scene_generation: u64,
    configured_layer_effects: &std::collections::HashMap<String, crate::ssd::WindowEffectConfig>,
    configured_background_effect: Option<&crate::ssd::BackgroundEffectConfig>,
    output: &Output,
    output_geo: smithay::utils::Rectangle<i32, Logical>,
    scale: smithay::utils::Scale<f64>,
    windows_top_to_bottom: &[smithay::desktop::Window],
    // Fullscreen fast path: a fullscreen window stacks above the Top layer
    // but below Overlay, so only Overlay surfaces stay visible.
    overlay_only: bool,
    layer_backdrop_cache: &mut std::collections::HashMap<
        String,
        crate::backend::shader_effect::CachedBackdropTexture,
    >,
    layer_framebuffer_effect_states: &mut std::collections::HashMap<
        String,
        crate::backend::shader_effect::ShaderEffectElementState,
    >,
    layer_effect_cache: &mut std::collections::HashMap<
        String,
        crate::backend::shader_effect::WindowEffectElementState,
    >,
) -> Result<Vec<TtyRenderElements>, Box<dyn std::error::Error>> {
    let map = layer_map_for_output(output);
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
        let root_elements = window_render::layer_surface_root_elements(
            renderer,
            output,
            &layer_surface,
            scale,
            1.0,
        )
        .into_iter()
        .map(TtyRenderElements::Window)
        .collect::<Vec<_>>();
        let layer_id = crate::ssd::layer_runtime_id(&layer_surface);
        if let (Some(effects), Some(layer_rect)) = (
            configured_layer_effects.get(&layer_id),
            layer_surface_logical_rect(output, &layer_surface),
        ) {
            // layerSource() "full" captures keep seeing popups, so build the
            // capture scene with them included (display passthrough stays
            // root-only — popups are already displayed above).
            let capture_elements =
                window_render::layer_surface_elements(renderer, output, &layer_surface, scale, 1.0)
                    .into_iter()
                    .map(TtyRenderElements::Window)
                    .collect::<Vec<_>>();
            elements.extend(layer_source_effect_elements(
                renderer,
                output,
                output_geo,
                scale,
                &layer_id,
                layer_rect,
                effects,
                root_elements,
                &capture_elements,
                layer_effect_cache,
            ));
        } else {
            elements.extend(root_elements);
        }
        let custom_background = configured_layer_effects
            .get(&layer_id)
            .and_then(|effects| effects.behind.as_ref())
            .filter(|effect| effect.effect.is_backdrop())
            .cloned();
        let effect_config = configured_background_effect;
        if custom_background.is_none()
            && let Some(effect_config) =
                effect_config.filter(|config| config.effect.supports_framebuffer_backdrop())
        {
            elements.extend(configured_background_framebuffer_effect_elements_for_layer(
                renderer,
                output,
                output_geo,
                scale,
                &layer_surface,
                1.0,
                layer_framebuffer_effect_states,
                effect_config,
            )?);
        } else {
            elements.extend(configured_background_effect_elements_for_layer(
                renderer,
                space,
                window_decorations,
                window_source_damage,
                lower_layer_source_damage,
                upper_layer_source_damage,
                lower_layer_scene_generation,
                output,
                output_geo,
                scale,
                windows_top_to_bottom,
                upper_layers_below,
                &layer_surface,
                1.0,
                layer_backdrop_cache,
                configured_background_effect,
                custom_background.as_ref(),
            )?);
        }
    }
    Ok(elements)
}

fn configured_background_framebuffer_effect_elements_for_layer(
    renderer: &mut GlesRenderer,
    output: &Output,
    output_geo: smithay::utils::Rectangle<i32, Logical>,
    scale: smithay::utils::Scale<f64>,
    layer_surface: &smithay::desktop::LayerSurface,
    alpha: f32,
    states: &mut std::collections::HashMap<
        String,
        crate::backend::shader_effect::ShaderEffectElementState,
    >,
    effect_config: &crate::ssd::BackgroundEffectConfig,
) -> Result<Vec<TtyRenderElements>, crate::backend::shader_effect::ShaderEffectError> {
    let layer_id = crate::ssd::layer_runtime_id(layer_surface);
    let stable_key = format!("tty:layer-top-framebuffer:{}:{}", output.name(), layer_id);
    Ok(
        crate::backend::shader_effect::framebuffer_backdrop_element_for_output_rects(
            renderer,
            states.entry(stable_key).or_default(),
            &protocol_background_effect_rects_for_layer(output, layer_surface),
            effect_config.effect.clone(),
            output_geo,
            scale,
            alpha,
        )?
        .map(|element| {
            vec![TtyRenderElements::Decoration(
                decoration::DecorationSceneElements::Backdrop(element),
            )]
        })
        .unwrap_or_default(),
    )
}

fn configured_background_framebuffer_effect_elements_for_window(
    renderer: &mut GlesRenderer,
    window_decorations: &mut std::collections::HashMap<
        smithay::desktop::Window,
        crate::ssd::WindowDecorationState,
    >,
    window: &smithay::desktop::Window,
    output_geo: smithay::utils::Rectangle<i32, Logical>,
    scale: smithay::utils::Scale<f64>,
    alpha: f32,
    effect_config: &crate::ssd::BackgroundEffectConfig,
) -> Vec<(
    usize,
    crate::backend::shader_effect::StableBackdropFramebufferElement,
)> {
    let Some(decoration) = window_decorations.get(window) else {
        return Vec::new();
    };
    let rects = protocol_background_effect_rects_for_window(window, decoration);
    let Some(decoration) = window_decorations.get_mut(window) else {
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
                effect_config.effect.clone(),
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
            let blur_padding = effect_config.effect.capture_padding.max(0);
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
                            effect_rect.x - decoration.layout.root.rect.x,
                            effect_rect.y - decoration.layout.root.rect.y,
                        )),
                        (effect_rect.width, effect_rect.height).into(),
                    );
                    let sample_rect = smithay::utils::Rectangle::new(
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
                        &effect_config.effect,
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
                        TtyRenderElements::Window,
                        TtyRenderElements::TransformedWindow,
                    )
                    .into_iter(),
                );
                capture_scene_texture_for_effect(
                    renderer,
                    "tty-protocol-window-backdrop",
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
                capture_scene_texture_for_effect(
                    renderer,
                    "tty-protocol-window-xray",
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
                    local_rect = ?smithay::utils::Rectangle::<i32, Logical>::new(
                        smithay::utils::Point::from((
                            effect_rect.x - decoration.layout.root.rect.x,
                            effect_rect.y - decoration.layout.root.rect.y,
                        )),
                        (effect_rect.width, effect_rect.height).into(),
                    ),
                    sample_rect = ?smithay::utils::Rectangle::<i32, Logical>::new(
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
            let texture = crate::backend::shader_effect::apply_effect_pipeline_cached_for_key(
                renderer,
                format!(
                    "tty:protocol-window:{}:{}",
                    decoration.snapshot.id, cache_key
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
                    effect_rect.x - decoration.layout.root.rect.x,
                    effect_rect.y - decoration.layout.root.rect.y,
                )),
                (effect_rect.width, effect_rect.height).into(),
            );
            let sample_rect = smithay::utils::Rectangle::new(
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
                sample_rect,
                sample_rect,
                &effect_config.effect,
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
    let preliminary_physical_location =
        crate::backend::visual::logical_point_to_relative_physical_point_from_origin(
            window_location,
            output_origin,
            capture_origin_physical,
            scale,
        );
    let client_physical_geometry = window_decorations.get(window).and_then(|decoration| {
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
                smithay::utils::Rectangle::new(output_origin, (0, 0).into()),
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
        let root_origin =
            crate::backend::visual::logical_point_to_relative_physical_point_from_origin(
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
        if let Some(content_clip) = decoration.content_clip {
            // Surface elements are positioned in capture-local physical coordinates here.
            // The clipped-surface shader converts that geometry back to logical space via
            // `output_origin`, so use the capture rect origin rather than the real output
            // origin. Otherwise only the WindowSlot/client mask is evaluated in the wrong
            // coordinate space while SSD elements still appear correctly.
            let clipped = window_render::clipped_surface_elements(
                window,
                renderer,
                physical_location,
                client_physical_geometry,
                capture_geo.loc,
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
                    TtyRenderElements::Window,
                    TtyRenderElements::TransformedWindow,
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
                    TtyRenderElements::Window,
                    TtyRenderElements::TransformedWindow,
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
            TtyRenderElements::Window,
            TtyRenderElements::TransformedWindow,
        )
        .into_iter(),
    );

    Ok(elements)
}

fn capture_live_snapshot_for_window(
    renderer: &mut GlesRenderer,
    window: &smithay::desktop::Window,
    _window_location: smithay::utils::Point<i32, Logical>,
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
    live_window_snapshot_trackers: &mut std::collections::HashMap<
        String,
        smithay::backend::renderer::damage::OutputDamageTracker,
    >,
) -> Result<(), smithay::backend::renderer::gles::GlesError> {
    let Some((snapshot_id, client_rect)) = window_decorations
        .get(window)
        .map(|decoration| (decoration.snapshot.id.clone(), decoration.client_rect))
    else {
        return Ok(());
    };
    // The close snapshot texture is client-rect-local. `surface_elements`
    // expects the same client-slot location that live rendering uses and
    // subtracts `window.geometry().loc` internally to place the root surface.
    // Passing `window_location - client_rect.loc` here applies that geometry
    // offset twice for CSD/GTK windows, which shifts the frozen client image.
    let physical_location = smithay::utils::Point::<i32, smithay::utils::Physical>::from((0, 0));

    let surface_elements =
        window_render::surface_elements(window, renderer, physical_location, scale, 1.0);
    if surface_elements.is_empty() {
        return Ok(());
    }
    let has_client_content = !surface_elements.is_empty();
    let elements = surface_elements
        .into_iter()
        .map(TtyRenderElements::Window)
        .collect::<Vec<_>>();

    let existing = live_window_snapshots.remove(&snapshot_id);
    let live_tracker = live_window_snapshot_trackers
        .entry(snapshot_id.clone())
        .or_insert_with(|| {
            smithay::backend::renderer::damage::OutputDamageTracker::new(
                (0, 0),
                1.0,
                Transform::Normal,
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
        live_window_snapshots.insert(snapshot_id.clone(), snapshot);
    }

    Ok(())
}

pub(crate) fn capture_live_snapshot_for_close(
    state: &mut ShojiWM,
    window: &smithay::desktop::Window,
) -> Result<bool, smithay::backend::renderer::gles::GlesError> {
    let Some(window_location) = state.space.element_location(window) else {
        return Ok(false);
    };
    let Some(snapshot_id) = state
        .window_decorations
        .get(window)
        .map(|decoration| decoration.snapshot.id.clone())
    else {
        return Ok(false);
    };
    let preferred_output_name = state.primary_output_name_for_window(window);
    let output = preferred_output_name
        .as_ref()
        .and_then(|name| {
            state
                .space
                .outputs()
                .find(|output| output.name() == *name)
                .cloned()
        })
        .or_else(|| state.space.outputs_for_element(window).first().cloned())
        .or_else(|| state.space.outputs().next().cloned());
    let Some(output) = output else {
        return Ok(false);
    };
    let output_name = output.name();
    let scale = Scale::from(output.current_scale().fractional_scale());
    let backend_node = state
        .tty_backends
        .iter()
        .find_map(|(node, backend)| {
            backend
                .surfaces
                .values()
                .any(|surface| surface.output.name() == output_name)
                .then_some(*node)
        })
        .or_else(|| state.tty_backends.keys().next().copied());
    let Some(backend_node) = backend_node else {
        return Ok(false);
    };

    let ShojiWM {
        tty_backends,
        window_decorations,
        live_window_snapshots,
        live_window_snapshot_trackers,
        ..
    } = state;
    let Some(backend) = tty_backends.get_mut(&backend_node) else {
        return Ok(false);
    };
    capture_live_snapshot_for_window(
        &mut backend.renderer,
        window,
        window_location,
        scale,
        0,
        window_decorations,
        live_window_snapshots,
        live_window_snapshot_trackers,
    )?;

    Ok(live_window_snapshots.contains_key(&snapshot_id))
}

fn closing_snapshot_elements(
    renderer: &mut GlesRenderer,
    output: &Output,
    closing_snapshots: &[crate::backend::snapshot::ClosingWindowSnapshot],
    output_geo: smithay::utils::Rectangle<i32, Logical>,
    scale: smithay::utils::Scale<f64>,
) -> Vec<TtyRenderElements> {
    let close_debug = std::env::var_os("SHOJI_CLOSE_DEBUG").is_some();
    closing_snapshots
        .iter()
        .flat_map(|snapshot| {
            if close_debug {
                tracing::info!(
                    window_id = %snapshot.window_id,
                    live_rect = ?snapshot.live.rect,
                    root_rect = ?snapshot.decoration.layout.root.rect,
                    transform_scale_x = snapshot.transform.scale_x,
                    transform_scale_y = snapshot.transform.scale_y,
                    transform_opacity = snapshot.transform.opacity,
                    "close debug: rendering closing snapshot element"
                );
            }
            let visual = window_visual_state(
                snapshot.decoration.layout.root.rect,
                snapshot.transform,
                output_geo,
                scale,
            );
            let root_origin =
                root_physical_origin(snapshot.decoration.layout.root.rect, output_geo, scale);

            let root_surface_source_elements =
                closing_root_surface_source_elements(renderer, snapshot, output_geo, scale, visual);
            let full_window_source_elements = closing_full_window_source_elements(
                renderer,
                snapshot,
                output_geo,
                scale,
                root_origin,
                visual,
            );

            let replace_effects = if let Some(effect) = snapshot
                .decoration
                .window_effects
                .as_ref()
                .and_then(|effects| effects.replace.as_ref())
            {
                let (rect, source_elements): (_, &[TtyRenderElements]) = match &effect.effect.input
                {
                    EffectInput::WindowSource(WindowSourceInclude::Full) => (
                        transformed_root_rect(
                            snapshot.decoration.layout.root.rect,
                            snapshot.transform,
                        ),
                        &full_window_source_elements,
                    ),
                    EffectInput::WindowSource(WindowSourceInclude::RootSurface) => (
                        transformed_rect(
                            snapshot.decoration.client_rect,
                            snapshot.decoration.layout.root.rect,
                            snapshot.transform,
                        ),
                        &root_surface_source_elements,
                    ),
                    _ => (
                        transformed_root_rect(
                            snapshot.decoration.layout.root.rect,
                            snapshot.transform,
                        ),
                        &full_window_source_elements,
                    ),
                };
                window_effect_elements(
                    renderer,
                    output,
                    output_geo,
                    scale,
                    &snapshot.window_id,
                    "closing-replace",
                    smithay::backend::renderer::element::Id::new(),
                    Default::default(),
                    rect,
                    effect,
                    source_elements,
                )
                .inspect_err(|error| {
                    warn!(
                        window_id = %snapshot.window_id,
                        ?error,
                        "failed to build closing replacement window effect"
                    );
                })
                .unwrap_or_default()
            } else {
                Vec::new()
            };
            let replacement_source_elements = if replace_effects.is_empty() {
                None
            } else {
                Some(replace_effects.as_slice())
            };

            let in_front_effects = snapshot
                .decoration
                .window_effects
                .as_ref()
                .and_then(|effects| effects.in_front.as_ref())
                .and_then(|effect| {
                    let (rect, source_elements): (_, &[TtyRenderElements]) =
                        match &effect.effect.input {
                            EffectInput::WindowSource(WindowSourceInclude::Full) => (
                                transformed_root_rect(
                                    snapshot.decoration.layout.root.rect,
                                    snapshot.transform,
                                ),
                                &full_window_source_elements,
                            ),
                            EffectInput::WindowSource(WindowSourceInclude::RootSurface) => (
                                transformed_rect(
                                    snapshot.decoration.client_rect,
                                    snapshot.decoration.layout.root.rect,
                                    snapshot.transform,
                                ),
                                &root_surface_source_elements,
                            ),
                            _ => (
                                transformed_root_rect(
                                    snapshot.decoration.layout.root.rect,
                                    snapshot.transform,
                                ),
                                replacement_source_elements.unwrap_or(&full_window_source_elements),
                            ),
                        };
                    window_effect_elements(
                        renderer,
                        output,
                        output_geo,
                        scale,
                        &snapshot.window_id,
                        "closing-in-front",
                        smithay::backend::renderer::element::Id::new(),
                        Default::default(),
                        rect,
                        effect,
                        source_elements,
                    )
                    .inspect_err(|error| {
                        warn!(
                            window_id = %snapshot.window_id,
                            ?error,
                            "failed to build closing in-front window effect"
                        );
                    })
                    .ok()
                });
            let behind_root_surface_effects = snapshot
                .decoration
                .window_effects
                .as_ref()
                .and_then(|effects| effects.behind_root_surface.as_ref())
                .and_then(|effect| {
                    let (rect, source_elements): (_, &[TtyRenderElements]) =
                        match &effect.effect.input {
                            EffectInput::WindowSource(WindowSourceInclude::Full) => (
                                transformed_root_rect(
                                    snapshot.decoration.layout.root.rect,
                                    snapshot.transform,
                                ),
                                &full_window_source_elements,
                            ),
                            EffectInput::WindowSource(WindowSourceInclude::RootSurface) => (
                                transformed_rect(
                                    snapshot.decoration.client_rect,
                                    snapshot.decoration.layout.root.rect,
                                    snapshot.transform,
                                ),
                                &root_surface_source_elements,
                            ),
                            _ => (
                                transformed_root_rect(
                                    snapshot.decoration.layout.root.rect,
                                    snapshot.transform,
                                ),
                                &full_window_source_elements,
                            ),
                        };
                    window_effect_elements(
                        renderer,
                        output,
                        output_geo,
                        scale,
                        &snapshot.window_id,
                        "closing-behind-root-surface",
                        smithay::backend::renderer::element::Id::new(),
                        Default::default(),
                        rect,
                        effect,
                        source_elements,
                    )
                    .inspect_err(|error| {
                        warn!(
                            window_id = %snapshot.window_id,
                            ?error,
                            "failed to build closing root-surface window behind effect"
                        );
                    })
                    .ok()
                });
            let behind_effects = snapshot
                .decoration
                .window_effects
                .as_ref()
                .and_then(|effects| effects.behind.as_ref())
                .and_then(|effect| {
                    let (rect, source_elements): (_, &[TtyRenderElements]) =
                        match &effect.effect.input {
                            EffectInput::WindowSource(WindowSourceInclude::Full) => (
                                transformed_root_rect(
                                    snapshot.decoration.layout.root.rect,
                                    snapshot.transform,
                                ),
                                &full_window_source_elements,
                            ),
                            EffectInput::WindowSource(WindowSourceInclude::RootSurface) => (
                                transformed_rect(
                                    snapshot.decoration.client_rect,
                                    snapshot.decoration.layout.root.rect,
                                    snapshot.transform,
                                ),
                                &root_surface_source_elements,
                            ),
                            _ => (
                                transformed_root_rect(
                                    snapshot.decoration.layout.root.rect,
                                    snapshot.transform,
                                ),
                                replacement_source_elements.unwrap_or(&full_window_source_elements),
                            ),
                        };
                    window_effect_elements(
                        renderer,
                        output,
                        output_geo,
                        scale,
                        &snapshot.window_id,
                        "closing-behind",
                        smithay::backend::renderer::element::Id::new(),
                        Default::default(),
                        rect,
                        effect,
                        source_elements,
                    )
                    .inspect_err(|error| {
                        warn!(
                            window_id = %snapshot.window_id,
                            ?error,
                            "failed to build closing window behind effect"
                        );
                    })
                    .ok()
                });

            let mut elements = Vec::new();
            if let Some(in_front_effects) = in_front_effects {
                elements.extend(in_front_effects);
            }
            if replace_effects.is_empty() {
                elements.extend(full_window_source_elements);
            } else {
                elements.extend(replace_effects);
            }
            if let Some(behind_root_surface_effects) = behind_root_surface_effects {
                elements.extend(behind_root_surface_effects);
            }
            if let Some(behind_effects) = behind_effects {
                elements.extend(behind_effects);
            }
            elements
        })
        .collect()
}

fn closing_decoration_elements(
    renderer: &mut GlesRenderer,
    decoration: &crate::ssd::WindowDecorationState,
    output_geo: smithay::utils::Rectangle<i32, Logical>,
    scale: smithay::utils::Scale<f64>,
    root_origin: Point<i32, smithay::utils::Physical>,
    visual: WindowVisualState,
) -> Vec<TtyRenderElements> {
    let mut elements = Vec::new();
    // Render compositor-drawn decorations through the normal pipeline. The snapshot
    // texture contains only the client area (live_window_snapshot), so decorations
    // are always rendered separately here — same as the live loop.
    if let Ok(icon_elements) = crate::backend::icon::icon_elements_for_decoration(
        renderer,
        decoration,
        output_geo,
        scale,
        visual.opacity,
    ) {
        if let Ok(transformed) = transform_text_elements(icon_elements, root_origin, visual) {
            elements.extend(transformed);
        }
    }
    if let Ok(text_elements) = crate::backend::text::text_elements_for_decoration(
        renderer,
        decoration,
        output_geo,
        scale,
        visual.opacity,
    ) {
        if let Ok(transformed) = transform_text_elements(text_elements, root_origin, visual) {
            elements.extend(transformed);
        }
    }
    let mut decoration = decoration.clone();
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
    elements
}

fn closing_full_window_source_elements(
    renderer: &mut GlesRenderer,
    snapshot: &crate::backend::snapshot::ClosingWindowSnapshot,
    output_geo: smithay::utils::Rectangle<i32, Logical>,
    scale: smithay::utils::Scale<f64>,
    root_origin: Point<i32, smithay::utils::Physical>,
    visual: WindowVisualState,
) -> Vec<TtyRenderElements> {
    let mut elements = closing_decoration_elements(
        renderer,
        &snapshot.decoration,
        output_geo,
        scale,
        root_origin,
        visual,
    );
    elements.extend(closing_root_surface_source_elements(
        renderer, snapshot, output_geo, scale, visual,
    ));
    elements
}

fn closing_root_surface_source_elements(
    renderer: &GlesRenderer,
    snapshot: &crate::backend::snapshot::ClosingWindowSnapshot,
    output_geo: smithay::utils::Rectangle<i32, Logical>,
    scale: smithay::utils::Scale<f64>,
    visual: WindowVisualState,
) -> Vec<TtyRenderElements> {
    // Render the frozen client-area snapshot as the window content.
    if let Some(element) =
        snapshot::live_snapshot_element(renderer, &snapshot.live, output_geo, scale, visual.opacity)
        && let Ok(transformed) = transform_snapshot_elements(vec![element], visual)
    {
        return transformed;
    }
    Vec::new()
}

/// True for NVIDIA block-linear DRM format modifiers that request lossless framebuffer
/// compression.
///
/// Why we filter these out of the dmabuf *scanout* feedback tranche: on NVIDIA the kernel
/// advertises these compressed modifiers as plane-capable, so a client (e.g. a game) happily
/// allocates a compressed buffer and we hand it the "scanout" hint. But the subsequent atomic
/// commit that would put that buffer directly on the primary plane is *rejected* by the driver,
/// so we silently fall back to compositing it through GL. With a tearing/high-FPS client the
/// allocator does not pick the same modifier every frame, so the buffer keeps flipping between
/// "directly scannable" and "needs GL fallback". Direct Scanout therefore engages and disengages
/// every few frames, which both tanks performance and — because the fast path and the fallback
/// path have visibly different latency/cadence — makes the pointer/view feel stuttery and
/// unresponsive. Never advertising the compressed modifiers for scanout keeps the client on a
/// modifier we can actually flip, so Direct Scanout stays engaged steadily.
///
/// Compression is encoded in bits 25:23 of the modifier; the vendor is the top byte.
fn is_nvidia_compressed_modifier(modifier: smithay::backend::allocator::Modifier) -> bool {
    const NVIDIA_VENDOR: u64 = 0x03;
    const BLOCK_LINEAR_2D: u64 = 0x10;
    const COMPRESSION_MASK: u64 = 0x7 << 23;

    let modifier = u64::from(modifier);
    modifier >> 56 == NVIDIA_VENDOR
        && modifier & BLOCK_LINEAR_2D != 0
        && modifier & COMPRESSION_MASK != 0
}

fn surface_dmabuf_feedback(
    drm_output: &GbmDrmOutput,
    render_formats: FormatSet,
    scanout_node: DrmNode,
) -> Result<SurfaceDmabufFeedback, Box<dyn std::error::Error>> {
    let scanout_formats = render_formats
        .iter()
        .filter(|format| !is_nvidia_compressed_modifier(format.modifier))
        .copied()
        .collect::<FormatSet>();
    let (primary_formats, primary_or_overlay_formats) = drm_output.with_compositor(|compositor| {
        let drm_surface = compositor.surface();
        let primary_formats = drm_surface.plane_info().formats.clone();
        let primary_or_overlay_formats = primary_formats
            .iter()
            .chain(
                drm_surface
                    .planes()
                    .overlay
                    .iter()
                    .flat_map(|plane| plane.formats.iter()),
            )
            .copied()
            .collect::<FormatSet>();
        (primary_formats, primary_or_overlay_formats)
    });

    let primary_scanout_formats = primary_formats
        .intersection(&scanout_formats)
        .copied()
        .collect::<Vec<_>>();
    let primary_or_overlay_scanout_formats = primary_or_overlay_formats
        .intersection(&scanout_formats)
        .copied()
        .collect::<Vec<_>>();

    info!(
        ?scanout_node,
        render_feedback_format_count = render_formats.iter().count(),
        scanout_feedback_format_count = scanout_formats.iter().count(),
        filtered_nvidia_compressed_formats =
            render_formats.iter().count() - scanout_formats.iter().count(),
        primary_scanout_format_count = primary_scanout_formats.len(),
        primary_or_overlay_scanout_format_count = primary_or_overlay_scanout_formats.len(),
        "built tty output dmabuf scanout feedback"
    );

    let render = DmabufFeedbackBuilder::new(scanout_node.dev_id(), render_formats).build()?;
    let scanout = DmabufFeedbackBuilder::new(scanout_node.dev_id(), scanout_formats)
        .add_preference_tranche(
            scanout_node.dev_id(),
            Some(TrancheFlags::Scanout),
            primary_scanout_formats,
        )
        .add_preference_tranche(
            scanout_node.dev_id(),
            Some(TrancheFlags::Scanout),
            primary_or_overlay_scanout_formats,
        )
        .build()?;

    Ok(SurfaceDmabufFeedback { render, scanout })
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
    state.seed_xwayland_refresh_override_from_output(&output, "tty-output-connected");
    state.create_output_global(&output);
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
    let dmabuf_feedback =
        surface_dmabuf_feedback(&drm_output, backend.renderer.dmabuf_formats(), node)?;

    let supports_async_flip = drm_output.supports_async_page_flip();
    info!(
        ?node,
        ?crtc,
        output = %output.name(),
        supports_async_flip,
        "tty surface async page-flip (tearing) capability"
    );
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
        commit_timing_timer_armed: false,
        commit_timing_timer_generation: 0,
        frame_callback_sequence: 0,
        redraw_state: TtyRedrawState::Idle,
        frame_duration,
        next_frame_target: None,
        estimated_render_duration: Duration::from_millis(4),
        last_presented_at: None,
        last_frame_callback_at: None,
        supports_async_flip,
        tearing_active: false,
        dmabuf_feedback,
    };
    backend.surfaces.insert(crtc, surface);
    debug!(?node, ?crtc, "stored tty surface");
    if let Some(surface) = backend.surfaces.get_mut(&crtc) {
        surface.redraw_state = TtyRedrawState::Queued;
    }
    state.apply_runtime_display_configuration();
    state.notify_runtime_outputs_changed();
    state.schedule_redraw();
    Ok(())
}

fn connector_disconnected(
    state: &mut ShojiWM,
    node: DrmNode,
    crtc: crtc::Handle,
    connector: connector::Info,
) {
    let output_name = format!(
        "{}-{}",
        connector.interface().as_str(),
        connector.interface_id()
    );
    let Some(backend) = state.tty_backends.get_mut(&node) else {
        return;
    };
    let Some(surface) = backend.surfaces.remove(&crtc) else {
        return;
    };
    let output = surface.output;
    state.space.unmap_output(&output);
    state.remove_output_global(&output);
    state.screencopy_state.remove_output(&output);
    state.output_capture_mirrors.remove(&output_name);
    // Output configuration belongs to the connector name, not to this short-lived Output
    // instance. Keep it across hot-unplug so a reconnected connector receives its mode, scale,
    // and position immediately. The TS runtime suppresses unchanged configuration payloads, so
    // deleting the Rust-side entry here would otherwise leave the new Output at scale 1.
    state.runtime_animation_outputs.remove(&output_name);
    state.damage_blink_visible.remove(&output_name);
    state.damage_blink_pending.remove(&output_name);
    state.damage_blink_capture_suppression.remove(&output_name);
    state.pending_decoration_damage.clear();
    state.apply_runtime_display_configuration();
    state.notify_runtime_outputs_changed();
    info!(
        ?node,
        ?crtc,
        output = %output_name,
        "disconnected tty output"
    );
}

pub fn device_changed(
    state: &mut ShojiWM,
    node: DrmNode,
) -> Result<(), Box<dyn std::error::Error>> {
    let scan_result = {
        let Some(backend) = state.tty_backends.get_mut(&node) else {
            return Ok(());
        };
        backend
            .drm_scanner
            .scan_connectors(backend.drm_output_manager.device())?
    };

    let mut changed = false;
    for scan in scan_result {
        debug!(?node, ?scan, "connector scan event");
        match scan {
            DrmScanEvent::Connected {
                connector,
                crtc: Some(crtc),
            } => {
                connector_connected(state, node, crtc, connector)?;
                changed = true;
            }
            DrmScanEvent::Disconnected {
                connector,
                crtc: Some(crtc),
            } => {
                connector_disconnected(state, node, crtc, connector);
                changed = true;
            }
            DrmScanEvent::Changed {
                connector,
                crtc: Some(crtc),
            } => {
                if let Some(surface) = state
                    .tty_backends
                    .get_mut(&node)
                    .and_then(|backend| backend.surfaces.get_mut(&crtc))
                {
                    surface.available_modes = connector.modes().to_vec();
                }
                state.apply_runtime_display_configuration();
                changed = true;
            }
            _ => {}
        }
    }

    if changed {
        state.notify_runtime_outputs_changed();
    }
    Ok(())
}

/// Rebuild one CRTC's DRM surface after the kernel rejected its screen
/// configuration (`DrmError::TestFailed` from a render/queue commit). This is
/// the expected shape of the first post-resume frame when the connector
/// topology changed during sleep: the stale surface carries state the new
/// topology cannot satisfy, so tear it down and reconnect the output through
/// the normal hotplug path instead of exiting the session.
///
/// Returns `Err` only when the same CRTC keeps failing in a tight window —
/// the last-resort fatal path so a configuration the kernel
/// always rejects cannot spin the render loop forever.
fn reset_surface_after_commit_failure(
    state: &mut ShojiWM,
    node: DrmNode,
    crtc: crtc::Handle,
) -> Result<(), Box<dyn std::error::Error>> {
    const MAX_RESETS_PER_WINDOW: u32 = 3;
    const RESET_WINDOW: Duration = Duration::from_secs(10);

    let Some(backend) = state.tty_backends.get_mut(&node) else {
        return Ok(());
    };
    let now = Instant::now();
    let entry = backend
        .surface_reset_attempts
        .entry(
            crtc,
        )
        .or_insert(
            (
                now, 
                0,
            ),
        );
    if now.duration_since(
        entry.0,
    ) > RESET_WINDOW {
        *entry = (
            now,
            0,
        );
    }
    entry.1 += 1;
    if entry.1 > MAX_RESETS_PER_WINDOW {
        return Err(format!(
            "giving up on crtc {crtc:?}: {MAX_RESETS_PER_WINDOW} surface resets within \
             {RESET_WINDOW:?} did not produce a working configuration"
        )
        .into());
    }

    let connector = backend
        .drm_scanner
        .crtcs()
        .find(|(_, scanned_crtc)| *scanned_crtc == crtc)
        .map(|(info, _)| info.clone());
    let Some(
        connector,
    ) = connector else {
        // The scanner no longer maps a connector to this CRTC — the surface
        // is left over from a change that never fully applied. A rescan emits
        // the missing disconnect/connect events through the hotplug path.
        warn!(
            ?node,
            ?crtc,
            "no connector mapped to failing crtc; rescanning device"
        );
        return device_changed(
            state,
            node,
        );
    };

    info!(
        ?node,
        ?crtc,
        connector = %format!(
            "{}-{}",
            connector.interface().as_str(),
            connector.interface_id()
        ),
        "resetting tty surface after failed atomic commit test"
    );
    connector_disconnected(
        state,
        node,
        crtc,
        connector
            .clone(),
    );
    if let Err(
        err,
    ) = connector_connected(
        state,
        node,
        crtc,
        connector,
    ) {
        // Leave this output disconnected rather than killing the session:
        // other outputs keep working, and a later hotplug event retries.
        warn!(
            ?node,
            ?crtc,
            ?err,
            "failed to reconnect output after surface reset",
        );
    }
    state.force_full_damage = true;
    state
        .schedule_redraw();
    Ok(())
}

pub fn device_removed(state: &mut ShojiWM, node: DrmNode) {
    let crtcs = {
        let Some(backend) = state.tty_backends.get_mut(&node) else {
            return;
        };
        backend
            .drm_scanner
            .crtcs()
            .map(|(connector, crtc)| (connector.clone(), crtc))
            .collect::<Vec<_>>()
    };

    for (connector, crtc) in crtcs {
        connector_disconnected(state, node, crtc, connector);
    }

    state.tty_backends.remove(&node);
    state.notify_runtime_outputs_changed();
    info!(?node, "removed tty drm device");
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

pub fn tty_connected_outputs(state: &crate::state::ShojiWM) -> Vec<Output> {
    let mut outputs = Vec::new();
    for backend in state.tty_backends.values() {
        for surface in backend.surfaces.values() {
            outputs.push(surface.output.clone());
        }
    }
    outputs
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
        // Rank the connector's PREFERRED *resolution* (the panel's native
        // timing) above raw pixel area: kernel `video=` parameters inject
        // synthetic modes into every connector, and on panels like the UX482
        // ScreenPad (native 1920x515) a synthetic 1920x1080 would otherwise
        // win and drive the panel at a timing it cannot display. Compare
        // sizes only, not the PREFERRED flag itself: the flag typically
        // marks the native resolution at 60Hz, and ranking by the flag would
        // pin such panels to 60Hz even when higher-refresh modes at the same
        // resolution exist. The trailing refresh keys pick the fastest one.
        DisplayModePreference::Auto => {
            let preferred_size = connector
                .modes()
                .iter()
                .find(|mode| mode.mode_type().contains(ModeTypeFlags::PREFERRED))
                .map(|mode| mode.size());
            connector
                .modes()
                .iter()
                .copied()
                .max_by_key(|mode| {
                    let wl_mode = WlMode::from(*mode);
                    (
                        Some(mode.size()) == preferred_size,
                        i64::from(wl_mode.size.w) * i64::from(wl_mode.size.h),
                        mode.vrefresh(),
                        wl_mode.refresh,
                    )
                })
                .unwrap_or(connector.modes()[0])
        }
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
        if animation_gap_debug_enabled() {
            info!(
                output = %surface.output.name(),
                generation,
                frame_duration_ms = surface.frame_duration.as_secs_f64() * 1000.0,
                "animation gap: tty estimated-vblank already armed"
            );
        }
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
            if animation_gap_debug_enabled() {
                info!(
                    output = %output.name(),
                    generation,
                    sequence,
                    should_redraw,
                    callback_time_ms = callback_time.as_secs_f64() * 1000.0,
                    "animation gap: tty estimated-vblank fired"
                );
            }
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
                if frame_liveness_debug_enabled() {
                    tracing::info!(
                        output = %output.name(),
                        queued = true,
                        generation,
                        sequence,
                        callback_time = ?callback_time,
                        "tty frame liveness: estimated vblank queued redraw",
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
                if frame_liveness_debug_enabled() {
                    tracing::info!(
                        output = %output.name(),
                        queued = false,
                        generation,
                        sequence,
                        callback_time = ?callback_time,
                        "tty frame liveness: estimated vblank sending primary-only callbacks",
                    );
                }
                state.send_primary_frame_callbacks_for_output(
                    &output,
                    callback_time,
                    Some(sequence),
                );
                let _ = state.display_handle.flush_clients();
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

fn schedule_commit_timing_timer(
    loop_handle: &LoopHandle<'_, ShojiWM>,
    state: &mut ShojiWM,
    node: DrmNode,
    crtc: crtc::Handle,
) {
    let Some(output) = state
        .tty_backends
        .get(&node)
        .and_then(|backend| backend.surfaces.get(&crtc))
        .map(|surface| surface.output.clone())
    else {
        return;
    };
    let Some(next_deadline) = state.next_commit_timing_deadline_for_output(&output) else {
        return;
    };
    let Some(backend) = state.tty_backends.get_mut(&node) else {
        return;
    };
    let Some(surface) = backend.surfaces.get_mut(&crtc) else {
        return;
    };

    if surface.frame_pending
        || matches!(
            surface.redraw_state,
            TtyRedrawState::Queued | TtyRedrawState::WaitingForVBlank { .. }
        )
    {
        return;
    }
    if surface.commit_timing_timer_armed {
        return;
    }

    let now = Duration::from(state.clock.now());
    let delay = next_deadline.saturating_sub(now);
    let generation = surface.commit_timing_timer_generation.wrapping_add(1);
    surface.commit_timing_timer_generation = generation;
    surface.commit_timing_timer_armed = true;

    let insert_loop_handle = loop_handle.clone();
    let callback_loop_handle = loop_handle.clone();
    if mpv_frame_debug_enabled() && output_has_visible_mpv(state, &output) {
        info!(
            output = %output.name(),
            generation,
            now_ms = now.as_secs_f64() * 1000.0,
            next_deadline_ms = next_deadline.as_secs_f64() * 1000.0,
            delay_ms = delay.as_secs_f64() * 1000.0,
            "mpv frame debug: commit timing timer armed"
        );
    }

    if insert_loop_handle
        .insert_source(Timer::from_duration(delay), move |_, _, state| {
            let timer_is_current = {
                let Some(backend) = state.tty_backends.get_mut(&node) else {
                    return TimeoutAction::Drop;
                };
                let Some(surface) = backend.surfaces.get_mut(&crtc) else {
                    return TimeoutAction::Drop;
                };
                if !surface.commit_timing_timer_armed
                    || surface.commit_timing_timer_generation != generation
                {
                    return TimeoutAction::Drop;
                }
                surface.commit_timing_timer_armed = false;
                true
            };
            if !timer_is_current {
                return TimeoutAction::Drop;
            }

            let now = Duration::from(state.clock.now());
            let signaled = state.signal_commit_timing_barriers_for_output(&output, now.into());
            let _ = state.display_handle.flush_clients();

            if mpv_frame_debug_enabled() && output_has_visible_mpv(state, &output) {
                info!(
                    output = %output.name(),
                    generation,
                    now_ms = now.as_secs_f64() * 1000.0,
                    requested_deadline_ms = next_deadline.as_secs_f64() * 1000.0,
                    signaled,
                    "mpv frame debug: commit timing timer fired"
                );
            }

            schedule_commit_timing_timer(&callback_loop_handle, state, node, crtc);
            TimeoutAction::Drop
        })
        .is_err()
    {
        if let Some(surface) = state
            .tty_backends
            .get_mut(&node)
            .and_then(|backend| backend.surfaces.get_mut(&crtc))
        {
            surface.commit_timing_timer_armed = false;
        }
        warn!(?node, ?crtc, "failed to schedule tty commit timing timer");
    }
}

/// Re-arm the commit-timing timers on every tty output.
///
/// `schedule_commit_timing_timer` otherwise only runs from `frame_finish` when an
/// output goes idle — but a commit-timing client (Mesa's Vulkan WSI paces every
/// frame through wp_commit_timer timestamps) submits its next timestamped commit a
/// few milliseconds *after* that check, once the presentation feedback sent by
/// that same `frame_finish` reaches it. The commit then sits blocked on its
/// timestamp barrier with no timer armed, and since the blocked commit holds the
/// only pending damage, no render ever runs `pre_repaint` to signal it: the
/// client wedges until an unrelated redraw (cursor motion, a shell clock tick)
/// happens to repaint. Calling this whenever a timestamped commit arrives keeps
/// the deadline covered; `schedule_commit_timing_timer` is a no-op for outputs
/// that are rendering or already have a timer armed.
pub fn arm_commit_timing_timers(state: &mut ShojiWM, loop_handle: &LoopHandle<'_, ShojiWM>) {
    let targets: Vec<(DrmNode, crtc::Handle)> = state
        .tty_backends
        .iter()
        .flat_map(|(node, backend)| backend.surfaces.keys().map(move |crtc| (*node, *crtc)))
        .collect();
    for (node, crtc) in targets {
        schedule_commit_timing_timer(loop_handle, state, node, crtc);
    }
}

fn blend_render_duration(previous: Duration, current: Duration) -> Duration {
    if previous.is_zero() {
        return current;
    }

    Duration::from_secs_f64(previous.as_secs_f64() * 0.75 + current.as_secs_f64() * 0.25)
}
