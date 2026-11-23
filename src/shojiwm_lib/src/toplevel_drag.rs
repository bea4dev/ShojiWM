//! Windows moved by a drag and drop (xdg-toplevel-drag-v1).
//!
//! A drag whose `wl_data_source` has an `xdg_toplevel_drag_v1` carries the
//! toplevel attached to it: the window follows the pointer, with the client's
//! offset between the pointer and the window geometry's top-left corner. The
//! move goes through [`WindowMove`], as an interactive move does, so a config
//! sees `onWindowMove` with the source `toplevel-drag` and keeps its
//! snapping, tiling and workspace rules. When the drag ends, the window stays
//! where the move left it.
//!
//! The carried window is left out of hit testing
//! (`hit_test_windows_top_to_bottom`), so it never becomes its own drop
//! target: the drop lands on what is under it.
//!
//! A torn-off tab attaches its new window before the window has a buffer.
//! Until then the window is placed directly, so its first frame shows at
//! the pointer; the move, and the config's events, start on the commit that
//! maps it. A window that unmaps is detached, as the protocol says.
//!
//! smithay's `DnDGrab` knows nothing of this. [`ToplevelDragGrab`] wraps it
//! and moves the window on each pointer motion before the grab sees it.

use smithay::{
    backend::renderer::utils::with_renderer_surface_state,
    desktop::Window,
    input::pointer::{
        AxisFrame, ButtonEvent, GestureHoldBeginEvent, GestureHoldEndEvent, GesturePinchBeginEvent,
        GesturePinchEndEvent, GesturePinchUpdateEvent, GestureSwipeBeginEvent,
        GestureSwipeEndEvent, GestureSwipeUpdateEvent, GrabStartData as PointerGrabStartData,
        MotionEvent, PointerGrab, PointerInnerHandle, RelativeMotionEvent,
    },
    reexports::wayland_server::protocol::{wl_data_source::WlDataSource, wl_surface::WlSurface},
    utils::{Logical, Point, Rectangle},
};
use wayland_protocols::xdg::{
    shell::server::xdg_toplevel::XdgToplevel,
    toplevel_drag::v1::server::xdg_toplevel_drag_v1::XdgToplevelDragV1,
};

use crate::{
    grabs::move_grab::WindowMove,
    protocols::toplevel_drag::{ToplevelDragHandler, ToplevelDragManagerState, toplevel_drag_data},
    ssd::WindowMoveSourceSnapshot,
    state::ShojiWM,
};

/// The drag object of the drag in progress, and the move of its window.
#[derive(Debug)]
pub(crate) struct ActiveToplevelDrag {
    drag: XdgToplevelDragV1,
    /// Set once the attached window is mapped.
    window_move: Option<WindowMove>,
}

fn window_has_buffer(window: &Window) -> bool {
    window.toplevel().is_some_and(|toplevel| {
        with_renderer_surface_state(toplevel.wl_surface(), |state| state.buffer().is_some())
            .unwrap_or(false)
    })
}

/// Where the window geometry's top-left corner goes for the pointer at
/// `pointer`.
fn window_location_for_pointer(
    pointer: Point<f64, Logical>,
    offset: Point<i32, Logical>,
) -> Point<i32, Logical> {
    (pointer - offset.to_f64()).to_i32_round()
}

impl ShojiWM {
    /// `start_drag` with `source`: when the source has a drag object, the
    /// drag carries its toplevel. Not from inside a pointer grab.
    pub(crate) fn begin_toplevel_drag(&mut self, source: &WlDataSource) {
        self.end_toplevel_drag();
        let Some(drag) = self.toplevel_drag_manager_state.drag_for_source(source) else {
            return;
        };
        if !toplevel_drag_data(&drag).with(|machine| machine.start()) {
            return;
        }
        self.toplevel_drag = Some(ActiveToplevelDrag {
            drag,
            window_move: None,
        });
        self.place_attached_toplevel();
    }

    /// The drag was dropped or cancelled: the toplevel is no longer
    /// attached. Safe inside the grab; the move itself ends in
    /// [`Self::end_toplevel_drag`].
    pub(crate) fn mark_toplevel_drag_ended(&mut self) {
        if let Some(active) = self.toplevel_drag.as_ref() {
            toplevel_drag_data(&active.drag).with(|machine| machine.end());
        }
    }

    /// The drag is over: the window stays where it is.
    pub(crate) fn end_toplevel_drag(&mut self) {
        self.stop_toplevel_drag_move();
        if let Some(active) = self.toplevel_drag.take() {
            toplevel_drag_data(&active.drag).with(|machine| machine.end());
        }
    }

    /// The root surface of the window the drag carries.
    pub(crate) fn dragged_toplevel_surface(&self) -> Option<WlSurface> {
        let active = self.toplevel_drag.as_ref()?;
        let toplevel = toplevel_drag_data(&active.drag)
            .with(|machine| machine.attached().map(|attached| attached.toplevel.clone()))?;
        let surface = self.xdg_shell_state.get_toplevel(&toplevel)?;
        Some(surface.wl_surface().clone())
    }

    /// The attached window and the pointer's offset in it.
    fn attached_window(&self) -> Option<(Window, Point<i32, Logical>)> {
        let active = self.toplevel_drag.as_ref()?;
        let offset = toplevel_drag_data(&active.drag)
            .with(|machine| machine.attached().map(|attached| attached.offset))?;
        let surface = self.dragged_toplevel_surface()?;
        let window = self
            .space
            .elements()
            .find(|window| {
                window
                    .toplevel()
                    .is_some_and(|toplevel| toplevel.wl_surface() == &surface)
            })
            .cloned()?;
        Some((window, offset))
    }

    /// Put the attached window under the pointer, and start its move once
    /// it is mapped. Not from inside a pointer grab: the move's start asks
    /// the config at once.
    fn place_attached_toplevel(&mut self) {
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        let location = pointer.current_location();
        let Some((window, offset)) = self.attached_window() else {
            self.stop_toplevel_drag_move();
            return;
        };
        let moving = self
            .toplevel_drag
            .as_ref()
            .and_then(|active| active.window_move.as_ref())
            .map(|window_move| window_move.window == window);
        match moving {
            Some(true) => {
                self.move_attached_toplevel(location);
                return;
            }
            // Another window took the attachment.
            Some(false) => self.stop_toplevel_drag_move(),
            None => {}
        }

        let target = window_location_for_pointer(location, offset);
        if !window_has_buffer(&window) {
            self.space.map_element(window, target, false);
            self.schedule_redraw();
            return;
        }
        let current = self.space.element_location(&window).unwrap_or(target);
        let rect = self
            .managed_resize_initial_rect(&window, Rectangle::new(current, window.geometry().size));
        // The config's rect may include decorations: shift it with the window.
        let rect = Rectangle::new(rect.loc + (target - current), rect.size);
        let mut window_move = WindowMove::new(
            window,
            location,
            target,
            rect,
            WindowMoveSourceSnapshot::ToplevelDrag,
        );
        window_move.notify_start(self);
        window_move.motion(self, location);
        if let Some(active) = self.toplevel_drag.as_mut() {
            active.window_move = Some(window_move);
        } else {
            window_move.finish(self);
        }
    }

    /// Move the window of the started move to follow `location`.
    fn move_attached_toplevel(&mut self, location: Point<f64, Logical>) {
        let Some(mut window_move) = self
            .toplevel_drag
            .as_mut()
            .and_then(|active| active.window_move.take())
        else {
            return;
        };
        window_move.motion(self, location);
        match self.toplevel_drag.as_mut() {
            Some(active) => active.window_move = Some(window_move),
            None => window_move.finish(self),
        }
    }

    /// The pointer moved during the drag (from [`ToplevelDragGrab`], with
    /// the pointer's lock held).
    fn toplevel_drag_motion(&mut self, location: Point<f64, Logical>) {
        let Some(active) = self.toplevel_drag.as_ref() else {
            return;
        };
        if active.window_move.is_some() {
            self.move_attached_toplevel(location);
            return;
        }
        let Some((window, offset)) = self.attached_window() else {
            return;
        };
        if window_has_buffer(&window) {
            // Mapped, but the move has not started: it starts outside the
            // grab.
            self.loop_handle
                .insert_idle(|state| state.place_attached_toplevel());
            return;
        }
        self.space
            .map_element(window, window_location_for_pointer(location, offset), false);
        self.schedule_redraw();
    }

    /// A surface committed: the attached window may have mapped or
    /// unmapped.
    pub(crate) fn toplevel_drag_commit(&mut self, surface: &WlSurface) {
        let Some(active) = self.toplevel_drag.as_ref() else {
            return;
        };
        let moving = active.window_move.is_some();
        let Some((window, _)) = self.attached_window() else {
            return;
        };
        if !window
            .toplevel()
            .is_some_and(|toplevel| toplevel.wl_surface() == surface)
        {
            return;
        }
        match (window_has_buffer(&window), moving) {
            (true, false) => self.place_attached_toplevel(),
            (false, true) => {
                // An unmapped toplevel is detached; the client attaches it
                // again after it maps, if it wants.
                if let Some(active) = self.toplevel_drag.as_ref()
                    && let Some(toplevel) = window.toplevel()
                {
                    toplevel_drag_data(&active.drag)
                        .with(|machine| machine.detach(toplevel.xdg_toplevel()));
                }
                self.stop_toplevel_drag_move();
            }
            _ => {}
        }
    }

    /// A toplevel is destroyed: if the drag carries it, it is detached.
    pub(crate) fn toplevel_drag_toplevel_destroyed(&mut self, toplevel: &XdgToplevel) {
        let Some(active) = self.toplevel_drag.as_ref() else {
            return;
        };
        if toplevel_drag_data(&active.drag).with(|machine| machine.detach(toplevel)) {
            self.stop_toplevel_drag_move();
        }
    }

    fn stop_toplevel_drag_move(&mut self) {
        let Some(window_move) = self
            .toplevel_drag
            .as_mut()
            .and_then(|active| active.window_move.take())
        else {
            return;
        };
        self.pointer_dispatch.discard_grab_motion();
        window_move.finish(self);
    }
}

impl ToplevelDragHandler for ShojiWM {
    fn toplevel_drag_manager_state(&mut self) -> &mut ToplevelDragManagerState {
        &mut self.toplevel_drag_manager_state
    }

    fn toplevel_drag_attached(&mut self, drag: &XdgToplevelDragV1) {
        if self
            .toplevel_drag
            .as_ref()
            .is_some_and(|active| active.drag == *drag)
        {
            self.place_attached_toplevel();
        }
    }

    fn toplevel_drag_destroyed(&mut self, drag: &XdgToplevelDragV1) {
        if self
            .toplevel_drag
            .as_ref()
            .is_some_and(|active| active.drag == *drag)
        {
            self.end_toplevel_drag();
        }
    }
}

/// A pointer grab that moves the attached window on each motion, then
/// hands every event to `inner` (smithay's `DnDGrab`).
pub(crate) struct ToplevelDragGrab<G> {
    pub inner: G,
}

impl<G: PointerGrab<ShojiWM>> PointerGrab<ShojiWM> for ToplevelDragGrab<G> {
    fn motion(
        &mut self,
        data: &mut ShojiWM,
        handle: &mut PointerInnerHandle<'_, ShojiWM>,
        focus: Option<(WlSurface, Point<f64, Logical>)>,
        event: &MotionEvent,
    ) {
        data.toplevel_drag_motion(event.location);
        self.inner.motion(data, handle, focus, event);
    }

    fn relative_motion(
        &mut self,
        data: &mut ShojiWM,
        handle: &mut PointerInnerHandle<'_, ShojiWM>,
        focus: Option<(WlSurface, Point<f64, Logical>)>,
        event: &RelativeMotionEvent,
    ) {
        self.inner.relative_motion(data, handle, focus, event);
    }

    fn button(
        &mut self,
        data: &mut ShojiWM,
        handle: &mut PointerInnerHandle<'_, ShojiWM>,
        event: &ButtonEvent,
    ) {
        self.inner.button(data, handle, event);
    }

    fn axis(
        &mut self,
        data: &mut ShojiWM,
        handle: &mut PointerInnerHandle<'_, ShojiWM>,
        details: AxisFrame,
    ) {
        self.inner.axis(data, handle, details);
    }

    fn frame(&mut self, data: &mut ShojiWM, handle: &mut PointerInnerHandle<'_, ShojiWM>) {
        self.inner.frame(data, handle);
    }

    fn gesture_swipe_begin(
        &mut self,
        data: &mut ShojiWM,
        handle: &mut PointerInnerHandle<'_, ShojiWM>,
        event: &GestureSwipeBeginEvent,
    ) {
        self.inner.gesture_swipe_begin(data, handle, event);
    }

    fn gesture_swipe_update(
        &mut self,
        data: &mut ShojiWM,
        handle: &mut PointerInnerHandle<'_, ShojiWM>,
        event: &GestureSwipeUpdateEvent,
    ) {
        self.inner.gesture_swipe_update(data, handle, event);
    }

    fn gesture_swipe_end(
        &mut self,
        data: &mut ShojiWM,
        handle: &mut PointerInnerHandle<'_, ShojiWM>,
        event: &GestureSwipeEndEvent,
    ) {
        self.inner.gesture_swipe_end(data, handle, event);
    }

    fn gesture_pinch_begin(
        &mut self,
        data: &mut ShojiWM,
        handle: &mut PointerInnerHandle<'_, ShojiWM>,
        event: &GesturePinchBeginEvent,
    ) {
        self.inner.gesture_pinch_begin(data, handle, event);
    }

    fn gesture_pinch_update(
        &mut self,
        data: &mut ShojiWM,
        handle: &mut PointerInnerHandle<'_, ShojiWM>,
        event: &GesturePinchUpdateEvent,
    ) {
        self.inner.gesture_pinch_update(data, handle, event);
    }

    fn gesture_pinch_end(
        &mut self,
        data: &mut ShojiWM,
        handle: &mut PointerInnerHandle<'_, ShojiWM>,
        event: &GesturePinchEndEvent,
    ) {
        self.inner.gesture_pinch_end(data, handle, event);
    }

    fn gesture_hold_begin(
        &mut self,
        data: &mut ShojiWM,
        handle: &mut PointerInnerHandle<'_, ShojiWM>,
        event: &GestureHoldBeginEvent,
    ) {
        self.inner.gesture_hold_begin(data, handle, event);
    }

    fn gesture_hold_end(
        &mut self,
        data: &mut ShojiWM,
        handle: &mut PointerInnerHandle<'_, ShojiWM>,
        event: &GestureHoldEndEvent,
    ) {
        self.inner.gesture_hold_end(data, handle, event);
    }

    fn start_data(&self) -> &PointerGrabStartData<ShojiWM> {
        self.inner.start_data()
    }

    fn unset(&mut self, data: &mut ShojiWM) {
        self.inner.unset(data);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_window_keeps_the_offset_the_client_gave_under_the_pointer() {
        let location = window_location_for_pointer(Point::from((300.4, 200.6)), (40, 10).into());
        assert_eq!(location, Point::from((260, 191)));
    }
}
