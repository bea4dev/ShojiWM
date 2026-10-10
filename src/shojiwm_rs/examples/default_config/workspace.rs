//! Port of the `Workspace` class of `packages/config/src/window-manager.ts`:
//! one workspace of one monitor, laid out either floating or as a
//! horizontally scrolling row of tiles.

use std::collections::{HashMap, HashSet};

use shojiwm_rs::prelude::*;

use crate::{
    window_animation::{play_rect_animation, stop_rect_animation},
    window_manager::*,
};

#[derive(Debug, Clone, Default)]
pub struct LayoutOptions {
    pub animate: Option<bool>,
    pub preserve_missing_active: bool,
    pub cancel_rect_animations: Option<bool>,
    pub immediate_floating_windows: Option<HashSet<Window>>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct AddWindowOptions {
    pub restore_scroll_if_initially_floating: bool,
}

/// What a window carries along when it moves between workspaces.
#[derive(Debug, Clone)]
pub struct WorkspaceWindowSnapshot {
    pub tile_width: Option<f64>,
    pub floating_rect: Option<Rect>,
    pub restore_rect: Option<Rect>,
    pub snap_zone: Option<SnapZone>,
    pub snap_monitor: Option<String>,
    pub minimized: bool,
    pub maximized: bool,
}

struct InitialTileState {
    scroll_offset: f64,
    active_window: Option<Window>,
    token: u64,
}

/// A running kinetic (inertial) scroll after a three-finger swipe.
struct KineticScroll {
    velocity_x: f64,
    last_time: Option<f64>,
    first_step: bool,
    snap_max_velocity: f64,
    timer: Option<TimerHandle>,
}

pub struct WorkspaceTransition {
    pub from_offset_y: f64,
    pub to_offset_y: f64,
    pub from_opacity: f64,
    pub to_opacity: f64,
    pub visible_after: bool,
}

pub struct Workspace {
    pub id: u64,
    pub index: u32,
    pub monitor: String,
    pub is_tiled: bool,
    windows: Vec<Window>,
    natural_root_rect: NaturalRootRect,
    active: ActiveWorkspaces,
    handle: WeakWindowManager,
    tile_width_by_window: HashMap<Window, f64>,
    restored_window_state: HashMap<Window, WorkspaceWindowSnapshot>,
    active_window: Option<Window>,
    visibility_animation_token: u64,
    dragging_window: Option<Window>,
    /// Layout slot reserved for the tile being dragged, captured during
    /// `apply_layout` so the bar can preview where the tile will land.
    last_dragging_slot_rect: Option<Rect>,
    last_applied_tile_viewport_rect: Option<Rect>,
    scroll_offset: f64,
    initial_tile_state: HashMap<Window, InitialTileState>,
    initial_tile_state_token: u64,
    tile_reorder_token_by_window: HashMap<Window, u64>,
    tile_reorder_token: u64,
    kinetic: Option<KineticScroll>,
    kinetic_token: u64,
}

impl Workspace {
    pub fn new(
        id: u64,
        index: u32,
        monitor: &str,
        natural_root_rect: NaturalRootRect,
        active: ActiveWorkspaces,
        handle: WeakWindowManager,
    ) -> Self {
        Self {
            id,
            index,
            monitor: monitor.to_owned(),
            is_tiled: false,
            windows: Vec::new(),
            natural_root_rect,
            active,
            handle,
            tile_width_by_window: HashMap::new(),
            restored_window_state: HashMap::new(),
            active_window: None,
            visibility_animation_token: 0,
            dragging_window: None,
            last_dragging_slot_rect: None,
            last_applied_tile_viewport_rect: None,
            scroll_offset: 0.0,
            initial_tile_state: HashMap::new(),
            initial_tile_state_token: 0,
            tile_reorder_token_by_window: HashMap::new(),
            tile_reorder_token: 0,
            kinetic: None,
            kinetic_token: 0,
        }
    }

    fn maximized_root_rect(&self, window: Window) -> Rect {
        maximized_rect_on(window, Some(&self.monitor))
    }

    pub fn move_to_monitor(&mut self, monitor: &str, index: u32) {
        self.monitor = monitor.to_owned();
        self.index = index;
        for window in self.windows.clone() {
            self.sync_window_visible_outputs(window);
            if get(window, &WINDOW_STATE_FULLSCREEN) {
                set(window, &WINDOW_STATE_RECT, self.fullscreen_root_rect(window));
                continue;
            }
            if get(window, &WINDOW_STATE_MAXIMIZED) {
                set(window, &WINDOW_STATE_RECT, self.maximized_root_rect(window));
                continue;
            }
            if !self.is_tiled || !self.should_tile(window) {
                let rect = self.clamp_rect_to_viewport(get(window, &WINDOW_STATE_RECT));
                set(window, &WINDOW_STATE_RECT, rect);
                set(
                    window,
                    &WINDOW_STATE_FLOATING_RECT,
                    self.is_tiled
                        .then(|| self.viewport_rect_to_floating_content_rect(rect)),
                );
            }
        }
    }

    /// Returns whether the window was restored from a move snapshot.
    pub fn add_window(&mut self, window: Window, options: AddWindowOptions) -> bool {
        if self.windows.contains(&window) {
            return false;
        }
        let previous_active_window = self.active_window;
        let previous_scroll_offset = self.scroll_offset;
        let restored = self.restored_window_state.remove(&window);
        let tile_insertion_index = (restored.is_none() && self.is_tiled && self.should_tile(window))
            .then(|| self.tile_insertion_index_after_focused_window());
        self.windows.push(window);
        if let Some(index) = tile_insertion_index {
            self.move_tile_window_to_index(window, index);
        }
        let tileable_in_current_mode = !self.is_tiled || self.should_tile(window);
        if restored.is_none() && tileable_in_current_mode {
            self.active_window = Some(window);
        }
        if let Some(restored) = &restored {
            window.cancel_all_animations();
            set(window, &WINDOW_STATE_FLOATING_RECT, restored.floating_rect);
            set(window, &WINDOW_STATE_RESTORE_RECT, restored.restore_rect);
            set(window, &WINDOW_STATE_SNAP_ZONE, restored.snap_zone);
            set(window, &WINDOW_STATE_SNAP_MONITOR, restored.snap_monitor.clone());
            set(window, &WINDOW_STATE_MINIMIZED, restored.minimized);
            set(window, &WINDOW_STATE_MINIMIZE_VISUAL_IDLE, restored.minimized);
            set(window, &WINDOW_STATE_MAXIMIZED, restored.maximized);
            if let Some(width) = restored.tile_width {
                self.tile_width_by_window.insert(window, width);
            }
        }
        let visible = self.is_active();
        set(window, &WINDOW_STATE_WORKSPACE_VISIBLE, visible);
        set(window, &WINDOW_STATE_WORKSPACE_OFFSET_Y, 0.0);
        set(window, &WINDOW_STATE_WORKSPACE_OPACITY, if visible { 1.0 } else { 0.0 });
        self.sync_window_visible_outputs(window);

        if !output_list().contains(&self.monitor) {
            return restored.is_some();
        }

        let restored_floating = restored.as_ref().and_then(|restored| restored.floating_rect);
        if let (Some(floating), false) = (restored_floating, self.is_tiled) {
            set(window, &WINDOW_STATE_RECT, floating);
        } else if self.is_tiled && self.should_tile(window) {
            let initial_rect = self.centered_floating_rect(window);
            set(
                window,
                &WINDOW_STATE_FLOATING_RECT,
                Some(restored_floating.unwrap_or(initial_rect)),
            );
            if restored.as_ref().and_then(|restored| restored.tile_width).is_none() {
                self.set_tile_width_from_rect(window, initial_rect, true);
                if options.restore_scroll_if_initially_floating {
                    self.remember_initial_tile_state(
                        window,
                        previous_scroll_offset,
                        previous_active_window,
                    );
                }
                self.scroll_to_window(window, false);
            }
            self.apply_layout(LayoutOptions {
                animate: Some(restored.is_none()),
                preserve_missing_active: restored.is_some(),
                ..LayoutOptions::default()
            });
        } else if self.is_tiled {
            let initial_rect = self.centered_floating_rect(window);
            let content_rect = restored_floating
                .unwrap_or_else(|| self.viewport_rect_to_floating_content_rect(initial_rect));
            set(window, &WINDOW_STATE_FLOATING_RECT, Some(content_rect));
            set(
                window,
                &WINDOW_STATE_RECT,
                self.floating_content_rect_to_viewport_rect(content_rect),
            );
        } else {
            set(window, &WINDOW_STATE_RECT, self.centered_floating_rect(window));
        }
        restored.is_some()
    }

    fn remember_initial_tile_state(
        &mut self,
        window: Window,
        scroll_offset: f64,
        active_window: Option<Window>,
    ) {
        self.initial_tile_state_token += 1;
        let token = self.initial_tile_state_token;
        self.initial_tile_state.insert(
            window,
            InitialTileState {
                scroll_offset,
                active_window,
                token,
            },
        );
        let handle = self.handle.clone();
        let id = self.id;
        set_timeout(INITIAL_TILEABILITY_SETTLE_DURATION, move || {
            handle.with_workspace(id, |workspace| {
                if workspace
                    .initial_tile_state
                    .get(&window)
                    .is_some_and(|state| state.token == token)
                {
                    workspace.initial_tile_state.remove(&window);
                }
            });
        });
    }

    /// `Some(next)` when the window was here: `next` inherits focus (`None`
    /// leaves the choice to the compositor). `None` when it was not here.
    pub fn remove_window(&mut self, window: Window) -> Option<Option<Window>> {
        let index = self.windows.iter().position(|current| *current == window)?;
        // Both describe the tile sequence this window is still part of, so
        // they have to be read before it is taken out.
        let was_tile = self.is_tileable(window);
        let tile_index = self.tile_index_of(window);
        self.windows.remove(index);
        self.tile_width_by_window.remove(&window);
        self.initial_tile_state.remove(&window);
        self.tile_reorder_token_by_window.remove(&window);
        set(window, &WINDOW_STATE_TILE_REORDERING, false);
        if self.dragging_window == Some(window) {
            self.dragging_window = None;
            set(window, &WINDOW_STATE_TILE_DRAGGING, false);
        }
        let mut next_focus = None;
        if self.active_window == Some(window) {
            next_focus = self.successor_for_removed_window(was_tile, tile_index);
            self.active_window = next_focus;
        }
        Some(next_focus)
    }

    /// Only a tile of a tiled workspace has a "next one along"; anywhere
    /// else the compositor's focus history knows better (e.g. returning to
    /// the terminal that asked for a dismissed dialog).
    fn successor_for_removed_window(&self, was_tile: bool, tile_index: Option<usize>) -> Option<Window> {
        let tile_index = tile_index?;
        if !self.is_tiled || !was_tile {
            return None;
        }
        let tileable = self.tileable_windows();
        if tileable.is_empty() {
            return None;
        }
        tileable.get(tile_index.min(tileable.len() - 1)).copied()
    }

    /// The window that inherits focus when the focused `window` is
    /// minimized, called once it already counts as minimized.
    ///
    /// A tile hands focus to the tile that slides into its slot, as a removed
    /// tile does. Anything else hands it to the topmost window still on
    /// screen: floating windows stack by focus order, so that is also the one
    /// used most recently. In a tiled workspace only floating windows compete
    /// on z-order — tiles scrolled out of view sit in the same stack — and the
    /// active tile is the fallback.
    pub fn successor_for_minimized_window(
        &self,
        window: Window,
        z_index_of: impl Fn(Window) -> i32,
    ) -> Option<Window> {
        if self.is_tiled
            && self.should_tile(window)
            && let Some(tile) = self.successor_for_removed_window(true, self.tile_index_of(window))
        {
            return Some(tile);
        }
        let topmost = self
            .windows
            .iter()
            .copied()
            .filter(|current| {
                *current != window
                    && !get(*current, &WINDOW_STATE_MINIMIZED)
                    && (!self.is_tiled || !self.should_tile(*current))
            })
            .max_by_key(|current| z_index_of(*current));
        if topmost.is_some() || !self.is_tiled {
            return topmost;
        }
        let tileable = self.tileable_windows();
        self.active_window_in(&tileable).or_else(|| tileable.last().copied())
    }

    pub fn remove_tile_drag_window(&mut self, window: Window) {
        if let Some(index) = self.windows.iter().position(|current| *current == window) {
            self.windows.remove(index);
            self.dragging_window = None;
        }
    }

    pub fn remove_floating_window(&mut self, window: Window) {
        if let Some(index) = self.windows.iter().position(|current| *current == window) {
            self.windows.remove(index);
            if self.active_window == Some(window) {
                self.active_window = self.active_window_in(&self.tileable_windows());
            }
        }
    }

    pub fn has_window(&self, window: Window) -> bool {
        self.windows.contains(&window)
    }

    pub fn window_count(&self) -> usize {
        self.windows.len()
    }

    pub fn list_windows(&self) -> Vec<Window> {
        self.windows.clone()
    }

    pub fn find_window_by_id(&self, id: &str) -> Option<Window> {
        self.windows.iter().copied().find(|window| window.id() == id)
    }

    pub fn is_active_window(&self, window: Window) -> bool {
        self.active_window == Some(window)
    }

    pub fn move_focused_tile(&mut self, direction: i32) -> bool {
        if !self.is_tiled {
            return false;
        }
        let Some(focused) = self.focused_window().filter(|window| self.should_tile(*window)) else {
            return false;
        };
        let tileable = self.tileable_windows();
        let Some(current) = tileable.iter().position(|window| *window == focused) else {
            return false;
        };
        let next = current as i64 + direction as i64;
        if next < 0 || next >= tileable.len() as i64 {
            return false;
        }
        self.stop_kinetic_scroll();
        self.active_window = Some(focused);
        self.mark_tile_reordering(focused);
        self.move_tile_window_to_index(focused, next as usize);
        self.scroll_to_window(focused, false);
        self.apply_layout(LayoutOptions::default());
        focused.focus();
        true
    }

    fn mark_tile_reordering(&mut self, window: Window) {
        self.tile_reorder_token += 1;
        let token = self.tile_reorder_token;
        self.tile_reorder_token_by_window.insert(window, token);
        set(window, &WINDOW_STATE_TILE_REORDERING, true);
        let handle = self.handle.clone();
        let id = self.id;
        set_timeout(TILE_ANIMATION_DURATION, move || {
            handle.with_workspace(id, |workspace| {
                if workspace.tile_reorder_token_by_window.get(&window) != Some(&token) {
                    return;
                }
                workspace.tile_reorder_token_by_window.remove(&window);
                set(window, &WINDOW_STATE_TILE_REORDERING, false);
            });
        });
    }

    pub fn take_window_for_move(&mut self, window: Window) -> Option<WorkspaceWindowSnapshot> {
        if !self.has_window(window) {
            return None;
        }
        let snapshot = self.snapshot_window(window);
        self.remove_window(window);
        Some(snapshot)
    }

    pub fn add_moved_window(&mut self, window: Window, snapshot: WorkspaceWindowSnapshot) -> bool {
        self.restored_window_state.insert(window, snapshot);
        self.add_window(window, AddWindowOptions::default())
    }

    pub fn is_restoring_window(&self, window: Window) -> bool {
        self.restored_window_state.contains_key(&window)
    }

    pub fn is_active(&self) -> bool {
        self.active.borrow().get(&self.monitor).copied().unwrap_or(1) == self.index
    }

    pub fn refresh_usable_area_layout(&mut self) {
        if !output_list().contains(&self.monitor) {
            return;
        }
        if self.is_tiled {
            let next = self.tile_viewport_rect();
            if self.last_applied_tile_viewport_rect == Some(next) {
                return;
            }
            self.apply_layout(LayoutOptions {
                animate: Some(false),
                preserve_missing_active: true,
                ..LayoutOptions::default()
            });
            return;
        }
        for window in self.windows.clone() {
            if !get(window, &WINDOW_STATE_MAXIMIZED) {
                continue;
            }
            stop_rect_animation(window, &WINDOW_STATE_RECT);
            set(window, &WINDOW_STATE_RECT, self.maximized_root_rect(window));
        }
    }

    fn set_window_visual(&self, window: Window, visible: bool, offset_y: f64, opacity: f64) {
        if get(window, &WINDOW_STATE_TILE_DRAGGING) {
            set(window, &WINDOW_STATE_WORKSPACE_VISIBLE, true);
            set(window, &WINDOW_STATE_WORKSPACE_OFFSET_Y, 0.0);
            set(window, &WINDOW_STATE_WORKSPACE_OPACITY, 1.0);
            return;
        }
        set(window, &WINDOW_STATE_WORKSPACE_VISIBLE, visible);
        set(window, &WINDOW_STATE_WORKSPACE_OFFSET_Y, offset_y);
        set(window, &WINDOW_STATE_WORKSPACE_OPACITY, opacity);
    }

    pub fn set_visible(&mut self, visible: bool) {
        self.visibility_animation_token += 1;
        for window in self.windows.clone() {
            self.sync_window_visible_outputs(window);
            self.set_window_visual(window, visible, 0.0, if visible { 1.0 } else { 0.0 });
        }
    }

    /// `set_visible` that also stops a running workspace slide/fade.
    pub fn show_instantly(&mut self, visible: bool) {
        for window in self.windows.clone() {
            cancel_workspace_visual_animation(window);
        }
        self.set_visible(visible);
    }

    pub fn prepare_workspace_transition(&mut self, offset_y: f64, opacity: f64) {
        self.visibility_animation_token += 1;
        for window in self.windows.clone() {
            self.sync_window_visible_outputs(window);
            self.set_window_visual(window, true, offset_y, opacity);
        }
    }

    pub fn set_workspace_gesture_visual(&mut self, offset_y: f64, opacity: f64) {
        self.visibility_animation_token += 1;
        for window in self.windows.clone() {
            self.sync_window_visible_outputs(window);
            cancel_workspace_visual_animation(window);
            self.set_window_visual(window, true, offset_y, opacity);
        }
    }

    pub fn animate_workspace_transition(&mut self, transition: WorkspaceTransition) {
        self.visibility_animation_token += 1;
        let token = self.visibility_animation_token;
        for window in self.windows.clone() {
            self.sync_window_visible_outputs(window);
            if get(window, &WINDOW_STATE_TILE_DRAGGING) {
                self.set_window_visual(window, true, 0.0, 1.0);
                continue;
            }
            // Minimized windows are hidden purely by `idle`; animating them
            // would bypass the idle render gate and fade them in with the
            // switch. Keep them on static state only.
            if get(window, &WINDOW_STATE_MINIMIZED) || get(window, &WINDOW_STATE_MINIMIZE_VISUAL_IDLE) {
                cancel_workspace_visual_animation(window);
                set(window, &WINDOW_STATE_WORKSPACE_VISIBLE, true);
                set(window, &WINDOW_STATE_WORKSPACE_OFFSET_Y, 0.0);
                set(window, &WINDOW_STATE_WORKSPACE_OPACITY, transition.to_opacity);
                continue;
            }
            // Schedule first, then flip VISIBLE, so a completed hold
            // animation never shows the window at its static position.
            schedule_workspace_visual_animation(
                window,
                transition.from_offset_y,
                transition.to_offset_y,
                transition.from_opacity,
                transition.to_opacity,
                WINDOW_MANAGEMENT_EASING,
                WORKSPACE_SWITCH_ANIMATION_DURATION,
            );
            set(window, &WINDOW_STATE_WORKSPACE_VISIBLE, true);
        }

        const VISIBILITY_COMMIT_BEFORE_END_MS: f64 = 32.0;
        let handle = self.handle.clone();
        let id = self.id;
        let visible_after = transition.visible_after;
        set_timeout(
            (WORKSPACE_SWITCH_ANIMATION_DURATION - VISIBILITY_COMMIT_BEFORE_END_MS).max(0.0),
            move || {
                handle.with_workspace(id, |workspace| {
                    if workspace.visibility_animation_token == token {
                        workspace.set_visible(visible_after);
                    }
                });
            },
        );
    }

    pub fn set_tiled(&mut self, tiled: bool) {
        if self.is_tiled == tiled {
            return;
        }
        self.stop_kinetic_scroll();

        let focused_tileable = self
            .focused_window()
            .filter(|window| self.should_tile(*window) && !get(*window, &WINDOW_STATE_MINIMIZED));
        self.is_tiled = tiled;
        if tiled {
            self.scroll_offset = 0.0;
            for window in self.windows.clone() {
                self.sync_window_visible_outputs(window);
            }
            for window in self.tileable_windows() {
                self.capture_floating_rect(window);
                let rect = get(window, &WINDOW_STATE_FLOATING_RECT)
                    .unwrap_or_else(|| get(window, &WINDOW_STATE_RECT));
                self.set_tile_width_from_rect(window, rect, true);
            }
            for window in self.floating_windows() {
                self.capture_floating_rect(window);
            }
            let tileable = self.tileable_windows();
            let previous_active = self.active_window_in(&tileable);
            self.active_window = focused_tileable
                .or(previous_active)
                .or_else(|| tileable.first().copied());
            if let Some(window) = focused_tileable {
                self.scroll_to_window(window, false);
            }
            self.apply_layout(LayoutOptions::default());
            if let Some(window) = focused_tileable {
                window.focus();
            }
            return;
        }

        for window in self.windows.clone() {
            // A window still maximized across the switch keeps its maximized
            // rect; restoring FLOATING_RECT here would configure the client
            // to the (often degenerate) rect captured at first commit.
            if get(window, &WINDOW_STATE_MAXIMIZED) {
                play_rect_animation(
                    window,
                    &WINDOW_STATE_RECT,
                    self.maximized_root_rect(window),
                    WINDOW_MANAGEMENT_EASING,
                    WINDOW_MANAGEMENT_ANIMATION_DURATION,
                );
                set(window, &WINDOW_STATE_FLOATING_RECT, None);
                self.sync_window_visible_outputs(window);
                continue;
            }
            if let Some(rect) = get(window, &WINDOW_STATE_FLOATING_RECT) {
                let viewport_rect = if self.should_tile(window) {
                    rect
                } else {
                    self.floating_content_rect_to_viewport_rect(rect)
                };
                play_rect_animation(
                    window,
                    &WINDOW_STATE_RECT,
                    viewport_rect,
                    WINDOW_MANAGEMENT_EASING,
                    WINDOW_MANAGEMENT_ANIMATION_DURATION,
                );
            }
            set(window, &WINDOW_STATE_FLOATING_RECT, None);
            self.sync_window_visible_outputs(window);
        }
        if let Some(window) = focused_tileable {
            self.active_window = Some(window);
            window.focus();
        }
    }

    pub fn apply_layout(&mut self, options: LayoutOptions) {
        if !self.is_tiled {
            return;
        }
        let tileable = self.tileable_windows();
        let animate = options.animate.unwrap_or(true);
        self.clamp_scroll_offset();

        if tileable.is_empty() {
            self.active_window = None;
            self.apply_floating_layout(animate, options.immediate_floating_windows.as_ref());
            return;
        }

        if !self.active_window.is_some_and(|active| tileable.contains(&active))
            && !options.preserve_missing_active
        {
            self.active_window = tileable.last().copied();
        }

        let viewport = self.tile_viewport_rect();
        self.last_applied_tile_viewport_rect = Some(viewport);
        let tile_height = viewport.height;
        let mut next_x = viewport.x - self.physical_aligned_scroll_offset();
        self.last_dragging_slot_rect = None;

        let count = tileable.len();
        for (index, window) in tileable.into_iter().enumerate() {
            let tile_width = self.tile_width_for_window(window, viewport);
            let rect = if get(window, &WINDOW_STATE_FULLSCREEN) {
                self.fullscreen_root_rect(window)
            } else if get(window, &WINDOW_STATE_MAXIMIZED) {
                self.maximized_tile_rect(window, next_x)
            } else {
                Rect::new(next_x, viewport.y, tile_width, tile_height)
            };
            if Some(window) == self.dragging_window {
                self.last_dragging_slot_rect = Some(rect);
            } else if animate {
                play_rect_animation(
                    window,
                    &WINDOW_STATE_RECT,
                    rect,
                    WINDOW_MANAGEMENT_EASING,
                    TILE_ANIMATION_DURATION,
                );
            } else {
                if options.cancel_rect_animations != Some(false) {
                    stop_rect_animation(window, &WINDOW_STATE_RECT);
                }
                set(window, &WINDOW_STATE_RECT, rect);
            }
            next_x += tile_width + if index == count - 1 { 0.0 } else { TILE_GAP };
        }
        self.apply_floating_layout(animate, options.immediate_floating_windows.as_ref());
    }

    pub fn resize_tile(&mut self, window: Window, event: &shojiwm_rs::ssd::WindowResizeEventSnapshot) {
        if !self.tileable_windows().contains(&window) {
            return;
        }
        use shojiwm_rs::ssd::WindowResizePhaseSnapshot as Phase;
        if matches!(event.phase, Phase::Start | Phase::Update) && get(window, &WINDOW_STATE_MAXIMIZED) {
            set(window, &WINDOW_STATE_MAXIMIZED, false);
            set(window, &WINDOW_STATE_RESTORE_RECT, None);
            window.unmaximize();
        }
        stop_rect_animation(window, &WINDOW_STATE_RECT);
        self.active_window = Some(window);

        let viewport = self.tile_viewport_rect();
        let min_width = self.min_tile_width(window, viewport);
        let max_width = self.max_tile_width(window);
        let width = clamp(event.current_rect.width, min_width, min_width.max(max_width));
        self.tile_width_by_window.insert(window, width);
        self.scroll_to_window(window, false);
        self.apply_layout(LayoutOptions::default());
    }

    pub fn dragging_slot_rect(&self) -> Option<Rect> {
        self.dragging_window.and(self.last_dragging_slot_rect)
    }

    pub fn begin_tile_drag(&mut self, window: Window, rect: Rect) {
        if !self.should_tile(window) {
            return;
        }
        self.active_window = Some(window);
        self.dragging_window = Some(window);
        let was_maximized = get(window, &WINDOW_STATE_MAXIMIZED);
        set(window, &WINDOW_STATE_MAXIMIZED, false);
        set(window, &WINDOW_STATE_RESTORE_RECT, None);
        if was_maximized {
            window.unmaximize();
        }
        set(window, &WINDOW_STATE_TILE_DRAGGING, true);
        self.sync_window_visible_outputs(window);
        set(window, &WINDOW_STATE_WORKSPACE_VISIBLE, true);
        set(window, &WINDOW_STATE_WORKSPACE_OFFSET_Y, 0.0);
        set(window, &WINDOW_STATE_WORKSPACE_OPACITY, 1.0);
        self.set_tile_width_from_rect(window, get(window, &WINDOW_STATE_RECT), false);
        stop_rect_animation(window, &WINDOW_STATE_RECT);
        set(window, &WINDOW_STATE_RECT, rect);
        self.apply_layout(LayoutOptions::default());
    }

    pub fn adopt_tile_drag_window(&mut self, window: Window, rect: Rect) {
        if !self.has_window(window) {
            self.windows.push(window);
        }
        let visible = self.is_active();
        self.active_window = Some(window);
        self.dragging_window = Some(window);
        self.set_tile_width_from_rect(window, rect, false);
        set(window, &WINDOW_STATE_TILE_DRAGGING, true);
        self.sync_window_visible_outputs(window);
        set(window, &WINDOW_STATE_WORKSPACE_VISIBLE, true);
        set(window, &WINDOW_STATE_WORKSPACE_OFFSET_Y, 0.0);
        set(window, &WINDOW_STATE_WORKSPACE_OPACITY, if visible { 1.0 } else { 0.0 });
        stop_rect_animation(window, &WINDOW_STATE_RECT);
        set(window, &WINDOW_STATE_RECT, rect);
    }

    pub fn adopt_floating_window(&mut self, window: Window, rect: Rect) {
        if !self.has_window(window) {
            self.windows.push(window);
        }
        let visible = self.is_active();
        self.active_window = Some(window);
        self.sync_window_visible_outputs(window);
        reset_workspace_visual_state(window, visible);
        set(
            window,
            &WINDOW_STATE_FLOATING_RECT,
            Some(if self.is_tiled {
                self.viewport_rect_to_floating_content_rect(rect)
            } else {
                rect
            }),
        );
        stop_rect_animation(window, &WINDOW_STATE_RECT);
        set(window, &WINDOW_STATE_RECT, rect);
    }

    pub fn update_tile_drag(&mut self, window: Window, rect: Rect, pointer_x: f64) {
        if self.dragging_window != Some(window) {
            self.begin_tile_drag(window, rect);
        }
        self.active_window = Some(window);
        let index = self.tile_insertion_index_for_pointer(window, pointer_x);
        self.move_tile_window_to_index(window, index);
        stop_rect_animation(window, &WINDOW_STATE_RECT);
        set(window, &WINDOW_STATE_RECT, rect);
        self.scroll_to_window(window, false);
        self.apply_layout(LayoutOptions::default());
    }

    pub fn end_tile_drag(&mut self, window: Window, cancelled: bool) {
        if self.dragging_window != Some(window) {
            return;
        }
        self.dragging_window = None;
        set(window, &WINDOW_STATE_TILE_DRAGGING, false);
        self.sync_window_visible_outputs(window);
        set(window, &WINDOW_STATE_WORKSPACE_OFFSET_Y, 0.0);
        set(window, &WINDOW_STATE_WORKSPACE_OPACITY, if self.is_active() { 1.0 } else { 0.0 });
        if !cancelled {
            self.active_window = Some(window);
            self.scroll_to_window(window, false);
        }
        self.apply_layout(LayoutOptions::default());
        if !cancelled && self.is_active() {
            window.focus();
        }
    }

    pub fn focus_window(&mut self, window: Window) {
        if !self.should_tile(window) || self.active_window == Some(window) {
            return;
        }
        self.active_window = Some(window);
        self.scroll_to_window(window, false);
        self.apply_layout(LayoutOptions::default());
    }

    /// "Go to this window": center it even when already visible.
    pub fn pan_to_window(&mut self, window: Window) {
        self.pan_to_window_with(window, true);
    }

    pub fn pan_to_window_with(&mut self, window: Window, animate: bool) {
        if !self.is_tiled || !self.should_tile(window) {
            return;
        }
        self.active_window = Some(window);
        self.scroll_to_window(window, true);
        self.apply_layout(LayoutOptions {
            animate: Some(animate),
            ..LayoutOptions::default()
        });
    }

    pub fn focus_window_under_pointer(&mut self, window: Window) -> Option<Window> {
        if !self.is_tiled || !self.has_window(window) || get(window, &WINDOW_STATE_MINIMIZED) {
            return None;
        }
        if let Some(focused) = self.focused_window()
            && focused != window
            && self.are_transient_relatives(focused, window)
        {
            return None;
        }
        if window.is_focused().get_untracked() {
            return None;
        }
        if self.should_tile(window) {
            let previous = self.active_window;
            self.active_window = Some(window);
            if previous != Some(window) {
                self.reapply_static_managed_layout();
            }
        }
        window.focus();
        Some(window)
    }

    fn are_transient_relatives(&self, a: Window, b: Window) -> bool {
        is_transient_child_of(a, b)
            || is_transient_child_of(b, a)
            || self.has_unparented_transient_affinity(a, b)
    }

    fn has_unparented_transient_affinity(&self, a: Window, b: Window) -> bool {
        let transient = (!self.should_tile(a) && a.is_transient().get_untracked()).then_some(a);
        let other = if transient == Some(a) {
            Some(b)
        } else {
            (!self.should_tile(b) && b.is_transient().get_untracked()).then_some(b)
        };
        let (Some(transient), Some(other)) = (transient, other) else {
            return false;
        };
        if transient.parent_id().get_untracked().is_some() {
            return false;
        }
        let app_id = transient.app_id().get_untracked();
        app_id.is_some() && app_id == other.app_id().get_untracked()
    }

    fn reapply_static_managed_layout(&mut self) {
        if !self.is_tiled || self.tileable_windows().is_empty() {
            return;
        }
        self.apply_layout(LayoutOptions {
            animate: Some(false),
            preserve_missing_active: true,
            cancel_rect_animations: Some(false),
            ..LayoutOptions::default()
        });
    }

    pub fn scroll_by(&mut self, delta_x: f64, stop_kinetic: bool, cancel_rect_animations: bool) -> bool {
        if !self.is_tiled || delta_x == 0.0 {
            return false;
        }
        if stop_kinetic {
            self.stop_kinetic_scroll();
        }
        let before = self.scroll_offset;
        self.scroll_offset += delta_x;
        self.clamp_scroll_offset();
        if self.scroll_offset == before {
            return false;
        }
        self.apply_layout(LayoutOptions {
            animate: Some(false),
            preserve_missing_active: true,
            cancel_rect_animations: Some(cancel_rect_animations),
            ..LayoutOptions::default()
        });
        true
    }

    /// Offsets where a slow scroll catches: the two full-visibility
    /// boundaries of every tile, and the centered offset of maximized tiles.
    fn tile_snap_offsets(&self) -> Vec<f64> {
        let tileable = self.tileable_windows();
        let viewport = self.tile_viewport_rect();
        let content_width = self.tile_content_width(&tileable, viewport);
        let max_scroll = (content_width - viewport.width).max(0.0);
        let mut offsets = Vec::new();
        let mut left = 0.0;
        for window in &tileable {
            let width = self.tile_width_for_window(*window, viewport);
            if get(*window, &WINDOW_STATE_MAXIMIZED) {
                offsets.push(left + width / 2.0 - viewport.width / 2.0);
            } else {
                offsets.push(left);
                offsets.push(left + width - viewport.width);
            }
            left += width + TILE_GAP;
        }
        let mut in_range: Vec<f64> = offsets
            .into_iter()
            .filter(|offset| *offset > 0.5 && *offset < max_scroll - 0.5)
            .collect();
        in_range.sort_by(f64::total_cmp);
        let mut deduped: Vec<f64> = Vec::new();
        for offset in in_range {
            if deduped.last().is_none_or(|last| offset - last > 1.0) {
                deduped.push(offset);
            }
        }
        deduped
    }

    /// First snap offset a scroll from `from` to `to` would cross.
    pub fn snap_offset_between(&self, from: f64, to: f64) -> Option<f64> {
        if to == from {
            return None;
        }
        let forward = to > from;
        let mut best: Option<f64> = None;
        for offset in self.tile_snap_offsets() {
            let crossed = if forward {
                offset > from + 0.5 && offset <= to
            } else {
                offset < from - 0.5 && offset >= to
            };
            if crossed && best.is_none_or(|best| if forward { offset < best } else { offset > best }) {
                best = Some(offset);
            }
        }
        best
    }

    pub fn scroll_position(&self) -> f64 {
        self.scroll_offset
    }

    /// Where a decaying glide settles: the tile closest to the screen center,
    /// flush to the edge it leans toward (centered when maximized).
    fn kinetic_snap_target(&self) -> Option<f64> {
        let viewport = self.tile_viewport_rect();
        let tileable = self.tileable_windows();
        let content_width = self.tile_content_width(&tileable, viewport);
        let max_scroll = (content_width - viewport.width).max(0.0);
        let from = clamp(self.scroll_offset, 0.0, max_scroll);
        let viewport_center = from + viewport.width / 2.0;
        let mut best: Option<(f64, f64)> = None;
        let mut left = 0.0;
        for window in &tileable {
            let width = self.tile_width_for_window(*window, viewport);
            let center = left + width / 2.0;
            let distance = (center - viewport_center).abs();
            let target = if get(*window, &WINDOW_STATE_MAXIMIZED) {
                center - viewport.width / 2.0
            } else if center < viewport_center {
                left
            } else {
                left + width - viewport.width
            };
            if best.is_none_or(|(best_distance, _)| distance < best_distance) {
                best = Some((distance, target));
            }
            left += width + TILE_GAP;
        }
        best.map(|(_, target)| clamp(target, 0.0, max_scroll))
    }

    /// Start a glide; returns whether the first step moved (the caller then
    /// runs its per-frame hook once).
    pub fn start_kinetic_scroll(&mut self, initial_velocity_x: f64, snap_max_velocity: f64) -> bool {
        self.stop_kinetic_scroll();
        if !self.is_tiled || initial_velocity_x.abs() < WORKSPACE_KINETIC_SCROLL_MIN_VELOCITY {
            return false;
        }
        self.kinetic_token += 1;
        let token = self.kinetic_token;
        self.kinetic = Some(KineticScroll {
            velocity_x: clamp(
                initial_velocity_x,
                -WORKSPACE_KINETIC_SCROLL_MAX_VELOCITY,
                WORKSPACE_KINETIC_SCROLL_MAX_VELOCITY,
            ),
            last_time: None,
            first_step: true,
            snap_max_velocity,
            timer: None,
        });
        let interval = self.kinetic_scroll_interval_ms();
        let (moved, running) = self.kinetic_step(interval);
        if !running {
            return moved;
        }
        let handle = self.handle.clone();
        let id = self.id;
        // The glide steps once per frame of the workspace's monitor.
        let timer = create_poll(interval, self.monitor.clone(), move |_| {
            handle.kinetic_frame(id, token);
        });
        if let Some(kinetic) = &mut self.kinetic {
            kinetic.timer = Some(timer);
        }
        moved
    }

    /// One frame of the glide, from its timer. Returns whether it moved.
    pub fn kinetic_tick(&mut self, token: u64) -> bool {
        if self.kinetic_token != token || !self.is_tiled || self.kinetic.is_none() {
            self.stop_kinetic_scroll();
            return false;
        }
        let interval = self.kinetic_scroll_interval_ms();
        let now = now_ms();
        let dt = {
            let kinetic = self.kinetic.as_mut().expect("checked above");
            let dt = kinetic.last_time.map_or(interval, |last| now - last).max(1.0);
            kinetic.last_time = Some(now);
            dt
        };
        self.kinetic_step(dt).0
    }

    /// Returns (moved, keep running).
    fn kinetic_step(&mut self, dt_ms: f64) -> (bool, bool) {
        let Some(kinetic) = &self.kinetic else {
            return (false, false);
        };
        let (velocity, first_step, snap_max) =
            (kinetic.velocity_x, kinetic.first_step, kinetic.snap_max_velocity);
        // Once the glide decays to snapping speed, settle on the anchor with
        // the standard tile animation.
        if snap_max > 0.0
            && velocity.abs() <= snap_max
            && let Some(target) = self.kinetic_snap_target()
        {
            self.stop_kinetic_scroll();
            if (target - self.scroll_offset).abs() > 0.5 {
                self.scroll_offset = target;
                self.apply_layout(LayoutOptions {
                    preserve_missing_active: true,
                    ..LayoutOptions::default()
                });
                return (true, false);
            }
            return (false, false);
        }

        let delta_x = velocity * dt_ms / 1000.0;
        let scrolled = self.scroll_by(delta_x, false, first_step);
        if let Some(kinetic) = &mut self.kinetic {
            kinetic.first_step = false;
        }
        if !scrolled {
            self.stop_kinetic_scroll();
            return (false, false);
        }
        let Some(kinetic) = &mut self.kinetic else {
            return (true, false);
        };
        kinetic.velocity_x *= (-dt_ms / WORKSPACE_KINETIC_SCROLL_TIME_CONSTANT_MS).exp();
        if kinetic.velocity_x.abs() < WORKSPACE_KINETIC_SCROLL_STOP_VELOCITY {
            self.stop_kinetic_scroll();
            return (true, false);
        }
        (true, true)
    }

    pub fn stop_kinetic_scroll(&mut self) {
        self.kinetic_token += 1;
        if let Some(kinetic) = self.kinetic.take()
            && let Some(timer) = kinetic.timer
        {
            timer.cancel();
        }
    }

    fn kinetic_scroll_interval_ms(&self) -> f64 {
        let refresh = COMPOSITOR
            .output
            .get(&self.monitor)
            .and_then(|output| output.resolution)
            .map(|mode| mode.refresh_rate)
            .filter(|rate| *rate > 0.0)
            .unwrap_or(WORKSPACE_KINETIC_SCROLL_FALLBACK_REFRESH_RATE);
        1000.0 / refresh.max(1.0)
    }

    pub fn focus_relative(&mut self, direction: i32) {
        let tileable = self.tileable_windows();
        if tileable.is_empty() {
            return;
        }
        let active_index = self
            .active_window
            .and_then(|active| tileable.iter().position(|window| *window == active));
        if let Some(index) = active_index
            && self.pan_active_tile_into_view(&tileable, index, direction)
        {
            return;
        }
        let fallback = self.focus_fallback_tile_index(&tileable, direction);
        let current = match (active_index, fallback) {
            (Some(index), _) => index as i64,
            (None, Some(fallback)) => fallback,
            (None, None) if direction < 0 => tileable.len() as i64,
            (None, None) => -1,
        };
        let next = (current + direction as i64).clamp(0, tileable.len() as i64 - 1) as usize;
        self.active_window = Some(tileable[next]);
        self.scroll_to_window(tileable[next], false);
        self.apply_layout(LayoutOptions::default());
        self.focus_active_window();
    }

    /// A focused tile sticking out on the side the focus key heads first
    /// pans fully into view; the next press moves on. Measured against the
    /// usable area (content inside the cosmetic margin is on screen).
    fn pan_active_tile_into_view(&mut self, tileable: &[Window], index: usize, direction: i32) -> bool {
        let viewport = self.tile_viewport_rect();
        let window_left = self.tile_left_for_index(tileable, index, viewport);
        let window_right = window_left + self.tile_width_for_window(tileable[index], viewport);
        let overflow = if direction < 0 {
            self.scroll_offset - window_left - TILE_MARGIN
        } else {
            window_right - (self.scroll_offset + viewport.width) - TILE_MARGIN
        };
        if overflow <= TILE_FOCUS_OVERFLOW_EPSILON {
            return false;
        }
        self.stop_kinetic_scroll();
        self.scroll_offset = if direction < 0 {
            window_left
        } else {
            window_right - viewport.width
        };
        self.clamp_scroll_offset();
        self.apply_layout(LayoutOptions::default());
        self.focus_active_window();
        true
    }

    fn focus_fallback_tile_index(&self, tileable: &[Window], direction: i32) -> Option<i64> {
        let focused = self.focused_window()?;
        if self.should_tile(focused) {
            return None;
        }
        let focused_center = get(focused, &WINDOW_STATE_RECT).center_x();
        let mut candidates: Vec<(usize, f64)> = tileable
            .iter()
            .enumerate()
            .map(|(index, window)| (index, get(*window, &WINDOW_STATE_RECT).center_x()))
            .filter(|(_, center)| {
                if direction < 0 {
                    *center < focused_center
                } else {
                    *center > focused_center
                }
            })
            .collect();
        if candidates.is_empty() {
            return None;
        }
        candidates.sort_by(|a, b| {
            if direction < 0 {
                b.1.total_cmp(&a.1)
            } else {
                a.1.total_cmp(&b.1)
            }
        });
        Some(candidates[0].0 as i64 - direction as i64)
    }

    pub fn focus_active_window(&self) {
        if let Some(active) = self.active_window.filter(|active| self.windows.contains(active)) {
            active.focus();
        }
    }

    pub fn should_tile(&self, window: Window) -> bool {
        window.is_resizable().get_untracked() && !window.is_transient().get_untracked()
    }

    pub fn reclassify_window(&mut self, window: Window, was_tileable: bool) {
        if !self.is_tiled || !self.has_window(window) {
            self.sync_window_visible_outputs(window);
            return;
        }
        let is_tileable = self.should_tile(window);
        if was_tileable == is_tileable {
            return;
        }
        self.stop_kinetic_scroll();
        if !is_tileable {
            let mut restored_active: Option<Option<Window>> = None;
            if let Some(state) = self.initial_tile_state.remove(&window) {
                self.scroll_offset = state.scroll_offset;
                restored_active = Some(state.active_window);
            }
            self.tile_width_by_window.remove(&window);
            stop_rect_animation(window, &WINDOW_STATE_RECT);
            // Removing this tile can clamp the scroll offset; the floating
            // content coordinate waits for apply_layout to settle it.
            set(window, &WINDOW_STATE_FLOATING_RECT, None);
            if self.active_window == Some(window) {
                let window_index = self.tile_index_of(window).unwrap_or(0);
                let tileable = self.tileable_windows();
                self.active_window = match restored_active.flatten() {
                    Some(active) if tileable.contains(&active) => Some(active),
                    _ if tileable.is_empty() => None,
                    _ => tileable.get(window_index.min(tileable.len() - 1)).copied(),
                };
            }
        } else {
            let viewport_rect = match get(window, &WINDOW_STATE_FLOATING_RECT) {
                Some(content) => self.floating_content_rect_to_viewport_rect(content),
                None => get(window, &WINDOW_STATE_RECT),
            };
            set(window, &WINDOW_STATE_FLOATING_RECT, Some(viewport_rect));
            self.set_tile_width_from_rect(window, viewport_rect, true);
            if window.is_focused().get_untracked() || self.active_window.is_none() {
                self.active_window = Some(window);
                self.scroll_to_window(window, false);
            }
        }
        self.sync_window_visible_outputs(window);
        self.apply_layout(LayoutOptions {
            preserve_missing_active: true,
            immediate_floating_windows: (!is_tileable).then(|| HashSet::from([window])),
            ..LayoutOptions::default()
        });
    }

    fn snapshot_window(&self, window: Window) -> WorkspaceWindowSnapshot {
        WorkspaceWindowSnapshot {
            tile_width: self.tile_width_by_window.get(&window).copied(),
            floating_rect: get(window, &WINDOW_STATE_FLOATING_RECT),
            restore_rect: get(window, &WINDOW_STATE_RESTORE_RECT),
            snap_zone: get(window, &WINDOW_STATE_SNAP_ZONE),
            snap_monitor: get(window, &WINDOW_STATE_SNAP_MONITOR),
            minimized: get(window, &WINDOW_STATE_MINIMIZED),
            maximized: get(window, &WINDOW_STATE_MAXIMIZED),
        }
    }

    fn sync_window_visible_outputs(&self, window: Window) {
        set(window, &WINDOW_STATE_WORKSPACE_TILED, self.is_tiled);
        set(window, &WINDOW_STATE_TILED, self.is_tiled && self.should_tile(window));
        set(
            window,
            &WINDOW_STATE_VISIBLE_OUTPUTS,
            self.is_tiled.then(|| vec![self.monitor.clone()]),
        );
    }

    /// Whether the window holds a slot in the tile sequence (minimized
    /// windows hold none).
    fn is_tileable(&self, window: Window) -> bool {
        self.should_tile(window) && !get(window, &WINDOW_STATE_MINIMIZED)
    }

    fn tileable_windows(&self) -> Vec<Window> {
        self.windows
            .iter()
            .copied()
            .filter(|window| self.is_tileable(*window))
            .collect()
    }

    /// The window's index in `tileable_windows()`, or the index it would
    /// take if it were a tile. `None` when it is not in this workspace.
    fn tile_index_of(&self, window: Window) -> Option<usize> {
        let position = self.windows.iter().position(|current| *current == window)?;
        Some(
            self.windows[..position]
                .iter()
                .filter(|current| self.is_tileable(**current))
                .count(),
        )
    }

    pub fn focused_window(&self) -> Option<Window> {
        self.windows
            .iter()
            .copied()
            .find(|window| window.is_focused().get_untracked())
    }

    pub fn sync_floating_window_rect(&self, window: Window, viewport_rect: Rect) {
        if !self.is_tiled {
            set(window, &WINDOW_STATE_FLOATING_RECT, Some(viewport_rect));
            return;
        }
        if self.should_tile(window) {
            return;
        }
        set(
            window,
            &WINDOW_STATE_FLOATING_RECT,
            Some(self.viewport_rect_to_floating_content_rect(viewport_rect)),
        );
    }

    fn active_window_in(&self, windows: &[Window]) -> Option<Window> {
        self.active_window.filter(|active| windows.contains(active))
    }

    fn tile_insertion_index_for_pointer(&self, window: Window, pointer_x: f64) -> usize {
        let tileable: Vec<Window> = self
            .tileable_windows()
            .into_iter()
            .filter(|current| *current != window)
            .collect();
        let viewport = self.tile_viewport_rect();
        let content_x = pointer_x - viewport.x + self.scroll_offset;
        let mut left = 0.0;
        for (index, tile) in tileable.iter().enumerate() {
            let width = self.tile_width_for_window(*tile, viewport);
            if content_x < left + width / 2.0 {
                return index;
            }
            left += width + TILE_GAP;
        }
        tileable.len()
    }

    fn tile_insertion_index_after_focused_window(&self) -> usize {
        let tileable = self.tileable_windows();
        let anchor = self
            .focused_window()
            .filter(|focused| self.should_tile(*focused))
            .or_else(|| self.active_window_in(&tileable));
        let Some(anchor) = anchor else {
            return tileable.len();
        };
        tileable
            .iter()
            .position(|window| *window == anchor)
            .map_or(tileable.len(), |index| index + 1)
    }

    fn move_tile_window_to_index(&mut self, window: Window, tile_index: usize) {
        let Some(current) = self.windows.iter().position(|candidate| *candidate == window) else {
            return;
        };
        self.windows.remove(current);
        let before = self.tileable_windows().get(tile_index).copied();
        if let Some(before) = before {
            let insert = self
                .windows
                .iter()
                .position(|candidate| *candidate == before)
                .unwrap_or(0);
            self.windows.insert(insert, window);
            return;
        }
        let last_tileable = self
            .windows
            .iter()
            .rposition(|candidate| self.should_tile(*candidate));
        self.windows
            .insert(last_tileable.map_or(0, |index| index + 1), window);
    }

    fn capture_floating_rect(&self, window: Window) {
        if get(window, &WINDOW_STATE_FLOATING_RECT).is_none() {
            let rect = get(window, &WINDOW_STATE_RECT);
            let rect = if self.is_tiled {
                self.viewport_rect_to_floating_content_rect(rect)
            } else {
                rect
            };
            set(window, &WINDOW_STATE_FLOATING_RECT, Some(rect));
        }
    }

    pub fn floating_windows(&self) -> Vec<Window> {
        self.windows
            .iter()
            .copied()
            .filter(|window| !self.should_tile(*window) && !get(*window, &WINDOW_STATE_MINIMIZED))
            .collect()
    }

    fn apply_floating_layout(&self, animate: bool, immediate: Option<&HashSet<Window>>) {
        for window in self.floating_windows() {
            // A maximized window's rect is owned by the maximize flow.
            if get(window, &WINDOW_STATE_MAXIMIZED) {
                continue;
            }
            let content_rect = get(window, &WINDOW_STATE_FLOATING_RECT).unwrap_or_else(|| {
                self.viewport_rect_to_floating_content_rect(self.centered_floating_rect(window))
            });
            set(window, &WINDOW_STATE_FLOATING_RECT, Some(content_rect));
            let rect = self.floating_content_rect_to_viewport_rect(content_rect);
            if animate && !immediate.is_some_and(|immediate| immediate.contains(&window)) {
                play_rect_animation(
                    window,
                    &WINDOW_STATE_RECT,
                    rect,
                    WINDOW_MANAGEMENT_EASING,
                    TILE_ANIMATION_DURATION,
                );
            } else {
                stop_rect_animation(window, &WINDOW_STATE_RECT);
                set(window, &WINDOW_STATE_RECT, rect);
            }
        }
    }

    fn centered_floating_rect(&self, window: Window) -> Rect {
        let size = (self.natural_root_rect)(window);
        let Some(output) = output_rect(&self.monitor) else {
            return size;
        };
        let area = COMPOSITOR.layer.usable_area(&self.monitor).unwrap_or(output);
        let (mut width, mut height) = (size.width, size.height);
        // A natural size read while the client geometry is still unsettled
        // is nothing but the SSD frame; freezing it as the floating rect
        // would later configure the client tiny. Fall back to a default.
        const DEGENERATE_SIZE_PX: f64 = 50.0;
        if width < DEGENERATE_SIZE_PX || height < DEGENERATE_SIZE_PX {
            width = (area.width * 0.6).round();
            height = (area.height * 0.7).round();
        }
        Rect::new(
            area.x + (area.width - width) / 2.0,
            area.y + (area.height - height) / 2.0,
            width,
            height,
        )
    }

    fn viewport_rect_to_floating_content_rect(&self, rect: Rect) -> Rect {
        Rect {
            x: rect.x + self.physical_aligned_scroll_offset(),
            ..rect
        }
    }

    fn floating_content_rect_to_viewport_rect(&self, rect: Rect) -> Rect {
        Rect {
            x: rect.x - self.physical_aligned_scroll_offset(),
            ..rect
        }
    }

    /// The scroll offset on the monitor's physical pixel grid, so adjacent
    /// windows move pixel-rigidly; the raw accumulator keeps sub-pixel
    /// gesture deltas.
    fn physical_aligned_scroll_offset(&self) -> f64 {
        let scale = COMPOSITOR
            .output
            .get(&self.monitor)
            .map_or(0.0, |output| output.scale);
        if !scale.is_finite() || scale <= 0.0 {
            return self.scroll_offset;
        }
        (self.scroll_offset * scale).round() / scale
    }

    fn clamp_rect_to_viewport(&self, rect: Rect) -> Rect {
        let viewport = self.tile_viewport_rect();
        let max_x = viewport.x + (viewport.width - rect.width).max(0.0);
        let max_y = viewport.y + (viewport.height - rect.height).max(0.0);
        Rect::new(
            clamp(rect.x, viewport.x, max_x),
            clamp(rect.y, viewport.y, max_y),
            rect.width,
            rect.height,
        )
    }

    fn scroll_to_window(&mut self, window: Window, force: bool) {
        self.stop_kinetic_scroll();
        let tileable = self.tileable_windows();
        let Some(index) = tileable.iter().position(|current| *current == window) else {
            return;
        };
        let viewport = self.tile_viewport_rect();
        let left = self.tile_left_for_index(&tileable, index, viewport);
        let right = left + self.tile_width_for_window(window, viewport);
        if get(window, &WINDOW_STATE_MAXIMIZED) || force {
            self.scroll_offset = left + (right - left) / 2.0 - viewport.width / 2.0;
        } else if left < self.scroll_offset {
            self.scroll_offset = left;
        } else if right > self.scroll_offset + viewport.width {
            self.scroll_offset = right - viewport.width;
        }
        self.clamp_scroll_offset();
    }

    fn clamp_scroll_offset(&mut self) {
        let tileable = self.tileable_windows();
        let viewport = self.tile_viewport_rect();
        let content_width = self.tile_content_width(&tileable, viewport);
        let max_scroll = (content_width - viewport.width).max(0.0);
        self.scroll_offset = clamp(self.scroll_offset, 0.0, max_scroll);
    }

    fn tile_width_for_window(&self, window: Window, viewport: Rect) -> f64 {
        if get(window, &WINDOW_STATE_MAXIMIZED) {
            return self.maximized_root_rect(window).width;
        }
        let width = self
            .tile_width_by_window
            .get(&window)
            .copied()
            .unwrap_or_else(|| default_tile_width(viewport));
        let min = self.min_tile_width(window, viewport);
        clamp(width, min, min.max(self.max_tile_width(window)))
    }

    fn maximized_tile_rect(&self, window: Window, x: f64) -> Rect {
        let maximized = self.maximized_root_rect(window);
        Rect { x, ..maximized }
    }

    fn set_tile_width_from_rect(&mut self, window: Window, rect: Rect, overwrite: bool) {
        if !overwrite && self.tile_width_by_window.contains_key(&window) {
            return;
        }
        let viewport = self.tile_viewport_rect();
        let min = self.min_tile_width(window, viewport);
        self.tile_width_by_window
            .insert(window, clamp(rect.width, min, min.max(self.max_tile_width(window))));
    }

    fn min_tile_width(&self, window: Window, viewport: Rect) -> f64 {
        let constraints = window.size_constraints().get_untracked();
        let extra = self.root_client_width_extra(window);
        TILE_MIN_WIDTH
            .max(constraints.min.map_or(1.0, |min| min.width as f64) + extra)
            .max(viewport.width * 0.2)
    }

    fn max_tile_width(&self, window: Window) -> f64 {
        let constraints = window.size_constraints().get_untracked();
        let extra = self.root_client_width_extra(window);
        match constraints.max.map(|max| max.width) {
            Some(max) if max > 0 => max as f64 + extra,
            _ => f64::INFINITY,
        }
    }

    fn root_client_width_extra(&self, window: Window) -> f64 {
        let natural = (self.natural_root_rect)(window);
        (natural.width - window.position().get_untracked().width).max(0.0)
    }

    fn tile_left_for_index(&self, tileable: &[Window], index: usize, viewport: Rect) -> f64 {
        tileable[..index]
            .iter()
            .map(|window| self.tile_width_for_window(*window, viewport) + TILE_GAP)
            .sum()
    }

    fn tile_content_width(&self, tileable: &[Window], viewport: Rect) -> f64 {
        if tileable.is_empty() {
            return 0.0;
        }
        tileable
            .iter()
            .map(|window| self.tile_width_for_window(*window, viewport))
            .sum::<f64>()
            + (tileable.len() - 1) as f64 * TILE_GAP
    }

    fn tile_viewport_rect(&self) -> Rect {
        let base = COMPOSITOR
            .layer
            .usable_area(&self.monitor)
            .or_else(|| output_rect(&self.monitor))
            .unwrap_or(Rect::new(0.0, 0.0, 1280.0, 720.0));
        inset_rect(base, TILE_MARGIN, TILE_MARGIN, TILE_MARGIN, TILE_MARGIN)
    }

    fn fullscreen_root_rect(&self, window: Window) -> Rect {
        output_rect(&self.monitor).unwrap_or_else(|| get(window, &WINDOW_STATE_RECT))
    }
}

fn default_tile_width(viewport: Rect) -> f64 {
    TILE_MIN_WIDTH.max(viewport.width * TILE_WIDTH_RATIO)
}

fn is_transient_child_of(child: Window, parent: Window) -> bool {
    child.is_transient().get_untracked()
        && child.parent_id().get_untracked().is_some_and(|id| id == parent.id())
}
