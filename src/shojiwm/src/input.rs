use smithay::{
    backend::{
        input::{
            AbsolutePositionEvent, Axis, AxisSource, ButtonState, Event, GestureBeginEvent,
            GestureEndEvent, GestureSwipeUpdateEvent, InputBackend, InputEvent, KeyState,
            KeyboardKeyEvent, PointerAxisEvent, PointerButtonEvent, PointerMotionEvent,
        },
        session::Session,
    },
    desktop::{Window, WindowSurfaceType},
    input::{
        keyboard::{FilterResult, keysyms},
        pointer::{
            AxisFrame, ButtonEvent, CursorIcon, GrabStartData as PointerGrabStartData, MotionEvent,
            RelativeMotionEvent,
        },
    },
    reexports::{
        wayland_protocols::xdg::shell::server::xdg_toplevel,
        wayland_server::{Resource, protocol::wl_surface::WlSurface},
    },
    utils::{Logical, Point, Rectangle, SERIAL_COUNTER, Serial},
    wayland::pointer_constraints::{PointerConstraint, with_pointer_constraint},
};
use std::time::Instant;
use tracing::{debug, info, warn};

use crate::{
    backend::visual::{inverse_transform_point, transformed_root_rect},
    grabs::{
        move_grab::MoveSurfaceGrab,
        resize_grab::{ResizeEdge, ResizeSurfaceGrab},
    },
    ssd::{
        DecorationEvaluator, DecorationHitTestResult, GestureSwipeEventSnapshot,
        GestureSwipePhaseSnapshot, LogicalPoint, PointerModifierStateSnapshot,
        PointerMoveEventSnapshot, PointerMovePointSnapshot, ResizeEdges, RuntimeWindowAction,
        WindowAction, WindowMoveSourceSnapshot, WindowResizeSourceSnapshot,
    },
    state::{ShojiWM, TrackedDecorationInteractionTarget},
};

enum KeyboardAction {
    Forward,
    Quit,
    ReloadConfig,
    RuntimeKeyBinding(String),
    LogMarker(u8),
    ChangeVt(i32),
}

fn compositor_pointer_grab_start_data(
    focus: Option<(WlSurface, Point<f64, Logical>)>,
    button: u32,
    location: Point<f64, Logical>,
) -> PointerGrabStartData<ShojiWM> {
    PointerGrabStartData {
        focus,
        button,
        location,
    }
}

fn layer_focus_debug_enabled() -> bool {
    std::env::var_os("SHOJI_LAYER_FOCUS_DEBUG").is_some()
}

fn pointer_button_debug_enabled() -> bool {
    std::env::var_os("SHOJI_POINTER_BUTTON_DEBUG").is_some()
}

const POINTER_TRAILING_EDGE_INSET: f64 = 0.5;

/// Projects a logical pointer position onto the union of all output rectangles.
///
/// The half-pixel inset keeps the projected point strictly inside a trailing
/// edge while still flooring to its final logical pixel. Pointer hit tests use
/// `to_i32_floor()` for the corresponding containment rule.
fn constrain_pointer_location_to_outputs(
    location: Point<f64, Logical>,
    output_geometries: impl IntoIterator<Item = Rectangle<i32, Logical>>,
) -> Option<Point<f64, Logical>> {
    let mut nearest = None;

    for geometry in output_geometries {
        if geometry.contains(location.to_i32_floor()) {
            return Some(location);
        }

        let min_x = geometry.loc.x as f64;
        let min_y = geometry.loc.y as f64;
        let max_x = (geometry.loc.x + geometry.size.w) as f64 - POINTER_TRAILING_EDGE_INSET;
        let max_y = (geometry.loc.y + geometry.size.h) as f64 - POINTER_TRAILING_EDGE_INSET;
        let candidate = Point::from((
            location.x.clamp(min_x, max_x),
            location.y.clamp(min_y, max_y),
        ));
        let delta = location - candidate;
        let distance_squared = delta.x * delta.x + delta.y * delta.y;

        if nearest
            .as_ref()
            .is_none_or(|(_, nearest_distance)| distance_squared < *nearest_distance)
        {
            nearest = Some((candidate, distance_squared));
        }
    }

    nearest.map(|(location, _)| location)
}

/// Classify a raw keysym as a modifier key (for modifier-tap detection),
/// independent of the left/right physical key.
fn modifier_class_of_keysym(keysym: u32) -> Option<crate::runtime_key_binding::ModifierClass> {
    use crate::runtime_key_binding::ModifierClass;
    match keysym {
        keysyms::KEY_Super_L | keysyms::KEY_Super_R | keysyms::KEY_Meta_L => {
            Some(ModifierClass::Logo)
        }
        keysyms::KEY_Control_L | keysyms::KEY_Control_R => Some(ModifierClass::Ctrl),
        keysyms::KEY_Alt_L | keysyms::KEY_Alt_R => Some(ModifierClass::Alt),
        keysyms::KEY_Shift_L | keysyms::KEY_Shift_R => Some(ModifierClass::Shift),
        _ => None,
    }
}

fn unfocused_popup_focus_debug_enabled() -> bool {
    std::env::var_os("SHOJI_UNFOCUSED_POPUP_FOCUS_DEBUG")
        .is_some_and(|value| value != "0" && !value.is_empty())
}

fn stack_hit_debug_enabled() -> bool {
    std::env::var_os("SHOJI_STACK_HIT_DEBUG").is_some_and(|value| value != "0" && !value.is_empty())
}

impl ShojiWM {
    fn input_event_counts_as_idle_activity<I: InputBackend>(event: &InputEvent<I>) -> bool {
        !matches!(
            event,
            InputEvent::DeviceAdded { .. }
                | InputEvent::DeviceRemoved { .. }
                | InputEvent::Special(_)
        )
    }

    fn notify_idle_activity(&mut self) {
        self.refresh_idle_inhibit_state();
        let seat = self.seat.clone();
        self.idle_notifier_state.notify_activity(&seat);
    }

    fn forward_locked_keyboard_event<I: InputBackend>(
        &mut self,
        event: &I::KeyboardKeyEvent,
        serial: Serial,
    ) {
        let _ = self.seat.get_keyboard().unwrap().input(
            self,
            event.key_code(),
            event.state(),
            serial,
            Event::time_msec(event),
            |data, modifiers, _handle| {
                data.current_keyboard_modifiers = *modifiers;
                FilterResult::<KeyboardAction>::Forward
            },
        );
        let _ = self.display_handle.flush_clients();
    }

    fn forward_locked_pointer_motion(
        &mut self,
        pos: smithay::utils::Point<f64, smithay::utils::Logical>,
        serial: Serial,
        time: u32,
    ) {
        let pointer = self.seat.get_pointer().unwrap();
        self.pointer_contents = self.pointer_contents_at(pos);
        pointer.motion(
            self,
            self.pointer_contents.surface.clone(),
            &MotionEvent {
                location: pos,
                serial,
                time,
            },
        );
        pointer.frame(self);
        self.schedule_redraw();
    }

    fn forward_locked_pointer_button(
        &mut self,
        button: u32,
        state: ButtonState,
        serial: Serial,
        time: u32,
    ) {
        let pointer = self.seat.get_pointer().unwrap();
        self.pointer_contents = self.pointer_contents_at(pointer.current_location());
        pointer.motion(
            self,
            self.pointer_contents.surface.clone(),
            &MotionEvent {
                location: pointer.current_location(),
                serial,
                time,
            },
        );
        pointer.button(
            self,
            &ButtonEvent {
                button,
                state,
                serial,
                time,
            },
        );
        pointer.frame(self);
        self.focus_session_lock_surface(serial);
        let _ = self.display_handle.flush_clients();
        self.schedule_redraw();
    }

    /// Invoke a runtime key binding handler by id and apply the resulting
    /// runtime config/state changes. Shared by the normal (intercepted) key
    /// path and the modifier-tap (forwarded) path.
    fn run_runtime_key_binding(&mut self, binding_id: &str) {
        let now_ms = std::time::Duration::from(self.clock.now()).as_millis() as u64;
        self.sync_runtime_display_state();
        match self
            .decoration_evaluator
            .invoke_key_binding(binding_id, now_ms)
        {
            Ok(invocation) => {
                self.consume_runtime_display_config(invocation.display_config);
                self.consume_runtime_workspace_config(invocation.workspace_config);
                self.consume_runtime_key_binding_config(invocation.key_binding_config);
                self.consume_runtime_pointer_config(invocation.pointer_config);
                self.consume_runtime_input_config(invocation.input_config);
                self.consume_runtime_event_config(invocation.event_config);
                self.consume_runtime_process_config(invocation.process_config);
                self.consume_runtime_debug_config(invocation.debug_config);
                if !invocation.process_actions.is_empty() {
                    self.apply_runtime_process_actions(invocation.process_actions);
                }
                if invocation.dirty {
                    self.runtime_poll_dirty = true;
                    self.mark_runtime_dirty_windows(
                        invocation.dirty_window_ids,
                        invocation.dirty_managed_window_ids,
                    );
                    self.request_tty_maintenance("runtime-key-binding-dirty");
                    self.schedule_redraw();
                }
                if !invocation.actions.is_empty() {
                    self.request_tty_maintenance("runtime-key-binding-actions");
                    self.apply_runtime_window_actions(invocation.actions);
                    self.schedule_redraw();
                }
                self.runtime_scheduler_enabled = invocation.next_poll_in_ms.is_some();
                if invocation.next_poll_in_ms == Some(0) {
                    self.request_tty_maintenance("runtime-key-binding-animation");
                    self.schedule_redraw();
                }
            }
            Err(error) => {
                tracing::warn!(?error, binding_id, "failed to invoke runtime key binding");
                self.config_error_report =
                    Some(crate::config_error::ConfigErrorReport::runtime(error));
                self.schedule_redraw();
            }
        }
    }

    fn dispatch_pointer_move_events(
        &mut self,
        previous_pos: smithay::utils::Point<f64, smithay::utils::Logical>,
        pos: smithay::utils::Point<f64, smithay::utils::Logical>,
        time_msec: u32,
    ) {
        if !self.runtime_pointer_move_enabled && !self.runtime_pointer_move_async_enabled {
            return;
        }

        let output_name = self.output_at_point(pos).map(|output| output.name());
        let delta = pos - previous_pos;
        let event = PointerMoveEventSnapshot {
            position: PointerMovePointSnapshot { x: pos.x, y: pos.y },
            delta: PointerMovePointSnapshot {
                x: delta.x,
                y: delta.y,
            },
            target: self.pointer_hit_target_snapshot(pos),
            output_name,
            modifiers: PointerModifierStateSnapshot {
                logo: self.current_keyboard_modifiers.logo,
                alt: self.current_keyboard_modifiers.alt,
                ctrl: self.current_keyboard_modifiers.ctrl,
                shift: self.current_keyboard_modifiers.shift,
            },
            timestamp: u64::from(time_msec),
        };
        let now_ms = std::time::Duration::from(self.clock.now()).as_millis() as u64;
        if self.runtime_pointer_move_enabled {
            match self.decoration_evaluator.pointer_move(&event, now_ms) {
                Ok(invocation) => self.handle_runtime_pointer_move_invocation(invocation),
                Err(error) => {
                    tracing::warn!(?error, "runtime pointer move event failed");
                }
            }
        }
        if self.runtime_pointer_move_async_enabled {
            self.decoration_evaluator.pointer_move_async(event, now_ms);
        }
    }

    fn pointer_hit_target_snapshot(
        &self,
        pos: smithay::utils::Point<f64, smithay::utils::Logical>,
    ) -> crate::ssd::PointerHitTargetSnapshot {
        if let Some(layer) = self.pointer_contents.layer.as_ref() {
            return crate::ssd::PointerHitTargetSnapshot::Layer {
                layer_id: crate::ssd::layer_runtime_id(layer),
            };
        }

        let surface_window = self
            .pointer_contents
            .surface
            .as_ref()
            .and_then(|(surface, _)| self.window_for_pointer_surface(surface));
        let logical_pos = LogicalPoint::new(pos.x.floor() as i32, pos.y.floor() as i32);
        let window = surface_window
            .or_else(|| {
                self.window_under_transformed(logical_pos)
                    .map(|(window, _)| window.clone())
            })
            .or_else(|| {
                self.raw_window_under(logical_pos)
                    .map(|(window, _)| window.clone())
            });

        window.map_or(crate::ssd::PointerHitTargetSnapshot::None, |window| {
            crate::ssd::PointerHitTargetSnapshot::Window {
                window_id: self.snapshot_window(&window).id,
            }
        })
    }

    fn dispatch_gesture_swipe_events(&mut self, event: GestureSwipeEventSnapshot) {
        if !self.runtime_gesture_swipe_enabled && !self.runtime_gesture_swipe_async_enabled {
            return;
        }
        let now_ms = std::time::Duration::from(self.clock.now()).as_millis() as u64;
        if self.runtime_gesture_swipe_enabled {
            match self.decoration_evaluator.gesture_swipe(&event, now_ms) {
                Ok(invocation) => self.handle_runtime_pointer_move_invocation(invocation),
                Err(error) => {
                    tracing::warn!(?error, "runtime gesture swipe event failed");
                }
            }
        }
        if self.runtime_gesture_swipe_async_enabled {
            self.decoration_evaluator.gesture_swipe_async(event, now_ms);
        }
    }

    pub fn process_input_event<I: InputBackend>(&mut self, event: InputEvent<I>) {
        if Self::input_event_counts_as_idle_activity(&event) {
            self.notify_idle_activity();
        }

        match event {
            InputEvent::Keyboard { event, .. } => {
                let device = event.device();
                crate::runtime_input::apply_keyboard_config_for_backend_device(self, &device);
                let serial = SERIAL_COUNTER.next_serial();
                if self.session_lock_active {
                    self.forward_locked_keyboard_event::<I>(&event, serial);
                    return;
                }
                let time = Event::time_msec(&event);
                let key_phase = match event.state() {
                    KeyState::Pressed => crate::runtime_key_binding::RuntimeKeyBindingPhase::Press,
                    KeyState::Released => {
                        crate::runtime_key_binding::RuntimeKeyBindingPhase::Release
                    }
                };
                let runtime_key_bindings = self.runtime_key_bindings.clone();

                let action = self
                    .seat
                    .get_keyboard()
                    .unwrap()
                    .input(
                        self,
                        event.key_code(),
                        event.state(),
                        serial,
                        time,
                        |data, modifiers, handle| {
                            data.current_keyboard_modifiers = *modifiers;

                            // --- modifier-only tap detection ---
                            // If no other key/button is pressed between press and
                            // release, fire modifier-only bindings as a "tap" on
                            // release. The modifier key itself is still forwarded
                            // normally (not consumed here).
                            let tap_class = handle
                                .raw_latin_sym_or_raw_current_sym()
                                .and_then(|sym| modifier_class_of_keysym(sym.raw()));
                            match key_phase {
                                crate::runtime_key_binding::RuntimeKeyBindingPhase::Press => {
                                    let was_idle = data.tap_pressed_keys == 0;
                                    data.tap_pressed_keys += 1;
                                    match tap_class {
                                        Some(class) if was_idle => {
                                            data.tap_armed_modifier = Some(class);
                                            data.tap_interrupted = false;
                                        }
                                        _ => {
                                            data.tap_interrupted = true;
                                        }
                                    }
                                }
                                crate::runtime_key_binding::RuntimeKeyBindingPhase::Release => {
                                    if let Some(class) = tap_class
                                        && data.tap_armed_modifier == Some(class)
                                        && !data.tap_interrupted
                                    {
                                        for binding in runtime_key_bindings.iter() {
                                            if binding.phase
                                                == crate::runtime_key_binding::RuntimeKeyBindingPhase::Release
                                                && binding.shortcut.modifier_class()
                                                    == Some(class)
                                            {
                                                data.pending_tap_binding_ids
                                                    .push(binding.id.clone());
                                            }
                                        }
                                    }
                                    data.tap_pressed_keys =
                                        data.tap_pressed_keys.saturating_sub(1);
                                    if data.tap_pressed_keys == 0 {
                                        data.tap_armed_modifier = None;
                                        data.tap_interrupted = false;
                                    }
                                }
                            }

                            if let Some(binding_id) = runtime_key_bindings
                                .iter()
                                .find(|binding| binding.matches(key_phase, modifiers, &handle))
                                .map(|binding| binding.id.clone())
                            {
                                return FilterResult::Intercept(KeyboardAction::RuntimeKeyBinding(
                                    binding_id,
                                ));
                            }

                            if matches!(
                                key_phase,
                                crate::runtime_key_binding::RuntimeKeyBindingPhase::Press,
                            ) && modifiers.logo
                                && modifiers.shift
                                && !modifiers.ctrl
                                && !modifiers.alt
                                && let Some(raw) = handle.raw_latin_sym_or_raw_current_sym()
                                && raw.raw() == keysyms::KEY_q
                            {
                                FilterResult::Intercept(KeyboardAction::Quit)
                            } else if matches!(
                                key_phase,
                                crate::runtime_key_binding::RuntimeKeyBindingPhase::Press,
                            ) && modifiers.logo
                                && modifiers.shift
                                && !modifiers.ctrl
                                && !modifiers.alt
                                && let Some(raw) = handle.raw_latin_sym_or_raw_current_sym()
                                && raw.raw() == keysyms::KEY_r
                            {
                                FilterResult::Intercept(KeyboardAction::ReloadConfig)
                            } else if matches!(
                                key_phase,
                                crate::runtime_key_binding::RuntimeKeyBindingPhase::Press,
                            ) && modifiers.logo
                                && modifiers.shift
                                && !modifiers.ctrl
                                && !modifiers.alt
                                && let Some(raw) = handle.raw_latin_sym_or_raw_current_sym()
                                && let Some(digit) = match raw.raw() {
                                    keysyms::KEY_0 => Some(0u8),
                                    keysyms::KEY_1 => Some(1),
                                    keysyms::KEY_2 => Some(2),
                                    keysyms::KEY_3 => Some(3),
                                    keysyms::KEY_4 => Some(4),
                                    keysyms::KEY_5 => Some(5),
                                    keysyms::KEY_6 => Some(6),
                                    keysyms::KEY_7 => Some(7),
                                    keysyms::KEY_8 => Some(8),
                                    keysyms::KEY_9 => Some(9),
                                    _ => None,
                                }
                            {
                                FilterResult::Intercept(KeyboardAction::LogMarker(digit))
                            } else if matches!(
                                key_phase,
                                crate::runtime_key_binding::RuntimeKeyBindingPhase::Press,
                            ) && modifiers.ctrl
                                && modifiers.alt
                                && !modifiers.shift
                                && !modifiers.logo
                                && let Some(raw) = handle.raw_latin_sym_or_raw_current_sym()
                                && let Some(vt) = match raw.raw() {
                                    keysyms::KEY_F1 => Some(1),
                                    keysyms::KEY_F2 => Some(2),
                                    keysyms::KEY_F3 => Some(3),
                                    keysyms::KEY_F4 => Some(4),
                                    keysyms::KEY_F5 => Some(5),
                                    keysyms::KEY_F6 => Some(6),
                                    keysyms::KEY_F7 => Some(7),
                                    keysyms::KEY_F8 => Some(8),
                                    keysyms::KEY_F9 => Some(9),
                                    keysyms::KEY_F10 => Some(10),
                                    keysyms::KEY_F11 => Some(11),
                                    keysyms::KEY_F12 => Some(12),
                                    _ => None,
                                }
                            {
                                FilterResult::Intercept(KeyboardAction::ChangeVt(vt))
                            } else {
                                FilterResult::Forward
                            }
                        },
                    )
                    .unwrap_or(KeyboardAction::Forward);

                match action {
                    KeyboardAction::Quit => self.shutdown(),
                    KeyboardAction::ReloadConfig => self.reload_decoration_runtime(),
                    KeyboardAction::RuntimeKeyBinding(binding_id) => {
                        self.run_runtime_key_binding(&binding_id);
                    }
                    KeyboardAction::LogMarker(digit) => {
                        tracing::info!(marker = digit, "log marker");
                    }
                    KeyboardAction::ChangeVt(vt) => {
                        if let Some(ref mut session) = self.tty_session {
                            if let Err(err) = session.change_vt(vt) {
                                warn!(?err, vt, "failed to change VT");
                            }
                        } else {
                            debug!(vt, "ignoring VT change req (no tty  session)")
                        }
                    }
                    KeyboardAction::Forward => {}
                }

                // Fire any modifier-only tap bindings queued above (the release was already forwarded).
                if !self.pending_tap_binding_ids.is_empty() {
                    let ids = std::mem::take(&mut self.pending_tap_binding_ids);
                    for id in ids {
                        self.run_runtime_key_binding(&id);
                    }
                }
            }
            InputEvent::PointerMotion { event, .. } => {
                let pointer = self.seat.get_pointer().unwrap();
                let previous_pos = pointer.current_location();
                // Keep using the last focus while a constraint is active. Re-running hit testing
                // here can briefly return a different surface while a client updates its surface
                // tree, which would make a locked pointer move for one event.
                let under = self.pointer_contents.surface.clone();
                let mut pointer_locked = false;
                let mut pointer_confined = false;
                let mut confine_region = None;

                if let Some((surface, surface_origin)) = under.as_ref()
                    && pointer.current_focus().as_ref() == Some(surface) {
                        with_pointer_constraint(surface, &pointer, |constraint| {
                            let Some(constraint) =
                                constraint.filter(|constraint| constraint.is_active())
                            else {
                                return;
                            };
                            if !constraint.region().is_none_or(|region| {
                                // Floor, not round: the pointer clamp above can
                                // sit half a logical pixel inside the trailing
                                // edge, and rounding would carry that back out
                                // of the region it is actually within.
                                region.contains((previous_pos - *surface_origin).to_i32_floor())
                            }) {
                                return;
                            }
                            match &*constraint {
                                PointerConstraint::Locked(_) => pointer_locked = true,
                                PointerConstraint::Confined(confined) => {
                                    pointer_confined = true;
                                    confine_region = confined.region().cloned();
                                }
                            }
                        });
                    }

                if pointer_locked {
                    pointer.relative_motion(
                        self,
                        under,
                        &RelativeMotionEvent {
                            delta: event.delta(),
                            delta_unaccel: event.delta_unaccel(),
                            utime: event.time(),
                        },
                    );
                    pointer.frame(self);
                    // A locked pointer (e.g. an FPS-style game grabbing the cursor) does not move
                    // the compositor cursor, so it intentionally does not schedule a redraw. Flush
                    // the relative event to the client immediately instead of waiting for an
                    // unrelated render/event-loop boundary; otherwise the client observes several
                    // high-rate mouse samples coalesced into one low-rate batch, which reads as
                    // choppy view movement.
                    let _ = self.display_handle.flush_clients();
                    return;
                }

                let Some(pos) = constrain_pointer_location_to_outputs(
                    previous_pos + event.delta(),
                    self.space
                        .outputs()
                        .filter_map(|output| self.space.output_geometry(output)),
                ) else {
                    return;
                };

                if self.session_lock_active {
                    self.forward_locked_pointer_motion(
                        pos,
                        SERIAL_COUNTER.next_serial(),
                        event.time_msec(),
                    );
                    return;
                }

                let next_contents = self.pointer_contents_at(pos);
                if pointer_confined {
                    let remains_on_surface = match (&under, &next_contents.surface) {
                        (Some((surface, _)), Some((next_surface, _))) => surface == next_surface,
                        _ => false,
                    };
                    let remains_in_region = confine_region.as_ref().is_none_or(|region| {
                        under.as_ref().is_some_and(|(_, surface_origin)| {
                            region.contains((pos - *surface_origin).to_i32_floor())
                        })
                    });
                    if !remains_on_surface || !remains_in_region {
                        pointer.relative_motion(
                            self,
                            under,
                            &RelativeMotionEvent {
                                delta: event.delta(),
                                delta_unaccel: event.delta_unaccel(),
                                utime: event.time(),
                            },
                        );
                        pointer.frame(self);
                        // Like a locked pointer, a rejected confined movement has no
                        // compositor-side visual change and therefore no redraw to promptly flush
                        // this event.
                        let _ = self.display_handle.flush_clients();
                        return;
                    }
                }

                let serial = SERIAL_COUNTER.next_serial();
                self.pointer_contents = next_contents;
                let under = self.pointer_contents.surface.clone();

                pointer.motion(
                    self,
                    under.clone(),
                    &MotionEvent {
                        location: pos,
                        serial,
                        time: event.time_msec(),
                    },
                );
                pointer.relative_motion(
                    self,
                    under,
                    &RelativeMotionEvent {
                        delta: event.delta(),
                        delta_unaccel: event.delta_unaccel(),
                        utime: event.time(),
                    },
                );
                pointer.frame(self);
                self.activate_pointer_constraint_at(pos);

                self.dispatch_pointer_move_events(previous_pos, pos, event.time_msec());
                self.update_decoration_hover_target(pos);
                if !pointer.is_grabbed() {
                    self.update_decoration_cursor_icon(pos);
                }
                self.schedule_redraw();
            }
            InputEvent::PointerMotionAbsolute { event, .. } => {
                let Some(output_bounds) = self.output_layout_bounds() else {
                    return;
                };

                let unconstrained_pos =
                    event.position_transformed(output_bounds.size) + output_bounds.loc.to_f64();
                let Some(pos) = constrain_pointer_location_to_outputs(
                    unconstrained_pos,
                    self.space
                        .outputs()
                        .filter_map(|output| self.space.output_geometry(output)),
                ) else {
                    return;
                };

                let serial = SERIAL_COUNTER.next_serial();

                let pointer = self.seat.get_pointer().unwrap();
                let previous_pos = pointer.current_location();
                if self.active_pointer_lock_focus(&pointer).is_some() {
                    return;
                }

                if self.session_lock_active {
                    self.forward_locked_pointer_motion(pos, serial, event.time_msec());
                    return;
                }

                self.pointer_contents = self.pointer_contents_at(pos);
                let under = self.pointer_contents.surface.clone();

                pointer.motion(
                    self,
                    under,
                    &MotionEvent {
                        location: pos,
                        serial,
                        time: event.time_msec(),
                    },
                );
                pointer.frame(self);
                self.activate_pointer_constraint_at(pos);
                self.dispatch_pointer_move_events(previous_pos, pos, event.time_msec());
                self.update_decoration_hover_target(pos);
                if !pointer.is_grabbed() {
                    self.update_decoration_cursor_icon(pos);
                }
                self.schedule_redraw();
            }
            InputEvent::PointerButton { event, .. } => {
                let pointer = self.seat.get_pointer().unwrap();

                let serial = SERIAL_COUNTER.next_serial();

                let button = event.button_code();

                let button_state = event.state();

                // A pointer button press cancels a pending modifier tap (e.g. Super+drag).
                if matches!(button_state, ButtonState::Pressed) {
                    self.tap_interrupted = true;
                }

                if pointer_button_debug_enabled() {
                    debug!(
                        button,
                        state = ?button_state,
                        pointer_location = ?pointer.current_location(),
                        "pointer button event received"
                    );
                }
                if button == 273 {
                    self.note_right_click_button(
                        matches!(button_state, ButtonState::Pressed),
                        pointer.current_location(),
                        "process-input-event",
                    );
                }
                if button == 272 && button_state == ButtonState::Released {
                    self.release_decoration_active_target();
                }

                if self.session_lock_active {
                    self.forward_locked_pointer_button(
                        button,
                        button_state,
                        serial,
                        event.time_msec(),
                    );
                    return;
                }

                // A locked-pointer client already owns pointer focus. Re-running compositor hit
                // testing before a button event can briefly change that focus while the client
                // updates its surface tree, deactivating the lock and leaking one absolute move.
                if self.active_pointer_lock_focus(&pointer).is_some() {
                    pointer.button(
                        self,
                        &ButtonEvent {
                            button,
                            state: button_state,
                            serial,
                            time: event.time_msec(),
                        },
                    );
                    pointer.frame(self);
                    // The locked client will commit its visual response itself. Avoid submitting
                    // an unchanged fullscreen frame before that commit.
                    let _ = self.display_handle.flush_clients();
                    return;
                }

                if ButtonState::Pressed == button_state && !pointer.is_grabbed() {
                    if unfocused_popup_focus_debug_enabled() && button == 273 {
                        let pos = pointer.current_location();
                        let keyboard_focus = self
                            .seat
                            .get_keyboard()
                            .and_then(|keyboard| keyboard.current_focus())
                            .map(|surface| surface.id().protocol_id());
                        let transformed_window_under = self
                            .window_under_transformed(LogicalPoint::new(
                                pos.x.floor() as i32,
                                pos.y.floor() as i32,
                            ))
                            .map(|(window, _)| {
                                window
                                    .toplevel()
                                    .map(|toplevel| toplevel.wl_surface().id().protocol_id())
                                    .unwrap_or_default()
                            });
                        let raw_window_under = self
                            .raw_window_under(LogicalPoint::new(
                                pos.x.floor() as i32,
                                pos.y.floor() as i32,
                            ))
                            .map(|(window, _)| {
                                window
                                    .toplevel()
                                    .map(|toplevel| toplevel.wl_surface().id().protocol_id())
                                    .unwrap_or_default()
                            });
                        let layer_under = self.layer_surface_under(pos).map(|layer| {
                            (
                                layer.wl_surface().id().protocol_id(),
                                layer.layer(),
                                layer.cached_state().keyboard_interactivity,
                            )
                        });
                        let pointer_target_before = self
                            .surface_under(pos)
                            .map(|(surface, origin)| (surface.id().protocol_id(), origin));
                        debug!(
                            button,
                            pointer_location = ?pos,
                            keyboard_focus_surface = ?keyboard_focus,
                            transformed_window_under = ?transformed_window_under,
                            raw_window_under = ?raw_window_under,
                            layer_under = ?layer_under,
                            pointer_target_before = ?pointer_target_before,
                            "unfocused popup focus debug: pointer press pre-focus"
                        );
                    }
                    self.pointer_contents = self.pointer_contents_at(pointer.current_location());
                    let under = self.pointer_contents.surface.clone();
                    self.log_stack_hit_debug(
                        "button-press-before-motion",
                        pointer.current_location(),
                    );
                    if layer_focus_debug_enabled() {
                        debug!(
                            pointer_location = ?pointer.current_location(),
                            pointer_target_surface =
                                under.as_ref().map(|(surface, _)| surface.id().protocol_id()),
                            pointer_target_origin = ?under.as_ref().map(|(_, origin)| *origin),
                            "pointer target before button dispatch"
                        );
                    }
                    pointer.motion(
                        self,
                        under,
                        &MotionEvent {
                            location: pointer.current_location(),
                            serial,
                            time: event.time_msec(),
                        },
                    );
                    pointer.frame(self);

                    if layer_focus_debug_enabled() {
                        let keyboard_focus = self
                            .seat
                            .get_keyboard()
                            .and_then(|keyboard| keyboard.current_focus())
                            .map(|surface| surface.id().protocol_id());
                        debug!(
                            pointer_location = ?pointer.current_location(),
                            button,
                            keyboard_focus_surface = ?keyboard_focus,
                            layer_under = ?self.pointer_contents.layer.as_ref().and_then(|layer| {
                                layer.can_receive_keyboard_focus().then(|| {
                                    (
                                        layer.wl_surface().id().protocol_id(),
                                        layer.layer(),
                                        layer.cached_state().keyboard_interactivity,
                                    )
                                })
                            }),
                            any_layer_under = ?self.pointer_contents.layer.as_ref().map(|layer| {
                                (
                                    layer.wl_surface().id().protocol_id(),
                                    layer.layer(),
                                    layer.cached_state().keyboard_interactivity,
                                )
                            }),
                            "layer focus decision on pointer press"
                        );
                    }
                    let layer_under_pointer = self.pointer_contents.layer.clone();
                    let _ = self.refresh_window_decorations();
                    // Runtime decoration refresh can change ManagedWindow zIndex
                    // (for example createWindowStack().raise() on open/focus).
                    // Recompute the pointer target before SSD hit-test gating;
                    // otherwise pointer_allows_window_interaction can compare
                    // the newly topmost decoration against a stale client
                    // surface from the previously topmost window.
                    self.pointer_contents = self.pointer_contents_at(pointer.current_location());
                    self.log_stack_hit_debug(
                        "button-press-after-refresh",
                        pointer.current_location(),
                    );
                    self.update_decoration_hover_target(pointer.current_location());
                    if button == 272 {
                        self.press_decoration_active_target(pointer.current_location());
                    }

                    if button == 272
                        && layer_under_pointer.is_none()
                        && self.runtime_window_move_modifier.is_some_and(|modifier| {
                            modifier.matches(&self.current_keyboard_modifiers)
                        })
                        && let Some(window) = self
                            .window_under_transformed(LogicalPoint::new(
                                pointer.current_location().x.floor() as i32,
                                pointer.current_location().y.floor() as i32,
                            ))
                            .map(|(window, _)| window.clone())
                        && self.pointer_allows_window_interaction(
                            self.pointer_contents
                                .surface
                                .as_ref()
                                .map(|(surface, _)| surface),
                            &window,
                        )
                    {
                        let focus_surface = self
                            .pointer_contents
                            .surface
                            .as_ref()
                            .map(|(surface, _)| surface.clone());
                        let start_data = compositor_pointer_grab_start_data(
                            self.pointer_contents.surface.clone(),
                            button,
                            pointer.current_location(),
                        );
                        self.focus_window_at_surface(&window, focus_surface.as_ref(), serial);

                        if let Some(initial_window_location) = self.space.element_location(&window)
                        {
                            let initial_window_rect = smithay::utils::Rectangle::new(
                                initial_window_location,
                                window.geometry().size,
                            );
                            let initial_event_rect =
                                self.managed_resize_initial_rect(&window, initial_window_rect);
                            let mut grab = MoveSurfaceGrab::start(
                                start_data,
                                window,
                                initial_window_location,
                                initial_event_rect,
                                WindowMoveSourceSnapshot::Modifier,
                            );
                            grab.notify_start(self);
                            pointer.set_grab(
                                self,
                                grab,
                                serial,
                                smithay::input::pointer::Focus::Clear,
                            );
                        }

                        pointer.frame(self);
                        let _ = self.display_handle.flush_clients();
                        self.schedule_redraw();
                        return;
                    }

                    if button == 273
                        && layer_under_pointer.is_none()
                        && self.runtime_window_resize_modifier.is_some_and(|modifier| {
                            modifier.matches(&self.current_keyboard_modifiers)
                        })
                        && let Some(window) = self
                            .window_under_transformed(LogicalPoint::new(
                                pointer.current_location().x.floor() as i32,
                                pointer.current_location().y.floor() as i32,
                            ))
                            .map(|(window, _)| window.clone())
                        && self.pointer_allows_window_interaction(
                            self.pointer_contents
                                .surface
                                .as_ref()
                                .map(|(surface, _)| surface),
                            &window,
                        )
                    {
                        let focus_surface = self
                            .pointer_contents
                            .surface
                            .as_ref()
                            .map(|(surface, _)| surface.clone());
                        let start_data = compositor_pointer_grab_start_data(
                            self.pointer_contents.surface.clone(),
                            button,
                            pointer.current_location(),
                        );
                        self.focus_window_at_surface(&window, focus_surface.as_ref(), serial);

                        if let Some(initial_window_location) = self.space.element_location(&window)
                        {
                            let initial_window_size = window.geometry().size;
                            let initial_window_rect = smithay::utils::Rectangle::new(
                                initial_window_location,
                                initial_window_size,
                            );
                            let initial_event_rect =
                                self.managed_resize_initial_rect(&window, initial_window_rect);

                            let pointer_loc = pointer.current_location();
                            let window_rect = initial_window_rect;
                            let center_x = window_rect.loc.x + window_rect.size.w / 2;
                            let center_y = window_rect.loc.y + window_rect.size.h / 2;

                            let mut edges = ResizeEdge::empty();
                            if pointer_loc.x < center_x as f64 {
                                edges |= ResizeEdge::LEFT;
                            } else {
                                edges |= ResizeEdge::RIGHT;
                            }
                            if pointer_loc.y < center_y as f64 {
                                edges |= ResizeEdge::TOP;
                            } else {
                                edges |= ResizeEdge::BOTTOM;
                            }

                            if let Some(mut grab) = ResizeSurfaceGrab::start(
                                start_data,
                                window,
                                edges,
                                initial_window_rect,
                                initial_event_rect,
                                WindowResizeSourceSnapshot::Modifier,
                            ) {
                                grab.notify_start(self);
                                pointer.set_grab(
                                    self,
                                    grab,
                                    serial,
                                    smithay::input::pointer::Focus::Clear,
                                );
                            }

                            pointer.frame(self);
                            let _ = self.display_handle.flush_clients();
                            self.schedule_redraw();
                            return;
                        }
                    }

                    if layer_under_pointer.is_none()
                        && let Some((window, hit)) =
                            self.decoration_under(pointer.current_location())
                        && self.pointer_allows_window_interaction(
                            self.pointer_contents
                                .surface
                                .as_ref()
                                .map(|(surface, _)| surface),
                            &window,
                        )
                    {
                        let focus_surface = self
                            .pointer_contents
                            .surface
                            .as_ref()
                            .map(|(surface, _)| surface.clone());
                        self.focus_window_at_surface(&window, focus_surface.as_ref(), serial);

                        match hit {
                            DecorationHitTestResult::Action(WindowAction::Close) => {
                                pointer.button(
                                    self,
                                    &ButtonEvent {
                                        button,
                                        state: button_state,
                                        serial,
                                        time: event.time_msec(),
                                    },
                                );
                                if let Err(error) =
                                    crate::backend::tty::capture_live_snapshot_for_close(
                                        self, &window,
                                    )
                                {
                                    warn!(
                                        ?error,
                                        "failed to capture decorated window before close request"
                                    );
                                }
                                if let Some(toplevel) = window.toplevel() {
                                    toplevel.send_close();
                                }
                            }
                            DecorationHitTestResult::Action(WindowAction::Maximize) => {
                                pointer.button(
                                    self,
                                    &ButtonEvent {
                                        button,
                                        state: button_state,
                                        serial,
                                        time: event.time_msec(),
                                    },
                                );
                                self.request_window_maximize(
                                    &window,
                                    true,
                                    crate::ssd::WindowStateRequestSourceSnapshot::Api,
                                );
                            }
                            DecorationHitTestResult::Action(WindowAction::Unmaximize) => {
                                pointer.button(
                                    self,
                                    &ButtonEvent {
                                        button,
                                        state: button_state,
                                        serial,
                                        time: event.time_msec(),
                                    },
                                );
                                self.request_window_maximize(
                                    &window,
                                    false,
                                    crate::ssd::WindowStateRequestSourceSnapshot::Api,
                                );
                            }
                            DecorationHitTestResult::Action(WindowAction::Minimize) => {
                                pointer.button(
                                    self,
                                    &ButtonEvent {
                                        button,
                                        state: button_state,
                                        serial,
                                        time: event.time_msec(),
                                    },
                                );
                                self.request_window_minimize(
                                    &window,
                                    true,
                                    crate::ssd::WindowStateRequestSourceSnapshot::Api,
                                );
                            }
                            DecorationHitTestResult::Action(WindowAction::RuntimeHandler(
                                handler_id,
                            )) => {
                                pointer.button(
                                    self,
                                    &ButtonEvent {
                                        button,
                                        state: button_state,
                                        serial,
                                        time: event.time_msec(),
                                    },
                                );

                                let window_id = self.snapshot_window(&window).id;
                                let now_ms =
                                    std::time::Duration::from(self.clock.now()).as_millis() as u64;
                                self.sync_runtime_display_state();
                                if let Ok(invocation) = self.decoration_evaluator.invoke_handler(
                                    &window_id,
                                    &handler_id,
                                    now_ms,
                                ) {
                                    self.consume_runtime_display_config(
                                        invocation.display_config.clone(),
                                    );
                                    self.consume_runtime_workspace_config(
                                        invocation.workspace_config.clone(),
                                    );
                                    self.consume_runtime_key_binding_config(
                                        invocation.key_binding_config.clone(),
                                    );
                                    self.consume_runtime_pointer_config(
                                        invocation.pointer_config.clone(),
                                    );
                                    self.consume_runtime_input_config(
                                        invocation.input_config.clone(),
                                    );
                                    self.consume_runtime_event_config(
                                        invocation.event_config.clone(),
                                    );
                                    self.consume_runtime_process_config(
                                        invocation.process_config.clone(),
                                    );
                                    if !invocation.process_actions.is_empty() {
                                        self.apply_runtime_process_actions(
                                            invocation.process_actions.clone(),
                                        );
                                    }
                                    self.apply_runtime_handler_invocation(&window, &invocation);

                                    if invocation.invoked {
                                        self.mark_runtime_dirty_windows(
                                            invocation.dirty_window_ids,
                                            invocation.dirty_managed_window_ids,
                                        );
                                        self.runtime_scheduler_enabled =
                                            invocation.next_poll_in_ms.is_some();
                                        self.apply_runtime_window_actions(invocation.actions);
                                        self.schedule_redraw();
                                    }
                                }
                            }
                            DecorationHitTestResult::Move => {
                                let start_data = compositor_pointer_grab_start_data(
                                    self.pointer_contents.surface.clone(),
                                    button,
                                    pointer.current_location(),
                                );
                                if let Some(initial_window_location) =
                                    self.space.element_location(&window)
                                {
                                    let initial_window_rect = smithay::utils::Rectangle::new(
                                        initial_window_location,
                                        window.geometry().size,
                                    );
                                    let initial_event_rect = self
                                        .managed_resize_initial_rect(&window, initial_window_rect);
                                    let mut grab = MoveSurfaceGrab::start(
                                        start_data,
                                        window,
                                        initial_window_location,
                                        initial_event_rect,
                                        WindowMoveSourceSnapshot::Ssd,
                                    );
                                    grab.notify_start(self);
                                    pointer.set_grab(
                                        self,
                                        grab,
                                        serial,
                                        smithay::input::pointer::Focus::Clear,
                                    );
                                }
                            }
                            DecorationHitTestResult::Resize(edges) => {
                                let start_data = compositor_pointer_grab_start_data(
                                    self.pointer_contents.surface.clone(),
                                    button,
                                    pointer.current_location(),
                                );
                                if let Some(initial_window_location) =
                                    self.space.element_location(&window)
                                {
                                    let initial_window_size = window.geometry().size;
                                    if let Some(toplevel) = window.toplevel() {
                                        toplevel.with_pending_state(|state| {
                                            state.states.set(
                                                smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::State::Resizing,
                                            );
                                        });
                                        toplevel.send_pending_configure();
                                    }

                                    let initial_window_rect = smithay::utils::Rectangle::new(
                                        initial_window_location,
                                        initial_window_size,
                                    );
                                    let initial_event_rect = self
                                        .managed_resize_initial_rect(&window, initial_window_rect);

                                    if let Some(mut grab) = ResizeSurfaceGrab::start(
                                        start_data,
                                        window,
                                        resize_edges_to_grab(edges),
                                        initial_window_rect,
                                        initial_event_rect,
                                        WindowResizeSourceSnapshot::Ssd,
                                    ) {
                                        grab.notify_start(self);
                                        pointer.set_grab(
                                            self,
                                            grab,
                                            serial,
                                            smithay::input::pointer::Focus::Clear,
                                        );
                                    }
                                }
                            }
                            DecorationHitTestResult::ClientArea => {
                                pointer.button(
                                    self,
                                    &ButtonEvent {
                                        button,
                                        state: button_state,
                                        serial,
                                        time: event.time_msec(),
                                    },
                                );
                            }
                            DecorationHitTestResult::Outside => {}
                        }

                        pointer.frame(self);
                        let _ = self.display_handle.flush_clients();
                        self.schedule_redraw();
                        return;
                    } else if layer_under_pointer.is_none()
                        && let Some((window, _loc)) = self
                            .window_under_transformed(LogicalPoint::new(
                                pointer.current_location().x.floor() as i32,
                                pointer.current_location().y.floor() as i32,
                            ))
                            .map(|(w, _)| (w.clone(), ()))
                        && self.pointer_allows_window_interaction(
                            self.pointer_contents
                                .surface
                                .as_ref()
                                .map(|(surface, _)| surface),
                            &window,
                        )
                    {
                        let focus_surface = self
                            .pointer_contents
                            .surface
                            .as_ref()
                            .map(|(surface, _)| surface.clone());
                        self.focus_window_at_surface(&window, focus_surface.as_ref(), serial);
                    } else if let Some(layer) = layer_under_pointer {
                        if layer.can_receive_keyboard_focus() {
                            self.focus_layer_surface_if_on_demand(Some(layer));
                            self.update_keyboard_focus(serial);
                        } else if layer_focus_debug_enabled() {
                            debug!(
                                layer_surface_id = layer.wl_surface().id().protocol_id(),
                                layer = ?layer.layer(),
                                keyboard_interactivity =
                                    ?layer.cached_state().keyboard_interactivity,
                                "leaving keyboard focus unchanged for non-interactive layer press"
                            );
                        }
                    } else {
                        self.focus_layer_surface_if_on_demand(None);
                        self.update_keyboard_focus(serial);
                    }

                    if unfocused_popup_focus_debug_enabled() && button == 273 {
                        let pos = pointer.current_location();
                        let keyboard_focus = self
                            .seat
                            .get_keyboard()
                            .and_then(|keyboard| keyboard.current_focus())
                            .map(|surface| surface.id().protocol_id());
                        let pointer_target_after = self
                            .surface_under(pos)
                            .map(|(surface, origin)| (surface.id().protocol_id(), origin));
                        debug!(
                            button,
                            pointer_location = ?pos,
                            keyboard_focus_surface = ?keyboard_focus,
                            pointer_target_after = ?pointer_target_after,
                            "unfocused popup focus debug: pointer press post-focus"
                        );
                    }
                };

                pointer.button(
                    self,
                    &ButtonEvent {
                        button,
                        state: button_state,
                        serial,
                        time: event.time_msec(),
                    },
                );
                pointer.frame(self);
                let _ = self.display_handle.flush_clients();
                // Ensure the next redraw runs so frame callbacks flow to the clients that just
                // received the button event. Without this, a button press on an idle surface
                // (e.g. clicking a noctalia bar widget) does not trigger a render, so Quickshell
                // can stall waiting for a wl_surface.frame callback before it even begins
                // rendering the popup. Cursor motion has the same effect via the PointerMotion
                // handler; doing it here keeps button and motion symmetric.
                self.schedule_redraw();
                if pointer_button_debug_enabled() {
                    debug!(
                        button,
                        state = ?button_state,
                        "pointer button forwarded and flushed"
                    );
                }
                if std::env::var_os("SHOJI_RIGHT_CLICK_TRACE").is_some() && button == 273 {
                    debug!(
                        state = ?button_state,
                        pointer_location = ?pointer.current_location(),
                        "right click trace: button forwarded and flushed"
                    );
                }
            }
            InputEvent::PointerAxis { event, .. } => {
                let source = event.source();
                let device = event.device();
                let scroll_factor = crate::runtime_input::scroll_factor_for_backend_device(
                    &self.runtime_input_config,
                    &self.runtime_input_devices,
                    &device,
                );

                let horizontal_amount = event.amount(Axis::Horizontal).unwrap_or_else(|| {
                    event.amount_v120(Axis::Horizontal).unwrap_or(0.0) * 15.0 / 120.
                }) * scroll_factor;
                let vertical_amount = event.amount(Axis::Vertical).unwrap_or_else(|| {
                    event.amount_v120(Axis::Vertical).unwrap_or(0.0) * 15.0 / 120.
                }) * scroll_factor;
                let horizontal_amount_discrete = event
                    .amount_v120(Axis::Horizontal)
                    .map(|value| value * scroll_factor);
                let vertical_amount_discrete = event
                    .amount_v120(Axis::Vertical)
                    .map(|value| value * scroll_factor);

                let mut frame = AxisFrame::new(event.time_msec()).source(source);
                if horizontal_amount != 0.0 {
                    frame = frame.value(Axis::Horizontal, horizontal_amount);
                    if let Some(discrete) = horizontal_amount_discrete {
                        frame = frame.v120(Axis::Horizontal, discrete.round() as i32);
                    }
                }
                if vertical_amount != 0.0 {
                    frame = frame.value(Axis::Vertical, vertical_amount);
                    if let Some(discrete) = vertical_amount_discrete {
                        frame = frame.v120(Axis::Vertical, discrete.round() as i32);
                    }
                }

                if source == AxisSource::Finger {
                    if event.amount(Axis::Horizontal) == Some(0.0) {
                        frame = frame.stop(Axis::Horizontal);
                    }
                    if event.amount(Axis::Vertical) == Some(0.0) {
                        frame = frame.stop(Axis::Vertical);
                    }
                }

                let pointer = self.seat.get_pointer().unwrap();
                if self.session_lock_active {
                    pointer.axis(self, frame);
                    pointer.frame(self);
                    let _ = self.display_handle.flush_clients();
                    return;
                }

                // Mouse-wheel shortcuts use the same configurable runtime
                // binding path as keyboard shortcuts. Touchpad/finger scrolling
                // remains continuous and is always forwarded to the client.
                if source == AxisSource::Wheel
                    && (horizontal_amount != 0.0 || vertical_amount != 0.0)
                {
                    self.tap_interrupted = true;
                    use crate::runtime_key_binding::RuntimeWheelDirection;
                    let vertical_direction = if vertical_amount < 0.0 {
                        Some(RuntimeWheelDirection::Up)
                    } else if vertical_amount > 0.0 {
                        Some(RuntimeWheelDirection::Down)
                    } else {
                        None
                    };
                    let horizontal_direction = if horizontal_amount < 0.0 {
                        Some(RuntimeWheelDirection::Left)
                    } else if horizontal_amount > 0.0 {
                        Some(RuntimeWheelDirection::Right)
                    } else {
                        None
                    };
                    let binding_id = [vertical_direction, horizontal_direction]
                        .into_iter()
                        .flatten()
                        .find_map(|direction| {
                            self.runtime_key_bindings
                                .iter()
                                .find(|binding| {
                                    binding.matches_wheel(
                                        direction,
                                        &self.current_keyboard_modifiers,
                                    )
                                })
                                .map(|binding| binding.id.clone())
                        });
                    if let Some(binding_id) = binding_id {
                        self.run_runtime_key_binding(&binding_id);
                        return;
                    }
                }
                pointer.axis(self, frame);
                pointer.frame(self);
            }
            InputEvent::GestureSwipeBegin { event, .. } => {
                if !self.runtime_gesture_swipe_enabled && !self.runtime_gesture_swipe_async_enabled
                {
                    return;
                }
                let timestamp = u64::from(event.time_msec());
                let device = event.device();
                let pointer_position = self
                    .seat
                    .get_pointer()
                    .map(|pointer| pointer.current_location());
                let output_name = pointer_position
                    .and_then(|position| self.output_at_point(position))
                    .map(|output| output.name());
                let position = pointer_position.map(|position| PointerMovePointSnapshot {
                    x: position.x,
                    y: position.y,
                });
                let device = crate::runtime_input::snapshot_for_backend_input_device(
                    &self.runtime_input_devices,
                    &device,
                );
                let fingers = event.fingers();
                self.runtime_gesture_swipe = Some(crate::state::RuntimeGestureSwipeState {
                    fingers,
                    total_x: 0.0,
                    total_y: 0.0,
                    last_timestamp: timestamp,
                    velocity_x: 0.0,
                    velocity_y: 0.0,
                });
                self.dispatch_gesture_swipe_events(GestureSwipeEventSnapshot {
                    phase: GestureSwipePhaseSnapshot::Begin,
                    fingers,
                    position,
                    delta_x: 0.0,
                    delta_y: 0.0,
                    total_x: 0.0,
                    total_y: 0.0,
                    velocity_x: 0.0,
                    velocity_y: 0.0,
                    output_name,
                    device,
                    timestamp,
                });
            }
            InputEvent::GestureSwipeUpdate { event, .. } => {
                if !self.runtime_gesture_swipe_enabled && !self.runtime_gesture_swipe_async_enabled
                {
                    return;
                }
                let timestamp = u64::from(event.time_msec());
                let delta_x = event.delta_x();
                let delta_y = event.delta_y();
                let device = event.device();
                let pointer_position = self
                    .seat
                    .get_pointer()
                    .map(|pointer| pointer.current_location());
                let output_name = pointer_position
                    .and_then(|position| self.output_at_point(position))
                    .map(|output| output.name());
                let position = pointer_position.map(|position| PointerMovePointSnapshot {
                    x: position.x,
                    y: position.y,
                });
                let device = crate::runtime_input::snapshot_for_backend_input_device(
                    &self.runtime_input_devices,
                    &device,
                );
                let Some(gesture) = self.runtime_gesture_swipe.as_mut() else {
                    return;
                };
                let dt_seconds =
                    timestamp.saturating_sub(gesture.last_timestamp).max(1) as f64 / 1000.0;
                gesture.total_x += delta_x;
                gesture.total_y += delta_y;
                gesture.velocity_x = delta_x / dt_seconds;
                gesture.velocity_y = delta_y / dt_seconds;
                gesture.last_timestamp = timestamp;
                let fingers = gesture.fingers;
                let total_x = gesture.total_x;
                let total_y = gesture.total_y;
                let velocity_x = gesture.velocity_x;
                let velocity_y = gesture.velocity_y;
                self.dispatch_gesture_swipe_events(GestureSwipeEventSnapshot {
                    phase: GestureSwipePhaseSnapshot::Update,
                    fingers,
                    position,
                    delta_x,
                    delta_y,
                    total_x,
                    total_y,
                    velocity_x,
                    velocity_y,
                    output_name,
                    device,
                    timestamp,
                });
            }
            InputEvent::GestureSwipeEnd { event, .. } => {
                if !self.runtime_gesture_swipe_enabled && !self.runtime_gesture_swipe_async_enabled
                {
                    self.runtime_gesture_swipe = None;
                    return;
                }
                let timestamp = u64::from(event.time_msec());
                let device = event.device();
                let pointer_position = self
                    .seat
                    .get_pointer()
                    .map(|pointer| pointer.current_location());
                let output_name = pointer_position
                    .and_then(|position| self.output_at_point(position))
                    .map(|output| output.name());
                let position = pointer_position.map(|position| PointerMovePointSnapshot {
                    x: position.x,
                    y: position.y,
                });
                let device = crate::runtime_input::snapshot_for_backend_input_device(
                    &self.runtime_input_devices,
                    &device,
                );
                let Some(gesture) = self.runtime_gesture_swipe.take() else {
                    return;
                };
                self.dispatch_gesture_swipe_events(GestureSwipeEventSnapshot {
                    phase: if event.cancelled() {
                        GestureSwipePhaseSnapshot::Cancel
                    } else {
                        GestureSwipePhaseSnapshot::End
                    },
                    fingers: gesture.fingers,
                    position,
                    delta_x: 0.0,
                    delta_y: 0.0,
                    total_x: gesture.total_x,
                    total_y: gesture.total_y,
                    velocity_x: gesture.velocity_x,
                    velocity_y: gesture.velocity_y,
                    output_name,
                    device,
                    timestamp,
                });
            }
            _ => {}
        }
    }

    pub(crate) fn apply_runtime_window_actions(&mut self, actions: Vec<RuntimeWindowAction>) {
        for runtime_action in actions {
            if matches!(
                runtime_action.action,
                crate::ssd::WaylandWindowAction::FinalizeClose
            ) {
                self.prune_window_shader_pipeline_caches(&runtime_action.window_id);
                self.closing_window_snapshots
                    .remove(&runtime_action.window_id);
                self.live_window_snapshots.remove(&runtime_action.window_id);
                self.live_window_snapshot_trackers
                    .remove(&runtime_action.window_id);
                self.complete_window_snapshots
                    .remove(&runtime_action.window_id);
                self.complete_window_snapshot_trackers
                    .remove(&runtime_action.window_id);
                self.windows_ready_for_decoration
                    .remove(&runtime_action.window_id);
                self.pending_initial_focus_window_ids
                    .remove(&runtime_action.window_id);
                self.pending_xdg_state_configure_window_ids
                    .remove(&runtime_action.window_id);
                self.snapshot_dirty_window_ids
                    .remove(&runtime_action.window_id);
                let _ = self
                    .decoration_evaluator
                    .window_closed(&runtime_action.window_id);
                self.runtime_dirty_window_ids
                    .remove(&runtime_action.window_id);
                self.runtime_managed_only_window_ids
                    .remove(&runtime_action.window_id);
                self.runtime_node_only_window_ids
                    .remove(&runtime_action.window_id);
                self.schedule_redraw();
                continue;
            }

            match runtime_action.action {
                crate::ssd::WaylandWindowAction::ScheduleAnimation => {
                    if let Some(animation) = runtime_action.animation {
                        self.schedule_managed_window_animation(runtime_action.window_id, animation);
                    }
                    continue;
                }
                crate::ssd::WaylandWindowAction::CancelAnimation => {
                    self.cancel_managed_window_animation(
                        &runtime_action.window_id,
                        runtime_action.channel.as_deref(),
                    );
                    continue;
                }
                _ => {}
            }

            let target_window = self
                .space
                .elements()
                .find(|window| self.snapshot_window(window).id == runtime_action.window_id)
                .cloned();

            let Some(window) = target_window else {
                continue;
            };

            match runtime_action.action {
                crate::ssd::WaylandWindowAction::Close => {
                    if let Err(error) =
                        crate::backend::tty::capture_live_snapshot_for_close(self, &window)
                    {
                        warn!(
                            window_id = runtime_action.window_id,
                            ?error,
                            "failed to capture runtime window before close request"
                        );
                    }
                    if let Some(toplevel) = window.toplevel() {
                        toplevel.send_close();
                    }
                }
                crate::ssd::WaylandWindowAction::Maximize => {
                    self.request_window_maximize(
                        &window,
                        true,
                        crate::ssd::WindowStateRequestSourceSnapshot::Api,
                    );
                }
                crate::ssd::WaylandWindowAction::Unmaximize => {
                    self.request_window_maximize(
                        &window,
                        false,
                        crate::ssd::WindowStateRequestSourceSnapshot::Api,
                    );
                }
                crate::ssd::WaylandWindowAction::Focus => {
                    let serial = SERIAL_COUNTER.next_serial();
                    self.focus_window(&window, serial);
                }
                crate::ssd::WaylandWindowAction::FinalizeClose => {}
                crate::ssd::WaylandWindowAction::ScheduleAnimation
                | crate::ssd::WaylandWindowAction::CancelAnimation => {}
                crate::ssd::WaylandWindowAction::Minimize => {
                    self.request_window_minimize(
                        &window,
                        true,
                        crate::ssd::WindowStateRequestSourceSnapshot::Api,
                    );
                }
                crate::ssd::WaylandWindowAction::Fullscreen => {
                    self.request_window_fullscreen(
                        &window,
                        true,
                        None,
                        crate::ssd::WindowStateRequestSourceSnapshot::Api,
                    );
                }
                crate::ssd::WaylandWindowAction::Unfullscreen => {
                    self.request_window_fullscreen(
                        &window,
                        false,
                        None,
                        crate::ssd::WindowStateRequestSourceSnapshot::Api,
                    );
                }
            }
        }
    }
}

/// `(protocol id, local point, serial, is_popup, owning-window info)`.
type SurfaceHitDebugInfo = (
    u32,
    smithay::utils::Point<f64, smithay::utils::Logical>,
    u32,
    bool,
    Option<(usize, String, String, Option<String>, i32)>,
);

impl ShojiWM {
    pub(crate) fn request_window_maximize(
        &mut self,
        window: &smithay::desktop::Window,
        maximized: bool,
        source: crate::ssd::WindowStateRequestSourceSnapshot,
    ) -> bool {
        let snapshot = self.snapshot_window(window);
        let now_ms = std::time::Duration::from(self.clock.now()).as_millis() as u64;
        let event = crate::ssd::WindowMaximizeRequestEventSnapshot {
            maximized,
            source,
            timestamp: now_ms,
        };
        tracing::info!(
            window_id = %snapshot.id,
            title = %snapshot.title,
            app_id = ?snapshot.app_id,
            maximized,
            source = ?source,
            "runtime window maximize request dispatch"
        );
        let invoked = self.invoke_window_maximize_request_event(&snapshot, &event, now_ms);
        if invoked {
            self.set_xdg_maximized_hint(window, &snapshot.id, maximized);
        }
        tracing::info!(
            window_id = %snapshot.id,
            invoked,
            "runtime window maximize request result"
        );
        invoked
    }

    fn set_xdg_maximized_hint(
        &mut self,
        window: &smithay::desktop::Window,
        window_id: &str,
        maximized: bool,
    ) {
        let Some(toplevel) = window.toplevel() else {
            return;
        };

        let changed = toplevel.with_pending_state(|state| {
            let was_maximized = state.states.contains(xdg_toplevel::State::Maximized);
            if maximized {
                state.states.set(xdg_toplevel::State::Maximized);
            } else {
                state.states.unset(xdg_toplevel::State::Maximized);
            }
            was_maximized != maximized
        });

        if changed {
            self.pending_xdg_state_configure_window_ids
                .insert(window_id.to_string());
            // Only update the xdg state here. ManagedWindow remains the geometry source of truth;
            // when the TS listener changes <ManagedWindow rect>, apply_managed_window_rects sends
            // the configure containing both the TS-selected size and this Maximized state. Sending
            // an immediate state-only configure would let clients observe "maximized at old size",
            // which can make Chromium/Electron stretch an old buffer for one configure cycle.
            if !self.runtime_poll_dirty {
                self.pending_xdg_state_configure_window_ids
                    .remove(window_id);
                toplevel.send_pending_configure();
            }
            self.schedule_redraw();
        }
    }

    pub(crate) fn request_window_fullscreen(
        &mut self,
        window: &smithay::desktop::Window,
        fullscreen: bool,
        output_name: Option<String>,
        source: crate::ssd::WindowStateRequestSourceSnapshot,
    ) -> bool {
        let snapshot = self.snapshot_window(window);
        let now_ms = std::time::Duration::from(self.clock.now()).as_millis() as u64;
        let event = crate::ssd::WindowFullscreenRequestEventSnapshot {
            fullscreen,
            output_name,
            source,
            timestamp: now_ms,
        };
        tracing::info!(
            window_id = %snapshot.id,
            title = %snapshot.title,
            app_id = ?snapshot.app_id,
            fullscreen,
            source = ?source,
            "runtime window fullscreen request dispatch"
        );
        // The runtime listener may synchronously focus the window or apply an animation. Those
        // actions can refresh ManagedWindow immediately, so stage the protocol state first. This
        // keeps that refresh from sending a configure with the new rect and the old Fullscreen
        // state. If no listener accepts the request, restore the pending state below.
        let xdg_state_changed =
            self.prepare_xdg_fullscreen_hint(window, &snapshot.id, fullscreen);
        let invoked = self.invoke_window_fullscreen_request_event(&snapshot, &event, now_ms);
        self.finish_xdg_fullscreen_hint(
            window,
            &snapshot.id,
            fullscreen,
            xdg_state_changed,
            invoked,
        );
        tracing::info!(
            window_id = %snapshot.id,
            invoked,
            "runtime window fullscreen request result"
        );
        invoked
    }

    fn prepare_xdg_fullscreen_hint(
        &mut self,
        window: &smithay::desktop::Window,
        window_id: &str,
        fullscreen: bool,
    ) -> bool {
        let Some(toplevel) = window.toplevel() else {
            return false;
        };

        let changed = toplevel.with_pending_state(|state| {
            let was_fullscreen = state.states.contains(xdg_toplevel::State::Fullscreen);
            if fullscreen {
                state.states.set(xdg_toplevel::State::Fullscreen);
            } else {
                state.states.unset(xdg_toplevel::State::Fullscreen);
            }
            was_fullscreen != fullscreen
        });

        if changed {
            self.pending_xdg_state_configure_window_ids
                .insert(window_id.to_string());
        }

        changed
    }

    fn finish_xdg_fullscreen_hint(
        &mut self,
        window: &smithay::desktop::Window,
        window_id: &str,
        fullscreen: bool,
        changed: bool,
        invoked: bool,
    ) {
        if !changed {
            return;
        }
        let Some(toplevel) = window.toplevel() else {
            self.pending_xdg_state_configure_window_ids
                .remove(window_id);
            return;
        };

        if !invoked {
            toplevel.with_pending_state(|state| {
                if fullscreen {
                    state.states.unset(xdg_toplevel::State::Fullscreen);
                } else {
                    state.states.set(xdg_toplevel::State::Fullscreen);
                }
            });
            self.pending_xdg_state_configure_window_ids
                .remove(window_id);
            return;
        }

        // ManagedWindow remains the geometry source of truth. Usually its synchronous refresh
        // consumes this marker and sends one configure containing both state and size. If the
        // listener changed no geometry, finish with a state-only configure instead.
        if !self.runtime_poll_dirty
            && self
                .pending_xdg_state_configure_window_ids
                .remove(window_id)
        {
            toplevel.send_pending_configure();
        }
        self.schedule_redraw();
    }

    pub(crate) fn request_window_minimize(
        &mut self,
        window: &smithay::desktop::Window,
        minimized: bool,
        source: crate::ssd::WindowStateRequestSourceSnapshot,
    ) -> bool {
        let snapshot = self.snapshot_window(window);
        let now_ms = std::time::Duration::from(self.clock.now()).as_millis() as u64;
        let event = crate::ssd::WindowMinimizeRequestEventSnapshot {
            minimized,
            source,
            timestamp: now_ms,
        };
        tracing::info!(
            window_id = %snapshot.id,
            title = %snapshot.title,
            app_id = ?snapshot.app_id,
            minimized,
            source = ?source,
            "runtime window minimize request dispatch"
        );
        let invoked = self.invoke_window_minimize_request_event(&snapshot, &event, now_ms);
        tracing::info!(
            window_id = %snapshot.id,
            invoked,
            "runtime window minimize request result"
        );
        invoked
    }

    pub(crate) fn request_window_activate(
        &mut self,
        window: &smithay::desktop::Window,
        source: crate::ssd::WindowActivateRequestSourceSnapshot,
    ) -> bool {
        // Do NOT pre-dispatch an unminimize here: the TS activate handler owns
        // the restore (its own `wasMinimized` branch) and must observe the
        // pre-activate minimized state. Restoring first made a dock click on a
        // minimized window look like "activate an unminimized, still-focused
        // window", which the minimize-raise toggle then immediately
        // re-minimized (visible as a one-frame flash of the window).
        let snapshot = self.snapshot_window(window);
        let now_ms = std::time::Duration::from(self.clock.now()).as_millis() as u64;
        let event = crate::ssd::WindowActivateRequestEventSnapshot {
            source,
            timestamp: now_ms,
        };
        tracing::info!(
            window_id = %snapshot.id,
            title = %snapshot.title,
            app_id = ?snapshot.app_id,
            source = ?source,
            "runtime window activate request dispatch"
        );
        let invoked = self.invoke_window_activate_request_event(&snapshot, &event, now_ms);
        tracing::info!(
            window_id = %snapshot.id,
            invoked,
            "runtime window activate request result"
        );
        invoked
    }

    fn surface_has_popup_ancestor(
        &self,
        surface: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
    ) -> bool {
        let mut current = Some(surface.clone());
        while let Some(candidate) = current {
            if self.popups.find_popup(&candidate).is_some() {
                return true;
            }
            current = smithay::wayland::compositor::get_parent(&candidate);
        }
        false
    }

    fn surface_is_over_window_non_popup_tree(
        &self,
        top_surface: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
        window: &smithay::desktop::Window,
    ) -> bool {
        if self.surface_has_popup_ancestor(top_surface) {
            return false;
        }

        let mut root = top_surface.clone();
        while let Some(parent) = smithay::wayland::compositor::get_parent(&root) {
            root = parent;
        }

        window
            .toplevel()
            .is_some_and(|toplevel| toplevel.wl_surface() == &root)
    }

    fn non_popup_window_for_surface(
        &self,
        surface: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
    ) -> Option<smithay::desktop::Window> {
        if self.surface_has_popup_ancestor(surface) {
            return None;
        }

        let mut root = surface.clone();
        while let Some(parent) = smithay::wayland::compositor::get_parent(&root) {
            root = parent;
        }

        self.space
            .elements()
            .find(|window| {
                window
                    .toplevel()
                    .is_some_and(|toplevel| toplevel.wl_surface() == &root)
                    || window
                        .x11_surface()
                        .and_then(|x11| x11.wl_surface())
                        .as_ref()
                        == Some(&root)
            })
            .cloned()
    }

    fn window_for_pointer_surface(
        &self,
        surface: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
    ) -> Option<smithay::desktop::Window> {
        let mut root = Self::surface_root(surface);
        if let Some(popup_root) = self
            .popups
            .find_popup(surface)
            .or_else(|| self.popups.find_popup(&root))
            .and_then(|popup| smithay::desktop::find_popup_root_surface(&popup).ok())
        {
            root = popup_root;
        }

        self.space
            .elements()
            .find(|window| {
                window
                    .toplevel()
                    .is_some_and(|toplevel| toplevel.wl_surface() == &root)
                    || window
                        .x11_surface()
                        .and_then(|x11| x11.wl_surface())
                        .as_ref()
                        == Some(&root)
            })
            .cloned()
    }

    fn surface_root(
        surface: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
    ) -> smithay::reexports::wayland_server::protocol::wl_surface::WlSurface {
        let mut root = surface.clone();
        while let Some(parent) = smithay::wayland::compositor::get_parent(&root) {
            root = parent;
        }
        root
    }

    fn surface_owner_debug(
        &self,
        surface: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
    ) -> Option<(usize, String, String, Option<String>, i32)> {
        let root = Self::surface_root(surface);
        self.windows_top_to_bottom()
            .into_iter()
            .enumerate()
            .find_map(|(stack_index, window)| {
                let owns_root = window
                    .toplevel()
                    .is_some_and(|toplevel| toplevel.wl_surface() == &root)
                    || window
                        .x11_surface()
                        .and_then(|x11| x11.wl_surface())
                        .as_ref()
                        == Some(&root);
                owns_root.then(|| {
                    let snapshot = self.snapshot_window(window);
                    (
                        stack_index,
                        snapshot.id,
                        snapshot.title,
                        snapshot.app_id,
                        self.managed_window_z_index(window),
                    )
                })
            })
    }

    fn surface_is_window_root_for_debug(
        window: &smithay::desktop::Window,
        surface: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
    ) -> bool {
        window
            .toplevel()
            .is_some_and(|toplevel| toplevel.wl_surface() == surface)
            || window
                .x11_surface()
                .and_then(|x11| x11.wl_surface())
                .as_ref()
                == Some(surface)
    }

    fn surface_hit_debug(
        &self,
        surface: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
        origin: smithay::utils::Point<f64, smithay::utils::Logical>,
    ) -> SurfaceHitDebugInfo {
        (
            surface.id().protocol_id(),
            origin,
            Self::surface_root(surface).id().protocol_id(),
            self.surface_has_popup_ancestor(surface),
            self.surface_owner_debug(surface),
        )
    }

    fn window_is_at_or_below(
        &self,
        candidate: &smithay::desktop::Window,
        target: &smithay::desktop::Window,
    ) -> bool {
        for window in self.windows_top_to_bottom() {
            if window == target {
                return true;
            }
            if window == candidate {
                return false;
            }
        }
        false
    }

    fn pointer_allows_window_interaction(
        &self,
        pointer_surface: Option<
            &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
        >,
        window: &smithay::desktop::Window,
    ) -> bool {
        if self.pointer_contents.layer.is_some() {
            return false;
        }

        match pointer_surface {
            Some(surface) => {
                if self.surface_has_popup_ancestor(surface) {
                    return false;
                }
                self.surface_is_over_window_non_popup_tree(surface, window)
                    || self
                        .non_popup_window_for_surface(surface)
                        .is_some_and(|surface_window| {
                            self.window_is_at_or_below(&surface_window, window)
                        })
            }
            None => true,
        }
    }

    fn log_stack_hit_debug(
        &self,
        label: &'static str,
        pos: smithay::utils::Point<f64, smithay::utils::Logical>,
    ) {
        if !stack_hit_debug_enabled() {
            return;
        }

        let logical_pos = LogicalPoint::new(pos.x.floor() as i32, pos.y.floor() as i32);
        let pointer_surface = self
            .pointer_contents
            .surface
            .as_ref()
            .map(|(surface, origin)| (surface.id().protocol_id(), *origin));
        let pointer_surface_detail = self
            .pointer_contents
            .surface
            .as_ref()
            .map(|(surface, origin)| self.surface_hit_debug(surface, *origin));
        let fresh_surface = self
            .surface_under(pos)
            .map(|(surface, origin)| (surface.id().protocol_id(), origin));
        let fresh_surface_detail = self
            .surface_under(pos)
            .map(|(surface, origin)| self.surface_hit_debug(&surface, origin));
        let decoration_under = self.decoration_under(pos).map(|(window, hit)| {
            let snapshot = self.snapshot_window(&window);
            let allowed = self.pointer_allows_window_interaction(
                self.pointer_contents
                    .surface
                    .as_ref()
                    .map(|(surface, _)| surface),
                &window,
            );
            (
                snapshot.id,
                snapshot.title,
                snapshot.app_id,
                self.managed_window_z_index(&window),
                hit,
                allowed,
            )
        });
        let surface_hits_by_window = self
            .windows_top_to_bottom()
            .into_iter()
            .enumerate()
            .filter_map(|(stack_index, window)| {
                let snapshot = self.snapshot_window(window);
                let location = self.space.element_location(window)?;
                let local_pos = if let Some(decoration) = self.window_decorations.get(window) {
                    if !self.decoration_allows_input_at(decoration, pos) {
                        return None;
                    }
                    inverse_transform_point(
                        pos,
                        decoration.layout.root.rect,
                        decoration.visual_transform,
                    ) - location.to_f64()
                } else {
                    pos - location.to_f64()
                };
                let (surface, loc) = window.surface_under(local_pos, WindowSurfaceType::ALL)?;
                Some((
                    stack_index,
                    snapshot.id,
                    snapshot.title,
                    snapshot.app_id,
                    self.managed_window_z_index(window),
                    local_pos,
                    surface.id().protocol_id(),
                    loc,
                    Self::surface_root(&surface).id().protocol_id(),
                    self.surface_has_popup_ancestor(&surface),
                    Self::surface_is_window_root_for_debug(window, &surface),
                ))
            })
            .collect::<Vec<_>>();
        let transformed_window_under =
            self.window_under_transformed(logical_pos)
                .map(|(window, _)| {
                    let snapshot = self.snapshot_window(window);
                    (
                        snapshot.id,
                        snapshot.title,
                        snapshot.app_id,
                        self.managed_window_z_index(window),
                    )
                });
        let raw_window_under = self.raw_window_under(logical_pos).map(|(window, rect)| {
            let snapshot = self.snapshot_window(window);
            (
                snapshot.id,
                snapshot.title,
                snapshot.app_id,
                self.managed_window_z_index(window),
                rect,
            )
        });
        let space_order = self
            .space
            .elements()
            .enumerate()
            .map(|(index, window)| {
                let snapshot = self.snapshot_window(window);
                (
                    index,
                    snapshot.id,
                    snapshot.title,
                    snapshot.app_id,
                    snapshot.is_focused,
                    self.managed_window_z_index(window),
                    self.window_decorations.get(window).map(|decoration| {
                        (
                            decoration.managed_window.managed,
                            decoration.managed_window.z_index,
                            decoration.layout.root.rect,
                            transformed_root_rect(
                                decoration.layout.root.rect,
                                decoration.visual_transform,
                            )
                            .contains(logical_pos),
                        )
                    }),
                )
            })
            .collect::<Vec<_>>();
        let sorted_order = self
            .windows_top_to_bottom()
            .into_iter()
            .enumerate()
            .map(|(index, window)| {
                let snapshot = self.snapshot_window(window);
                (
                    index,
                    snapshot.id,
                    snapshot.title,
                    snapshot.app_id,
                    snapshot.is_focused,
                    self.managed_window_z_index(window),
                    self.window_decorations.get(window).map(|decoration| {
                        (
                            decoration.managed_window.managed,
                            decoration.managed_window.z_index,
                            decoration.layout.root.rect,
                            transformed_root_rect(
                                decoration.layout.root.rect,
                                decoration.visual_transform,
                            )
                            .contains(logical_pos),
                        )
                    }),
                )
            })
            .collect::<Vec<_>>();

        info!(
            label,
            pointer_location = ?pos,
            logical_pos = ?logical_pos,
            pointer_contents_surface = ?pointer_surface,
            pointer_contents_surface_detail = ?pointer_surface_detail,
            fresh_surface_under = ?fresh_surface,
            fresh_surface_under_detail = ?fresh_surface_detail,
            pointer_layer = ?self.pointer_contents.layer.as_ref().map(|layer| {
                (
                    layer.wl_surface().id().protocol_id(),
                    layer.layer(),
                    layer.cached_state().keyboard_interactivity,
                )
            }),
            decoration_under = ?decoration_under,
            transformed_window_under = ?transformed_window_under,
            raw_window_under = ?raw_window_under,
            surface_hits_by_window = ?surface_hits_by_window,
            space_order = ?space_order,
            sorted_order = ?sorted_order,
            "stack hit debug"
        );
    }

    fn tracked_decoration_interaction_target_under(
        &self,
        pos: smithay::utils::Point<f64, smithay::utils::Logical>,
    ) -> Option<TrackedDecorationInteractionTarget> {
        let pointer_contents = self.pointer_contents_at(pos);
        if pointer_contents.layer.is_some() {
            return None;
        }

        let (window, target) = self.decoration_interaction_target_under(pos)?;
        self.pointer_allows_window_interaction(
            pointer_contents
                .surface
                .as_ref()
                .map(|(surface, _)| surface),
            &window,
        )
        .then(|| TrackedDecorationInteractionTarget {
            window_id: self.snapshot_window(&window).id,
            window,
            target,
        })
    }

    fn update_decoration_hover_target(
        &mut self,
        pos: smithay::utils::Point<f64, smithay::utils::Logical>,
    ) {
        let next = self
            .tracked_decoration_interaction_target_under(pos)
            .filter(|target| target.target.handlers.hover_change.is_some());
        if self
            .decoration_hover_target
            .as_ref()
            .zip(next.as_ref())
            .is_some_and(|(current, next)| current.same_node(next))
        {
            return;
        }

        if let Some(previous) = self.decoration_hover_target.take()
            && let Some(handler) = previous.target.handlers.hover_change.as_ref()
        {
            self.invoke_decoration_runtime_handler(
                &previous.window,
                &previous.window_id,
                handler.handler_for(false),
            );
        }

        if let Some(next_target) = next {
            if let Some(handler) = next_target.target.handlers.hover_change.as_ref() {
                self.invoke_decoration_runtime_handler(
                    &next_target.window,
                    &next_target.window_id,
                    handler.handler_for(true),
                );
            }
            self.decoration_hover_target = Some(next_target);
        }
    }

    fn press_decoration_active_target(
        &mut self,
        pos: smithay::utils::Point<f64, smithay::utils::Logical>,
    ) {
        let next = self
            .tracked_decoration_interaction_target_under(pos)
            .filter(|target| target.target.handlers.active_change.is_some());
        if self
            .decoration_active_target
            .as_ref()
            .zip(next.as_ref())
            .is_some_and(|(current, next)| current.same_node(next))
        {
            return;
        }

        self.release_decoration_active_target();

        if let Some(next_target) = next {
            if let Some(handler) = next_target.target.handlers.active_change.as_ref() {
                self.invoke_decoration_runtime_handler(
                    &next_target.window,
                    &next_target.window_id,
                    handler.handler_for(true),
                );
            }
            self.decoration_active_target = Some(next_target);
        }
    }

    fn release_decoration_active_target(&mut self) {
        if let Some(previous) = self.decoration_active_target.take()
            && let Some(handler) = previous.target.handlers.active_change.as_ref()
        {
            self.invoke_decoration_runtime_handler(
                &previous.window,
                &previous.window_id,
                handler.handler_for(false),
            );
        }
    }

    fn invoke_decoration_runtime_handler(
        &mut self,
        window: &Window,
        window_id: &str,
        handler_id: &str,
    ) -> bool {
        let now_ms = std::time::Duration::from(self.clock.now()).as_millis() as u64;
        self.sync_runtime_display_state();
        let Ok(invocation) = self
            .decoration_evaluator
            .invoke_handler(window_id, handler_id, now_ms)
        else {
            return false;
        };

        self.consume_runtime_display_config(invocation.display_config.clone());
        self.consume_runtime_workspace_config(invocation.workspace_config.clone());
        self.consume_runtime_key_binding_config(invocation.key_binding_config.clone());
        self.consume_runtime_pointer_config(invocation.pointer_config.clone());
        self.consume_runtime_input_config(invocation.input_config.clone());
        self.consume_runtime_event_config(invocation.event_config.clone());
        self.consume_runtime_process_config(invocation.process_config.clone());
        if !invocation.process_actions.is_empty() {
            self.apply_runtime_process_actions(invocation.process_actions.clone());
        }
        self.apply_runtime_handler_invocation(window, &invocation);

        let invoked = invocation.invoked;
        if invoked {
            self.mark_runtime_dirty_windows(
                invocation.dirty_window_ids,
                invocation.dirty_managed_window_ids,
            );
            self.runtime_scheduler_enabled = invocation.next_poll_in_ms.is_some();
            self.apply_runtime_window_actions(invocation.actions);
            self.schedule_redraw();
        }

        invoked
    }

    pub(crate) fn update_decoration_cursor_icon(
        &mut self,
        pos: smithay::utils::Point<f64, smithay::utils::Logical>,
    ) {
        let pointer_contents = self.pointer_contents_at(pos);
        let next_override = if pointer_contents.layer.is_some() {
            None
        } else {
            self.decoration_under(pos).and_then(|(window, hit)| {
                self.pointer_allows_window_interaction(
                    pointer_contents
                        .surface
                        .as_ref()
                        .map(|(surface, _)| surface),
                    &window,
                )
                .then_some(hit)
                .and_then(|hit| match hit {
                    DecorationHitTestResult::Resize(edges) => {
                        Some(resize_edges_to_cursor_icon(edges))
                    }
                    DecorationHitTestResult::Move
                    | DecorationHitTestResult::Action(_)
                    | DecorationHitTestResult::Outside => Some(CursorIcon::Default),
                    DecorationHitTestResult::ClientArea => None,
                })
            })
        };

        if self.cursor_override != next_override {
            self.cursor_override = next_override;
            self.schedule_redraw();
        }
    }

    pub(crate) fn focus_window(&mut self, window: &smithay::desktop::Window, serial: Serial) {
        self.focus_window_at_surface(window, None, serial);
    }

    pub(crate) fn apply_pending_initial_focus_for_window(
        &mut self,
        window_id: &str,
        window: &smithay::desktop::Window,
    ) {
        if !self.pending_initial_focus_window_ids.remove(window_id) {
            return;
        }

        let serial = SERIAL_COUNTER.next_serial();
        self.focus_window(window, serial);
    }

    pub(crate) fn focus_window_at_surface(
        &mut self,
        window: &smithay::desktop::Window,
        surface: Option<&smithay::reexports::wayland_server::protocol::wl_surface::WlSurface>,
        serial: Serial,
    ) {
        let started_at = Instant::now();
        let window_id = window
            .toplevel()
            .map(|toplevel| toplevel.wl_surface().id().protocol_id())
            .unwrap_or_default();
        if !self
            .window_decorations
            .get(window)
            .is_some_and(|decoration| {
                decoration.managed_window.managed && decoration.managed_window.z_index.is_some()
            })
        {
            self.space.raise_element(window, true);
        }
        self.update_xwayland_refresh_override_for_window(window, "window-focus");
        self.set_window_keyboard_focus_target_surface(window, surface);
        self.focus_layer_surface_if_on_demand(None);
        self.update_keyboard_focus(serial);
        self.sync_wlr_foreign_toplevel_states();
        debug!(
            window_id,
            focus_surface_id = surface.map(|surface| surface.id().protocol_id()),
            elapsed_ms = started_at.elapsed().as_secs_f64() * 1000.0,
            "focus_window finished"
        );
    }

    pub(crate) fn refresh_pointer_focus(&mut self, time_msec: u32) {
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        if pointer.is_grabbed() {
            return;
        }
        if self.active_pointer_lock_focus(&pointer).is_some() {
            return;
        }

        let location = pointer.current_location();
        self.pointer_contents = self.pointer_contents_at(location);
        let under = self.pointer_contents.surface.clone();
        pointer.motion(
            self,
            under,
            &MotionEvent {
                location,
                serial: SERIAL_COUNTER.next_serial(),
                time: time_msec,
            },
        );
        pointer.frame(self);
        self.update_decoration_hover_target(location);
        self.update_decoration_cursor_icon(location);
    }

    fn active_pointer_lock_focus(
        &self,
        pointer: &smithay::input::pointer::PointerHandle<Self>,
    ) -> Option<(
        smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
        smithay::utils::Point<f64, smithay::utils::Logical>,
    )> {
        let focus = self.pointer_contents.surface.clone()?;
        if pointer.current_focus().as_ref() != Some(&focus.0) {
            return None;
        }
        let location = pointer.current_location();
        let locked = with_pointer_constraint(&focus.0, pointer, |constraint| {
            constraint.is_some_and(|constraint| {
                constraint.is_active()
                    && matches!(&*constraint, PointerConstraint::Locked(_))
                    && constraint
                        .region()
                        .is_none_or(|region| region.contains((location - focus.1).to_i32_floor()))
            })
        });
        locked.then_some(focus)
    }

    fn activate_pointer_constraint_at(
        &mut self,
        location: smithay::utils::Point<f64, smithay::utils::Logical>,
    ) {
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        let Some((surface, surface_origin)) = self.surface_under(location) else {
            return;
        };
        if pointer.current_focus().as_ref() != Some(&surface) {
            return;
        }

        with_pointer_constraint(&surface, &pointer, |constraint| {
            let Some(constraint) = constraint.filter(|constraint| !constraint.is_active()) else {
                return;
            };
            let local = (location - surface_origin).to_i32_floor();
            if constraint
                .region()
                .is_none_or(|region| region.contains(local))
            {
                constraint.activate();
            }
        });
    }
}

fn resize_edges_to_cursor_icon(edges: ResizeEdges) -> CursorIcon {
    match edges {
        edges if edges == (ResizeEdges::TOP | ResizeEdges::LEFT) => CursorIcon::NwResize,
        edges if edges == (ResizeEdges::TOP | ResizeEdges::RIGHT) => CursorIcon::NeResize,
        edges if edges == (ResizeEdges::BOTTOM | ResizeEdges::LEFT) => CursorIcon::SwResize,
        edges if edges == (ResizeEdges::BOTTOM | ResizeEdges::RIGHT) => CursorIcon::SeResize,
        edges if edges == ResizeEdges::LEFT => CursorIcon::WResize,
        edges if edges == ResizeEdges::RIGHT => CursorIcon::EResize,
        edges if edges == ResizeEdges::TOP => CursorIcon::NResize,
        edges if edges == ResizeEdges::BOTTOM => CursorIcon::SResize,
        edges if edges.intersects(ResizeEdges::LEFT | ResizeEdges::RIGHT) => CursorIcon::EwResize,
        edges if edges.intersects(ResizeEdges::TOP | ResizeEdges::BOTTOM) => CursorIcon::NsResize,
        _ => CursorIcon::AllResize,
    }
}

fn resize_edges_to_grab(edges: ResizeEdges) -> ResizeEdge {
    let mut converted = ResizeEdge::empty();
    if edges.contains(ResizeEdges::TOP) {
        converted |= ResizeEdge::TOP;
    }
    if edges.contains(ResizeEdges::BOTTOM) {
        converted |= ResizeEdge::BOTTOM;
    }
    if edges.contains(ResizeEdges::LEFT) {
        converted |= ResizeEdge::LEFT;
    }
    if edges.contains(ResizeEdges::RIGHT) {
        converted |= ResizeEdge::RIGHT;
    }
    converted
}

#[cfg(test)]
mod pointer_output_constraint_tests {
    use super::constrain_pointer_location_to_outputs;
    use smithay::utils::{Logical, Point, Rectangle};

    fn outputs_with_vertical_notch() -> [Rectangle<i32, Logical>; 2] {
        [
            Rectangle::new((0, 0).into(), (100, 100).into()),
            Rectangle::new((100, 25).into(), (100, 50).into()),
        ]
    }

    #[test]
    fn preserves_locations_inside_either_output() {
        let outputs = outputs_with_vertical_notch();

        assert_eq!(
            constrain_pointer_location_to_outputs(Point::from((20.25, 80.75)), outputs),
            Some(Point::from((20.25, 80.75)))
        );
        assert_eq!(
            constrain_pointer_location_to_outputs(Point::from((150.25, 30.75)), outputs),
            Some(Point::from((150.25, 30.75)))
        );
    }

    #[test]
    fn blocks_motion_into_an_uncovered_notch() {
        let constrained = constrain_pointer_location_to_outputs(
            Point::from((100.5, 10.0)),
            outputs_with_vertical_notch(),
        );

        assert_eq!(constrained, Some(Point::from((99.5, 10.0))));
    }

    #[test]
    fn allows_motion_across_a_shared_output_edge() {
        let constrained = constrain_pointer_location_to_outputs(
            Point::from((100.5, 30.0)),
            outputs_with_vertical_notch(),
        );

        assert_eq!(constrained, Some(Point::from((100.5, 30.0))));
    }

    #[test]
    fn projects_to_the_nearest_output_edge_outside_the_union() {
        let constrained = constrain_pointer_location_to_outputs(
            Point::from((150.0, 90.0)),
            outputs_with_vertical_notch(),
        );

        assert_eq!(constrained, Some(Point::from((150.0, 74.5))));
    }
}
