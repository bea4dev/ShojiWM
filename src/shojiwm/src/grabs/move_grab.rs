//! Move grab is the state of a composer during which the client window is being dragged around.
//!
//! eg. Usually whenever a user clicks on the app's titlebar and starts dragging, the compositors
//! enters a MoveSurfaceGrab state.

use crate::{
    ssd::{
        LogicalRect, PointerModifierStateSnapshot, WindowMoveEventSnapshot,
        WindowMovePhaseSnapshot, WindowMoveSourceSnapshot, WindowPositionSnapshot,
        WindowResizePointSnapshot,
    },
    state::ShojiWM,
};
use smithay::{
    desktop::Window,
    input::pointer::{
        AxisFrame, ButtonEvent, CursorIcon, GestureHoldBeginEvent, GestureHoldEndEvent,
        GesturePinchBeginEvent, GesturePinchEndEvent, GesturePinchUpdateEvent,
        GestureSwipeBeginEvent, GestureSwipeEndEvent, GestureSwipeUpdateEvent,
        GrabStartData as PointerGrabStartData, MotionEvent, PointerGrab, PointerInnerHandle,
        RelativeMotionEvent,
    },
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::{Logical, Point},
};

pub struct MoveSurfaceGrab {
    pub start_data: PointerGrabStartData<ShojiWM>,
    pub window: Window,
    pub initial_window_location: Point<i32, Logical>,
    pub initial_event_rect: smithay::utils::Rectangle<i32, Logical>,
    pub source: WindowMoveSourceSnapshot,
    pub runtime_managed: bool,
    pub last_pointer: Point<f64, Logical>,
}

impl MoveSurfaceGrab {
    pub fn start(
        start_data: PointerGrabStartData<ShojiWM>,
        window: Window,
        initial_window_location: Point<i32, Logical>,
        initial_event_rect: smithay::utils::Rectangle<i32, Logical>,
        source: WindowMoveSourceSnapshot,
    ) -> Self {
        let last_pointer = start_data.location;
        Self {
            start_data,
            window,
            initial_window_location,
            initial_event_rect,
            source,
            runtime_managed: false,
            last_pointer,
        }
    }

    pub fn notify_start(&mut self, data: &mut ShojiWM) {
        data.cursor_override = Some(CursorIcon::Grabbing);
        data.schedule_redraw();
        self.runtime_managed = self.invoke_runtime_event(
            data,
            WindowMovePhaseSnapshot::Start,
            self.start_data.location,
        );
    }

    fn invoke_runtime_event(
        &self,
        data: &mut ShojiWM,
        phase: WindowMovePhaseSnapshot,
        current_pointer: Point<f64, Logical>,
    ) -> bool {
        let window_id = data.snapshot_window(&self.window).id;
        let event = self.runtime_event(data, phase, current_pointer);
        let now_ms = std::time::Duration::from(data.clock.now()).as_millis() as u64;
        data.invoke_window_move_event(&window_id, &event, now_ms)
    }

    fn runtime_event(
        &self,
        data: &ShojiWM,
        phase: WindowMovePhaseSnapshot,
        current_pointer: Point<f64, Logical>,
    ) -> WindowMoveEventSnapshot {
        let start_pointer = self.start_data.location;
        let delta = current_pointer - start_pointer;
        let current_rect = move_rect_for_delta(self.initial_event_rect, delta);
        let output_name = data
            .space
            .outputs()
            .find(|output| {
                data.space
                    .output_geometry(output)
                    .is_some_and(|geometry| geometry.contains(current_pointer.to_i32_floor()))
            })
            .map(|output| output.name());

        WindowMoveEventSnapshot {
            source: self.source,
            phase,
            start_pointer: point_snapshot(start_pointer),
            current_pointer: point_snapshot(current_pointer),
            delta: point_snapshot(delta),
            start_rect: rect_snapshot(self.initial_event_rect),
            current_rect,
            output_name,
            modifiers: PointerModifierStateSnapshot {
                logo: data.current_keyboard_modifiers.logo,
                alt: data.current_keyboard_modifiers.alt,
                ctrl: data.current_keyboard_modifiers.ctrl,
                shift: data.current_keyboard_modifiers.shift,
            },
            timestamp: std::time::Duration::from(data.clock.now()).as_millis() as u64,
        }
    }
}

impl PointerGrab<ShojiWM> for MoveSurfaceGrab {
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
            self.invoke_runtime_event(data, WindowMovePhaseSnapshot::Update, event.location);
            return;
        }

        let delta = event.location - self.start_data.location;
        let new_location = self.initial_window_location.to_f64() + delta;
        // Unlike the runtime-managed path above, this fallback places the
        // window directly in the `Space`, whose element locations are whole
        // logical pixels, and a window without a managed rect has nowhere to
        // carry a fractional remainder. So this path keeps the coarser step;
        // the sub-pixel precision lives in `WindowMoveEventSnapshot`, which
        // config code turns into a managed rect.
        let new_location = new_location.to_i32_round();
        let old_location = data
            .space
            .element_location(&self.window)
            .unwrap_or(self.initial_window_location);

        if old_location != new_location {
            let window_id = data.snapshot_window(&self.window).id;
            let move_delta_x = new_location.x - old_location.x;
            let move_delta_y = new_location.y - old_location.y;
            let (old_source_rect, new_source_rect) =
                if let Some(decoration) = data.window_decorations.get(&self.window) {
                    let old_root = decoration.layout.root.rect;
                    (
                        old_root,
                        LogicalRect::new(
                            old_root.x + move_delta_x,
                            old_root.y + move_delta_y,
                            old_root.width,
                            old_root.height,
                        ),
                    )
                } else {
                    let bbox = self.window.bbox();
                    let old_rect = LogicalRect::new(
                        old_location.x + bbox.loc.x,
                        old_location.y + bbox.loc.y,
                        bbox.size.w,
                        bbox.size.h,
                    );
                    let new_rect = LogicalRect::new(
                        new_location.x + bbox.loc.x,
                        new_location.y + bbox.loc.y,
                        bbox.size.w,
                        bbox.size.h,
                    );
                    (old_rect, new_rect)
                };
            if let Some(decoration) = data.window_decorations.get(&self.window) {
                let old_root = decoration.layout.root.rect;
                let new_root = LogicalRect::new(
                    old_root.x + move_delta_x,
                    old_root.y + move_delta_y,
                    old_root.width,
                    old_root.height,
                );
                data.pending_decoration_damage.push(old_root);
                data.pending_decoration_damage.push(new_root);
            }

            for output in data.space.outputs() {
                if let Some(output_geo) = data.space.output_geometry(output) {
                    data.pending_decoration_damage.push(LogicalRect::new(
                        output_geo.loc.x,
                        output_geo.loc.y,
                        output_geo.size.w,
                        output_geo.size.h,
                    ));
                }
            }

            data.space
                .map_element(self.window.clone(), new_location, true);
            data.update_xwayland_refresh_override_for_window(&self.window, "window-move");
            data.window_source_damage
                .push(crate::state::OwnedDamageRect {
                    owner: window_id.clone(),
                    rect: old_source_rect,
                });
            data.window_source_damage
                .push(crate::state::OwnedDamageRect {
                    owner: window_id.clone(),
                    rect: new_source_rect,
                });
            data.window_scene_generation = data.window_scene_generation.wrapping_add(1);
            data.schedule_redraw();
        }
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
                self.invoke_runtime_event(data, WindowMovePhaseSnapshot::End, self.last_pointer);
            }
            data.update_decoration_cursor_icon(self.last_pointer);
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

    fn unset(&mut self, data: &mut ShojiWM) {
        data.update_decoration_cursor_icon(self.last_pointer);
    }
}

/// Offsets the rect the drag started from by the raw pointer delta.
///
/// The delta stays fractional on purpose. Rounding it to whole logical pixels
/// used to quantize the dragged window to a 1.5 (or 1.8) physical pixel grid
/// depending on the output scale, which is coarser than the single physical
/// pixel the cursor itself moves in — so the window stepped in visible jumps
/// and slipped against the cursor it was supposed to be stuck to.
fn move_rect_for_delta(
    initial: smithay::utils::Rectangle<i32, Logical>,
    delta: Point<f64, Logical>,
) -> WindowPositionSnapshot {
    WindowPositionSnapshot {
        x: initial.loc.x as f64 + delta.x,
        y: initial.loc.y as f64 + delta.y,
        width: initial.size.w as f64,
        height: initial.size.h as f64,
    }
}

fn point_snapshot(point: Point<f64, Logical>) -> WindowResizePointSnapshot {
    WindowResizePointSnapshot {
        x: point.x,
        y: point.y,
    }
}

fn rect_snapshot(rect: smithay::utils::Rectangle<i32, Logical>) -> WindowPositionSnapshot {
    WindowPositionSnapshot {
        x: rect.loc.x as f64,
        y: rect.loc.y as f64,
        width: rect.size.w as f64,
        height: rect.size.h as f64,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn move_rect_keeps_the_pointer_delta_fractional() {
        // The pointer moves in fractions of a logical pixel (one physical
        // pixel is 1/scale of one). Rounding here pinned dragged windows to
        // the integer logical grid, which at scale 1.5 is a 1.5 physical pixel
        // step — coarser than the cursor's own, so the window lagged behind in
        // visible jumps. The fraction has to reach the managed rect, whose
        // remainder anchors the rendered origin.
        let initial =
            smithay::utils::Rectangle::<i32, Logical>::new((763, 349).into(), (968, 813).into());
        let rect = move_rect_for_delta(initial, Point::from((0.4, -0.7)));
        assert_eq!(rect.x, 763.4);
        assert_eq!(rect.y, 348.3);
        // The size is carried through untouched: a move never resizes.
        assert_eq!(rect.width, 968.0);
        assert_eq!(rect.height, 813.0);
    }

    #[test]
    fn pointer_snapshot_is_not_quantized() {
        let snapshot = point_snapshot(Point::from((1280.25, 719.75)));
        assert_eq!(snapshot.x, 1280.25);
        assert_eq!(snapshot.y, 719.75);
    }
}
