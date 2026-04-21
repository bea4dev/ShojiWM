use smithay::{
    backend::input::{
        AbsolutePositionEvent, Axis, AxisSource, ButtonState, Event, InputBackend, InputEvent,
        KeyState, KeyboardKeyEvent, PointerAxisEvent, PointerButtonEvent, PointerMotionEvent,
    },
    input::{
        keyboard::{FilterResult, keysyms},
        pointer::{AxisFrame, ButtonEvent, CursorIcon, MotionEvent},
    },
    reexports::wayland_server::Resource,
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
    RuntimeKeyBinding(String),
}

fn layer_focus_debug_enabled() -> bool {
    std::env::var_os("SHOJI_LAYER_FOCUS_DEBUG").is_some()
}

fn pointer_button_debug_enabled() -> bool {
    std::env::var_os("SHOJI_POINTER_BUTTON_DEBUG").is_some()
}

fn unfocused_popup_focus_debug_enabled() -> bool {
    std::env::var_os("SHOJI_UNFOCUSED_POPUP_FOCUS_DEBUG")
        .is_some_and(|value| value != "0" && !value.is_empty())
}

impl ShojiWM {
    pub fn process_input_event<I: InputBackend>(&mut self, event: InputEvent<I>) {
        match event {
            InputEvent::Keyboard { event, .. } => {
                let serial = SERIAL_COUNTER.next_serial();
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
                        |_, modifiers, handle| {
                            if let Some(binding_id) = runtime_key_bindings
                                .iter()
                                .find(|binding| binding.matches(key_phase, modifiers, &handle))
                                .map(|binding| binding.id.clone())
                            {
                                return FilterResult::Intercept(KeyboardAction::RuntimeKeyBinding(
                                    binding_id,
                                ));
                            }

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
                    KeyboardAction::RuntimeKeyBinding(binding_id) => {
                        let now_ms = std::time::Duration::from(self.clock.now()).as_millis() as u64;
                        self.sync_runtime_display_state();
                        match self
                            .decoration_evaluator
                            .invoke_key_binding(&binding_id, now_ms)
                        {
                            Ok(invocation) => {
                                self.consume_runtime_display_config(invocation.display_config);
                                self.consume_runtime_key_binding_config(
                                    invocation.key_binding_config,
                                );
                                self.consume_runtime_process_config(invocation.process_config);
                                if !invocation.process_actions.is_empty() {
                                    self.apply_runtime_process_actions(invocation.process_actions);
                                }
                                if invocation.dirty {
                                    self.runtime_poll_dirty = true;
                                    self.runtime_dirty_window_ids
                                        .extend(invocation.dirty_window_ids.into_iter());
                                    self.request_tty_maintenance("runtime-key-binding-dirty");
                                    self.schedule_redraw();
                                }
                                if !invocation.actions.is_empty() {
                                    self.request_tty_maintenance("runtime-key-binding-actions");
                                    self.apply_runtime_window_actions(invocation.actions);
                                    self.schedule_redraw();
                                }
                                self.runtime_scheduler_enabled =
                                    invocation.next_poll_in_ms.is_some();
                                if invocation.next_poll_in_ms == Some(0) {
                                    self.request_tty_maintenance("runtime-key-binding-animation");
                                    self.schedule_redraw();
                                }
                            }
                            Err(error) => {
                                tracing::warn!(
                                    ?error,
                                    binding_id,
                                    "failed to invoke runtime key binding"
                                );
                            }
                        }
                    }
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
                self.update_decoration_cursor_icon(pos);
                self.schedule_redraw();
            }
            InputEvent::PointerButton { event, .. } => {
                let pointer = self.seat.get_pointer().unwrap();

                let serial = SERIAL_COUNTER.next_serial();

                let button = event.button_code();

                let button_state = event.state();

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
                                    self.consume_runtime_key_binding_config(
                                        invocation.key_binding_config.clone(),
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

                                    if let Some(grab) = ResizeSurfaceGrab::start(
                                        start_data,
                                        window,
                                        resize_edges_to_grab(edges),
                                        smithay::utils::Rectangle::new(
                                            initial_window_location,
                                            initial_window_size,
                                        ),
                                    ) {
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
                        self.focus_window(&window, serial);
                    } else {
                        self.focus_layer_surface_if_on_demand(self.pointer_contents.layer.clone());
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
            Some(surface) => self.surface_is_over_window_non_popup_tree(surface, window),
            None => true,
        }
    }

    fn update_decoration_cursor_icon(
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

    fn focus_window(&mut self, window: &smithay::desktop::Window, serial: Serial) {
        let started_at = Instant::now();
        let window_id = window
            .toplevel()
            .map(|toplevel| toplevel.wl_surface().id().protocol_id())
            .unwrap_or_default();
        self.space.raise_element(window, true);
        self.set_window_keyboard_focus_target(Some(window));
        self.focus_layer_surface_if_on_demand(None);
        self.update_keyboard_focus(serial);
        debug!(
            window_id,
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
