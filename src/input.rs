use smithay::{
    backend::input::{
        AbsolutePositionEvent, Axis, AxisSource, ButtonState, Event, InputBackend, InputEvent,
        KeyboardKeyEvent, PointerAxisEvent, PointerButtonEvent, PointerMotionEvent,
    },
    input::{
        keyboard::{FilterResult, keysyms},
        pointer::{AxisFrame, ButtonEvent, MotionEvent},
    },
    reexports::wayland_server::{protocol::wl_surface::WlSurface, Resource},
    utils::{SERIAL_COUNTER, Serial},
};
use std::time::Instant;
use tracing::debug;

use crate::{
    grabs::{
        move_grab::MoveSurfaceGrab,
        resize_grab::{ResizeEdge, ResizeSurfaceGrab},
    },
    ssd::{DecorationHitTestResult, ResizeEdges, WindowAction},
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

                if let KeyboardAction::Quit = action {
                    self.shutdown();
                }
            }
            InputEvent::PointerMotion { event, .. } => {
                let output = self.space.outputs().next().unwrap();
                let output_geo = self.space.output_geometry(output).unwrap();

                let pointer = self.seat.get_pointer().unwrap();
                let mut pos = pointer.current_location() + event.delta();

                pos.x = pos.x.clamp(
                    output_geo.loc.x as f64,
                    (output_geo.loc.x + output_geo.size.w - 1) as f64,
                );
                pos.y = pos.y.clamp(
                    output_geo.loc.y as f64,
                    (output_geo.loc.y + output_geo.size.h - 1) as f64,
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

                self.schedule_redraw();
            }
            InputEvent::PointerMotionAbsolute { event, .. } => {
                let output = self.space.outputs().next().unwrap();

                let output_geo = self.space.output_geometry(output).unwrap();

                let pos = event.position_transformed(output_geo.size) + output_geo.loc.to_f64();

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

                        pointer.button(
                            self,
                            &ButtonEvent {
                                button,
                                state: button_state,
                                serial,
                                time: event.time_msec(),
                            },
                        );

                        match hit {
                            DecorationHitTestResult::Action(WindowAction::Close) => {
                                if let Some(toplevel) = window.toplevel() {
                                    toplevel.send_close();
                                }
                            }
                            DecorationHitTestResult::Action(_) => {}
                            DecorationHitTestResult::Move => {
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
                            DecorationHitTestResult::ClientArea
                            | DecorationHitTestResult::Outside => {}
                        }

                        pointer.frame(self);
                        self.schedule_redraw();
                        return;
                    } else if let Some((window, _loc)) = self
                        .space
                        .element_under(pointer.current_location())
                        .map(|(w, l)| (w.clone(), l))
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
}

impl ShojiWM {
    fn focus_window(&mut self, window: &smithay::desktop::Window, serial: Serial) {
        let started_at = Instant::now();
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
            self.seat
                .get_keyboard()
                .unwrap()
                .set_focus(self, Some(toplevel.wl_surface().clone()), serial);
        }

        self.schedule_redraw();
        debug!(
            window_id = window
                .toplevel()
                .map(|toplevel| toplevel.wl_surface().id().protocol_id())
                .unwrap_or_default(),
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
