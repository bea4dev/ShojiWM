use std::time::Duration;

use std::collections::HashMap;

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
        let throttle = Some(Duration::from_secs(1));

        self.signal_post_repaint_barriers(output);

        self.space.elements().for_each(|window| {
            if self.space.outputs_for_element(window).contains(output) {
                window.send_frame(output, time, throttle, surface_primary_scanout_output);
            }
        });

        let map = layer_map_for_output(output);
        for layer_surface in map.layers() {
            layer_surface.send_frame(output, time, throttle, surface_primary_scanout_output);
        }
    }
}
