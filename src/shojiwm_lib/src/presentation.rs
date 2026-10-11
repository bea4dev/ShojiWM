use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    sync::{Mutex, OnceLock},
    time::Duration,
};

use smithay::{
    backend::renderer::element::{
        Id, RenderElementPresentationState, RenderElementState, RenderElementStates,
    },
    desktop::{
        Space, Window, layer_map_for_output,
        utils::{
            OutputPresentationFeedback, send_frames_surface_tree,
            surface_presentation_feedback_flags_from_states, surface_primary_scanout_output,
            take_presentation_feedback_surface_tree, update_surface_primary_scanout_output,
            with_surfaces_surface_tree,
        },
    },
    output::Output,
    reexports::wayland_server::{Client, Resource, backend::ClientId},
    utils::{Monotonic, Time},
    wayland::{
        commit_timing::CommitTimerBarrierStateUserData,
        compositor::{CompositorHandler, SurfaceAttributes, SurfaceData},
        fifo::FifoBarrierCachedState,
        fractional_scale::with_fractional_scale,
        presentation::PresentationFeedbackCachedState,
        session_lock::LockSurface,
    },
};
use tracing::info;

use crate::{backend::window::layer_surface_is_mapped, state::ShojiWM};

const PRIMARY_OUTPUT_KEEP_WITHIN_PERCENT: u128 = 110;

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

fn prefer_next_primary_scanout_compare<'a>(
    _current_output: &'a smithay::output::Output,
    _current_state: &RenderElementState,
    next_output: &'a smithay::output::Output,
    _next_state: &RenderElementState,
) -> &'a smithay::output::Output {
    next_output
}

fn stable_primary_output_for_window(
    space: &Space<Window>,
    window: &Window,
    visible_outputs: Option<&[String]>,
) -> Option<Output> {
    let rect = space.element_bbox(window)?;

    // Filter candidate outputs by `visibleOutputs` when the window declared
    // one. Workspace horizontal scroll positions tile windows far outside the
    // visible viewport — and that scrolled-out bbox happily intersects an
    // adjacent monitor's logical area. Without this filter the primary
    // scanout output flips to the unrelated monitor mid-scroll, which then
    // pushes the other monitor's scale to the client via
    // `wp_fractional_scale.preferred_scale`. The xdg client reconfigures,
    // SSD relayouts, buffers re-raster — visible frame drops even on a
    // gaming-class GPU. Pinning candidacy to the visibleOutputs set keeps
    // the primary anchored to the workspace's home monitor regardless of
    // where the scrolled-off rect happens to land.
    let candidate_outputs: Vec<&Output> = if let Some(allowed) = visible_outputs {
        let filtered: Vec<&Output> = space
            .outputs()
            .filter(|output| allowed.iter().any(|name| name == &output.name()))
            .collect();
        if filtered.is_empty() {
            // visibleOutputs referenced names that no longer match any
            // connected output (eg. monitor unplugged). Fall through to
            // the full set rather than dropping primary assignment entirely.
            space.outputs().collect()
        } else {
            filtered
        }
    } else {
        space.outputs().collect()
    };

    let mut overlaps = Vec::new();
    for output in &candidate_outputs {
        let Some(geometry) = space.output_geometry(output) else {
            continue;
        };
        let area = geometry
            .intersection(rect)
            .map(|overlap| i64::from(overlap.size.w) * i64::from(overlap.size.h))
            .unwrap_or(0);
        if area > 0 {
            overlaps.push(((*output).clone(), area as u128));
        }
    }

    if overlaps.is_empty() {
        // Window's bbox doesn't intersect any candidate output (scrolled
        // entirely off-screen of its home monitor). Pick the first
        // candidate as a deterministic fallback so we keep emitting
        // preferred_scale = home-monitor scale rather than dropping the
        // primary assignment and letting smithay's default selection take
        // over.
        return candidate_outputs.first().map(|output| (*output).clone());
    }

    stable_primary_output(&overlaps, window_primary_output(window).as_ref())
}

fn window_primary_output(window: &Window) -> Option<Output> {
    let mut current = None;
    window.with_surfaces(|surface, states| {
        if current.is_none() {
            current = surface_primary_scanout_output(surface, states);
        }
    });
    current
}

fn stable_primary_output(
    candidates: &[(Output, u128)],
    current: Option<&Output>,
) -> Option<Output> {
    let (best_output, best_area) = candidates
        .iter()
        .filter(|(_, area)| *area > 0)
        .max_by(|(a, aa), (b, ba)| aa.cmp(ba).then_with(|| b.name().cmp(&a.name())))?;
    if let Some(current) = current {
        let current_area = candidates
            .iter()
            .find_map(|(output, area)| (output == current).then_some(*area))
            .unwrap_or(0);
        // Fractional-scale and damage rounding can feed small area changes back
        // into the client's preferred scale. Share the native 10% hold threshold.
        if current_area > 0
            && current_area.saturating_mul(PRIMARY_OUTPUT_KEEP_WITHIN_PERCENT)
                >= best_area.saturating_mul(100)
        {
            return Some(current.clone());
        }
    }
    Some(best_output.clone())
}

fn window_had_presented_surface(
    window: &Window,
    render_element_states: &RenderElementStates,
) -> bool {
    let mut presented = false;
    window.with_surfaces(|surface, _| {
        if render_element_states.element_was_presented(Id::from_wayland_resource(surface)) {
            presented = true;
        }
    });
    presented
}

fn synthetic_presented_states_for_window(window: &Window) -> RenderElementStates {
    let mut states = RenderElementStates::default();
    window.with_surfaces(|surface, _| {
        states.states.insert(
            Id::from_wayland_resource(surface),
            RenderElementState {
                visible_area: usize::MAX,
                presentation_state: RenderElementPresentationState::Rendering { reason: None },
                needs_capture: false,
            },
        );
    });
    states
}

fn frame_callback_debug_enabled() -> bool {
    crate::env_flag!("SHOJI_FRAME_CALLBACK_DEBUG")
}

fn frame_liveness_debug_enabled() -> bool {
    std::env::var_os("SHOJI_FRAME_LIVENESS_DEBUG")
        .is_some_and(|value| value != "0" && !value.is_empty())
}

fn frame_throttle_debug_enabled() -> bool {
    crate::env_flag!("SHOJI_FRAME_THROTTLE_DEBUG")
}

fn scale_notify_debug_enabled() -> bool {
    crate::env_flag!("SHOJI_SCALE_NOTIFY_DEBUG")
}

fn fifo_debug_enabled() -> bool {
    crate::env_flag!("SHOJI_FIFO_DEBUG")
}

fn mpv_frame_debug_enabled() -> bool {
    std::env::var_os("SHOJI_MPV_FRAME_DEBUG").is_some_and(|value| value != "0" && !value.is_empty())
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

/// Marks every surface of `window` as presented on `output`.
///
/// Windows composited through an offscreen texture (a full-window transform snapshot, a
/// replacement effect) have no surface element in the output's render states, so
/// [`update_primary_scanout_output`] clears their primary output and frame callbacks fall back
/// to the throttled rate: a scaled window kept on screen updated at about 1 fps.
pub fn restore_primary_scanout_for_offscreen_window(window: &Window, output: &Output) {
    use smithay::backend::renderer::element::{
        Id, RenderElementPresentationState, RenderElementState, RenderElementStates,
    };
    use smithay::desktop::utils::update_surface_primary_scanout_output;

    let mut synthetic_states = RenderElementStates::default();
    window.with_surfaces(|surface, _| {
        synthetic_states.states.insert(
            Id::from_wayland_resource(surface),
            // usize::MAX so `area_primary_scanout_compare` always picks this output, whatever
            // area the previous primary output had stored.
            RenderElementState {
                visible_area: usize::MAX,
                presentation_state: RenderElementPresentationState::Rendering { reason: None },
                needs_capture: false,
            },
        );
    });
    window.with_surfaces(|surface, states| {
        update_surface_primary_scanout_output(
            surface,
            output,
            states,
            None,
            &synthetic_states,
            area_primary_scanout_compare,
        );
    });
}

fn composition_primary_output(
    outputs: impl Iterator<Item = Output>,
    targets: &HashMap<String, crate::backend::composition::CompositionTargets>,
    ids: &[Id],
    current: Option<&Output>,
) -> Option<Output> {
    let mut composited = false;
    let candidates: Vec<_> = outputs
        .filter_map(|output| {
            let target = targets.get(&output.name())?;
            composited |= ids
                .iter()
                .any(|id| target.presented_sources().element_was_presented(id.clone()));
            let area = ids
                .iter()
                .filter_map(|id| {
                    target
                        .effective_output_states()
                        .element_render_state(id.clone())
                })
                .filter(|state| state.presentation_state != RenderElementPresentationState::Skipped)
                .map(|state| state.visible_area as u128)
                .max()
                .unwrap_or(0);
            (area > 0).then_some((output, area))
        })
        .collect();
    composited
        .then(|| stable_primary_output(&candidates, current))
        .flatten()
}

pub struct WindowPrimaryOutputContext<'a> {
    pub composition_targets: &'a HashMap<String, crate::backend::composition::CompositionTargets>,
    pub snapshot_window_ids: Option<&'a HashSet<String>>,
    pub restore_presented: bool,
    pub restore_replacements: bool,
}

fn update_native_window_primary(
    space: &Space<Window>,
    window: &Window,
    output: &Output,
    render_element_states: &RenderElementStates,
    window_decorations: &HashMap<Window, crate::ssd::WindowDecorationState>,
    throttle_debug: bool,
) {
    let visible_outputs = window_decorations
        .get(window)
        .and_then(|decoration| decoration.managed_window.visible_outputs.clone());
    let Some(selected_primary_output) =
        stable_primary_output_for_window(space, window, visible_outputs.as_deref())
    else {
        return;
    };

    if &selected_primary_output != output {
        if !window_had_presented_surface(window, render_element_states) {
            // A preview can have made this output primary despite the native
            // window coordinates. Clear that assignment when the preview leaves.
            window.with_surfaces(|surface, states| {
                update_surface_primary_scanout_output(
                    surface,
                    output,
                    states,
                    None,
                    render_element_states,
                    prefer_next_primary_scanout_compare,
                );
            });
            return;
        }

        // The current output may repaint before the selected primary output. If the window was
        // actually presented on this frame, update the primary immediately so scale and frame
        // callbacks do not bounce through the stale output until the other monitor repaints.
        let synthetic_states = synthetic_presented_states_for_window(window);
        window.with_surfaces(|surface, states| {
            update_surface_primary_scanout_output(
                surface,
                &selected_primary_output,
                states,
                None,
                &synthetic_states,
                prefer_next_primary_scanout_compare,
            );
        });
        return;
    }

    window.with_surfaces(|surface, states| {
        if throttle_debug {
            use smithay::backend::renderer::element::Id;
            let element_id = Id::from_wayland_resource(surface);
            let was_presented = render_element_states.element_was_presented(element_id);
            let current_primary = surface_primary_scanout_output(surface, states);
            info!(
                surface = ?surface.id(),
                output = %output.name(),
                was_presented,
                current_primary = ?current_primary.as_ref().map(|o| o.name()),
                "update_primary_scanout_output: surface check",
            );
        }
        update_surface_primary_scanout_output(
            surface,
            output,
            states,
            None,
            render_element_states,
            prefer_next_primary_scanout_compare,
        );
    });
}

pub fn update_primary_scanout_output(
    space: &Space<Window>,
    output: &Output,
    cursor_status: &smithay::input::pointer::CursorImageStatus,
    session_lock_surface: Option<&LockSurface>,
    render_element_states: &RenderElementStates,
    window_decorations: &HashMap<Window, crate::ssd::WindowDecorationState>,
    window_context: &WindowPrimaryOutputContext<'_>,
) {
    // Keep smithay's primary-scanout bookkeeping in sync with the surfaces we actually rendered.
    //
    // This turned out to matter for Chrome on the TTY backend: without updating the primary
    // scanout output before collecting presentation feedback, Chrome would often behave as if the
    // output cadence was only ~60 Hz even when the monitor was actually running at 66 Hz.
    let throttle_debug = frame_throttle_debug_enabled();
    for window in space.elements() {
        let mut ids = Vec::new();
        window.with_surfaces(|surface, _| ids.push(Id::from_wayland_resource(surface)));
        // Native fallbacks also assign primaries. Resolve the preview against the
        // previous frame's primary before any such assignment can erase hysteresis.
        if let Some(selected) = composition_primary_output(
            space.outputs().cloned(),
            window_context.composition_targets,
            &ids,
            window_primary_output(window).as_ref(),
        ) {
            let states = synthetic_presented_states_for_window(window);
            window.with_surfaces(|surface, data| {
                update_surface_primary_scanout_output(
                    surface,
                    &selected,
                    data,
                    None,
                    &states,
                    prefer_next_primary_scanout_compare,
                );
            });
            continue;
        }

        update_native_window_primary(
            space,
            window,
            output,
            render_element_states,
            window_decorations,
            throttle_debug,
        );
        let offscreen = window_decorations.get(window).is_some_and(|decoration| {
            window_context
                .snapshot_window_ids
                .is_some_and(|ids| ids.contains(&decoration.snapshot.id))
                || (window_context.restore_replacements
                    && decoration
                        .window_effects
                        .as_ref()
                        .and_then(|effects| effects.replace.as_ref())
                        .is_some())
        });
        if space.outputs_for_element(window).contains(output)
            && (offscreen
                || (window_context.restore_presented
                    && window_had_presented_surface(window, render_element_states)))
        {
            restore_primary_scanout_for_offscreen_window(window, output);
        }
    }

    let map = layer_map_for_output(output);
    for layer_surface in map.layers().filter(|layer| layer_surface_is_mapped(layer)) {
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

    if let Some(lock_surface) = session_lock_surface {
        with_surfaces_surface_tree(lock_surface.wl_surface(), |surface, states| {
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
    session_lock_surface: Option<&LockSurface>,
    render_element_states: &RenderElementStates,
) -> OutputPresentationFeedback {
    let mut output_presentation_feedback = OutputPresentationFeedback::new(output);

    space.elements().for_each(|window| {
        if space.outputs_for_element(window).contains(output)
            || window_had_presented_surface(window, render_element_states)
        {
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
    for layer_surface in map.layers().filter(|layer| layer_surface_is_mapped(layer)) {
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

    if let Some(lock_surface) = session_lock_surface {
        take_presentation_feedback_surface_tree(
            lock_surface.wl_surface(),
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
    fn with_session_lock_surfaces_for_output<F>(&self, output: &Output, mut f: F)
    where
        F: FnMut(
            &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
            &SurfaceData,
        ),
    {
        if let Some(lock_surface) = self.session_lock_surface_for_output(output) {
            with_surfaces_surface_tree(lock_surface.wl_surface(), |surface, states| {
                f(surface, states);
            });
        }
    }

    fn window_frame_processing_applies_to_output(&self, window: &Window, output: &Output) -> bool {
        if self
            .composition_targets
            .get(&output.name())
            .is_some_and(|targets| {
                window_had_presented_surface(window, targets.presented_sources())
            })
        {
            return true;
        }
        // A custom composition drawing the window by id (into a render
        // texture, say) shows it even while the config hides it.
        if let Some(ids) = self.composition_windows.get(&output.name())
            && self
                .window_decorations
                .get(window)
                .is_some_and(|decoration| ids.contains(&decoration.snapshot.id))
        {
            return true;
        }
        if !self.window_allows_render(window) {
            return false;
        }

        if let Some(decoration) = self.window_decorations.get(window)
            && decoration.managed_window.managed
        {
            return decoration.managed_window_allows_render_on_output(output.name().as_str());
        }

        self.space.outputs_for_element(window).contains(output)
    }

    fn log_mpv_pending_surface_callbacks(
        &self,
        output: &Output,
        time: Duration,
        frame_callback_sequence: Option<u32>,
        label: &'static str,
    ) {
        if !mpv_frame_debug_enabled() {
            return;
        }

        self.space.elements().for_each(|window| {
            let Some(decoration) = self.window_decorations.get(window) else {
                return;
            };
            if decoration.snapshot.app_id.as_deref() != Some("mpv")
                || !self.window_frame_processing_applies_to_output(window, output)
            {
                return;
            }

            window.with_surfaces(|surface, states| {
                let pending_frame_callbacks = states
                    .cached_state
                    .get::<SurfaceAttributes>()
                    .current()
                    .frame_callbacks
                    .len();
                let pending_presentation_feedbacks = states
                    .cached_state
                    .get::<PresentationFeedbackCachedState>()
                    .current()
                    .callbacks
                    .len();
                let primary = surface_primary_scanout_output(surface, states);
                info!(
                    output = %output.name(),
                    surface = ?surface.id(),
                    pending_frame_callbacks,
                    pending_presentation_feedbacks,
                    primary = ?primary.as_ref().map(|output| output.name()),
                    callback_time_ms = time.as_secs_f64() * 1000.0,
                    sequence = ?frame_callback_sequence,
                    label,
                    "mpv frame debug: pending surface callbacks before send"
                );
            });
        });
    }

    pub fn send_primary_frame_callbacks_for_output(
        &mut self,
        output: &Output,
        time: Duration,
        frame_callback_sequence: Option<u32>,
    ) {
        let throttle = Some(Duration::from_secs(1));
        let debug = frame_callback_debug_enabled() || frame_liveness_debug_enabled();
        let callback_count = std::cell::Cell::new(0usize);
        self.log_mpv_pending_surface_callbacks(
            output,
            time,
            frame_callback_sequence,
            "primary-only",
        );

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
            if self.window_frame_processing_applies_to_output(window, output) {
                window.send_frame(output, time, throttle, should_send);
            }
        });

        let map = layer_map_for_output(output);
        for layer_surface in map.layers().filter(|layer| layer_surface_is_mapped(layer)) {
            layer_surface.send_frame(output, time, throttle, should_send);
        }
        drop(map);

        if let Some(lock_surface) = self.session_lock_surface_for_output(output) {
            send_frames_surface_tree(
                lock_surface.wl_surface(),
                output,
                time,
                throttle,
                &should_send,
            );
        }

        if let smithay::input::pointer::CursorImageStatus::Surface(surface) = &self.cursor_status {
            send_frames_surface_tree(surface, output, time, throttle, &should_send);
        }

        if debug {
            info!(
                output = %output.name(),
                surface_count = callback_count.get(),
                sequence = ?frame_callback_sequence,
                "primary-only frame callbacks sent"
            );
        }
    }

    pub fn send_frame_callbacks_for_output(
        &mut self,
        output: &Output,
        time: Duration,
        frame_callback_sequence: Option<u32>,
    ) {
        // Throttle frame callbacks for surfaces that are not on their primary scanout output.
        // This limits idle clients (e.g. Firefox, whose root surface has no buffer and thus
        // no render element) to ~1 callback/second, matching anvil's behaviour.
        let throttle = Some(Duration::from_secs(1));
        let debug = frame_callback_debug_enabled();
        let throttle_debug = frame_throttle_debug_enabled();
        let callback_count = std::cell::Cell::new(0usize);
        self.log_mpv_pending_surface_callbacks(output, time, frame_callback_sequence, "all");

        let should_send =
            |surface: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
             states: &smithay::wayland::compositor::SurfaceData| {
                let current_primary_output = surface_primary_scanout_output(surface, states);
                if throttle_debug {
                    info!(
                        surface = ?surface.id(),
                        primary = ?current_primary_output.as_ref().map(|o| o.name()),
                        target_output = %output.name(),
                        "send_frame: surface primary check",
                    );
                }
                if current_primary_output.as_ref() != Some(output) {
                    // primary is None or different output — Smithay throttle will decide
                    if throttle_debug {
                        info!(
                            surface = ?surface.id(),
                            primary = ?current_primary_output.as_ref().map(|o| o.name()),
                            "send_frame: no primary → throttle path (should_send=None)",
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
            if self.window_frame_processing_applies_to_output(window, output) {
                window.send_frame(output, time, throttle, should_send);
            }
        });

        let map = layer_map_for_output(output);
        for layer_surface in map.layers().filter(|layer| layer_surface_is_mapped(layer)) {
            layer_surface.send_frame(output, time, throttle, should_send);
        }
        drop(map);

        if let Some(lock_surface) = self.session_lock_surface_for_output(output) {
            send_frames_surface_tree(
                lock_surface.wl_surface(),
                output,
                time,
                throttle,
                &should_send,
            );
        }

        // Cursor surfaces (e.g. Xwayland cursor surfaces forwarded by xwayland-satellite)
        // also need frame callbacks so the client can commit subsequent cursor buffers — without
        // these, set_cursor calls following the first one stall and the cursor type stops
        // updating.
        if let smithay::input::pointer::CursorImageStatus::Surface(surface) = &self.cursor_status {
            send_frames_surface_tree(surface, output, time, throttle, &should_send);
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

    pub fn signal_commit_timing_barriers_for_output(
        &mut self,
        output: &Output,
        frame_target: Time<Monotonic>,
    ) -> bool {
        #[allow(clippy::mutable_key_type)]
        let mut clients: HashMap<ClientId, Client> = HashMap::new();

        let debug_fifo = fifo_debug_enabled();
        let debug_mpv = mpv_frame_debug_enabled();
        self.space.elements().for_each(|window| {
            if !self.window_frame_processing_applies_to_output(window, output) {
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
            let app_id_is_mpv = app_id.as_deref() == Some("mpv");
            window.with_surfaces(|surface, states| {
                let commit_timer_signaled = states
                    .data_map
                    .get::<CommitTimerBarrierStateUserData>()
                    .map(|commit_timer| {
                        let mut commit_timer_state = commit_timer.lock().unwrap();
                        commit_timer_state.signal_until(frame_target)
                    });
                if debug_mpv && app_id_is_mpv {
                    info!(
                        surface = ?surface.id(),
                        output = %output.name(),
                        has_commit_timer = commit_timer_signaled.is_some(),
                        commit_timer_signaled,
                        frame_target_ms = Duration::from(frame_target).as_secs_f64() * 1000.0,
                        "mpv frame debug: pre_repaint commit timer"
                    );
                }
                if commit_timer_signaled == Some(true) {
                    if debug_fifo || (debug_mpv && app_id_is_mpv) {
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
        for layer_surface in map.layers().filter(|layer| layer_surface_is_mapped(layer)) {
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
        let signaled = !clients.is_empty();
        for client in clients.into_values() {
            self.client_compositor_state(&client)
                .blocker_cleared(self, &dh);
        }
        signaled
    }

    pub fn pre_repaint(&mut self, output: &Output, frame_target: Time<Monotonic>) {
        self.signal_commit_timing_barriers_for_output(output, frame_target);
    }

    pub fn next_commit_timing_deadline_for_output(&self, output: &Output) -> Option<Duration> {
        let mut next_deadline: Option<Duration> = None;

        self.space.elements().for_each(|window| {
            if !self.window_frame_processing_applies_to_output(window, output) {
                return;
            }

            window.with_surfaces(|_, states| {
                let deadline = states
                    .data_map
                    .get::<CommitTimerBarrierStateUserData>()
                    .and_then(|commit_timer| {
                        let commit_timer_state = commit_timer.lock().unwrap();
                        commit_timer_state.next_deadline()
                    })
                    .map(|deadline| {
                        let deadline: Time<Monotonic> = deadline.into();
                        Duration::from(deadline)
                    });
                if let Some(deadline) = deadline {
                    next_deadline = Some(match next_deadline {
                        Some(current) => current.min(deadline),
                        None => deadline,
                    });
                }
            });
        });

        let map = layer_map_for_output(output);
        for layer_surface in map.layers().filter(|layer| layer_surface_is_mapped(layer)) {
            layer_surface.with_surfaces(|_, states| {
                let deadline = states
                    .data_map
                    .get::<CommitTimerBarrierStateUserData>()
                    .and_then(|commit_timer| {
                        let commit_timer_state = commit_timer.lock().unwrap();
                        commit_timer_state.next_deadline()
                    })
                    .map(|deadline| {
                        let deadline: Time<Monotonic> = deadline.into();
                        Duration::from(deadline)
                    });
                if let Some(deadline) = deadline {
                    next_deadline = Some(match next_deadline {
                        Some(current) => current.min(deadline),
                        None => deadline,
                    });
                }
            });
        }
        drop(map);

        self.with_session_lock_surfaces_for_output(output, |_, states| {
            let deadline = states
                .data_map
                .get::<CommitTimerBarrierStateUserData>()
                .and_then(|commit_timer| {
                    let commit_timer_state = commit_timer.lock().unwrap();
                    commit_timer_state.next_deadline()
                })
                .map(|deadline| {
                    let deadline: Time<Monotonic> = deadline.into();
                    Duration::from(deadline)
                });
            if let Some(deadline) = deadline {
                next_deadline = Some(match next_deadline {
                    Some(current) => current.min(deadline),
                    None => deadline,
                });
            }
        });

        next_deadline
    }

    pub fn signal_post_repaint_barriers(&mut self, output: &Output) {
        #[allow(clippy::mutable_key_type)]
        let mut clients: HashMap<ClientId, Client> = HashMap::new();

        let debug_scale = scale_notify_debug_enabled();
        let debug_fifo = fifo_debug_enabled();
        let debug_mpv = mpv_frame_debug_enabled();
        self.space.elements().for_each(|window| {
            if self.window_frame_processing_applies_to_output(window, output) {
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
                let app_id_is_mpv = app_id.as_deref() == Some("mpv");
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
                    if debug_mpv && app_id_is_mpv {
                        info!(
                            surface = ?surface.id(),
                            output = %output.name(),
                            primary = ?primary_scanout_output.as_ref().map(|output| output.name()),
                            has_fifo_barrier = fifo_barrier.is_some(),
                            "mpv frame debug: post_repaint fifo barrier"
                        );
                    }
                    if let Some(fifo_barrier) = fifo_barrier {
                        if debug_fifo || (debug_mpv && app_id_is_mpv) {
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
        for layer_surface in map.layers().filter(|layer| layer_surface_is_mapped(layer)) {
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

        self.with_session_lock_surfaces_for_output(output, |surface, states| {
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
                                "preferred scale changed for session lock surface"
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
                if let Some(client) = surface.client() {
                    clients.insert(client.id(), client);
                }
            }
        });

        // Update fractional scale on cursor surfaces too. This matters for Xwayland-via-
        // satellite, which sizes its cursor buffers based on the preferred scale; without
        // these notifications it never learns that the cursor moved to a different-scale
        // output and renders cursors at the wrong size (most visibly: huge near monitor edges
        // and after crossing scale boundaries).
        if let smithay::input::pointer::CursorImageStatus::Surface(cursor_surface) =
            &self.cursor_status
        {
            with_surfaces_surface_tree(cursor_surface, |surface, states| {
                let primary_scanout_output = surface_primary_scanout_output(surface, states);
                if let Some(scanout) = primary_scanout_output.as_ref() {
                    let scale = scanout.current_scale().fractional_scale();
                    with_fractional_scale(states, |fractional_scale| {
                        if debug_scale {
                            let protocol_id = surface.id().protocol_id();
                            let prev = previous_preferred_scale(protocol_id, scale);
                            if prev != Some(scale) {
                                info!(
                                    surface = ?surface.id(),
                                    output = %scanout.name(),
                                    prev_scale = ?prev,
                                    new_scale = scale,
                                    "preferred scale changed for cursor surface"
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
                    if let Some(client) = surface.client() {
                        clients.insert(client.id(), client);
                    }
                }
            });
        }

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

#[cfg(test)]
mod composition_tests {
    use super::*;
    use crate::backend::composition::CompositionTargets;

    fn output(name: &str) -> Output {
        Output::new(
            name.into(),
            smithay::output::PhysicalProperties {
                size: (0, 0).into(),
                subpixel: smithay::output::Subpixel::Unknown,
                make: "test".into(),
                model: "test".into(),
                serial_number: String::new(),
            },
        )
    }

    fn states(id: &Id, area: usize) -> RenderElementStates {
        RenderElementStates {
            states: HashMap::from([(
                id.clone(),
                RenderElementState {
                    visible_area: area,
                    presentation_state: RenderElementPresentationState::Rendering { reason: None },
                    needs_capture: false,
                },
            )]),
        }
    }

    fn record(target: &mut CompositionTargets, surface: &Id, area: usize, preview: bool) {
        target.update_presentation(
            if preview {
                states(surface, area)
            } else {
                Default::default()
            },
            states(surface, area),
        );
    }

    #[test]
    fn preview_uses_its_output_without_stealing_a_larger_native_view() {
        let (a, b) = (output("native"), output("preview"));
        let surface = Id::new();
        let ids = [surface.clone()];
        let mut targets = HashMap::from([
            (a.name(), CompositionTargets::default()),
            (b.name(), CompositionTargets::default()),
        ]);
        record(targets.get_mut(&b.name()).unwrap(), &surface, 100, true);
        assert_eq!(
            composition_primary_output([a.clone(), b.clone()].into_iter(), &targets, &ids, None),
            Some(b.clone())
        );
        record(targets.get_mut(&a.name()).unwrap(), &surface, 10000, false);
        assert_eq!(
            composition_primary_output(
                [a.clone(), b.clone()].into_iter(),
                &targets,
                &ids,
                Some(&b)
            ),
            Some(a.clone())
        );
        assert_eq!(
            composition_primary_output(
                [b.clone(), a.clone()].into_iter(),
                &targets,
                &ids,
                Some(&b)
            ),
            Some(a.clone())
        );
        targets.get_mut(&a.name()).unwrap().clear_presentation();
        assert_eq!(
            composition_primary_output(
                [a.clone(), b.clone()].into_iter(),
                &targets,
                &ids,
                Some(&a)
            ),
            Some(b.clone())
        );
        assert_eq!(
            composition_primary_output([a.clone()].into_iter(), &targets, &ids, Some(&b)),
            None,
            "removed output must not remain primary"
        );
        targets.get_mut(&b.name()).unwrap().clear_presentation();
        assert_eq!(
            composition_primary_output([a, b.clone()].into_iter(), &targets, &ids, Some(&b)),
            None,
            "hidden preview must release its primary override"
        );
    }

    #[test]
    fn primary_keeps_the_existing_native_ten_percent_threshold() {
        let (a, b) = (output("a"), output("b"));
        assert_eq!(
            stable_primary_output(&[(a.clone(), 100), (b.clone(), 110)], Some(&a)),
            Some(a.clone())
        );
        assert_eq!(
            stable_primary_output(&[(a.clone(), 100), (b.clone(), 111)], Some(&a)),
            Some(b.clone())
        );
        assert_eq!(
            stable_primary_output(&[(a.clone(), 0), (b.clone(), 1)], Some(&a)),
            Some(b.clone())
        );
        assert_eq!(
            stable_primary_output(&[(b.clone(), 1)], Some(&a)),
            Some(b.clone())
        );
        assert_eq!(stable_primary_output(&[], Some(&a)), None);
        assert_eq!(
            stable_primary_output(&[(b.clone(), 100), (a.clone(), 100)], None),
            Some(a.clone())
        );
        assert_eq!(
            stable_primary_output(&[(a.clone(), 100), (b.clone(), 100)], Some(&b)),
            Some(b.clone())
        );
        assert_eq!(
            stable_primary_output(
                &[(a.clone(), usize::MAX as u128 - 1), (b, usize::MAX as u128)],
                Some(&a)
            ),
            Some(a)
        );
    }

    #[test]
    fn preview_primary_survives_small_changes_in_either_output_frame_order() {
        let (a, b) = (output("native"), output("preview"));
        let surface = Id::new();
        for order in [[&a, &b], [&b, &a]] {
            let mut targets = HashMap::from([
                (a.name(), CompositionTargets::default()),
                (b.name(), CompositionTargets::default()),
            ]);
            record(targets.get_mut(&a.name()).unwrap(), &surface, 10000, false);
            record(targets.get_mut(&b.name()).unwrap(), &surface, 10000, true);
            // Start on the preview, so a native fallback cannot silently reset
            // the previous choice and make this pass only for the native output.
            let mut primary = Some(b.clone());
            for (aa, ba) in [
                (10001, 10000),
                (10000, 10001),
                (10999, 10000),
                (10000, 10999),
            ] {
                for frame_output in order {
                    let is_preview = frame_output == &b;
                    record(
                        targets.get_mut(&frame_output.name()).unwrap(),
                        &surface,
                        if is_preview { ba } else { aa },
                        is_preview,
                    );
                    primary = composition_primary_output(
                        order.into_iter().cloned(),
                        &targets,
                        std::slice::from_ref(&surface),
                        primary.as_ref(),
                    );
                    assert_eq!(primary, Some(b.clone()));
                }
            }
            // A cached/no-damage report must retain the same selection.
            assert_eq!(
                composition_primary_output(
                    order.into_iter().cloned(),
                    &targets,
                    std::slice::from_ref(&surface),
                    primary.as_ref()
                ),
                Some(b.clone())
            );
            record(targets.get_mut(&a.name()).unwrap(), &surface, 12100, false);
            primary = composition_primary_output(
                order.into_iter().cloned(),
                &targets,
                std::slice::from_ref(&surface),
                primary.as_ref(),
            );
            assert_eq!(
                primary,
                Some(a.clone()),
                "a gain greater than 10% must switch"
            );
            targets.get_mut(&a.name()).unwrap().clear_presentation();
            assert_eq!(
                composition_primary_output(
                    order.into_iter().cloned(),
                    &targets,
                    std::slice::from_ref(&surface),
                    primary.as_ref()
                ),
                Some(b.clone()),
                "an invisible primary must not be held by hysteresis"
            );
            assert_eq!(
                composition_primary_output(
                    [b.clone()].into_iter(),
                    &targets,
                    std::slice::from_ref(&surface),
                    primary.as_ref()
                ),
                Some(b.clone()),
                "a removed primary must not be held by hysteresis"
            );
        }
    }

    #[test]
    fn replacing_and_clearing_presentation_cannot_retain_old_source_eligibility() {
        let (old, new) = (Id::new(), Id::new());
        let mut target = CompositionTargets::default();
        record(&mut target, &old, 100, true);
        assert!(
            target
                .presented_sources()
                .element_was_presented(old.clone())
        );
        record(&mut target, &new, 100, false);
        assert!(target.presented_sources().states.is_empty());
        assert!(!target.effective_output_states().element_was_presented(old));
        assert!(target.effective_output_states().element_was_presented(new));
        target.clear_presentation();
        assert!(target.presented_sources().states.is_empty());
        assert!(target.effective_output_states().states.is_empty());
    }

    #[test]
    fn common_update_preserves_preview_primary_before_native_fallbacks_in_both_frame_orders() {
        let client = crate::presentation_test_client::TestClient::new();
        let window = client.window();
        let surface = Id::from_wayland_resource(window.toplevel().unwrap().wl_surface());
        let (a, b) = (output("native"), output("preview"));
        let mut space = Space::default();
        for (output, x) in [(&a, 0), (&b, 200)] {
            output.change_current_state(
                Some(smithay::output::Mode {
                    size: (100, 100).into(),
                    refresh: 60000,
                }),
                None,
                None,
                None,
            );
            space.map_output(output, (x, 0));
        }
        space.map_element(window.clone(), (0, 0), false);
        space.refresh();
        assert_eq!(space.outputs_for_element(&window), vec![a.clone()]);
        assert_eq!(
            stable_primary_output_for_window(&space, &window, None),
            Some(a.clone())
        );

        for order in [[&a, &b], [&b, &a]] {
            let mut targets = HashMap::from([
                (a.name(), CompositionTargets::default()),
                (b.name(), CompositionTargets::default()),
            ]);
            record(targets.get_mut(&a.name()).unwrap(), &surface, 10000, false);
            record(targets.get_mut(&b.name()).unwrap(), &surface, 10000, true);
            let initial_states = synthetic_presented_states_for_window(&window);
            window.with_surfaces(|surface, data| {
                update_surface_primary_scanout_output(
                    surface,
                    &b,
                    data,
                    None,
                    &initial_states,
                    prefer_next_primary_scanout_compare,
                );
            });
            assert_eq!(window_primary_output(&window), Some(b.clone()));
            for (native_area, preview_area) in [
                (10001, 10000),
                (10000, 10001),
                (11000, 10000),
                (10000, 11000),
            ] {
                for output in order {
                    record(
                        targets.get_mut(&output.name()).unwrap(),
                        &surface,
                        if output == &a {
                            native_area
                        } else {
                            preview_area
                        },
                        output == &b,
                    );
                    update_primary_scanout_output(
                        &space,
                        output,
                        &smithay::input::pointer::CursorImageStatus::Hidden,
                        None,
                        targets[&output.name()].effective_output_states(),
                        &HashMap::new(),
                        &WindowPrimaryOutputContext {
                            composition_targets: &targets,
                            snapshot_window_ids: None,
                            restore_presented: true,
                            restore_replacements: true,
                        },
                    );
                    assert_eq!(window_primary_output(&window), Some(b.clone()));
                }
            }
            // An undamaged frame skips native restoration but still has to keep
            // the final primary before callbacks and feedback are collected.
            for output in order {
                update_primary_scanout_output(
                    &space,
                    output,
                    &smithay::input::pointer::CursorImageStatus::Hidden,
                    None,
                    targets[&output.name()].effective_output_states(),
                    &HashMap::new(),
                    &WindowPrimaryOutputContext {
                        composition_targets: &targets,
                        snapshot_window_ids: None,
                        restore_presented: false,
                        restore_replacements: false,
                    },
                );
                assert_eq!(window_primary_output(&window), Some(b.clone()));
            }
            targets.get_mut(&b.name()).unwrap().clear_presentation();
            update_primary_scanout_output(
                &space,
                &a,
                &smithay::input::pointer::CursorImageStatus::Hidden,
                None,
                targets[&a.name()].effective_output_states(),
                &HashMap::new(),
                &WindowPrimaryOutputContext {
                    composition_targets: &targets,
                    snapshot_window_ids: None,
                    restore_presented: true,
                    restore_replacements: true,
                },
            );
            assert_eq!(
                window_primary_output(&window),
                Some(a.clone()),
                "native primary must resume when the preview disappears"
            );
        }
    }
}
