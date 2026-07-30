//! Resize grab is the state of a composer during which the client window is being resized.
//!
//! eg. Usually whenever a user clicks on the app's border and starts dragging, the compositors
//! enters a ResizeSurfaceGrab state.

use crate::ssd::{
    WindowPositionSnapshot, WindowResizeEdgesSnapshot, WindowResizeEventSnapshot,
    WindowResizePhaseSnapshot, WindowResizePointSnapshot, WindowResizeSourceSnapshot,
};
use crate::state::ShojiWM;
use smithay::{
    desktop::{Space, Window},
    input::pointer::{
        AxisFrame, ButtonEvent, CursorIcon, GestureHoldBeginEvent, GestureHoldEndEvent,
        GesturePinchBeginEvent, GesturePinchEndEvent, GesturePinchUpdateEvent,
        GestureSwipeBeginEvent, GestureSwipeEndEvent, GestureSwipeUpdateEvent,
        GrabStartData as PointerGrabStartData, MotionEvent, PointerGrab, PointerInnerHandle,
        RelativeMotionEvent,
    },
    reexports::{
        wayland_protocols::xdg::shell::server::xdg_toplevel,
        wayland_server::protocol::wl_surface::WlSurface,
    },
    utils::{Logical, Point, Rectangle, Size},
    wayland::{compositor, shell::xdg::SurfaceCachedState},
};
use std::cell::RefCell;
use tracing::info;

fn managed_rect_debug_enabled() -> bool {
    std::env::var_os("SHOJI_MANAGED_RECT_DEBUG")
        .is_some_and(|value| value != "0" && !value.is_empty())
}

bitflags::bitflags! {
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    pub struct ResizeEdge: u32 {
        const TOP          = 0b0001;
        const BOTTOM       = 0b0010;
        const LEFT         = 0b0100;
        const RIGHT        = 0b1000;

        const TOP_LEFT     = Self::TOP.bits() | Self::LEFT.bits();
        const BOTTOM_LEFT  = Self::BOTTOM.bits() | Self::LEFT.bits();

        const TOP_RIGHT    = Self::TOP.bits() | Self::RIGHT.bits();
        const BOTTOM_RIGHT = Self::BOTTOM.bits() | Self::RIGHT.bits();
    }
}

impl From<xdg_toplevel::ResizeEdge> for ResizeEdge {
    #[inline]
    fn from(x: xdg_toplevel::ResizeEdge) -> Self {
        Self::from_bits(x as u32).unwrap()
    }
}

pub struct ResizeSurfaceGrab {
    start_data: PointerGrabStartData<ShojiWM>,
    window: Window,

    edges: ResizeEdge,
    source: WindowResizeSourceSnapshot,
    runtime_managed: bool,

    initial_rect: Rectangle<i32, Logical>,
    initial_event_rect: Rectangle<i32, Logical>,
    last_pointer: Point<f64, Logical>,
    last_window_size: Size<i32, Logical>,
}

impl ResizeSurfaceGrab {
    pub fn start(
        start_data: PointerGrabStartData<ShojiWM>,
        window: Window,
        edges: ResizeEdge,
        initial_window_rect: Rectangle<i32, Logical>,
        initial_event_rect: Rectangle<i32, Logical>,
        source: WindowResizeSourceSnapshot,
    ) -> Option<Self> {
        let toplevel = window.toplevel()?;
        let initial_rect = initial_window_rect;
        let last_pointer = start_data.location;

        ResizeSurfaceState::with(toplevel.wl_surface(), |state| {
            *state = ResizeSurfaceState::Resizing {
                edges,
                initial_rect,
            };
        });

        Some(Self {
            start_data,
            window,
            edges,
            source,
            runtime_managed: false,
            initial_rect,
            initial_event_rect,
            last_pointer,
            last_window_size: initial_rect.size,
        })
    }

    pub fn notify_start(&mut self, data: &mut ShojiWM) {
        data.cursor_override = Some(match self.edges {
            e if e.contains(ResizeEdge::TOP | ResizeEdge::LEFT) => CursorIcon::NwResize,
            e if e.contains(ResizeEdge::TOP | ResizeEdge::RIGHT) => CursorIcon::NeResize,
            e if e.contains(ResizeEdge::BOTTOM | ResizeEdge::LEFT) => CursorIcon::SwResize,
            e if e.contains(ResizeEdge::BOTTOM | ResizeEdge::RIGHT) => CursorIcon::SeResize,
            e if e.contains(ResizeEdge::LEFT) => CursorIcon::WResize,
            e if e.contains(ResizeEdge::RIGHT) => CursorIcon::EResize,
            e if e.contains(ResizeEdge::TOP) => CursorIcon::NResize,
            e if e.contains(ResizeEdge::BOTTOM) => CursorIcon::SResize,
            e if e.contains(ResizeEdge::LEFT | ResizeEdge::RIGHT) => CursorIcon::EwResize,
            e if e.contains(ResizeEdge::TOP | ResizeEdge::BOTTOM) => CursorIcon::NsResize,
            _ => CursorIcon::AllResize,
        });
        data.schedule_redraw();

        self.runtime_managed = self.invoke_runtime_event(
            data,
            WindowResizePhaseSnapshot::Start,
            self.start_data.location,
        );
    }

    fn invoke_runtime_event(
        &self,
        data: &mut ShojiWM,
        phase: WindowResizePhaseSnapshot,
        current_pointer: Point<f64, Logical>,
    ) -> bool {
        let window_id = data.snapshot_window(&self.window).id;
        let event = self.runtime_event(data, phase, current_pointer);
        let now_ms = std::time::Duration::from(data.clock.now()).as_millis() as u64;
        data.invoke_window_resize_event(&window_id, &event, now_ms)
    }

    fn runtime_event(
        &self,
        data: &ShojiWM,
        phase: WindowResizePhaseSnapshot,
        current_pointer: Point<f64, Logical>,
    ) -> WindowResizeEventSnapshot {
        let window_id = data.snapshot_window(&self.window).id;
        let start_pointer = self.start_data.location;
        let delta = current_pointer - start_pointer;
        let current_rect = resize_rect_for_delta(self.initial_event_rect, self.edges, delta);
        let output_name = data
            .space
            .outputs()
            .find(|output| {
                data.space
                    .output_geometry(output)
                    .is_some_and(|geometry| geometry.contains(current_pointer.to_i32_floor()))
            })
            .map(|output| output.name());

        if managed_rect_debug_enabled() {
            info!(
                window_id,
                ?phase,
                ?self.source,
                start_pointer_x = start_pointer.x,
                start_pointer_y = start_pointer.y,
                current_pointer_x = current_pointer.x,
                current_pointer_y = current_pointer.y,
                delta_x = delta.x,
                delta_y = delta.y,
                start_rect_x = self.initial_event_rect.loc.x,
                start_rect_y = self.initial_event_rect.loc.y,
                start_rect_width = self.initial_event_rect.size.w,
                start_rect_height = self.initial_event_rect.size.h,
                start_rect_right = self.initial_event_rect.loc.x + self.initial_event_rect.size.w,
                start_rect_bottom = self.initial_event_rect.loc.y + self.initial_event_rect.size.h,
                current_rect_x = current_rect.loc.x,
                current_rect_y = current_rect.loc.y,
                current_rect_width = current_rect.size.w,
                current_rect_height = current_rect.size.h,
                current_rect_right = current_rect.loc.x + current_rect.size.w,
                current_rect_bottom = current_rect.loc.y + current_rect.size.h,
                "managed rect debug: resize event"
            );
        }

        WindowResizeEventSnapshot {
            source: self.source,
            phase,
            edges: resize_edges_snapshot(self.edges),
            start_pointer: point_snapshot(start_pointer),
            current_pointer: point_snapshot(current_pointer),
            delta: point_snapshot(delta),
            start_rect: rect_snapshot(self.initial_event_rect),
            current_rect: rect_snapshot(current_rect),
            output_name,
            timestamp: std::time::Duration::from(data.clock.now()).as_millis() as u64,
        }
    }
}

impl PointerGrab<ShojiWM> for ResizeSurfaceGrab {
    fn motion(
        &mut self,
        data: &mut ShojiWM,
        handle: &mut PointerInnerHandle<'_, ShojiWM>,
        _focus: Option<(WlSurface, Point<f64, Logical>)>,
        event: &MotionEvent,
    ) {
        // While the grab is active, no client has pointer focus
        handle.motion(data, None, event);
        self.last_pointer = event.location;

        if self.runtime_managed {
            self.invoke_runtime_event(data, WindowResizePhaseSnapshot::Update, event.location);
            return;
        }

        let mut delta = event.location - self.start_data.location;

        let mut new_window_width = self.initial_rect.size.w;
        let mut new_window_height = self.initial_rect.size.h;

        if self.edges.intersects(ResizeEdge::LEFT | ResizeEdge::RIGHT) {
            if self.edges.intersects(ResizeEdge::LEFT) {
                delta.x = -delta.x;
            }

            new_window_width = (self.initial_rect.size.w as f64 + delta.x) as i32;
        }

        if self.edges.intersects(ResizeEdge::TOP | ResizeEdge::BOTTOM) {
            if self.edges.intersects(ResizeEdge::TOP) {
                delta.y = -delta.y;
            }

            new_window_height = (self.initial_rect.size.h as f64 + delta.y) as i32;
        }

        let Some(toplevel_surface) = self.window.toplevel() else {
            return;
        };
        let (min_size, max_size) =
            compositor::with_states(toplevel_surface.wl_surface(), |states| {
                let mut guard = states.cached_state.get::<SurfaceCachedState>();
                let data = guard.current();
                (data.min_size, data.max_size)
            });

        let min_width = min_size.w.max(1);
        let min_height = min_size.h.max(1);

        let max_width = if max_size.w == 0 {
            i32::MAX
        } else {
            max_size.w
        };
        let max_height = if max_size.h == 0 {
            i32::MAX
        } else {
            max_size.h
        };

        self.last_window_size = Size::from((
            new_window_width.max(min_width).min(max_width),
            new_window_height.max(min_height).min(max_height),
        ));

        let xdg = toplevel_surface;
        xdg.with_pending_state(|state| {
            state.states.set(xdg_toplevel::State::Resizing);
            state.size = Some(self.last_window_size);
        });

        xdg.send_pending_configure();
    }

    fn relative_motion(
        &mut self,
        data: &mut ShojiWM,
        handle: &mut PointerInnerHandle<'_, ShojiWM>,
        focus: Option<(WlSurface, Point<f64, Logical>)>,
        event: &RelativeMotionEvent,
    ) {
        handle.relative_motion(data, focus, event);
    }

    fn button(
        &mut self,
        data: &mut ShojiWM,
        handle: &mut PointerInnerHandle<'_, ShojiWM>,
        event: &ButtonEvent,
    ) {
        handle.button(data, event);

        // The button is a button code as defined in the
        // Linux kernel's linux/input-event-codes.h header file, e.g. BTN_LEFT.
        const BTN_LEFT: u32 = 0x110;

        if !handle.current_pressed().contains(&BTN_LEFT) {
            // No more buttons are pressed, release the grab.
            handle.unset_grab(self, data, event.serial, event.time, true);

            if self.runtime_managed {
                self.invoke_runtime_event(data, WindowResizePhaseSnapshot::End, self.last_pointer);
                if let Some(xdg) = self.window.toplevel() {
                    xdg.with_pending_state(|state| {
                        state.states.unset(xdg_toplevel::State::Resizing);
                    });
                    xdg.send_pending_configure();
                    ResizeSurfaceState::with(xdg.wl_surface(), |state| {
                        *state = ResizeSurfaceState::Idle;
                    });
                }
            } else if let Some(xdg) = self.window.toplevel() {
                xdg.with_pending_state(|state| {
                    state.states.unset(xdg_toplevel::State::Resizing);
                    state.size = Some(self.last_window_size);
                });

                xdg.send_pending_configure();

                ResizeSurfaceState::with(xdg.wl_surface(), |state| {
                    *state = ResizeSurfaceState::WaitingForLastCommit {
                        edges: self.edges,
                        initial_rect: self.initial_rect,
                    };
                });
            }
        }
    }

    fn axis(
        &mut self,
        data: &mut ShojiWM,
        handle: &mut PointerInnerHandle<'_, ShojiWM>,
        details: AxisFrame,
    ) {
        handle.axis(data, details)
    }

    fn frame(&mut self, data: &mut ShojiWM, handle: &mut PointerInnerHandle<'_, ShojiWM>) {
        handle.frame(data);
    }

    fn gesture_swipe_begin(
        &mut self,
        data: &mut ShojiWM,
        handle: &mut PointerInnerHandle<'_, ShojiWM>,
        event: &GestureSwipeBeginEvent,
    ) {
        handle.gesture_swipe_begin(data, event)
    }

    fn gesture_swipe_update(
        &mut self,
        data: &mut ShojiWM,
        handle: &mut PointerInnerHandle<'_, ShojiWM>,
        event: &GestureSwipeUpdateEvent,
    ) {
        handle.gesture_swipe_update(data, event)
    }

    fn gesture_swipe_end(
        &mut self,
        data: &mut ShojiWM,
        handle: &mut PointerInnerHandle<'_, ShojiWM>,
        event: &GestureSwipeEndEvent,
    ) {
        handle.gesture_swipe_end(data, event)
    }

    fn gesture_pinch_begin(
        &mut self,
        data: &mut ShojiWM,
        handle: &mut PointerInnerHandle<'_, ShojiWM>,
        event: &GesturePinchBeginEvent,
    ) {
        handle.gesture_pinch_begin(data, event)
    }

    fn gesture_pinch_update(
        &mut self,
        data: &mut ShojiWM,
        handle: &mut PointerInnerHandle<'_, ShojiWM>,
        event: &GesturePinchUpdateEvent,
    ) {
        handle.gesture_pinch_update(data, event)
    }

    fn gesture_pinch_end(
        &mut self,
        data: &mut ShojiWM,
        handle: &mut PointerInnerHandle<'_, ShojiWM>,
        event: &GesturePinchEndEvent,
    ) {
        handle.gesture_pinch_end(data, event)
    }

    fn gesture_hold_begin(
        &mut self,
        data: &mut ShojiWM,
        handle: &mut PointerInnerHandle<'_, ShojiWM>,
        event: &GestureHoldBeginEvent,
    ) {
        handle.gesture_hold_begin(data, event)
    }

    fn gesture_hold_end(
        &mut self,
        data: &mut ShojiWM,
        handle: &mut PointerInnerHandle<'_, ShojiWM>,
        event: &GestureHoldEndEvent,
    ) {
        handle.gesture_hold_end(data, event)
    }

    fn start_data(&self) -> &PointerGrabStartData<ShojiWM> {
        &self.start_data
    }

    fn unset(&mut self, _data: &mut ShojiWM) {}
}

fn resize_rect_for_delta(
    initial: Rectangle<i32, Logical>,
    edges: ResizeEdge,
    delta: Point<f64, Logical>,
) -> Rectangle<i32, Logical> {
    let mut x = initial.loc.x;
    let mut y = initial.loc.y;
    let mut width = initial.size.w;
    let mut height = initial.size.h;

    if edges.intersects(ResizeEdge::LEFT) {
        let dx = delta.x.round() as i32;
        x += dx;
        width -= dx;
    } else if edges.intersects(ResizeEdge::RIGHT) {
        width += delta.x.round() as i32;
    }

    if edges.intersects(ResizeEdge::TOP) {
        let dy = delta.y.round() as i32;
        y += dy;
        height -= dy;
    } else if edges.intersects(ResizeEdge::BOTTOM) {
        height += delta.y.round() as i32;
    }

    Rectangle::new((x, y).into(), (width.max(1), height.max(1)).into())
}

fn resize_edges_snapshot(edges: ResizeEdge) -> WindowResizeEdgesSnapshot {
    WindowResizeEdgesSnapshot {
        left: edges.intersects(ResizeEdge::LEFT),
        right: edges.intersects(ResizeEdge::RIGHT),
        top: edges.intersects(ResizeEdge::TOP),
        bottom: edges.intersects(ResizeEdge::BOTTOM),
    }
}

fn point_snapshot(point: Point<f64, Logical>) -> WindowResizePointSnapshot {
    WindowResizePointSnapshot {
        x: point.x.round() as i32,
        y: point.y.round() as i32,
    }
}

fn rect_snapshot(rect: Rectangle<i32, Logical>) -> WindowPositionSnapshot {
    WindowPositionSnapshot {
        x: rect.loc.x,
        y: rect.loc.y,
        width: rect.size.w,
        height: rect.size.h,
    }
}

/// State of the resize operation.
///
/// It is stored inside of WlSurface,
/// and can be accessed using [`ResizeSurfaceState::with`]
#[derive(Debug, Clone, Copy, Eq, PartialEq, Default)]
enum ResizeSurfaceState {
    #[default]
    Idle,
    Resizing {
        edges: ResizeEdge,
        /// The initial window size and location.
        initial_rect: Rectangle<i32, Logical>,
    },
    /// Resize is done, we are now waiting for last commit, to do the final move
    WaitingForLastCommit {
        edges: ResizeEdge,
        /// The initial window size and location.
        initial_rect: Rectangle<i32, Logical>,
    },
}

impl ResizeSurfaceState {
    fn with<F, T>(surface: &WlSurface, cb: F) -> T
    where
        F: FnOnce(&mut Self) -> T,
    {
        compositor::with_states(surface, |states| {
            states.data_map.insert_if_missing(RefCell::<Self>::default);
            let state = states.data_map.get::<RefCell<Self>>().unwrap();

            cb(&mut state.borrow_mut())
        })
    }

    fn commit(&mut self) -> Option<(ResizeEdge, Rectangle<i32, Logical>)> {
        match *self {
            Self::Resizing {
                edges,
                initial_rect,
            } => Some((edges, initial_rect)),
            Self::WaitingForLastCommit {
                edges,
                initial_rect,
            } => {
                // The resize is done, let's go back to idle
                *self = Self::Idle;

                Some((edges, initial_rect))
            }
            Self::Idle => None,
        }
    }
}

/// Should be called on `WlSurface::commit`
pub fn handle_commit(space: &mut Space<Window>, surface: &WlSurface) -> Option<()> {
    let window = space
        .elements()
        .find(|w| w.toplevel().is_some_and(|t| t.wl_surface() == surface))
        .cloned()?;

    let mut window_loc = space.element_location(&window)?;
    let geometry = window.geometry();

    let new_loc: Point<Option<i32>, Logical> = ResizeSurfaceState::with(surface, |state| {
        state
            .commit()
            .and_then(|(edges, initial_rect)| {
                // If the window is being resized by top or left, its location must be adjusted
                // accordingly.
                edges.intersects(ResizeEdge::TOP_LEFT).then(|| {
                    let new_x = edges
                        .intersects(ResizeEdge::LEFT)
                        .then_some(initial_rect.loc.x + (initial_rect.size.w - geometry.size.w));

                    let new_y = edges
                        .intersects(ResizeEdge::TOP)
                        .then_some(initial_rect.loc.y + (initial_rect.size.h - geometry.size.h));

                    (new_x, new_y).into()
                })
            })
            .unwrap_or_default()
    });

    if let Some(new_x) = new_loc.x {
        window_loc.x = new_x;
    }
    if let Some(new_y) = new_loc.y {
        window_loc.y = new_y;
    }

    if new_loc.x.is_some() || new_loc.y.is_some() {
        // If TOP or LEFT side of the window got resized, we have to move it
        space.map_element(window, window_loc, false);
    }

    Some(())
}
