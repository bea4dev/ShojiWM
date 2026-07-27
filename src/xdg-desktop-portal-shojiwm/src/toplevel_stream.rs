//! Toplevel (single-window) streaming via `ext-image-copy-capture-v1`.
//!
//! Sibling to [`crate::pipewire_stream`], which streams whole outputs through
//! `zwlr-screencopy-unstable-v1`. Same architecture
//! (DRIVER + ALLOC_BUFFERS + wayland-driven queue + single-thread integration —
//! see that module for the long rationale on each piece), with three protocol
//! differences:
//!
//!   1. The capture source is created via
//!      `ext_foreign_toplevel_image_capture_source_manager_v1::create_source`
//!      pointed at an `ExtForeignToplevelHandleV1` (looked up by identifier).
//!   2. A long-lived `ExtImageCopyCaptureSessionV1` advertises
//!      `buffer_size` / `shm_format` / `done` once at startup, and *those*
//!      values pick the PipeWire stream dims/format rather than the picker
//!      hint. (For outputs the picker already knows the right dims; for a
//!      toplevel the compositor is the only authoritative source.)
//!   3. Per frame: `dequeue_raw_buffer → session.create_frame →
//!      frame.attach_buffer → frame.capture → ready → queue_raw_buffer →
//!      next dequeue`. The "no buffer_done event" sequence is simpler than
//!      wlr-screencopy's two-step buffer / buffer_done dance.

use std::cell::RefCell;
use std::collections::HashMap;
use std::io::Cursor;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd, RawFd};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread;

use pipewire as pw;
use pw::spa;
use pw::spa::pod::serialize::PodSerializer;
use pw::spa::pod::{ChoiceValue, Object, Pod, Property, Value};
use pw::spa::support::system::IoFlags;
use pw::spa::utils::{Choice, ChoiceEnum, ChoiceFlags, Fraction, Id, Rectangle};
use spa::sys as spa_sys;

use crate::pipewire_stream::{
    build_header_meta_param,
    fill_header_meta,
    parse_pipewire_max_framerate,
};
use wayland_client::protocol::{wl_buffer, wl_registry, wl_shm, wl_shm_pool};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum};
use wayland_protocols::ext::foreign_toplevel_list::v1::client::{
    ext_foreign_toplevel_handle_v1::{self, ExtForeignToplevelHandleV1},
    ext_foreign_toplevel_list_v1::{self, ExtForeignToplevelListV1},
};
use wayland_protocols::ext::image_capture_source::v1::client::{
    ext_foreign_toplevel_image_capture_source_manager_v1::ExtForeignToplevelImageCaptureSourceManagerV1,
    ext_image_capture_source_v1::ExtImageCaptureSourceV1,
};
use wayland_protocols::ext::image_copy_capture::v1::client::{
    ext_image_copy_capture_frame_v1::{self, ExtImageCopyCaptureFrameV1},
    ext_image_copy_capture_manager_v1::{self, ExtImageCopyCaptureManagerV1},
    ext_image_copy_capture_session_v1::{self, ExtImageCopyCaptureSessionV1},
};

#[derive(Debug, Clone)]
pub struct StreamSpec {
    pub toplevel_identifier: String,
    pub framerate: u32,
    /// Whether to advertise `paint_cursors` on the session. Honoured by the
    /// compositor only if it composites cursor for per-toplevel captures
    /// (which we don't yet — see `image_copy_capture_render.rs`).
    pub cursor_visible: bool,
}

pub struct StreamHandle {
    stop: Arc<AtomicBool>,
    join: Option<thread::JoinHandle<()>>,
}

impl Drop for StreamHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

pub fn start(
    spec: StreamSpec,
) -> Result<(u32, StreamHandle), Box<dyn std::error::Error + Send + Sync>> {
    let stop = Arc::new(AtomicBool::new(false));
    let stop_for_thread = stop.clone();
    let (tx, rx) = mpsc::sync_channel::<Result<u32, String>>(1);
    let join = thread::Builder::new()
        .name("portal-toplevel-stream".into())
        .spawn(move || {
            if let Err(e) = run(spec, tx, stop_for_thread) {
                tracing::error!("toplevel stream thread exited: {e}");
            }
        })?;
    let node_id = rx
        .recv()
        .map_err(|_| "toplevel stream thread died before reporting node id".to_string())?
        .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { e.into() })?;
    tracing::info!(node_id, "toplevel stream live");
    Ok((
        node_id,
        StreamHandle {
            stop,
            join: Some(join),
        },
    ))
}

// ─── Shared state ────────────────────────────────────────────────────────

struct AppState {
    spec: StreamSpec,

    // Wayland resources
    conn: Option<Connection>,
    qh: QueueHandle<AppState>,
    shm: Option<wl_shm::WlShm>,
    capture_manager: Option<ExtImageCopyCaptureManagerV1>,
    toplevel_source_manager: Option<ExtForeignToplevelImageCaptureSourceManagerV1>,

    // Toplevel discovery (filled during bootstrap roundtrips).
    toplevels: Vec<DiscoveredToplevel>,
    toplevel_index: HashMap<u32, usize>,
    target_toplevel: Option<ExtForeignToplevelHandleV1>,

    // Session + source (created after toplevel found).
    source: Option<ExtImageCaptureSourceV1>,
    session: Option<ExtImageCopyCaptureSessionV1>,

    // Session-advertised buffer constraints.
    adv_width: u32,
    adv_height: u32,
    adv_format: Option<wl_shm::Format>,
    adv_done: bool,
    session_stopped: bool,

    // PipeWire stream (cloneable Rc handle).
    stream: Option<pw::stream::StreamRc>,
    /// pw_buffer raw ptr → its wrapping wl_buffer + owned memfd.
    pw_buffer_slots: HashMap<usize, BufferSlot>,
    /// pw_buffer raw ptr → its negotiated stride (cached at add_buffer time).
    pw_buffer_stride: HashMap<usize, i32>,

    // Per-frame in-flight state.
    pending_frame: Option<PendingFrame>,

    // node-id handoff (taken on first Paused transition).
    node_id_tx: Option<mpsc::SyncSender<Result<u32, String>>>,

    // Diagnostics.
    frames_completed: u64,
    last_log_at: std::time::Instant,
    /// Whether the PW stream is currently in Streaming state. Gates capture
    /// kicks from `on_add_buffer` so we don't capture into a paused stream.
    is_streaming: bool,
    /// True from a consumer-initiated Format renegotiation until the
    /// replacement buffer pool starts arriving — same quiesce as the output
    /// path, so we never push frames into a stream whose consumer is mid-way
    /// through a buffer-pool rebuild.
    renegotiating: bool,
    /// Frame interval from the negotiated `maxFramerate`; frames are queued
    /// no faster than this (capture stays event-paced, early frames are held
    /// in `spare_buffer` and recaptured).
    frame_interval: Option<std::time::Duration>,
    last_queue_at: Option<std::time::Instant>,
    /// A dequeued-but-unqueued pw_buffer held back by the framerate
    /// throttle; the next kick reuses it instead of dequeuing another slot.
    spare_buffer: Option<usize>,
    /// Monotonic per-stream frame counter for `spa_meta_header.seq`.
    frame_sequence: u64,

    // Set when the PW stream transitions to Error/Unconnected (consumer
    // disconnected, etc.). Every callback short-circuits afterwards so we
    // never touch a torn-down buffer / stream — which is what produced the
    // SEGV when OBS quit.
    dying: bool,
    stop_flag: Option<Arc<AtomicBool>>,

    // Renegotiation: set in the session BufferSize handler when constraints
    // change AFTER the initial Done (e.g. compositor saw the toplevel resize
    // and called `update_constraints`). The io callback picks this up after
    // dispatch_pending and calls `pw_stream_update_params` so PipeWire
    // reallocates buffers at the new size.
    needs_renegotiate: bool,
    negotiated_width: u32,
    negotiated_height: u32,
}

struct DiscoveredToplevel {
    proxy: ExtForeignToplevelHandleV1,
    identifier: String,
}

struct PendingFrame {
    frame: ExtImageCopyCaptureFrameV1,
    pw_buffer: usize,
}

struct BufferSlot {
    wl_buffer: wl_buffer::WlBuffer,
    _shm_pool: wl_shm_pool::WlShmPool,
    _fd: OwnedFd,
    /// The fd handed to PipeWire in `spa_data.fd`. libpipewire does not take
    /// ownership of fds the client fills in under ALLOC_BUFFERS, so it must
    /// stay owned here and drop with the slot — otherwise every buffer-pool
    /// rebuild leaks one fd per buffer.
    _pw_fd: OwnedFd,
}

unsafe impl Send for AppState {}

struct FdHolder(BorrowedFd<'static>);
impl AsRawFd for FdHolder {
    fn as_raw_fd(&self) -> RawFd {
        self.0.as_raw_fd()
    }
}

// ─── Main thread entry ────────────────────────────────────────────────────

fn run(
    spec: StreamSpec,
    node_id_tx: mpsc::SyncSender<Result<u32, String>>,
    stop: Arc<AtomicBool>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&mainloop, None)?;
    let core = context.connect_rc(None)?;

    let conn = Connection::connect_to_env()?;
    let mut event_queue: EventQueue<AppState> = conn.new_event_queue();
    let qh = event_queue.handle();
    let _registry = conn.display().get_registry(&qh, ());

    let mut state = AppState {
        spec: spec.clone(),
        conn: Some(conn.clone()),
        qh: qh.clone(),
        shm: None,
        capture_manager: None,
        toplevel_source_manager: None,
        toplevels: Vec::new(),
        toplevel_index: HashMap::new(),
        target_toplevel: None,
        source: None,
        session: None,
        adv_width: 0,
        adv_height: 0,
        adv_format: None,
        adv_done: false,
        session_stopped: false,
        stream: None,
        pw_buffer_slots: HashMap::new(),
        pw_buffer_stride: HashMap::new(),
        pending_frame: None,
        node_id_tx: Some(node_id_tx),
        frames_completed: 0,
        last_log_at: std::time::Instant::now(),
        is_streaming: false,
        renegotiating: false,
        frame_interval: None,
        last_queue_at: None,
        spare_buffer: None,
        frame_sequence: 0,
        dying: false,
        stop_flag: Some(stop.clone()),
        needs_renegotiate: false,
        negotiated_width: 0,
        negotiated_height: 0,
    };

    // Bind globals (round 1), receive toplevel list events (round 2 + 3).
    for _ in 0..3 {
        event_queue.roundtrip(&mut state)?;
    }

    if state.capture_manager.is_none() {
        let err = "compositor doesn't expose ext_image_copy_capture_manager_v1";
        let _ = state.node_id_tx.take().map(|tx| tx.send(Err(err.into())));
        return Err(err.into());
    }
    if state.toplevel_source_manager.is_none() {
        let err = "compositor doesn't expose ext_foreign_toplevel_image_capture_source_manager_v1";
        let _ = state.node_id_tx.take().map(|tx| tx.send(Err(err.into())));
        return Err(err.into());
    }
    if state.shm.is_none() {
        let err = "compositor doesn't expose wl_shm";
        let _ = state.node_id_tx.take().map(|tx| tx.send(Err(err.into())));
        return Err(err.into());
    }
    state.target_toplevel = state
        .toplevels
        .iter()
        .find(|t| t.identifier == spec.toplevel_identifier)
        .map(|t| t.proxy.clone());
    if state.target_toplevel.is_none() {
        let err = format!(
            "toplevel with identifier {:?} not found",
            spec.toplevel_identifier
        );
        let _ = state.node_id_tx.take().map(|tx| tx.send(Err(err.clone())));
        return Err(err.into());
    }

    // Create source + session, then wait for the session to advertise its
    // constraints. The PipeWire stream depends on those dims/format.
    {
        let manager = state.toplevel_source_manager.clone().unwrap();
        let capture = state.capture_manager.clone().unwrap();
        let toplevel = state.target_toplevel.clone().unwrap();
        let source = manager.create_source(&toplevel, &qh, ());
        let options = if spec.cursor_visible {
            ext_image_copy_capture_manager_v1::Options::PaintCursors
        } else {
            ext_image_copy_capture_manager_v1::Options::empty()
        };
        let session = capture.create_session(&source, options, &qh, ());
        state.source = Some(source);
        state.session = Some(session);
    }
    conn.flush()?;
    for _ in 0..6 {
        event_queue.roundtrip(&mut state)?;
        if state.adv_done || state.session_stopped {
            break;
        }
    }
    if state.session_stopped {
        let err = "session stopped before advertising constraints";
        let _ = state.node_id_tx.take().map(|tx| tx.send(Err(err.into())));
        return Err(err.into());
    }
    if !state.adv_done || state.adv_width == 0 || state.adv_height == 0 {
        let err = "session never finalized buffer constraints";
        let _ = state.node_id_tx.take().map(|tx| tx.send(Err(err.into())));
        return Err(err.into());
    }
    if !matches!(state.adv_format, Some(wl_shm::Format::Xrgb8888)) {
        tracing::warn!(
            advertised = ?state.adv_format,
            "toplevel session didn't advertise Xrgb8888; forcing it anyway"
        );
        state.adv_format = Some(wl_shm::Format::Xrgb8888);
    }
    tracing::info!(
        width = state.adv_width,
        height = state.adv_height,
        format = ?state.adv_format,
        "toplevel session constraints negotiated"
    );
    state.negotiated_width = state.adv_width;
    state.negotiated_height = state.adv_height;

    // Build PipeWire stream with the negotiated dims.
    let stream = pw::stream::StreamRc::new(
        core,
        "shojiwm-toplevel-screencast",
        pw::properties::properties! {
            *pw::keys::MEDIA_CLASS => "Video/Source",
            *pw::keys::MEDIA_ROLE => "Screen",
            *pw::keys::NODE_NAME => "shojiwm-portal-toplevel-stream",
            *pw::keys::NODE_DESCRIPTION => "ShojiWM portal toplevel screencast",
        },
    )?;
    state.stream = Some(stream.clone());

    let state_rc = Rc::new(RefCell::new(state));

    let s_state = state_rc.clone();
    let s_add = state_rc.clone();
    let s_remove = state_rc.clone();
    let s_param = state_rc
        .clone();
    let _listener = stream
        .add_local_listener_with_user_data(())
        .state_changed(move |stream, _ud, old, new| {
            s_state.borrow_mut().on_state_changed(stream, old, new);
        })
        .param_changed(move |stream, _ud, id, param| {
            s_param
                .borrow_mut()
                .on_param_changed(
                    stream,
                    id,
                    param,
                );
        })
        .add_buffer(move |stream, _ud, buffer| {
            s_add.borrow_mut().on_add_buffer(stream, buffer);
        })
        .remove_buffer(move |stream, _ud, buffer| {
            s_remove.borrow_mut().on_remove_buffer(stream, buffer);
        })
        .process(|_, _| {
            // No-op: cycle is driven by queue_buffer from on_frame_ready.
        })
        .register()?;

    let (adv_width, adv_height) = {
        let s = state_rc.borrow();
        (s.adv_width, s.adv_height)
    };
    let format_bytes = build_video_format_param(adv_width, adv_height, spec.framerate)?;
    let buffers_bytes = build_buffers_param(adv_width, adv_height)?;
    let meta_bytes = build_header_meta_param()?;
    let format_pod = Pod::from_bytes(&format_bytes).ok_or("format POD parse failed".to_string())?;
    let buffers_pod =
        Pod::from_bytes(&buffers_bytes).ok_or("buffers POD parse failed".to_string())?;
    let meta_pod = Pod::from_bytes(
        &meta_bytes,
    ).ok_or(
        "meta POD parse failed"
            .to_string(),
    )?;
    let mut params = [
        format_pod,
        buffers_pod,
        meta_pod,
    ];
    stream.connect(
        spa::utils::Direction::Output,
        None,
        pw::stream::StreamFlags::DRIVER | pw::stream::StreamFlags::ALLOC_BUFFERS,
        &mut params,
    )?;
    tracing::info!("toplevel screencast: PW stream connected (DRIVER | ALLOC_BUFFERS)");

    // Attach wayland fd to the PW main loop.
    let wl_fd = conn.as_fd().try_clone_to_owned()?;
    let wl_fd_static: BorrowedFd<'static> =
        unsafe { std::mem::transmute::<BorrowedFd<'_>, BorrowedFd<'static>>(wl_fd.as_fd()) };
    let fd_holder = FdHolder(wl_fd_static);

    let s_for_io = state_rc.clone();
    let conn_for_io = conn.clone();
    let event_queue_cell = RefCell::new(event_queue);
    let _io = mainloop.loop_().add_io(fd_holder, IoFlags::IN, move |_| {
        if let Some(guard) = conn_for_io.prepare_read() {
            let _ = guard.read();
        }
        let mut eq = event_queue_cell.borrow_mut();
        let mut state = s_for_io.borrow_mut();
        if let Err(e) = eq.dispatch_pending(&mut *state) {
            tracing::error!("wayland dispatch: {e}");
        }
        // Renegotiation: if a session BufferSize event flipped the flag,
        // push the new format / buffer params to PipeWire. PW reallocates
        // its buffers and our add_buffer/remove_buffer callbacks recreate
        // the wl_buffer wraps at the new dims.
        if state.needs_renegotiate {
            state.needs_renegotiate = false;
            let stream_clone = state.stream.clone();
            let framerate = state.spec.framerate;
            let new_w = state.adv_width;
            let new_h = state.adv_height;
            state.negotiated_width = new_w;
            state.negotiated_height = new_h;
            if let Some(pending) = state.pending_frame.take() {
                pending.frame.destroy();
            }
            if let Some(stream) = stream_clone {
                tracing::info!(
                    new_w,
                    new_h,
                    "toplevel: pushing renegotiated PW stream params"
                );
                match (
                    build_video_format_param(new_w, new_h, framerate),
                    build_buffers_param(new_w, new_h),
                    build_header_meta_param(),
                ) {
                    (
                        Ok(
                            fbytes,
                        ),
                        Ok(
                            bbytes,
                        ),
                        Ok(
                            mbytes,
                        ),
                    ) => {
                        if let (
                            Some(
                                fpod,
                            ),
                            Some(
                                bpod,
                            ),
                            Some(
                                mpod,
                            ),
                        ) = (
                            Pod::from_bytes(
                                &fbytes,
                            ),
                            Pod::from_bytes(
                                &bbytes,
                            ),
                            Pod::from_bytes(
                                &mbytes,
                            ),
                        ) {
                            let mut params = [
                                fpod,
                                bpod,
                                mpod,
                            ];
                            if let Err(e) = stream.update_params(&mut params) {
                                tracing::warn!("update_params failed: {e:?}");
                            }
                            // No latch reset needed: the capture cycle restarts
                            // from on_state_changed / on_add_buffer once the
                            // reallocated buffers land (pending_frame is None).
                        }
                    }
                    (
                        e1,
                        e2,
                        e3,
                    ) => {
                        tracing::warn!(
                            ?e1,
                            ?e2,
                            ?e3,
                            "build params failed during renegotiate",
                        );
                    }
                }
            }
        }
        let _ = conn_for_io.flush();
    });

    conn.flush()?;

    let mainloop_for_stop = mainloop.clone();
    let stop_for_event = stop.clone();
    let event_check = mainloop.loop_().add_timer(move |_| {
        if stop_for_event.load(Ordering::SeqCst) {
            mainloop_for_stop.quit();
        }
    });
    let _ = event_check.update_timer(
        Some(std::time::Duration::from_millis(200)),
        Some(std::time::Duration::from_millis(200)),
    );

    mainloop.run();

    let mut state = state_rc.borrow_mut();
    state.pw_buffer_slots.clear();
    state.pending_frame = None;
    if let Some(s) = state.session.take() {
        s.destroy();
    }
    if let Some(s) = state.source.take() {
        s.destroy();
    }
    tracing::info!("toplevel screencast thread exiting cleanly");
    Ok(())
}

// ─── Wayland Dispatch impls ──────────────────────────────────────────────

impl Dispatch<wl_registry::WlRegistry, ()> for AppState {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        else {
            return;
        };
        match interface.as_str() {
            "wl_shm" => {
                state.shm =
                    Some(registry.bind::<wl_shm::WlShm, _, _>(name, version.min(1), qh, ()));
            }
            "ext_image_copy_capture_manager_v1" => {
                state.capture_manager = Some(registry.bind::<ExtImageCopyCaptureManagerV1, _, _>(
                    name,
                    version.min(1),
                    qh,
                    (),
                ));
            }
            "ext_foreign_toplevel_image_capture_source_manager_v1" => {
                state.toplevel_source_manager = Some(
                    registry.bind::<ExtForeignToplevelImageCaptureSourceManagerV1, _, _>(
                        name,
                        version.min(1),
                        qh,
                        (),
                    ),
                );
            }
            "ext_foreign_toplevel_list_v1" => {
                registry.bind::<ExtForeignToplevelListV1, _, _>(name, version.min(1), qh, ());
            }
            _ => {}
        }
    }
}

macro_rules! empty_dispatch {
    ($t:ty) => {
        impl Dispatch<$t, ()> for AppState {
            fn event(
                _: &mut Self,
                _: &$t,
                _: <$t as Proxy>::Event,
                _: &(),
                _: &Connection,
                _: &QueueHandle<Self>,
            ) {
            }
        }
    };
}
empty_dispatch!(wl_shm::WlShm);
empty_dispatch!(wl_shm_pool::WlShmPool);
empty_dispatch!(wl_buffer::WlBuffer);
empty_dispatch!(ExtImageCopyCaptureManagerV1);
empty_dispatch!(ExtForeignToplevelImageCaptureSourceManagerV1);
empty_dispatch!(ExtImageCaptureSourceV1);

impl Dispatch<ExtForeignToplevelListV1, ()> for AppState {
    fn event(
        _: &mut Self,
        _: &ExtForeignToplevelListV1,
        _: ext_foreign_toplevel_list_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
    wayland_client::event_created_child!(AppState, ExtForeignToplevelListV1, [
        ext_foreign_toplevel_list_v1::EVT_TOPLEVEL_OPCODE => (ExtForeignToplevelHandleV1, ()),
    ]);
}

impl Dispatch<ExtForeignToplevelHandleV1, ()> for AppState {
    fn event(
        state: &mut Self,
        proxy: &ExtForeignToplevelHandleV1,
        event: ext_foreign_toplevel_handle_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let id = proxy.id().protocol_id();
        let idx = if let Some(&existing) = state.toplevel_index.get(&id) {
            existing
        } else {
            state.toplevels.push(DiscoveredToplevel {
                proxy: proxy.clone(),
                identifier: String::new(),
            });
            let new = state.toplevels.len() - 1;
            state.toplevel_index.insert(id, new);
            new
        };
        match event {
            ext_foreign_toplevel_handle_v1::Event::Identifier { identifier } => {
                state.toplevels[idx].identifier = identifier;
            }
            ext_foreign_toplevel_handle_v1::Event::Closed => {
                if let Some(remove_idx) = state.toplevel_index.remove(&id) {
                    state.toplevels.remove(remove_idx);
                    for entry in state.toplevel_index.values_mut() {
                        if *entry > remove_idx {
                            *entry -= 1;
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<ExtImageCopyCaptureSessionV1, ()> for AppState {
    fn event(
        state: &mut Self,
        _: &ExtImageCopyCaptureSessionV1,
        event: ext_image_copy_capture_session_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            ext_image_copy_capture_session_v1::Event::BufferSize { width, height } => {
                // BufferSize received AFTER the initial Done is a
                // renegotiation (the compositor saw the source resize).
                // Flag it so the io callback can swap in fresh PW params.
                if state.adv_done
                    && (state.negotiated_width != width || state.negotiated_height != height)
                {
                    state.needs_renegotiate = true;
                }
                state.adv_width = width;
                state.adv_height = height;
            }
            ext_image_copy_capture_session_v1::Event::ShmFormat { format } => {
                if let WEnum::Value(f) = format {
                    let is_preferred = matches!(f, wl_shm::Format::Xrgb8888);
                    if state.adv_format.is_none() || is_preferred {
                        state.adv_format = Some(f);
                    }
                }
            }
            ext_image_copy_capture_session_v1::Event::Done => {
                state.adv_done = true;
            }
            ext_image_copy_capture_session_v1::Event::Stopped => {
                state.session_stopped = true;
            }
            _ => {}
        }
    }
}

impl Dispatch<ExtImageCopyCaptureFrameV1, ()> for AppState {
    fn event(
        state: &mut Self,
        _: &ExtImageCopyCaptureFrameV1,
        event: ext_image_copy_capture_frame_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            ext_image_copy_capture_frame_v1::Event::Ready => state.on_frame_ready(),
            ext_image_copy_capture_frame_v1::Event::Failed { .. } => state.on_frame_failed(),
            _ => {}
        }
    }
}

// ─── AppState behaviour ──────────────────────────────────────────────────

impl AppState {
    fn on_state_changed(
        &mut self,
        stream: &pw::stream::Stream,
        old: pw::stream::StreamState,
        new: pw::stream::StreamState,
    ) {
        tracing::info!(?old, ?new, "pw stream state");
        if matches!(
            new,
            pw::stream::StreamState::Paused | pw::stream::StreamState::Streaming
        ) && let Some(tx) = self.node_id_tx.take()
        {
            let _ = tx.send(Ok(stream.node_id()));
        }

        self.is_streaming = matches!(new, pw::stream::StreamState::Streaming,);

        // Entering Streaming means the renegotiation (if any) has concluded
        // from the graph's point of view — never leave the quiesce flag stuck.
        if self.is_streaming {
            self.renegotiating = false;
        }

        // Kick a capture whenever we (re-)enter Streaming with no frame in
        // flight. The old one-shot `capture_kicked` latch only covered our own
        // size renegotiations; a consumer-initiated renegotiation (Chromium
        // swapping its preview consumer for the WebRTC capturer) also bounces
        // the stream through Paused and abandons the in-flight frame, leaving
        // no other restart path. `pending_frame.is_some()` covers the benign
        // Paused→Streaming blips where a capture is still in flight.
        if self.is_streaming && self.pending_frame.is_none() {
            self.kick_capture();
        }
        if matches!(
            new,
            pw::stream::StreamState::Error(_) | pw::stream::StreamState::Unconnected
        ) {
            // Consumer disconnected (or stream errored). Drop any in-flight
            // frame, mark dying so subsequent callbacks short-circuit, and
            // ask the mainloop to quit on its next tick.
            self.dying = true;
            if let Some(pending) = self.pending_frame.take() {
                pending.frame.destroy();
            }
            if let Some(stop) = self.stop_flag.as_ref() {
                stop.store(true, Ordering::SeqCst);
            }
        }
    }

    fn on_param_changed(
        &mut self,
        _stream: &pw::stream::Stream,
        id: u32,
        param: Option<&Pod>,
    ) {
        if id != spa_sys::SPA_PARAM_Format {
            return;
        }
        let Some(
            param,
        ) = param else {
            return;
        };
        // Same quiesce as the output path: a Format change while a buffer
        // pool exists means the consumer is renegotiating mid-stream, and
        // frames must not be pushed into the half-rebuilt stream.
        if !self.pw_buffer_slots.is_empty() && !self.renegotiating {
            tracing::info!(
                "toplevel: format renegotiation started; \
                pausing capture until the new buffer pool arrives"
            );
            self.renegotiating = true;
        }
        self.frame_interval = parse_pipewire_max_framerate(
            param
        )
            .filter(
                |framerate| framerate.num > 0 && framerate.denom > 0,
            )
            .map(|framerate| {
                std::time::Duration::from_secs_f64(
                    framerate.denom as f64 / framerate.num as f64,
                )
            });
    }

    fn on_add_buffer(&mut self, _stream: &pw::stream::Stream, buffer: *mut pw::sys::pw_buffer) {
        // The replacement pool from a renegotiation is arriving — capture may
        // resume (the kick at the end of this function restarts the cycle).
        self.renegotiating = false;
        let stride = (self.adv_width * 4) as i32;
        let size = stride as usize * self.adv_height as usize;
        let memfd =
            match rustix::fs::memfd_create("shojiwm-portal-pwbuf", rustix::fs::MemfdFlags::CLOEXEC)
            {
                Ok(fd) => fd,
                Err(e) => {
                    tracing::error!("memfd_create: {e}");
                    return;
                }
            };
        if let Err(e) = rustix::fs::ftruncate(&memfd, size as u64) {
            tracing::error!("ftruncate: {e}");
            return;
        }
        let Some(shm) = self.shm.as_ref() else {
            return;
        };
        let pool = shm.create_pool(memfd.as_fd(), size as i32, &self.qh, ());
        let wl_buf = pool.create_buffer(
            0,
            self.adv_width as i32,
            self.adv_height as i32,
            stride,
            wl_shm::Format::Xrgb8888,
            &self.qh,
            (),
        );

        let fd_for_pw = match memfd.try_clone() {
            Ok(c) => c,
            Err(e) => {
                tracing::error!("dup memfd for pw: {e}");
                return;
            }
        };
        unsafe {
            let buf = (*buffer).buffer;
            if buf.is_null() {
                tracing::error!("on_add_buffer: pw_buffer.buffer is null");
                return;
            }
            let datas = std::slice::from_raw_parts_mut((*buf).datas, (*buf).n_datas as usize);
            if datas.is_empty() {
                tracing::error!("on_add_buffer: no datas in pw_buffer");
                return;
            }
            let data = &mut datas[0];
            data.type_ = spa_sys::SPA_DATA_MemFd;
            data.flags = spa_sys::SPA_DATA_FLAG_READWRITE;
            data.fd = fd_for_pw.as_raw_fd() as i64;
            data.data = std::ptr::null_mut();
            data.maxsize = size as u32;
            data.mapoffset = 0;

            let chunk = &mut *data.chunk;
            chunk.offset = 0;
            chunk.stride = stride;
            chunk.size = size as u32;
            chunk.flags = spa_sys::SPA_CHUNK_FLAG_NONE as i32;
        }

        let key = buffer as usize;
        self.pw_buffer_stride.insert(key, stride);
        self.pw_buffer_slots.insert(
            key,
            BufferSlot {
                wl_buffer: wl_buf,
                _shm_pool: pool,
                _fd: memfd,
                _pw_fd: fd_for_pw,
            },
        );

        // A renegotiation can replace the whole buffer pool without the stream
        // ever leaving Streaming; the teardown abandons any in-flight frame, so
        // restart the capture cycle as soon as a fresh buffer exists.
        if self.is_streaming && self.pending_frame.is_none() {
            self.kick_capture();
        }
    }

    fn on_remove_buffer(&mut self, _stream: &pw::stream::Stream, buffer: *mut pw::sys::pw_buffer) {
        let key = buffer as usize;
        if let Some(slot) = self.pw_buffer_slots.remove(&key) {
            slot.wl_buffer.destroy();
        }
        self.pw_buffer_stride.remove(&key);
        // A throttle-held buffer from the outgoing pool must not be reused.
        if self.spare_buffer == Some(
            key
        ) {
            self.spare_buffer = None;
        }
        // Abandon any in-flight frame that pointed at this buffer so a late
        // Ready doesn't dereference freed PipeWire state (the consumer
        // disconnect SEGV path).
        if let Some(pending) = &self.pending_frame
            && pending.pw_buffer == key
        {
            let pending = self.pending_frame.take().unwrap();
            pending.frame.destroy();
        }
    }

    /// Dequeue a PW buffer, create a new wlr frame on the session, attach
    /// the wl_buffer that wraps the same memfd, and submit. The compositor
    /// renders straight into the PW-owned memory and answers with Ready.
    fn kick_capture(&mut self) {
        if self.dying {
            return;
        }
        // Only capture while the stream is actually Streaming — same gate as
        // the output path: capturing/queueing into a paused driver stream
        // wakes the node right back up and forces a buffer-pool rebuild every
        // cycle, which starves consumers stuck in buffer allocation. The
        // Streaming transition in `on_state_changed` restarts the cycle.
        if !self.is_streaming {
            return;
        }
        // Likewise while a format renegotiation is in flight: the cycle
        // restarts from `on_add_buffer` when the replacement pool arrives.
        if self.renegotiating {
            return;
        }
        let Some(session) = self.session.clone() else {
            return;
        };
        let Some(stream) = self.stream.clone() else {
            return;
        };
        // Reuse a throttle-held buffer if one exists, else dequeue a fresh
        // PW buffer.
        let pw_buf = match self.spare_buffer.take() {
            Some(
                key,
            ) => key as *mut pw::sys::pw_buffer,
            None => unsafe {
                stream
                    .dequeue_raw_buffer()
            },
        };
        if pw_buf.is_null() {
            // Consumer hasn't returned a buffer yet. Same situation as the
            // output path: small backoff and let the natural cycle re-kick.
            tracing::debug!("kick_capture: dequeue_raw_buffer returned null");
            thread::sleep(std::time::Duration::from_millis(2));
            return;
        }
        let key = pw_buf as usize;
        let Some(slot) = self.pw_buffer_slots.get(&key) else {
            tracing::error!("kick_capture: no slot for dequeued pw_buffer {key:#x}");
            unsafe { stream.queue_raw_buffer(pw_buf) };
            return;
        };
        let frame = session.create_frame(&self.qh, ());
        frame.attach_buffer(&slot.wl_buffer);
        frame.capture();
        self.pending_frame = Some(PendingFrame {
            frame,
            pw_buffer: key,
        });
        if let Some(conn) = self.conn.as_ref()
            && let Err(e) = conn.flush()
        {
            tracing::warn!("kick_capture: flush failed: {e}");
        }
    }

    fn on_frame_ready(&mut self) {
        if self.dying {
            return;
        }
        let Some(pending) = self.pending_frame.take() else {
            return;
        };
        pending.frame.destroy();

        // Drop the frame without queueing if the stream left Streaming while
        // it was in flight — queueing into a paused driver stream is exactly
        // the wake-up that restarts the pause/rebuild churn. The buffer stays
        // dequeued until the next pool rebuild, which is fine.
        // If the frame landed before the negotiated frame interval elapsed,
        // hold the buffer in `spare_buffer` instead of queueing — the next
        // kick recaptures into it, so the consumer sees at most the
        // negotiated framerate.
        // Compare against 3/4 of the interval, not the full interval: when
        // the negotiated rate equals the capture rate, scheduling jitter
        // would otherwise flag ~every frame as early and halve the throughput.
        let throttled = match (
            self.frame_interval,
            self.last_queue_at,
        ) {
            (
                Some(
                    interval,
                ),
                Some(
                    last_queue_at,
                ),
            ) => last_queue_at
                .elapsed() < interval
                * 3 / 4,
            _ => false,
        };
        if let Some(stream) = self.stream.clone()
            && pending.pw_buffer != 0
            && !self.dying
            && self.is_streaming
            && !self.renegotiating
            && self.pw_buffer_slots.contains_key(&pending.pw_buffer)
        {
            if throttled {
                self.spare_buffer = Some(pending.pw_buffer);
            } else {
                let pw_buf = pending.pw_buffer as *mut pw::sys::pw_buffer;
                unsafe {
                    if !pw_buf.is_null()
                        && !(*pw_buf).buffer.is_null()
                        && (*(*pw_buf).buffer).n_datas > 0
                    {
                        let datas = std::slice::from_raw_parts_mut(
                            (*(*pw_buf).buffer).datas,
                            (*(*pw_buf).buffer).n_datas as usize,
                        );
                        let data = &mut datas[0];
                        let stride = *self.pw_buffer_stride.get(&pending.pw_buffer).unwrap_or(&0);
                        let chunk = &mut *data.chunk;
                        chunk.offset = 0;
                        chunk.stride = stride;
                        chunk.size = (stride as u32) * self.adv_height;
                        chunk.flags = spa_sys::SPA_CHUNK_FLAG_NONE as i32;
                        fill_header_meta(pw_buf, self.frame_sequence);
                    }
                    stream.queue_raw_buffer(pw_buf);
                }
                self.frame_sequence += 1;
                self.last_queue_at = Some(std::time::Instant::now());
                self.frames_completed += 1;
            }
        }
        if self.last_log_at.elapsed() >= std::time::Duration::from_secs(2) {
            tracing::info!(
                frames = self.frames_completed,
                "toplevel screencast: frames queued"
            );
            self.last_log_at = std::time::Instant::now();
        }

        self.kick_capture();
    }

    fn on_frame_failed(&mut self) {
        if self.dying {
            return;
        }
        tracing::warn!("toplevel screencast frame failed");
        if let Some(pending) = self.pending_frame.take() {
            pending.frame.destroy();
            // PW expects the dequeued buffer back, but its content is stale —
            // mark the chunk empty and corrupted so consumers skip it instead
            // of re-showing whatever the buffer held last.
            if pending.pw_buffer != 0
                && let Some(stream) = self.stream.clone()
            {
                let pw_buf = pending.pw_buffer as *mut pw::sys::pw_buffer;
                unsafe {
                    if !pw_buf.is_null()
                        && !(*pw_buf).buffer.is_null()
                        && (*(*pw_buf).buffer).n_datas > 0
                    {
                        let datas = std::slice::from_raw_parts_mut(
                            (*(*pw_buf).buffer).datas,
                            (*(*pw_buf).buffer).n_datas as usize,
                        );
                        let chunk = &mut *datas[0].chunk;
                        chunk.size = 0;
                        chunk.flags = spa_sys::SPA_CHUNK_FLAG_CORRUPTED as i32;
                    }
                    stream.queue_raw_buffer(
                        pw_buf,
                    );
                }
            }
        }
        thread::sleep(std::time::Duration::from_millis(50));
        self.kick_capture();
    }
}

// ─── POD builders ─────────────────────────────────────────────────────────

fn build_video_format_param(
    width: u32,
    height: u32,
    framerate: u32,
) -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>> {
    let max_framerate = Fraction {
        num: framerate.max(1),
        denom: 1,
    };
    let preferred_framerate = Fraction {
        num: framerate.max(1).min(60),
        denom: 1,
    };
    let obj = Value::Object(Object {
        type_: spa_sys::SPA_TYPE_OBJECT_Format,
        id: spa_sys::SPA_PARAM_EnumFormat,
        properties: vec![
            Property::new(
                spa_sys::SPA_FORMAT_mediaType,
                Value::Id(Id(spa_sys::SPA_MEDIA_TYPE_video)),
            ),
            Property::new(
                spa_sys::SPA_FORMAT_mediaSubtype,
                Value::Id(Id(spa_sys::SPA_MEDIA_SUBTYPE_raw)),
            ),
            Property::new(
                spa_sys::SPA_FORMAT_VIDEO_format,
                Value::Id(Id(spa_sys::SPA_VIDEO_FORMAT_BGRx)),
            ),
            Property::new(
                spa_sys::SPA_FORMAT_VIDEO_size,
                Value::Rectangle(Rectangle { width, height }),
            ),
            Property::new(
                spa_sys::SPA_FORMAT_VIDEO_framerate,
                Value::Choice(ChoiceValue::Fraction(Choice(
                    ChoiceFlags::empty(),
                    ChoiceEnum::Range {
                        default: preferred_framerate,
                        min: Fraction { num: 0, denom: 1 },
                        max: max_framerate,
                    },
                ))),
            ),
            Property::new(
                spa_sys::SPA_FORMAT_VIDEO_maxFramerate,
                Value::Choice(ChoiceValue::Fraction(Choice(
                    ChoiceFlags::empty(),
                    ChoiceEnum::Range {
                        default: preferred_framerate,
                        min: Fraction { num: 0, denom: 1 },
                        max: max_framerate,
                    },
                ))),
            ),
        ],
    });
    Ok(PodSerializer::serialize(Cursor::new(Vec::new()), &obj)?
        .0
        .into_inner())
}

fn build_buffers_param(
    width: u32,
    height: u32,
) -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>> {
    let stride = (width * 4) as i32;
    let size = stride * height as i32;
    let memfd_flag = 1 << spa_sys::SPA_DATA_MemFd;
    let obj = Value::Object(Object {
        type_: spa_sys::SPA_TYPE_OBJECT_ParamBuffers,
        id: spa_sys::SPA_PARAM_Buffers,
        properties: vec![
            Property::new(
                spa_sys::SPA_PARAM_BUFFERS_buffers,
                Value::Choice(ChoiceValue::Int(Choice(
                    ChoiceFlags::empty(),
                    ChoiceEnum::Range {
                        default: 8,
                        min: 2,
                        max: 16,
                    },
                ))),
            ),
            Property::new(spa_sys::SPA_PARAM_BUFFERS_blocks, Value::Int(1)),
            Property::new(spa_sys::SPA_PARAM_BUFFERS_size, Value::Int(size)),
            Property::new(spa_sys::SPA_PARAM_BUFFERS_stride, Value::Int(stride)),
            Property::new(
                spa_sys::SPA_PARAM_BUFFERS_dataType,
                Value::Choice(ChoiceValue::Int(Choice(
                    ChoiceFlags::empty(),
                    ChoiceEnum::Flags {
                        default: memfd_flag,
                        flags: vec![memfd_flag],
                    },
                ))),
            ),
        ],
    });
    Ok(PodSerializer::serialize(Cursor::new(Vec::new()), &obj)?
        .0
        .into_inner())
}
