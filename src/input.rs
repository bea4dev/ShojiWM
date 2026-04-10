use smithay::{
    backend::input::{
        AbsolutePositionEvent, Axis, AxisSource, ButtonState, Event, InputBackend, InputEvent,
        KeyboardKeyEvent, PointerAxisEvent, PointerButtonEvent, PointerMotionEvent,
    },
    input::{
        keyboard::{FilterResult, keysyms},
        pointer::{AxisFrame, ButtonEvent, CursorIcon, MotionEvent},
    },
    reexports::wayland_server::{Resource, protocol::wl_surface::WlSurface},
    utils::{SERIAL_COUNTER, Serial},
};
use std::time::Instant;
use tracing::debug;

use crate::{
    grabs::{
        move_grab::MoveSurfaceGrab,
        resize_grab::{ResizeEdge, ResizeSurfaceGrab},
    },
    ssd::{
        DecorationEvaluator, DecorationHitTestResult, LogicalPoint, ResizeEdges,
        RuntimeWindowAction, WindowAction,
    },
    state::ShojiWM,
};

enum KeyboardAction {
    Forward,
    Quit,
}

impl ShojiWM {
    pub fn process_input_event<I: InputBackend>(&mut self, event: InputEvent<I>) {
        match event {
            InputEvent::Keyboard { event, .. } => {
                let serial = SERIAL_COUNTER.next_serial();
                let time = Event::time_msec(&event);

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
                        |_, modifiers, handle| {
                            let keysym = handle.modified_sym();

                            if modifiers.logo && keysym.raw() == keysyms::KEY_q {
                                FilterResult::Intercept(KeyboardAction::Quit)
                            } else {
                                FilterResult::Forward
                            }
                        },
                    )
                    .unwrap_or(KeyboardAction::Forward);

                match action {
                    KeyboardAction::Quit => self.shutdown(),
                    KeyboardAction::Forward => {}
                }
            }
            InputEvent::PointerMotion { event, .. } => {
                let Some(output_bounds) = self.output_layout_bounds() else {
                    return;
                };

                let pointer = self.seat.get_pointer().unwrap();
                let mut pos = pointer.current_location() + event.delta();

                pos.x = pos.x.clamp(
                    output_bounds.loc.x as f64,
                    (output_bounds.loc.x + output_bounds.size.w - 1) as f64,
                );
                pos.y = pos.y.clamp(
                    output_bounds.loc.y as f64,
                    (output_bounds.loc.y + output_bounds.size.h - 1) as f64,
                );

                let serial = SERIAL_COUNTER.next_serial();
                let under = self.surface_under(pos);

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

                self.update_decoration_cursor_icon(pos);
                self.schedule_redraw();
            }
            InputEvent::PointerMotionAbsolute { event, .. } => {
                let Some(output_bounds) = self.output_layout_bounds() else {
                    return;
                };

                let pos =
                    event.position_transformed(output_bounds.size) + output_bounds.loc.to_f64();

                let serial = SERIAL_COUNTER.next_serial();

                let pointer = self.seat.get_pointer().unwrap();

                let under = self.surface_under(pos);

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
                self.update_decoration_cursor_icon(pos);
                self.schedule_redraw();
            }
            InputEvent::PointerButton { event, .. } => {
                let pointer = self.seat.get_pointer().unwrap();

                let serial = SERIAL_COUNTER.next_serial();

                let button = event.button_code();

                let button_state = event.state();

                if ButtonState::Pressed == button_state && !pointer.is_grabbed() {
                    let _ = self.refresh_window_decorations();

                    if let Some((window, hit)) = self.decoration_under(pointer.current_location()) {
                        self.focus_window(&window, serial);

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
                                if let Some(toplevel) = window.toplevel() {
                                    toplevel.send_close();
                                }
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
                                        self.runtime_dirty_window_ids
                                            .extend(invocation.dirty_window_ids.into_iter());
                                        self.runtime_scheduler_enabled =
                                            invocation.next_poll_in_ms.is_some();
                                        self.apply_runtime_window_actions(invocation.actions);
                                        self.schedule_redraw();
                                    }
                                }
                            }
                            DecorationHitTestResult::Action(_) => {
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
                            DecorationHitTestResult::Move => {
                                pointer.button(
                                    self,
                                    &ButtonEvent {
                                        button,
                                        state: button_state,
                                        serial,
                                        time: event.time_msec(),
                                    },
                                );
                                if let (Some(start_data), Some(initial_window_location)) = (
                                    pointer.grab_start_data(),
                                    self.space.element_location(&window),
                                ) {
                                    let grab = MoveSurfaceGrab {
                                        start_data,
                                        window,
                                        initial_window_location,
                                    };
                                    pointer.set_grab(
                                        self,
                                        grab,
                                        serial,
                                        smithay::input::pointer::Focus::Clear,
                                    );
                                }
                            }
                            DecorationHitTestResult::Resize(edges) => {
                                pointer.button(
                                    self,
                                    &ButtonEvent {
                                        button,
                                        state: button_state,
                                        serial,
                                        time: event.time_msec(),
                                    },
                                );
                                if let (Some(start_data), Some(initial_window_location)) = (
                                    pointer.grab_start_data(),
                                    self.space.element_location(&window),
                                ) {
                                    let initial_window_size = window.geometry().size;
                                    if let Some(toplevel) = window.toplevel() {
                                        toplevel.with_pending_state(|state| {
                                            state.states.set(
                                                smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::State::Resizing,
                                            );
                                        });
                                        toplevel.send_pending_configure();
                                    }

                                    let grab = ResizeSurfaceGrab::start(
                                        start_data,
                                        window,
                                        resize_edges_to_grab(edges),
                                        smithay::utils::Rectangle::new(
                                            initial_window_location,
                                            initial_window_size,
                                        ),
                                    );
                                    pointer.set_grab(
                                        self,
                                        grab,
                                        serial,
                                        smithay::input::pointer::Focus::Clear,
                                    );
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
                        self.schedule_redraw();
                        return;
                    } else if let Some((window, _loc)) = self
                        .window_under_transformed(LogicalPoint::new(
                            pointer.current_location().x.floor() as i32,
                            pointer.current_location().y.floor() as i32,
                        ))
                        .map(|(w, _)| (w.clone(), ()))
                    {
                        self.focus_window(&window, serial);
                    } else {
                        self.clear_focus(serial);
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
            }
            InputEvent::PointerAxis { event, .. } => {
                let source = event.source();

                let horizontal_amount = event.amount(Axis::Horizontal).unwrap_or_else(|| {
                    event.amount_v120(Axis::Horizontal).unwrap_or(0.0) * 15.0 / 120.
                });
                let vertical_amount = event.amount(Axis::Vertical).unwrap_or_else(|| {
                    event.amount_v120(Axis::Vertical).unwrap_or(0.0) * 15.0 / 120.
                });
                let horizontal_amount_discrete = event.amount_v120(Axis::Horizontal);
                let vertical_amount_discrete = event.amount_v120(Axis::Vertical);

                let mut frame = AxisFrame::new(event.time_msec()).source(source);
                if horizontal_amount != 0.0 {
                    frame = frame.value(Axis::Horizontal, horizontal_amount);
                    if let Some(discrete) = horizontal_amount_discrete {
                        frame = frame.v120(Axis::Horizontal, discrete as i32);
                    }
                }
                if vertical_amount != 0.0 {
                    frame = frame.value(Axis::Vertical, vertical_amount);
                    if let Some(discrete) = vertical_amount_discrete {
                        frame = frame.v120(Axis::Vertical, discrete as i32);
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
                pointer.axis(self, frame);
                pointer.frame(self);
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
                self.closing_window_snapshots
                    .remove(&runtime_action.window_id);
                self.live_window_snapshots.remove(&runtime_action.window_id);
                self.complete_window_snapshots
                    .remove(&runtime_action.window_id);
                self.windows_ready_for_decoration
                    .remove(&runtime_action.window_id);
                self.snapshot_dirty_window_ids
                    .remove(&runtime_action.window_id);
                let _ = self
                    .decoration_evaluator
                    .window_closed(&runtime_action.window_id);
                self.runtime_dirty_window_ids
                    .remove(&runtime_action.window_id);
                self.schedule_redraw();
                continue;
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
                    if let Some(toplevel) = window.toplevel() {
                        toplevel.send_close();
                    }
                }
                crate::ssd::WaylandWindowAction::Maximize => {
                    if let Some(toplevel) = window.toplevel() {
                        toplevel.with_pending_state(|state| {
                            state.states.set(
                                smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::State::Maximized,
                            );
                        });
                        toplevel.send_pending_configure();
                    }
                }
                crate::ssd::WaylandWindowAction::FinalizeClose => {}
                crate::ssd::WaylandWindowAction::Minimize => {}
            }
        }
    }
}

impl ShojiWM {
    fn update_decoration_cursor_icon(
        &mut self,
        pos: smithay::utils::Point<f64, smithay::utils::Logical>,
    ) {
        let next_override = self.decoration_under(pos).and_then(|(_, hit)| match hit {
            DecorationHitTestResult::Resize(edges) => Some(resize_edges_to_cursor_icon(edges)),
            DecorationHitTestResult::Move
            | DecorationHitTestResult::Action(_)
            | DecorationHitTestResult::Outside => Some(CursorIcon::Default),
            DecorationHitTestResult::ClientArea => None,
        });

        if self.cursor_override != next_override {
            self.cursor_override = next_override;
            self.schedule_redraw();
        }
    }

    fn focus_window(&mut self, window: &smithay::desktop::Window, serial: Serial) {
        let started_at = Instant::now();
        let window_id = window
            .toplevel()
            .map(|toplevel| toplevel.wl_surface().id().protocol_id())
            .unwrap_or_default();
        self.space.raise_element(window, true);

        for candidate in self.space.elements() {
            let should_activate = candidate == window;
            if candidate.set_activated(should_activate) {
                if let Some(toplevel) = candidate.toplevel() {
                    let _ = toplevel.send_pending_configure();
                }
            }
        }

        if let Some(toplevel) = window.toplevel() {
            self.seat.get_keyboard().unwrap().set_focus(
                self,
                Some(toplevel.wl_surface().clone()),
                serial,
            );
        }

        self.schedule_redraw();
        debug!(
            window_id,
            elapsed_ms = started_at.elapsed().as_secs_f64() * 1000.0,
            "focus_window finished"
        );
    }

    fn clear_focus(&mut self, serial: Serial) {
        let started_at = Instant::now();
        for window in self.space.elements() {
            if window.set_activated(false) {
                if let Some(toplevel) = window.toplevel() {
                    let _ = toplevel.send_pending_configure();
                }
            }
        }

        self.seat
            .get_keyboard()
            .unwrap()
            .set_focus(self, Option::<WlSurface>::None, serial);

        self.schedule_redraw();
        debug!(
            elapsed_ms = started_at.elapsed().as_secs_f64() * 1000.0,
            "clear_focus finished"
        );
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
