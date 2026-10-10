//! `hyprland_toplevel_export_manager_v1` server: capturing a single window.
//!
//! Quickshell's `ScreencopyView` captures windows only through this protocol
//! (outputs go through ext-image-copy-capture), naming them by their
//! `zwlr_foreign_toplevel_handle_v1` (`capture_toplevel_with_wlr_toplevel_handle`,
//! version 2). Version 1's `capture_toplevel` names a window by its address in
//! Hyprland, which means nothing here, so those frames fail.
//!
//! Each frame advertises one wl_shm Xrgb8888 buffer of the window's size, then
//! `copy` queues it in [`HyprlandToplevelExportState`] for the backend's next
//! render pass (see `backend::image_copy_capture_render`). A copy without
//! `ignore_damage` waits until the window changes.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};

use smithay::backend::renderer::damage::OutputDamageTracker;
use smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer;
use smithay::reexports::wayland_server::protocol::wl_shm;
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource, backend::ClientId,
    backend::GlobalId,
};
use smithay::utils::{Buffer as BufferCoords, Size};
use smithay::wayland::shm;
use wayland_protocols_wlr::foreign_toplevel::v1::server::zwlr_foreign_toplevel_handle_v1::ZwlrForeignToplevelHandleV1;

use crate::wlr_foreign_toplevel::WlrForeignToplevelHandle;

pub use self::generated::server::{
    hyprland_toplevel_export_frame_v1::{self, HyprlandToplevelExportFrameV1},
    hyprland_toplevel_export_manager_v1::{self, HyprlandToplevelExportManagerV1},
};

#[allow(missing_docs, dead_code, non_camel_case_types, unused_imports, clippy::all)]
mod generated {
    pub mod server {
        use smithay::reexports::wayland_server;
        use smithay::reexports::wayland_server::protocol::*;
        use wayland_protocols_wlr::foreign_toplevel::v1::server::*;

        pub mod __interfaces {
            use smithay::reexports::wayland_server::protocol::__interfaces::*;
            use wayland_protocols_wlr::foreign_toplevel::v1::server::__interfaces::*;
            wayland_scanner::generate_interfaces!("protocols/hyprland-toplevel-export-v1.xml");
        }
        use self::__interfaces::*;

        wayland_scanner::generate_server_code!("protocols/hyprland-toplevel-export-v1.xml");
    }
}

const VERSION: u32 = 2;

pub trait HyprlandToplevelExportHandler {
    fn hyprland_toplevel_export_state(&mut self) -> &mut HyprlandToplevelExportState;

    /// The buffer size a capture of this window needs right now, or `None`
    /// when the window is gone or has no size yet.
    fn hyprland_toplevel_export_size(
        &self,
        handle: &WlrForeignToplevelHandle,
    ) -> Option<Size<i32, BufferCoords>>;

    /// A copy was queued. It needs a render pass even when nothing is drawn
    /// anew: the window may have changed since the previous copy, and an
    /// `ignore_damage` copy must not wait at all. A pass that finds the window
    /// unchanged leaves the copy waiting.
    fn hyprland_toplevel_export_queued(&mut self);
}

/// A copy waiting for the backend's next render pass.
pub struct PendingToplevelExport {
    pub frame: HyprlandToplevelExportFrameV1,
    pub manager: HyprlandToplevelExportManagerV1,
    pub handle: WlrForeignToplevelHandle,
    pub buffer: WlBuffer,
    pub overlay_cursor: bool,
    pub ignore_damage: bool,
}

pub struct HyprlandToplevelExportState {
    global: GlobalId,
    pub pending: Vec<PendingToplevelExport>,
    /// Per manager and window, what the last copy saw, so the next copy can
    /// wait for (and report) the window's damage since.
    trackers: HashMap<HyprlandToplevelExportManagerV1, Vec<(WlrForeignToplevelHandle, OutputDamageTracker)>>,
}

impl HyprlandToplevelExportState {
    pub fn new<D>(display: &DisplayHandle) -> Self
    where
        D: GlobalDispatch<HyprlandToplevelExportManagerV1, ()>
            + Dispatch<HyprlandToplevelExportManagerV1, ()>
            + Dispatch<HyprlandToplevelExportFrameV1, ToplevelExportFrameData>
            + HyprlandToplevelExportHandler
            + 'static,
    {
        let global = display.create_global::<D, HyprlandToplevelExportManagerV1, _>(VERSION, ());
        Self {
            global,
            pending: Vec::new(),
            trackers: HashMap::new(),
        }
    }

    pub fn global(&self) -> GlobalId {
        self.global.clone()
    }

    /// The damage tracker for `handle`'s window as seen by `manager`, created
    /// (which reports everything as damaged) when missing or when the buffer
    /// size changed.
    pub fn tracker(
        &mut self,
        manager: &HyprlandToplevelExportManagerV1,
        handle: &WlrForeignToplevelHandle,
        size: smithay::utils::Size<i32, smithay::utils::Physical>,
        scale: smithay::utils::Scale<f64>,
    ) -> &mut OutputDamageTracker {
        let trackers = self.trackers.entry(manager.clone()).or_default();
        trackers.retain(|(handle, _)| !handle.is_closed());
        let index = match trackers.iter().position(|(known, _)| known.same_as(handle)) {
            Some(index) => index,
            None => {
                trackers.push((handle.clone(), OutputDamageTracker::new(size, scale, Default::default())));
                trackers.len() - 1
            }
        };
        let tracker = &mut trackers[index].1;
        let (current_size, current_scale, _) = tracker
            .mode()
            .clone()
            .try_into()
            .unwrap_or((size, scale, Default::default()));
        if current_size != size || current_scale != scale {
            *tracker = OutputDamageTracker::new(size, scale, Default::default());
        }
        tracker
    }
}

/// Data of a frame object.
pub struct ToplevelExportFrameData {
    /// `None` when the capture failed at creation (the `failed` event was sent).
    handle: Option<WlrForeignToplevelHandle>,
    size: Size<i32, BufferCoords>,
    overlay_cursor: bool,
    manager: HyprlandToplevelExportManagerV1,
    copied: AtomicBool,
}

impl<D> GlobalDispatch<HyprlandToplevelExportManagerV1, (), D> for HyprlandToplevelExportState
where
    D: GlobalDispatch<HyprlandToplevelExportManagerV1, ()>
        + Dispatch<HyprlandToplevelExportManagerV1, ()>
        + Dispatch<HyprlandToplevelExportFrameV1, ToplevelExportFrameData>
        + HyprlandToplevelExportHandler
        + 'static,
{
    fn bind(
        _state: &mut D,
        _dh: &DisplayHandle,
        _client: &Client,
        manager: New<HyprlandToplevelExportManagerV1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, D>,
    ) {
        data_init.init(manager, ());
    }
}

impl<D> Dispatch<HyprlandToplevelExportManagerV1, (), D> for HyprlandToplevelExportState
where
    D: GlobalDispatch<HyprlandToplevelExportManagerV1, ()>
        + Dispatch<HyprlandToplevelExportManagerV1, ()>
        + Dispatch<HyprlandToplevelExportFrameV1, ToplevelExportFrameData>
        + HyprlandToplevelExportHandler
        + 'static,
{
    fn request(
        state: &mut D,
        _client: &Client,
        manager: &HyprlandToplevelExportManagerV1,
        request: hyprland_toplevel_export_manager_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, D>,
    ) {
        let (frame, overlay_cursor, handle) = match request {
            hyprland_toplevel_export_manager_v1::Request::CaptureToplevel {
                frame,
                overlay_cursor,
                handle: _,
            } => (frame, overlay_cursor, None),
            hyprland_toplevel_export_manager_v1::Request::CaptureToplevelWithWlrToplevelHandle {
                frame,
                overlay_cursor,
                handle,
            } => (frame, overlay_cursor, Some(handle)),
            hyprland_toplevel_export_manager_v1::Request::Destroy => return,
        };

        let handle = handle
            .as_ref()
            .and_then(|handle: &ZwlrForeignToplevelHandleV1| {
                handle.data::<WlrForeignToplevelHandle>().cloned()
            })
            .filter(|handle| !handle.is_closed());
        let size = handle
            .as_ref()
            .and_then(|handle| state.hyprland_toplevel_export_size(handle));
        let (handle, size) = match (handle, size) {
            (Some(handle), Some(size)) => (Some(handle), size),
            _ => (None, Size::default()),
        };
        let frame = data_init.init(
            frame,
            ToplevelExportFrameData {
                handle: handle.clone(),
                size,
                overlay_cursor: overlay_cursor != 0,
                manager: manager.clone(),
                copied: AtomicBool::new(false),
            },
        );
        if handle.is_none() {
            frame.failed();
            return;
        }
        frame.buffer(
            wl_shm::Format::Xrgb8888,
            size.w as u32,
            size.h as u32,
            size.w as u32 * 4,
        );
        frame.buffer_done();
    }

    fn destroyed(
        state: &mut D,
        _client: ClientId,
        manager: &HyprlandToplevelExportManagerV1,
        _data: &(),
    ) {
        state
            .hyprland_toplevel_export_state()
            .trackers
            .remove(manager);
    }
}

impl<D> Dispatch<HyprlandToplevelExportFrameV1, ToplevelExportFrameData, D>
    for HyprlandToplevelExportState
where
    D: GlobalDispatch<HyprlandToplevelExportManagerV1, ()>
        + Dispatch<HyprlandToplevelExportManagerV1, ()>
        + Dispatch<HyprlandToplevelExportFrameV1, ToplevelExportFrameData>
        + HyprlandToplevelExportHandler
        + 'static,
{
    fn request(
        state: &mut D,
        _client: &Client,
        frame: &HyprlandToplevelExportFrameV1,
        request: hyprland_toplevel_export_frame_v1::Request,
        data: &ToplevelExportFrameData,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, D>,
    ) {
        let hyprland_toplevel_export_frame_v1::Request::Copy {
            buffer,
            ignore_damage,
        } = request
        else {
            return;
        };
        let Some(handle) = data.handle.clone() else {
            // Already failed at creation.
            return;
        };
        if data.copied.swap(true, Ordering::SeqCst) {
            frame.post_error(
                hyprland_toplevel_export_frame_v1::Error::AlreadyUsed,
                "frame already used",
            );
            return;
        }
        // The stride may be larger than advertised: Quickshell pads rows to
        // 256 bytes. The pool's bounds are checked when the frame is drawn.
        let valid = shm::with_buffer_contents(&buffer, |_, _, buffer_data| {
            buffer_data.format == wl_shm::Format::Xrgb8888
                && buffer_data.width == data.size.w
                && buffer_data.height == data.size.h
                && buffer_data.stride >= data.size.w * 4
        })
        .unwrap_or(false);
        if !valid {
            frame.post_error(
                hyprland_toplevel_export_frame_v1::Error::InvalidBuffer,
                "invalid buffer",
            );
            return;
        }

        state
            .hyprland_toplevel_export_state()
            .pending
            .push(PendingToplevelExport {
                frame: frame.clone(),
                manager: data.manager.clone(),
                handle,
                buffer,
                overlay_cursor: data.overlay_cursor,
                ignore_damage: ignore_damage != 0,
            });
        state.hyprland_toplevel_export_queued();
    }

    fn destroyed(
        state: &mut D,
        _client: ClientId,
        frame: &HyprlandToplevelExportFrameV1,
        _data: &ToplevelExportFrameData,
    ) {
        state
            .hyprland_toplevel_export_state()
            .pending
            .retain(|pending| pending.frame != *frame);
    }
}

#[macro_export]
macro_rules! delegate_hyprland_toplevel_export {
    ($ty: ty) => {
        smithay::reexports::wayland_server::delegate_global_dispatch!($ty: [
            $crate::protocols::hyprland_toplevel_export::HyprlandToplevelExportManagerV1: ()
        ] => $crate::protocols::hyprland_toplevel_export::HyprlandToplevelExportState);

        smithay::reexports::wayland_server::delegate_dispatch!($ty: [
            $crate::protocols::hyprland_toplevel_export::HyprlandToplevelExportManagerV1: ()
        ] => $crate::protocols::hyprland_toplevel_export::HyprlandToplevelExportState);

        smithay::reexports::wayland_server::delegate_dispatch!($ty: [
            $crate::protocols::hyprland_toplevel_export::HyprlandToplevelExportFrameV1: $crate::protocols::hyprland_toplevel_export::ToplevelExportFrameData
        ] => $crate::protocols::hyprland_toplevel_export::HyprlandToplevelExportState);
    };
}
