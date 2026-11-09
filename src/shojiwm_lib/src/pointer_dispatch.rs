//! Paces the config runtime's share of pointer motion to the display.
//!
//! A gaming mouse reports 1000 times a second or more, and during a window drag
//! each report reached the config twice (`onWindowMove`, `onPointerMove`), each
//! a blocking round trip to the runtime thread. Measured at 130 Hz on a 1000 Hz
//! mouse: 0.57 ms per report, so over half of the main thread's time went to
//! waiting for the runtime, and a 10 ms render stretched to a 30 ms frame while
//! the backlog it left behind was worked off. The drag ran at 33 fps with the
//! CPU and the GPU both far from busy.
//!
//! Nothing can use more than one position per frame, so the runtime gets the
//! latest one instead of all of them. The rest of the motion is untouched:
//! clients and the cursor plane still see every report.
//!
//! A sample goes out at once if none went out for a refresh period, so slow
//! motion and low-rate devices are not delayed. Otherwise it waits for whichever
//! comes first:
//!
//!  * any other input event, so that the config sees events in their real order
//!    (the last position before a button press or release, for one);
//!  * a vblank or the start of a render, so that the frame shows the newest
//!    position;
//!  * a timer, for motion that does not lead to a render.

use std::time::{Duration, Instant};

use smithay::{
    reexports::calloop::timer::{TimeoutAction, Timer},
    utils::{Logical, Point},
};
use tracing::warn;

use crate::{
    ssd::{WindowMoveEventSnapshot, WindowResizeEventSnapshot},
    state::ShojiWM,
};

/// The latest motion of a window move or resize grab.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum PendingGrabMotion {
    WindowMove {
        window_id: String,
        event: WindowMoveEventSnapshot,
    },
    WindowResize {
        window_id: String,
        event: WindowResizeEventSnapshot,
    },
}

/// Pointer motion the config has not heard about: where it last heard from the
/// pointer and where the pointer is now, so the reported delta loses nothing.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct PendingPointerMove {
    pub origin: Point<f64, Logical>,
    pub position: Point<f64, Logical>,
    pub time_msec: u32,
}

#[derive(Debug, Default)]
pub struct PointerDispatchQueue {
    pending_grab: Option<PendingGrabMotion>,
    pending_move: Option<PendingPointerMove>,
    last_flush_at: Option<Instant>,
    timer_armed: bool,
}

impl PointerDispatchQueue {
    fn is_empty(&self) -> bool {
        self.pending_grab.is_none() && self.pending_move.is_none()
    }

    fn queue_move(
        &mut self,
        previous: Point<f64, Logical>,
        position: Point<f64, Logical>,
        time_msec: u32,
    ) {
        let origin = self.pending_move.map_or(previous, |pending| pending.origin);
        self.pending_move = Some(PendingPointerMove {
            origin,
            position,
            time_msec,
        });
    }

    /// How long a sample queued now has to wait. Zero when it is due.
    fn delay_until_due(&self, now: Instant, interval: Duration) -> Duration {
        self.last_flush_at.map_or(Duration::ZERO, |flushed_at| {
            interval.saturating_sub(now.saturating_duration_since(flushed_at))
        })
    }

    fn take(&mut self, now: Instant) -> (Option<PendingGrabMotion>, Option<PendingPointerMove>) {
        self.last_flush_at = Some(now);
        (self.pending_grab.take(), self.pending_move.take())
    }

    /// A grab that ended must not report motion after its last event.
    pub(crate) fn discard_grab_motion(&mut self) {
        self.pending_grab = None;
    }
}

impl ShojiWM {
    pub(crate) fn queue_pointer_move_dispatch(
        &mut self,
        previous: Point<f64, Logical>,
        position: Point<f64, Logical>,
        time_msec: u32,
    ) {
        if !self.runtime_pointer_move_enabled && !self.runtime_pointer_move_async_enabled {
            return;
        }
        self.pointer_dispatch
            .queue_move(previous, position, time_msec);
    }

    pub(crate) fn queue_grab_motion_dispatch(&mut self, motion: PendingGrabMotion) {
        self.pointer_dispatch.pending_grab = Some(motion);
        // `process_input_event` pumps once it is done with the event. Grabs also see
        // motion nothing pumps after, such as focus refreshes.
        if !self.user_input_in_flight {
            let delay = self.pointer_dispatch_delay();
            self.arm_pointer_dispatch_timer(delay);
        }
    }

    /// Send what is queued if it is due, and make sure it goes out later otherwise.
    pub(crate) fn pump_pointer_dispatch(&mut self) {
        if self.pointer_dispatch.is_empty() {
            return;
        }
        let delay = self.pointer_dispatch_delay();
        if delay.is_zero() {
            self.flush_pointer_dispatch();
        } else {
            self.arm_pointer_dispatch_timer(delay);
        }
    }

    /// Send what is queued now, whatever the pace.
    pub(crate) fn flush_pointer_dispatch(&mut self) {
        if self.pointer_dispatch.is_empty() {
            return;
        }
        timescope::scope!("pointer dispatch flush");
        let (grab, pointer_move) = self.pointer_dispatch.take(Instant::now());
        // The input behind this sample may have been handled a while ago, but a focus
        // decision the config makes from it is still the user's doing.
        let previous_in_flight = std::mem::replace(&mut self.user_input_in_flight, true);
        let now_ms = Duration::from(self.clock.now()).as_millis() as u64;
        match grab {
            Some(PendingGrabMotion::WindowMove { window_id, event }) => {
                self.invoke_window_move_event(&window_id, &event, now_ms);
            }
            Some(PendingGrabMotion::WindowResize { window_id, event }) => {
                self.invoke_window_resize_event(&window_id, &event, now_ms);
            }
            None => {}
        }
        if let Some(motion) = pointer_move {
            self.dispatch_pointer_move_events(motion.origin, motion.position, motion.time_msec);
        }
        self.user_input_in_flight = previous_in_flight;
        // The config can move the cursor, which queues motion of its own.
        self.pump_pointer_dispatch();
    }

    fn pointer_dispatch_delay(&self) -> Duration {
        let interval = Duration::from_millis(self.runtime_frame_sync_interval_ms());
        self.pointer_dispatch
            .delay_until_due(Instant::now(), interval)
    }

    fn arm_pointer_dispatch_timer(&mut self, delay: Duration) {
        if self.pointer_dispatch.timer_armed {
            return;
        }
        let armed = self
            .loop_handle
            .insert_source(Timer::from_duration(delay), |_, _, state| {
                state.record_event_source_wake("pointer-dispatch-timer");
                state.pointer_dispatch.timer_armed = false;
                state.pump_pointer_dispatch();
                TimeoutAction::Drop
            });
        match armed {
            Ok(_) => self.pointer_dispatch.timer_armed = true,
            Err(error) => {
                warn!(?error, "failed to arm the pointer dispatch timer");
                self.flush_pointer_dispatch();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const INTERVAL: Duration = Duration::from_millis(8);

    fn point(x: f64, y: f64) -> Point<f64, Logical> {
        Point::from((x, y))
    }

    #[test]
    fn the_first_sample_is_due_at_once() {
        let queue = PointerDispatchQueue::default();
        assert_eq!(
            queue.delay_until_due(Instant::now(), INTERVAL),
            Duration::ZERO
        );
    }

    #[test]
    fn samples_within_the_interval_wait_for_the_rest_of_it() {
        let mut queue = PointerDispatchQueue::default();
        let flushed_at = Instant::now();
        queue.take(flushed_at);

        assert_eq!(
            queue.delay_until_due(flushed_at + Duration::from_millis(3), INTERVAL),
            Duration::from_millis(5),
        );
        assert_eq!(
            queue.delay_until_due(flushed_at + INTERVAL, INTERVAL),
            Duration::ZERO,
        );
        assert_eq!(
            queue.delay_until_due(flushed_at + Duration::from_secs(1), INTERVAL),
            Duration::ZERO,
        );
    }

    #[test]
    fn coalesced_motion_keeps_the_whole_delta_and_the_latest_position() {
        let mut queue = PointerDispatchQueue::default();
        queue.queue_move(point(10.0, 10.0), point(11.0, 10.5), 100);
        queue.queue_move(point(11.0, 10.5), point(12.5, 9.0), 101);
        queue.queue_move(point(12.5, 9.0), point(13.0, 8.0), 102);

        let (_, motion) = queue.take(Instant::now());
        let motion = motion.expect("motion was queued");
        assert_eq!(motion.origin, point(10.0, 10.0));
        assert_eq!(motion.position, point(13.0, 8.0));
        assert_eq!(motion.time_msec, 102);
    }

    #[test]
    fn motion_after_a_flush_starts_from_where_the_flush_left_off() {
        let mut queue = PointerDispatchQueue::default();
        queue.queue_move(point(0.0, 0.0), point(5.0, 5.0), 1);
        queue.take(Instant::now());
        assert!(queue.is_empty());

        queue.queue_move(point(5.0, 5.0), point(6.0, 6.0), 2);
        let (_, motion) = queue.take(Instant::now());
        assert_eq!(motion.expect("motion was queued").origin, point(5.0, 5.0));
    }

    #[test]
    fn discarding_grab_motion_leaves_pointer_motion_alone() {
        let mut queue = PointerDispatchQueue::default();
        queue.pending_grab = Some(PendingGrabMotion::WindowResize {
            window_id: "1".to_owned(),
            event: test_resize_event(),
        });
        queue.queue_move(point(0.0, 0.0), point(1.0, 1.0), 1);

        queue.discard_grab_motion();

        let (grab, motion) = queue.take(Instant::now());
        assert!(grab.is_none());
        assert!(motion.is_some());
    }

    fn test_resize_event() -> WindowResizeEventSnapshot {
        use crate::ssd::{
            WindowPositionSnapshot, WindowResizeEdgesSnapshot, WindowResizePhaseSnapshot,
            WindowResizePointSnapshot, WindowResizeSourceSnapshot,
        };
        let point = || WindowResizePointSnapshot { x: 0.0, y: 0.0 };
        let rect = || WindowPositionSnapshot {
            x: 0.0,
            y: 0.0,
            width: 1.0,
            height: 1.0,
        };
        WindowResizeEventSnapshot {
            source: WindowResizeSourceSnapshot::Ssd,
            phase: WindowResizePhaseSnapshot::Update,
            edges: WindowResizeEdgesSnapshot {
                left: false,
                right: true,
                top: false,
                bottom: false,
            },
            start_pointer: point(),
            current_pointer: point(),
            delta: point(),
            start_rect: rect(),
            current_rect: rect(),
            output_name: None,
            timestamp: 0,
        }
    }
}
