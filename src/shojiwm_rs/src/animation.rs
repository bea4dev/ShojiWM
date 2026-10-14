//! Time: easing curves, animated values and timers.
//!
//! The compositor drives time, and time is kept per output: each output's
//! clock is the presentation time of its own frames. Work bound to an output
//! ([`create_poll`], [`Animation::on_output`]) steps once per frame of that
//! output, stamped with the frame's presentation time; the config tells the
//! compositor when that work next comes due (its schedule). Work bound to no
//! output ([`set_timeout`], [`set_interval`], plain [`Animation`]s) runs on
//! any tick.
//!
//! ```
//! use shojiwm_rs::prelude::*;
//!
//! let scope = Scope::root();
//! let open = scope.run(|| Animation::new(0.0));
//! open.start(AnimationOptions::to(1.0, 200.0).easing(Easing::EASE_OUT_CUBIC));
//! let scale = scope.memo(move || 0.8 + open.get() * 0.2);
//! # let _ = scale;
//! ```

use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    rc::Rc,
};

use shojiwm_lib::ssd::ManagedWindowAnimationEasingSnapshot;

use crate::reactive::{Memo, Prop, ReadSignal, Signal, on_cleanup, signal, untrack};

/// An easing curve mapping progress `0..=1` to eased progress.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Easing {
    Linear,
    /// CSS `cubic-bezier(x1, y1, x2, y2)`.
    CubicBezier(f64, f64, f64, f64),
}

impl Easing {
    pub const EASE: Self = Self::CubicBezier(0.25, 0.1, 0.25, 1.0);
    pub const EASE_IN: Self = Self::CubicBezier(0.42, 0.0, 1.0, 1.0);
    pub const EASE_OUT: Self = Self::CubicBezier(0.0, 0.0, 0.58, 1.0);
    pub const EASE_IN_OUT: Self = Self::CubicBezier(0.42, 0.0, 0.58, 1.0);
    pub const EASE_OUT_CUBIC: Self = Self::CubicBezier(0.22, 1.0, 0.36, 1.0);
    pub const EASE_IN_OUT_CUBIC: Self = Self::CubicBezier(0.65, 0.0, 0.35, 1.0);

    pub fn apply(self, progress: f64) -> f64 {
        let progress = clamp_unit(progress);
        match self {
            Self::Linear => progress,
            Self::CubicBezier(x1, y1, x2, y2) => {
                if progress == 0.0 || progress == 1.0 {
                    return progress;
                }
                cubic_bezier_y(x1, y1, x2, y2, progress)
            }
        }
    }
}

impl From<Easing> for ManagedWindowAnimationEasingSnapshot {
    fn from(easing: Easing) -> Self {
        match easing {
            Easing::Linear => Self::Linear,
            Easing::CubicBezier(x1, y1, x2, y2) => Self::CubicBezier { x1, y1, x2, y2 },
        }
    }
}

/// `cubicBezier(x1, y1, x2, y2)`.
pub const fn cubic_bezier(x1: f64, y1: f64, x2: f64, y2: f64) -> Easing {
    Easing::CubicBezier(x1, y1, x2, y2)
}

fn clamp_unit(value: f64) -> f64 {
    if !value.is_finite() {
        0.0
    } else {
        value.clamp(0.0, 1.0)
    }
}

fn cubic_bezier_y(x1: f64, y1: f64, x2: f64, y2: f64, x: f64) -> f64 {
    let cx = 3.0 * x1;
    let bx = 3.0 * (x2 - x1) - cx;
    let ax = 1.0 - cx - bx;
    let cy = 3.0 * y1;
    let by = 3.0 * (y2 - y1) - cy;
    let ay = 1.0 - cy - by;
    let sample_x = |t: f64| ((ax * t + bx) * t + cx) * t;
    let sample_y = |t: f64| ((ay * t + by) * t + cy) * t;
    let derivative_x = |t: f64| (3.0 * ax * t + 2.0 * bx) * t + cx;

    let solve_x = || {
        let mut t = x;
        for _ in 0..8 {
            let estimate = sample_x(t) - x;
            if estimate.abs() < 1e-6 {
                return t;
            }
            let derivative = derivative_x(t);
            if derivative.abs() < 1e-6 {
                break;
            }
            t -= estimate / derivative;
        }
        let (mut lower, mut upper) = (0.0, 1.0);
        t = x;
        for _ in 0..12 {
            let estimate = sample_x(t);
            if (estimate - x).abs() < 1e-7 {
                return t;
            }
            if x > estimate {
                lower = t;
            } else {
                upper = t;
            }
            t = (upper - lower) * 0.5 + lower;
        }
        t
    };
    sample_y(solve_x())
}

/// How a running animation repeats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Repeat {
    Loop,
    PingPong,
}

/// Parameters of [`Animation::start`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AnimationOptions {
    pub duration_ms: f64,
    /// Start value; the current value when `None`.
    pub from: Option<f64>,
    pub to: f64,
    pub easing: Easing,
    pub repeat: Option<Repeat>,
}

impl AnimationOptions {
    /// Animate from the current value to `to` over `duration_ms`.
    pub fn to(to: f64, duration_ms: f64) -> Self {
        Self {
            duration_ms,
            from: None,
            to,
            easing: Easing::Linear,
            repeat: None,
        }
    }

    pub fn from(mut self, from: f64) -> Self {
        self.from = Some(from);
        self
    }

    pub fn easing(mut self, easing: Easing) -> Self {
        self.easing = easing;
        self
    }

    pub fn repeat(mut self, repeat: Repeat) -> Self {
        self.repeat = Some(repeat);
        self
    }
}

struct Timeline {
    started_at_ms: f64,
    duration_ms: f64,
    from: f64,
    to: f64,
    easing: Easing,
    repeat: Option<Repeat>,
    /// `start_then`'s callback: `true` once `to` is reached, `false` when
    /// cut short.
    finished: Option<Box<dyn FnOnce(bool)>>,
}

struct AnimationEntry {
    value: Signal<f64>,
    timeline: Option<Timeline>,
    /// The output whose clock drives it; `None` keeps the shared clock.
    output: Option<OutputResolver>,
}

type OutputResolver = Rc<dyn Fn() -> Option<String>>;
type AnimationRef = Rc<RefCell<AnimationEntry>>;
type GroupCallback = Rc<dyn Fn(&TimerHandle, &str)>;

/// The output a poll or an animation is bound to (`createPoll`'s `output`):
/// an output name, a signal of one (followed as it changes, e.g.
/// [`Window::output`](crate::window::Window::output)), or a function read on
/// every tick. `None` means "no output right now"; the work then runs on
/// wall-clock timer ticks until it has one again.
#[derive(Clone)]
pub struct PollOutput(OutputResolver);

impl PollOutput {
    pub fn from_fn(f: impl Fn() -> Option<String> + 'static) -> Self {
        Self(Rc::new(f))
    }

    fn resolve(&self) -> Option<String> {
        untrack(|| (self.0)())
    }
}

impl From<&str> for PollOutput {
    fn from(name: &str) -> Self {
        String::from(name).into()
    }
}

impl From<String> for PollOutput {
    fn from(name: String) -> Self {
        Self(Rc::new(move || Some(name.clone())))
    }
}

impl From<&String> for PollOutput {
    fn from(name: &String) -> Self {
        name.clone().into()
    }
}

impl From<ReadSignal<Option<String>>> for PollOutput {
    fn from(signal: ReadSignal<Option<String>>) -> Self {
        Self(Rc::new(move || signal.try_with(Clone::clone).flatten()))
    }
}

impl From<Signal<Option<String>>> for PollOutput {
    fn from(signal: Signal<Option<String>>) -> Self {
        signal.read_only().into()
    }
}

impl From<Memo<Option<String>>> for PollOutput {
    fn from(memo: Memo<Option<String>>) -> Self {
        Self(Rc::new(move || memo.try_with(Clone::clone).flatten()))
    }
}

/// Time, kept per output.
///
/// An output that renders frames (`frame_outputs`, as the compositor last
/// reported them) has its own clock: the presentation time of its frames,
/// advanced only by the frame ticks for that output. Polls and animations
/// bound to it step there, exactly once per frame and stamped with the vblank
/// the frame lands on, so their cadence on screen is the same from run to run
/// however far ahead of the wall clock frames are prepared. Work bound to an
/// output that renders no frames runs on wall-clock timer ticks instead
/// (`wall_ms`). Work bound to no output at all (`set_timeout`,
/// `set_interval`, unbound animations) runs on any tick and sees
/// `scheduler_ms`, the latest time any clock has reached.
#[derive(Default)]
struct Clock {
    /// What [`now_ms`] returns: the time of the poll being run, else
    /// `scheduler_ms`.
    now_ms: f64,
    scheduler_ms: f64,
    wall_ms: f64,
    output_ms: HashMap<String, f64>,
    frame_outputs: HashSet<String>,
    animations: Vec<Option<Rc<RefCell<AnimationEntry>>>>,
    timers: Vec<Timer>,
    groups: Vec<PollGroup>,
    next_timer_id: u64,
}

impl Clock {
    fn is_frame_driven(&self, output: Option<&str>) -> bool {
        output.is_some_and(|output| self.frame_outputs.contains(output))
    }

    /// The time work bound to `output` starting now is anchored at: the
    /// output's own clock while it renders frames (never behind the wall
    /// clock, so an output that has been idle does not hand new work a head
    /// start), the wall clock otherwise.
    fn base_ms(&self, output: Option<&str>) -> f64 {
        match output {
            Some(name) if self.frame_outputs.contains(name) => {
                self.output_ms.get(name).copied().unwrap_or(self.wall_ms).max(self.wall_ms)
            }
            _ => self.wall_ms,
        }
    }
}

type PollCallback = Rc<dyn Fn(&TimerHandle)>;

struct Timer {
    id: u64,
    due_ms: f64,
    interval_ms: Option<f64>,
    callback: PollCallback,
    /// `Some` for a poll bound to an output.
    output: Option<PollOutput>,
    /// Time of the current (or last) run.
    now_ms: f64,
}

struct PollGroup {
    id: u64,
    interval_ms: f64,
    callback: GroupCallback,
    members: Vec<(String, TimerHandle)>,
}

thread_local! {
    static CLOCK: RefCell<Clock> = RefCell::new(Clock::default());
}

pub(crate) fn reset_clock() {
    CLOCK.with(|clock| *clock.borrow_mut() = Clock::default());
}

/// The time of the poll being run (its output's clock), else the latest time
/// the compositor has reported, in milliseconds.
pub fn now_ms() -> f64 {
    CLOCK.with(|clock| clock.borrow().now_ms)
}

/// Advance every clock to at least `now_ms` (tests, and runtimes without
/// frame ticks).
pub(crate) fn set_now(now_ms: f64) {
    CLOCK.with(|clock| {
        let mut clock = clock.borrow_mut();
        clock.wall_ms = clock.wall_ms.max(now_ms);
        clock.scheduler_ms = clock.scheduler_ms.max(now_ms);
        clock.now_ms = clock.scheduler_ms;
    });
}

/// The start of a runtime turn that is not a frame tick: the request's
/// timestamp is wall-clock time. Every clock is monotonic: a stamp from
/// behind never pulls one back.
pub(crate) fn begin_turn(now_ms: f64) {
    if now_ms > 0.0 {
        set_now(now_ms);
    }
    let (wall, scheduler) = CLOCK.with(|clock| {
        let clock = clock.borrow();
        (clock.wall_ms, clock.scheduler_ms)
    });
    advance_where(wall, |clock, output| {
        output.is_some() && !clock.is_frame_driven(output.as_deref())
    });
    // A turn may start animations of its own; unbound ones keep the shared
    // clock and are synchronized at every turn boundary.
    advance_where(scheduler, |_, output| output.is_none());
}

/// A scheduler tick: `output` names the output whose frame (presented at
/// `now_ms`) is about to render; `None` is a wall-clock timer tick. Runs the
/// polls this tick drives and advances the animations on its clock.
pub(crate) fn tick(output: Option<&str>, frame_outputs: &[String], now_ms: f64) {
    let tick_ms = CLOCK.with(|clock| {
        let mut clock = clock.borrow_mut();
        let clock = &mut *clock;
        if frame_outputs.len() != clock.frame_outputs.len()
            || frame_outputs.iter().any(|name| !clock.frame_outputs.contains(name))
        {
            clock.frame_outputs = frame_outputs.iter().cloned().collect();
            let frame = &clock.frame_outputs;
            clock.output_ms.retain(|name, _| frame.contains(name));
        }
        let tick_ms = match output {
            Some(name) => {
                let time = clock.output_ms.get(name).copied().unwrap_or(now_ms).max(now_ms);
                clock.output_ms.insert(name.to_owned(), time);
                time
            }
            None => {
                clock.wall_ms = clock.wall_ms.max(now_ms);
                clock.wall_ms
            }
        };
        clock.scheduler_ms = clock.scheduler_ms.max(tick_ms);
        tick_ms
    });
    run_due_polls(output, tick_ms);
    match output {
        Some(name) => advance_where(tick_ms, |_, bound| bound.as_deref() == Some(name)),
        None => advance_where(tick_ms, |clock, bound| {
            bound.is_some() && !clock.is_frame_driven(bound.as_deref())
        }),
    }
    let scheduler = CLOCK.with(|clock| {
        let mut clock = clock.borrow_mut();
        clock.now_ms = clock.scheduler_ms;
        clock.scheduler_ms
    });
    advance_where(scheduler, |_, bound| bound.is_none());
}

/// A tick runs work due within this much past its time; the compositor uses
/// the same tolerance when it decides whether to tick.
const FRAME_DUE_TOLERANCE_MS: f64 = 0.5;

fn run_due_polls(tick_output: Option<&str>, tick_ms: f64) {
    let candidates: Vec<(u64, Option<PollOutput>)> = CLOCK.with(|clock| {
        clock
            .borrow()
            .timers
            .iter()
            .map(|timer| (timer.id, timer.output.clone()))
            .collect()
    });
    for (id, output) in candidates {
        // Where the poll stands for this tick: the time it would run at, or
        // `None` when this tick does not drive it. A poll bound to a
        // frame-rendering output runs only on that output's frames; one bound
        // to an output without frames, only on timer ticks; an unbound timer,
        // on any tick.
        let run_ms = match &output {
            None => Some(tick_ms),
            Some(output) => {
                let bound = output.resolve();
                let frame_driven = CLOCK.with(|clock| clock.borrow().is_frame_driven(bound.as_deref()));
                if frame_driven {
                    (bound.as_deref() == tick_output).then_some(tick_ms)
                } else {
                    tick_output.is_none().then_some(tick_ms)
                }
            }
        };
        let Some(run_ms) = run_ms else {
            continue;
        };
        let callback = CLOCK.with(|clock| {
            let mut clock = clock.borrow_mut();
            let index = clock.timers.iter().position(|timer| timer.id == id)?;
            let timer = &mut clock.timers[index];
            if timer.due_ms > run_ms + FRAME_DUE_TOLERANCE_MS {
                return None;
            }
            timer.now_ms = run_ms;
            let callback = timer.callback.clone();
            match timer.interval_ms {
                Some(interval) => timer.due_ms = run_ms + interval,
                None => {
                    clock.timers.remove(index);
                }
            }
            clock.now_ms = run_ms;
            Some(callback)
        });
        if let Some(callback) = callback {
            callback(&TimerHandle(id));
        }
    }
}

fn advance_where(now_ms: f64, select: impl Fn(&Clock, Option<String>) -> bool) {
    let entries: Vec<(AnimationRef, Option<OutputResolver>)> = CLOCK.with(|clock| {
        clock
            .borrow()
            .animations
            .iter()
            .flatten()
            .filter(|entry| entry.borrow().timeline.is_some())
            .map(|entry| (entry.clone(), entry.borrow().output.clone()))
            .collect()
    });
    for (entry, output) in entries {
        // Bound to an output that resolves to nothing: the shared clock.
        let bound = output.and_then(|output| untrack(|| output()));
        if CLOCK.with(|clock| select(&clock.borrow(), bound)) {
            advance_entry(&entry, now_ms);
        }
    }
}

/// Advance every running animation to `now_ms`, whatever its clock. Returns
/// whether any is still running.
#[cfg(test)]
pub(crate) fn advance_animations(now_ms: f64) -> bool {
    advance_where(now_ms, |_, _| true);
    has_running_animations()
}

fn advance_entry(entry: &Rc<RefCell<AnimationEntry>>, now_ms: f64) {
    let update = {
        let mut entry = entry.borrow_mut();
        let Some(timeline) = &entry.timeline else {
            return;
        };
        let elapsed = (now_ms - timeline.started_at_ms).max(0.0);
        let raw = elapsed / timeline.duration_ms;
        let progress = match timeline.repeat {
            None => clamp_unit(raw),
            Some(Repeat::Loop) => raw - raw.floor(),
            Some(Repeat::PingPong) => {
                let cycle = raw % 2.0;
                if cycle <= 1.0 { cycle } else { 2.0 - cycle }
            }
        };
        let eased = timeline.easing.apply(progress);
        let next = timeline.from + (timeline.to - timeline.from) * eased;
        if timeline.repeat.is_none() && (raw >= 1.0 || (next - timeline.to).abs() <= 1e-4) {
            let to = timeline.to;
            let finished = entry.timeline.take().and_then(|timeline| timeline.finished);
            (entry.value, to, finished)
        } else {
            (entry.value, next, None)
        }
    };
    update.0.set(update.1);
    if let Some(finished) = update.2 {
        finished(true);
    }
}

/// Clear `entry`'s timeline, telling its `start_then` callback it was cut short.
fn interrupt(entry: &AnimationRef) {
    let finished = entry
        .borrow_mut()
        .timeline
        .take()
        .and_then(|timeline| timeline.finished);
    if let Some(finished) = finished {
        finished(false);
    }
}

/// A number animated over time; reading it is tracked like a signal.
#[derive(Clone, Copy)]
pub struct Animation {
    value: Signal<f64>,
    slot: usize,
}

impl Animation {
    /// Created in (and disposed with) the current scope. It runs on the
    /// shared clock; see [`on_output`](Self::on_output).
    pub fn new(initial: f64) -> Self {
        let value = signal(initial);
        let entry = Rc::new(RefCell::new(AnimationEntry {
            value,
            timeline: None,
            output: None,
        }));
        let slot = CLOCK.with(|clock| {
            let mut clock = clock.borrow_mut();
            match clock.animations.iter().position(Option::is_none) {
                Some(slot) => {
                    clock.animations[slot] = Some(entry);
                    slot
                }
                None => {
                    clock.animations.push(Some(entry));
                    clock.animations.len() - 1
                }
            }
        });
        on_cleanup(move || {
            CLOCK.with(|clock| {
                if let Some(entry) = clock.borrow_mut().animations.get_mut(slot) {
                    *entry = None;
                }
            });
        });
        Self { value, slot }
    }

    /// Step on the frames of `output` (e.g. the window it is shown on), like
    /// the animations of a TypeScript window or layer: once per frame, at the
    /// frame's presentation time.
    pub fn on_output(self, output: impl Into<PollOutput>) -> Self {
        if let Some(entry) = self.entry() {
            entry.borrow_mut().output = Some(output.into().0);
        }
        self
    }

    fn entry(&self) -> Option<Rc<RefCell<AnimationEntry>>> {
        CLOCK.with(|clock| {
            clock
                .borrow()
                .animations
                .get(self.slot)
                .cloned()
                .flatten()
                .filter(|entry| entry.borrow().value == self.value)
        })
    }

    /// Tracked read of the current value.
    pub fn get(&self) -> f64 {
        self.value.try_with(|value| *value).unwrap_or_default()
    }

    pub fn get_untracked(&self) -> f64 {
        self.value
            .try_with(|value| *value)
            .map(|_| self.value.get_untracked())
            .unwrap_or_default()
    }

    pub fn signal(&self) -> ReadSignal<f64> {
        self.value.read_only()
    }

    pub fn map<U: PartialEq + 'static>(self, f: impl Fn(f64) -> U + 'static) -> Memo<U> {
        self.value.map(move |value| f(*value))
    }

    pub fn start(&self, options: AnimationOptions) {
        self.start_with(options, None);
    }

    /// [`start`](Self::start), then run `f` with `true` once the value
    /// reaches its target, or with `false` if `start`, `stop` or `set` cut it
    /// short (a repeating animation only ends that way). Like awaiting the
    /// promise TypeScript's `start` returns.
    ///
    /// ```no_run
    /// # use shojiwm_rs::prelude::*;
    /// # let fade = Animation::new(1.0);
    /// fade.start_then(AnimationOptions::to(0.0, 150.0), |done| {
    ///     if done {
    ///         // dispose of what faded out
    ///     }
    /// });
    /// ```
    pub fn start_then(&self, options: AnimationOptions, f: impl FnOnce(bool) + 'static) {
        self.start_with(options, Some(Box::new(f)));
    }

    fn start_with(&self, options: AnimationOptions, finished: Option<Box<dyn FnOnce(bool)>>) {
        let Some(entry) = self.entry() else {
            if let Some(finished) = finished {
                finished(false);
            }
            return;
        };
        interrupt(&entry);
        let from = options.from.unwrap_or_else(|| self.get_untracked());
        let output = entry.borrow().output.clone();
        let started_at_ms = match output {
            Some(output) => {
                let bound = untrack(|| output());
                CLOCK.with(|clock| clock.borrow().base_ms(bound.as_deref()))
            }
            None => CLOCK.with(|clock| clock.borrow().scheduler_ms),
        };
        entry.borrow_mut().timeline = Some(Timeline {
            started_at_ms,
            duration_ms: options.duration_ms.max(1.0).floor(),
            from,
            to: options.to,
            easing: options.easing,
            repeat: options.repeat,
            finished,
        });
        self.value.set(from);
    }

    pub fn stop(&self) {
        if let Some(entry) = self.entry() {
            interrupt(&entry);
        }
    }

    /// Jump to `value`, stopping any running animation.
    pub fn set(&self, value: f64) {
        self.stop();
        self.value.set(value);
    }

    pub fn running(&self) -> bool {
        self.entry()
            .is_some_and(|entry| entry.borrow().timeline.is_some())
    }
}

impl From<Animation> for Prop<f64> {
    fn from(animation: Animation) -> Self {
        Prop::Signal(animation.value)
    }
}

pub(crate) fn has_running_animations() -> bool {
    CLOCK.with(|clock| {
        clock
            .borrow()
            .animations
            .iter()
            .flatten()
            .any(|entry| entry.borrow().timeline.is_some())
    })
}

/// A scheduled callback or poll; cancel it with [`TimerHandle::cancel`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimerHandle(u64);

/// The handle of a [`create_poll`] (`PollHandle` in TypeScript).
pub type PollHandle = TimerHandle;

impl TimerHandle {
    pub fn cancel(self) {
        let members = CLOCK.with(|clock| {
            let mut clock = clock.borrow_mut();
            clock.timers.retain(|timer| timer.id != self.0);
            let index = clock.groups.iter().position(|group| group.id == self.0)?;
            Some(clock.groups.remove(index).members)
        });
        for (_, member) in members.unwrap_or_default() {
            member.cancel();
        }
    }

    pub fn is_pending(self) -> bool {
        CLOCK.with(|clock| {
            let clock = clock.borrow();
            clock.timers.iter().any(|timer| timer.id == self.0)
                || clock.groups.iter().any(|group| group.id == self.0)
        })
    }

    /// Time of the current (or last) run on the poll's clock: for a poll
    /// bound to an output, the presentation time of the frame this run
    /// belongs to.
    pub fn now_ms(self) -> f64 {
        CLOCK.with(|clock| {
            let clock = clock.borrow();
            clock
                .timers
                .iter()
                .find(|timer| timer.id == self.0)
                .map_or(clock.now_ms, |timer| timer.now_ms)
        })
    }
}

/// Whole milliseconds, at least 1, like the TypeScript scheduler's polls.
///
/// Rounding *down* matters for per-frame intervals: a 60 Hz interval of
/// 16.67 ms would sometimes come due a fraction of a millisecond after the
/// next frame's timestamp (frame times jitter), skipping that frame and
/// catching up with a double step on the next one. At 16 ms it fires on
/// every frame.
fn whole_ms(delay_ms: f64) -> f64 {
    if delay_ms.is_finite() { delay_ms.floor().max(1.0) } else { 1.0 }
}

fn add_timer(
    delay_ms: f64,
    interval_ms: Option<f64>,
    output: Option<PollOutput>,
    callback: PollCallback,
) -> TimerHandle {
    let bound = output.as_ref().map(PollOutput::resolve);
    CLOCK.with(|clock| {
        let mut clock = clock.borrow_mut();
        clock.next_timer_id += 1;
        let id = clock.next_timer_id;
        let base_ms = match &bound {
            Some(bound) => clock.base_ms(bound.as_deref()),
            None => clock.scheduler_ms,
        };
        clock.timers.push(Timer {
            id,
            due_ms: base_ms + whole_ms(delay_ms),
            interval_ms,
            callback,
            output,
            now_ms: base_ms,
        });
        TimerHandle(id)
    })
}

/// Run `f` once, `delay_ms` from now (`setTimeout`), on the shared clock.
pub fn set_timeout(delay_ms: f64, f: impl Fn() + 'static) -> TimerHandle {
    add_timer(delay_ms, None, None, Rc::new(move |_| f()))
}

/// Run `f` every `interval_ms` until cancelled, on the shared clock: on
/// whichever output renders next. For anything shown on screen prefer
/// [`create_poll`], which steps on one output's frames.
pub fn set_interval(interval_ms: f64, f: impl Fn() + 'static) -> TimerHandle {
    let interval_ms = whole_ms(interval_ms);
    add_timer(interval_ms, Some(interval_ms), None, Rc::new(move |_| f()))
}

/// Run `f` every `interval_ms` on the frames of `output` (`createPoll`).
///
/// A poll runs at most once per frame of that output, stamped with the
/// frame's presentation time ([`TimerHandle::now_ms`]), so its steps are
/// evenly spaced on screen. An interval shorter than one refresh period (`1`
/// for "every frame") runs every frame; intervals are whole milliseconds,
/// rounded down. When the output renders no frames (off, gone, or `None`)
/// the poll runs on wall-clock timers. It keeps running until cancelled
/// (cancelling inside `f` makes a one-shot timer).
///
/// ```no_run
/// use shojiwm_rs::prelude::*;
///
/// let last = std::cell::Cell::new(None::<f64>);
/// create_poll(1.0, "DP-1", move |handle| {
///     let dt = last.get().map_or(0.0, |last| handle.now_ms() - last);
///     last.set(Some(handle.now_ms()));
///     # let _ = dt;
/// });
/// ```
pub fn create_poll(
    interval_ms: f64,
    output: impl Into<PollOutput>,
    f: impl Fn(&PollHandle) + 'static,
) -> PollHandle {
    let interval_ms = whole_ms(interval_ms);
    add_timer(interval_ms, Some(interval_ms), Some(output.into()), Rc::new(f))
}

/// Run `f` every `interval_ms` on every enabled output, each on its own
/// clock (`createPollForEachOutput`). Outputs that appear later get their
/// own poll; a poll whose output goes away is cancelled. Cancelling the
/// returned handle cancels all of them. Keep state per output name: the
/// outputs' times come from different clocks and interleave.
pub fn create_poll_for_each_output(
    interval_ms: f64,
    f: impl Fn(&PollHandle, &str) + 'static,
) -> PollHandle {
    let id = CLOCK.with(|clock| {
        let mut clock = clock.borrow_mut();
        clock.next_timer_id += 1;
        let id = clock.next_timer_id;
        clock.groups.push(PollGroup {
            id,
            interval_ms,
            callback: Rc::new(f),
            members: Vec::new(),
        });
        id
    });
    sync_output_poll_groups();
    TimerHandle(id)
}

/// Give every [`create_poll_for_each_output`] group one poll per enabled
/// output. Called when outputs change.
pub(crate) fn sync_output_poll_groups() {
    let outputs: Vec<String> = crate::reactive::untrack(|| {
        crate::runtime::global().outputs.with(|outputs| {
            outputs
                .values()
                .filter(|output| output.enabled)
                .map(|output| output.name.clone())
                .collect()
        })
    });
    type Group = (u64, f64, GroupCallback, Vec<(String, TimerHandle)>);
    let groups: Vec<Group> =
        CLOCK.with(|clock| {
            clock
                .borrow()
                .groups
                .iter()
                .map(|group| (group.id, group.interval_ms, group.callback.clone(), group.members.clone()))
                .collect()
        });
    for (id, interval_ms, callback, members) in groups {
        let mut next = Vec::new();
        for (name, member) in members {
            if outputs.contains(&name) && member.is_pending() {
                next.push((name, member));
            } else {
                member.cancel();
            }
        }
        for name in &outputs {
            if next.iter().any(|(member, _)| member == name) {
                continue;
            }
            let callback = callback.clone();
            let output = name.clone();
            let poll = create_poll(interval_ms, name.as_str(), move |handle| callback(handle, &output));
            next.push((name.clone(), poll));
        }
        CLOCK.with(|clock| {
            if let Some(group) = clock.borrow_mut().groups.iter_mut().find(|group| group.id == id) {
                group.members = next;
            }
        });
    }
}

/// When scheduled work next comes due, per frame-rendering output and for
/// the wall-clock timer (the compositor's `RuntimeSchedule`): `0` while an
/// animation on that clock runs, i.e. every frame.
pub(crate) fn schedule() -> shojiwm_lib::runtime_api::RuntimeSchedule {
    let mut schedule = shojiwm_lib::runtime_api::RuntimeSchedule::default();
    type Pending = (Vec<(f64, Option<PollOutput>)>, Vec<Option<OutputResolver>>, Vec<String>);
    let (timers, animations, frame_outputs): Pending = CLOCK.with(|clock| {
            let clock = clock.borrow();
            (
                clock.timers.iter().map(|timer| (timer.due_ms, timer.output.clone())).collect(),
                clock
                    .animations
                    .iter()
                    .flatten()
                    .filter(|entry| entry.borrow().timeline.is_some())
                    .map(|entry| entry.borrow().output.clone())
                    .collect(),
                clock.frame_outputs.iter().cloned().collect(),
            )
        });
    let due = |output: Option<String>, at_ms: f64, schedule: &mut shojiwm_lib::runtime_api::RuntimeSchedule| {
        match output {
            // Work bound to no output runs on any tick: the next frame of
            // every rendering output, or a timer tick when none renders.
            None if frame_outputs.is_empty() => {
                schedule.timer_due_ms = Some(schedule.timer_due_ms.map_or(at_ms, |due| due.min(at_ms)));
            }
            None => {
                for name in &frame_outputs {
                    let entry = schedule.output_due_ms.entry(name.clone()).or_insert(at_ms);
                    *entry = entry.min(at_ms);
                }
            }
            Some(name) if frame_outputs.contains(&name) => {
                let entry = schedule.output_due_ms.entry(name).or_insert(at_ms);
                *entry = entry.min(at_ms);
            }
            Some(_) => {
                schedule.timer_due_ms = Some(schedule.timer_due_ms.map_or(at_ms, |due| due.min(at_ms)));
            }
        }
    };
    for (due_ms, output) in timers {
        match output {
            None => due(None, due_ms, &mut schedule),
            // An output that resolves to nothing right now runs on timers.
            Some(output) => match output.resolve() {
                Some(name) => due(Some(name), due_ms, &mut schedule),
                None => {
                    schedule.timer_due_ms = Some(schedule.timer_due_ms.map_or(due_ms, |due| due.min(due_ms)));
                }
            },
        }
    }
    for output in animations {
        let bound = output.and_then(|output| untrack(|| output()));
        due(bound, 0.0, &mut schedule);
    }
    schedule
}

/// Milliseconds until the next timer, if any, from the wall clock.
pub(crate) fn next_timer_delay() -> Option<u64> {
    CLOCK.with(|clock| {
        let clock = clock.borrow();
        clock
            .timers
            .iter()
            .map(|timer| (timer.due_ms - clock.wall_ms).ceil().max(1.0) as u64)
            .min()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reactive::Scope;

    #[test]
    fn cubic_bezier_matches_endpoints_and_is_monotonic() {
        let ease = Easing::EASE_OUT_CUBIC;
        assert_eq!(ease.apply(0.0), 0.0);
        assert_eq!(ease.apply(1.0), 1.0);
        let mut previous = 0.0;
        for step in 1..10 {
            let value = ease.apply(step as f64 / 10.0);
            assert!(value >= previous);
            previous = value;
        }
        assert!((Easing::Linear.apply(0.25) - 0.25).abs() < 1e-9);
    }

    #[test]
    fn animation_advances_with_the_clock() {
        reset_clock();
        let scope = Scope::root();
        let value = scope.run(|| Animation::new(0.0));
        set_now(100.0);
        value.start(AnimationOptions::to(1.0, 100.0));
        assert!(value.running());
        set_now(150.0);
        assert!(advance_animations(150.0));
        assert!((value.get_untracked() - 0.5).abs() < 1e-9);
        set_now(200.0);
        assert!(!advance_animations(200.0));
        assert_eq!(value.get_untracked(), 1.0);
        scope.dispose();
        assert!(!has_running_animations());
    }

    #[test]
    fn a_per_frame_interval_fires_on_every_jittery_frame() {
        reset_clock();
        let fired = Rc::new(std::cell::Cell::new(0));
        let frame = 1000.0 / 60.0;
        {
            let fired = fired.clone();
            set_interval(frame, move || fired.set(fired.get() + 1));
        }
        // Predicted presentation times wobble by a fraction of a millisecond.
        for (index, jitter) in [0.0, -0.3, 0.2, -0.4, 0.1, -0.2, 0.3, -0.1].into_iter().enumerate() {
            let now = (index + 1) as f64 * frame + jitter;
            tick(None, &[], now);
            assert_eq!(fired.get(), index + 1, "frame {index} was skipped");
        }
    }

    #[test]
    fn timers_fire_once_or_repeatedly() {
        reset_clock();
        let fired = Rc::new(RefCell::new(Vec::new()));
        {
            let fired = fired.clone();
            set_timeout(10.0, move || fired.borrow_mut().push("once"));
        }
        let interval = {
            let fired = fired.clone();
            set_interval(5.0, move || fired.borrow_mut().push("tick"))
        };
        assert_eq!(next_timer_delay(), Some(5));
        tick(None, &[], 5.0);
        tick(None, &[], 10.0);
        interval.cancel();
        tick(None, &[], 20.0);
        assert_eq!(*fired.borrow(), ["tick", "once", "tick"]);
        assert_eq!(next_timer_delay(), None);
    }

    #[test]
    fn polls_step_on_their_own_output_clock() {
        reset_clock();
        let runs = Rc::new(RefCell::new(Vec::new()));
        for name in ["A", "B"] {
            let runs = runs.clone();
            create_poll(1.0, name, move |handle| runs.borrow_mut().push((name, handle.now_ms())));
        }
        let frames = ["A".to_owned(), "B".to_owned()];
        tick(Some("A"), &frames, 16.0);
        tick(Some("B"), &frames, 10.0);
        // A wall-clock tick runs neither: both outputs render frames.
        tick(None, &frames, 20.0);
        tick(Some("A"), &frames, 32.0);
        assert_eq!(*runs.borrow(), [("A", 16.0), ("B", 10.0), ("A", 32.0)]);

        let due = schedule();
        assert_eq!(due.output_due_ms["A"], 33.0);
        assert_eq!(due.timer_due_ms, None);

        // "B" stops rendering frames: its poll moves to timer ticks.
        tick(None, &frames[..1], 40.0);
        assert_eq!(runs.borrow().last(), Some(&("B", 40.0)));
        assert!(schedule().timer_due_ms.is_some());
    }

    #[test]
    fn bound_animations_follow_their_output_frames() {
        reset_clock();
        let scope = Scope::root();
        let value = scope.run(|| Animation::new(0.0).on_output("A"));
        let frames = ["A".to_owned()];
        tick(Some("A"), &frames, 100.0);
        value.start(AnimationOptions::to(1.0, 100.0));
        assert_eq!(schedule().output_due_ms["A"], 0.0);
        // Another clock moving does not step it.
        begin_turn(140.0);
        assert_eq!(value.get_untracked(), 0.0);
        tick(Some("A"), &frames, 150.0);
        assert!((value.get_untracked() - 0.5).abs() < 1e-9);
        scope.dispose();
    }
}
