use std::{
    cell::RefCell,
    collections::HashMap,
    sync::{Mutex, OnceLock},
    time::Duration,
};

use smithay::{
    backend::renderer::element::{RenderElementState, RenderElementStates},
    desktop::{
        Space, Window, layer_map_for_output,
        utils::{
            OutputPresentationFeedback, surface_presentation_feedback_flags_from_states,
            surface_primary_scanout_output, update_surface_primary_scanout_output,
            with_surfaces_surface_tree,
        },
    },
    output::Output,
    reexports::wayland_server::{Client, Resource, backend::ClientId},
    utils::{Monotonic, Time},
    wayland::{
        commit_timing::CommitTimerBarrierStateUserData, compositor::CompositorHandler,
        fifo::FifoBarrierCachedState, fractional_scale::with_fractional_scale,
    },
};
use tracing::info;

use crate::state::ShojiWM;

/// Primary scanout output comparison that picks the output with greater visible area.
///
/// Smithay's `default_primary_scanout_output_compare` also considers refresh rate: it
/// switches to the "next" output whenever that output has a higher refresh rate, regardless
/// of how much of the window is actually visible there. This causes the primary scanout output
/// to oscillate when a window overlaps two monitors of different refresh rates — for example,
/// after moving a window to an external monitor, any pixel of overlap with the internal
/// high-refresh display would immediately flip the primary back, triggering a scale change.
/// That scale change causes Chrome/Firefox to re-render all tabs, which emits new commits,
/// which trigger another frame, which causes the oscillation to repeat at high frequency.
///
/// Using visible area as the sole criterion keeps the primary on whichever output shows
/// more of the window, which is stable and matches user expectation.
pub fn area_primary_scanout_compare<'a>(
    current_output: &'a smithay::output::Output,
    current_state: &RenderElementState,
    next_output: &'a smithay::output::Output,
    next_state: &RenderElementState,
) -> &'a smithay::output::Output {
    if next_state.visible_area > current_state.visible_area {
        next_output
    } else {
        current_output
    }
}

fn frame_callback_debug_enabled() -> bool {
    std::env::var_os("SHOJI_FRAME_CALLBACK_DEBUG").is_some()
}

fn scale_notify_debug_enabled() -> bool {
    std::env::var_os("SHOJI_SCALE_NOTIFY_DEBUG").is_some()
}

fn fifo_debug_enabled() -> bool {
    std::env::var_os("SHOJI_FIFO_DEBUG").is_some()
}

/// Returns the previous preferred scale for a surface (by protocol id), for change detection.
fn previous_preferred_scale(protocol_id: u32, scale: f64) -> Option<f64> {
    static SCALES: OnceLock<Mutex<HashMap<u32, f64>>> = OnceLock::new();
    let map = SCALES.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = map.lock().ok()?;
    guard.insert(protocol_id, scale)
}

#[derive(Default)]
struct SurfaceFrameThrottlingState {
    last_sent_at: RefCell<Option<(Output, u32)>>,
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
                area_primary_scanout_compare,
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
                area_primary_scanout_compare,
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
                area_primary_scanout_compare,
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
                surface_presentation_feedback_flags_from_states(
                    surface,
                    None,
                    render_element_states,
                )
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
        let throttle = None;
        let debug = frame_callback_debug_enabled();
        let callback_count = std::cell::Cell::new(0usize);

        let should_send =
            |surface: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
             states: &smithay::wayland::compositor::SurfaceData| {
                let current_primary_output = surface_primary_scanout_output(surface, states);
                if current_primary_output.as_ref() != Some(output) {
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
                        return None;
                    }
                    *last_sent_at = Some((output.clone(), sequence));
                }

                if debug {
                    callback_count.set(callback_count.get() + 1);
                }
                Some(output.clone())
            };

        self.space.elements().for_each(|window| {
            if self.space.outputs_for_element(window).contains(output) {
                window.send_frame(output, time, throttle, &should_send);
            }
        });

        let map = layer_map_for_output(output);
        for layer_surface in map.layers() {
            layer_surface.send_frame(output, time, throttle, &should_send);
        }

        if debug {
            info!(
                output = %output.name(),
                surface_count = callback_count.get(),
                sequence = ?frame_callback_sequence,
                "frame callbacks sent"
            );
        }
    }

    pub fn pre_repaint(&mut self, output: &Output, frame_target: Time<Monotonic>) {
        #[allow(clippy::mutable_key_type)]
        let mut clients: HashMap<ClientId, Client> = HashMap::new();

        let debug_fifo = fifo_debug_enabled();
        self.space.elements().for_each(|window| {
            if !self.space.outputs_for_element(window).contains(output) {
                return;
            }

            let app_id = window
                .toplevel()
                .and_then(|t| {
                    smithay::wayland::compositor::with_states(t.wl_surface(), |states| {
                        states
                            .data_map
                            .get::<smithay::wayland::shell::xdg::XdgToplevelSurfaceData>()
                            .map(|d| d.lock().ok()?.app_id.clone())
                    })
                })
                .flatten();
            window.with_surfaces(|surface, states| {
                if let Some(mut commit_timer_state) = states
                    .data_map
                    .get::<CommitTimerBarrierStateUserData>()
                    .map(|commit_timer| commit_timer.lock().unwrap())
                    && commit_timer_state.signal_until(frame_target)
                {
                    if debug_fifo {
                        info!(
                            surface = ?surface.id(),
                            app_id = ?app_id,
                            output = %output.name(),
                            "commit timer barrier signaled for window surface"
                        );
                    }
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
            self.client_compositor_state(&client)
                .blocker_cleared(self, &dh);
        }
    }

    pub fn signal_post_repaint_barriers(&mut self, output: &Output) {
        #[allow(clippy::mutable_key_type)]
        let mut clients: HashMap<ClientId, Client> = HashMap::new();

        let debug_scale = scale_notify_debug_enabled();
        let debug_fifo = fifo_debug_enabled();
        self.space.elements().for_each(|window| {
            if self.space.outputs_for_element(window).contains(output) {
                let app_id = window
                    .toplevel()
                    .and_then(|t| {
                        smithay::wayland::compositor::with_states(t.wl_surface(), |states| {
                            states
                                .data_map
                                .get::<smithay::wayland::shell::xdg::XdgToplevelSurfaceData>()
                                .map(|d| d.lock().ok()?.app_id.clone())
                        })
                    })
                    .flatten();
                window.with_surfaces(|surface, states| {
                    let primary_scanout_output = surface_primary_scanout_output(surface, states);
                    if let Some(output) = primary_scanout_output.as_ref() {
                        let scale = output.current_scale().fractional_scale();
                        with_fractional_scale(states, |fractional_scale| {
                            if debug_scale {
                                let protocol_id = surface.id().protocol_id();
                                let prev = previous_preferred_scale(protocol_id, scale);
                                if prev != Some(scale) {
                                    info!(
                                        surface = ?surface.id(),
                                        output = %output.name(),
                                        prev_scale = ?prev,
                                        new_scale = scale,
                                        "preferred scale changed for surface"
                                    );
                                }
                            }
                            fractional_scale.set_preferred_scale(scale);
                        });
                    }
                    let fifo_barrier = states
                        .cached_state
                        .get::<FifoBarrierCachedState>()
                        .current()
                        .barrier
                        .take();
                    if let Some(fifo_barrier) = fifo_barrier {
                        if debug_fifo {
                            info!(
                                surface = ?surface.id(),
                                app_id = ?app_id,
                                output = %output.name(),
                                "fifo barrier signaled for window surface"
                            );
                        }
                        fifo_barrier.signal();
                        let client = surface.client().unwrap();
                        clients.insert(client.id(), client);
                    } else if debug_fifo {
                        info!(
                            surface = ?surface.id(),
                            app_id = ?app_id,
                            output = %output.name(),
                            "no fifo barrier for window surface"
                        );
                    }
                });
            }
        });

        let map = layer_map_for_output(output);
        for layer_surface in map.layers() {
            layer_surface.with_surfaces(|surface, states| {
                let primary_scanout_output = surface_primary_scanout_output(surface, states);
                if let Some(output) = primary_scanout_output.as_ref() {
                    let scale = output.current_scale().fractional_scale();
                    with_fractional_scale(states, |fractional_scale| {
                        if debug_scale {
                            let protocol_id = surface.id().protocol_id();
                            let prev = previous_preferred_scale(protocol_id, scale);
                            if prev != Some(scale) {
                                info!(
                                    surface = ?surface.id(),
                                    output = %output.name(),
                                    prev_scale = ?prev,
                                    new_scale = scale,
                                    "preferred scale changed for layer surface"
                                );
                            }
                        }
                        fractional_scale.set_preferred_scale(scale);
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
            self.client_compositor_state(&client)
                .blocker_cleared(self, &dh);
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
