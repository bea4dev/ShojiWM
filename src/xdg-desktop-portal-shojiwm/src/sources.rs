//! Source enumeration + one-shot thumbnail capture.
//!
//! A single Wayland session does double duty:
//!
//!   1. Bind `wl_output` and `ext_foreign_toplevel_list_v1` to discover what
//!      can be captured.
//!   2. For each discovered source, run a one-shot `ext-image-copy-capture-v1`
//!      cycle (output_image_capture_source or foreign_toplevel_image_capture_source
//!      → session → buffer constraints → frame → ready) and convert the
//!      captured SHM buffer into an `iced` image handle for the picker UI.
//!
//! Everything is synchronous — `enumerate` and thumbnail capture share one
//! Connection and one EventQueue, all driven by `roundtrip`. Cheap enough
//! for the picker (~100ms for a handful of windows) and dramatically simpler
//! than running a parallel async pipeline. Each `SelectSources` call gets a
//! fresh Wayland connection that's torn down when the function returns.

use std::os::fd::AsFd;
use std::ptr::NonNull;
use std::sync::{Arc, Mutex};

use iced::widget::image;
use rustix::mm::{MapFlags, ProtFlags};
use wayland_client::Proxy;
use wayland_client::protocol::{wl_buffer, wl_output, wl_registry, wl_shm, wl_shm_pool};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, WEnum};
use wayland_protocols::ext::foreign_toplevel_list::v1::client::{
    ext_foreign_toplevel_handle_v1::{self, ExtForeignToplevelHandleV1},
    ext_foreign_toplevel_list_v1::{self, ExtForeignToplevelListV1},
};
use wayland_protocols::ext::image_capture_source::v1::client::{
    ext_foreign_toplevel_image_capture_source_manager_v1::ExtForeignToplevelImageCaptureSourceManagerV1,
    ext_image_capture_source_v1::ExtImageCaptureSourceV1,
    ext_output_image_capture_source_manager_v1::ExtOutputImageCaptureSourceManagerV1,
};
use wayland_protocols::ext::image_copy_capture::v1::client::{
    ext_image_copy_capture_frame_v1::{self, ExtImageCopyCaptureFrameV1},
    ext_image_copy_capture_manager_v1::{self, ExtImageCopyCaptureManagerV1},
    ext_image_copy_capture_session_v1::{self, ExtImageCopyCaptureSessionV1},
};
use wayland_protocols_wlr::foreign_toplevel::v1::client::{
    zwlr_foreign_toplevel_handle_v1::{self, ZwlrForeignToplevelHandleV1},
    zwlr_foreign_toplevel_manager_v1::{self, ZwlrForeignToplevelManagerV1},
};

const MAX_THUMBNAIL_DIM: u32 = 320;

pub mod source_types {
    pub const MONITOR: u32 = 1;
    pub const WINDOW: u32 = 2;
}

#[derive(Debug, Clone)]
pub struct OutputInfo {
    pub name: String,
    pub description: String,
    pub position: (i32, i32),
    /// Current mode dimensions in hardware (untransformed) orientation, as
    /// sent by `wl_output.mode`.
    pub width: i32,
    pub height: i32,
    pub refresh_mhz: i32,
    /// Output rotation/flip from `wl_output.geometry`. Determines the stream
    /// route: wlr-screencopy delivers rotated (scanout-orientation) frames,
    /// so transformed outputs stream through ext-image-copy-capture instead,
    /// which this compositor renders upright.
    pub transform: wl_output::Transform,
}

impl OutputInfo {
    /// Mode dimensions in the transformed (as-displayed) orientation.
    pub fn logical_mode_size(&self) -> (i32, i32) {
        match self.transform {
            wl_output::Transform::_90
            | wl_output::Transform::_270
            | wl_output::Transform::Flipped90
            | wl_output::Transform::Flipped270 => (self.height, self.width),
            _ => (self.width, self.height),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ToplevelInfo {
    pub identifier: String,
    pub title: String,
    pub app_id: String,
}

#[derive(Debug, Clone)]
pub enum SourceKind {
    Output(OutputInfo),
    Toplevel(ToplevelInfo),
}

#[derive(Debug, Clone)]
pub struct SourceInfo {
    pub kind: SourceKind,
    pub thumbnail: Option<image::Handle>,
}

impl SourceInfo {
    /// Stable identifier used to route per-source updates from the
    /// background thumbnail refresh thread back to the picker UI.
    pub fn id(&self) -> &str {
        match &self.kind {
            SourceKind::Output(o) => &o.name,
            SourceKind::Toplevel(t) => &t.identifier,
        }
    }

    pub fn label(&self) -> String {
        match &self.kind {
            SourceKind::Output(o) => {
                if o.description.is_empty() {
                    o.name.clone()
                } else {
                    format!("{} — {}", o.name, o.description)
                }
            }
            SourceKind::Toplevel(t) => {
                if t.app_id.is_empty() {
                    t.title.clone()
                } else if t.title.is_empty() {
                    t.app_id.clone()
                } else {
                    format!("{} — {}", t.app_id, t.title)
                }
            }
        }
    }

    pub fn detail(&self) -> String {
        match &self.kind {
            SourceKind::Output(o) => format!(
                "{}×{} @ {:.2}Hz · ({}, {})",
                o.width,
                o.height,
                o.refresh_mhz as f64 / 1000.0,
                o.position.0,
                o.position.1,
            ),
            SourceKind::Toplevel(t) => {
                if t.identifier.is_empty() {
                    crate::i18n::t("source.window")
                } else {
                    let short = &t.identifier[..t.identifier.len().min(12)];
                    crate::i18n::t_args("source.window_with_id", &[("id", short)])
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------

struct DiscoveredOutput {
    global_name: u32,
    proxy: wl_output::WlOutput,
    info: OutputInfo,
    pending: OutputInfo,
}

struct DiscoveredToplevel {
    proxy: ExtForeignToplevelHandleV1,
    info: ToplevelInfo,
}

#[derive(Default)]
struct DiscoveryState {
    outputs: Vec<DiscoveredOutput>,
    toplevels: Vec<DiscoveredToplevel>,
    toplevel_index: std::collections::HashMap<u32, usize>,

    shm: Option<wl_shm::WlShm>,
    capture_manager: Option<ExtImageCopyCaptureManagerV1>,
    output_source_manager: Option<ExtOutputImageCaptureSourceManagerV1>,
    toplevel_source_manager: Option<ExtForeignToplevelImageCaptureSourceManagerV1>,

    /// Per wlr-foreign-toplevel handle: whether it is activated and the
    /// outputs it is on. ext-foreign-toplevel-list carries neither.
    wlr_toplevels: Vec<(ZwlrForeignToplevelHandleV1, bool, Vec<wl_output::WlOutput>)>,
    focused_output: Option<wl_output::WlOutput>,
}

impl DiscoveryState {
    fn refresh_focused_output(&mut self) {
        self.focused_output = self
            .wlr_toplevels
            .iter()
            .find(|(_, activated, _)| *activated)
            .and_then(|(_, _, outputs)| outputs.first().cloned());
    }
}

struct AppData {
    state: Arc<Mutex<DiscoveryState>>,
    capture: Arc<Mutex<CaptureState>>,
}

#[derive(Default)]
struct CaptureState {
    /// The size + format the compositor wants us to allocate for the *current*
    /// session, filled in as buffer_size / shm_format events arrive.
    constraint_width: u32,
    constraint_height: u32,
    constraint_format: Option<wl_shm::Format>,
    constraints_done: bool,

    /// Set when the in-flight frame resolves.
    frame_ready: bool,
    frame_failed: bool,
}

impl Dispatch<wl_registry::WlRegistry, ()> for AppData {
    fn event(
        _app: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::GlobalRemove { name } = event {
            _app.state.lock().unwrap().outputs.retain(|output| {
                if output.global_name != name {
                    return true;
                }
                if output.proxy.version() >= 3 {
                    output.proxy.release();
                }
                false
            });
            return;
        }
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
                let proxy =
                    registry.bind::<wl_output::WlOutput, _, _>(name, version.min(4), qh, ());
                let info = OutputInfo {
                    name: String::new(),
                    description: String::new(),
                    position: (0, 0),
                    width: 0,
                    height: 0,
                    refresh_mhz: 0,
                    transform: wl_output::Transform::Normal,
                };
                _app.state.lock().unwrap().outputs.push(DiscoveredOutput {
                    global_name: name,
                    proxy,
                    pending: info.clone(),
                    info,
                });
            }
            "wl_shm" => {
                let shm = registry.bind::<wl_shm::WlShm, _, _>(name, version.min(1), qh, ());
                _app.state.lock().unwrap().shm = Some(shm);
            }
            "ext_foreign_toplevel_list_v1" => {
                registry.bind::<ExtForeignToplevelListV1, _, _>(name, version.min(1), qh, ());
            }
            "zwlr_foreign_toplevel_manager_v1" => {
                registry.bind::<ZwlrForeignToplevelManagerV1, _, _>(name, version.min(3), qh, ());
            }
            "ext_image_copy_capture_manager_v1" => {
                let m = registry.bind::<ExtImageCopyCaptureManagerV1, _, _>(
                    name,
                    version.min(1),
                    qh,
                    (),
                );
                _app.state.lock().unwrap().capture_manager = Some(m);
            }
            "ext_output_image_capture_source_manager_v1" => {
                let m = registry.bind::<ExtOutputImageCaptureSourceManagerV1, _, _>(
                    name,
                    version.min(1),
                    qh,
                    (),
                );
                _app.state.lock().unwrap().output_source_manager = Some(m);
            }
            "ext_foreign_toplevel_image_capture_source_manager_v1" => {
                let m = registry.bind::<ExtForeignToplevelImageCaptureSourceManagerV1, _, _>(
                    name,
                    version.min(1),
                    qh,
                    (),
                );
                _app.state.lock().unwrap().toplevel_source_manager = Some(m);
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_output::WlOutput, ()> for AppData {
    fn event(
        app: &mut Self,
        output: &wl_output::WlOutput,
        event: wl_output::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let mut state = app.state.lock().unwrap();
        let Some(entry) = state
            .outputs
            .iter_mut()
            .find(|entry| &entry.proxy == output)
        else {
            return;
        };
        let cur = &mut entry.pending;
        match event {
            wl_output::Event::Name { name } => cur.name = name,
            wl_output::Event::Description { description } => cur.description = description,
            wl_output::Event::Geometry {
                x, y, transform, ..
            } => {
                cur.position = (x, y);
                if let WEnum::Value(transform) = transform {
                    cur.transform = transform;
                }
            }
            wl_output::Event::Mode {
                flags,
                width,
                height,
                refresh,
            } if flags
                .into_result()
                .map(|f| f.contains(wl_output::Mode::Current))
                .unwrap_or(false) =>
            {
                cur.width = width;
                cur.height = height;
                cur.refresh_mhz = refresh;
            }
            wl_output::Event::Done => {
                entry.info = entry.pending.clone();
            }
            _ => {}
        }
    }
}

impl Dispatch<ExtForeignToplevelListV1, ()> for AppData {
    fn event(
        _app: &mut Self,
        _: &ExtForeignToplevelListV1,
        _event: ext_foreign_toplevel_list_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }

    wayland_client::event_created_child!(AppData, ExtForeignToplevelListV1, [
        ext_foreign_toplevel_list_v1::EVT_TOPLEVEL_OPCODE => (ExtForeignToplevelHandleV1, ()),
    ]);
}

impl Dispatch<ExtForeignToplevelHandleV1, ()> for AppData {
    fn event(
        app: &mut Self,
        proxy: &ExtForeignToplevelHandleV1,
        event: ext_foreign_toplevel_handle_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let id = proxy.id().protocol_id();
        let mut state = app.state.lock().unwrap();
        let idx = if let Some(&existing) = state.toplevel_index.get(&id) {
            existing
        } else {
            state.toplevels.push(DiscoveredToplevel {
                proxy: proxy.clone(),
                info: ToplevelInfo {
                    identifier: String::new(),
                    title: String::new(),
                    app_id: String::new(),
                },
            });
            let new_idx = state.toplevels.len() - 1;
            state.toplevel_index.insert(id, new_idx);
            new_idx
        };
        match event {
            ext_foreign_toplevel_handle_v1::Event::Title { title } => {
                state.toplevels[idx].info.title = title;
            }
            ext_foreign_toplevel_handle_v1::Event::AppId { app_id } => {
                state.toplevels[idx].info.app_id = app_id;
            }
            ext_foreign_toplevel_handle_v1::Event::Identifier { identifier } => {
                state.toplevels[idx].info.identifier = identifier;
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
                proxy.destroy();
            }
            _ => {}
        }
    }
}

impl Dispatch<ZwlrForeignToplevelManagerV1, ()> for AppData {
    fn event(
        _: &mut Self,
        _: &ZwlrForeignToplevelManagerV1,
        _: zwlr_foreign_toplevel_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }

    wayland_client::event_created_child!(AppData, ZwlrForeignToplevelManagerV1, [
        zwlr_foreign_toplevel_manager_v1::EVT_TOPLEVEL_OPCODE => (ZwlrForeignToplevelHandleV1, ()),
    ]);
}

impl Dispatch<ZwlrForeignToplevelHandleV1, ()> for AppData {
    fn event(
        app: &mut Self,
        proxy: &ZwlrForeignToplevelHandleV1,
        event: zwlr_foreign_toplevel_handle_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let mut state = app.state.lock().unwrap();
        let index = match state.wlr_toplevels.iter().position(|(h, _, _)| h == proxy) {
            Some(index) => index,
            None => {
                state.wlr_toplevels.push((proxy.clone(), false, Vec::new()));
                state.wlr_toplevels.len() - 1
            }
        };
        match event {
            zwlr_foreign_toplevel_handle_v1::Event::State { state: states } => {
                let activated = zwlr_foreign_toplevel_handle_v1::State::Activated as u32;
                state.wlr_toplevels[index].1 = states.chunks_exact(4).any(|chunk| {
                    u32::from_ne_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]) == activated
                });
            }
            zwlr_foreign_toplevel_handle_v1::Event::OutputEnter { output } => {
                state.wlr_toplevels[index].2.push(output);
            }
            zwlr_foreign_toplevel_handle_v1::Event::OutputLeave { output } => {
                state.wlr_toplevels[index].2.retain(|o| *o != output);
            }
            zwlr_foreign_toplevel_handle_v1::Event::Closed => {
                state.wlr_toplevels.remove(index);
                proxy.destroy();
            }
            _ => {}
        }
        state.refresh_focused_output();
    }
}

// Empty/no-op dispatch for everything else we touch.
macro_rules! empty_dispatch {
    ($t:ty) => {
        impl Dispatch<$t, ()> for AppData {
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
empty_dispatch!(ExtOutputImageCaptureSourceManagerV1);
empty_dispatch!(ExtForeignToplevelImageCaptureSourceManagerV1);
empty_dispatch!(ExtImageCaptureSourceV1);

impl Dispatch<ExtImageCopyCaptureSessionV1, ()> for AppData {
    fn event(
        app: &mut Self,
        _: &ExtImageCopyCaptureSessionV1,
        event: ext_image_copy_capture_session_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let mut c = app.capture.lock().unwrap();
        match event {
            ext_image_copy_capture_session_v1::Event::BufferSize { width, height } => {
                c.constraint_width = width;
                c.constraint_height = height;
            }
            ext_image_copy_capture_session_v1::Event::ShmFormat {
                format: WEnum::Value(f),
            } => {
                // Prefer Xrgb8888 / Argb8888 over more exotic ones.
                let is_preferred = matches!(f, wl_shm::Format::Xrgb8888 | wl_shm::Format::Argb8888);
                if c.constraint_format.is_none() || is_preferred {
                    c.constraint_format = Some(f);
                }
            }
            ext_image_copy_capture_session_v1::Event::Done => {
                c.constraints_done = true;
            }
            ext_image_copy_capture_session_v1::Event::Stopped => {
                c.frame_failed = true;
            }
            _ => {}
        }
    }
}

impl Dispatch<ExtImageCopyCaptureFrameV1, ()> for AppData {
    fn event(
        app: &mut Self,
        _: &ExtImageCopyCaptureFrameV1,
        event: ext_image_copy_capture_frame_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let mut c = app.capture.lock().unwrap();
        match event {
            ext_image_copy_capture_frame_v1::Event::Ready => c.frame_ready = true,
            ext_image_copy_capture_frame_v1::Event::Failed { .. } => c.frame_failed = true,
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------

fn capture_output_thumbnail(
    event_queue: &mut EventQueue<AppData>,
    app: &mut AppData,
    qh: &QueueHandle<AppData>,
    output: &wl_output::WlOutput,
) -> Option<image::Handle> {
    let (manager, output_mgr) = {
        let s = app.state.lock().unwrap();
        (s.capture_manager.clone()?, s.output_source_manager.clone()?)
    };
    let source = output_mgr.create_source(output, qh, ());
    let thumbnail = capture_via_source(
        event_queue,
        app,
        qh,
        &manager,
        &source,
        THUMBNAIL_PATIENCE,
        decode_thumbnail,
    );
    source.destroy();
    thumbnail
}

fn capture_toplevel_thumbnail(
    event_queue: &mut EventQueue<AppData>,
    app: &mut AppData,
    qh: &QueueHandle<AppData>,
    toplevel: &ExtForeignToplevelHandleV1,
) -> Option<image::Handle> {
    let (manager, toplevel_mgr) = {
        let s = app.state.lock().unwrap();
        (
            s.capture_manager.clone()?,
            s.toplevel_source_manager.clone()?,
        )
    };
    let source = toplevel_mgr.create_source(toplevel, qh, ());
    let thumbnail = capture_via_source(
        event_queue,
        app,
        qh,
        &manager,
        &source,
        THUMBNAIL_PATIENCE,
        decode_thumbnail,
    );
    source.destroy();
    thumbnail
}

/// How long a thumbnail capture may take before the picker moves on.
const THUMBNAIL_PATIENCE: std::time::Duration = std::time::Duration::from_millis(100);
/// The compositor completes a frame at the output's next render pass, which
/// can be a few vblanks away while something animates.
const CAPTURE_PATIENCE: std::time::Duration = std::time::Duration::from_secs(2);

/// Run one capture cycle against a freshly-created `ExtImageCaptureSourceV1`
/// and hand the captured SHM bytes to `decode`. `None` on any failure path,
/// including no frame within `patience`.
fn capture_via_source<T>(
    event_queue: &mut EventQueue<AppData>,
    app: &mut AppData,
    qh: &QueueHandle<AppData>,
    manager: &ExtImageCopyCaptureManagerV1,
    source: &ExtImageCaptureSourceV1,
    patience: std::time::Duration,
    decode: impl FnOnce(&[u8], u32, u32, wl_shm::Format) -> Option<T>,
) -> Option<T> {
    // Reset per-capture state.
    {
        let mut c = app.capture.lock().unwrap();
        *c = CaptureState::default();
    }

    let session = manager.create_session(
        source,
        ext_image_copy_capture_manager_v1::Options::empty(),
        qh,
        (),
    );

    // Drain buffer_size / shm_format / done.
    for _ in 0..8 {
        if event_queue.roundtrip(app).is_err() {
            session.destroy();
            return None;
        }
        if app.capture.lock().unwrap().constraints_done {
            break;
        }
    }
    let (width, height, format) = {
        let c = app.capture.lock().unwrap();
        if !c.constraints_done {
            tracing::debug!("thumbnail: constraints never finalized");
            session.destroy();
            return None;
        }
        (c.constraint_width, c.constraint_height, c.constraint_format)
    };
    if width == 0 || height == 0 {
        session.destroy();
        return None;
    }
    let format = format.unwrap_or(wl_shm::Format::Xrgb8888);
    let Some(stride) = width
        .checked_mul(4)
        .and_then(|stride| i32::try_from(stride).ok())
    else {
        session.destroy();
        return None;
    };
    let size = stride as usize * height as usize;

    // Allocate a memfd-backed buffer the compositor can write into.
    let Some(buffer_artifacts) = allocate_shm_buffer(app, qh, width, height, stride, format) else {
        session.destroy();
        return None;
    };
    let (wl_buffer, ptr, pool, _memfd) = buffer_artifacts;

    let frame = session.create_frame(qh, ());
    frame.attach_buffer(&wl_buffer);
    frame.capture();

    // Drain until ready/failed (or we give up).
    let deadline = std::time::Instant::now() + patience;
    let outcome = loop {
        if event_queue.roundtrip(app).is_err() {
            break false;
        }
        let c = app.capture.lock().unwrap();
        if c.frame_ready {
            break true;
        }
        if c.frame_failed {
            break false;
        }
        drop(c);
        if std::time::Instant::now() >= deadline {
            break false;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    };

    let decoded = if outcome {
        // SAFETY: `ptr` maps `size` bytes of the memfd the compositor wrote
        // into; `ready` means the write is complete.
        let bytes = unsafe { std::slice::from_raw_parts(ptr.as_ptr(), size) };
        decode(bytes, width, height, format)
    } else {
        None
    };

    frame.destroy();
    session.destroy();
    wl_buffer.destroy();
    pool.destroy();
    if let Err(error) = unsafe { rustix::mm::munmap(ptr.as_ptr().cast(), size) } {
        tracing::warn!(%error, "failed to unmap capture buffer");
    }

    decoded
}

/// (wl_buffer, mmap ptr, pool, memfd) — all four must stay alive together.
fn allocate_shm_buffer(
    app: &mut AppData,
    qh: &QueueHandle<AppData>,
    width: u32,
    height: u32,
    stride: i32,
    format: wl_shm::Format,
) -> Option<(
    wl_buffer::WlBuffer,
    NonNull<u8>,
    wl_shm_pool::WlShmPool,
    std::os::fd::OwnedFd,
)> {
    let shm = app.state.lock().unwrap().shm.clone()?;
    let size = stride as usize * height as usize;
    let pool_size = i32::try_from(size).ok()?;
    let memfd =
        rustix::fs::memfd_create("shojiwm-portal-capture", rustix::fs::MemfdFlags::CLOEXEC).ok()?;
    rustix::fs::ftruncate(&memfd, size as u64).ok()?;
    let ptr = unsafe {
        rustix::mm::mmap(
            std::ptr::null_mut(),
            size,
            ProtFlags::READ,
            MapFlags::SHARED,
            memfd.as_fd(),
            0,
        )
    }
    .ok()?;
    let ptr = NonNull::new(ptr.cast::<u8>())?;
    let pool = shm.create_pool(memfd.as_fd(), pool_size, qh, ());
    let buffer = pool.create_buffer(0, width as i32, height as i32, stride, format, qh, ());
    Some((buffer, ptr, pool, memfd))
}

/// Red, green and blue of the pixel at byte offset `i` of an SHM buffer.
fn pixel_rgb(bytes: &[u8], i: usize, format: wl_shm::Format) -> (u8, u8, u8) {
    match format {
        wl_shm::Format::Xbgr8888 | wl_shm::Format::Abgr8888 => {
            (bytes[i], bytes[i + 1], bytes[i + 2])
        }
        // Xrgb8888 / Argb8888, the formats preferred above, are B, G, R, X|A
        // in memory.
        _ => (bytes[i + 2], bytes[i + 1], bytes[i]),
    }
}

/// Convert the SHM buffer into an iced RGBA image, downscaled to at most
/// `MAX_THUMBNAIL_DIM` on the longest edge to keep widget memory bounded.
fn decode_thumbnail(
    bytes: &[u8],
    width: u32,
    height: u32,
    format: wl_shm::Format,
) -> Option<image::Handle> {
    // Compute downscale factor (integer nearest-neighbour).
    let longest = width.max(height);
    let factor = longest.div_ceil(MAX_THUMBNAIL_DIM).max(1);
    let dst_w = (width / factor).max(1);
    let dst_h = (height / factor).max(1);

    let mut rgba = vec![0u8; (dst_w * dst_h * 4) as usize];
    let stride = (width * 4) as usize;
    for y in 0..dst_h {
        let src_y = (y * factor) as usize;
        for x in 0..dst_w {
            let src_x = (x * factor) as usize;
            let i = src_y * stride + src_x * 4;
            if i + 3 >= bytes.len() {
                continue;
            }
            let (r, g, b) = pixel_rgb(bytes, i, format);
            let o = ((y * dst_w + x) * 4) as usize;
            rgba[o] = r;
            rgba[o + 1] = g;
            rgba[o + 2] = b;
            rgba[o + 3] = 0xff;
        }
    }
    Some(image::Handle::from_rgba(dst_w, dst_h, rgba))
}

/// A full-resolution capture, opaque RGBA rows top to bottom.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capture {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

fn decode_full(bytes: &[u8], width: u32, height: u32, format: wl_shm::Format) -> Option<Capture> {
    let pixels = width as usize * height as usize;
    if bytes.len() < pixels * 4 {
        return None;
    }
    let mut rgba = Vec::with_capacity(pixels * 4);
    for i in (0..pixels * 4).step_by(4) {
        let (r, g, b) = pixel_rgb(bytes, i, format);
        rgba.extend_from_slice(&[r, g, b, 0xff]);
    }
    Some(Capture {
        width,
        height,
        rgba,
    })
}

/// Which outputs a one-shot capture takes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutputTarget {
    /// The output of the focused window, or the first output (by position)
    /// when no window has focus.
    Focused,
    /// Every output, by position.
    All,
}

/// Capture outputs at full resolution over a fresh connection, each with its
/// name.
pub fn capture_outputs(target: &OutputTarget) -> Result<Vec<(String, Capture)>, String> {
    let conn = Connection::connect_to_env().map_err(|e| format!("wayland connect: {e}"))?;
    let mut event_queue: EventQueue<AppData> = conn.new_event_queue();
    let qh = event_queue.handle();
    let _registry = conn.display().get_registry(&qh, ());
    let mut app = AppData {
        state: Arc::new(Mutex::new(DiscoveryState::default())),
        capture: Arc::new(Mutex::new(CaptureState::default())),
    };
    // Globals, then their initial events (output names, toplevel state).
    for _ in 0..3 {
        event_queue
            .roundtrip(&mut app)
            .map_err(|e| format!("wayland roundtrip: {e}"))?;
    }
    let (chosen, manager, output_mgr) = {
        let s = app.state.lock().unwrap();
        let outputs: Vec<(String, (i32, i32), wl_output::WlOutput)> = s
            .outputs
            .iter()
            .filter(|o| !o.info.name.is_empty())
            .map(|o| (o.info.name.clone(), o.info.position, o.proxy.clone()))
            .collect();
        let focused = s.focused_output.as_ref().and_then(|focused| {
            outputs
                .iter()
                .find(|(_, _, proxy)| proxy == focused)
                .map(|(name, _, _)| name.clone())
        });
        let listed: Vec<(String, (i32, i32))> = outputs
            .iter()
            .map(|(name, position, _)| (name.clone(), *position))
            .collect();
        let chosen: Vec<(String, wl_output::WlOutput)> =
            choose_outputs(target, &listed, focused.as_deref())
                .into_iter()
                .filter_map(|name| {
                    outputs
                        .iter()
                        .find(|(n, _, _)| *n == name)
                        .map(|(_, _, proxy)| (name, proxy.clone()))
                })
                .collect();
        (
            chosen,
            s.capture_manager
                .clone()
                .ok_or("compositor lacks ext_image_copy_capture_manager_v1")?,
            s.output_source_manager
                .clone()
                .ok_or("compositor lacks ext_output_image_capture_source_manager_v1")?,
        )
    };
    if chosen.is_empty() {
        return Err("no output to capture".to_owned());
    }
    chosen
        .into_iter()
        .map(|(name, output)| {
            let source = output_mgr.create_source(&output, &qh, ());
            let capture = capture_via_source(
                &mut event_queue,
                &mut app,
                &qh,
                &manager,
                &source,
                CAPTURE_PATIENCE,
                decode_full,
            );
            source.destroy();
            capture
                .map(|capture| (name.clone(), capture))
                .ok_or_else(|| format!("capturing {name} failed"))
        })
        .collect()
}

/// The names of the outputs `target` means among `outputs` (name, position).
fn choose_outputs(
    target: &OutputTarget,
    outputs: &[(String, (i32, i32))],
    focused: Option<&str>,
) -> Vec<String> {
    let mut by_position = outputs.to_vec();
    by_position.sort_by(|a, b| (a.1, &a.0).cmp(&(b.1, &b.0)));
    let names = by_position.into_iter().map(|(name, _)| name);
    match target {
        OutputTarget::All => names.collect(),
        OutputTarget::Focused => focused
            .map(str::to_owned)
            .into_iter()
            .chain(names)
            .take(1)
            .collect(),
    }
}

// ---------------------------------------------------------------------------
// Live refresh ("Lv.2")
// ---------------------------------------------------------------------------

/// A current source list, including the latest available thumbnails.
#[derive(Debug, Clone)]
pub struct SourceUpdate {
    pub session_key: String,
    pub sources: Vec<SourceInfo>,
}

/// Handle to the background thumbnail-refresh thread. Drop it to stop.
pub struct ThumbnailStream {
    stop: Arc<std::sync::atomic::AtomicBool>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl Drop for ThumbnailStream {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

/// Spawn a background thread that:
///   1. enumerates all sources + captures their first thumbnail,
///   2. sends the initial `Vec<SourceInfo>` through `init_tx`,
///   3. then round-robins through the sources, pushing current source lists on
///      `updates_tx` until the returned `ThumbnailStream` is dropped.
///
/// One wayland Connection lives on the thread for its whole lifetime —
/// per-source sessions are created and destroyed per refresh because
/// `ext-image-copy-capture-v1` sessions are themselves not long-lived
/// streaming primitives; they're billed per frame.
pub fn start_thumbnail_stream(
    session_key: String,
    requested: u32,
    init_tx: tokio::sync::oneshot::Sender<Vec<SourceInfo>>,
    updates_tx: tokio::sync::mpsc::Sender<SourceUpdate>,
) -> ThumbnailStream {
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop_for_thread = stop.clone();
    let join = std::thread::Builder::new()
        .name("portal-thumbnails".into())
        .spawn(move || {
            if let Err(e) = run_stream(session_key, requested, init_tx, updates_tx, stop_for_thread)
            {
                tracing::warn!("thumbnail stream exited: {e}");
            }
        })
        .expect("spawn thumbnail thread");
    ThumbnailStream {
        stop,
        join: Some(join),
    }
}

fn run_stream(
    session_key: String,
    requested: u32,
    init_tx: tokio::sync::oneshot::Sender<Vec<SourceInfo>>,
    updates_tx: tokio::sync::mpsc::Sender<SourceUpdate>,
    stop: Arc<std::sync::atomic::AtomicBool>,
) -> Result<(), Box<dyn std::error::Error>> {
    let conn = Connection::connect_to_env()?;
    let display = conn.display();
    let mut event_queue: EventQueue<AppData> = conn.new_event_queue();
    let qh = event_queue.handle();
    let _registry = display.get_registry(&qh, ());

    let state = Arc::new(Mutex::new(DiscoveryState::default()));
    let capture = Arc::new(Mutex::new(CaptureState::default()));
    let mut app = AppData {
        state: state.clone(),
        capture: capture.clone(),
    };
    for _ in 0..3 {
        event_queue.roundtrip(&mut app)?;
    }

    // Rebuild this list after dispatching events so open/close, title changes
    // and output hotplug reach the picker while it is open.
    enum LiveSource {
        Output {
            proxy: wl_output::WlOutput,
            info: SourceInfo,
        },
        Toplevel {
            proxy: ExtForeignToplevelHandleV1,
            info: SourceInfo,
        },
    }
    impl LiveSource {
        fn info(&self) -> &SourceInfo {
            match self {
                Self::Output { info, .. } | Self::Toplevel { info, .. } => info,
            }
        }
    }
    let discover = |thumbnails: &std::collections::HashMap<String, image::Handle>| {
        let s = state.lock().unwrap();
        let mut v: Vec<LiveSource> = Vec::new();
        if requested & source_types::MONITOR != 0 {
            let mut outputs: Vec<_> = s
                .outputs
                .iter()
                .filter(|o| !o.info.name.is_empty() && o.info.width > 0 && o.info.height > 0)
                .collect();
            outputs.sort_by(|a, b| {
                (a.info.position, &a.info.name).cmp(&(b.info.position, &b.info.name))
            });
            for o in outputs {
                v.push(LiveSource::Output {
                    proxy: o.proxy.clone(),
                    info: SourceInfo {
                        kind: SourceKind::Output(o.info.clone()),
                        thumbnail: thumbnails.get(&o.info.name).cloned(),
                    },
                });
            }
        }
        if requested & source_types::WINDOW != 0 {
            for t in &s.toplevels {
                if t.info.identifier.is_empty() {
                    continue;
                }
                v.push(LiveSource::Toplevel {
                    proxy: t.proxy.clone(),
                    info: SourceInfo {
                        kind: SourceKind::Toplevel(t.info.clone()),
                        thumbnail: thumbnails.get(&t.info.identifier).cloned(),
                    },
                });
            }
        }
        v
    };

    let mut thumbnails = std::collections::HashMap::new();
    let initial = discover(&thumbnails)
        .iter()
        .map(|src| src.info().clone())
        .collect();
    if init_tx.send(initial).is_err() {
        return Ok(());
    }

    // Round-robin refresh. Short sleep between captures keeps CPU bounded
    // and avoids starving the compositor / cursor when many windows are
    // present. Per-source rate ≈ 1 / (N_sources × (per_capture + tick))
    // — typical numbers: 1 source ~10fps, 5 sources ~2-3fps. Adjust if the
    // picker feels sluggish vs. too hot.
    let tick = std::time::Duration::from_millis(50);
    let mut cursor = 0usize;
    while !stop.load(std::sync::atomic::Ordering::SeqCst) {
        event_queue.roundtrip(&mut app)?;
        let live_sources = discover(&thumbnails);
        if !live_sources.is_empty() {
            let src = &live_sources[cursor % live_sources.len()];
            cursor = cursor.wrapping_add(1);
            let handle = match src {
                LiveSource::Output { proxy, .. } => {
                    capture_output_thumbnail(&mut event_queue, &mut app, &qh, proxy)
                }
                LiveSource::Toplevel { proxy, .. } => {
                    capture_toplevel_thumbnail(&mut event_queue, &mut app, &qh, proxy)
                }
            };
            if let Some(handle) = handle {
                thumbnails.insert(src.info().id().to_owned(), handle);
            }
        }
        let sources: Vec<_> = discover(&thumbnails)
            .iter()
            .map(|src| src.info().clone())
            .collect();
        thumbnails.retain(|id, _| sources.iter().any(|src| src.id() == id));
        match updates_tx.try_send(SourceUpdate {
            session_key: session_key.clone(),
            sources,
        }) {
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => break,
            // Keep only a bounded number of snapshots if the UI is busy.
            Ok(()) | Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {}
        }
        std::thread::sleep(tick);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outputs() -> Vec<(String, (i32, i32))> {
        vec![("DP-2".into(), (1920, 0)), ("eDP-1".into(), (0, 0))]
    }

    #[test]
    fn all_outputs_come_by_position() {
        assert_eq!(
            choose_outputs(&OutputTarget::All, &outputs(), Some("DP-2")),
            ["eDP-1", "DP-2"]
        );
    }

    #[test]
    fn the_focused_output_falls_back_to_the_first_by_position() {
        let focused = OutputTarget::Focused;
        assert_eq!(choose_outputs(&focused, &outputs(), Some("DP-2")), ["DP-2"]);
        assert_eq!(choose_outputs(&focused, &outputs(), None), ["eDP-1"]);
        assert!(choose_outputs(&focused, &[], None).is_empty());
    }

    #[test]
    fn full_captures_are_opaque_rgba() {
        // One pixel each of red and blue, as B G R X in memory.
        let xrgb = [0, 0, 255, 0, 255, 0, 0, 0];
        let capture = decode_full(&xrgb, 2, 1, wl_shm::Format::Xrgb8888).unwrap();
        assert_eq!(capture.rgba, [255, 0, 0, 255, 0, 0, 255, 255]);

        let xbgr = [255, 0, 0, 0, 0, 0, 255, 0];
        let capture = decode_full(&xbgr, 2, 1, wl_shm::Format::Xbgr8888).unwrap();
        assert_eq!(capture.rgba, [255, 0, 0, 255, 0, 0, 255, 255]);

        assert_eq!(decode_full(&xrgb, 2, 2, wl_shm::Format::Xrgb8888), None);
    }
}
