//! Windows as the config sees them: a `Copy` handle with reactive fields,
//! per-window state and actions (the TypeScript `WaylandWindow`).

use std::{any::TypeId, fmt, rc::Rc};

use shojiwm_lib::protocols::plasma_shell::PlasmaSurfaceSnapshot;
use shojiwm_lib::ssd::{
    DecorationInteractionSnapshot, ManagedWindowAnimationMode, ManagedWindowAnimationSnapshot,
    ManagedWindowPointAnimationSnapshot, ManagedWindowPointSnapshot,
    ManagedWindowRectAnimationSnapshot, ManagedWindowScalarAnimationSnapshot, RuntimeWindowAction,
    WaylandWindowAction, WaylandWindowSnapshot, WindowDecorationStateSnapshot, WindowIconSnapshot,
    window_model::WindowSizeConstraintsSnapshot,
};

use crate::{
    animation::Easing,
    reactive::{ReadSignal, Scope, Signal, untrack},
    runtime::{self, WindowEntry},
    view::Rect,
};

/// The reactive fields of a window, mirrored from its snapshots.
#[derive(Clone, Copy)]
pub(crate) struct WindowSignals {
    pub title: Signal<String>,
    pub app_id: Signal<Option<String>>,
    pub position: Signal<Rect>,
    pub snapshot_rect: Signal<Rect>,
    pub is_focused: Signal<bool>,
    pub is_floating: Signal<bool>,
    pub is_maximized: Signal<bool>,
    pub is_fullscreen: Signal<bool>,
    pub is_xwayland: Signal<bool>,
    pub size_constraints: Signal<WindowSizeConstraintsSnapshot>,
    pub is_resizable: Signal<bool>,
    pub is_transient: Signal<bool>,
    pub parent_id: Signal<Option<String>>,
    pub plasma: Signal<Option<PlasmaSurfaceSnapshot>>,
    pub minimized_geometry: Signal<Option<Rect>>,
    pub icon: Signal<Option<WindowIconSnapshot>>,
    pub interaction: Signal<DecorationInteractionSnapshot>,
    pub decoration: Signal<WindowDecorationStateSnapshot>,
    pub output: Signal<Option<String>>,
}

impl WindowSignals {
    pub fn new(scope: Scope, snapshot: &WaylandWindowSnapshot) -> Self {
        Self {
            title: scope.signal(snapshot.title.clone()),
            app_id: scope.signal(snapshot.app_id.clone()),
            position: scope.signal(snapshot.position.into()),
            snapshot_rect: scope.signal(snapshot.rect.into()),
            is_focused: scope.signal(snapshot.is_focused),
            is_floating: scope.signal(snapshot.is_floating),
            is_maximized: scope.signal(snapshot.is_maximized),
            is_fullscreen: scope.signal(snapshot.is_fullscreen),
            is_xwayland: scope.signal(snapshot.is_xwayland),
            size_constraints: scope.signal(snapshot.size_constraints),
            is_resizable: scope.signal(snapshot.is_resizable),
            is_transient: scope.signal(snapshot.is_transient),
            parent_id: scope.signal(snapshot.parent_id.clone()),
            plasma: scope.signal(snapshot.plasma.clone()),
            minimized_geometry: scope.signal(snapshot.minimized_geometry.map(Into::into)),
            icon: scope.signal(snapshot.icon.clone()),
            interaction: scope.signal(snapshot.interaction.clone()),
            decoration: scope.signal(snapshot.decoration),
            output: scope.signal(snapshot.output_name.clone()),
        }
    }

    /// Write every field; unchanged ones notify nobody.
    pub fn update(&self, snapshot: &WaylandWindowSnapshot) {
        self.title.set(snapshot.title.clone());
        self.app_id.set(snapshot.app_id.clone());
        self.position.set(snapshot.position.into());
        self.snapshot_rect.set(snapshot.rect.into());
        self.is_focused.set(snapshot.is_focused);
        self.is_floating.set(snapshot.is_floating);
        self.is_maximized.set(snapshot.is_maximized);
        self.is_fullscreen.set(snapshot.is_fullscreen);
        self.is_xwayland.set(snapshot.is_xwayland);
        self.size_constraints.set(snapshot.size_constraints);
        self.is_resizable.set(snapshot.is_resizable);
        self.is_transient.set(snapshot.is_transient);
        self.parent_id.set(snapshot.parent_id.clone());
        self.plasma.set(snapshot.plasma.clone());
        self.minimized_geometry.set(snapshot.minimized_geometry.map(Into::into));
        self.icon.set(snapshot.icon.clone());
        self.interaction.set(snapshot.interaction.clone());
        self.decoration.set(snapshot.decoration);
        self.output.set(snapshot.output_name.clone());
    }
}

/// A handle to a window. Cheap to copy; once the window is gone, reads
/// return defaults and actions do nothing.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Window {
    pub(crate) slot: u32,
    pub(crate) generation: u32,
}

impl fmt::Debug for Window {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.entry() {
            Some(entry) => write!(f, "Window({})", entry.id),
            None => write!(f, "Window(<closed>)"),
        }
    }
}

macro_rules! window_signals {
    ($($(#[$doc:meta])* $name:ident: $ty:ty),* $(,)?) => {
        impl Window {
            $(
                $(#[$doc])*
                pub fn $name(&self) -> ReadSignal<$ty> {
                    self.signals().$name.read_only()
                }
            )*
        }
    };
}

window_signals! {
    title: String,
    app_id: Option<String>,
    /// Client geometry in global logical coordinates.
    position: Rect,
    is_focused: bool,
    is_floating: bool,
    is_maximized: bool,
    is_fullscreen: bool,
    is_xwayland: bool,
    size_constraints: WindowSizeConstraintsSnapshot,
    is_resizable: bool,
    is_transient: bool,
    parent_id: Option<String>,
    /// What plasmashell asked for this window (KDE `org_kde_plasma_surface`:
    /// role, position); `None` for every other window.
    plasma: Option<PlasmaSurfaceSnapshot>,
    /// Where a KDE taskbar shows this window, in global logical coordinates
    /// (`org_kde_plasma_window.set_minimized_geometry`); `None` when no
    /// taskbar set one.
    minimized_geometry: Option<Rect>,
    icon: Option<WindowIconSnapshot>,
    interaction: DecorationInteractionSnapshot,
    decoration: WindowDecorationStateSnapshot,
    /// The output the window belongs to: the one containing its center, else
    /// one it overlaps, else the nearest (`window.output`).
    output: Option<String>,
}

impl Window {
    /// [`create_poll`](crate::animation::create_poll) on the frames of the
    /// window's output (`window.createPoll`); it moves with the window.
    pub fn create_poll(
        &self,
        interval_ms: f64,
        f: impl Fn(&crate::animation::PollHandle) + 'static,
    ) -> crate::animation::PollHandle {
        crate::animation::create_poll(interval_ms, self.output(), f)
    }

    pub(crate) fn entry(&self) -> Option<Rc<WindowEntry>> {
        runtime::WINDOWS.with(|windows| windows.borrow().get(self.slot, self.generation))
    }

    pub(crate) fn signals(&self) -> WindowSignals {
        match self.entry() {
            Some(entry) => entry.signals,
            None => dead_signals(),
        }
    }

    /// Draw this window's decoration at `scale` times its authored size:
    /// every length in its view (sizes, offsets, spacing, borders, radii,
    /// shadows, fonts, popup offsets) is multiplied, so text stays sharp and
    /// hit testing follows. A label without a font size or line height gets
    /// the defaults' scaled values. Shader uniforms are not touched, nor is
    /// the window's rect: a config that reserves room for its decoration
    /// (a titlebar height in the managed rect) scales that itself. The
    /// default is 1; a change rebuilds the whole view.
    pub fn set_ui_scale(&self, scale: f64) {
        let Some(entry) = self.entry() else {
            return;
        };
        let scale = if scale.is_finite() && scale > 0.0 { scale } else { 1.0 };
        if entry.context.ui_scale.replace(scale) != scale {
            entry.context.mark_full();
        }
    }

    /// Whether the window still exists.
    pub fn is_alive(&self) -> bool {
        self.entry().is_some()
    }

    pub fn id(&self) -> String {
        self.entry()
            .map(|entry| entry.id.to_string())
            .unwrap_or_default()
    }

    /// The window's rect: the managed rect when the composition set one,
    /// otherwise the compositor's. Not tracked.
    pub fn rect(&self) -> Rect {
        let Some(entry) = self.entry() else {
            return Rect::default();
        };
        if let Some(rect) = entry.managed_state.borrow().rect {
            return rect.into();
        }
        entry.signals.snapshot_rect.get_untracked()
    }

    /// The last snapshot the compositor sent.
    pub fn snapshot(&self) -> Option<WaylandWindowSnapshot> {
        self.entry().map(|entry| entry.snapshot.borrow().clone())
    }

    /// Per-window state for `key` (`window.state[key]`), created on first
    /// use from the key's default. Lives until the window closes.
    pub fn state<T: 'static>(&self, key: &WindowStateKey<T>) -> Signal<T> {
        let Some(entry) = self.entry() else {
            // A closed window: hand out a detached signal so callers need no
            // special case.
            return untrack(|| Scope::root().signal((key.default)(*self)));
        };
        let slot = (key.name, TypeId::of::<T>());
        if let Some(existing) = entry.state.borrow().get(&slot) {
            return *existing
                .downcast_ref::<Signal<T>>()
                .expect("window state type is keyed by TypeId");
        }
        let initial = untrack(|| (key.default)(*self));
        let signal = entry.scope.signal(initial);
        entry.state.borrow_mut().insert(slot, Rc::new(signal));
        signal
    }

    fn act(&self, action: WaylandWindowAction) {
        if let Some(entry) = self.entry() {
            runtime::push_action(RuntimeWindowAction {
                window_id: entry.id.to_string(),
                action,
                animation: None,
                channel: None,
            });
        }
    }

    pub fn close(&self) {
        self.act(WaylandWindowAction::Close);
    }

    pub fn maximize(&self) {
        self.act(WaylandWindowAction::Maximize);
    }

    pub fn unmaximize(&self) {
        self.act(WaylandWindowAction::Unmaximize);
    }

    pub fn minimize(&self) {
        self.act(WaylandWindowAction::Minimize);
    }

    pub fn fullscreen(&self) {
        self.act(WaylandWindowAction::Fullscreen);
    }

    pub fn unfullscreen(&self) {
        self.act(WaylandWindowAction::Unfullscreen);
    }

    pub fn focus(&self) {
        self.act(WaylandWindowAction::Focus);
    }

    /// Let the compositor animate the managed rect / offset / opacity.
    pub fn schedule_animation(&self, animation: ManagedAnimation) {
        if let Some(entry) = self.entry() {
            runtime::push_action(RuntimeWindowAction {
                window_id: entry.id.to_string(),
                action: WaylandWindowAction::ScheduleAnimation,
                animation: Some(animation.0),
                channel: None,
            });
        }
    }

    /// Cancel every animation the compositor runs on this window.
    pub fn cancel_all_animations(&self) {
        if let Some(entry) = self.entry() {
            runtime::push_action(RuntimeWindowAction {
                window_id: entry.id.to_string(),
                action: WaylandWindowAction::CancelAnimation,
                animation: None,
                channel: None,
            });
        }
    }

    pub fn cancel_animation(&self, channel: &str) {
        if let Some(entry) = self.entry() {
            runtime::push_action(RuntimeWindowAction {
                window_id: entry.id.to_string(),
                action: WaylandWindowAction::CancelAnimation,
                animation: None,
                channel: Some(channel.to_owned()),
            });
        }
    }

    /// How long the close animation takes, set from `on_start_close`. The
    /// compositor keeps the closing snapshot alive that long.
    pub fn set_close_animation_duration(&self, duration_ms: u64) {
        if let Some(entry) = self.entry() {
            entry.close_duration_ms.set(duration_ms);
        }
    }
}

fn empty_snapshot() -> WaylandWindowSnapshot {
    WaylandWindowSnapshot {
        output_name: None,
        plasma: None,
        minimized_geometry: None,
        id: String::new(),
        title: String::new(),
        app_id: None,
        position: Default::default(),
        rect: Default::default(),
        is_focused: false,
        is_floating: true,
        is_maximized: false,
        is_fullscreen: false,
        is_xwayland: false,
        decoration: Default::default(),
        size_constraints: Default::default(),
        is_resizable: false,
        is_transient: false,
        parent_id: None,
        icon: None,
        interaction: Default::default(),
    }
}

/// Signals standing in for a closed window, shared by every dead handle.
fn dead_signals() -> WindowSignals {
    thread_local! {
        static DEAD: std::cell::Cell<Option<WindowSignals>> = const { std::cell::Cell::new(None) };
    }
    DEAD.with(|slot| match slot.get() {
        Some(signals) if signals.title.is_alive() => signals,
        _ => {
            let signals = untrack(|| WindowSignals::new(Scope::root(), &empty_snapshot()));
            slot.set(Some(signals));
            signals
        }
    })
}

/// A typed per-window state slot (`createWindowState`). Declare it once:
///
/// ```
/// use shojiwm_rs::prelude::*;
///
/// static TILED: WindowStateKey<bool> = WindowStateKey::new("tiled", |_| false);
/// ```
pub struct WindowStateKey<T: 'static> {
    name: &'static str,
    default: fn(Window) -> T,
}

impl<T: 'static> WindowStateKey<T> {
    pub const fn new(name: &'static str, default: fn(Window) -> T) -> Self {
        Self { name, default }
    }

    pub fn name(&self) -> &'static str {
        self.name
    }
}

/// An animation the compositor runs on a managed window
/// (`window.scheduleAnimation`).
#[derive(Debug, Clone, PartialEq)]
pub struct ManagedAnimation(ManagedWindowAnimationSnapshot);

/// How an animated value combines with the static one.
pub use shojiwm_lib::ssd::ManagedWindowAnimationMode as AnimationMode;

impl ManagedAnimation {
    /// An animation on `channel`; a new one on the same channel replaces it.
    pub fn new(channel: impl Into<String>) -> Self {
        Self(ManagedWindowAnimationSnapshot {
            channel: channel.into(),
            rect: None,
            offset: None,
            opacity: None,
        })
    }

    pub fn rect(
        mut self,
        from: Option<Rect>,
        to: Rect,
        duration_ms: u64,
        easing: Easing,
        mode: ManagedWindowAnimationMode,
    ) -> Self {
        self.0.rect = Some(ManagedWindowRectAnimationSnapshot {
            from: from.map(Into::into),
            to: to.into(),
            duration: duration_ms,
            easing: easing.into(),
            mode,
        });
        self
    }

    pub fn offset(
        mut self,
        from: Option<(f64, f64)>,
        to: (f64, f64),
        duration_ms: u64,
        easing: Easing,
        mode: ManagedWindowAnimationMode,
    ) -> Self {
        self.0.offset = Some(ManagedWindowPointAnimationSnapshot {
            from: from.map(|(x, y)| ManagedWindowPointSnapshot { x, y }),
            to: ManagedWindowPointSnapshot { x: to.0, y: to.1 },
            duration: duration_ms,
            easing: easing.into(),
            mode,
        });
        self
    }

    pub fn opacity(
        mut self,
        from: Option<f64>,
        to: f64,
        duration_ms: u64,
        easing: Easing,
        mode: ManagedWindowAnimationMode,
    ) -> Self {
        self.0.opacity = Some(ManagedWindowScalarAnimationSnapshot {
            from,
            to,
            duration: duration_ms,
            easing: easing.into(),
            mode,
        });
        self
    }
}
