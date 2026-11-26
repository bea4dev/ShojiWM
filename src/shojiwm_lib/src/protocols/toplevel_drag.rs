//! `xdg_toplevel_drag_manager_v1` (staging xdg-toplevel-drag-v1.xml).
//!
//! A client moves a window with a drag and drop: it makes an
//! `xdg_toplevel_drag_v1` for the `wl_data_source` of the drag and attaches a
//! toplevel to it, before or during the drag. The compositor then moves that
//! toplevel with the pointer until the drag ends. Browsers and toolkits use it
//! to tear a tab off into a new window and to dock it back.
//!
//! This module keeps the protocol objects and the state of each drag object
//! ([`DragMachine`]); `crate::toplevel_drag` moves the window.

use std::sync::Mutex;

use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource, backend::ClientId,
    protocol::wl_data_source::WlDataSource,
};
use smithay::utils::{Logical, Point};
use wayland_protocols::xdg::{
    shell::server::xdg_toplevel::XdgToplevel,
    toplevel_drag::v1::server::{
        xdg_toplevel_drag_manager_v1::{self, XdgToplevelDragManagerV1},
        xdg_toplevel_drag_v1::{self, XdgToplevelDragV1},
    },
};

const VERSION: u32 = 1;

pub trait ToplevelDragHandler {
    fn toplevel_drag_manager_state(&mut self) -> &mut ToplevelDragManagerState;
    /// `drag` got a toplevel (`attach`).
    fn toplevel_drag_attached(&mut self, drag: &XdgToplevelDragV1);
    /// `drag` is gone (`destroy`, or its client went away).
    fn toplevel_drag_destroyed(&mut self, drag: &XdgToplevelDragV1);
}

/// Where a drag object is in the life of its drag and drop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DragPhase {
    /// `wl_data_device.start_drag` has not run for the source yet.
    #[default]
    Waiting,
    Dragging,
    /// Dropped or cancelled. The object only waits for its `destroy`.
    Ended,
}

/// The toplevel moved with the drag, and where the pointer sits in it
/// (relative to its window geometry).
#[derive(Debug, Clone, PartialEq)]
pub struct Attachment<T> {
    pub toplevel: T,
    pub offset: Point<i32, Logical>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DragError {
    /// `attach` while a toplevel that still has its role is attached.
    ToplevelAttached,
    /// `destroy` before the drag ended.
    OngoingDrag,
}

/// The state of one `xdg_toplevel_drag_v1`, generic over the toplevel so
/// it can be tested without a client.
#[derive(Debug)]
pub struct DragMachine<T> {
    phase: DragPhase,
    attached: Option<Attachment<T>>,
}

impl<T> Default for DragMachine<T> {
    fn default() -> Self {
        Self {
            phase: DragPhase::default(),
            attached: None,
        }
    }
}

impl<T: Clone + PartialEq> DragMachine<T> {
    pub fn phase(&self) -> DragPhase {
        self.phase
    }

    pub fn attached(&self) -> Option<&Attachment<T>> {
        self.attached.as_ref()
    }

    /// `attach`. `has_role` tells whether a toplevel is still alive as an
    /// `xdg_toplevel`. Returns whether the toplevel is now attached: after
    /// the drag ended, an attach has no effect.
    pub fn attach(
        &mut self,
        toplevel: T,
        offset: Point<i32, Logical>,
        has_role: impl Fn(&T) -> bool,
    ) -> Result<bool, DragError> {
        if self
            .attached
            .as_ref()
            .is_some_and(|attached| has_role(&attached.toplevel))
        {
            return Err(DragError::ToplevelAttached);
        }
        if self.phase == DragPhase::Ended {
            self.attached = None;
            return Ok(false);
        }
        self.attached = Some(Attachment { toplevel, offset });
        Ok(true)
    }

    /// The source's drag started. Returns false when it already had.
    pub fn start(&mut self) -> bool {
        if self.phase != DragPhase::Waiting {
            return false;
        }
        self.phase = DragPhase::Dragging;
        true
    }

    /// The drag was dropped or cancelled. Returns the toplevel that was
    /// attached: it stays where it is.
    pub fn end(&mut self) -> Option<Attachment<T>> {
        self.phase = DragPhase::Ended;
        self.attached.take()
    }

    /// `toplevel` was unmapped or destroyed: the protocol detaches it.
    /// Returns whether it was attached.
    pub fn detach(&mut self, toplevel: &T) -> bool {
        if self
            .attached
            .as_ref()
            .is_some_and(|attached| attached.toplevel == *toplevel)
        {
            self.attached = None;
            return true;
        }
        false
    }

    /// `destroy`: allowed before the drag starts and after it ends.
    pub fn check_destroy(&self) -> Result<(), DragError> {
        match self.phase {
            DragPhase::Dragging => Err(DragError::OngoingDrag),
            DragPhase::Waiting | DragPhase::Ended => Ok(()),
        }
    }
}

/// The user data of an `xdg_toplevel_drag_v1`.
#[derive(Debug)]
pub struct ToplevelDragData {
    pub source: WlDataSource,
    machine: Mutex<DragMachine<XdgToplevel>>,
}

impl ToplevelDragData {
    pub fn with<R>(&self, f: impl FnOnce(&mut DragMachine<XdgToplevel>) -> R) -> R {
        f(&mut self.machine.lock().unwrap())
    }
}

/// The data of `drag`; every drag object has it.
pub fn toplevel_drag_data(drag: &XdgToplevelDragV1) -> &ToplevelDragData {
    drag.data::<ToplevelDragData>()
        .expect("xdg_toplevel_drag_v1 without its data")
}

#[derive(Debug, Default)]
pub struct ToplevelDragManagerState {
    drags: Vec<XdgToplevelDragV1>,
}

impl ToplevelDragManagerState {
    pub fn new<D>(display: &DisplayHandle) -> Self
    where
        D: GlobalDispatch<XdgToplevelDragManagerV1, ()>,
        D: Dispatch<XdgToplevelDragManagerV1, ()>,
        D: Dispatch<XdgToplevelDragV1, ToplevelDragData>,
        D: 'static,
    {
        display.create_global::<D, XdgToplevelDragManagerV1, _>(VERSION, ());
        Self::default()
    }

    /// The drag object made for `source`.
    pub fn drag_for_source(&self, source: &WlDataSource) -> Option<XdgToplevelDragV1> {
        self.drags
            .iter()
            .find(|drag| toplevel_drag_data(drag).source == *source)
            .cloned()
    }
}

impl<D> GlobalDispatch<XdgToplevelDragManagerV1, (), D> for ToplevelDragManagerState
where
    D: GlobalDispatch<XdgToplevelDragManagerV1, ()>,
    D: Dispatch<XdgToplevelDragManagerV1, ()>,
    D: Dispatch<XdgToplevelDragV1, ToplevelDragData>,
    D: ToplevelDragHandler,
    D: 'static,
{
    fn bind(
        _state: &mut D,
        _dh: &DisplayHandle,
        _client: &Client,
        manager: New<XdgToplevelDragManagerV1>,
        _data: &(),
        data_init: &mut DataInit<'_, D>,
    ) {
        data_init.init(manager, ());
    }
}

impl<D> Dispatch<XdgToplevelDragManagerV1, (), D> for ToplevelDragManagerState
where
    D: Dispatch<XdgToplevelDragManagerV1, ()>,
    D: Dispatch<XdgToplevelDragV1, ToplevelDragData>,
    D: ToplevelDragHandler,
    D: 'static,
{
    fn request(
        state: &mut D,
        _client: &Client,
        manager: &XdgToplevelDragManagerV1,
        request: xdg_toplevel_drag_manager_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            xdg_toplevel_drag_manager_v1::Request::GetXdgToplevelDrag { id, data_source } => {
                let taken = state
                    .toplevel_drag_manager_state()
                    .drag_for_source(&data_source)
                    .is_some();
                let drag = data_init.init(
                    id,
                    ToplevelDragData {
                        source: data_source,
                        machine: Mutex::new(DragMachine::default()),
                    },
                );
                if taken {
                    manager.post_error(
                        xdg_toplevel_drag_manager_v1::Error::InvalidSource,
                        "the data source already has an xdg_toplevel_drag_v1",
                    );
                    return;
                }
                state.toplevel_drag_manager_state().drags.push(drag);
            }
            xdg_toplevel_drag_manager_v1::Request::Destroy => {}
            _ => {}
        }
    }
}

impl<D> Dispatch<XdgToplevelDragV1, ToplevelDragData, D> for ToplevelDragManagerState
where
    D: Dispatch<XdgToplevelDragV1, ToplevelDragData>,
    D: ToplevelDragHandler,
    D: 'static,
{
    fn request(
        state: &mut D,
        _client: &Client,
        drag: &XdgToplevelDragV1,
        request: xdg_toplevel_drag_v1::Request,
        data: &ToplevelDragData,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            xdg_toplevel_drag_v1::Request::Attach {
                toplevel,
                x_offset,
                y_offset,
            } => {
                let attached = data.with(|machine| {
                    machine.attach(toplevel, (x_offset, y_offset).into(), |toplevel| {
                        toplevel.is_alive()
                    })
                });
                match attached {
                    Ok(true) => state.toplevel_drag_attached(drag),
                    Ok(false) => {}
                    Err(_) => drag.post_error(
                        xdg_toplevel_drag_v1::Error::ToplevelAttached,
                        "a toplevel is already attached",
                    ),
                }
            }
            xdg_toplevel_drag_v1::Request::Destroy => {
                if data.with(|machine| machine.check_destroy()).is_err() {
                    drag.post_error(
                        xdg_toplevel_drag_v1::Error::OngoingDrag,
                        "destroyed before the drag ended",
                    );
                }
            }
            _ => {}
        }
    }

    fn destroyed(
        state: &mut D,
        _client: ClientId,
        drag: &XdgToplevelDragV1,
        _data: &ToplevelDragData,
    ) {
        state
            .toplevel_drag_manager_state()
            .drags
            .retain(|existing| existing != drag);
        state.toplevel_drag_destroyed(drag);
    }
}

/// Delegate the `xdg_toplevel_drag_manager_v1` global to
/// [`ToplevelDragManagerState`].
#[macro_export]
macro_rules! delegate_toplevel_drag {
    ($ty: ty) => {
        smithay::reexports::wayland_server::delegate_global_dispatch!($ty: [
            wayland_protocols::xdg::toplevel_drag::v1::server::xdg_toplevel_drag_manager_v1::XdgToplevelDragManagerV1: ()
        ] => $crate::protocols::toplevel_drag::ToplevelDragManagerState);

        smithay::reexports::wayland_server::delegate_dispatch!($ty: [
            wayland_protocols::xdg::toplevel_drag::v1::server::xdg_toplevel_drag_manager_v1::XdgToplevelDragManagerV1: ()
        ] => $crate::protocols::toplevel_drag::ToplevelDragManagerState);

        smithay::reexports::wayland_server::delegate_dispatch!($ty: [
            wayland_protocols::xdg::toplevel_drag::v1::server::xdg_toplevel_drag_v1::XdgToplevelDragV1: $crate::protocols::toplevel_drag::ToplevelDragData
        ] => $crate::protocols::toplevel_drag::ToplevelDragManagerState);
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A toplevel, by id.
    #[derive(Debug, Clone, PartialEq)]
    struct Toplevel(u32);

    fn alive(_: &Toplevel) -> bool {
        true
    }

    fn dead(_: &Toplevel) -> bool {
        false
    }

    #[test]
    fn a_toplevel_attached_before_the_drag_moves_with_it_until_the_drop() {
        let mut machine = DragMachine::default();
        assert_eq!(machine.attach(Toplevel(1), (10, 5).into(), alive), Ok(true));
        assert!(machine.check_destroy().is_ok(), "no drag yet");
        assert!(machine.start());
        assert_eq!(machine.phase(), DragPhase::Dragging);
        assert_eq!(machine.attached().unwrap().offset, (10, 5).into());
        assert_eq!(machine.check_destroy(), Err(DragError::OngoingDrag));

        let left = machine.end().unwrap();
        assert_eq!(left.toplevel, Toplevel(1));
        assert!(machine.attached().is_none());
        assert!(machine.check_destroy().is_ok());
    }

    #[test]
    fn a_new_toplevel_can_be_attached_during_the_drag() {
        let mut machine = DragMachine::default();
        assert!(machine.start());
        assert!(!machine.start(), "a source starts one drag");
        assert_eq!(machine.attach(Toplevel(2), (0, 0).into(), alive), Ok(true));
        assert_eq!(machine.attached().unwrap().toplevel, Toplevel(2));
    }

    #[test]
    fn attaching_over_a_live_toplevel_is_an_error() {
        let mut machine = DragMachine::default();
        machine.attach(Toplevel(1), (0, 0).into(), alive).unwrap();
        assert_eq!(
            machine.attach(Toplevel(2), (0, 0).into(), alive),
            Err(DragError::ToplevelAttached)
        );
        // Once the first toplevel lost its role, another may take its place.
        assert_eq!(machine.attach(Toplevel(2), (3, 4).into(), dead), Ok(true));
        assert_eq!(machine.attached().unwrap().toplevel, Toplevel(2));
    }

    #[test]
    fn an_unmapped_toplevel_is_detached_and_can_attach_again() {
        let mut machine = DragMachine::default();
        machine.start();
        machine.attach(Toplevel(1), (0, 0).into(), alive).unwrap();
        assert!(!machine.detach(&Toplevel(9)), "only the attached one");
        assert!(machine.detach(&Toplevel(1)));
        assert!(machine.attached().is_none());
        assert_eq!(machine.attach(Toplevel(1), (0, 0).into(), alive), Ok(true));
    }

    #[test]
    fn an_attach_after_the_drag_ended_does_nothing() {
        let mut machine = DragMachine::default();
        machine.start();
        assert!(machine.end().is_none());
        assert_eq!(machine.attach(Toplevel(1), (0, 0).into(), alive), Ok(false));
        assert!(machine.attached().is_none());
        assert!(!machine.start(), "an ended drag does not start again");
    }
}
