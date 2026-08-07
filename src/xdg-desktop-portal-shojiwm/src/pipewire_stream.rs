//! Unified PipeWire + wlr-screencopy pipeline for ScreenCast.
//!
//! # Architecture: DRIVER + ALLOC_BUFFERS + wayland-driven queue
//!
//! This module deliberately mirrors xdg-desktop-portal-hyprland's approach
//! rather than the more obvious "PipeWire pulls, we push" model. The choice
//! is what gives us full output-refresh framerates (~65 fps on a 66 Hz panel)
//! instead of being pinned to the PipeWire graph driver's audio quantum
//! (~46.875 fps = 1024/48000 — the original xdpw bug, see
//! `knowledges/screencast-30fps-xdpw-bug.md`).
//!
//! ## Why each piece matters
//!
//! - **`PW_STREAM_FLAG_DRIVER`** marks our stream as the cycle driver in
//!   PipeWire's graph. Without it, the graph driver ends up being whatever
//!   audio sink is running, and that sink ticks at its quantum (typically
//!   1024/48000 ≈ 21.3 ms = 46.875 Hz). Our video stream is then scheduled
//!   piggy-back on those ticks — that's exactly the regression
//!   `xdpw@ca7a3e2e` introduced. With DRIVER we own the cycle pacing.
//!
//! - **`PW_STREAM_FLAG_ALLOC_BUFFERS`** tells PipeWire to pre-allocate the
//!   buffer slots from our `SPA_PARAM_Buffers` advert; our `add_buffer`
//!   callback fires once per slot with the empty `pw_buffer` so we can
//!   populate `spa_data{type, fd, maxsize}`. The preferred path allocates a
//!   GBM DMA-BUF and exposes the same BO both as a linux-dmabuf `wl_buffer`
//!   and as a PipeWire DMA-BUF. If GBM or linux-dmabuf is unavailable we fall
//!   back to the older memfd + `wl_shm_pool` path.
//!
//! - **Wayland-driven queue** is what actually closes the timing loop. With
//!   DRIVER set, `on_process` no longer fires on its own — there is no
//!   external pull. We instead let each `wlr-screencopy ready` event call
//!   `pw_stream_queue_buffer`, which both delivers the frame to consumers
//!   and advances the PipeWire cycle. The next `capture_output` is issued
//!   immediately after, so our cycle rate matches the rate at which the
//!   compositor finishes new screencopy frames — i.e. one per vblank, i.e.
//!   the output refresh.
//!
//!   The earlier "pull" model (no DRIVER, `MAP_BUFFERS`, fill in
//!   `on_process`) "worked" but observed exactly 46.875 fps because the
//!   audio sink was driving. The xdph-style "push from wayland" model is
//!   what severs that link.
//!
//! - **Single thread** with `pipewire::loop_::Loop::add_io` attaching the
//!   wayland socket fd to the same loop the PipeWire main loop polls. This
//!   removes all cross-thread synchronization around the stream handle: the
//!   wayland event callbacks (which call `dequeue_raw_buffer` /
//!   `queue_raw_buffer`) and the PipeWire stream callbacks (which set up
//!   buffers in `add_buffer`) all run on the same thread, accessing
//!   `AppState` through one `Rc<RefCell<_>>`.
//!
//!   `pipewire::stream::StreamRc` is `Rc`-based (`!Send`), and switching to
//!   `pw_thread_loop` to get a `Send`-able handle was an option, but the
//!   unified-loop approach is simpler and matches what xdph does in C.
//!
//! ## Flow per frame
//!
//!   1. `kick_capture()` sends `capture_output` to the compositor and flushes
//!      the wayland socket immediately. (Without the flush the request sits
//!      in the outbound queue forever — the `add_io` callback only wakes on
//!      *incoming* bytes, so there's a catch-22 if we never push first.)
//!   2. Compositor sends `Buffer { format, w, h, stride }` advertising the
//!      pixel layout — we ignore the values (they match what we negotiated
//!      with PipeWire) and just wait for the synchronization event.
//!   3. `BufferDone` arrives → `dequeue_raw_buffer()` pops a pw_buffer; we
//!      look up its paired `wl_buffer` and call `frame.copy(&wl_buffer)`.
//!      The compositor writes pixels directly into the PipeWire-owned backing
//!      storage.
//!   4. `Ready` arrives → we set `chunk.size` on the pw_buffer, call
//!      `queue_raw_buffer()` (this is the wake-up signal for consumers AND
//!      the cycle advance for the DRIVER stream), then immediately call
//!      `kick_capture()` for the next frame.
//!
//! ## Why not the other architectures we tried
//!
//! - `MAP_BUFFERS` + `on_process` pull + cache copy (Phase 4b): worked, but
//!   audio-quantum-paced at 46.875 fps.
//! - `DRIVER` + `pw_stream_trigger_process` from a timer (Phase 4a): returned
//!   `EIO` because the stream never became a real driver in the graph
//!   negotiation. (Even when it did, this would still couple to whatever
//!   pace our timer was set at, not vblank.)
//! - Two-thread design with cross-thread `EventSource::signal()`: works in
//!   principle but adds locking around the `StreamRc` and is materially
//!   harder to reason about. Same architecture, more friction.

use std::cell::RefCell;
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::Cursor;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd, RawFd};
use std::path::{Path, PathBuf};
use std::ptr::NonNull;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread;

use drm_fourcc::{DrmFourcc, DrmModifier};
use gbm::{BufferObject, BufferObjectFlags, Device as GbmDevice};
use pipewire as pw;
use pw::spa;
use pw::spa::pod::deserialize::PodDeserializer;
use pw::spa::pod::serialize::PodSerializer;
use pw::spa::pod::{ChoiceValue, Object, Pod, PodPropFlags, Property, PropertyFlags, Value};
use pw::spa::support::system::IoFlags;
use pw::spa::utils::{Choice, ChoiceEnum, ChoiceFlags, Fraction, Id, Rectangle};
use spa::sys as spa_sys;
use wayland_client::protocol::{wl_buffer, wl_output, wl_registry, wl_shm, wl_shm_pool};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum};
use wayland_protocols::wp::linux_dmabuf::zv1::client::{
    zwp_linux_buffer_params_v1, zwp_linux_dmabuf_v1,
};
use wayland_protocols_wlr::screencopy::v1::client::{
    zwlr_screencopy_frame_v1::{self, ZwlrScreencopyFrameV1},
    zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1,
};

#[derive(Debug, Clone)]
pub struct StreamSpec {
    pub output_name: String,
    pub width: u32,
    pub height: u32,
    pub framerate: u32,
    /// Whether to render the cursor into the captured output. Translates to
    /// `overlay_cursor=1` on wlr-screencopy's `capture_output`.
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
        .name("portal-screencast".into())
        .spawn(move || {
            if let Err(e) = run(spec, tx, stop_for_thread) {
                tracing::error!("screencast thread exited: {e}");
            }
        })?;
    let node_id = rx
        .recv()
        .map_err(|_| "screencast thread died before reporting node id".to_string())?
        .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { e.into() })?;
    tracing::info!(node_id, "screencast stream live");
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
    manager: Option<ZwlrScreencopyManagerV1>,
    target_output: Option<wl_output::WlOutput>,
    shm: Option<wl_shm::WlShm>,
    dmabuf: Option<zwp_linux_dmabuf_v1::ZwpLinuxDmabufV1>,
    gbm: Option<GbmDevice<File>>,
    dmabuf_modifiers: Vec<DrmModifier>,
    negotiated_dmabuf_modifier: Option<DrmModifier>,
    dmabuf_failed: bool,
    /// Whether the CURRENTLY negotiated format carries a DMA-BUF modifier.
    /// Buffer allocation must follow this: a modifier-less format means the
    /// consumer expects mappable memory, and serving DMA-BUF anyway puts
    /// OBS into a renegotiation livelock. Unlike `dmabuf_failed` this is NOT
    /// a latch — Chromium swaps consumers mid-session (preview → WebRTC
    /// capturer) and the next consumer may negotiate DMA-BUF again, so the
    /// advertised formats always stay broad and this just tracks the latest
    /// negotiation result.
    format_has_modifier: bool,

    // PipeWire stream (cloneable Rc handle)
    stream: Option<pw::stream::StreamRc>,
    /// pw_buffer raw ptr -> its wrapping wl_buffer + backing storage.
    pw_buffer_slots: HashMap<usize, BufferSlot>,
    /// pw_buffer raw ptr → its negotiated stride (cached at add_buffer time).
    pw_buffer_stride: HashMap<usize, i32>,

    // wlr-screencopy session state
    pending_frame: Option<PendingFrame>,
    adv_format: Option<wl_shm::Format>,
    adv_width: u32,
    adv_height: u32,
    adv_stride: u32,
    adv_flags: zwlr_screencopy_frame_v1::Flags,

    // node-id handoff (taken on first Paused transition)
    node_id_tx: Option<mpsc::SyncSender<Result<u32, String>>>,

    // Diagnostics
    frames_completed: u64,
    last_log_at: std::time::Instant,
    /// Whether the PW stream is currently in Streaming state. Gates capture
    /// kicks from `on_add_buffer` so we don't capture into a paused stream.
    is_streaming: bool,
    /// True from a mid-stream Format renegotiation until the replacement
    /// buffer pool starts arriving. While set, no captures are kicked and no
    /// frames are queued: pushing frames into a stream whose consumer is
    /// mid-way through tearing down one buffer pool and importing the next
    /// is exactly the window where Chromium's capture device has been seen
    /// to stall (KWin's paced/coalesced delivery never renegotiates under
    /// fire, and KDE doesn't exhibit the stall).
    renegotiating: bool,
    /// Frame interval derived from the negotiated `maxFramerate`. `None`
    /// until a format with a usable framerate is negotiated; frames are then
    /// queued no faster than this, matching KWin. Capture still runs at
    /// vblank pace — early frames are held in `spare_buffer` and recopied
    /// rather than queued.
    frame_interval: Option<std::time::Duration>,
    last_queue_at: Option<std::time::Instant>,
    /// A dequeued-but-unqueued pw_buffer held back by the framerate
    /// throttle. Reused for the next capture instead of dequeuing another
    /// slot, so the pool never starves while throttling.
    spare_buffer: Option<usize>,
    /// Monotonic per-stream frame counter for `spa_meta_header.seq`.
    frame_sequence: u64,

    // Same dying / stop_flag pattern as toplevel_stream.rs — once the
    // consumer disconnects we short-circuit every callback so we don't
    // dereference a stale pw_buffer.
    dying: bool,
    stop_flag: Option<Arc<AtomicBool>>,
}

struct PendingFrame {
    frame: ZwlrScreencopyFrameV1,
    /// The PW buffer this frame is being copied into.
    pw_buffer: usize,
}

struct BufferSlot {
    wl_buffer: wl_buffer::WlBuffer,
    _storage: BufferSlotStorage,
    /// The fd handed to PipeWire in `spa_data.fd`. libpipewire does not take
    /// ownership of fds the client fills in under ALLOC_BUFFERS, so it must
    /// stay owned here and drop with the slot — otherwise every buffer-pool
    /// rebuild leaks one fd per buffer.
    _pw_fd: OwnedFd,
}

enum BufferSlotStorage {
    Shm {
        _shm_pool: wl_shm_pool::WlShmPool,
        _fd: OwnedFd,
    },
    Dmabuf {
        _bo: BufferObject<()>,
        _wl_fd: OwnedFd,
    },
}

struct AllocatedSlot {
    wl_buffer: wl_buffer::WlBuffer,
    storage: BufferSlotStorage,
    fd_for_pw: OwnedFd,
    stride: i32,
    size: usize,
    data_type: spa_sys::spa_data_type,
}

// Raw pw_buffer pointers don't carry Send / Sync inferred by the compiler, but
// the whole AppState only ever lives on a single thread.
unsafe impl Send for AppState {}

/// Carrier for an OwnedFd inside `add_io` (which needs `AsRawFd`).
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

    // Connect Wayland.
    let conn = Connection::connect_to_env()?;
    let mut event_queue: EventQueue<AppState> = conn.new_event_queue();
    let qh = event_queue.handle();
    let _registry = conn.display().get_registry(&qh, ());

    // Stub AppState for the bootstrap roundtrips.
    let mut state = AppState {
        spec: spec.clone(),
        conn: Some(conn.clone()),
        qh: qh.clone(),
        manager: None,
        target_output: None,
        shm: None,
        dmabuf: None,
        gbm: None,
        dmabuf_modifiers: Vec::new(),
        negotiated_dmabuf_modifier: None,
        dmabuf_failed: false,
        format_has_modifier: false,
        stream: None,
        pw_buffer_slots: HashMap::new(),
        pw_buffer_stride: HashMap::new(),
        pending_frame: None,
        adv_format: None,
        adv_width: 0,
        adv_height: 0,
        adv_stride: 0,
        adv_flags: zwlr_screencopy_frame_v1::Flags::empty(),
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
    };

    // Bind globals (round 1), then receive wl_output names (round 2).
    event_queue.roundtrip(&mut state)?;
    event_queue.roundtrip(&mut state)?;

    if state.manager.is_none() {
        let _ = state.node_id_tx.take().map(|tx| {
            tx.send(Err(
                "compositor doesn't expose zwlr_screencopy_manager_v1".into()
            ))
        });
        return Err("no zwlr_screencopy_manager_v1".into());
    }
    if state.shm.is_none() {
        let _ = state
            .node_id_tx
            .take()
            .map(|tx| tx.send(Err("compositor doesn't expose wl_shm".into())));
        return Err("no wl_shm".into());
    }
    state.gbm = if screencast_dmabuf_disabled_by_env() {
        tracing::info!("screencast: DMA-BUF capture disabled by environment; using SHM capture");
        None
    } else {
        match init_gbm_device_for_output(&spec.output_name) {
            Ok(Some(device)) if state.dmabuf.is_some() => {
                tracing::info!(
                    backend = device.backend_name(),
                    "screencast: DMA-BUF capture enabled"
                );
                Some(device)
            }
            Ok(Some(_)) => {
                tracing::info!("screencast: zwp_linux_dmabuf_v1 missing; using SHM capture");
                None
            }
            Ok(None) => {
                tracing::info!(
                    "screencast: no usable DMA-BUF render node found; using SHM capture"
                );
                None
            }
            Err(e) => {
                tracing::warn!("screencast: failed to initialize GBM ({e}); using SHM capture");
                None
            }
        }
    };
    if state.target_output.is_none() {
        let _ = state
            .node_id_tx
            .take()
            .map(|tx| tx.send(Err(format!("output {:?} not found", spec.output_name))));
        return Err("target output not found".into());
    }
    tracing::info!(output = spec.output_name, "screencast: globals bound");

    // Build the PipeWire stream.
    let stream = pw::stream::StreamRc::new(
        core,
        "shojiwm-screencast",
        pw::properties::properties! {
            *pw::keys::MEDIA_CLASS => "Video/Source",
            *pw::keys::MEDIA_ROLE => "Screen",
            *pw::keys::NODE_NAME => "shojiwm-portal-stream",
            *pw::keys::NODE_DESCRIPTION => "ShojiWM portal screencast",
        },
    )?;
    state.stream = Some(stream.clone());

    let state_rc = Rc::new(RefCell::new(state));

    // Register stream listeners — they all funnel into AppState via the Rc.
    let s_state = state_rc.clone();
    let s_add = state_rc.clone();
    let s_remove = state_rc.clone();
    let s_param = state_rc.clone();
    let _listener = stream
        .add_local_listener_with_user_data(())
        .state_changed(move |stream, _ud, old, new| {
            s_state.borrow_mut().on_state_changed(stream, old, new);
        })
        .param_changed(move |stream, _ud, id, param| {
            s_param.borrow_mut().on_param_changed(stream, id, param);
        })
        .add_buffer(move |stream, _ud, buffer| {
            s_add.borrow_mut().on_add_buffer(stream, buffer);
        })
        .remove_buffer(move |stream, _ud, buffer| {
            s_remove.borrow_mut().on_remove_buffer(stream, buffer);
        })
        .process(|_, _| {
            // No-op: the cycle is driven by queue_buffer from the wlr-screencopy
            // ready handler. on_process firing here is rare under DRIVER and
            // doesn't need to do anything.
        })
        .register()?;

    // Negotiate format + buffer params.
    let dmabuf_modifiers = {
        let state = state_rc.borrow();
        state.usable_dmabuf_modifiers()
    };
    if !dmabuf_modifiers.is_empty() {
        tracing::info!(
            count = dmabuf_modifiers.len(),
            modifiers = ?dmabuf_modifiers
                .iter()
                .map(|modifier| format!("{:#x}", u64::from(*modifier)))
                .collect::<Vec<_>>(),
            "screencast: advertising DMA-BUF modifiers to PipeWire"
        );
    }
    let mut param_bytes = build_video_format_params(&spec, &dmabuf_modifiers, false)?;
    let buffers_bytes = build_buffers_param(&spec, !dmabuf_modifiers.is_empty())?;
    param_bytes.push(buffers_bytes);
    param_bytes
        .push(
            build_header_meta_param()?,
        );
    let mut params = Vec::with_capacity(param_bytes.len());
    for bytes in &param_bytes {
        params.push(Pod::from_bytes(bytes).ok_or("PipeWire POD parse failed".to_string())?);
    }
    stream.connect(
        spa::utils::Direction::Output,
        None,
        pw::stream::StreamFlags::DRIVER | pw::stream::StreamFlags::ALLOC_BUFFERS,
        &mut params,
    )?;
    tracing::info!("screencast: PW stream connected (DRIVER | ALLOC_BUFFERS)");

    // Attach the wayland fd to the PW main loop. Move event_queue into the io
    // closure — after this point all wayland dispatching is fd-driven.
    let wl_fd = conn.as_fd().try_clone_to_owned()?;
    // SAFETY: we own the OwnedFd for the lifetime of the closure; transmuting
    // to BorrowedFd<'static> is unsafe but the fd outlives the IoSource.
    let wl_fd_static: BorrowedFd<'static> =
        unsafe { std::mem::transmute::<BorrowedFd<'_>, BorrowedFd<'static>>(wl_fd.as_fd()) };
    let fd_holder = FdHolder(wl_fd_static);

    let s_for_io = state_rc.clone();
    let conn_for_io = conn.clone();
    let event_queue_cell = RefCell::new(event_queue);
    let _io = mainloop.loop_().add_io(fd_holder, IoFlags::IN, move |_| {
        // Read whatever wayland has on the socket without blocking.
        if let Some(guard) = conn_for_io.prepare_read() {
            let _ = guard.read();
        }
        let mut eq = event_queue_cell.borrow_mut();
        let mut state = s_for_io.borrow_mut();
        if let Err(e) = eq.dispatch_pending(&mut *state) {
            tracing::error!("wayland dispatch: {e}");
        }
        let _ = conn_for_io.flush();
    });
    tracing::info!("wayland fd attached to PW loop");

    // Make sure any pending wayland requests (the registry bind etc.) hit the
    // wire before mainloop starts blocking on its own poll.
    conn.flush()?;

    // Run forever (until stop flag flips via teardown_loop event below or
    // process exit).
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

    // Cleanup: detach stream slots; OwnedFds drop closes them.
    let mut state = state_rc.borrow_mut();
    state.pw_buffer_slots.clear();
    state.pending_frame = None;
    tracing::info!("screencast thread exiting cleanly");
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
            "wl_output" => {
                // Bind eagerly; filter by name when we get the Name event.
                let _output =
                    registry.bind::<wl_output::WlOutput, _, _>(name, version.min(4), qh, ());
            }
            "wl_shm" => {
                state.shm =
                    Some(registry.bind::<wl_shm::WlShm, _, _>(name, version.min(1), qh, ()));
            }
            "zwp_linux_dmabuf_v1" => {
                state.dmabuf = Some(
                    registry.bind::<zwp_linux_dmabuf_v1::ZwpLinuxDmabufV1, _, _>(
                        name,
                        version.min(3),
                        qh,
                        (),
                    ),
                );
            }
            "zwlr_screencopy_manager_v1" => {
                state.manager = Some(registry.bind::<ZwlrScreencopyManagerV1, _, _>(
                    name,
                    version.min(3),
                    qh,
                    (),
                ));
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_output::WlOutput, ()> for AppState {
    fn event(
        state: &mut Self,
        output: &wl_output::WlOutput,
        event: wl_output::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_output::Event::Name { name } = event
            && name == state.spec.output_name
            && state.target_output.is_none()
        {
            state.target_output = Some(output.clone());
        }
    }
}

impl Dispatch<wl_shm::WlShm, ()> for AppState {
    fn event(
        _: &mut Self,
        _: &wl_shm::WlShm,
        _: wl_shm::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_shm_pool::WlShmPool, ()> for AppState {
    fn event(
        _: &mut Self,
        _: &wl_shm_pool::WlShmPool,
        _: wl_shm_pool::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_buffer::WlBuffer, ()> for AppState {
    fn event(
        _: &mut Self,
        _: &wl_buffer::WlBuffer,
        _: wl_buffer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<zwp_linux_dmabuf_v1::ZwpLinuxDmabufV1, ()> for AppState {
    fn event(
        state: &mut Self,
        _: &zwp_linux_dmabuf_v1::ZwpLinuxDmabufV1,
        event: zwp_linux_dmabuf_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwp_linux_dmabuf_v1::Event::Modifier {
                format,
                modifier_hi,
                modifier_lo,
            } if format == DrmFourcc::Xrgb8888 as u32 => {
                let modifier = DrmModifier::from(((modifier_hi as u64) << 32) | modifier_lo as u64);
                if !state.dmabuf_modifiers.contains(&modifier) {
                    state.dmabuf_modifiers.push(modifier);
                }
            }
            zwp_linux_dmabuf_v1::Event::Format { format }
                if format == DrmFourcc::Xrgb8888 as u32
                && !state.dmabuf_modifiers.contains(&DrmModifier::Invalid) => {
                    state.dmabuf_modifiers.push(DrmModifier::Invalid);
                }
            _ => {}
        }
    }
}

impl Dispatch<zwp_linux_buffer_params_v1::ZwpLinuxBufferParamsV1, ()> for AppState {
    fn event(
        _: &mut Self,
        _: &zwp_linux_buffer_params_v1::ZwpLinuxBufferParamsV1,
        event: zwp_linux_buffer_params_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if matches!(event, zwp_linux_buffer_params_v1::Event::Failed) {
            tracing::warn!("screencast: DMA-BUF wl_buffer creation failed");
        }
    }

    wayland_client::event_created_child!(AppState, zwp_linux_buffer_params_v1::ZwpLinuxBufferParamsV1, [
        zwp_linux_buffer_params_v1::EVT_CREATED_OPCODE => (wl_buffer::WlBuffer, ())
    ]);
}

impl Dispatch<ZwlrScreencopyManagerV1, ()> for AppState {
    fn event(
        _: &mut Self,
        _: &ZwlrScreencopyManagerV1,
        _: <ZwlrScreencopyManagerV1 as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwlrScreencopyFrameV1, ()> for AppState {
    fn event(
        state: &mut Self,
        frame: &ZwlrScreencopyFrameV1,
        event: zwlr_screencopy_frame_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_screencopy_frame_v1::Event::Buffer {
                format,
                width,
                height,
                stride,
            } => {
                if let WEnum::Value(fmt) = format {
                    state.adv_format = Some(fmt);
                }
                state.adv_width = width;
                state.adv_height = height;
                state.adv_stride = stride;
            }
            zwlr_screencopy_frame_v1::Event::LinuxDmabuf { .. } => {}
            zwlr_screencopy_frame_v1::Event::BufferDone => {
                state.on_buffer_done(frame);
            }
            zwlr_screencopy_frame_v1::Event::Flags {
                flags: WEnum::Value(f),
            } => {
                state.adv_flags = f;
            }
            zwlr_screencopy_frame_v1::Event::Damage { .. } => {}
            zwlr_screencopy_frame_v1::Event::Ready { .. } => {
                state.on_frame_ready();
            }
            zwlr_screencopy_frame_v1::Event::Failed => {
                state.on_frame_failed();
            }
            _ => {}
        }
    }
}

// ─── AppState behaviour ──────────────────────────────────────────────────

impl AppState {
    fn usable_dmabuf_modifiers(&self) -> Vec<DrmModifier> {
        let Some(gbm) = self.gbm.as_ref() else {
            return Vec::new();
        };
        let mut modifiers = self.dmabuf_modifiers.clone();
        if modifiers.is_empty() {
            modifiers.push(DrmModifier::Linear);
        }
        modifiers
            .into_iter()
            .filter(|modifier| {
                *modifier == DrmModifier::Invalid
                    || gbm
                        .format_modifier_plane_count(DrmFourcc::Xrgb8888, *modifier)
                        .unwrap_or(0)
                        > 0
            })
            .collect()
    }

    fn on_param_changed(&mut self, stream: &pw::stream::Stream, id: u32, param: Option<&Pod>) {
        if id != spa_sys::SPA_PARAM_Format {
            return;
        }
        let Some(param) = param else {
            return;
        };
        // A Format change while a buffer pool exists means the consumer is
        // renegotiating mid-stream (e.g. the DMA-BUF → SHM downgrade after a
        // failed cross-GPU import). Quiesce the capture cycle until the new
        // pool arrives in `on_add_buffer` — continuing to queue frames into
        // the half-torn-down stream races the consumer's own rebuild.
        if !self.pw_buffer_slots.is_empty() && !self.renegotiating {
            tracing::info!(
                "screencast: format renegotiation started; \
                pausing capture until the new buffer pool arrives"
            );
            self.renegotiating = true;
        }
        self.frame_interval = parse_pipewire_max_framerate(param)
            .filter(|framerate| framerate.num > 0 && framerate.denom > 0)
            .map(|framerate| {
                std::time::Duration::from_secs_f64(
                    framerate.denom as f64 / framerate.num as f64,
                )
            });
        let Some(modifier_param) = parse_pipewire_modifier_param(param) else {
            // A format WITHOUT a modifier property means the consumer wants
            // mappable memory, not DMA-BUF. OBS does this after a failed
            // GL import: it drops the modifier and renegotiates. If we keep
            // serving DMA-BUF anyway (stale `negotiated_dmabuf_modifier`),
            // OBS fails the import again and renegotiates in a ~20 Hz loop —
            // each cycle reallocating the whole 8×8 MB buffer pool, which
            // pegs several cores and leaks GPU memory until the machine dies.
            // Allocation follows the format:
            // SHM while this stays negotiated.
            //
            // Deliberately NOT a latch, and deliberately no update_params
            // narrowing the advert to SHM-only: Chromium swaps consumers
            // mid-session (preview → WebRTC capturer on Go Live), and the
            // next consumer may offer DMA-BUF formats again. A narrowed
            // advert makes that negotiation intersect to nothing — the link
            // then never completes, the stream sits Paused forever, and
            // vesktop's Go Live dies with endless portal retries.
            if self.format_has_modifier {
                tracing::info!(
                    "screencast: consumer \
                    negotiated a modifier-less format; allocating SHM buffers"
                );
            }
            self.format_has_modifier = false;
            self.negotiated_dmabuf_modifier = None;
            return;
        };
        self.format_has_modifier = true;

        if modifier_param.dont_fixate {
            let Some(modifier) = self.choose_working_dmabuf_modifier(&modifier_param.modifiers)
            else {
                tracing::warn!(
                    requested = ?modifier_param
                        .modifiers
                        .iter()
                        .map(|modifier| format!("{:#x}", u64::from(*modifier)))
                        .collect::<Vec<_>>(),
                    "screencast: no PipeWire-requested DMA-BUF modifier can be allocated; falling back to SHM"
                );
                self.dmabuf_failed = true;
                if let Err(e) = self.update_pipewire_params(stream, &[]) {
                    tracing::warn!(
                        "screencast: failed to update PipeWire params for SHM fallback: {e}"
                    );
                }
                return;
            };

            self.negotiated_dmabuf_modifier = Some(modifier);
            tracing::info!(
                modifier = format!("{:#x}", u64::from(modifier)),
                "screencast: fixating DMA-BUF modifier"
            );
            if let Err(e) = self.update_pipewire_params(stream, &[modifier]) {
                tracing::warn!("screencast: failed to fixate PipeWire DMA-BUF modifier: {e}");
                self.dmabuf_failed = true;
            }
        } else if let Some(modifier) = modifier_param.modifiers.first().copied() {
            self.negotiated_dmabuf_modifier = Some(modifier);
            tracing::info!(
                modifier = format!("{:#x}", u64::from(modifier)),
                "screencast: PipeWire selected DMA-BUF modifier"
            );
        }
    }

    fn choose_working_dmabuf_modifier(&self, modifiers: &[DrmModifier]) -> Option<DrmModifier> {
        let gbm = self.gbm.as_ref()?;
        for modifier in modifiers {
            if create_screencast_bo(gbm, self.spec.width, self.spec.height, *modifier)
                .ok()
                .flatten()
                .is_some()
            {
                return Some(*modifier);
            }
        }
        None
    }

    fn update_pipewire_params(
        &self,
        stream: &pw::stream::Stream,
        modifiers: &[DrmModifier],
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let mut param_bytes = build_video_format_params(&self.spec, modifiers, true)?;
        let buffers_bytes = build_buffers_param(&self.spec, !modifiers.is_empty())?;
        param_bytes.push(buffers_bytes);
        param_bytes
            .push(
                build_header_meta_param()?,
            );
        let mut params = Vec::with_capacity(param_bytes.len());
        for bytes in &param_bytes {
            params.push(Pod::from_bytes(bytes).ok_or("PipeWire POD parse failed".to_string())?);
        }
        stream.update_params(&mut params)?;
        Ok(())
    }

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
        // flight. A one-shot latch is not enough here: when the consumer
        // renegotiates buffers (Chromium switches its preview consumer for the
        // WebRTC capturer, flipping DMA-BUF → SHM), the stream bounces through
        // Paused, the buffer teardown abandons `pending_frame`, and the cycle
        // has no other way to restart — the session then sits in Streaming
        // delivering nothing, forever. `pending_frame.is_some()` covers the
        // benign Paused→Streaming blips where a capture is still in flight.
        if self.is_streaming && self.pending_frame.is_none() {
            self.kick_capture();
        }
        if matches!(
            new,
            pw::stream::StreamState::Error(_) | pw::stream::StreamState::Unconnected
        ) {
            self.dying = true;
            if let Some(pending) = self.pending_frame.take() {
                pending.frame.destroy();
            }
            if let Some(stop) = self.stop_flag.as_ref() {
                stop.store(true, Ordering::SeqCst);
            }
        }
    }

    fn on_add_buffer(&mut self, _stream: &pw::stream::Stream, buffer: *mut pw::sys::pw_buffer) {
        // The replacement pool from a renegotiation is arriving — capture may
        // resume (the kick at the end of this function restarts the cycle).
        self.renegotiating = false;
        let negotiated_data_types = unsafe {
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
            datas[0].type_
        };

        let slot = match self.create_slot_for_pw_data_types(negotiated_data_types) {
            Ok(slot) => slot,
            Err(e) => {
                tracing::error!(
                    data_types = negotiated_data_types,
                    "create screencast buffer: {e}"
                );
                return;
            }
        };
        let stride = slot.stride;
        let size = slot.size;
        let data_type = slot.data_type;
        let fd_for_pw = slot.fd_for_pw;
        let wl_buf = slot.wl_buffer;
        let storage = slot.storage;

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
            tracing::debug!(
                n_metas = (*buf).n_metas,
                "on_add_buffer: negotiated buffer metas"
            );
            let data = &mut datas[0];
            data.type_ = data_type;
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
                _storage: storage,
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

    fn create_slot_for_pw_data_types(
        &self,
        data_types: spa_sys::spa_data_type,
    ) -> Result<AllocatedSlot, Box<dyn std::error::Error + Send + Sync>> {
        let dmabuf_flag = 1 << spa_sys::SPA_DATA_DmaBuf;
        let memfd_flag = 1 << spa_sys::SPA_DATA_MemFd;
        let allows_dmabuf = data_types & dmabuf_flag != 0 || data_types == spa_sys::SPA_DATA_DmaBuf;
        let allows_memfd = data_types & memfd_flag != 0 || data_types == spa_sys::SPA_DATA_MemFd;

        // DMA-BUF is only valid while the negotiated format carries a
        // modifier; under a modifier-less format the consumer expects
        // mappable memory even if its Buffers param still allows DmaBuf.
        if allows_dmabuf && self.format_has_modifier && !self.dmabuf_failed {
            match self.create_dmabuf_slot() {
                Ok(Some(slot)) => return Ok(slot),
                Ok(None) if !allows_memfd => {
                    return Err(
                        "PipeWire selected DMA-BUF, but DMA-BUF allocation is unavailable".into(),
                    );
                }
                Ok(None) => {}
                Err(e) if !allows_memfd => return Err(e),
                Err(e) => {
                    tracing::warn!("create DMA-BUF screencast buffer: {e}; falling back to SHM");
                }
            }
        }

        if allows_memfd {
            return self.create_shm_slot();
        }

        Err(format!("unsupported PipeWire data types bitmask: {data_types}").into())
    }

    fn create_dmabuf_slot(
        &self,
    ) -> Result<Option<AllocatedSlot>, Box<dyn std::error::Error + Send + Sync>> {
        let (Some(dmabuf), Some(gbm)) = (self.dmabuf.as_ref(), self.gbm.as_ref()) else {
            return Ok(None);
        };

        let flags = BufferObjectFlags::RENDERING;
        if !gbm.is_format_supported(DrmFourcc::Xrgb8888, flags) {
            return Ok(None);
        }

        let modifier = self
            .negotiated_dmabuf_modifier
            .unwrap_or(DrmModifier::Linear);
        let Some(bo) = create_screencast_bo(gbm, self.spec.width, self.spec.height, modifier)?
        else {
            return Ok(None);
        };
        if bo.plane_count() != 1 {
            return Err(format!(
                "expected single-plane XRGB8888 BO, got {}",
                bo.plane_count()
            )
            .into());
        }
        let stride = bo.stride_for_plane(0) as i32;
        let size = stride as usize * self.spec.height as usize;
        let modifier = u64::from(bo.modifier());
        let modifier_hi = (modifier >> 32) as u32;
        let modifier_lo = (modifier & 0xffff_ffff) as u32;

        let wl_fd = bo.fd_for_plane(0)?;
        let fd_for_pw = bo.fd_for_plane(0)?;
        let params = dmabuf.create_params(&self.qh, ());
        params.add(
            wl_fd.as_fd(),
            0,
            bo.offset(0),
            stride as u32,
            modifier_hi,
            modifier_lo,
        );
        let wl_buffer = params.create_immed(
            self.spec.width as i32,
            self.spec.height as i32,
            DrmFourcc::Xrgb8888 as u32,
            zwp_linux_buffer_params_v1::Flags::empty(),
            &self.qh,
            (),
        );
        params.destroy();

        Ok(Some(AllocatedSlot {
            wl_buffer,
            storage: BufferSlotStorage::Dmabuf {
                _bo: bo,
                _wl_fd: wl_fd,
            },
            fd_for_pw,
            stride,
            size,
            data_type: spa_sys::SPA_DATA_DmaBuf,
        }))
    }

    fn create_shm_slot(&self) -> Result<AllocatedSlot, Box<dyn std::error::Error + Send + Sync>> {
        let stride = (self.spec.width * 4) as i32;
        let size = stride as usize * self.spec.height as usize;
        let memfd =
            rustix::fs::memfd_create("shojiwm-portal-pwbuf", rustix::fs::MemfdFlags::CLOEXEC)?;
        rustix::fs::ftruncate(&memfd, size as u64)?;

        let Some(shm) = self.shm.as_ref() else {
            return Err("wl_shm is not bound".into());
        };
        let pool = shm.create_pool(memfd.as_fd(), size as i32, &self.qh, ());
        let wl_buffer = pool.create_buffer(
            0,
            self.spec.width as i32,
            self.spec.height as i32,
            stride,
            wl_shm::Format::Xrgb8888,
            &self.qh,
            (),
        );
        let fd_for_pw = memfd.try_clone()?;

        Ok(AllocatedSlot {
            wl_buffer,
            storage: BufferSlotStorage::Shm {
                _shm_pool: pool,
                _fd: memfd,
            },
            fd_for_pw,
            stride,
            size,
            data_type: spa_sys::SPA_DATA_MemFd,
        })
    }

    fn on_remove_buffer(&mut self, _stream: &pw::stream::Stream, buffer: *mut pw::sys::pw_buffer) {
        let key = buffer as usize;
        if let Some(slot) = self.pw_buffer_slots.remove(&key) {
            slot.wl_buffer.destroy();
        }
        self.pw_buffer_stride.remove(&key);
        // A throttle-held buffer from the outgoing pool must not be reused.
        if self.spare_buffer == Some(key) {
            self.spare_buffer = None;
        }
        // If an in-flight wlr-screencopy frame was targeting this buffer,
        // abandon it. A late Ready event would otherwise call queue_raw_buffer
        // on the now-freed pointer (use-after-free → SEGV — observed when OBS
        // disconnects mid-stream).
        if let Some(pending) = &self.pending_frame
            && pending.pw_buffer == key
        {
            let pending = self.pending_frame.take().unwrap();
            pending.frame.destroy();
        }
    }

    /// Issue the next capture_output request. Must be called when there is
    /// no in-flight frame.
    fn kick_capture(&mut self) {
        if self.dying {
            return;
        }
        // Only capture while the stream is actually Streaming. With no active
        // consumer the graph pauses our driver node after every cycle; if we
        // keep capturing and queueing anyway, each queue wakes the node back
        // up and forces a full buffer-pool rebuild (~each frame). That churn
        // starves consumers stuck in buffer allocation (OBS never got a first
        // frame) and, before the fd-ownership fix, leaked one fd per buffer
        // per rebuild. The Streaming transition in `on_state_changed` restarts
        // the cycle when a consumer shows up.
        if !self.is_streaming {
            return;
        }
        // Likewise while a format renegotiation is in flight: the cycle
        // restarts from `on_add_buffer` when the replacement pool arrives.
        if self.renegotiating {
            return;
        }
        let (Some(manager), Some(output)) = (self.manager.as_ref(), self.target_output.as_ref())
        else {
            tracing::warn!("kick_capture: missing manager or output");
            return;
        };
        let overlay_cursor = if self.spec.cursor_visible { 1 } else { 0 };
        let frame = manager.capture_output(overlay_cursor, output, &self.qh, ());
        self.pending_frame = Some(PendingFrame {
            frame,
            pw_buffer: 0,
        });
        // Critical: flush the request to the compositor immediately. The
        // wayland fd add_io callback only fires when we receive bytes, so
        // without an explicit flush here the request would sit in the
        // outbound queue forever and the compositor would never respond.
        if let Some(conn) = self.conn.as_ref()
            && let Err(e) = conn.flush()
        {
            tracing::warn!("kick_capture: flush failed: {e}");
        }
    }

    fn on_buffer_done(&mut self, _frame: &ZwlrScreencopyFrameV1) {
        if self.dying {
            return;
        }
        // Reuse a throttle-held buffer if one exists, else dequeue a fresh
        // PW buffer to fill.
        let Some(stream) = self.stream.clone() else {
            return;
        };
        let pw_buf = match self.spare_buffer.take() {
            Some(key) => key as *mut pw::sys::pw_buffer,
            None => unsafe { 
                stream
                    .dequeue_raw_buffer()
            },
        };
        if pw_buf.is_null() {
            // Consumer hasn't returned a buffer yet — all advertised slots
            // are in flight. Demote to debug (frequent under slow consumers
            // like OBS at high resolution) and back off a bit before retrying
            // so we don't busy-loop. Functionally a dropped frame.
            tracing::debug!("buffer_done: dequeue_raw_buffer returned null");
            if let Some(p) = self.pending_frame.take() {
                p.frame.destroy();
            }
            thread::sleep(std::time::Duration::from_millis(2));
            self.kick_capture();
            return;
        }
        let key = pw_buf as usize;
        let Some(slot) = self.pw_buffer_slots.get(&key) else {
            tracing::error!("buffer_done: no slot for dequeued pw_buffer {key:#x}");
            unsafe { stream.queue_raw_buffer(pw_buf) };
            return;
        };
        // Tell wlr-screencopy to copy into our wl_buffer.
        let Some(pending) = self.pending_frame.as_mut() else {
            tracing::error!("buffer_done: no pending frame");
            unsafe { stream.queue_raw_buffer(pw_buf) };
            return;
        };
        pending.frame.copy(&slot.wl_buffer);
        pending.pw_buffer = key;
    }

    fn on_frame_ready(&mut self) {
        if self.dying {
            return;
        }
        let Some(pending) = self.pending_frame.take() else {
            return;
        };
        pending.frame.destroy();

        // Set the chunk size on the dequeued PW buffer so the consumer reads
        // the right amount, then queue it. Re-check that the buffer still
        // exists in our slot map — PW may have freed it between dequeue and
        // ready (consumer disconnect path).
        // If the stream left Streaming while this frame was in flight, drop it
        // without queueing: queueing into a paused driver stream is exactly the
        // wake-up that restarts the pause/rebuild churn. The buffer stays dequeued
        // until the next pool rebuild, which is fine.
        // If the frame landed before the negotiated frame interval elapsed,
        // hold the buffer in `spare_buffer` instead of queueing — the next
        // capture recopies into it, so the consumer sees at most the
        // negotiated framerate while capture stays vblank-paced.
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
                self.spare_buffer = Some(
                    pending.pw_buffer
                );
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
                        chunk.size = (stride as u32) * self.spec.height;
                        chunk.flags = spa_sys::SPA_CHUNK_FLAG_NONE as i32;
                        fill_header_meta(pw_buf, self.frame_sequence);
                    }
                    stream.queue_raw_buffer(pw_buf);
                }
                self.frame_sequence += 1;
                self.last_queue_at = Some(
                    std::time::Instant::now()
                );
                self.frames_completed += 1;
            }
        }
        if self.last_log_at.elapsed() >= std::time::Duration::from_secs(2) {
            let elapsed = self.last_log_at.elapsed().as_secs_f64();
            let effective_fps = self.frames_completed as f64 / elapsed.max(0.001);
            tracing::info!(
                frames = self.frames_completed,
                effective_fps,
                "screencast: frames queued"
            );
            self.frames_completed = 0;
            self.last_log_at = std::time::Instant::now();
        }

        // Issue the next capture immediately — Ready arrived at compositor
        // pace, so this paces our cycle to vblank.
        self.kick_capture();
    }

    fn on_frame_failed(&mut self) {
        if self.dying {
            return;
        }
        tracing::warn!("screencast frame failed");
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
                    stream.queue_raw_buffer(pw_buf);
                }
            }
        }
        // Backoff briefly and retry.
        thread::sleep(std::time::Duration::from_millis(50));
        self.kick_capture();
    }
}

fn create_screencast_bo(
    gbm: &GbmDevice<File>,
    width: u32,
    height: u32,
    modifier: DrmModifier,
) -> Result<Option<BufferObject<()>>, Box<dyn std::error::Error + Send + Sync>> {
    let flags = BufferObjectFlags::RENDERING;

    if modifier == DrmModifier::Invalid {
        match gbm.create_buffer_object::<()>(width, height, DrmFourcc::Xrgb8888, flags) {
            Ok(bo) => return Ok(Some(bo)),
            Err(e) => {
                tracing::warn!(
                    "screencast: implicit DMA-BUF allocation failed ({e}); falling back to SHM"
                );
            }
        }
    } else {
        match gbm.create_buffer_object_with_modifiers2::<()>(
            width,
            height,
            DrmFourcc::Xrgb8888,
            [modifier].into_iter(),
            flags,
        ) {
            Ok(bo) => return Ok(Some(bo)),
            Err(e) => {
                tracing::warn!(
                    modifier = format!("{:#x}", u64::from(modifier)),
                    "screencast: DMA-BUF allocation failed ({e}); falling back to SHM"
                );
            }
        }
    }

    Ok(None)
}

fn init_gbm_device_for_output(
    output_name: &str,
) -> Result<Option<GbmDevice<File>>, Box<dyn std::error::Error + Send + Sync>> {
    let mut candidates = Vec::new();
    if let Ok(path) = std::env::var("SHOJI_SCREENCAST_RENDER_NODE") {
        candidates.push(PathBuf::from(path));
    }
    if let Some(path) = render_node_for_output(output_name) {
        tracing::info!(
            output = output_name,
            path = %path.display(),
            "screencast: selected output-matched render node"
        );
        candidates.push(path);
    } else if !screencast_dmabuf_force_enabled() && nvidia_drm_device_has_connected_output() {
        tracing::warn!(
            output = output_name,
            "screencast: connected NVIDIA output detected but no matching render node was found; using SHM capture"
        );
        return Ok(None);
    }
    candidates.extend((128..200).map(|idx| PathBuf::from(format!("/dev/dri/renderD{idx}"))));
    dedup_paths(&mut candidates);

    for path in candidates {
        let file = match OpenOptions::new().read(true).write(true).open(&path) {
            Ok(file) => file,
            Err(_) => continue,
        };
        match GbmDevice::new(file) {
            Ok(device) => {
                tracing::info!(path = %path.display(), "screencast: opened GBM render node");
                return Ok(Some(device));
            }
            Err(e) => {
                tracing::debug!(path = %path.display(), "GBM device init failed: {e}");
            }
        }
    }

    Ok(None)
}

fn dedup_paths(paths: &mut Vec<PathBuf>) {
    let mut seen = Vec::<PathBuf>::new();
    paths.retain(|path| {
        if seen.iter().any(|seen_path| seen_path == path) {
            false
        } else {
            seen.push(path.clone());
            true
        }
    });
}

fn screencast_dmabuf_disabled_by_env() -> bool {
    std::env::var_os("SHOJI_SCREENCAST_NO_DMABUF").is_some()
        || std::env::var_os("SHOJI_SCREENCOPY_NO_DMABUF").is_some()
}

fn screencast_dmabuf_force_enabled() -> bool {
    std::env::var_os("SHOJI_SCREENCAST_FORCE_DMABUF").is_some()
        || std::env::var_os("SHOJI_SCREENCOPY_FORCE_DMABUF").is_some()
}

fn nvidia_drm_device_has_connected_output() -> bool {
    let Ok(entries) = fs::read_dir("/sys/class/drm") else {
        return false;
    };

    entries.filter_map(Result::ok).any(|entry| {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            return false;
        };

        if !name.starts_with("card") || !name.contains('-') {
            return false;
        }

        let connector_path = entry.path();
        let Ok(status) = fs::read_to_string(connector_path.join("status")) else {
            return false;
        };
        if status.trim() != "connected" {
            return false;
        }

        let Some(card_name) = name.split('-').next() else {
            return false;
        };
        let vendor_path = Path::new("/sys/class/drm")
            .join(card_name)
            .join("device/vendor");
        fs::read_to_string(vendor_path)
            .ok()
            .is_some_and(|vendor| vendor.trim().eq_ignore_ascii_case("0x10de"))
    })
}

fn render_node_for_output(output_name: &str) -> Option<PathBuf> {
    let card_name = card_name_for_connected_output(output_name)?;
    let card_device =
        fs::canonicalize(Path::new("/sys/class/drm").join(&card_name).join("device")).ok()?;

    let entries = fs::read_dir("/sys/class/drm").ok()?;
    for entry in entries.filter_map(Result::ok) {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !name.starts_with("renderD") {
            continue;
        }

        let Ok(render_device) = fs::canonicalize(entry.path().join("device")) else {
            continue;
        };
        if render_device == card_device {
            return Some(Path::new("/dev/dri").join(name));
        }
    }

    None
}

fn card_name_for_connected_output(output_name: &str) -> Option<String> {
    let entries = fs::read_dir("/sys/class/drm").ok()?;
    for entry in entries.filter_map(Result::ok) {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };

        let Some((card_name, connector_name)) = split_drm_connector_name(name) else {
            continue;
        };
        if connector_name != output_name {
            continue;
        }

        let status = fs::read_to_string(entry.path().join("status")).ok()?;
        if status.trim() == "connected" {
            return Some(card_name.to_owned());
        }
    }

    None
}

fn split_drm_connector_name(name: &str) -> Option<(&str, &str)> {
    let rest = name.strip_prefix("card")?;
    let dash = rest.find('-')?;
    let card_len = "card".len() + dash;
    Some((&name[..card_len], &name[card_len + 1..]))
}

// ─── POD builders ─────────────────────────────────────────────────────────

fn build_video_format_params(
    spec: &StreamSpec,
    modifiers: &[DrmModifier],
    fixated: bool,
) -> Result<Vec<Vec<u8>>, Box<dyn std::error::Error + Send + Sync>> {
    let mut params = Vec::new();
    if !modifiers.is_empty() {
        params.push(build_video_format_param(spec, Some((modifiers, fixated)))?);
    }
    params.push(build_video_format_param(spec, None)?);
    Ok(params)
}

fn build_video_format_param(
    spec: &StreamSpec,
    modifiers: Option<(&[DrmModifier], bool)>,
) -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>> {
    let max_framerate = Fraction {
        num: spec.framerate.max(1),
        denom: 1,
    };
    let mut properties = vec![
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
    ];
    if let Some((modifiers, fixated)) = modifiers
        && let Some(default) = modifiers.first()
    {
        if fixated {
            properties.push(Property {
                key: spa_sys::SPA_FORMAT_VIDEO_modifier,
                flags: PropertyFlags::MANDATORY,
                value: Value::Long(u64::from(*default) as i64),
            });
        } else {
            properties.push(Property {
                key: spa_sys::SPA_FORMAT_VIDEO_modifier,
                flags: PropertyFlags::MANDATORY | PropertyFlags::DONT_FIXATE,
                value: Value::Choice(ChoiceValue::Long(Choice(
                    ChoiceFlags::empty(),
                    ChoiceEnum::Enum {
                        default: u64::from(*default) as i64,
                        alternatives: modifiers
                            .iter()
                            .map(|modifier| u64::from(*modifier) as i64)
                            .collect(),
                    },
                ))),
            });
        }
    }
    properties.extend([
        Property::new(
            spa_sys::SPA_FORMAT_VIDEO_size,
            Value::Rectangle(Rectangle {
                width: spec.width,
                height: spec.height,
            }),
        ),
        Property::new(
            spa_sys::SPA_FORMAT_VIDEO_framerate,
            Value::Fraction(Fraction { num: 0, denom: 1 }),
        ),
        Property::new(
            spa_sys::SPA_FORMAT_VIDEO_maxFramerate,
            Value::Choice(ChoiceValue::Fraction(Choice(
                ChoiceFlags::empty(),
                ChoiceEnum::Range {
                    default: max_framerate,
                    min: Fraction { num: 1, denom: 1 },
                    max: max_framerate,
                },
            ))),
        ),
    ]);
    let obj = Value::Object(Object {
        type_: spa_sys::SPA_TYPE_OBJECT_Format,
        id: spa_sys::SPA_PARAM_EnumFormat,
        properties,
    });
    Ok(PodSerializer::serialize(Cursor::new(Vec::new()), &obj)?
        .0
        .into_inner())
}

struct PipewireModifierParam {
    modifiers: Vec<DrmModifier>,
    dont_fixate: bool,
}

fn parse_pipewire_modifier_param(param: &Pod) -> Option<PipewireModifierParam> {
    let obj = param.as_object().ok()?;
    let prop = obj.find_prop(Id(spa_sys::SPA_FORMAT_VIDEO_modifier))?;
    let dont_fixate = prop.flags().contains(PodPropFlags::DONT_FIXATE);
    let ptr = NonNull::new(prop.value().as_raw_ptr())?;
    let value: Value = unsafe { PodDeserializer::deserialize_ptr(ptr).ok()? };
    let mut modifiers = Vec::new();
    match value {
        Value::Long(modifier) => {
            push_unique_modifier(&mut modifiers, modifier);
        }
        Value::Choice(ChoiceValue::Long(Choice(_, choice))) => match choice {
            ChoiceEnum::None(modifier) => {
                push_unique_modifier(&mut modifiers, modifier);
            }
            ChoiceEnum::Enum {
                default,
                alternatives,
            } => {
                push_unique_modifier(&mut modifiers, default);
                for modifier in alternatives {
                    push_unique_modifier(&mut modifiers, modifier);
                }
            }
            ChoiceEnum::Range { default, .. } | ChoiceEnum::Step { default, .. } => {
                push_unique_modifier(&mut modifiers, default);
            }
            ChoiceEnum::Flags { default, flags } => {
                push_unique_modifier(&mut modifiers, default);
                for modifier in flags {
                    push_unique_modifier(&mut modifiers, modifier);
                }
            }
        },
        _ => return None,
    }
    if modifiers.is_empty() {
        None
    } else {
        Some(PipewireModifierParam {
            modifiers,
            dont_fixate,
        })
    }
}

fn push_unique_modifier(modifiers: &mut Vec<DrmModifier>, modifier: i64) {
    let modifier = DrmModifier::from(modifier as u64);
    if !modifiers.contains(&modifier) {
        modifiers.push(modifier);
    }
}

fn build_buffers_param(
    spec: &StreamSpec,
    prefer_dmabuf: bool,
) -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>> {
    let memfd_flag = 1 << spa_sys::SPA_DATA_MemFd;
    let dmabuf_flag = 1 << spa_sys::SPA_DATA_DmaBuf;
    let mut properties = vec![
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
    ];

    if prefer_dmabuf {
        properties.push(Property::new(
            spa_sys::SPA_PARAM_BUFFERS_dataType,
            Value::Choice(ChoiceValue::Int(Choice(
                ChoiceFlags::empty(),
                ChoiceEnum::Flags {
                    default: dmabuf_flag | memfd_flag,
                    flags: vec![dmabuf_flag, memfd_flag],
                },
            ))),
        ));
    } else {
        let stride = (spec.width * 4) as i32;
        let size = stride * spec.height as i32;
        properties.extend([
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
        ]);
    }

    let obj = Value::Object(Object {
        type_: spa_sys::SPA_TYPE_OBJECT_ParamBuffers,
        id: spa_sys::SPA_PARAM_Buffers,
        properties,
    });
    Ok(PodSerializer::serialize(Cursor::new(Vec::new()), &obj)?
        .0
        .into_inner())
}

/// Advertise a per-buffer `SPA_META_Header` region. Every mainstream portal
/// (KWin, Mutter, xdph) provides this; consumers use the pts/seq for frame
/// pacing, and its absence puts us off the code paths consumers are actually
/// tested against.
pub(crate) fn build_header_meta_param() -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>> {
    let obj = Value::Object(Object {
        type_: spa_sys::SPA_TYPE_OBJECT_ParamMeta,
        id: spa_sys::SPA_PARAM_Meta,
        properties: vec![
            Property::new(
                spa_sys::SPA_PARAM_META_type,
                Value::Id(
                    Id(
                        spa_sys::SPA_META_Header,
                    ),
                ),
            ),
            Property::new(
                spa_sys::SPA_PARAM_META_size,
                Value::Int(
                    size_of::<spa_sys::spa_meta_header>() as i32,
                ),
            ),
        ],
    });
    Ok(PodSerializer::serialize(Cursor::new(Vec::new()), &obj)?
        .0
        .into_inner())
}

/// Fill the negotiated `SPA_META_Header` region on a pw_buffer with a
/// monotonic timestamp and sequence number. `flags` must be explicitly
/// zeroed — a stray `SPA_META_HEADER_FLAG_CORRUPTED` makes consumers drop
/// the frame silently.
pub(crate) unsafe fn fill_header_meta(
    pw_buf: *mut pw::sys::pw_buffer,
    seq: u64,
) {
    unsafe {
        let buf = (*pw_buf).buffer;
        if buf.is_null() || (*buf).metas.is_null() {
            return;
        }
        let metas = std::slice::from_raw_parts_mut(
            (*buf).metas,
            (*buf).n_metas as usize,
        );
        for meta in metas {
            if meta.type_ != spa_sys::SPA_META_Header
                || (meta.size as usize) < size_of::<spa_sys::spa_meta_header>()
                || meta.data.is_null()
            {
                continue;
            }
            let now = rustix::time::clock_gettime(
                rustix::time::ClockId::Monotonic,
            );
            let header = &mut *(
                meta.data as *mut spa_sys::spa_meta_header
            );
            header.flags = 0;
            header.offset = 0;
            header.seq = seq;
            header.pts = now.tv_sec as i64 * 1_000_000_000 + now.tv_nsec as i64;
            header.dts_offset = 0;
        }
    }
}

/// Extract the fixated `maxFramerate` from a negotiated Format param.
pub(crate) fn parse_pipewire_max_framerate(
    param: &Pod,
) -> Option<Fraction> {
    let obj = param
        .as_object()
        .ok()?;
    let prop = obj
        .find_prop(
            Id(
                spa_sys::SPA_FORMAT_VIDEO_maxFramerate,
            ),
        )?;
    let ptr = NonNull::new(
        prop
            .value()
            .as_raw_ptr(),
    )?;
    let value: Value = unsafe {
        PodDeserializer::deserialize_ptr(
            ptr,
        ).ok()?
    };
    match value {
        Value::Fraction(
            fraction,
        ) => Some(
            fraction,
        ),
        Value::Choice(
            ChoiceValue::Fraction(
                Choice(
                    _,
                    choice,
                ),
            ),
        ) => match choice {
            ChoiceEnum::None(
                fraction,
            ) => Some(
                fraction,
            ),
            ChoiceEnum::Range {
                default, ..
            }
            | ChoiceEnum::Step {
                default,
                ..
            }
            | ChoiceEnum::Enum {
                default,
                ..
            }
            | ChoiceEnum::Flags {
                default,
                ..
            } => Some(
                default,
            ),
        },
        _ => None,
    }
}
