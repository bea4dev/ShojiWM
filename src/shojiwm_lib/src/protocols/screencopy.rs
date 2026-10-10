use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::backend::allocator::{Buffer, Fourcc};
use smithay::backend::renderer::damage::OutputDamageTracker;
use smithay::backend::renderer::sync::SyncPoint;
use smithay::output::Output;
use smithay::reexports::calloop::generic::Generic;
use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay::reexports::calloop::{Interest, LoopHandle, Mode, PostAction};
use smithay::reexports::wayland_protocols_wlr::screencopy::v1::server::{
    zwlr_screencopy_frame_v1, zwlr_screencopy_manager_v1,
};
use smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer;
use smithay::reexports::wayland_server::protocol::wl_shm::Format;
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
};
use smithay::utils::{Physical, Point, Rectangle, Size, Transform};
use smithay::wayland::{dmabuf, shm};
use tracing::{error, trace};
use zwlr_screencopy_frame_v1::{Flags, ZwlrScreencopyFrameV1};
use zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1;

const VERSION: u32 = 3;

pub struct ScreencopyQueue {
    damage_tracker: OutputDamageTracker,
    pending_frames: HashSet<ZwlrScreencopyFrameV1>,
    screencopies: Vec<Screencopy>,
    /// Vblank of the last frame served to a pipelined client, so a second
    /// render for the same vblank doesn't hand out a duplicate frame.
    pub last_served_target: Option<Duration>,
    /// Set once the client has had more than one copy queued at a time;
    /// sticky, since right after a frame is served only one may be left.
    pipelined: bool,
    /// Output of the last frame served; lets a render notice that a
    /// pipelined client capturing it has no copy queued (diagnostics).
    pub last_output: Option<Output>,
}

impl ScreencopyQueue {
    pub fn new() -> Self {
        Self {
            damage_tracker: OutputDamageTracker::new((0, 0), 1.0, Transform::Normal),
            pending_frames: HashSet::new(),
            screencopies: Vec::new(),
            last_served_target: None,
            pipelined: false,
            last_output: None,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.pending_frames.is_empty() && self.screencopies.is_empty()
    }

    pub fn split(&mut self) -> (&mut OutputDamageTracker, Option<&Screencopy>) {
        let ScreencopyQueue {
            damage_tracker,
            screencopies,
            ..
        } = self;
        (damage_tracker, screencopies.first())
    }

    pub fn push(&mut self, screencopy: Screencopy) {
        self.screencopies.push(screencopy);
        if self.screencopies.len() > 1 {
            self.pipelined = true;
        }
    }

    /// Whether the client keeps several captures in flight instead of
    /// waiting for each `ready`.
    pub fn is_pipelined(&self) -> bool {
        self.pipelined
    }

    pub fn pop(&mut self) -> Screencopy {
        self.screencopies.remove(0)
    }

    fn remove_output(&mut self, output: &Output) {
        self.screencopies
            .retain(|screencopy| screencopy.output() != output);
    }

    fn remove_frame(&mut self, frame: &ZwlrScreencopyFrameV1) {
        self.pending_frames.remove(frame);
        self.screencopies
            .retain(|screencopy| screencopy.frame != *frame);
    }
}

impl Default for ScreencopyQueue {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Default)]
pub struct ScreencopyManagerState {
    queues: HashMap<ZwlrScreencopyManagerV1, ScreencopyQueue>,
}

pub struct ScreencopyManagerGlobalData {
    filter: Box<dyn for<'c> Fn(&'c Client) -> bool + Send + Sync>,
}

impl ScreencopyManagerState {
    pub fn new<D, F>(display: &DisplayHandle, filter: F) -> Self
    where
        D: GlobalDispatch<ZwlrScreencopyManagerV1, ScreencopyManagerGlobalData>,
        D: Dispatch<ZwlrScreencopyManagerV1, ()>,
        D: Dispatch<ZwlrScreencopyFrameV1, ScreencopyFrameState>,
        D: ScreencopyHandler,
        D: 'static,
        F: for<'c> Fn(&'c Client) -> bool + Send + Sync + 'static,
    {
        let global_data = ScreencopyManagerGlobalData {
            filter: Box::new(filter),
        };
        display.create_global::<D, ZwlrScreencopyManagerV1, _>(VERSION, global_data);

        Self {
            queues: HashMap::new(),
        }
    }

    pub fn push(&mut self, manager: &ZwlrScreencopyManagerV1, screencopy: Screencopy) {
        let Some(queue) = self.queues.get_mut(manager) else {
            error!("screencopy queue must not be deleted as long as frames exist");
            return;
        };
        queue.push(screencopy);
    }

    pub fn damage_tracker(
        &mut self,
        manager: &ZwlrScreencopyManagerV1,
    ) -> Option<&mut OutputDamageTracker> {
        let queue = self.queues.get_mut(manager)?;
        Some(&mut queue.damage_tracker)
    }

    pub fn remove_output(&mut self, output: &Output) {
        for queue in self.queues.values_mut() {
            queue.remove_output(output);
        }
        self.cleanup_queues();
    }

    pub fn with_queues_mut(&mut self, mut f: impl FnMut(&mut ScreencopyQueue)) {
        for queue in self.queues.values_mut() {
            f(queue);
        }
        self.cleanup_queues();
    }

    pub fn has_screencopy_for_output(&self, output: &Output) -> bool {
        self.queues.values().any(|queue| {
            queue
                .screencopies
                .iter()
                .any(|screencopy| screencopy.output() == output)
        })
    }

    fn cleanup_queues(&mut self) {
        self.queues
            .retain(|manager, queue| manager.is_alive() || !queue.is_empty());
    }
}

impl<D> GlobalDispatch<ZwlrScreencopyManagerV1, ScreencopyManagerGlobalData, D>
    for ScreencopyManagerState
where
    D: GlobalDispatch<ZwlrScreencopyManagerV1, ScreencopyManagerGlobalData>,
    D: Dispatch<ZwlrScreencopyManagerV1, ()>,
    D: Dispatch<ZwlrScreencopyFrameV1, ScreencopyFrameState>,
    D: ScreencopyHandler,
    D: 'static,
{
    fn bind(
        state: &mut D,
        _dh: &DisplayHandle,
        _client: &Client,
        manager: New<ZwlrScreencopyManagerV1>,
        _manager_state: &ScreencopyManagerGlobalData,
        data_init: &mut DataInit<'_, D>,
    ) {
        let manager = data_init.init(manager, ());
        let state = state.screencopy_state();
        state.queues.insert(manager.clone(), ScreencopyQueue::new());
    }

    fn can_view(client: Client, global_data: &ScreencopyManagerGlobalData) -> bool {
        (global_data.filter)(&client)
    }
}

impl<D> Dispatch<ZwlrScreencopyManagerV1, (), D> for ScreencopyManagerState
where
    D: GlobalDispatch<ZwlrScreencopyManagerV1, ScreencopyManagerGlobalData>,
    D: Dispatch<ZwlrScreencopyManagerV1, ()>,
    D: Dispatch<ZwlrScreencopyFrameV1, ScreencopyFrameState>,
    D: ScreencopyHandler,
    D: 'static,
{
    fn request(
        state: &mut D,
        _client: &Client,
        manager: &ZwlrScreencopyManagerV1,
        request: zwlr_screencopy_manager_v1::Request,
        _data: &(),
        _display: &DisplayHandle,
        data_init: &mut DataInit<'_, D>,
    ) {
        if crate::env_flag!("SHOJI_SCREENCOPY_PROFILE")
            && matches!(
                request,
                zwlr_screencopy_manager_v1::Request::CaptureOutput { .. }
                    | zwlr_screencopy_manager_v1::Request::CaptureOutputRegion { .. }
            )
        {
            tracing::info!("screencopy: capture_output request received from client");
        }
        let (frame, overlay_cursor, buffer_size, region_loc, output) = match request {
            zwlr_screencopy_manager_v1::Request::CaptureOutput {
                frame,
                overlay_cursor,
                output,
            } => {
                let Some(output) = Output::from_resource(&output) else {
                    trace!("screencopy client requested non-existent output");
                    let frame = data_init.init(frame, ScreencopyFrameState::Failed);
                    frame.failed();
                    return;
                };
                if !state.screencopy_output_enabled(&output) {
                    trace!(output = %output.name(), "screencopy client requested disabled output");
                    let frame = data_init.init(frame, ScreencopyFrameState::Failed);
                    frame.failed();
                    return;
                }

                let buffer_size = output.current_mode().unwrap().size;
                let region_loc = Point::from((0, 0));

                (frame, overlay_cursor, buffer_size, region_loc, output)
            }
            zwlr_screencopy_manager_v1::Request::CaptureOutputRegion {
                frame,
                overlay_cursor,
                x,
                y,
                width,
                height,
                output,
            } => {
                if width <= 0 || height <= 0 {
                    trace!("screencopy client requested invalid sized region");
                    let frame = data_init.init(frame, ScreencopyFrameState::Failed);
                    frame.failed();
                    return;
                }

                let Some(output) = Output::from_resource(&output) else {
                    trace!("screencopy client requested non-existent output");
                    let frame = data_init.init(frame, ScreencopyFrameState::Failed);
                    frame.failed();
                    return;
                };
                if !state.screencopy_output_enabled(&output) {
                    trace!(output = %output.name(), "screencopy client requested disabled output");
                    let frame = data_init.init(frame, ScreencopyFrameState::Failed);
                    frame.failed();
                    return;
                }

                let output_transform = output.current_transform();
                let output_physical_size =
                    output_transform.transform_size(output.current_mode().unwrap().size);
                let output_rect = Rectangle::from_size(output_physical_size);

                let rect = Rectangle::new(Point::from((x, y)), Size::from((width, height)));

                let output_scale = output.current_scale().fractional_scale();
                let physical_rect = rect.to_physical_precise_round(output_scale);

                let Some(clamped_rect) = physical_rect.intersection(output_rect) else {
                    trace!("screencopy client requested region outside of output");
                    let frame = data_init.init(frame, ScreencopyFrameState::Failed);
                    frame.failed();
                    return;
                };

                let untransformed_rect = output_transform
                    .invert()
                    .transform_rect_in(clamped_rect, &output_physical_size);

                (
                    frame,
                    overlay_cursor,
                    untransformed_rect.size,
                    clamped_rect.loc,
                    output,
                )
            }
            zwlr_screencopy_manager_v1::Request::Destroy => return,
            _ => unreachable!(),
        };

        let overlay_cursor = overlay_cursor != 0;
        let info = ScreencopyFrameInfo {
            output,
            overlay_cursor,
            buffer_size,
            region_loc,
        };
        let frame = data_init.init(
            frame,
            ScreencopyFrameState::Pending {
                manager: manager.clone(),
                info,
                copied: Arc::new(AtomicBool::new(false)),
            },
        );

        // Send desired SHM buffer parameters.
        frame.buffer(
            Format::Xrgb8888,
            buffer_size.w as u32,
            buffer_size.h as u32,
            buffer_size.w as u32 * 4,
        );

        if frame.version() >= 3 {
            // Conditionally advertise DMA-BUF. With Intel `Y_TILED_GEN12_RC_CCS_CC`
            // (the modifier xdp-wlr/PipeWire negotiate by default on this hardware),
            // OBS-side consumption is slow enough to back-pressure the PipeWire
            // buffer pool and cap effective fps at ~19. Setting SHOJI_SCREENCOPY_NO_DMABUF=1
            // forces clients onto the SHM fallback, which uses an uncompressed buffer.
            if screencopy_dmabuf_advertising_enabled() {
                frame.linux_dmabuf(
                    Fourcc::Xrgb8888 as u32,
                    buffer_size.w as u32,
                    buffer_size.h as u32,
                );
            }
            frame.buffer_done();
        }

        let state = state.screencopy_state();
        let queue = state.queues.get_mut(manager).unwrap();
        queue.pending_frames.insert(frame);
    }

    fn destroyed(
        state: &mut D,
        _client: smithay::reexports::wayland_server::backend::ClientId,
        manager: &ZwlrScreencopyManagerV1,
        _data: &(),
    ) {
        let state = state.screencopy_state();
        let Some(queue) = state.queues.get_mut(manager) else {
            return;
        };
        if queue.is_empty() {
            state.queues.remove(manager);
        }
    }
}

fn screencopy_dmabuf_advertising_enabled() -> bool {
    if crate::env_flag!("SHOJI_SCREENCOPY_NO_DMABUF")
        || crate::env_flag!("SHOJI_SCREENCAST_NO_DMABUF")
    {
        return false;
    }

    true
}

pub trait ScreencopyHandler {
    fn frame(&mut self, manager: &ZwlrScreencopyManagerV1, screencopy: Screencopy);

    fn screencopy_output_enabled(&self, output: &Output) -> bool;

    fn screencopy_state(&mut self) -> &mut ScreencopyManagerState;
}

#[allow(missing_docs)]
#[macro_export]
macro_rules! delegate_screencopy {
    ($(@<$( $lt:tt $( : $clt:tt $(+ $dlt:tt )* )? ),+>)? $ty: ty) => {
        smithay::reexports::wayland_server::delegate_global_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty: [
            smithay::reexports::wayland_protocols_wlr::screencopy::v1::server::zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1: $crate::protocols::screencopy::ScreencopyManagerGlobalData
        ] => $crate::protocols::screencopy::ScreencopyManagerState);

        smithay::reexports::wayland_server::delegate_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty: [
            smithay::reexports::wayland_protocols_wlr::screencopy::v1::server::zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1: ()
        ] => $crate::protocols::screencopy::ScreencopyManagerState);

        smithay::reexports::wayland_server::delegate_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty: [
            smithay::reexports::wayland_protocols_wlr::screencopy::v1::server::zwlr_screencopy_frame_v1::ZwlrScreencopyFrameV1: $crate::protocols::screencopy::ScreencopyFrameState
        ] => $crate::protocols::screencopy::ScreencopyManagerState);
    };
}

#[derive(Clone)]
pub struct ScreencopyFrameInfo {
    output: Output,
    buffer_size: Size<i32, Physical>,
    region_loc: Point<i32, Physical>,
    overlay_cursor: bool,
}

pub enum ScreencopyFrameState {
    Failed,
    Pending {
        manager: ZwlrScreencopyManagerV1,
        info: ScreencopyFrameInfo,
        copied: Arc<AtomicBool>,
    },
}

impl<D> Dispatch<ZwlrScreencopyFrameV1, ScreencopyFrameState, D> for ScreencopyManagerState
where
    D: Dispatch<ZwlrScreencopyFrameV1, ScreencopyFrameState>,
    D: ScreencopyHandler,
    D: 'static,
{
    fn request(
        state: &mut D,
        _client: &Client,
        frame: &ZwlrScreencopyFrameV1,
        request: zwlr_screencopy_frame_v1::Request,
        data: &ScreencopyFrameState,
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, D>,
    ) {
        if matches!(request, zwlr_screencopy_frame_v1::Request::Destroy) {
            return;
        }

        let ScreencopyFrameState::Pending {
            manager,
            info,
            copied,
        } = data
        else {
            return;
        };

        if !state.screencopy_output_enabled(&info.output) {
            trace!(output = %info.output.name(), "screencopy client copied disabled output");
            frame.failed();
            let state = state.screencopy_state();
            let Some(queue) = state.queues.get_mut(manager) else {
                return;
            };
            queue.remove_frame(frame);
            if queue.is_empty() && !manager.is_alive() {
                state.queues.remove(manager);
            }
            return;
        }

        if copied.load(Ordering::SeqCst) {
            frame.post_error(
                zwlr_screencopy_frame_v1::Error::AlreadyUsed,
                "copy was already requested",
            );
            return;
        }

        let (buffer, with_damage) = match request {
            zwlr_screencopy_frame_v1::Request::Copy { buffer } => (buffer, false),
            zwlr_screencopy_frame_v1::Request::CopyWithDamage { buffer } => (buffer, true),
            _ => unreachable!(),
        };

        if crate::env_flag!("SHOJI_SCREENCOPY_PROFILE") {
            tracing::info!(with_damage, "screencopy: copy request received from client");
        }

        let size = info.buffer_size;

        let buffer = if let Ok(dmabuf) = dmabuf::get_dmabuf(&buffer) {
            if !screencopy_dmabuf_advertising_enabled() {
                trace!("screencopy client supplied DMA-BUF while DMA-BUF capture is disabled");
                frame.failed();
                let state = state.screencopy_state();
                let Some(queue) = state.queues.get_mut(manager) else {
                    return;
                };
                queue.remove_frame(frame);
                if queue.is_empty() && !manager.is_alive() {
                    state.queues.remove(manager);
                }
                return;
            }
            if dmabuf.format().code == Fourcc::Xrgb8888
                && dmabuf.width() == size.w as u32
                && dmabuf.height() == size.h as u32
            {
                ScreencopyBuffer::Dmabuf(dmabuf.clone())
            } else {
                frame.post_error(
                    zwlr_screencopy_frame_v1::Error::InvalidBuffer,
                    "invalid dmabuf parameters",
                );
                return;
            }
        } else if shm::with_buffer_contents(&buffer, |_, shm_len, buffer_data| {
            buffer_data.format == Format::Xrgb8888
                && buffer_data.width == size.w
                && buffer_data.height == size.h
                && buffer_data.stride == size.w * 4
                && shm_len == buffer_data.stride as usize * buffer_data.height as usize
        })
        .unwrap_or(false)
        {
            ScreencopyBuffer::Shm(buffer)
        } else {
            frame.post_error(
                zwlr_screencopy_frame_v1::Error::InvalidBuffer,
                "invalid buffer",
            );
            return;
        };

        copied.store(true, Ordering::SeqCst);

        state.frame(
            manager,
            Screencopy {
                buffer,
                frame: frame.clone(),
                info: info.clone(),
                with_damage,
                submitted: false,
            },
        );

        let state = state.screencopy_state();
        let queue = state.queues.get_mut(manager).unwrap();
        queue.pending_frames.remove(frame);
        if queue.is_empty() && !manager.is_alive() {
            state.queues.remove(manager);
        }
    }

    fn destroyed(
        state: &mut D,
        _client: smithay::reexports::wayland_server::backend::ClientId,
        frame: &ZwlrScreencopyFrameV1,
        data: &ScreencopyFrameState,
    ) {
        let ScreencopyFrameState::Pending { manager, .. } = data else {
            return;
        };

        let state = state.screencopy_state();
        let Some(queue) = state.queues.get_mut(manager) else {
            return;
        };

        queue.remove_frame(frame);

        if queue.is_empty() && !manager.is_alive() {
            state.queues.remove(manager);
        }
    }
}

#[derive(Clone)]
pub enum ScreencopyBuffer {
    Dmabuf(Dmabuf),
    Shm(WlBuffer),
}

pub struct Screencopy {
    info: ScreencopyFrameInfo,
    frame: ZwlrScreencopyFrameV1,
    buffer: ScreencopyBuffer,
    with_damage: bool,
    submitted: bool,
}

impl Drop for Screencopy {
    fn drop(&mut self) {
        if !self.submitted {
            self.frame.failed();
        }
    }
}

impl Screencopy {
    pub fn buffer(&self) -> &ScreencopyBuffer {
        &self.buffer
    }

    pub fn region_loc(&self) -> Point<i32, Physical> {
        self.info.region_loc
    }

    pub fn buffer_size(&self) -> Size<i32, Physical> {
        self.info.buffer_size
    }

    pub fn output(&self) -> &Output {
        &self.info.output
    }

    pub fn overlay_cursor(&self) -> bool {
        self.info.overlay_cursor
    }

    pub fn with_damage(&self) -> bool {
        self.with_damage
    }

    pub fn damage(&self, damages: impl Iterator<Item = Rectangle<i32, smithay::utils::Buffer>>) {
        for Rectangle { loc, size } in damages {
            self.frame
                .damage(loc.x as u32, loc.y as u32, size.w as u32, size.h as u32);
        }
    }

    fn submit(mut self, y_invert: bool, timestamp: Duration) {
        self.frame.flags(if y_invert {
            Flags::YInvert
        } else {
            Flags::empty()
        });

        let tv_sec_hi = (timestamp.as_secs() >> 32) as u32;
        let tv_sec_lo = (timestamp.as_secs() & 0xFFFFFFFF) as u32;
        let tv_nsec = timestamp.subsec_nanos();
        self.frame.ready(tv_sec_hi, tv_sec_lo, tv_nsec);

        self.submitted = true;
    }

    /// Send `ready` once the render's fence has signalled and, when
    /// `present_at` is given, not before that monotonic time. Pass the
    /// vblank the captured frame was rendered for: frames then reach the
    /// client at the moment their content appears on screen, at a steady
    /// vblank cadence, instead of at render time (which wanders by up to a
    /// couple of refresh periods with render-ahead). Consumers that sample
    /// the latest frame on their own clock, like OBS, otherwise pick up
    /// unevenly spaced content and animations judder in the recording.
    pub fn submit_after_sync<T>(
        self,
        y_invert: bool,
        sync_point: Option<SyncPoint>,
        present_at: Option<Duration>,
        event_loop: &LoopHandle<'_, T>,
    ) {
        let timestamp = present_at.unwrap_or_else(monotonic_now);
        let delay = timestamp.saturating_sub(monotonic_now());
        // Late (dropped or delayed flip): don't hold the client's capture
        // loop back any further.
        let wait_deadline = !delay.is_zero() && delay <= Duration::from_millis(100);
        let sync_fd = sync_point.and_then(|s| s.export());
        if !wait_deadline && sync_fd.is_none() {
            self.submit(y_invert, timestamp);
            return;
        }

        // Fence and deadline are awaited as two independent sources; whichever
        // fires last sends `ready`.
        let waits = usize::from(wait_deadline) + usize::from(sync_fd.is_some());
        let pending = Rc::new(RefCell::new((Some(self), waits)));
        let arrive = move |pending: &Rc<RefCell<(Option<Screencopy>, usize)>>| {
            let mut pending = pending.borrow_mut();
            pending.1 -= 1;
            if pending.1 == 0
                && let Some(screencopy) = pending.0.take()
            {
                screencopy.submit(y_invert, timestamp);
            }
        };
        if let Some(sync_fd) = sync_fd {
            let pending = pending.clone();
            let source = Generic::new(sync_fd, Interest::READ, Mode::OneShot);
            event_loop
                .insert_source(source, move |_, _, _| {
                    arrive(&pending);
                    Ok(PostAction::Remove)
                })
                .unwrap();
        }
        if wait_deadline {
            event_loop
                .insert_source(Timer::from_duration(delay), move |_, _, _| {
                    arrive(&pending);
                    TimeoutAction::Drop
                })
                .unwrap();
        }
    }
}

fn monotonic_now() -> Duration {
    use smithay::utils::{Clock, Monotonic};
    Duration::from(Clock::<Monotonic>::new().now())
}
