use std::{cell::RefCell, collections::HashMap, time::Duration};

use smithay::{
    backend::renderer::element::{
        default_primary_scanout_output_compare, RenderElementStates,
    },
    desktop::{
        utils::{
            surface_presentation_feedback_flags_from_states, surface_primary_scanout_output,
            update_surface_primary_scanout_output, with_surfaces_surface_tree, OutputPresentationFeedback,
        },
        layer_map_for_output, Space, Window,
    },
    output::Output,
    reexports::wayland_server::{backend::ClientId, Client, Resource},
    utils::{Monotonic, Time},
    wayland::{
        commit_timing::CommitTimerBarrierStateUserData,
        compositor::CompositorHandler,
        fifo::FifoBarrierCachedState,
        fractional_scale::with_fractional_scale,
    },
};

use crate::state::ShojiWM;

#[derive(Default)]
struct SurfaceFrameThrottlingState {
    last_sent_at: RefCell<Option<(Output, u32)>>,
}

fn frame_callback_debug_enabled() -> bool {
    std::env::var_os("SHOJI_FRAME_CALLBACK_DEBUG")
        .is_some_and(|value| value != "0" && !value.is_empty())
}

pub fn update_primary_scanout_output(
    space: &Space<Window>,
    output: &Output,
    cursor_status: &smithay::input::pointer::CursorImageStatus,
    render_element_states: &RenderElementStates,
) {
    // Keep smithay's primary-scanout bookkeeping in sync with the surfaces we actually rendered.
    //
    // This turned out to matter for Chrome on the TTY backend: without updating the primary
    // scanout output before collecting presentation feedback, Chrome would often behave as if the
    // output cadence was only ~60 Hz even when the monitor was actually running at 66 Hz.
    space.elements().for_each(|window| {
        window.with_surfaces(|surface, states| {
            update_surface_primary_scanout_output(
                surface,
                output,
                states,
                None,
                render_element_states,
                default_primary_scanout_output_compare,
            );
        });
    });

    let map = layer_map_for_output(output);
    for layer_surface in map.layers() {
        layer_surface.with_surfaces(|surface, states| {
            update_surface_primary_scanout_output(
                surface,
                output,
                states,
                None,
                render_element_states,
                default_primary_scanout_output_compare,
            );
        });
    }

    if let smithay::input::pointer::CursorImageStatus::Surface(surface) = cursor_status {
        with_surfaces_surface_tree(surface, |surface, states| {
            update_surface_primary_scanout_output(
                surface,
                output,
                states,
                None,
                render_element_states,
                default_primary_scanout_output_compare,
            );
        });
    }
}

pub fn take_presentation_feedback(
    output: &Output,
    space: &Space<Window>,
    render_element_states: &RenderElementStates,
) -> OutputPresentationFeedback {
    let mut output_presentation_feedback = OutputPresentationFeedback::new(output);

    space.elements().for_each(|window| {
        if space.outputs_for_element(window).contains(output) {
            window.take_presentation_feedback(
                &mut output_presentation_feedback,
                surface_primary_scanout_output,
                |surface, _| {
                    surface_presentation_feedback_flags_from_states(
                        surface,
                        None,
                        render_element_states,
                    )
                },
            );
        }
    });

    let map = layer_map_for_output(output);
    for layer_surface in map.layers() {
        layer_surface.take_presentation_feedback(
            &mut output_presentation_feedback,
            surface_primary_scanout_output,
            |surface, _| {
                surface_presentation_feedback_flags_from_states(surface, None, render_element_states)
            },
        );
    }

    output_presentation_feedback
}

impl ShojiWM {
    pub fn send_frame_callbacks_for_output(
        &mut self,
        output: &Output,
        time: Duration,
        frame_callback_sequence: Option<u32>,
    ) {
        let throttle = Some(Duration::from_secs(1));
        let frame_callback_debug = frame_callback_debug_enabled();

        if frame_callback_debug {
            let visible_windows = self
                .space
                .elements_for_output(output)
                .filter_map(|window| {
                    self.window_decorations.get(window).map(|decoration| {
                        format!(
                            "{}:{}:{:?}",
                            decoration.snapshot.id,
                            decoration.snapshot.title,
                            decoration.snapshot.app_id
                        )
                    })
                })
                .collect::<Vec<_>>();
            tracing::trace!(
                output = %output.name(),
                sequence = frame_callback_sequence,
                visible_windows = ?visible_windows,
                "frame callback output window snapshot"
            );
        }

        let should_send = |surface: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
                           states: &smithay::wayland::compositor::SurfaceData| {
            let current_primary_output = surface_primary_scanout_output(surface, states);
            if current_primary_output.as_ref() != Some(output) {
                if frame_callback_debug {
                    tracing::trace!(
                        output = %output.name(),
                        sequence = frame_callback_sequence,
                        surface = ?surface.id(),
                        primary_output = ?current_primary_output.as_ref().map(Output::name),
                        "skipping frame callback because primary output does not match"
                    );
                }
                return None;
            }

            if let Some(sequence) = frame_callback_sequence {
                let frame_throttling_state = states
                    .data_map
                    .get_or_insert(SurfaceFrameThrottlingState::default);
                let mut last_sent_at = frame_throttling_state.last_sent_at.borrow_mut();
                if let Some((last_output, last_sequence)) = &*last_sent_at
                    && last_output == output
                    && *last_sequence == sequence
                {
                    if frame_callback_debug {
                        tracing::trace!(
                            output = %output.name(),
                            sequence,
                            surface = ?surface.id(),
                            "skipping frame callback because it was already sent this refresh cycle"
                        );
                    }
                    return None;
                }
                *last_sent_at = Some((output.clone(), sequence));
            }

            if frame_callback_debug {
                tracing::trace!(
                    output = %output.name(),
                    sequence = frame_callback_sequence,
                    surface = ?surface.id(),
                    "sending frame callback to surface"
                );
            }
            Some(output.clone())
        };

        self.space.elements().for_each(|window| {
            if self.space.outputs_for_element(window).contains(output) {
                if frame_callback_debug
                    && let Some(decoration) = self.window_decorations.get(window)
                {
                    tracing::trace!(
                        output = %output.name(),
                        sequence = frame_callback_sequence,
                        window_id = %decoration.snapshot.id,
                        title = %decoration.snapshot.title,
                        app_id = ?decoration.snapshot.app_id,
                        "considering frame callbacks for window"
                    );
                }
                window.send_frame(output, time, throttle, &should_send);
            }
        });

        let map = layer_map_for_output(output);
        for layer_surface in map.layers() {
            layer_surface.send_frame(output, time, throttle, &should_send);
        }
    }

    pub fn pre_repaint(&mut self, output: &Output, frame_target: Time<Monotonic>) {
        #[allow(clippy::mutable_key_type)]
        let mut clients: HashMap<ClientId, Client> = HashMap::new();

        self.space.elements().for_each(|window| {
            if !self.space.outputs_for_element(window).contains(output) {
                return;
            }

            window.with_surfaces(|surface, states| {
                if let Some(mut commit_timer_state) = states
                    .data_map
                    .get::<CommitTimerBarrierStateUserData>()
                    .map(|commit_timer| commit_timer.lock().unwrap())
                    && commit_timer_state.signal_until(frame_target)
                {
                    let client = surface.client().unwrap();
                    clients.insert(client.id(), client);
                }
            });
        });

        let map = layer_map_for_output(output);
        for layer_surface in map.layers() {
            layer_surface.with_surfaces(|surface, states| {
                if let Some(mut commit_timer_state) = states
                    .data_map
                    .get::<CommitTimerBarrierStateUserData>()
                    .map(|commit_timer| commit_timer.lock().unwrap())
                    && commit_timer_state.signal_until(frame_target)
                {
                    let client = surface.client().unwrap();
                    clients.insert(client.id(), client);
                }
            });
        }
        drop(map);

        let dh = self.display_handle.clone();
        for client in clients.into_values() {
            self.client_compositor_state(&client).blocker_cleared(self, &dh);
        }
    }

    pub fn signal_post_repaint_barriers(&mut self, output: &Output) {
        #[allow(clippy::mutable_key_type)]
        let mut clients: HashMap<ClientId, Client> = HashMap::new();

        self.space.elements().for_each(|window| {
            if self.space.outputs_for_element(window).contains(output) {
                window.with_surfaces(|surface, states| {
                    let primary_scanout_output = surface_primary_scanout_output(surface, states);
                    if let Some(output) = primary_scanout_output.as_ref() {
                        with_fractional_scale(states, |fractional_scale| {
                            fractional_scale.set_preferred_scale(output.current_scale().fractional_scale());
                        });
                    }
                    let fifo_barrier = states
                        .cached_state
                        .get::<FifoBarrierCachedState>()
                        .current()
                        .barrier
                        .take();
                    if let Some(fifo_barrier) = fifo_barrier {
                        fifo_barrier.signal();
                        let client = surface.client().unwrap();
                        clients.insert(client.id(), client);
                    }
                });
            }
        });

        let map = layer_map_for_output(output);
        for layer_surface in map.layers() {
            layer_surface.with_surfaces(|surface, states| {
                let primary_scanout_output = surface_primary_scanout_output(surface, states);
                if let Some(output) = primary_scanout_output.as_ref() {
                    with_fractional_scale(states, |fractional_scale| {
                        fractional_scale.set_preferred_scale(output.current_scale().fractional_scale());
                    });
                }
                let fifo_barrier = states
                    .cached_state
                    .get::<FifoBarrierCachedState>()
                    .current()
                    .barrier
                    .take();
                if let Some(fifo_barrier) = fifo_barrier {
                    fifo_barrier.signal();
                    let client = surface.client().unwrap();
                    clients.insert(client.id(), client);
                }
            });
        }

        drop(map);

        let dh = self.display_handle.clone();
        for client in clients.into_values() {
            self.client_compositor_state(&client).blocker_cleared(self, &dh);
        }
    }

    pub fn post_repaint(
        &mut self,
        output: &Output,
        time: Duration,
        _render_element_states: &RenderElementStates,
    ) {
        self.signal_post_repaint_barriers(output);
        self.send_frame_callbacks_for_output(output, time, None);
    }

    pub fn post_repaint_with_sequence(
        &mut self,
        output: &Output,
        time: Duration,
        _render_element_states: &RenderElementStates,
        frame_callback_sequence: Option<u32>,
    ) {
        self.signal_post_repaint_barriers(output);
        self.send_frame_callbacks_for_output(output, time, frame_callback_sequence);
    }
}
