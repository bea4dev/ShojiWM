//! Port of `packages/config/src/window-switcher.tsx`: the window switchers'
//! common core (`Super+Tab`).
//!
//! A switcher lifts every window (all workspaces, minimized ones too, most
//! recently used first) off the desktop and lays them out in a scene; a
//! `SwitcherLayout` decides where each window goes and how input moves the
//! selection (`flip_3d.rs`: a receding 3D stack, `window_grid.rs`: a flat
//! grid). The core does the rest, the same for every layout:
//!
//! - `Escape` returns to the desktop unchanged
//! - `Return` / `Space` or a click on a window switches
//!
//! Each window is a render texture of its own, framed with a margin for its
//! shadow, laid out as a plane of a 3D scene. Opening and closing ease with
//! the window manager's easing; the layout starts and ends exactly on the
//! windows' real positions, so the desktop morphs into it and back.
//!
//! A layout with `card_actions` (the grid) also acts on the windows: the
//! title bar buttons of the selected card minimize, maximize or close its
//! window, a middle click closes it, and the other cards make room when one
//! closes. A window that asks before closing ("save changes?") ends the
//! switcher on it, so its dialog is not hidden behind the cards.
//!
//! Backdrop blur cannot work inside a texture that holds one window (nothing
//! lies under the window there), so window blur is switched off while the
//! switcher is open (`blur_suspended`) and faded back in afterwards
//! (`backdrop_strength`): the hand-off back to the real desktop happens in the
//! slow tail of the closing animation, and the blur fades in over that tail.
//!
//! Unlike the TypeScript version, a layout's input handlers return a
//! `SwitcherAction` for the core to carry out instead of calling back into
//! it, which keeps the session's `RefCell`s from being borrowed twice.

use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    rc::{Rc, Weak},
};

use shojiwm_rs::{
    prelude::*,
    ssd::{GestureSwipeEventSnapshot, WaylandOutputSnapshot},
};

use crate::window_manager::{WINDOW_MANAGEMENT_EASING, WINDOW_STATE_MINIMIZED, WindowManager};

/// Opening and closing, in ms.
const TRANSITION_MS: f64 = 450.0;
/// The blur fading back in after closing, in ms.
const BLUR_FADE_MS: f64 = 350.0;
/// Room around each window's texture for its shadow (logical px).
pub const MARGIN: f64 = 48.0;
/// Closing hands back to the real desktop once every window is this close (px).
const HANDOFF_EPSILON_PX: f64 = 0.75;
/// World units between windows of the real stack. On the desktop the windows are
/// pulled towards the camera by their stacking rank (and shrunk to match, so
/// they still cover exactly their pixels): the depth test then lays them over
/// each other in their real order as they settle, instead of z-fighting.
const STACK_DEPTH_STEP: f64 = 2.0;
/// The camera's vertical field of view: narrow, for Vista's long-lens look.
const CAMERA_FOV_DEGREES: f64 = 12.0;
/// Cards making room after one closes, and the closed card fading out (ms).
const RELAYOUT_MS: f64 = 300.0;
/// A card fading to or from its minimized look (ms).
const CARD_FADE_MS: f64 = 200.0;

/// A window's placement in the scene (world units; z = 0 is the screen, 1:1).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pose {
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub rotate_x: f64,
    pub rotate_y: f64,
    pub scale: f64,
    pub opacity: f64,
}

/// The output a switcher is open on, with its logical size.
pub struct Viewport {
    pub output: WaylandOutputSnapshot,
    pub width: f64,
    pub height: f64,
}

/// What a layout's input handler asks the core to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwitcherAction {
    None,
    /// Select window `index` (unbounded, taken modulo the window count).
    Select(i64),
    /// Switch to the selected window.
    Commit,
    /// Back to the desktop unchanged.
    Cancel,
}

/// Where a switcher puts the windows, and how input moves the selection.
pub trait SwitcherLayout {
    /// Names the window textures.
    fn name(&self) -> &'static str;
    /// How far the desktop dims behind the windows.
    fn dim_alpha(&self) -> f64;
    /// The window selected on opening.
    fn initial_selection(&self, count: usize) -> i64;
    /// Fresh per-session state for `windows`.
    fn begin(&self, windows: &[Window]) -> Box<dyn LayoutSession>;
    /// The windows act on clicks (title bar buttons, middle click) and stay
    /// in the layout when minimized.
    fn card_actions(&self) -> bool {
        false
    }
    /// How opaque a minimized window's card is.
    fn minimized_opacity(&self) -> f64 {
        1.0
    }
}

/// A layout's state for one opening of the switcher. `selected` is the
/// selected window's index, unbounded (taken modulo the window count).
pub trait LayoutSession {
    /// Window `index`'s pose while the switcher is shown. Windows not on screen
    /// before opening (or after closing) fade in from (out to) behind it.
    fn pose(&self, index: usize, rect: Rect, viewport: &Viewport, selected: i64) -> Pose;
    /// Advance the layout's own animation by `dt_ms`; true while it still moves.
    fn tick(&mut self, _selected: i64, _dt_ms: f64) -> bool {
        false
    }
    /// Closing started: stop the layout's own animation where it is headed.
    fn settle(&mut self, _selected: i64) {}
    /// Keys besides `Escape` / `Return` / `Space`; `None` when not handled.
    fn on_key(&mut self, _event: &InputGrabKeyEvent, _selected: i64) -> Option<SwitcherAction> {
        None
    }
    /// `Super` went up.
    fn on_super_released(&mut self, selected: i64) -> SwitcherAction;
    /// The pointer moved; `hit` is the index of the window under it.
    fn on_pointer_motion(&mut self, _hit: Option<usize>, _selected: i64) -> SwitcherAction {
        SwitcherAction::None
    }
    fn on_scroll(&mut self, _event: &InputGrabScrollEvent, _selected: i64) -> SwitcherAction {
        SwitcherAction::None
    }
    fn on_swipe(&mut self, _event: &GestureSwipeEventSnapshot, _selected: i64) -> SwitcherAction {
        SwitcherAction::None
    }
    /// A left click that hit no window.
    fn on_click_outside(&mut self) -> SwitcherAction {
        SwitcherAction::None
    }
    /// A window left the switcher (it closed): lay out `windows` again.
    fn windows_changed(&mut self, _windows: &[Window]) {}
}

/// A title bar button.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TitlebarButton {
    Minimize,
    Maximize,
    Close,
}

/// The title bar button at `(x, y)`: logical px from `window.rect()`'s top-left corner.
pub type TitlebarButtonAt = dyn Fn(Window, f64, f64) -> Option<TitlebarButton>;

/// A card whose window closed, fading out where it was.
struct Departing {
    window: Window,
    rect: Rect,
    from: Pose,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Entering,
    Shown,
    Leaving,
}

struct Session {
    output: String,
    windows: Vec<Window>,
    /// Where each window was on screen when the switcher opened (`None`: not shown).
    start_poses: HashMap<Window, Option<Pose>>,
    /// Each window's rank in the real stack (0 = bottom), once the switcher closes.
    ranks_after: HashMap<Window, usize>,
    /// The pose each window had when closing started.
    leave_from: HashMap<Window, Pose>,
    /// Windows on screen once the switcher closes.
    shown_after: HashSet<Window>,
    phase: Phase,
    /// Linear progress of the current opening/closing, 0..1.
    progress: f64,
    phase_start_ms: Option<f64>,
    /// The selected window's index, unbounded (taken modulo the window count).
    selected: i64,
    /// False after the selected card closed, until something selects a window.
    has_selection: bool,
    last_tick_ms: Option<f64>,
    grab: Option<InputGrab>,
    poll: Option<TimerHandle>,
    layout: Box<dyn LayoutSession>,
    /// Where each card moves from while the cards make room; progress 0..1.
    move_from: HashMap<Window, Pose>,
    move_progress: f64,
    departing: Vec<Departing>,
    /// Each card's opacity factor (a minimized card is dimmed).
    card_alpha: HashMap<Window, f64>,
    /// Windows asked to close from a card that have not closed yet, oldest first.
    close_requested: Vec<Window>,
}

type SessionRef = Rc<RefCell<Session>>;

struct Placed {
    window: Window,
    rect: Rect,
    pose: Pose,
}

struct PlacedPlane {
    window: Window,
    rect: Rect,
    pose: Pose,
    width: f64,
    height: f64,
    transform: Transform3D,
}

struct Inner {
    wm: WindowManager,
    layout: Box<dyn SwitcherLayout>,
    /// A window's place in the real stack, higher on top (its decoration's zIndex).
    stack_order: Box<dyn Fn(Window) -> i32>,
    current: RefCell<Option<SessionRef>>,
    /// Bumped when a session starts or ends, and whenever its animated state
    /// changes, so the composition re-evaluates once per frame while
    /// something moves.
    revision: Signal<u64>,
    blur_suspended: Signal<bool>,
    backdrop_strength: Signal<f64>,
    selected_window_id: Signal<Option<String>>,
    blur_fade: RefCell<Option<TimerHandle>>,
    titlebar_button_at: RefCell<Option<Rc<TitlebarButtonAt>>>,
}

/// A window switcher, whatever its layout. Cheap to clone.
#[derive(Clone)]
pub struct WindowSwitcher(Rc<Inner>);

impl WindowSwitcher {
    pub fn new(
        layout: impl SwitcherLayout + 'static,
        wm: WindowManager,
        stack_order: impl Fn(Window) -> i32 + 'static,
    ) -> Self {
        let inner = Rc::new(Inner {
            wm,
            layout: Box::new(layout),
            stack_order: Box::new(stack_order),
            current: RefCell::new(None),
            revision: signal(0),
            blur_suspended: signal(false),
            backdrop_strength: signal(1.0),
            selected_window_id: signal(None),
            blur_fade: RefCell::new(None),
            titlebar_button_at: RefCell::new(None),
        });
        if inner.layout.card_actions() {
            // Listeners cannot be removed, so they live as long as the
            // switcher and act on whichever session is open.
            let on = |f: fn(&Self, &SessionRef, Window)| {
                let weak: Weak<Inner> = Rc::downgrade(&inner);
                move |window: Window| {
                    let Some(inner) = weak.upgrade() else {
                        return;
                    };
                    let this = Self(inner);
                    if let Some(session) = this.current() {
                        f(&this, &session, window);
                    }
                }
            };
            COMPOSITOR.event.on_start_close(on(Self::remove_card));
            COMPOSITOR.event.on_close(on(Self::remove_card));
            COMPOSITOR.event.on_open(on(Self::window_opened));
        }
        Self(inner)
    }

    /// Where a card's title bar buttons are (layouts with `card_actions`).
    pub fn set_titlebar_button_at(&self, f: impl Fn(Window, f64, f64) -> Option<TitlebarButton> + 'static) {
        *self.0.titlebar_button_at.borrow_mut() = Some(Rc::new(f));
    }

    /// True while window backdrop effects should be off.
    pub fn blur_suspended(&self) -> ReadSignal<bool> {
        self.0.blur_suspended.read_only()
    }

    /// Scale for window backdrop effects (0..1); fades them back in after closing.
    pub fn backdrop_strength(&self) -> ReadSignal<f64> {
        self.0.backdrop_strength.read_only()
    }

    /// The selected window while the switcher is open, else `None`.
    pub fn selected_window_id(&self) -> ReadSignal<Option<String>> {
        self.0.selected_window_id.read_only()
    }

    fn touch(&self) {
        self.0.revision.update(|revision| *revision += 1);
    }

    fn current(&self) -> Option<SessionRef> {
        self.0.current.borrow().clone()
    }

    fn is_current(&self, session: &SessionRef) -> bool {
        self.current().is_some_and(|current| Rc::ptr_eq(&current, session))
    }

    fn set_current(&self, session: Option<SessionRef>) {
        *self.0.current.borrow_mut() = session;
        self.touch();
    }

    fn rank_map(&self, windows: &[Window]) -> HashMap<Window, usize> {
        let mut ordered: Vec<(usize, Window, i32)> = windows
            .iter()
            .enumerate()
            .map(|(recency, window)| (recency, *window, (self.0.stack_order)(*window)))
            .collect();
        // Ties: the more recently used window is on top.
        ordered.sort_by(|a, b| a.2.cmp(&b.2).then(b.0.cmp(&a.0)));
        ordered
            .into_iter()
            .enumerate()
            .map(|(rank, (_, window, _))| (window, rank))
            .collect()
    }

    /// Every window's pose for this frame.
    fn poses(&self, session: &Session, viewport: &Viewport) -> Vec<Placed> {
        let eased = WINDOW_MANAGEMENT_EASING.apply(session.progress);
        let moved = WINDOW_MANAGEMENT_EASING.apply(session.move_progress);
        let height = viewport.height;
        session
            .windows
            .iter()
            .enumerate()
            .map(|(index, window)| {
                let rect = window.rect();
                let shown = session.layout.pose(index, rect, viewport, session.selected);
                let pose = if session.phase == Phase::Leaving {
                    let from = session.leave_from.get(window).copied().unwrap_or(shown);
                    let target = if session.shown_after.contains(window) {
                        real_pose(rect, viewport, session.ranks_after.get(window).copied().unwrap_or(0))
                    } else {
                        Pose {
                            z: from.z - height * 0.15,
                            opacity: 0.0,
                            ..from
                        }
                    };
                    lerp_pose(from, target, eased)
                } else {
                    let from = session.start_poses.get(window).copied().flatten().unwrap_or(Pose {
                        z: shown.z - height * 0.3,
                        opacity: 0.0,
                        ..shown
                    });
                    match (session.phase, session.move_from.get(window)) {
                        (Phase::Entering, _) => lerp_pose(from, shown, eased),
                        (Phase::Shown, Some(moving_from)) => lerp_pose(*moving_from, shown, moved),
                        _ => shown,
                    }
                };
                let alpha = session.card_alpha.get(window).copied().unwrap_or(1.0);
                Placed {
                    window: *window,
                    rect,
                    pose: Pose {
                        opacity: pose.opacity * alpha,
                        ..pose
                    },
                }
            })
            .collect()
    }

    /// How opaque a card wants to be: dim when minimized.
    fn card_alpha_target(&self, window: Window) -> f64 {
        if window.state(&WINDOW_STATE_MINIMIZED).get_untracked() {
            self.0.layout.minimized_opacity()
        } else {
            1.0
        }
    }

    /// Move the cards' opacities towards their targets; true while one moves.
    fn step_card_alphas(&self, session: &mut Session, dt_ms: f64) -> bool {
        let mut moving = false;
        for window in session.windows.clone() {
            let target = self.card_alpha_target(window);
            let now = session.card_alpha.get(&window).copied().unwrap_or(target);
            let step = dt_ms / CARD_FADE_MS;
            let next = if now < target { (now + step).min(target) } else { (now - step).max(target) };
            session.card_alpha.insert(window, next);
            moving |= next != target;
        }
        moving
    }

    fn dim_alpha(&self, session: &Session) -> f64 {
        let eased = WINDOW_MANAGEMENT_EASING.apply(session.progress);
        let dim = self.0.layout.dim_alpha();
        match session.phase {
            Phase::Entering => dim * eased,
            Phase::Shown => dim,
            Phase::Leaving => dim * (1.0 - eased),
        }
    }

    fn ensure_ticking(&self, session: &SessionRef) {
        if session.borrow().poll.is_some() {
            return;
        }
        let this = self.clone();
        let ticking = session.clone();
        let output = session.borrow().output.clone();
        let poll = create_poll(1.0, output, move |handle| this.tick(&ticking, handle.now_ms()));
        let mut session = session.borrow_mut();
        session.last_tick_ms = None;
        session.poll = Some(poll);
    }

    fn stop_ticking(&self, session: &SessionRef) {
        if let Some(poll) = session.borrow_mut().poll.take() {
            poll.cancel();
        }
    }

    fn tick(&self, session: &SessionRef, now_ms: f64) {
        if !self.is_current(session) {
            self.stop_ticking(session);
            return;
        }
        let (relaid_out, dt) = {
            let mut s = session.borrow_mut();
            let dt = s.last_tick_ms.map_or(0.0, |last| now_ms - last);
            s.last_tick_ms = Some(now_ms);
            let mut relaid_out = false;
            if s.move_progress < 1.0 {
                s.move_progress = (s.move_progress + dt / RELAYOUT_MS).min(1.0);
                relaid_out = s.move_progress >= 1.0;
            }
            (relaid_out, dt)
        };
        if relaid_out {
            self.relayout_done(session);
            if !self.is_current(session) {
                return;
            }
        }
        let (hand_off, moving) = {
            let mut s = session.borrow_mut();
            if s.phase != Phase::Shown {
                let start = *s.phase_start_ms.get_or_insert(now_ms);
                s.progress = ((now_ms - start) / TRANSITION_MS).min(1.0);
            }
            let selected = s.selected;
            let mut moving = s.layout.tick(selected, dt);
            moving |= s.move_progress < 1.0 || relaid_out;
            moving |= self.step_card_alphas(&mut s, dt);

            if s.phase == Phase::Entering && s.progress >= 1.0 {
                s.phase = Phase::Shown;
                s.progress = 1.0;
            }
            (s.phase == Phase::Leaving && self.ready_to_hand_off(&s), moving)
        };
        if hand_off {
            self.finish(session);
            return;
        }
        self.touch();
        if session.borrow().phase == Phase::Shown && !moving {
            self.stop_ticking(session);
        }
    }

    /// Every window is (visually) where the real desktop will show it.
    fn ready_to_hand_off(&self, session: &Session) -> bool {
        if session.progress >= 1.0 {
            return true;
        }
        let Some(viewport) = viewport_of(&session.output) else {
            return true;
        };
        if self.dim_alpha(session) > 0.01 {
            return false;
        }
        self.poses(session, &viewport).iter().all(|placed| {
            if !session.shown_after.contains(&placed.window) {
                return placed.pose.opacity < 0.02;
            }
            let rect = placed.rect;
            let target = real_pose(
                rect,
                &viewport,
                session.ranks_after.get(&placed.window).copied().unwrap_or(0),
            );
            let pose = placed.pose;
            let half_extent = rect.width.max(rect.height) / 2.0 + MARGIN;
            let deviation = (pose.x - target.x).abs()
                + (pose.y - target.y).abs()
                + (pose.z - target.z).abs()
                + (pose.rotate_x.abs() + pose.rotate_y.abs()).to_radians() * half_extent
                + (pose.scale - target.scale).abs() * half_extent;
            deviation < HANDOFF_EPSILON_PX
        })
    }

    /// Open the switcher on `output_name` (no-op while open).
    pub fn open(&self, output_name: &str) {
        if self.current().is_some() {
            return;
        }
        let windows = self.0.wm.read(|wm| wm.list_windows_by_recent_use());
        let Some(viewport) = viewport_of(output_name) else {
            return;
        };
        if windows.is_empty() {
            return;
        }
        let ranks = self.rank_map(&windows);
        let start_poses = windows
            .iter()
            .map(|window| {
                let shown = self.0.wm.read(|wm| wm.is_window_shown_on(*window, output_name));
                let pose = shown
                    .then(|| real_pose(window.rect(), &viewport, ranks.get(window).copied().unwrap_or(0)));
                (*window, pose)
            })
            .collect();
        let card_alpha = windows
            .iter()
            .map(|window| (*window, self.card_alpha_target(*window)))
            .collect();
        let session = Rc::new(RefCell::new(Session {
            output: output_name.to_owned(),
            selected: self.0.layout.initial_selection(windows.len()),
            has_selection: true,
            layout: self.0.layout.begin(&windows),
            move_from: HashMap::new(),
            move_progress: 1.0,
            departing: Vec::new(),
            card_alpha,
            close_requested: Vec::new(),
            windows,
            start_poses,
            ranks_after: ranks,
            leave_from: HashMap::new(),
            shown_after: HashSet::new(),
            phase: Phase::Entering,
            progress: 0.0,
            phase_start_ms: None,
            last_tick_ms: None,
            grab: None,
            poll: None,
        }));
        if let Some(fade) = self.0.blur_fade.borrow_mut().take() {
            fade.cancel();
        }
        self.0.backdrop_strength.set(0.0);
        self.0.blur_suspended.set(true);
        let grab = COMPOSITOR.input.grab(self.grab_options(&session));
        session.borrow_mut().grab = Some(grab);
        self.set_current(Some(session.clone()));
        let selected = selected_window(&session.borrow()).map(|window| window.id());
        self.0.selected_window_id.set(selected);
        self.ensure_ticking(&session);
    }

    fn grab_options(&self, session: &SessionRef) -> InputGrabOptions {
        let on_cancel = {
            let (this, session) = (self.clone(), session.clone());
            move |_: &InputGrabCancelReason| {
                // The screen locked or a handler failed: drop the switcher at once.
                session.borrow_mut().grab = None;
                this.finish(&session);
            }
        };
        InputGrabOptions::new()
            .on_key(self.handler(session, Self::on_key))
            .on_pointer_motion(self.handler(session, Self::on_pointer_motion))
            .on_pointer_button(self.handler(session, Self::on_pointer_button))
            .on_scroll(self.handler(session, Self::on_scroll))
            .on_swipe(self.handler(session, Self::on_swipe))
            .on_cancel(on_cancel)
    }

    /// A grab handler that runs `f` on this session.
    fn handler<E: 'static>(&self, session: &SessionRef, f: fn(&Self, &SessionRef, &E)) -> impl Fn(&E) + 'static {
        let (this, session) = (self.clone(), session.clone());
        move |event| f(&this, &session, event)
    }

    fn on_key(&self, session: &SessionRef, event: &InputGrabKeyEvent) {
        if event.state == InputGrabState::Released {
            if event.key == "Super_L" || event.key == "Super_R" {
                let action = {
                    let mut s = session.borrow_mut();
                    let selected = s.selected;
                    s.layout.on_super_released(selected)
                };
                self.apply(session, action);
            }
            return;
        }
        let handled = {
            let mut s = session.borrow_mut();
            let selected = s.selected;
            s.layout.on_key(event, selected)
        };
        let action = handled.unwrap_or(match event.key.as_str() {
            "Return" | "KP_Enter" | "space" => SwitcherAction::Commit,
            "Escape" => SwitcherAction::Cancel,
            _ => SwitcherAction::None,
        });
        self.apply(session, action);
    }

    fn on_pointer_motion(&self, session: &SessionRef, event: &InputGrabPointerMotionEvent) {
        let hit = self.window_at(session, event.position.x, event.position.y);
        let action = {
            let mut s = session.borrow_mut();
            let hit = hit.and_then(|window| s.windows.iter().position(|other| *other == window));
            let selected = s.selected;
            s.layout.on_pointer_motion(hit, selected)
        };
        self.apply(session, action);
    }

    fn on_pointer_button(&self, session: &SessionRef, event: &InputGrabPointerButtonEvent) {
        if event.state != InputGrabState::Pressed {
            return;
        }
        let card_actions = self.0.layout.card_actions();
        let hit = self.card_at(session, event.position.x, event.position.y);
        match event.button_name.as_deref() {
            // KWin: a middle click closes the window.
            Some("middle") => {
                if let (Some((window, _, _)), true) = (hit, card_actions) {
                    self.request_close(session, window);
                }
                return;
            }
            Some("left") => {}
            _ => return,
        }
        // Only the selected card's title bar buttons work.
        if let Some((window, x, y)) = hit {
            let selected = {
                let s = session.borrow();
                s.phase == Phase::Shown && selected_window(&s) == Some(window)
            };
            let titlebar_button_at = self.0.titlebar_button_at.borrow().clone();
            if card_actions && selected {
                if let Some(button) = titlebar_button_at.and_then(|at| at(window, x, y)) {
                    self.press_card_button(session, window, button);
                    return;
                }
            }
        }
        if let Some((window, _, _)) = hit {
            self.close(session, Some(window));
        } else {
            let action = session.borrow_mut().layout.on_click_outside();
            self.apply(session, action);
        }
    }

    fn on_scroll(&self, session: &SessionRef, event: &InputGrabScrollEvent) {
        let action = {
            let mut s = session.borrow_mut();
            let selected = s.selected;
            s.layout.on_scroll(event, selected)
        };
        self.apply(session, action);
    }

    fn on_swipe(&self, session: &SessionRef, event: &GestureSwipeEventSnapshot) {
        let action = {
            let mut s = session.borrow_mut();
            let selected = s.selected;
            s.layout.on_swipe(event, selected)
        };
        self.apply(session, action);
    }

    fn apply(&self, session: &SessionRef, action: SwitcherAction) {
        match action {
            SwitcherAction::None => {}
            SwitcherAction::Select(index) => self.select(session, index),
            SwitcherAction::Commit => self.commit(session),
            SwitcherAction::Cancel => self.close(session, None),
        }
    }

    /// A title bar button of a card.
    fn press_card_button(&self, session: &SessionRef, window: Window, button: TitlebarButton) {
        match button {
            // The card leaves once the window really closes (it may ask first).
            TitlebarButton::Close => self.request_close(session, window),
            TitlebarButton::Maximize => {
                if !window.is_resizable().get_untracked() {
                    return;
                }
                self.close(session, Some(window));
                if window.is_maximized().get_untracked() {
                    window.unmaximize();
                } else {
                    window.maximize();
                }
            }
            // The card stays (dimmed with `minimized_opacity`); a click restores it.
            TitlebarButton::Minimize => {
                if !window.state(&WINDOW_STATE_MINIMIZED).get_untracked() {
                    window.minimize();
                    self.ensure_ticking(session);
                }
            }
        }
    }

    /// Ask a card's window to close; its card leaves when it really closes.
    fn request_close(&self, session: &SessionRef, window: Window) {
        {
            let mut s = session.borrow_mut();
            if !s.close_requested.contains(&window) {
                s.close_requested.push(window);
            }
        }
        window.close();
    }

    /// A window opened while a card's window was asked to close: most likely
    /// its "save changes?" dialog, which the switcher would cover. Switch to
    /// the window that asks, so the dialog shows.
    fn window_opened(&self, session: &SessionRef, opened: Window) {
        let target = {
            let s = session.borrow();
            let Some(last) = s.close_requested.last().copied() else {
                return;
            };
            let parent = opened.parent_id().get_untracked();
            let owner = s
                .close_requested
                .iter()
                .copied()
                .find(|window| parent.as_deref() == Some(window.id().as_str()))
                .unwrap_or(last);
            s.windows.contains(&owner).then_some(owner)
        };
        self.close(session, target);
    }

    /// A window closed: its card fades out and the others make room.
    fn remove_card(&self, session: &SessionRef, window: Window) {
        let output = session.borrow().output.clone();
        let Some(viewport) = viewport_of(&output) else {
            return;
        };
        {
            let mut s = session.borrow_mut();
            let Some(index) = s.windows.iter().position(|other| *other == window) else {
                return;
            };
            let poses = self.poses(&s, &viewport);
            s.move_from.clear();
            for placed in poses {
                if placed.window == window {
                    s.departing.push(Departing {
                        window,
                        rect: placed.rect,
                        from: placed.pose,
                    });
                } else {
                    s.move_from.insert(placed.window, placed.pose);
                }
            }
            s.windows.remove(index);
            s.card_alpha.remove(&window);
            s.close_requested.retain(|other| *other != window);
            s.has_selection = false;
            let windows = s.windows.clone();
            s.layout.windows_changed(&windows);
            s.move_progress = 0.0;
        }
        self.0.selected_window_id.set(None);
        self.ensure_ticking(session);
        self.touch();
    }

    /// The cards are in their new places: select the one under the pointer.
    fn relayout_done(&self, session: &SessionRef) {
        let (empty, has_selection) = {
            let mut s = session.borrow_mut();
            s.move_from.clear();
            s.departing.clear();
            (s.windows.is_empty(), s.has_selection)
        };
        if empty {
            self.close(session, None);
            return;
        }
        if has_selection {
            return; // A key or the pointer picked the next window already.
        }
        let Some(cursor) = COMPOSITOR.cursor.get_position() else {
            return;
        };
        if let Some(window) = self.window_at(session, cursor.x, cursor.y) {
            let index = session.borrow().windows.iter().position(|other| *other == window);
            if let Some(index) = index {
                self.select(session, index as i64);
            }
        }
    }

    fn select(&self, session: &SessionRef, index: i64) {
        if !self.is_current(session) {
            return;
        }
        let selected = {
            let mut s = session.borrow_mut();
            if s.phase == Phase::Leaving || (s.has_selection && s.selected == index) {
                return;
            }
            s.selected = index;
            s.has_selection = true;
            selected_window(&s).map(|window| window.id())
        };
        self.0.selected_window_id.set(selected);
        self.ensure_ticking(session);
    }

    fn commit(&self, session: &SessionRef) {
        let selected = selected_window(&session.borrow());
        self.close(session, selected);
    }

    /// Close the switcher, switching to `target` (`None`: back to the desktop as it was).
    fn close(&self, session: &SessionRef, target: Option<Window>) {
        if !self.is_current(session) || session.borrow().phase == Phase::Leaving {
            return;
        }
        let grab = session.borrow_mut().grab.take();
        if let Some(grab) = grab {
            grab.release();
        }
        let output_name = session.borrow().output.clone();
        let Some(viewport) = viewport_of(&output_name) else {
            self.finish(session);
            return;
        };
        // Freeze where everything is, then switch for real.
        let poses = self.poses(&session.borrow(), &viewport);
        {
            let mut s = session.borrow_mut();
            for placed in poses {
                s.leave_from.insert(placed.window, placed.pose);
            }
            let selected = s.selected;
            s.layout.settle(selected);
        }
        if let Some(target) = target {
            let id = target.id();
            self.0.wm.with(|wm| wm.activate_window_by_id_instant(&id));
        }
        let windows = session.borrow().windows.clone();
        let shown: HashSet<Window> = windows
            .iter()
            .copied()
            .filter(|window| self.0.wm.read(|wm| wm.is_window_shown_on(*window, &output_name)))
            .collect();
        let mut ranks = self.rank_map(&windows);
        if let Some(target) = target {
            // Activation puts it on top; its focus may land a moment later.
            ranks.insert(target, windows.len());
        }
        {
            let mut s = session.borrow_mut();
            s.shown_after.extend(shown);
            s.ranks_after = ranks;
            s.phase = Phase::Leaving;
            s.progress = 0.0;
            s.phase_start_ms = None;
        }
        self.0.selected_window_id.set(None);
        self.ensure_ticking(session);
        self.touch();
    }

    /// Back to the real desktop, and fade window blur in.
    fn finish(&self, session: &SessionRef) {
        self.stop_ticking(session);
        let grab = session.borrow_mut().grab.take();
        if let Some(grab) = grab {
            grab.release();
        }
        if !self.is_current(session) {
            return;
        }
        self.0.backdrop_strength.set(0.0);
        self.0.blur_suspended.set(false);
        self.set_current(None);
        self.0.selected_window_id.set(None);
        let fade_start = std::cell::Cell::new(None::<f64>);
        let strength = self.0.backdrop_strength;
        let this = self.clone();
        let output = session.borrow().output.clone();
        let fade = create_poll(1.0, output, move |poll| {
            let now = poll.now_ms();
            let start = fade_start.get().unwrap_or(now);
            fade_start.set(Some(start));
            let linear = ((now - start) / BLUR_FADE_MS).min(1.0);
            strength.set(ease_out_cubic(linear));
            if linear >= 1.0 {
                poll.cancel();
                let mut slot = this.0.blur_fade.borrow_mut();
                if *slot == Some(*poll) {
                    *slot = None;
                }
            }
        });
        *self.0.blur_fade.borrow_mut() = Some(fade);
    }

    fn window_at(&self, session: &SessionRef, global_x: f64, global_y: f64) -> Option<Window> {
        let session = session.borrow();
        let viewport = viewport_of(&session.output)?;
        let placed = self.visible_planes(&session, &viewport);
        let planes: Vec<PickablePlane> = placed
            .iter()
            .map(|plane| PickablePlane {
                width: plane.width,
                height: plane.height,
                transform: plane.transform.matrix,
            })
            .collect();
        let hit = pick_plane(
            &switcher_camera(viewport.width, viewport.height),
            (viewport.width, viewport.height),
            &planes,
            global_x - viewport.output.position.x as f64,
            global_y - viewport.output.position.y as f64,
        )?;
        Some(placed[hit].window)
    }

    /// The card under a global point, with the point in its window's own
    /// logical px (from the top-left corner of `window.rect()`, unscaled).
    fn card_at(&self, session: &SessionRef, global_x: f64, global_y: f64) -> Option<(Window, f64, f64)> {
        let window = self.window_at(session, global_x, global_y)?;
        let session = session.borrow();
        let viewport = viewport_of(&session.output)?;
        let placed = self
            .visible_planes(&session, &viewport)
            .into_iter()
            .find(|plane| plane.window == window)?;
        let camera = switcher_camera(viewport.width, viewport.height);
        let matrix = placed.transform.matrix;
        // The plane's corners on screen: top-left, top-right, bottom-left.
        let corner = |x: f64, y: f64| {
            let world = [
                matrix[0] * x + matrix[4] * y + matrix[12],
                matrix[1] * x + matrix[5] * y + matrix[13],
                matrix[2] * x + matrix[6] * y + matrix[14],
            ];
            project_point(&camera, (viewport.width, viewport.height), world)
        };
        let (half_width, half_height) = (placed.width / 2.0, placed.height / 2.0);
        let origin = corner(-half_width, half_height)?;
        let right = corner(half_width, half_height)?;
        let down = corner(-half_width, -half_height)?;
        // Solve point = origin + u * (right - origin) + v * (down - origin).
        let (ax, ay) = (right.x - origin.x, right.y - origin.y);
        let (bx, by) = (down.x - origin.x, down.y - origin.y);
        let px = global_x - viewport.output.position.x as f64 - origin.x;
        let py = global_y - viewport.output.position.y as f64 - origin.y;
        let det = ax * by - ay * bx;
        if det.abs() < 1e-6 {
            return None;
        }
        let u = (px * by - py * bx) / det;
        let v = (ax * py - ay * px) / det;
        Some((window, u * placed.width - MARGIN, v * placed.height - MARGIN))
    }

    /// Cards whose window closed, fading out where they were.
    fn departing_planes(&self, session: &Session) -> Vec<PlacedPlane> {
        let eased = WINDOW_MANAGEMENT_EASING.apply(session.move_progress);
        session
            .departing
            .iter()
            .map(|departing| {
                let to = Pose {
                    scale: departing.from.scale * 0.85,
                    opacity: 0.0,
                    ..departing.from
                };
                plane_for(departing.window, departing.rect, lerp_pose(departing.from, to, eased))
            })
            .collect()
    }

    fn visible_planes(&self, session: &Session, viewport: &Viewport) -> Vec<PlacedPlane> {
        let alive: HashSet<Window> = self.0.wm.read(|wm| wm.list_windows()).into_iter().collect();
        self.poses(session, viewport)
            .into_iter()
            .filter(|placed| alive.contains(&placed.window) && placed.pose.opacity > 0.001)
            .map(|Placed { window, rect, pose }| plane_for(window, rect, pose))
            .collect()
    }

    /// The switcher's composition for `output` while open, else `None`.
    pub fn compose(&self, output: &WaylandOutputSnapshot) -> Option<OutputStack> {
        self.0.revision.get();
        let session = self.current()?;
        let session = session.borrow();
        if session.output != output.name {
            return None;
        }
        let (width, height) = output_logical_size(output);
        let viewport = Viewport {
            output: output.clone(),
            width,
            height,
        };
        let mut planes = self.departing_planes(&session);
        planes.extend(self.visible_planes(&session, &viewport));
        let name = self.0.layout.name();
        let scene = Scene3D::new(switcher_camera(width, height)).planes(planes.iter().map(|plane| {
            Plane::new(&window_texture(name, plane.window, plane.rect, output), plane.width, plane.height)
                .transform(plane.transform)
                .opacity(plane.pose.opacity)
        }));
        Some(
            OutputStack::new()
                .child(Layers::new([LayerName::Background, LayerName::Bottom]))
                .child(Solid::new([0.0, 0.0, 0.0, self.dim_alpha(&session)]))
                .child(scene)
                .child(Layers::new([LayerName::Top, LayerName::Overlay]))
                .child(LayerPopups),
        )
    }
}

fn plane_for(window: Window, rect: Rect, pose: Pose) -> PlacedPlane {
    PlacedPlane {
        window,
        rect,
        pose,
        width: rect.width + MARGIN * 2.0,
        height: rect.height + MARGIN * 2.0,
        transform: transform3d()
            .translate(pose.x, pose.y, pose.z)
            .rotate_x(pose.rotate_x)
            .rotate_y(pose.rotate_y)
            .scale(pose.scale),
    }
}

fn selected_window(session: &Session) -> Option<Window> {
    let count = session.windows.len() as i64;
    if count == 0 || !session.has_selection {
        return None;
    }
    session.windows.get(session.selected.rem_euclid(count) as usize).copied()
}

fn window_texture(name: &str, window: Window, rect: Rect, output: &WaylandOutputSnapshot) -> RenderTexture {
    RenderTexture::new()
        .key(format!("{name}-{}", window.id()))
        .size(rect.width + MARGIN * 2.0, rect.height + MARGIN * 2.0)
        .child(Windows::only([window]).offset(
            output.position.x as f64 - rect.x + MARGIN,
            output.position.y as f64 - rect.y + MARGIN,
        ))
}

fn viewport_of(name: &str) -> Option<Viewport> {
    let output = COMPOSITOR.output.get(name)?;
    let (width, height) = output_logical_size(&output);
    Some(Viewport { output, width, height })
}

/// Distance at which the camera sees the plane z = 0 1:1 in logical pixels.
pub fn camera_distance(height: f64) -> f64 {
    height / 2.0 / (CAMERA_FOV_DEGREES.to_radians() / 2.0).tan()
}

/// Like `screen_camera` with a narrow field of view, but with room for a
/// layout that recedes several times the camera distance.
fn switcher_camera(width: f64, height: f64) -> Camera {
    let distance = camera_distance(height);
    Camera {
        projection: perspective(CAMERA_FOV_DEGREES, width / height.max(1.0), distance / 100.0, distance * 6.0),
        view: look_at([0.0, 0.0, distance], [0.0, 0.0, 0.0]),
    }
}

/// A flat pose on the screen plane: the window centred on `(center_x,
/// center_y)` (logical px from the viewport's top-left corner), `scale` times
/// its size.
pub fn flat_pose(center_x: f64, center_y: f64, scale: f64, viewport: &Viewport) -> Pose {
    Pose {
        x: center_x - viewport.width / 2.0,
        y: viewport.height / 2.0 - center_y,
        z: 0.0,
        rotate_x: 0.0,
        rotate_y: 0.0,
        scale,
        opacity: 1.0,
    }
}

/// The window exactly where it is on screen. The camera maps z = 0 1:1; a
/// window `rank` steps up the stack sits that much nearer the camera, shrunk
/// by the perspective it gains so it covers the same pixels.
fn real_pose(rect: Rect, viewport: &Viewport, rank: usize) -> Pose {
    let Viewport { output, width, height } = viewport;
    let center_x = rect.x - output.position.x as f64 + rect.width / 2.0;
    let center_y = rect.y - output.position.y as f64 + rect.height / 2.0;
    let distance = camera_distance(*height);
    let z = rank as f64 * STACK_DEPTH_STEP;
    let shrink = (distance - z) / distance;
    Pose {
        x: (center_x - width / 2.0) * shrink,
        y: (height / 2.0 - center_y) * shrink,
        z,
        rotate_x: 0.0,
        rotate_y: 0.0,
        scale: shrink,
        opacity: 1.0,
    }
}

fn lerp(from: f64, to: f64, t: f64) -> f64 {
    from + (to - from) * t
}

fn lerp_pose(from: Pose, to: Pose, t: f64) -> Pose {
    Pose {
        x: lerp(from.x, to.x, t),
        y: lerp(from.y, to.y, t),
        z: lerp(from.z, to.z, t),
        rotate_x: lerp(from.rotate_x, to.rotate_x, t),
        rotate_y: lerp(from.rotate_y, to.rotate_y, t),
        scale: lerp(from.scale, to.scale, t),
        opacity: lerp(from.opacity, to.opacity, t),
    }
}

pub fn clamp01(value: f64) -> f64 {
    value.clamp(0.0, 1.0)
}

fn ease_out_cubic(t: f64) -> f64 {
    1.0 - (1.0 - t).powi(3)
}
