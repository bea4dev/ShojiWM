//! Port of `packages/config/src/window-manager.ts`: a hybrid window manager
//! with floating workspaces (Windows-style edge snapping) and scrollable
//! tiling workspaces, per-monitor dynamic workspaces and touchpad gestures.
//!
//! The TypeScript class mutates itself from event listeners. Here the state
//! lives in one `RefCell` behind [`WindowManager`]; every entry point goes
//! through [`WindowManager::with`], which also delivers what the manager
//! wants to tell the outside world (snap previews, workspace changes) once
//! the borrow is released, so those callbacks may freely call back in.

use std::{
    cell::RefCell,
    collections::{BTreeMap, HashMap, HashSet},
    rc::{Rc, Weak},
    time::{SystemTime, UNIX_EPOCH},
};

use shojiwm_rs::{
    prelude::*,
    ssd::{
        GestureSwipeEventSnapshot, GestureSwipePhaseSnapshot, PointerHitTargetSnapshot,
        PointerMovePointSnapshot, PointerMoveEventSnapshot, WindowActivateRequestEventSnapshot,
        WindowActivateRequestSourceSnapshot, WindowFullscreenRequestEventSnapshot,
        WindowMaximizeRequestEventSnapshot, WindowMinimizeRequestEventSnapshot,
        WindowMoveEventSnapshot, WindowMovePhaseSnapshot, WindowMoveSourceSnapshot,
        WindowResizeEventSnapshot, WindowResizePhaseSnapshot, WindowStateRequestSourceSnapshot,
    },
};

use crate::{
    window_animation::{play_rect_animation, stop_rect_animation},
    workspace::{AddWindowOptions, LayoutOptions, Workspace, WorkspaceTransition},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapZone {
    Maximize,
    Left,
    Right,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

impl SnapZone {
    /// A half or quarter (everything but maximize).
    fn is_layout(self) -> bool {
        self != Self::Maximize
    }

    fn is_left(self) -> bool {
        matches!(self, Self::Left | Self::TopLeft | Self::BottomLeft)
    }

    fn is_right(self) -> bool {
        matches!(self, Self::Right | Self::TopRight | Self::BottomRight)
    }

    fn is_top(self) -> bool {
        matches!(self, Self::TopLeft | Self::TopRight)
    }

    fn is_bottom(self) -> bool {
        matches!(self, Self::BottomLeft | Self::BottomRight)
    }

    fn conflicts_with(current: Option<Self>, next: Self) -> bool {
        let Some(current) = current.filter(|zone| zone.is_layout()) else {
            return false;
        };
        if current == next {
            return true;
        }
        matches!(
            (current, next),
            (Self::TopLeft | Self::BottomLeft, Self::Left)
                | (Self::TopRight | Self::BottomRight, Self::Right)
                | (Self::Left, Self::TopLeft | Self::BottomLeft)
                | (Self::Right, Self::TopRight | Self::BottomRight)
        )
    }
}

pub static WINDOW_STATE_RECT: WindowStateKey<Rect> = WindowStateKey::new("rect", |window| window.rect());
pub static WINDOW_STATE_RESTORE_RECT: WindowStateKey<Option<Rect>> = WindowStateKey::new("restoreRect", |_| None);
pub static WINDOW_STATE_MINIMIZED: WindowStateKey<bool> = WindowStateKey::new("minimized", |_| false);
pub static WINDOW_STATE_MINIMIZE_VISUAL_IDLE: WindowStateKey<bool> =
    WindowStateKey::new("minimizeVisualIdle", |_| false);
pub static WINDOW_STATE_MAXIMIZED: WindowStateKey<bool> = WindowStateKey::new("maximized", |_| false);
pub static WINDOW_STATE_FULLSCREEN: WindowStateKey<bool> = WindowStateKey::new("fullscreen", |_| false);
/// Pre-fullscreen rect, separate from the maximize restore rect so a window
/// maximized before going fullscreen comes back maximized.
pub static WINDOW_STATE_FULLSCREEN_RESTORE_RECT: WindowStateKey<Option<Rect>> =
    WindowStateKey::new("fullscreenRestoreRect", |_| None);
pub static WINDOW_STATE_WORKSPACE_VISIBLE: WindowStateKey<bool> = WindowStateKey::new("workspaceVisible", |_| true);
pub static WINDOW_STATE_WORKSPACE_OFFSET_Y: WindowStateKey<f64> = WindowStateKey::new("workspaceOffsetY", |_| 0.0);
pub static WINDOW_STATE_WORKSPACE_OPACITY: WindowStateKey<f64> = WindowStateKey::new("workspaceOpacity", |_| 1.0);
pub static WINDOW_STATE_TILE_DRAGGING: WindowStateKey<bool> = WindowStateKey::new("tileDragging", |_| false);
pub static WINDOW_STATE_TILE_REORDERING: WindowStateKey<bool> = WindowStateKey::new("tileReordering", |_| false);
pub static WINDOW_STATE_TILED: WindowStateKey<bool> = WindowStateKey::new("tiled", |_| false);
pub static WINDOW_STATE_WORKSPACE_TILED: WindowStateKey<bool> = WindowStateKey::new("workspaceTiled", |_| false);
pub static WINDOW_STATE_VISIBLE_OUTPUTS: WindowStateKey<Option<Vec<String>>> =
    WindowStateKey::new("visibleOutputs", |_| None);
pub static WINDOW_STATE_FLOATING_RECT: WindowStateKey<Option<Rect>> = WindowStateKey::new("floatingRect", |_| None);
pub static WINDOW_STATE_SNAP_ZONE: WindowStateKey<Option<SnapZone>> = WindowStateKey::new("snapZone", |_| None);
pub static WINDOW_STATE_SNAP_MONITOR: WindowStateKey<Option<String>> = WindowStateKey::new("snapMonitor", |_| None);

pub const OPEN_CLOSE_ANIMATION_DURATION: f64 = 500.0;
pub const INITIAL_TILEABILITY_SETTLE_DURATION: f64 = 1000.0;
pub const WINDOW_MANAGEMENT_ANIMATION_DURATION: f64 = 300.0;
const UNMAXIMIZE_GRAB_ANIMATION_DURATION: f64 = 90.0;
/// A restore and an activate closer together than this are one taskbar click.
const RESTORE_ACTIVATE_GRACE_MS: f64 = 100.0;
pub const WINDOW_MANAGEMENT_EASING: Easing = cubic_bezier(0.1, 0.9, 0.2, 1.0);
const WINDOW_OPEN_EASING: Easing = cubic_bezier(0.1, 1.1, 0.1, 1.1);
const WINDOW_CLOSE_EASING: Easing = cubic_bezier(0.3, -0.3, 0.0, 1.0);
const WINDOW_MINIMIZE_RECT_EASING: Easing = cubic_bezier(0.3, -0.3, 0.0, 1.0);
const WINDOW_UNMINIMIZE_RECT_EASING: Easing = cubic_bezier(0.1, 1.1, 0.1, 1.1);
const WINDOW_MINIMIZE_OPACITY_EASING: Easing = cubic_bezier(0.3, -0.3, 0.0, 1.0);
const WINDOW_UNMINIMIZE_OPACITY_EASING: Easing = cubic_bezier(0.1, 1.1, 0.1, 1.1);
pub const TILE_ANIMATION_DURATION: f64 = 500.0;
pub const WORKSPACE_SWITCH_ANIMATION_DURATION: f64 = 500.0;
const WORKSPACE_GESTURE_FINGERS: u32 = 3;
const WORKSPACE_GESTURE_AXIS_LOCK_PX: f64 = 8.0;
const WORKSPACE_GESTURE_THRESHOLD_RATIO: f64 = 0.22;
const WORKSPACE_GESTURE_VELOCITY_THRESHOLD: f64 = 900.0;
pub const WORKSPACE_KINETIC_SCROLL_MIN_VELOCITY: f64 = 120.0;
pub const WORKSPACE_KINETIC_SCROLL_MAX_VELOCITY: f64 = 5000.0;
pub const WORKSPACE_KINETIC_SCROLL_STOP_VELOCITY: f64 = 18.0;
pub const WORKSPACE_KINETIC_SCROLL_TIME_CONSTANT_MS: f64 = 360.0;
pub const WORKSPACE_KINETIC_SCROLL_FALLBACK_REFRESH_RATE: f64 = 120.0;
const TILE_DRAG_WORKSPACE_EDGE_PX: f64 = 80.0;
const TILE_DRAG_WORKSPACE_SWITCH_INTERVAL_MS: u64 = 420;
pub const TILE_GAP: f64 = 12.0;
pub const TILE_MARGIN: f64 = 12.0;
pub const TILE_WIDTH_RATIO: f64 = 0.5;
pub const TILE_MIN_WIDTH: f64 = 240.0;
/// Fractional-scale rounding can leave a tile a hair past the screen edge.
pub const TILE_FOCUS_OVERFLOW_EPSILON: f64 = 1.0;

// Windows-style edge snapping for floating drags (logical px).
const SNAP_EDGE_PX: f64 = 16.0;
const SNAP_CORNER_PX: f64 = 140.0;
const SNAP_GAP_PX: f64 = 8.0;

const OPEN_ANIMATION_CHANNEL: &str = "window.open";
const CLOSE_ANIMATION_CHANNEL: &str = "window.close";
const MINIMIZE_ANIMATION_CHANNEL: &str = "window.minimize";
const WORKSPACE_VISUAL_ANIMATION_CHANNEL: &str = "workspace.visual";
const WORKSPACE_VISUAL_RECT_ANIMATION_CHANNEL: &str = "workspace.visual.rect";
const WORKSPACE_VISUAL_OPACITY_ANIMATION_CHANNEL: &str = "workspace.visual.opacity";
pub const WINDOW_BORDER_PX: f64 = 2.0;
pub const TITLEBAR_HEIGHT: f64 = 30.0;
const MAXIMIZED_WINDOW_PADDING: f64 = 8.0;

pub type NaturalRootRect = fn(Window) -> Rect;
pub type ActiveWorkspaces = Rc<RefCell<BTreeMap<String, u32>>>;

/// Read window state without tracking (the manager runs in listeners).
pub fn get<T: Clone + 'static>(window: Window, key: &'static WindowStateKey<T>) -> T {
    window.state(key).get_untracked()
}

pub fn set<T: PartialEq + 'static>(window: Window, key: &'static WindowStateKey<T>, value: T) {
    window.state(key).set(value);
}

pub fn clamp(value: f64, min: f64, max: f64) -> f64 {
    value.max(min).min(max)
}

pub fn inset_rect(rect: Rect, top: f64, right: f64, bottom: f64, left: f64) -> Rect {
    Rect::new(
        rect.x + left,
        rect.y + top,
        (rect.width - left - right).max(1.0),
        (rect.height - top - bottom).max(1.0),
    )
}

fn inset_maximized(rect: Rect) -> Rect {
    let padding = MAXIMIZED_WINDOW_PADDING;
    inset_rect(rect, padding, padding, padding, padding)
}

pub fn output_list() -> Vec<String> {
    COMPOSITOR.output.list()
}

/// The whole output in logical coordinates.
pub fn output_rect(name: &str) -> Option<Rect> {
    let output = COMPOSITOR.output.get(name)?;
    let mode = output.resolution?;
    Some(Rect::new(
        output.position.x as f64,
        output.position.y as f64,
        mode.width as f64 / output.scale,
        mode.height as f64 / output.scale,
    ))
}

pub fn output_name_at(x: f64, y: f64) -> Option<String> {
    output_list()
        .into_iter()
        .find(|name| output_rect(name).is_some_and(|rect| rect.contains(x, y)))
}

/// The maximized rect of `window` on `output`: its usable area minus the
/// maximize padding.
pub fn maximized_rect_on(window: Window, output: Option<&str>) -> Rect {
    let rect = get(window, &WINDOW_STATE_RECT);
    let Some(output) = output else {
        return rect;
    };
    if let Some(usable) = COMPOSITOR.layer.usable_area(output) {
        return inset_maximized(usable);
    }
    output_rect(output).map_or(rect, inset_maximized)
}

fn wall_clock_ms() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |elapsed| elapsed.as_secs_f64() * 1000.0)
}

/// A snap preview for the bar, monitor-local.
#[derive(Debug, Clone, PartialEq)]
pub struct SnapPreview {
    pub monitor: String,
    pub rect: Option<Rect>,
    pub kind: &'static str,
}

/// Bar-facing summary of the workspace layout.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkspacesView {
    pub current_monitor: String,
    pub monitors: Vec<MonitorView>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MonitorView {
    pub name: String,
    pub active: u32,
    pub workspaces: Vec<WorkspaceView>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WorkspaceView {
    pub index: u32,
    pub window_count: usize,
    pub is_tiled: bool,
    pub active: bool,
    pub windows: Vec<WindowView>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WindowView {
    pub id: String,
    pub app_id: Option<String>,
    pub title: String,
    pub focused: bool,
    pub last_focused_at: f64,
}

impl WorkspacesView {
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "currentMonitor": self.current_monitor,
            "monitors": self.monitors.iter().map(|monitor| serde_json::json!({
                "name": monitor.name,
                "active": monitor.active,
                "workspaces": monitor.workspaces.iter().map(|workspace| serde_json::json!({
                    "index": workspace.index,
                    "windowCount": workspace.window_count,
                    "isTiled": workspace.is_tiled,
                    "active": workspace.active,
                    "windows": workspace.windows.iter().map(|window| serde_json::json!({
                        "id": window.id,
                        "appId": window.app_id,
                        "title": window.title,
                        "focused": window.focused,
                        "lastFocusedAt": window.last_focused_at,
                    })).collect::<Vec<_>>(),
                })).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
        })
    }
}

#[derive(Debug, Clone, Copy)]
pub struct WorkspaceGestureSpeed {
    pub workspace_scroll_factor: f64,
    pub workspace_scroll_kinetic_factor: f64,
    pub workspace_switch_factor: f64,
    pub workspace_switch_velocity_factor: f64,
    /// At or below this scroll speed (logical px/s) a three-finger scroll
    /// catches on tile snap positions. 0 disables snapping.
    pub workspace_scroll_snap_max_velocity: f64,
    /// Finger travel needed to break out of a caught position.
    pub workspace_scroll_snap_breakout_px: f64,
}

impl Default for WorkspaceGestureSpeed {
    fn default() -> Self {
        Self {
            workspace_scroll_factor: 1.0,
            workspace_scroll_kinetic_factor: 1.0,
            workspace_switch_factor: 1.0,
            workspace_switch_velocity_factor: 1.0,
            workspace_scroll_snap_max_velocity: 300.0,
            workspace_scroll_snap_breakout_px: 48.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GestureMode {
    WorkspaceSwitch,
    WorkspaceScroll,
}

struct WorkspaceGesture {
    monitor: String,
    current_index: u32,
    direction: i32,
    distance: f64,
    from: u64,
    to: Option<u64>,
    from_offset_y: f64,
    to_offset_y: f64,
    from_opacity: f64,
    to_opacity: f64,
}

struct Drag {
    window: Window,
    workspace: u64,
    last_workspace_switch_at: u64,
}

struct MaximizedMoveDrag {
    window: Window,
    width: f64,
    height: f64,
}

struct FloatingSnap {
    window: Window,
    monitor: String,
    zone: SnapZone,
    rect: Rect,
}

struct ScrollSnapLatch {
    overshoot: f64,
}

struct FloatingSnapLayout {
    split_x: f64,
    left_split_y: f64,
    right_split_y: f64,
}

/// Messages for the outside world, delivered after the borrow is released.
#[derive(Default)]
struct Outbox {
    snap_previews: Vec<SnapPreview>,
    workspaces_changed: bool,
}

type SnapPreviewBroadcaster = Rc<dyn Fn(&SnapPreview)>;
type WorkspaceChangeBroadcaster = Rc<dyn Fn()>;

#[derive(Default)]
struct Broadcasters {
    snap_preview: Option<SnapPreviewBroadcaster>,
    workspace_change: Option<WorkspaceChangeBroadcaster>,
}

/// Shared handle to the window manager.
#[derive(Clone)]
pub struct WindowManager {
    state: Rc<RefCell<HybridWindowManager>>,
    broadcasters: Rc<RefCell<Broadcasters>>,
}

/// Weak handle for timers and subscriptions.
#[derive(Clone)]
pub struct WeakWindowManager {
    state: Weak<RefCell<HybridWindowManager>>,
    broadcasters: Weak<RefCell<Broadcasters>>,
}

impl WeakWindowManager {
    pub fn upgrade(&self) -> Option<WindowManager> {
        Some(WindowManager {
            state: self.state.upgrade()?,
            broadcasters: self.broadcasters.upgrade()?,
        })
    }

    pub fn with_workspace(&self, id: u64, f: impl FnOnce(&mut Workspace)) {
        if let Some(manager) = self.upgrade() {
            manager.with(|wm| {
                if let Some(workspace) = wm.workspace_by_id_mut(id) {
                    f(workspace);
                }
            });
        }
    }

    /// One frame of a kinetic scroll, from its timer.
    pub fn kinetic_frame(&self, id: u64, token: u64) {
        if let Some(manager) = self.upgrade() {
            manager.with(|wm| wm.kinetic_frame(id, token));
        }
    }
}

impl WindowManager {
    pub fn new(natural_root_rect: NaturalRootRect) -> Self {
        let manager = Self {
            state: Rc::new(RefCell::new(HybridWindowManager::new(natural_root_rect))),
            broadcasters: Rc::default(),
        };
        let weak = manager.downgrade();
        manager.state.borrow_mut().handle = weak;
        manager.with(|wm| wm.sync_workspaces());
        manager
    }

    pub fn downgrade(&self) -> WeakWindowManager {
        WeakWindowManager {
            state: Rc::downgrade(&self.state),
            broadcasters: Rc::downgrade(&self.broadcasters),
        }
    }

    /// Run `f` on the manager, then deliver its outbox.
    pub fn with<R>(&self, f: impl FnOnce(&mut HybridWindowManager) -> R) -> R {
        let (result, outbox) = {
            let mut wm = self.state.borrow_mut();
            let result = f(&mut wm);
            (result, std::mem::take(&mut wm.outbox))
        };
        self.deliver(outbox);
        result
    }

    /// Like [`with`](Self::with), but deferred to a timer when the manager
    /// is already in use (a subscription firing mid-call).
    pub fn with_or_defer(&self, f: impl FnOnce(&mut HybridWindowManager) + 'static) {
        if self.state.try_borrow_mut().is_ok() {
            self.with(f);
            return;
        }
        let weak = self.downgrade();
        let f = RefCell::new(Some(f));
        set_timeout(1.0, move || {
            if let (Some(manager), Some(f)) = (weak.upgrade(), f.borrow_mut().take()) {
                manager.with(f);
            }
        });
    }

    fn deliver(&self, outbox: Outbox) {
        let (snap, change) = {
            let broadcasters = self.broadcasters.borrow();
            (
                broadcasters.snap_preview.clone(),
                broadcasters.workspace_change.clone(),
            )
        };
        if let Some(snap) = snap {
            for preview in &outbox.snap_previews {
                snap(preview);
            }
        }
        if outbox.workspaces_changed
            && let Some(change) = change
        {
            change();
        }
    }

    pub fn set_snap_preview_broadcaster(&self, f: impl Fn(&SnapPreview) + 'static) {
        self.broadcasters.borrow_mut().snap_preview = Some(Rc::new(f));
    }

    pub fn set_workspace_change_broadcaster(&self, f: impl Fn() + 'static) {
        self.broadcasters.borrow_mut().workspace_change = Some(Rc::new(f));
    }

    /// Read-only access, e.g. from a workspace factory.
    pub fn read<R>(&self, f: impl FnOnce(&HybridWindowManager) -> R) -> R {
        f(&self.state.borrow())
    }

    pub fn window_z_index(&self, window: Window) -> ReadSignal<i32> {
        self.state.borrow().window_stack.z_index(window)
    }
}

pub struct HybridWindowManager {
    /// In creation order, like the TypeScript `Map`.
    workspaces: Vec<Workspace>,
    next_workspace_id: u64,
    active_workspace_by_monitor: ActiveWorkspaces,
    window_stack: WindowStack,
    natural_root_rect: NaturalRootRect,
    handle: WeakWindowManager,
    outbox: Outbox,
    /// MRU focus time per window, for the dock.
    last_focused_at: HashMap<String, f64>,
    pending_initial_focus: HashMap<Window, f64>,
    tileability: HashMap<Window, bool>,
    tileability_subscriptions: HashMap<Window, Scope>,
    current_monitor: String,
    is_grabbing: bool,
    tile_drag: Option<Drag>,
    floating_drag: Option<Drag>,
    maximized_move_drag: Option<MaximizedMoveDrag>,
    workspace_gesture: Option<WorkspaceGesture>,
    workspace_gesture_mode: Option<GestureMode>,
    workspace_scroll_gesture_rect_animations_cancelled: bool,
    workspace_scroll_snap_latch: Option<ScrollSnapLatch>,
    /// EMA of the active gesture's scroll speed (logical px/s).
    workspace_scroll_gesture_speed: Option<f64>,
    gesture_speed: WorkspaceGestureSpeed,
    last_pointer_position: Option<PointerMovePointSnapshot>,
    last_pointer_target: PointerHitTargetSnapshot,
    floating_snap: Option<FloatingSnap>,
    restored_during_initial_configure: HashSet<Window>,
    deferred_initial_layout: HashSet<Window>,
    /// Windows that presented a frame; only those can be minimized by a
    /// taskbar re-click (a dock activating a window it just launched must
    /// not minimize it).
    presented_windows: HashSet<Window>,
    /// Last restore from minimized (wall clock), to recognise the activate
    /// that arrives in the same taskbar click.
    last_restore_at: HashMap<Window, f64>,
    /// Set while `activate_window_by_id_instant` runs: state changes it makes
    /// skip their animations.
    instant_transitions: bool,
    /// Where the pointer was when a kinetic scroll started, per workspace.
    kinetic_frame_context: HashMap<u64, (Option<PointerMovePointSnapshot>, String)>,
}

impl HybridWindowManager {
    fn new(natural_root_rect: NaturalRootRect) -> Self {
        Self {
            workspaces: Vec::new(),
            next_workspace_id: 1,
            active_workspace_by_monitor: Rc::default(),
            window_stack: WindowStack::default(),
            natural_root_rect,
            handle: WeakWindowManager {
                state: Weak::new(),
                broadcasters: Weak::new(),
            },
            outbox: Outbox::default(),
            last_focused_at: HashMap::new(),
            pending_initial_focus: HashMap::new(),
            tileability: HashMap::new(),
            tileability_subscriptions: HashMap::new(),
            current_monitor: String::new(),
            is_grabbing: false,
            tile_drag: None,
            floating_drag: None,
            maximized_move_drag: None,
            workspace_gesture: None,
            workspace_gesture_mode: None,
            workspace_scroll_gesture_rect_animations_cancelled: false,
            workspace_scroll_snap_latch: None,
            workspace_scroll_gesture_speed: None,
            gesture_speed: WorkspaceGestureSpeed::default(),
            last_pointer_position: None,
            last_pointer_target: PointerHitTargetSnapshot::None,
            floating_snap: None,
            restored_during_initial_configure: HashSet::new(),
            deferred_initial_layout: HashSet::new(),
            presented_windows: HashSet::new(),
            last_restore_at: HashMap::new(),
            instant_transitions: false,
            kinetic_frame_context: HashMap::new(),
        }
    }

    fn active_index(&self, monitor: &str) -> u32 {
        self.active_workspace_by_monitor
            .borrow()
            .get(monitor)
            .copied()
            .unwrap_or(1)
    }

    fn set_active_index(&self, monitor: &str, index: u32) {
        self.active_workspace_by_monitor
            .borrow_mut()
            .insert(monitor.to_owned(), index);
    }

    fn position(&self, monitor: &str, index: u32) -> Option<usize> {
        self.workspaces
            .iter()
            .position(|workspace| workspace.monitor == monitor && workspace.index == index)
    }

    pub fn workspace_by_id_mut(&mut self, id: u64) -> Option<&mut Workspace> {
        self.workspaces.iter_mut().find(|workspace| workspace.id == id)
    }

    fn workspace_by_id(&self, id: u64) -> Option<&Workspace> {
        self.workspaces.iter().find(|workspace| workspace.id == id)
    }

    fn ws(&mut self, id: u64) -> &mut Workspace {
        self.workspace_by_id_mut(id).expect("workspace id is live")
    }

    pub fn configure_workspace_gesture_speed(&mut self, speed: WorkspaceGestureSpeed) {
        let factor = |value: f64, fallback: f64| {
            if value.is_finite() && value > 0.0 { value } else { fallback }
        };
        let defaults = WorkspaceGestureSpeed::default();
        let scroll = factor(speed.workspace_scroll_factor, defaults.workspace_scroll_factor);
        let switch = factor(speed.workspace_switch_factor, defaults.workspace_switch_factor);
        self.gesture_speed = WorkspaceGestureSpeed {
            workspace_scroll_factor: scroll,
            workspace_scroll_kinetic_factor: factor(speed.workspace_scroll_kinetic_factor, scroll),
            workspace_switch_factor: switch,
            workspace_switch_velocity_factor: factor(speed.workspace_switch_velocity_factor, switch),
            workspace_scroll_snap_max_velocity: if speed.workspace_scroll_snap_max_velocity.is_finite()
                && speed.workspace_scroll_snap_max_velocity >= 0.0
            {
                speed.workspace_scroll_snap_max_velocity
            } else {
                defaults.workspace_scroll_snap_max_velocity
            },
            workspace_scroll_snap_breakout_px: factor(
                speed.workspace_scroll_snap_breakout_px,
                defaults.workspace_scroll_snap_breakout_px,
            ),
        };
    }

    pub fn on_pointer_move(&mut self, event: &PointerMoveEventSnapshot) {
        self.sync_workspaces();
        if let Some(output) = &event.output_name {
            self.current_monitor = output.clone();
        }
        self.last_pointer_position = Some(event.position);
        self.last_pointer_target = event.target.clone();
        self.focus_window_at_pointer_target(&event.target, event.output_name.as_deref());
    }

    pub fn on_gesture_swipe(&mut self, event: &GestureSwipeEventSnapshot) {
        if event.fingers != WORKSPACE_GESTURE_FINGERS {
            return;
        }
        self.sync_workspaces();
        if let Some(position) = event.position {
            self.last_pointer_position = Some(position);
        }
        match event.phase {
            GestureSwipePhaseSnapshot::Begin => {
                self.workspace_gesture = None;
                self.workspace_gesture_mode = None;
                self.workspace_scroll_gesture_rect_animations_cancelled = false;
                self.workspace_scroll_snap_latch = None;
                self.workspace_scroll_gesture_speed = None;
                self.current_monitor = self.gesture_monitor(event);
            }
            GestureSwipePhaseSnapshot::Update => match self.resolve_workspace_gesture_mode(event) {
                Some(GestureMode::WorkspaceScroll) => {
                    self.workspace_gesture = None;
                    self.update_workspace_scroll_gesture(event);
                }
                Some(GestureMode::WorkspaceSwitch) => self.update_workspace_gesture(event),
                None => {}
            },
            GestureSwipePhaseSnapshot::End | GestureSwipePhaseSnapshot::Cancel => {
                if self.workspace_gesture_mode == Some(GestureMode::WorkspaceScroll) {
                    self.workspace_gesture_mode = None;
                    self.workspace_gesture = None;
                    self.finish_workspace_scroll_gesture(event);
                    self.workspace_scroll_gesture_rect_animations_cancelled = false;
                    let position = event.position.or(self.last_pointer_position);
                    self.focus_window_at_pointer_position(position, event.output_name.as_deref());
                    return;
                }
                self.workspace_gesture_mode = None;
                self.workspace_scroll_gesture_rect_animations_cancelled = false;
                self.finish_workspace_gesture(event);
            }
        }
    }

    pub fn on_output_change(&mut self, event: &OutputChangeEvent) {
        let live: Vec<String> = event
            .current
            .values()
            .filter(|output| output.enabled)
            .map(|output| output.name.clone())
            .collect();
        if live.is_empty() {
            return;
        }
        let fallback = if !self.current_monitor.is_empty() && live.contains(&self.current_monitor) {
            self.current_monitor.clone()
        } else {
            live[0].clone()
        };

        let orphaned: Vec<u64> = self
            .workspaces
            .iter()
            .filter(|workspace| !live.contains(&workspace.monitor))
            .map(|workspace| workspace.id)
            .collect();
        if orphaned.is_empty() {
            self.sync_workspaces();
            self.refresh_usable_area_layouts();
            return;
        }

        let orphaned_active: BTreeMap<String, u32> = self
            .active_workspace_by_monitor
            .borrow()
            .iter()
            .filter(|(monitor, _)| !live.contains(monitor))
            .map(|(monitor, index)| (monitor.clone(), *index))
            .collect();
        self.active_workspace_by_monitor
            .borrow_mut()
            .retain(|monitor, _| live.contains(monitor));

        for id in orphaned {
            let position = self
                .workspaces
                .iter()
                .position(|workspace| workspace.id == id)
                .expect("orphan is live");
            let mut workspace = self.workspaces.remove(position);
            if workspace.window_count() == 0 {
                continue;
            }
            let target_index = self.available_workspace_index(&fallback, workspace.index);
            let was_active = orphaned_active.get(&workspace.monitor) == Some(&workspace.index);
            workspace.move_to_monitor(&fallback, target_index);
            let has_active = self.active_workspace_by_monitor.borrow().contains_key(&fallback);
            if was_active || !has_active {
                self.set_active_index(&fallback, target_index);
            }
            self.workspaces.push(workspace);
            let workspace = self.workspaces.last_mut().expect("just pushed");
            let active = workspace.is_active();
            workspace.set_visible(active);
            workspace.apply_layout(LayoutOptions {
                animate: Some(false),
                preserve_missing_active: true,
                ..LayoutOptions::default()
            });
        }

        if !live.contains(&self.current_monitor) {
            self.current_monitor = fallback;
        }
        self.sync_workspaces();
        self.refresh_usable_area_layouts();
        self.sync_workspace_visibility();
    }

    pub fn on_open(&mut self, window: Window) {
        self.track_window_tileability(window);
        window.focus();
        self.window_stack.add(window, Placement::Front);
        window.set_close_animation_duration(OPEN_CLOSE_ANIMATION_DURATION as u64);
    }

    fn track_window_tileability(&mut self, window: Window) {
        self.untrack_window_tileability(window);
        let tileable = |window: Window| {
            window.is_resizable().get_untracked() && !window.is_transient().get_untracked()
        };
        self.tileability.insert(window, tileable(window));

        let scope = untrack(Scope::root);
        let handle = self.handle.clone();
        let first_run = std::cell::Cell::new(true);
        scope.effect(move || {
            let _ = window.is_resizable().get();
            let _ = window.is_transient().get();
            if first_run.replace(false) {
                return;
            }
            if let Some(manager) = handle.upgrade() {
                manager.with_or_defer(move |wm| wm.on_tileability_change(window));
            }
        });
        self.tileability_subscriptions.insert(window, scope);
    }

    fn on_tileability_change(&mut self, window: Window) {
        let Some(&was_tileable) = self.tileability.get(&window) else {
            return;
        };
        let is_tileable = window.is_resizable().get_untracked() && !window.is_transient().get_untracked();
        if was_tileable == is_tileable {
            return;
        }
        self.tileability.insert(window, is_tileable);
        if !is_tileable {
            self.pending_initial_focus.remove(&window);
        }
        let Some(id) = self.find_workspace_for_window(window) else {
            return;
        };
        self.ws(id).reclassify_window(window, was_tileable);
        self.apply_workspace_stack_policy(Some(id));
    }

    fn untrack_window_tileability(&mut self, window: Window) {
        if let Some(scope) = self.tileability_subscriptions.remove(&window) {
            scope.dispose();
        }
        self.tileability.remove(&window);
    }

    /// Returns (workspace, restored existing window).
    fn initialize_window_layout(&mut self, window: Window, options: AddWindowOptions) -> (Option<u64>, bool) {
        if let Some(existing) = self.find_workspace_for_window(window) {
            return (
                Some(existing),
                self.restored_during_initial_configure.contains(&window),
            );
        }

        let mut restored_existing = false;
        let workspace = self
            .find_workspace_restoring_window(window)
            .or_else(|| self.current_workspace());
        if let Some(id) = workspace {
            restored_existing = self.ws(id).add_window(window, options);
            let workspace = self.workspace_by_id(id).expect("live");
            if !restored_existing && workspace.is_tiled && workspace.should_tile(window) {
                self.track_pending_initial_focus(window);
            }
            self.apply_workspace_stack_policy(Some(id));
            self.sync_workspace_visibility();
        } else {
            set(window, &WINDOW_STATE_RECT, (self.natural_root_rect)(window));
        }

        if window.is_maximized().get_untracked() {
            set(
                window,
                &WINDOW_STATE_RESTORE_RECT,
                Some(self.initial_restore_rect_for_maximized_window(window)),
            );
            set(window, &WINDOW_STATE_RECT, self.maximized_rect_for_window(window, None));
            set(window, &WINDOW_STATE_MAXIMIZED, true);
        }

        // A window already fullscreen when it joins keeps the fullscreen rect:
        // xwayland-satellite requests fullscreen before the first commit, and
        // SDL2 leaves fullscreen when the configure carries a windowed size.
        if get(window, &WINDOW_STATE_FULLSCREEN) || window.is_fullscreen().get_untracked() {
            let rect = self.fullscreen_rect_for_window(window, None);
            set(window, &WINDOW_STATE_FULLSCREEN, true);
            stop_rect_animation(window, &WINDOW_STATE_RECT);
            set(window, &WINDOW_STATE_RECT, rect);
            if let Some(id) = workspace {
                self.ws(id).sync_floating_window_rect(window, rect);
            }
        }

        if restored_existing {
            self.restored_during_initial_configure.insert(window);
        }
        (workspace, restored_existing)
    }

    pub fn on_initial_configure(&mut self, window: Window) {
        if self.should_defer_initial_window_layout(window) {
            self.deferred_initial_layout.insert(window);
            return;
        }
        self.deferred_initial_layout.remove(&window);
        self.initialize_window_layout(window, AddWindowOptions::default());
    }

    fn should_defer_initial_window_layout(&mut self, window: Window) -> bool {
        if self.find_workspace_restoring_window(window).is_some() {
            return false;
        }
        // Maximized/fullscreen windows need an output-sized initial configure.
        if window.is_maximized().get_untracked() || window.is_fullscreen().get_untracked() {
            return false;
        }
        // In floating mode the client picks its natural size first.
        let tiled = self
            .current_workspace()
            .and_then(|id| self.workspace_by_id(id))
            .is_some_and(|workspace| workspace.is_tiled);
        if !tiled {
            return true;
        }
        if !window.is_resizable().get_untracked() || window.is_transient().get_untracked() {
            return true;
        }
        // Without min/max constraints a tiled window cannot yet be told from
        // a client about to declare a fixed size.
        let constraints = window.size_constraints().get_untracked();
        constraints.min.is_none() && constraints.max.is_none()
    }

    pub fn on_first_commit(&mut self, window: Window) {
        self.presented_windows.insert(window);
        if !self.tileability.contains_key(&window) {
            self.track_window_tileability(window);
        }
        if !self.window_stack.has(window) {
            self.window_stack.add(window, Placement::Back);
        }
        window.set_close_animation_duration(OPEN_CLOSE_ANIMATION_DURATION as u64);

        let deferred = self.deferred_initial_layout.remove(&window);
        let (_, restored) = self.initialize_window_layout(
            window,
            AddWindowOptions {
                restore_scroll_if_initially_floating: deferred,
            },
        );
        let restored_during_configure = self.restored_during_initial_configure.remove(&window);
        if !(restored || restored_during_configure) {
            schedule_open_animation(window);
        }
    }

    pub fn on_start_close(&mut self, window: Window) {
        schedule_close_animation(window);
        let ids: Vec<u64> = self.workspaces.iter().map(|workspace| workspace.id).collect();
        for id in ids {
            if let Some(next_focus) = self.ws(id).remove_window(window) {
                self.ws(id).apply_layout(LayoutOptions::default());
                if let Some(next) = next_focus {
                    next.focus();
                }
                break;
            }
        }
        self.sync_workspace_visibility();
    }

    pub fn on_close(&mut self, window: Window) {
        self.presented_windows.remove(&window);
        self.last_restore_at.remove(&window);
        self.restored_during_initial_configure.remove(&window);
        self.deferred_initial_layout.remove(&window);
        self.pending_initial_focus.remove(&window);
        self.untrack_window_tileability(window);
        self.window_stack.remove(window);
        self.last_focused_at.remove(&window.id());
        let ids: Vec<u64> = self.workspaces.iter().map(|workspace| workspace.id).collect();
        for id in ids {
            if self.ws(id).remove_window(window).is_some() {
                self.ws(id).apply_layout(LayoutOptions::default());
            }
        }
        self.sync_workspace_visibility();
    }

    pub fn on_focus(&mut self, window: Window, focused: bool) {
        if !focused {
            return;
        }
        let workspace = self.find_workspace_for_window(window);
        self.window_stack.raise(window);
        if self.should_defer_focus_layout_for_initial_open(window, workspace) {
            self.apply_workspace_stack_policy(workspace);
            return;
        }
        if let Some(id) = workspace {
            let ws = self.workspace_by_id(id).expect("live");
            if ws.is_tiled && ws.is_active() {
                self.ws(id).focus_window(window);
                self.apply_workspace_stack_policy(Some(id));
            }
        }
    }

    pub fn on_window_resize(&mut self, window: Window, event: &WindowResizeEventSnapshot) {
        if !window.is_resizable().get_untracked() {
            return;
        }
        let workspace = self.find_workspace_for_window(window);
        if matches!(event.phase, WindowResizePhaseSnapshot::Start | WindowResizePhaseSnapshot::Update) {
            self.begin_interactive_unmaximize(window);
        }
        if let Some(id) = workspace {
            let ws = self.workspace_by_id(id).expect("live");
            if ws.is_tiled && ws.should_tile(window) {
                self.ws(id).resize_tile(window, event);
                self.apply_workspace_stack_policy(Some(id));
                return;
            }
        }

        let next_rect = self.constrain_resize_rect(window, event);
        if let Some(id) = workspace
            && self.resize_floating_snap_layout(window, event, id, next_rect)
        {
            self.apply_workspace_stack_policy(Some(id));
            return;
        }
        stop_rect_animation(window, &WINDOW_STATE_RECT);
        set(window, &WINDOW_STATE_RECT, next_rect);
        if let Some(id) = workspace {
            self.ws(id).sync_floating_window_rect(window, next_rect);
        }
        self.apply_workspace_stack_policy(workspace);
    }

    pub fn on_window_move(&mut self, window: Window, event: &WindowMoveEventSnapshot) {
        let workspace = self.find_workspace_for_window(window);
        if let Some(id) = workspace {
            let ws = self.workspace_by_id(id).expect("live");
            if ws.is_tiled && ws.should_tile(window) {
                self.on_tile_window_move(window, event, id);
                let current = self.find_workspace_for_window(window);
                self.apply_workspace_stack_policy(current);
                return;
            }
            self.on_floating_window_move(window, event, id);
            return;
        }

        if event.phase == WindowMovePhaseSnapshot::Start && get(window, &WINDOW_STATE_MAXIMIZED) {
            let restore = get(window, &WINDOW_STATE_RESTORE_RECT).unwrap_or(event.current_rect.into());
            self.maximized_move_drag = Some(MaximizedMoveDrag {
                window,
                width: restore.width,
                height: restore.height,
            });
            self.begin_interactive_unmaximize(window);
        }
        if event.phase == WindowMovePhaseSnapshot::Start {
            self.is_grabbing = true;
            clear_window_snap_state(window);
        }

        let maximized_drag = self
            .maximized_move_drag
            .as_ref()
            .filter(|drag| drag.window == window)
            .map(|drag| (drag.width, drag.height));
        if let Some((width, height)) = maximized_drag {
            let next_rect = restore_rect_for_maximized_move(event, width, height);
            if event.phase == WindowMovePhaseSnapshot::Start {
                play_rect_animation(
                    window,
                    &WINDOW_STATE_RECT,
                    next_rect,
                    WINDOW_MANAGEMENT_EASING,
                    UNMAXIMIZE_GRAB_ANIMATION_DURATION,
                );
            } else {
                stop_rect_animation(window, &WINDOW_STATE_RECT);
                set(window, &WINDOW_STATE_RECT, next_rect);
            }
            match event.phase {
                WindowMovePhaseSnapshot::End | WindowMovePhaseSnapshot::Cancel => {
                    self.is_grabbing = false;
                    self.maximized_move_drag = None;
                    self.finish_floating_drag_snap(window, event, workspace);
                }
                _ => self.update_floating_drag_snap(window, event),
            }
            return;
        }

        if matches!(event.phase, WindowMovePhaseSnapshot::End | WindowMovePhaseSnapshot::Cancel) {
            self.is_grabbing = false;
            if !self.finish_floating_drag_snap(window, event, workspace) {
                stop_rect_animation(window, &WINDOW_STATE_RECT);
                set(window, &WINDOW_STATE_RECT, event.current_rect.into());
            }
            self.apply_workspace_stack_policy(workspace);
            return;
        }
        self.update_floating_drag_snap(window, event);
        stop_rect_animation(window, &WINDOW_STATE_RECT);
        set(window, &WINDOW_STATE_RECT, event.current_rect.into());
        self.apply_workspace_stack_policy(workspace);
    }

    fn on_floating_window_move(&mut self, window: Window, event: &WindowMoveEventSnapshot, workspace: u64) {
        if event.phase == WindowMovePhaseSnapshot::Start
            || self.floating_drag.as_ref().is_none_or(|drag| drag.window != window)
        {
            self.is_grabbing = true;
            self.floating_drag = Some(Drag {
                window,
                workspace,
                last_workspace_switch_at: event.timestamp,
            });
            if get(window, &WINDOW_STATE_MAXIMIZED) {
                let restore = get(window, &WINDOW_STATE_RESTORE_RECT).unwrap_or(event.current_rect.into());
                self.maximized_move_drag = Some(MaximizedMoveDrag {
                    window,
                    width: restore.width,
                    height: restore.height,
                });
                self.begin_interactive_unmaximize(window);
            }
            clear_window_snap_state(window);
        }
        if self.floating_drag.is_none() {
            return;
        }

        let maximized_drag = self
            .maximized_move_drag
            .as_ref()
            .filter(|drag| drag.window == window)
            .map(|drag| (drag.width, drag.height));
        let next_rect = match maximized_drag {
            Some((width, height)) => restore_rect_for_maximized_move(event, width, height),
            None => event.current_rect.into(),
        };
        if maximized_drag.is_some() && event.phase == WindowMovePhaseSnapshot::Start {
            play_rect_animation(
                window,
                &WINDOW_STATE_RECT,
                next_rect,
                WINDOW_MANAGEMENT_EASING,
                UNMAXIMIZE_GRAB_ANIMATION_DURATION,
            );
        } else {
            stop_rect_animation(window, &WINDOW_STATE_RECT);
            set(window, &WINDOW_STATE_RECT, next_rect);
        }

        if event.phase != WindowMovePhaseSnapshot::Cancel {
            let target = self.workspace_for_drag(event, true);
            let drag_workspace = self.floating_drag.as_ref().map_or(workspace, |drag| drag.workspace);
            if target != drag_workspace {
                self.ws(drag_workspace).remove_floating_window(window);
                self.ws(drag_workspace).apply_layout(LayoutOptions::default());
                let target_ws = self.workspace_by_id(target).expect("live");
                if target_ws.is_tiled && target_ws.should_tile(window) {
                    self.clear_floating_snap_preview();
                    self.ws(target).adopt_tile_drag_window(window, next_rect);
                    self.floating_drag = None;
                    self.tile_drag = Some(Drag {
                        window,
                        workspace: target,
                        last_workspace_switch_at: event.timestamp,
                    });
                    self.sync_workspace_visibility();
                    self.ws(target)
                        .update_tile_drag(window, next_rect, event.current_pointer.x);
                    let (monitor, slot) = {
                        let ws = self.workspace_by_id(target).expect("live");
                        (ws.monitor.clone(), ws.dragging_slot_rect())
                    };
                    self.emit_snap_preview(&monitor, slot, "tiling");
                    self.apply_workspace_stack_policy(Some(target));
                    if event.phase == WindowMovePhaseSnapshot::End {
                        self.ws(target).end_tile_drag(window, false);
                        self.tile_drag = None;
                        self.maximized_move_drag = None;
                        self.is_grabbing = false;
                    }
                    window.focus();
                    return;
                }
                self.ws(target).adopt_floating_window(window, next_rect);
                if let Some(drag) = &mut self.floating_drag {
                    drag.workspace = target;
                }
                self.sync_workspace_visibility();
                window.focus();
            } else {
                self.ws(target).sync_floating_window_rect(window, next_rect);
            }
            self.apply_workspace_stack_policy(Some(target));
            self.update_floating_drag_snap(window, event);
        }

        if matches!(event.phase, WindowMovePhaseSnapshot::End | WindowMovePhaseSnapshot::Cancel) {
            let drag_workspace = self.floating_drag.as_ref().map_or(workspace, |drag| drag.workspace);
            if !self.finish_floating_drag_snap(window, event, Some(drag_workspace)) {
                stop_rect_animation(window, &WINDOW_STATE_RECT);
                set(window, &WINDOW_STATE_RECT, next_rect);
                self.ws(drag_workspace).sync_floating_window_rect(window, next_rect);
            }
            self.apply_workspace_stack_policy(Some(drag_workspace));
            self.floating_drag = None;
            if maximized_drag.is_some() {
                self.maximized_move_drag = None;
            }
            self.is_grabbing = false;
        }
    }

    fn on_tile_window_move(&mut self, window: Window, event: &WindowMoveEventSnapshot, workspace: u64) {
        if event.phase == WindowMovePhaseSnapshot::Start
            || self.tile_drag.as_ref().is_none_or(|drag| drag.window != window)
        {
            self.is_grabbing = true;
            self.ws(workspace).begin_tile_drag(window, event.current_rect.into());
            self.tile_drag = Some(Drag {
                window,
                workspace,
                last_workspace_switch_at: event.timestamp,
            });
        }
        let Some(drag_workspace) = self.tile_drag.as_ref().map(|drag| drag.workspace) else {
            return;
        };

        if matches!(event.phase, WindowMovePhaseSnapshot::End | WindowMovePhaseSnapshot::Cancel) {
            let monitor = self.workspace_by_id(drag_workspace).expect("live").monitor.clone();
            self.emit_snap_preview(&monitor, None, "tiling");
            self.ws(drag_workspace)
                .end_tile_drag(window, event.phase == WindowMovePhaseSnapshot::Cancel);
            self.tile_drag = None;
            self.is_grabbing = false;
            return;
        }

        let target = self.workspace_for_drag(event, false);
        if target != drag_workspace {
            let monitor = self.workspace_by_id(drag_workspace).expect("live").monitor.clone();
            self.emit_snap_preview(&monitor, None, "tiling");
            self.ws(drag_workspace).remove_tile_drag_window(window);
            self.ws(drag_workspace).apply_layout(LayoutOptions::default());
            let target_ws = self.workspace_by_id(target).expect("live");
            if !target_ws.is_tiled || !target_ws.should_tile(window) {
                set(window, &WINDOW_STATE_TILE_DRAGGING, false);
                self.ws(target)
                    .adopt_floating_window(window, event.current_rect.into());
                self.tile_drag = None;
                self.floating_drag = Some(Drag {
                    window,
                    workspace: target,
                    last_workspace_switch_at: event.timestamp,
                });
                self.sync_workspace_visibility();
                self.apply_workspace_stack_policy(Some(target));
                self.update_floating_drag_snap(window, event);
                return;
            }
            self.ws(target)
                .adopt_tile_drag_window(window, event.current_rect.into());
            if let Some(drag) = &mut self.tile_drag {
                drag.workspace = target;
            }
            self.sync_workspace_visibility();
        }

        self.ws(target)
            .update_tile_drag(window, event.current_rect.into(), event.current_pointer.x);
        let (monitor, slot) = {
            let ws = self.workspace_by_id(target).expect("live");
            (ws.monitor.clone(), ws.dragging_slot_rect())
        };
        self.emit_snap_preview(&monitor, slot, "tiling");
    }

    pub fn on_window_maximize_request(&mut self, window: Window, event: &WindowMaximizeRequestEventSnapshot) {
        let workspace = self.find_workspace_for_window(window);
        if self.is_grabbing {
            return;
        }
        set(window, &WINDOW_STATE_MINIMIZED, false);
        clear_window_snap_state(window);

        if let Some(id) = workspace {
            let ws = self.workspace_by_id(id).expect("live");
            if ws.is_tiled && ws.should_tile(window) {
                set(window, &WINDOW_STATE_RESTORE_RECT, None);
                set(window, &WINDOW_STATE_MAXIMIZED, event.maximized);
                if event.maximized {
                    // Recenter even when already active: the tile is now as
                    // wide as the viewport.
                    self.ws(id).pan_to_window(window);
                    self.apply_workspace_stack_policy(Some(id));
                    window.focus();
                } else {
                    self.ws(id).apply_layout(LayoutOptions::default());
                    self.apply_workspace_stack_policy(Some(id));
                }
                return;
            }
        }

        // While fullscreen, the fullscreen rect owns the window: only track
        // the maximized flag (SDL2 drops maximized right after entering
        // fullscreen; animating that would show the shell through).
        if get(window, &WINDOW_STATE_FULLSCREEN) {
            if event.maximized
                && !get(window, &WINDOW_STATE_MAXIMIZED)
                && let Some(restore) = get(window, &WINDOW_STATE_FULLSCREEN_RESTORE_RECT)
            {
                set(window, &WINDOW_STATE_RESTORE_RECT, Some(restore));
            }
            if !event.maximized {
                set(window, &WINDOW_STATE_RESTORE_RECT, None);
            }
            set(window, &WINDOW_STATE_MAXIMIZED, event.maximized);
            return;
        }

        if !event.maximized {
            if let Some(restore) = get(window, &WINDOW_STATE_RESTORE_RECT) {
                if let Some(id) = workspace {
                    self.ws(id).sync_floating_window_rect(window, restore);
                }
                play_rect_animation(
                    window,
                    &WINDOW_STATE_RECT,
                    restore,
                    WINDOW_MANAGEMENT_EASING,
                    WINDOW_MANAGEMENT_ANIMATION_DURATION,
                );
            }
            set(window, &WINDOW_STATE_RESTORE_RECT, None);
            set(window, &WINDOW_STATE_MAXIMIZED, false);
            return;
        }

        if !get(window, &WINDOW_STATE_MAXIMIZED) {
            let current = get(window, &WINDOW_STATE_RECT);
            if current.width > 1.0 && current.height > 1.0 {
                set(window, &WINDOW_STATE_RESTORE_RECT, Some(current));
            }
        }
        let maximized = self.maximized_rect_for_window(window, None);
        if let Some(id) = workspace {
            self.ws(id).sync_floating_window_rect(window, maximized);
        }
        play_rect_animation(
            window,
            &WINDOW_STATE_RECT,
            maximized,
            WINDOW_MANAGEMENT_EASING,
            WINDOW_MANAGEMENT_ANIMATION_DURATION,
        );
        set(window, &WINDOW_STATE_MAXIMIZED, true);
        self.apply_workspace_stack_policy(workspace);
    }

    pub fn on_window_minimize_request(&mut self, window: Window, event: &WindowMinimizeRequestEventSnapshot) {
        // A client minimize while fullscreen is the X11 focus-loss reflex of
        // game toolkits (GLFW auto-iconify, SDL2 minimize-on-focus-loss), not
        // the user; honoring it drops the game out of its workspace.
        if event.minimized
            && event.source == WindowStateRequestSourceSnapshot::ClientCsd
            && get(window, &WINDOW_STATE_FULLSCREEN)
        {
            return;
        }
        self.set_minimized(window, event.minimized);
    }

    fn set_minimized(&mut self, window: Window, minimized: bool) {
        let was_minimized = get(window, &WINDOW_STATE_MINIMIZED);
        let workspace = self.find_workspace_for_window(window);
        if was_minimized != minimized {
            stop_rect_animation(window, &WINDOW_STATE_RECT);
            if !minimized {
                self.last_restore_at.insert(window, wall_clock_ms());
                set(window, &WINDOW_STATE_MINIMIZE_VISUAL_IDLE, false);
            }
            set(window, &WINDOW_STATE_MINIMIZED, minimized);
            if minimized {
                set(window, &WINDOW_STATE_MINIMIZE_VISUAL_IDLE, true);
            }
            schedule_minimize_animation(window, minimized, self.instant_transitions);
        }
        // A minimized window keeps the keyboard until something else is
        // focused: the compositor only elects a successor when the owner goes
        // away, and a minimized window is still mapped. Read after the state
        // flip so the window no longer counts as a tile or a visible candidate.
        let successor = if minimized && !was_minimized && window.is_focused().get_untracked() {
            workspace.and_then(|id| {
                self.workspace_by_id(id)
                    .expect("live")
                    .successor_for_minimized_window(window, |current| {
                        self.window_stack.z_index_value(current)
                    })
            })
        } else {
            None
        };
        if let Some(id) = workspace
            && self.workspace_by_id(id).expect("live").is_tiled
        {
            let ws = self.workspace_by_id(id).expect("live");
            let should_tile = ws.should_tile(window);
            let successor_tile = successor.filter(|successor| ws.should_tile(*successor));
            if !minimized && should_tile {
                self.ws(id).focus_window(window);
            } else if let Some(successor) = successor_tile {
                self.ws(id).focus_window(successor);
            } else {
                self.ws(id).apply_layout(LayoutOptions::default());
            }
            self.apply_workspace_stack_policy(Some(id));
        }
        if let Some(successor) = successor {
            successor.focus();
        }
    }

    pub fn on_window_activate_request(&mut self, window: Window, event: &WindowActivateRequestEventSnapshot) {
        let was_minimized = get(window, &WINDOW_STATE_MINIMIZED);
        if was_minimized {
            self.set_minimized(window, false);
        }
        let workspace = self.find_workspace_for_window(window);
        // Minimize-raise toggle: dock/taskbar sources only. Applications
        // raising themselves must never be minimized.
        let from_app = matches!(
            event.source,
            WindowActivateRequestSourceSnapshot::XdgActivation
                | WindowActivateRequestSourceSnapshot::Xwayland
                | WindowActivateRequestSourceSnapshot::Keybind
        );
        if !was_minimized && !from_app && self.minimize_on_reactivate(window, workspace) {
            return;
        }
        if let Some(id) = workspace {
            let (monitor, index) = {
                let ws = self.workspace_by_id(id).expect("live");
                (ws.monitor.clone(), ws.index)
            };
            self.switch_workspace_to(&monitor, index, false);
        }
        window.focus();
    }

    /// Re-activating the focused window from a dock minimizes it (floating
    /// workspaces, visible workspace, presented windows only).
    fn minimize_on_reactivate(&mut self, window: Window, workspace: Option<u64>) -> bool {
        if !self.presented_windows.contains(&window) {
            return false;
        }
        let Some(ws) = workspace.and_then(|id| self.workspace_by_id(id)) else {
            return false;
        };
        if ws.is_tiled || !ws.is_active() {
            return false;
        }
        if get(window, &WINDOW_STATE_MINIMIZED) || !window.is_focused().get_untracked() {
            return false;
        }
        if self
            .last_restore_at
            .get(&window)
            .is_some_and(|at| wall_clock_ms() - at <= RESTORE_ACTIVATE_GRACE_MS)
        {
            return false;
        }
        window.minimize();
        true
    }

    pub fn toggle_current_workspace_tiling(&mut self) {
        if let Some(id) = self.current_workspace() {
            let tiled = self.workspace_by_id(id).expect("live").is_tiled;
            self.ws(id).set_tiled(!tiled);
            self.apply_workspace_stack_policy(Some(id));
            self.restore_floating_focus_stacking(id);
        }
    }

    pub fn toggle_workspace_tiling_for_monitor(&mut self, monitor: &str) {
        self.sync_workspaces();
        if let Some(id) = self.workspace_for_monitor(monitor) {
            let tiled = self.workspace_by_id(id).expect("live").is_tiled;
            self.ws(id).set_tiled(!tiled);
            self.apply_workspace_stack_policy(Some(id));
            self.restore_floating_focus_stacking(id);
        }
    }

    /// After leaving tiling the focused window goes back on top (focusing an
    /// already-focused window emits no focus event).
    fn restore_floating_focus_stacking(&mut self, id: u64) {
        let ws = self.workspace_by_id(id).expect("live");
        if ws.is_tiled {
            return;
        }
        if let Some(focused) = ws.focused_window()
            && self.window_stack.has(focused)
        {
            self.window_stack.raise(focused);
        }
    }

    pub fn focus_tile(&mut self, direction: i32) {
        if let Some(id) = self.current_workspace()
            && self.workspace_by_id(id).expect("live").is_tiled
        {
            self.ws(id).focus_relative(direction);
            self.apply_workspace_stack_policy(Some(id));
        }
    }

    pub fn move_focused_tile(&mut self, direction: i32) {
        if let Some(id) = self.current_workspace()
            && self.workspace_by_id(id).expect("live").is_tiled
            && self.ws(id).move_focused_tile(direction)
        {
            self.apply_workspace_stack_policy(Some(id));
        }
    }

    pub fn move_focused_window_to_workspace(&mut self, direction: i32) {
        self.sync_workspaces();
        let focused = self
            .workspaces
            .iter()
            .find_map(|workspace| workspace.focused_window().map(|window| (workspace.id, window)));
        let Some((from, window)) = focused else {
            return;
        };
        let (monitor, from_index) = {
            let ws = self.workspace_by_id(from).expect("live");
            (ws.monitor.clone(), ws.index)
        };
        let target_index = (from_index as i64 + direction as i64).max(1) as u32;
        if target_index == from_index {
            return;
        }
        let target = self.ensure_workspace(&monitor, target_index);
        let Some(snapshot) = self.ws(from).take_window_for_move(window) else {
            return;
        };
        self.ws(target).add_moved_window(window, snapshot);
        self.ws(from).apply_layout(LayoutOptions::default());
        self.ws(target).apply_layout(LayoutOptions::default());
        self.switch_workspace_to(&monitor, target_index, false);
        if self.workspace_by_id(target).expect("live").is_tiled {
            self.ws(target).pan_to_window(window);
        }
        window.focus();
        self.apply_workspace_stack_policy(Some(from));
        self.apply_workspace_stack_policy(Some(target));
        self.sync_workspace_visibility();
    }

    pub fn close_focused_window(&self) {
        if let Some(window) = self.workspaces.iter().find_map(Workspace::focused_window) {
            window.close();
        }
    }

    pub fn toggle_focused_window_maximize(&self) {
        for workspace in &self.workspaces {
            let Some(focused) = workspace.focused_window() else {
                continue;
            };
            if !focused.is_resizable().get_untracked() {
                continue;
            }
            if get(focused, &WINDOW_STATE_MAXIMIZED) {
                focused.unmaximize();
            } else {
                focused.maximize();
            }
            return;
        }
    }

    /// Compositor-side fullscreen toggle: the escape hatch for Wine games
    /// whose "windowed" size equals the desktop and so never leave
    /// fullscreen on their own.
    pub fn toggle_focused_window_fullscreen(&self) {
        if let Some(focused) = self.workspaces.iter().find_map(Workspace::focused_window) {
            if get(focused, &WINDOW_STATE_FULLSCREEN) {
                focused.unfullscreen();
            } else {
                focused.fullscreen();
            }
        }
    }

    pub fn refresh_usable_area_layouts(&mut self) {
        self.sync_workspaces();
        // Re-applying during a drag would clobber it (a maximized window would
        // flash back to its full rect whenever a layer surface maps).
        if self.is_grabbing {
            return;
        }
        for workspace in &mut self.workspaces {
            workspace.refresh_usable_area_layout();
        }
        self.sync_workspace_visibility();
    }

    pub fn switch_workspace(&mut self, direction: i32) {
        let monitor = if self.current_monitor.is_empty() {
            output_list().into_iter().next().unwrap_or_default()
        } else {
            self.current_monitor.clone()
        };
        if monitor.is_empty() {
            return;
        }
        let current = self.active_index(&monitor);
        self.switch_workspace_to(&monitor, (current as i64 + direction as i64).max(1) as u32, true);
    }

    /// Animated switch to `target_index` on `monitor`, sliding in the
    /// direction implied by the current index.
    pub fn switch_workspace_to(&mut self, monitor: &str, target_index: u32, focus_active_after: bool) {
        self.switch_workspace_to_with(monitor, target_index, focus_active_after, false);
    }

    /// [`switch_workspace_to`](Self::switch_workspace_to); `instant` jumps
    /// there without the slide.
    pub fn switch_workspace_to_with(
        &mut self,
        monitor: &str,
        target_index: u32,
        focus_active_after: bool,
        instant: bool,
    ) {
        self.workspace_gesture = None;
        self.sync_workspaces();
        if monitor.is_empty() || target_index < 1 {
            return;
        }
        let current = self.active_index(monitor);
        if target_index == current {
            return;
        }
        let direction = if target_index > current { 1.0 } else { -1.0 };
        let from = self.ensure_workspace(monitor, current);
        let to = self.ensure_workspace(monitor, target_index);
        let distance = workspace_viewport_rect(monitor).height;

        self.set_active_index(monitor, target_index);
        self.current_monitor = monitor.to_owned();
        for workspace in &mut self.workspaces {
            if workspace.id == from || workspace.id == to {
                continue;
            }
            let active = workspace.is_active();
            workspace.set_visible(active);
        }

        if instant {
            self.ws(from).show_instantly(false);
            self.ws(to).show_instantly(true);
            self.ws(to).apply_layout(LayoutOptions {
                animate: Some(false),
                ..LayoutOptions::default()
            });
        } else {
            self.ws(from).animate_workspace_transition(WorkspaceTransition {
                from_offset_y: 0.0,
                to_offset_y: -direction * distance,
                from_opacity: 1.0,
                to_opacity: 0.0,
                visible_after: false,
            });
            self.ws(to).prepare_workspace_transition(direction * distance, 0.0);
            self.ws(to).apply_layout(LayoutOptions::default());
            self.ws(to).animate_workspace_transition(WorkspaceTransition {
                from_offset_y: direction * distance,
                to_offset_y: 0.0,
                from_opacity: 0.0,
                to_opacity: 1.0,
                visible_after: true,
            });
        }
        // Callers focusing a different window afterwards opt out, so the
        // resulting focus callbacks do not stomp on their pan.
        if focus_active_after {
            self.ws(to).focus_active_window();
        }
        self.apply_workspace_stack_policy(Some(from));
        self.apply_workspace_stack_policy(Some(to));
        self.outbox.workspaces_changed = true;
    }

    fn current_workspace(&mut self) -> Option<u64> {
        self.sync_workspaces();
        let monitor = self.current_monitor.clone();
        self.workspace_for_monitor(&monitor)
            .or_else(|| self.workspaces.first().map(|workspace| workspace.id))
    }

    /// Monitor under the cursor.
    pub fn current_monitor_name(&mut self) -> String {
        self.sync_workspaces();
        if self.current_monitor.is_empty() {
            output_list().into_iter().next().unwrap_or_default()
        } else {
            self.current_monitor.clone()
        }
    }

    /// Per-monitor view of the workspaces for the bar.
    pub fn view_for_ipc(&self) -> WorkspacesView {
        let mut by_monitor: BTreeMap<String, Vec<WorkspaceView>> = BTreeMap::new();
        for workspace in &self.workspaces {
            let active = self.active_index(&workspace.monitor) == workspace.index;
            by_monitor
                .entry(workspace.monitor.clone())
                .or_default()
                .push(WorkspaceView {
                    index: workspace.index,
                    window_count: workspace.window_count(),
                    is_tiled: workspace.is_tiled,
                    active,
                    windows: workspace
                        .list_windows()
                        .into_iter()
                        .map(|window| {
                            let id = window.id();
                            WindowView {
                                last_focused_at: self.last_focused_at.get(&id).copied().unwrap_or(0.0),
                                id,
                                app_id: window.app_id().get_untracked(),
                                title: window.title().get_untracked(),
                                focused: window.is_focused().get_untracked(),
                            }
                        })
                        .collect(),
                });
        }
        let monitors = output_list()
            .into_iter()
            .map(|name| {
                let active = self.active_index(&name);
                // Workspaces with windows, plus the active one even when empty.
                let mut workspaces: Vec<WorkspaceView> = by_monitor
                    .remove(&name)
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|workspace| workspace.window_count > 0 || workspace.active)
                    .collect();
                if !workspaces.iter().any(|workspace| workspace.index == active) {
                    workspaces.push(WorkspaceView {
                        index: active,
                        window_count: 0,
                        is_tiled: false,
                        active: true,
                        windows: Vec::new(),
                    });
                }
                workspaces.sort_by_key(|workspace| workspace.index);
                MonitorView {
                    name,
                    active,
                    workspaces,
                }
            })
            .collect();
        WorkspacesView {
            current_monitor: self.current_monitor.clone(),
            monitors,
        }
    }

    /// MRU stamp for the dock.
    pub fn record_focus(&mut self, window: Window) {
        self.last_focused_at.insert(window.id(), wall_clock_ms());
    }

    fn track_pending_initial_focus(&mut self, window: Window) {
        let token = wall_clock_ms();
        self.pending_initial_focus.insert(window, token);
        let handle = self.handle.clone();
        set_timeout(WINDOW_MANAGEMENT_ANIMATION_DURATION, move || {
            if let Some(manager) = handle.upgrade() {
                manager.with(|wm| {
                    if wm.pending_initial_focus.get(&window) == Some(&token) {
                        wm.pending_initial_focus.remove(&window);
                    }
                });
            }
        });
    }

    fn should_defer_focus_layout_for_initial_open(&mut self, window: Window, workspace: Option<u64>) -> bool {
        let Some(ws) = workspace.and_then(|id| self.workspace_by_id(id)) else {
            return false;
        };
        if !ws.is_tiled || !ws.is_active() {
            return false;
        }
        if self.pending_initial_focus.remove(&window).is_some() {
            return false;
        }
        let ws = self.workspace_by_id(workspace.expect("checked")).expect("live");
        self.pending_initial_focus
            .keys()
            .any(|pending| ws.is_active_window(*pending) && ws.has_window(*pending))
    }

    pub fn find_window_by_id(&self, id: &str) -> Option<Window> {
        self.workspaces
            .iter()
            .find_map(|workspace| workspace.find_window_by_id(id))
    }

    /// "Go to this window" for the dock: unminimize, switch workspace, pan,
    /// focus. Returns whether the window exists.
    pub fn activate_window_by_id(&mut self, id: &str) -> bool {
        self.activate_window_by_id_with(id, false)
    }

    /// [`activate_window_by_id`](Self::activate_window_by_id) without the
    /// window manager's own animations and without the minimize-raise toggle:
    /// the caller (e.g. the Flip 3D switcher) animates the change itself.
    pub fn activate_window_by_id_instant(&mut self, id: &str) -> bool {
        self.activate_window_by_id_with(id, true)
    }

    fn activate_window_by_id_with(&mut self, id: &str, instant: bool) -> bool {
        let Some(window) = self.find_window_by_id(id) else {
            return false;
        };
        let Some(workspace) = self.find_workspace_for_window(window) else {
            return false;
        };
        if get(window, &WINDOW_STATE_MINIMIZED) {
            self.instant_transitions = instant;
            self.set_minimized(window, false);
            self.instant_transitions = false;
        } else if !instant && self.minimize_on_reactivate(window, Some(workspace)) {
            return true;
        }
        let (monitor, index, tiled) = {
            let ws = self.workspace_by_id(workspace).expect("live");
            (ws.monitor.clone(), ws.index, ws.is_tiled)
        };
        self.switch_workspace_to_with(&monitor, index, false, instant);
        if tiled {
            self.ws(workspace).pan_to_window_with(window, !instant);
        }
        window.focus();
        true
    }

    /// Every managed window across all workspaces, most recently focused
    /// first (the focused window leads).
    pub fn list_windows_by_recent_use(&self) -> Vec<Window> {
        let last_use = |window: Window| {
            if window.is_focused().get_untracked() {
                f64::INFINITY
            } else {
                self.last_focused_at.get(&window.id()).copied().unwrap_or(0.0)
            }
        };
        let mut windows: Vec<(usize, Window, f64)> = self
            .list_windows()
            .into_iter()
            .enumerate()
            .map(|(order, window)| (order, window, last_use(window)))
            .collect();
        windows.sort_by(|a, b| b.2.total_cmp(&a.2).then(a.0.cmp(&b.0)));
        windows.into_iter().map(|(_, window, _)| window).collect()
    }

    /// Whether `window` is on screen on `monitor` right now: its workspace is
    /// the active one there and it is not minimized.
    pub fn is_window_shown_on(&self, window: Window, monitor: &str) -> bool {
        self.find_workspace_for_window(window)
            .and_then(|id| self.workspace_by_id(id))
            .is_some_and(|workspace| {
                workspace.monitor == monitor
                    && workspace.is_active()
                    && !get(window, &WINDOW_STATE_MINIMIZED)
            })
    }

    /// Every managed window across all workspaces.
    pub fn list_windows(&self) -> Vec<Window> {
        self.workspaces
            .iter()
            .flat_map(|workspace| workspace.list_windows())
            .collect()
    }

    pub fn activate(&mut self, monitor: &str, index: u32) {
        if !monitor.is_empty() && index >= 1 {
            self.switch_workspace_to(monitor, index, true);
        }
    }

    fn begin_interactive_unmaximize(&mut self, window: Window) -> bool {
        if !get(window, &WINDOW_STATE_MAXIMIZED) {
            return false;
        }
        set(window, &WINDOW_STATE_MAXIMIZED, false);
        set(window, &WINDOW_STATE_RESTORE_RECT, None);
        clear_window_snap_state(window);
        window.unmaximize();
        true
    }

    /// Floating windows stay above tiles on a tiled workspace. Floating
    /// workspaces stack purely by focus order, which `on_focus` maintains.
    fn apply_workspace_stack_policy(&mut self, workspace: Option<u64>) {
        let Some(ws) = workspace.and_then(|id| self.workspace_by_id(id)) else {
            return;
        };
        if !ws.is_tiled {
            return;
        }
        let mut floating: Vec<Window> = ws
            .floating_windows()
            .into_iter()
            .filter(|window| self.window_stack.has(*window))
            .collect();
        floating.sort_by_key(|window| self.window_stack.z_index_value(*window));
        for window in floating {
            self.window_stack.raise(window);
        }
    }

    fn sync_workspaces(&mut self) {
        let outputs = output_list();
        for monitor in &outputs {
            let index = self.active_index(monitor);
            self.set_active_index(monitor, index);
            self.ensure_workspace(monitor, index);
        }
        if self.current_monitor.is_empty() || !outputs.contains(&self.current_monitor) {
            self.current_monitor = outputs.first().cloned().unwrap_or_default();
        }
    }

    fn workspace_for_monitor(&mut self, monitor: &str) -> Option<u64> {
        if monitor.is_empty() {
            return None;
        }
        let index = self.active_index(monitor);
        Some(self.ensure_workspace(monitor, index))
    }

    fn ensure_workspace(&mut self, monitor: &str, index: u32) -> u64 {
        if let Some(position) = self.position(monitor, index) {
            return self.workspaces[position].id;
        }
        let id = self.next_workspace_id;
        self.next_workspace_id += 1;
        self.workspaces.push(Workspace::new(
            id,
            index,
            monitor,
            self.natural_root_rect,
            self.active_workspace_by_monitor.clone(),
            self.handle.clone(),
        ));
        id
    }

    fn gesture_monitor(&self, event: &GestureSwipeEventSnapshot) -> String {
        let outputs = output_list();
        if let Some(output) = event.output_name.as_ref().filter(|name| outputs.contains(name)) {
            return output.clone();
        }
        if self.current_monitor.is_empty() {
            outputs.into_iter().next().unwrap_or_default()
        } else {
            self.current_monitor.clone()
        }
    }

    fn resolve_workspace_gesture_mode(&mut self, event: &GestureSwipeEventSnapshot) -> Option<GestureMode> {
        if self.workspace_gesture_mode.is_some() {
            return self.workspace_gesture_mode;
        }
        let abs_y = (event.total_y * self.gesture_speed.workspace_switch_factor).abs();
        let scaled_abs_x = event.total_x.abs() * self.gesture_speed.workspace_scroll_factor;
        if scaled_abs_x.max(abs_y) < WORKSPACE_GESTURE_AXIS_LOCK_PX {
            return None;
        }
        let mode = if scaled_abs_x > abs_y {
            self.workspace_scroll_gesture_rect_animations_cancelled = false;
            GestureMode::WorkspaceScroll
        } else {
            GestureMode::WorkspaceSwitch
        };
        self.workspace_gesture_mode = Some(mode);
        Some(mode)
    }

    fn update_workspace_scroll_gesture(&mut self, event: &GestureSwipeEventSnapshot) {
        let monitor = self.gesture_monitor(event);
        let Some(id) = self.workspace_for_monitor(&monitor) else {
            return;
        };
        if !self.workspace_by_id(id).expect("live").is_tiled {
            return;
        }
        self.current_monitor = monitor.clone();
        self.ws(id).stop_kinetic_scroll();
        let mut delta_x = -event.delta_x * self.gesture_speed.workspace_scroll_factor;

        // libinput's instantaneous velocity is noisy; one slow sample
        // mid-flick must not fake a below-threshold catch.
        let event_speed = event.velocity_x.abs() * self.gesture_speed.workspace_scroll_factor;
        let speed = match self.workspace_scroll_gesture_speed {
            None => event_speed,
            Some(previous) => previous * 0.6 + event_speed * 0.4,
        };
        self.workspace_scroll_gesture_speed = Some(speed);

        if let Some(latch) = &mut self.workspace_scroll_snap_latch {
            // Caught on a snap position: swallow travel until it exceeds the
            // breakout distance, then resume with the excess.
            latch.overshoot += delta_x;
            let breakout = self.gesture_speed.workspace_scroll_snap_breakout_px;
            if latch.overshoot.abs() <= breakout {
                return;
            }
            delta_x = latch.overshoot - latch.overshoot.signum() * breakout;
            self.workspace_scroll_snap_latch = None;
        }

        let snap_max = self.gesture_speed.workspace_scroll_snap_max_velocity;
        if self.workspace_scroll_snap_latch.is_none() && snap_max > 0.0 && speed <= snap_max {
            let ws = self.workspace_by_id(id).expect("live");
            let from = ws.scroll_position();
            if let Some(snap) = ws.snap_offset_between(from, from + delta_x) {
                delta_x = snap - from;
                self.workspace_scroll_snap_latch = Some(ScrollSnapLatch { overshoot: 0.0 });
            }
        }

        let cancel = !self.workspace_scroll_gesture_rect_animations_cancelled;
        let scrolled = self.ws(id).scroll_by(delta_x, false, cancel);
        if scrolled && cancel {
            self.workspace_scroll_gesture_rect_animations_cancelled = true;
        }
        let position = event.position.or(self.last_pointer_position);
        self.focus_window_at_pointer_position(position, Some(&monitor));
        self.apply_workspace_stack_policy(Some(id));
    }

    fn finish_workspace_scroll_gesture(&mut self, event: &GestureSwipeEventSnapshot) {
        if event.phase != GestureSwipePhaseSnapshot::End {
            return;
        }
        let monitor = self.gesture_monitor(event);
        let Some(id) = self.workspace_for_monitor(&monitor) else {
            return;
        };
        if !self.workspace_by_id(id).expect("live").is_tiled {
            return;
        }
        // Fingers lifted while caught: stay caught, no glide.
        if self.workspace_scroll_snap_latch.take().is_some() {
            self.workspace_scroll_gesture_speed = None;
            return;
        }
        self.workspace_scroll_gesture_speed = None;
        self.kinetic_frame_context
            .insert(id, (event.position.or(self.last_pointer_position), monitor));
        let velocity = -event.velocity_x * self.gesture_speed.workspace_scroll_kinetic_factor;
        let snap_max = self.gesture_speed.workspace_scroll_snap_max_velocity;
        let moved = self.ws(id).start_kinetic_scroll(velocity, snap_max);
        if moved {
            self.kinetic_on_frame(id);
        }
    }

    fn kinetic_frame(&mut self, id: u64, token: u64) {
        let moved = match self.workspace_by_id_mut(id) {
            Some(workspace) => workspace.kinetic_tick(token),
            None => false,
        };
        if moved {
            self.kinetic_on_frame(id);
        }
    }

    fn kinetic_on_frame(&mut self, id: u64) {
        if let Some((position, monitor)) = self.kinetic_frame_context.get(&id).cloned() {
            self.focus_window_at_pointer_position(position, Some(&monitor));
        }
        self.apply_workspace_stack_policy(Some(id));
    }

    fn update_workspace_gesture(&mut self, event: &GestureSwipeEventSnapshot) {
        let monitor = self.gesture_monitor(event);
        if monitor.is_empty() {
            return;
        }
        let distance = workspace_viewport_rect(&monitor).height.max(1.0);
        let raw_offset_y = clamp(
            event.total_y * self.gesture_speed.workspace_switch_factor,
            -distance,
            distance,
        );
        if raw_offset_y.abs() < 1.0 {
            return;
        }
        let direction = if raw_offset_y < 0.0 { 1 } else { -1 };
        let current_index = self.active_index(&monitor);
        let next_index = current_index as i64 + direction as i64;
        let from = self.ensure_workspace(&monitor, current_index);
        let to = (next_index >= 1).then(|| self.ensure_workspace(&monitor, next_index as u32));
        let target_changed = self.workspace_gesture.as_ref().is_none_or(|gesture| {
            gesture.monitor != monitor || gesture.current_index != current_index || gesture.to != to
        });
        self.current_monitor = monitor.clone();

        let Some(to) = to else {
            if target_changed {
                for workspace in &mut self.workspaces {
                    if workspace.id != from {
                        let active = workspace.is_active();
                        workspace.set_visible(active);
                    }
                }
            }
            let resistance = raw_offset_y * 0.25;
            self.ws(from).set_workspace_gesture_visual(resistance, 1.0);
            self.workspace_gesture = Some(WorkspaceGesture {
                monitor,
                current_index,
                direction,
                distance,
                from,
                to: None,
                from_offset_y: resistance,
                to_offset_y: direction as f64 * distance,
                from_opacity: 1.0,
                to_opacity: 0.0,
            });
            return;
        };

        let progress = clamp(raw_offset_y.abs() / distance, 0.0, 1.0);
        let to_offset_y = direction as f64 * distance + raw_offset_y;
        let from_opacity = 1.0 - progress;
        let to_opacity = progress;
        if target_changed {
            for workspace in &mut self.workspaces {
                if workspace.id != from && workspace.id != to {
                    let active = workspace.is_active();
                    workspace.set_visible(active);
                }
            }
            // Snap, don't animate: the finger drives the slide through a
            // static offset, and a rect animation overrides the whole rect —
            // offset included — so the tiles would sit at their final position
            // until it ended. (Keyboard switching animates the offset itself.)
            self.ws(to).apply_layout(LayoutOptions {
                animate: Some(false),
                ..LayoutOptions::default()
            });
        }
        self.ws(from).set_workspace_gesture_visual(raw_offset_y, from_opacity);
        self.ws(to).set_workspace_gesture_visual(to_offset_y, to_opacity);
        self.apply_workspace_stack_policy(Some(from));
        self.apply_workspace_stack_policy(Some(to));
        self.workspace_gesture = Some(WorkspaceGesture {
            monitor,
            current_index,
            direction,
            distance,
            from,
            to: Some(to),
            from_offset_y: raw_offset_y,
            to_offset_y,
            from_opacity,
            to_opacity,
        });
    }

    fn finish_workspace_gesture(&mut self, event: &GestureSwipeEventSnapshot) {
        let Some(gesture) = self.workspace_gesture.take() else {
            return;
        };
        let commit = event.phase == GestureSwipePhaseSnapshot::End
            && gesture.to.is_some()
            && ((event.total_y * self.gesture_speed.workspace_switch_factor).abs()
                >= gesture.distance * WORKSPACE_GESTURE_THRESHOLD_RATIO
                || (event.velocity_y * self.gesture_speed.workspace_switch_velocity_factor).abs()
                    >= WORKSPACE_GESTURE_VELOCITY_THRESHOLD);
        let direction = gesture.direction as f64;

        if let (true, Some(to)) = (commit, gesture.to) {
            self.set_active_index(
                &gesture.monitor,
                (gesture.current_index as i64 + gesture.direction as i64) as u32,
            );
            self.current_monitor = gesture.monitor.clone();
            self.ws(gesture.from).animate_workspace_transition(WorkspaceTransition {
                from_offset_y: gesture.from_offset_y,
                to_offset_y: -direction * gesture.distance,
                from_opacity: gesture.from_opacity,
                to_opacity: 0.0,
                visible_after: false,
            });
            self.ws(to).animate_workspace_transition(WorkspaceTransition {
                from_offset_y: gesture.to_offset_y,
                to_offset_y: 0.0,
                from_opacity: gesture.to_opacity,
                to_opacity: 1.0,
                visible_after: true,
            });
            self.ws(to).focus_active_window();
            self.apply_workspace_stack_policy(Some(gesture.from));
            self.apply_workspace_stack_policy(Some(to));
            self.outbox.workspaces_changed = true;
            return;
        }

        self.ws(gesture.from).animate_workspace_transition(WorkspaceTransition {
            from_offset_y: gesture.from_offset_y,
            to_offset_y: 0.0,
            from_opacity: gesture.from_opacity,
            to_opacity: 1.0,
            visible_after: true,
        });
        if let Some(to) = gesture.to {
            self.ws(to).animate_workspace_transition(WorkspaceTransition {
                from_offset_y: gesture.to_offset_y,
                to_offset_y: direction * gesture.distance,
                from_opacity: gesture.to_opacity,
                to_opacity: 0.0,
                visible_after: false,
            });
        }
        self.apply_workspace_stack_policy(Some(gesture.from));
    }

    fn focus_window_at_pointer_target(&mut self, target: &PointerHitTargetSnapshot, monitor_hint: Option<&str>) {
        let PointerHitTargetSnapshot::Window { window_id } = target else {
            return;
        };
        let Some((id, window)) = self
            .workspaces
            .iter()
            .find_map(|workspace| workspace.find_window_by_id(window_id).map(|window| (workspace.id, window)))
        else {
            return;
        };
        let ws = self.workspace_by_id(id).expect("live");
        if !ws.is_tiled || !ws.is_active() {
            return;
        }
        if self.ws(id).focus_window_under_pointer(window).is_none() {
            return;
        }
        self.current_monitor = match monitor_hint.filter(|hint| output_list().iter().any(|name| name == hint)) {
            Some(hint) => hint.to_owned(),
            None => self.workspace_by_id(id).expect("live").monitor.clone(),
        };
    }

    fn focus_window_at_pointer_position(
        &mut self,
        position: Option<PointerMovePointSnapshot>,
        monitor_hint: Option<&str>,
    ) {
        let Some(position) = position else {
            return;
        };
        if matches!(self.last_pointer_target, PointerHitTargetSnapshot::Layer { .. }) {
            return;
        }
        let outputs = output_list();
        let monitor = match monitor_hint.filter(|hint| outputs.iter().any(|name| name == hint)) {
            Some(hint) => hint.to_owned(),
            None => output_name_at(position.x, position.y).unwrap_or_else(|| self.current_monitor.clone()),
        };
        let Some(id) = self.workspace_for_monitor(&monitor) else {
            return;
        };
        let ws = self.workspace_by_id(id).expect("live");
        if !ws.is_tiled || !ws.is_active() {
            return;
        }
        let mut candidates: Vec<Window> = ws
            .list_windows()
            .into_iter()
            .filter(|window| {
                !get(*window, &WINDOW_STATE_MINIMIZED)
                    && self.window_stack.has(*window)
                    && get(*window, &WINDOW_STATE_RECT).contains(
                        position.x,
                        position.y - get(*window, &WINDOW_STATE_WORKSPACE_OFFSET_Y),
                    )
            })
            .collect();
        candidates.sort_by_key(|window| std::cmp::Reverse(self.window_stack.z_index_value(*window)));
        let Some(window) = candidates.first().copied() else {
            return;
        };
        if self.ws(id).focus_window_under_pointer(window).is_some() {
            self.current_monitor = monitor;
        }
    }

    fn available_workspace_index(&self, monitor: &str, preferred: u32) -> u32 {
        if self.position(monitor, preferred).is_none() {
            return preferred;
        }
        let mut index = 1;
        while self.position(monitor, index).is_some() {
            index += 1;
        }
        index
    }

    fn sync_workspace_visibility(&mut self) {
        for workspace in &mut self.workspaces {
            let active = workspace.is_active();
            workspace.set_visible(active);
        }
    }

    fn find_workspace_for_window(&self, window: Window) -> Option<u64> {
        self.workspaces
            .iter()
            .find(|workspace| workspace.has_window(window))
            .map(|workspace| workspace.id)
    }

    fn find_workspace_restoring_window(&self, window: Window) -> Option<u64> {
        self.workspaces
            .iter()
            .find(|workspace| workspace.is_restoring_window(window))
            .map(|workspace| workspace.id)
    }

    /// The workspace a dragged window belongs to now; Shift at the top or
    /// bottom edge switches workspaces while dragging.
    fn workspace_for_drag(&mut self, event: &WindowMoveEventSnapshot, floating: bool) -> u64 {
        let drag_workspace = if floating {
            self.floating_drag.as_ref().map(|drag| drag.workspace)
        } else {
            self.tile_drag.as_ref().map(|drag| drag.workspace)
        }
        .expect("a drag is in progress");
        let last_switch = if floating {
            self.floating_drag.as_ref().map(|drag| drag.last_workspace_switch_at)
        } else {
            self.tile_drag.as_ref().map(|drag| drag.last_workspace_switch_at)
        }
        .unwrap_or(0);
        let monitor = match event
            .output_name
            .as_ref()
            .filter(|name| output_list().contains(name))
        {
            Some(name) => name.clone(),
            None => self.workspace_by_id(drag_workspace).expect("live").monitor.clone(),
        };
        let mut index = self.active_index(&monitor);
        let edge = tile_drag_workspace_edge_direction(&monitor, event.current_pointer.y);
        if event.modifiers.shift
            && edge != 0
            && event.timestamp.saturating_sub(last_switch) >= TILE_DRAG_WORKSPACE_SWITCH_INTERVAL_MS
        {
            let next = (index as i64 + edge as i64).max(1) as u32;
            if next != index {
                self.current_monitor = monitor.clone();
                self.switch_workspace(edge);
                let drag = if floating { &mut self.floating_drag } else { &mut self.tile_drag };
                if let Some(drag) = drag {
                    drag.last_workspace_switch_at = event.timestamp;
                }
                index = self.active_index(&monitor);
            }
        }
        self.ensure_workspace(&monitor, index)
    }

    fn constrain_resize_rect(&self, window: Window, event: &WindowResizeEventSnapshot) -> Rect {
        let constraints = window.size_constraints().get_untracked();
        let (extra_width, extra_height) = self.client_to_root_size_extra(window);
        let min_width = constraints.min.map_or(1.0, |min| (min.width as f64).max(1.0)) + extra_width;
        let min_height = constraints.min.map_or(1.0, |min| (min.height as f64).max(1.0)) + extra_height;
        let max_width = match constraints.max.map(|max| max.width) {
            Some(max) if max > 0 => max as f64 + extra_width,
            _ => f64::INFINITY,
        };
        let max_height = match constraints.max.map(|max| max.height) {
            Some(max) if max > 0 => max as f64 + extra_height,
            _ => f64::INFINITY,
        };
        let width = clamp(event.current_rect.width, min_width, min_width.max(max_width));
        let height = clamp(event.current_rect.height, min_height, min_height.max(max_height));
        let x = if event.edges.left {
            event.start_rect.x + event.start_rect.width - width
        } else {
            event.current_rect.x
        };
        let y = if event.edges.top {
            event.start_rect.y + event.start_rect.height - height
        } else {
            event.current_rect.y
        };
        Rect::new(x, y, width, height)
    }

    fn client_to_root_size_extra(&self, window: Window) -> (f64, f64) {
        let natural = (self.natural_root_rect)(window);
        let client = window.position().get_untracked();
        (
            (natural.width - client.width).max(0.0),
            (natural.height - client.height).max(0.0),
        )
    }

    fn maximized_rect_for_window(&self, window: Window, preferred: Option<&str>) -> Rect {
        let rect = get(window, &WINDOW_STATE_RECT);
        let output = preferred
            .map(str::to_owned)
            .or_else(|| output_name_at(rect.center_x(), rect.center_y()))
            .or_else(|| (!self.current_monitor.is_empty()).then(|| self.current_monitor.clone()));
        maximized_rect_on(window, output.as_deref())
    }

    /// Fullscreen covers the whole output (no usable-area inset, no
    /// padding), which also lets the tty backend scan the client out.
    fn fullscreen_rect_for_window(&self, window: Window, preferred: Option<&str>) -> Rect {
        let rect = get(window, &WINDOW_STATE_RECT);
        let output = preferred
            .map(str::to_owned)
            .or_else(|| output_name_at(rect.center_x(), rect.center_y()))
            .or_else(|| (!self.current_monitor.is_empty()).then(|| self.current_monitor.clone()));
        output.and_then(|output| output_rect(&output)).unwrap_or(rect)
    }

    pub fn on_window_fullscreen_request(&mut self, window: Window, event: &WindowFullscreenRequestEventSnapshot) {
        if self.is_grabbing {
            return;
        }
        let workspace = self.find_workspace_for_window(window);
        set(window, &WINDOW_STATE_MINIMIZED, false);
        clear_window_snap_state(window);

        if !event.fullscreen {
            let restore = get(window, &WINDOW_STATE_FULLSCREEN_RESTORE_RECT);
            set(window, &WINDOW_STATE_FULLSCREEN, false);
            set(window, &WINDOW_STATE_FULLSCREEN_RESTORE_RECT, None);
            // A tile returns to its slot; a floating window animates back.
            if let Some(id) = workspace {
                let ws = self.workspace_by_id(id).expect("live");
                if ws.is_tiled && ws.should_tile(window) {
                    self.ws(id).apply_layout(LayoutOptions::default());
                    self.apply_workspace_stack_policy(Some(id));
                    return;
                }
            }
            let target = if get(window, &WINDOW_STATE_MAXIMIZED) {
                Some(self.maximized_rect_for_window(window, None))
            } else {
                restore
            };
            if let Some(target) = target {
                if let Some(id) = workspace {
                    self.ws(id).sync_floating_window_rect(window, target);
                }
                play_rect_animation(
                    window,
                    &WINDOW_STATE_RECT,
                    target,
                    WINDOW_MANAGEMENT_EASING,
                    WINDOW_MANAGEMENT_ANIMATION_DURATION,
                );
            }
            self.apply_workspace_stack_policy(workspace);
            return;
        }

        if !get(window, &WINDOW_STATE_FULLSCREEN) {
            let current = get(window, &WINDOW_STATE_RECT);
            if current.width > 1.0 && current.height > 1.0 {
                set(window, &WINDOW_STATE_FULLSCREEN_RESTORE_RECT, Some(current));
            }
        }
        let rect = self.fullscreen_rect_for_window(window, event.output_name.as_deref());
        set(window, &WINDOW_STATE_FULLSCREEN, true);
        if let Some(id) = workspace {
            self.ws(id).focus_window(window);
            self.ws(id).sync_floating_window_rect(window, rect);
        }
        play_rect_animation(
            window,
            &WINDOW_STATE_RECT,
            rect,
            WINDOW_MANAGEMENT_EASING,
            WINDOW_MANAGEMENT_ANIMATION_DURATION,
        );
        self.apply_workspace_stack_policy(workspace);
        window.focus();
    }

    fn initial_restore_rect_for_maximized_window(&self, window: Window) -> Rect {
        let maximized = self.maximized_rect_for_window(window, None);
        let width = (maximized.width * 0.7).max(1.0);
        let height = (maximized.height * 0.7).max(1.0);
        Rect::new(
            maximized.x + (maximized.width - width) / 2.0,
            maximized.y + (maximized.height - height) / 2.0,
            width,
            height,
        )
    }

    // ----------------------------------------------------------------------
    // Snap zones: Windows-style edge snapping for floating drags, plus the
    // tiling drag slot preview. The bar draws the preview; this side picks
    // the zone, broadcasts it and applies the snap on drop.
    // ----------------------------------------------------------------------

    /// Usable area inset by the maximize padding: the base of snap rects.
    fn monitor_snap_base_rect(&self, monitor: &str) -> Option<Rect> {
        COMPOSITOR
            .layer
            .usable_area(monitor)
            .or_else(|| output_rect(monitor))
            .map(inset_maximized)
    }

    fn floating_snap_zone_at(&self, monitor: &str, x: f64, y: f64) -> Option<SnapZone> {
        let full = output_rect(monitor)?;
        let (left, top, right, bottom) = (full.x, full.y, full.right(), full.bottom());
        let near_left = x <= left + SNAP_EDGE_PX;
        let near_right = x >= right - SNAP_EDGE_PX;
        let near_top = y <= top + SNAP_EDGE_PX;
        // Corners win over edges so the quarters stay reachable.
        if near_left && y <= top + SNAP_CORNER_PX {
            return Some(SnapZone::TopLeft);
        }
        if near_left && y >= bottom - SNAP_CORNER_PX {
            return Some(SnapZone::BottomLeft);
        }
        if near_right && y <= top + SNAP_CORNER_PX {
            return Some(SnapZone::TopRight);
        }
        if near_right && y >= bottom - SNAP_CORNER_PX {
            return Some(SnapZone::BottomRight);
        }
        if near_top {
            return Some(SnapZone::Maximize);
        }
        if near_left {
            return Some(SnapZone::Left);
        }
        if near_right {
            return Some(SnapZone::Right);
        }
        None
    }

    fn snap_zone_rect(&self, monitor: &str, zone: SnapZone) -> Option<Rect> {
        let base = self.monitor_snap_base_rect(monitor)?;
        let half_w = (base.width - SNAP_GAP_PX) / 2.0;
        let half_h = (base.height - SNAP_GAP_PX) / 2.0;
        let right_x = base.x + half_w + SNAP_GAP_PX;
        let bottom_y = base.y + half_h + SNAP_GAP_PX;
        Some(match zone {
            SnapZone::Maximize => base,
            SnapZone::Left => Rect::new(base.x, base.y, half_w, base.height),
            SnapZone::Right => Rect::new(right_x, base.y, half_w, base.height),
            SnapZone::TopLeft => Rect::new(base.x, base.y, half_w, half_h),
            SnapZone::TopRight => Rect::new(right_x, base.y, half_w, half_h),
            SnapZone::BottomLeft => Rect::new(base.x, bottom_y, half_w, half_h),
            SnapZone::BottomRight => Rect::new(right_x, bottom_y, half_w, half_h),
        })
    }

    /// Resizing a snapped window moves the shared split lines of every
    /// window snapped on the same monitor.
    fn resize_floating_snap_layout(
        &mut self,
        window: Window,
        event: &WindowResizeEventSnapshot,
        workspace: u64,
        next: Rect,
    ) -> bool {
        let ws = self.workspace_by_id(workspace).expect("live");
        if ws.is_tiled {
            return false;
        }
        let Some(zone) = get(window, &WINDOW_STATE_SNAP_ZONE).filter(|zone| zone.is_layout()) else {
            return false;
        };
        let monitor = get(window, &WINDOW_STATE_SNAP_MONITOR)
            .filter(|monitor| !monitor.is_empty())
            .unwrap_or_else(|| ws.monitor.clone());
        let Some(base) = self.monitor_snap_base_rect(&monitor) else {
            return false;
        };
        let snapped: Vec<Window> = ws
            .list_windows()
            .into_iter()
            .filter(|candidate| is_window_in_floating_snap_layout(*candidate, &monitor))
            .collect();
        if !snapped.contains(&window) {
            return false;
        }

        let mut layout = floating_snap_layout_from_windows(base, &snapped);
        let mut changed = false;
        if event.edges.right && zone.is_left() {
            layout.split_x = next.right();
            changed = true;
        } else if event.edges.left && zone.is_right() {
            layout.split_x = next.x - SNAP_GAP_PX;
            changed = true;
        }
        let left_column = zone.is_left();
        if event.edges.bottom && zone.is_top() {
            if left_column {
                layout.left_split_y = next.bottom();
            } else {
                layout.right_split_y = next.bottom();
            }
            changed = true;
        } else if event.edges.top && zone.is_bottom() {
            if left_column {
                layout.left_split_y = next.y - SNAP_GAP_PX;
            } else {
                layout.right_split_y = next.y - SNAP_GAP_PX;
            }
            changed = true;
        }
        if !changed {
            return false;
        }
        self.clamp_floating_snap_layout(base, &mut layout, &snapped);
        for snapped_window in snapped {
            if let Some(zone) = get(snapped_window, &WINDOW_STATE_SNAP_ZONE).filter(|zone| zone.is_layout()) {
                stop_rect_animation(snapped_window, &WINDOW_STATE_RECT);
                set(snapped_window, &WINDOW_STATE_RECT, floating_snap_rect_for_zone(base, &layout, zone));
            }
        }
        true
    }

    fn clamp_floating_snap_layout(&self, base: Rect, layout: &mut FloatingSnapLayout, windows: &[Window]) {
        let (mut left_width, mut right_width) = (1.0_f64, 1.0_f64);
        let (mut left_top, mut left_bottom, mut right_top, mut right_bottom) = (1.0_f64, 1.0_f64, 1.0_f64, 1.0_f64);
        for window in windows {
            let Some(zone) = get(*window, &WINDOW_STATE_SNAP_ZONE).filter(|zone| zone.is_layout()) else {
                continue;
            };
            let constraints = window.size_constraints().get_untracked();
            let (extra_width, extra_height) = self.client_to_root_size_extra(*window);
            let min_width = constraints.min.map_or(1.0, |min| (min.width as f64).max(1.0)) + extra_width;
            let min_height = constraints.min.map_or(1.0, |min| (min.height as f64).max(1.0)) + extra_height;
            if zone.is_left() {
                left_width = left_width.max(min_width);
            } else {
                right_width = right_width.max(min_width);
            }
            match (zone.is_top(), zone.is_bottom(), zone.is_left()) {
                (true, _, true) => left_top = left_top.max(min_height),
                (true, _, false) => right_top = right_top.max(min_height),
                (_, true, true) => left_bottom = left_bottom.max(min_height),
                (_, true, false) => right_bottom = right_bottom.max(min_height),
                _ => {}
            }
        }
        layout.split_x = clamp(
            layout.split_x,
            base.x + left_width,
            base.x + base.width - SNAP_GAP_PX - right_width,
        );
        layout.left_split_y = clamp(
            layout.left_split_y,
            base.y + left_top,
            base.y + base.height - SNAP_GAP_PX - left_bottom,
        );
        layout.right_split_y = clamp(
            layout.right_split_y,
            base.y + right_top,
            base.y + base.height - SNAP_GAP_PX - right_bottom,
        );
    }

    fn set_window_snap_state(&self, workspace: Option<u64>, window: Window, monitor: &str, zone: SnapZone) {
        if let Some(ws) = workspace.and_then(|id| self.workspace_by_id(id)) {
            for other in ws.list_windows() {
                if other != window
                    && get(other, &WINDOW_STATE_SNAP_MONITOR).as_deref() == Some(monitor)
                    && SnapZone::conflicts_with(get(other, &WINDOW_STATE_SNAP_ZONE), zone)
                {
                    clear_window_snap_state(other);
                }
            }
        }
        set(window, &WINDOW_STATE_SNAP_ZONE, Some(zone));
        set(window, &WINDOW_STATE_SNAP_MONITOR, Some(monitor.to_owned()));
    }

    /// Queue a preview (monitor-local) or a hide for the bar.
    fn emit_snap_preview(&mut self, monitor: &str, rect: Option<Rect>, kind: &'static str) {
        let rect = rect.map(|rect| {
            let origin = COMPOSITOR
                .output
                .get(monitor)
                .map_or((0.0, 0.0), |output| (output.position.x as f64, output.position.y as f64));
            Rect::new(rect.x - origin.0, rect.y - origin.1, rect.width, rect.height)
        });
        self.outbox.snap_previews.push(SnapPreview {
            monitor: monitor.to_owned(),
            rect,
            kind,
        });
    }

    fn update_floating_drag_snap(&mut self, window: Window, event: &WindowMoveEventSnapshot) {
        if event.modifiers.shift || event.phase == WindowMovePhaseSnapshot::Start {
            self.clear_floating_snap_preview();
            return;
        }
        if matches!(event.phase, WindowMovePhaseSnapshot::End | WindowMovePhaseSnapshot::Cancel) {
            return;
        }
        let monitor = match event.output_name.as_ref().filter(|name| output_list().contains(name)) {
            Some(name) => name.clone(),
            None => self.current_monitor.clone(),
        };
        let zone = (!monitor.is_empty())
            .then(|| self.floating_snap_zone_at(&monitor, event.current_pointer.x, event.current_pointer.y))
            .flatten();
        let Some((zone, rect)) = zone.and_then(|zone| self.snap_zone_rect(&monitor, zone).map(|rect| (zone, rect)))
        else {
            self.clear_floating_snap_preview();
            return;
        };
        if let Some(previous) = &self.floating_snap
            && (previous.window != window || previous.monitor != monitor)
        {
            let previous_monitor = previous.monitor.clone();
            self.emit_snap_preview(&previous_monitor, None, "floating");
        }
        self.floating_snap = Some(FloatingSnap {
            window,
            monitor: monitor.clone(),
            zone,
            rect,
        });
        self.emit_snap_preview(&monitor, Some(rect), "floating");
    }

    fn clear_floating_snap_preview(&mut self) {
        if let Some(snap) = self.floating_snap.take() {
            self.emit_snap_preview(&snap.monitor, None, "floating");
        }
    }

    /// Apply the pending snap on drop; returns whether the window snapped.
    fn finish_floating_drag_snap(
        &mut self,
        window: Window,
        event: &WindowMoveEventSnapshot,
        workspace: Option<u64>,
    ) -> bool {
        if event.modifiers.shift {
            self.clear_floating_snap_preview();
            return false;
        }
        let Some(snap) = self.floating_snap.take() else {
            return false;
        };
        self.emit_snap_preview(&snap.monitor, None, "floating");
        if snap.window != window || event.phase != WindowMovePhaseSnapshot::End {
            return false;
        }

        let maximized = get(window, &WINDOW_STATE_MAXIMIZED);
        if snap.zone == SnapZone::Maximize {
            clear_window_snap_state(window);
            // Route through the real maximize so the client's maximized state
            // (and the SSD maximize icon) stays in sync.
            if !maximized {
                window.maximize();
            } else {
                let rect = self.maximized_rect_for_window(window, None);
                play_rect_animation(
                    window,
                    &WINDOW_STATE_RECT,
                    rect,
                    WINDOW_MANAGEMENT_EASING,
                    WINDOW_MANAGEMENT_ANIMATION_DURATION,
                );
                if let Some(id) = workspace {
                    self.ws(id).sync_floating_window_rect(window, rect);
                }
            }
        } else {
            // Unmaximize first (clearing the restore rect so it does not
            // animate back and fight the snap), then place the window.
            if maximized {
                set(window, &WINDOW_STATE_RESTORE_RECT, None);
                set(window, &WINDOW_STATE_MAXIMIZED, false);
                window.unmaximize();
            }
            play_rect_animation(
                window,
                &WINDOW_STATE_RECT,
                snap.rect,
                WINDOW_MANAGEMENT_EASING,
                WINDOW_MANAGEMENT_ANIMATION_DURATION,
            );
            self.set_window_snap_state(workspace, window, &snap.monitor, snap.zone);
            if let Some(id) = workspace {
                self.ws(id).sync_floating_window_rect(window, snap.rect);
            }
        }
        self.apply_workspace_stack_policy(workspace);
        true
    }
}

fn is_window_in_floating_snap_layout(window: Window, monitor: &str) -> bool {
    get(window, &WINDOW_STATE_SNAP_ZONE).is_some_and(SnapZone::is_layout)
        && get(window, &WINDOW_STATE_SNAP_MONITOR).as_deref() == Some(monitor)
        && !get(window, &WINDOW_STATE_MINIMIZED)
        && !get(window, &WINDOW_STATE_MAXIMIZED)
}

fn floating_snap_layout_from_windows(base: Rect, windows: &[Window]) -> FloatingSnapLayout {
    let average = |values: &[f64], fallback: f64| {
        if values.is_empty() {
            fallback
        } else {
            values.iter().sum::<f64>() / values.len() as f64
        }
    };
    let default_split_x = base.x + (base.width - SNAP_GAP_PX) / 2.0;
    let default_split_y = base.y + (base.height - SNAP_GAP_PX) / 2.0;
    let (mut split_x, mut left_y, mut right_y) = (Vec::new(), Vec::new(), Vec::new());
    for window in windows {
        let Some(zone) = get(*window, &WINDOW_STATE_SNAP_ZONE).filter(|zone| zone.is_layout()) else {
            continue;
        };
        let rect = get(*window, &WINDOW_STATE_RECT);
        if zone.is_left() {
            split_x.push(rect.right());
        } else {
            split_x.push(rect.x - SNAP_GAP_PX);
        }
        let column = if zone.is_left() { &mut left_y } else { &mut right_y };
        if zone.is_top() {
            column.push(rect.bottom());
        } else if zone.is_bottom() {
            column.push(rect.y - SNAP_GAP_PX);
        }
    }
    FloatingSnapLayout {
        split_x: average(&split_x, default_split_x),
        left_split_y: average(&left_y, default_split_y),
        right_split_y: average(&right_y, default_split_y),
    }
}

fn floating_snap_rect_for_zone(base: Rect, layout: &FloatingSnapLayout, zone: SnapZone) -> Rect {
    let right_x = layout.split_x + SNAP_GAP_PX;
    let left_width = (layout.split_x - base.x).max(1.0);
    let right_width = (base.right() - right_x).max(1.0);
    match zone {
        SnapZone::Maximize => base,
        SnapZone::Left => Rect::new(base.x, base.y, left_width, base.height),
        SnapZone::Right => Rect::new(right_x, base.y, right_width, base.height),
        SnapZone::TopLeft => Rect::new(base.x, base.y, left_width, (layout.left_split_y - base.y).max(1.0)),
        SnapZone::BottomLeft => {
            let y = layout.left_split_y + SNAP_GAP_PX;
            Rect::new(base.x, y, left_width, (base.bottom() - y).max(1.0))
        }
        SnapZone::TopRight => Rect::new(right_x, base.y, right_width, (layout.right_split_y - base.y).max(1.0)),
        SnapZone::BottomRight => {
            let y = layout.right_split_y + SNAP_GAP_PX;
            Rect::new(right_x, y, right_width, (base.bottom() - y).max(1.0))
        }
    }
}

pub fn clear_window_snap_state(window: Window) {
    set(window, &WINDOW_STATE_SNAP_ZONE, None);
    set(window, &WINDOW_STATE_SNAP_MONITOR, None);
}

fn restore_rect_for_maximized_move(event: &WindowMoveEventSnapshot, width: f64, height: f64) -> Rect {
    let pointer = event.current_pointer;
    let titlebar_center_y = WINDOW_BORDER_PX + TITLEBAR_HEIGHT / 2.0;
    let pointer_offset_y = if event.source == WindowMoveSourceSnapshot::Modifier {
        height / 2.0
    } else {
        (height / 2.0).min(titlebar_center_y)
    };
    Rect::new(pointer.x - width / 2.0, pointer.y - pointer_offset_y, width, height)
}

fn tile_drag_workspace_edge_direction(monitor: &str, y: f64) -> i32 {
    let rect = workspace_viewport_rect(monitor);
    if y < rect.y + TILE_DRAG_WORKSPACE_EDGE_PX {
        -1
    } else if y > rect.y + rect.height - TILE_DRAG_WORKSPACE_EDGE_PX {
        1
    } else {
        0
    }
}

fn workspace_viewport_rect(monitor: &str) -> Rect {
    COMPOSITOR
        .layer
        .usable_area(monitor)
        .or_else(|| output_rect(monitor))
        .unwrap_or(Rect::new(0.0, 0.0, 1280.0, 720.0))
}

// Rect deltas use `add` so open/close/workspace motion layers on top of the
// override-mode layout animation. Open/close opacity multiplies; workspace
// opacity is its own override channel so an inactive workspace whose base
// opacity is already 0 can still fade back in.

fn delta(y: f64) -> Rect {
    Rect::new(0.0, y, 0.0, 0.0)
}

fn schedule_open_animation(window: Window) {
    let duration = OPEN_CLOSE_ANIMATION_DURATION as u64;
    window.schedule_animation(
        ManagedAnimation::new(OPEN_ANIMATION_CHANNEL)
            .rect(Some(delta(200.0)), delta(0.0), duration, WINDOW_OPEN_EASING, AnimationMode::Add)
            .opacity(Some(0.0), 1.0, duration, WINDOW_OPEN_EASING, AnimationMode::Multiply),
    );
}

fn schedule_close_animation(window: Window) {
    let duration = OPEN_CLOSE_ANIMATION_DURATION as u64;
    window.schedule_animation(
        ManagedAnimation::new(CLOSE_ANIMATION_CHANNEL)
            .rect(Some(delta(0.0)), delta(120.0), duration, WINDOW_CLOSE_EASING, AnimationMode::Add)
            .opacity(Some(1.0), 0.0, duration, WINDOW_CLOSE_EASING, AnimationMode::Multiply),
    );
}

fn schedule_minimize_animation(window: Window, minimized: bool, instant: bool) {
    if instant {
        // Replace whatever the channel holds with its end pose.
        let rest = delta(if minimized { 120.0 } else { 0.0 });
        let opacity = if minimized { 0.0 } else { 1.0 };
        window.schedule_animation(
            ManagedAnimation::new(MINIMIZE_ANIMATION_CHANNEL)
                .rect(Some(rest), rest, 1, Easing::Linear, AnimationMode::Add)
                .opacity(Some(opacity), opacity, 1, Easing::Linear, AnimationMode::Override),
        );
        return;
    }
    let duration = OPEN_CLOSE_ANIMATION_DURATION as u64;
    let (rect_from, rect_to, rect_easing) = if minimized {
        (delta(0.0), delta(120.0), WINDOW_MINIMIZE_RECT_EASING)
    } else {
        (delta(200.0), delta(0.0), WINDOW_UNMINIMIZE_RECT_EASING)
    };
    let (opacity_from, opacity_to, opacity_easing) = if minimized {
        (1.0, 0.0, WINDOW_MINIMIZE_OPACITY_EASING)
    } else {
        (0.0, 1.0, WINDOW_UNMINIMIZE_OPACITY_EASING)
    };
    window.schedule_animation(
        ManagedAnimation::new(MINIMIZE_ANIMATION_CHANNEL)
            .rect(Some(rect_from), rect_to, duration, rect_easing, AnimationMode::Add)
            .opacity(Some(opacity_from), opacity_to, duration, opacity_easing, AnimationMode::Override),
    );
}

pub fn schedule_workspace_visual_animation(
    window: Window,
    from_offset_y: f64,
    to_offset_y: f64,
    from_opacity: f64,
    to_opacity: f64,
    easing: Easing,
    duration: f64,
) {
    cancel_workspace_visual_animation(window);
    set(window, &WINDOW_STATE_WORKSPACE_OFFSET_Y, 0.0);
    set(window, &WINDOW_STATE_WORKSPACE_OPACITY, to_opacity);
    let duration = duration as u64;
    window.schedule_animation(ManagedAnimation::new(WORKSPACE_VISUAL_RECT_ANIMATION_CHANNEL).rect(
        Some(delta(from_offset_y)),
        delta(to_offset_y),
        duration,
        easing,
        AnimationMode::Add,
    ));
    window.schedule_animation(ManagedAnimation::new(WORKSPACE_VISUAL_OPACITY_ANIMATION_CHANNEL).opacity(
        Some(from_opacity),
        to_opacity,
        duration,
        easing,
        AnimationMode::Override,
    ));
}

pub fn reset_workspace_visual_state(window: Window, visible: bool) {
    cancel_workspace_visual_animation(window);
    set(window, &WINDOW_STATE_WORKSPACE_VISIBLE, visible);
    set(window, &WINDOW_STATE_WORKSPACE_OFFSET_Y, 0.0);
    set(window, &WINDOW_STATE_WORKSPACE_OPACITY, if visible { 1.0 } else { 0.0 });
}

pub fn cancel_workspace_visual_animation(window: Window) {
    window.cancel_animation(WORKSPACE_VISUAL_ANIMATION_CHANNEL);
    window.cancel_animation(WORKSPACE_VISUAL_RECT_ANIMATION_CHANNEL);
    window.cancel_animation(WORKSPACE_VISUAL_OPACITY_ANIMATION_CHANNEL);
}
