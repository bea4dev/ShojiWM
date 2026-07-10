import {
  createWindowStack,
  createWindowState,
  cubicBezier,
  createManagedPoll,
  markManagedWindowDirty,
  markWindowDirty,
  read,
  seconds,
  COMPOSITOR,
  type EasingFunction,
  type GestureSwipeEvent,
  type OutputChangeEvent,
  type PointerMoveEvent,
  type PollHandle,
  type ReadonlySignal,
  type WaylandWindow,
  type WindowActivateRequestEvent,
  type WindowFullscreenRequestEvent,
  type WindowMaximizeRequestEvent,
  type WindowMinimizeRequestEvent,
  type WindowMoveEvent,
  type WindowResizeEvent,
  type WindowResizeRect,
} from "shoji_wm";
import type { ManagedWindowRect, WindowSizeConstraints } from "shoji_wm/types";
import {
  playRectAnimation,
  stopRectAnimation,
  type RectAnimationOptions,
} from "./window-animation";

export type SnapZone =
  | "maximize"
  | "left"
  | "right"
  | "top-left"
  | "top-right"
  | "bottom-left"
  | "bottom-right";

export const WINDOW_STATE_RECT = createWindowState<ManagedWindowRect>("rect", {
  default: (window) => window.rect,
});
export const WINDOW_STATE_RESTORE_RECT =
  createWindowState<ManagedWindowRect | null>("restoreRect", {
    default: null,
  });
export const WINDOW_STATE_MINIMIZED = createWindowState<boolean>("minimized", {
  default: false,
});
export const WINDOW_STATE_MINIMIZE_VISUAL_IDLE = createWindowState<boolean>(
  "minimizeVisualIdle",
  {
    default: false,
  },
);
export const WINDOW_STATE_MAXIMIZED = createWindowState<boolean>("maximized", {
  default: false,
});
export const WINDOW_STATE_FULLSCREEN = createWindowState<boolean>(
  "fullscreen",
  {
    default: false,
  },
);
// Pre-fullscreen rect, kept separate from WINDOW_STATE_RESTORE_RECT so a
// window that was maximized before going fullscreen restores back to its
// maximized rect (and the maximize restore rect underneath stays intact).
export const WINDOW_STATE_FULLSCREEN_RESTORE_RECT =
  createWindowState<ManagedWindowRect | null>("fullscreenRestoreRect", {
    default: null,
  });
export const WINDOW_STATE_WORKSPACE_VISIBLE = createWindowState<boolean>(
  "workspaceVisible",
  {
    default: true,
  },
);
export const WINDOW_STATE_WORKSPACE_OFFSET_Y = createWindowState<number>(
  "workspaceOffsetY",
  {
    default: 0,
  },
);
export const WINDOW_STATE_WORKSPACE_OPACITY = createWindowState<number>(
  "workspaceOpacity",
  {
    default: 1,
  },
);
export const WINDOW_STATE_TILE_DRAGGING = createWindowState<boolean>(
  "tileDragging",
  {
    default: false,
  },
);
export const WINDOW_STATE_TILED = createWindowState<boolean>("tiled", {
  default: false,
});
export const WINDOW_STATE_VISIBLE_OUTPUTS = createWindowState<string[] | null>(
  "visibleOutputs",
  {
    default: null,
  },
);
export const WINDOW_STATE_FLOATING_RECT =
  createWindowState<ManagedWindowRect | null>("floatingRect", {
    default: null,
  });
export const WINDOW_STATE_SNAP_ZONE =
  createWindowState<SnapZone | null>("snapZone", {
    default: null,
  });
export const WINDOW_STATE_SNAP_MONITOR =
  createWindowState<string | null>("snapMonitor", {
    default: null,
  });

const OPEN_CLOSE_ANIMATION_DURATION = seconds(0.5);
const WINDOW_MANAGEMENT_ANIMATION_DURATION = seconds(0.3);
const UNMAXIMIZE_GRAB_ANIMATION_DURATION = 90;
const WINDOW_MANAGEMENT_EASING = cubicBezier(0.1, 0.9, 0.2, 1.0);
const WINDOW_OPEN_EASING = cubicBezier(0.1, 1.1, 0.1, 1.1);
const WINDOW_CLOSE_EASING = cubicBezier(0.3, -0.3, 0, 1);
const WINDOW_MINIMIZE_RECT_EASING = cubicBezier(0.3, -0.3, 0, 1);
const WINDOW_UNMINIMIZE_RECT_EASING = cubicBezier(0.1, 1.1, 0.1, 1.1);
const WINDOW_MINIMIZE_OPACITY_EASING = cubicBezier(0.3, -0.3, 0, 1);
const WINDOW_UNMINIMIZE_OPACITY_EASING = cubicBezier(0.1, 1.1, 0.1, 1.1);
export const TILE_ANIMATION_DURATION = seconds(0.5);
const WORKSPACE_SWITCH_ANIMATION_DURATION = seconds(0.5);
const WORKSPACE_GESTURE_FINGERS = 3;
const WORKSPACE_GESTURE_AXIS_LOCK_PX = 8;
const WORKSPACE_GESTURE_THRESHOLD_RATIO = 0.22;
const WORKSPACE_GESTURE_VELOCITY_THRESHOLD = 900;
const WORKSPACE_KINETIC_SCROLL_MIN_VELOCITY = 120;
const WORKSPACE_KINETIC_SCROLL_MAX_VELOCITY = 5000;
const WORKSPACE_KINETIC_SCROLL_STOP_VELOCITY = 18;
const WORKSPACE_KINETIC_SCROLL_TIME_CONSTANT_MS = 360;
const WORKSPACE_KINETIC_SCROLL_FALLBACK_REFRESH_RATE = 120;
const TILE_DRAG_WORKSPACE_EDGE_PX = 80;
const TILE_DRAG_WORKSPACE_SWITCH_INTERVAL_MS = 420;
const TILE_GAP = 12;
const TILE_MARGIN = 12;
const TILE_WIDTH_RATIO = 0.5;
const TILE_MIN_WIDTH = 240;
const MANAGED_WINDOW_ONLY_REBUILD_SUPPRESSION = {
  allowManagedWindowOnly: true,
  onViolation: "fallback-last",
} as const;
const STRICT_MANAGED_WINDOW_ONLY_REBUILD_SUPPRESSION = {
  allowManagedWindowOnly: true,
  onViolation: "fallback",
} as const;
const MANAGED_WINDOW_ONLY_ANIMATION = {
  suppressSSDRebuild: true,
} as const;

// Windows-style edge snapping for floating drags. Distances are logical px.
//   - within SNAP_EDGE_PX of an edge triggers that edge's zone
//   - within SNAP_CORNER_PX of a corner (along both axes) triggers a quarter
//   - SNAP_GAP_PX is the gap left between adjacent halves/quarters
const SNAP_EDGE_PX = 16;
const SNAP_CORNER_PX = 140;
const SNAP_GAP_PX = 8;

/** Monitor-local logical rect (relative to the monitor origin) for the bar. */
export interface SnapPreviewRect {
  x: number;
  y: number;
  width: number;
  height: number;
}

export interface SnapPreviewPayload {
  monitor: string;
  rect: SnapPreviewRect | null;
  kind: "floating" | "tiling";
}

export type SnapPreviewBroadcaster = (preview: SnapPreviewPayload) => void;
export type WorkspaceChangeBroadcaster = () => void;

type LayoutSnapZone = Exclude<SnapZone, "maximize">;
type SnapColumn = "left" | "right";

interface FloatingSnapLayout {
  splitX: number;
  leftSplitY: number;
  rightSplitY: number;
}

function markWindowCompositionDirty(window: WaylandWindow): void {
  markWindowDirty(window.id);
}

interface LayoutOptions {
  suppressSSDRebuild?: boolean;
  animate?: boolean;
  preserveMissingActive?: boolean;
  cancelRectAnimations?: boolean;
}

/**
 * Tiling layout mode for a workspace. Hyprland-inspired `dwindle` and `master`
 * join the original `scrolling` row layout; `scrolling` is the default so
 * existing behavior is preserved.
 */
export type TileLayout = "scrolling" | "dwindle" | "master";

const TILE_LAYOUT_ORDER: TileLayout[] = ["scrolling", "dwindle", "master"];

interface HybridWindowManagerSnapshot {
  currentMonitor: string;
  activeWorkspaceByMonitor: [string, number][];
  workspaces: WorkspaceSnapshot[];
}

interface WorkspaceSnapshot {
  monitor: string;
  index: number;
  isTiled: boolean;
  activeWindowId: string | null;
  scrollOffset: number;
  tileLayout: TileLayout;
  windows: WorkspaceWindowSnapshot[];
}

interface WorkspaceWindowSnapshot {
  id: string;
  tileWidth?: number;
  floatingRect?: ManagedWindowRect | null;
  restoreRect?: ManagedWindowRect | null;
  snapZone?: SnapZone | null;
  snapMonitor?: string | null;
  minimized: boolean;
  maximized: boolean;
}

/**
 * Compact, serializable view of the workspace layout for external clients
 * (e.g. the bar) consumed over the IPC transport. Per-monitor so a per-output
 * bar can render just its own workspaces.
 */
export interface WorkspacesViewWindow {
  id: string;
  appId?: string;
  title: string;
  focused: boolean;
  /** epoch ms — most recent focus time for MRU ordering. 0 if never focused. */
  lastFocusedAt: number;
}

export interface WorkspacesViewWorkspace {
  index: number;
  windowCount: number;
  isTiled: boolean;
  tileLayout: TileLayout;
  active: boolean;
  windows: WorkspacesViewWindow[];
}

export interface WorkspacesViewMonitor {
  name: string;
  active: number;
  workspaces: WorkspacesViewWorkspace[];
}

export interface WorkspacesView {
  currentMonitor: string;
  monitors: WorkspacesViewMonitor[];
}

interface WorkspaceGestureState {
  monitor: string;
  currentIndex: number;
  direction: -1 | 1;
  distance: number;
  fromWorkspace: Workspace;
  toWorkspace: Workspace | null;
  fromOffsetY: number;
  toOffsetY: number;
  fromOpacity: number;
  toOpacity: number;
}

type WorkspaceGestureMode = "workspace-switch" | "workspace-scroll";

export interface WorkspaceGestureSpeedConfig {
  /**
   * Horizontal three-finger swipe movement multiplier for scrolling inside a
   * tiled workspace.
   */
  workspaceScrollFactor?: number;
  /**
   * Horizontal release velocity multiplier for kinetic workspace scrolling.
   * Defaults to workspaceScrollFactor when omitted.
   */
  workspaceScrollKineticFactor?: number;
  /**
   * Vertical three-finger swipe movement multiplier for workspace switching.
   */
  workspaceSwitchFactor?: number;
  /**
   * Vertical release velocity multiplier for deciding whether to commit a
   * workspace switch. Defaults to workspaceSwitchFactor when omitted.
   */
  workspaceSwitchVelocityFactor?: number;
}

interface ResolvedWorkspaceGestureSpeedConfig {
  workspaceScrollFactor: number;
  workspaceScrollKineticFactor: number;
  workspaceSwitchFactor: number;
  workspaceSwitchVelocityFactor: number;
}

const DEFAULT_WORKSPACE_GESTURE_SPEED: ResolvedWorkspaceGestureSpeedConfig = {
  workspaceScrollFactor: 1,
  workspaceScrollKineticFactor: 1,
  workspaceSwitchFactor: 1,
  workspaceSwitchVelocityFactor: 1,
};

function hotReloadDebugEnabled(): boolean {
  const env = (globalThis as { process?: { env?: Record<string, string> } })
    .process?.env;
  const value = env?.SHOJI_HOT_RELOAD_DEBUG;
  return value !== undefined && value !== "" && value !== "0";
}

function hotReloadDebug(
  message: string,
  details: Record<string, unknown> = {},
): void {
  if (!hotReloadDebugEnabled()) {
    return;
  }
  console.info(`hot-reload ${message}`, JSON.stringify(details));
}

function sanitizeGestureSpeedFactor(
  value: number | undefined,
  fallback: number,
): number {
  if (value === undefined) {
    return fallback;
  }
  if (!Number.isFinite(value) || value <= 0) {
    return fallback;
  }
  return value;
}

const OPEN_ANIMATION_CHANNEL = "window.open";
const CLOSE_ANIMATION_CHANNEL = "window.close";
const MINIMIZE_ANIMATION_CHANNEL = "window.minimize";
const WORKSPACE_VISUAL_ANIMATION_CHANNEL = "workspace.visual";
const WORKSPACE_VISUAL_RECT_ANIMATION_CHANNEL = `${WORKSPACE_VISUAL_ANIMATION_CHANNEL}.rect`;
const WORKSPACE_VISUAL_OPACITY_ANIMATION_CHANNEL = `${WORKSPACE_VISUAL_ANIMATION_CHANNEL}.opacity`;
export const WINDOW_BORDER_PX = 2;
export const TITLEBAR_HEIGHT = 30;
export const MAXIMIZED_WINDOW_PADDING = {
  top: 8,
  right: 8,
  bottom: 8,
  left: 8,
};

export class HybridWindowManager {
  private readonly workspaces = new Map<string, Workspace>();
  private readonly activeWorkspaceByMonitor = new Map<string, number>();
  private readonly windowStack = createWindowStack();
  private readonly naturalRootRect: (rect: WaylandWindow) => ManagedWindowRect;
  // Tracks MRU focus time per window id so the dock can pick "the most recent
  // window of an app" deterministically. Updated by recordFocus().
  private readonly lastFocusedAt = new Map<string, number>();
  private readonly pendingInitialFocusByWindowId = new Map<string, number>();
  private currentMonitor: string;
  private isGrabbing = false;
  private tileDrag: {
    window: WaylandWindow;
    workspace: Workspace;
    lastWorkspaceSwitchAt: number;
  } | null = null;
  private floatingDrag: {
    window: WaylandWindow;
    workspace: Workspace;
    lastWorkspaceSwitchAt: number;
  } | null = null;
  private maximizedMoveDrag: {
    windowId: string;
    width: number;
    height: number;
  } | null = null;
  private workspaceGesture: WorkspaceGestureState | null = null;
  private workspaceGestureMode: WorkspaceGestureMode | null = null;
  private workspaceScrollGestureRectAnimationsCancelled = false;
  private workspaceGestureSpeed = { ...DEFAULT_WORKSPACE_GESTURE_SPEED };
  private lastPointerPosition: PointerMoveEvent["position"] | null = null;
  private lastPointerTarget: PointerMoveEvent["target"] = { kind: "none" };
  // Broadcasts the active snap-zone preview rect to external clients (the bar).
  private snapPreviewBroadcaster: SnapPreviewBroadcaster | null = null;
  private workspaceChangeBroadcaster: WorkspaceChangeBroadcaster | null = null;
  // Pending Windows-style snap decision for the in-flight floating drag.
  private floatingSnap: {
    windowId: string;
    monitor: string;
    zone: SnapZone;
    rect: ManagedWindowRect;
  } | null = null;

  public constructor(
    naturalRootRect: (rect: WaylandWindow) => ManagedWindowRect,
  ) {
    this.currentMonitor = "";
    this.naturalRootRect = naturalRootRect;
    this.syncWorkspaces();
  }

  public configureWorkspaceGestureSpeed(
    config: WorkspaceGestureSpeedConfig,
  ): void {
    const workspaceScrollFactor = sanitizeGestureSpeedFactor(
      config.workspaceScrollFactor,
      DEFAULT_WORKSPACE_GESTURE_SPEED.workspaceScrollFactor,
    );
    const workspaceSwitchFactor = sanitizeGestureSpeedFactor(
      config.workspaceSwitchFactor,
      DEFAULT_WORKSPACE_GESTURE_SPEED.workspaceSwitchFactor,
    );
    this.workspaceGestureSpeed = {
      workspaceScrollFactor,
      workspaceScrollKineticFactor: sanitizeGestureSpeedFactor(
        config.workspaceScrollKineticFactor,
        workspaceScrollFactor,
      ),
      workspaceSwitchFactor,
      workspaceSwitchVelocityFactor: sanitizeGestureSpeedFactor(
        config.workspaceSwitchVelocityFactor,
        workspaceSwitchFactor,
      ),
    };
  }

  public onPointerMove(event: PointerMoveEvent) {
    this.syncWorkspaces();
    this.currentMonitor = event.outputName ?? this.currentMonitor;
    this.lastPointerPosition = event.position;
    this.lastPointerTarget = event.target;
    this.focusWindowAtPointerTarget(event.target, event.outputName);
  }

  public onGestureSwipe(event: GestureSwipeEvent) {
    if (event.fingers !== WORKSPACE_GESTURE_FINGERS) {
      return;
    }

    this.syncWorkspaces();
    if (event.position) {
      this.lastPointerPosition = event.position;
    }

    if (event.phase === "begin") {
      this.workspaceGesture = null;
      this.workspaceGestureMode = null;
      this.workspaceScrollGestureRectAnimationsCancelled = false;
      this.currentMonitor = this.gestureMonitor(event);
      return;
    }

    if (event.phase === "update") {
      const mode = this.resolveWorkspaceGestureMode(event);
      if (mode === "workspace-scroll") {
        this.workspaceGesture = null;
        this.updateWorkspaceScrollGesture(event);
        return;
      }
      if (mode === "workspace-switch") {
        this.updateWorkspaceGesture(event);
      }
      return;
    }

    if (this.workspaceGestureMode === "workspace-scroll") {
      this.workspaceGestureMode = null;
      this.workspaceGesture = null;
      this.finishWorkspaceScrollGesture(event);
      this.workspaceScrollGestureRectAnimationsCancelled = false;
      this.focusWindowAtPointerPosition(
        event.position ?? this.lastPointerPosition,
        event.outputName,
      );
      return;
    }

    this.workspaceGestureMode = null;
    this.workspaceScrollGestureRectAnimationsCancelled = false;
    this.finishWorkspaceGesture(event);
  }

  public onOutputChange(event: OutputChangeEvent) {
    const liveMonitors = new Set(
      event.outputs
        .filter((output) => output.enabled)
        .map((output) => output.name),
    );
    if (liveMonitors.size === 0) {
      return;
    }

    const fallbackMonitor =
      (this.currentMonitor && liveMonitors.has(this.currentMonitor)
        ? this.currentMonitor
        : undefined) ?? Array.from(liveMonitors)[0];
    if (!fallbackMonitor) {
      return;
    }

    const orphanedWorkspaces = Array.from(this.workspaces.values()).filter(
      (workspace) => !liveMonitors.has(workspace.monitor),
    );
    if (orphanedWorkspaces.length === 0) {
      this.syncWorkspaces();
      this.refreshUsableAreaLayouts();
      return;
    }

    const orphanedActiveWorkspaceByMonitor = new Map(
      Array.from(this.activeWorkspaceByMonitor.entries()).filter(
        ([monitor]) => !liveMonitors.has(monitor),
      ),
    );

    for (const monitor of Array.from(this.activeWorkspaceByMonitor.keys())) {
      if (!liveMonitors.has(monitor)) {
        this.activeWorkspaceByMonitor.delete(monitor);
      }
    }

    for (const workspace of orphanedWorkspaces) {
      const oldKey = workspaceKey(workspace.monitor, workspace.index);
      this.workspaces.delete(oldKey);

      if (workspace.windowCount() === 0) {
        continue;
      }

      const targetMonitor = fallbackMonitor;
      const targetIndex = this.availableWorkspaceIndex(
        targetMonitor,
        workspace.index,
      );
      const wasActiveOnRemovedMonitor =
        orphanedActiveWorkspaceByMonitor.get(workspace.monitor) ===
        workspace.index;
      workspace.moveToMonitor(targetMonitor, targetIndex);
      this.workspaces.set(workspaceKey(targetMonitor, targetIndex), workspace);
      if (
        wasActiveOnRemovedMonitor ||
        !this.activeWorkspaceByMonitor.has(targetMonitor)
      ) {
        this.activeWorkspaceByMonitor.set(targetMonitor, targetIndex);
      }
      workspace.setVisible(workspace.isActive());
      workspace.applyLayout({
        suppressSSDRebuild: false,
        animate: false,
        preserveMissingActive: true,
      });
    }

    if (!liveMonitors.has(this.currentMonitor)) {
      this.currentMonitor = fallbackMonitor;
    }
    this.syncWorkspaces();
    this.refreshUsableAreaLayouts();
    this.syncWorkspaceVisibility();
  }

  public onOpen(window: WaylandWindow) {
    window.focus();
    this.windowStack.add(window);

    window.setCloseAnimationDuration(OPEN_CLOSE_ANIMATION_DURATION);
  }

  public snapshot(): HybridWindowManagerSnapshot {
    const snapshot = {
      currentMonitor: this.currentMonitor,
      activeWorkspaceByMonitor: Array.from(
        this.activeWorkspaceByMonitor.entries(),
      ),
      workspaces: Array.from(this.workspaces.values()).map((workspace) =>
        workspace.snapshot(),
      ),
    };
    hotReloadDebug("hybrid-snapshot", {
      currentMonitor: snapshot.currentMonitor,
      workspaceCount: snapshot.workspaces.length,
      workspaces: snapshot.workspaces.map((workspace) => ({
        monitor: workspace.monitor,
        index: workspace.index,
        isTiled: workspace.isTiled,
        activeWindowId: workspace.activeWindowId,
        windowIds: workspace.windows.map((window) => window.id),
      })),
    });
    return snapshot;
  }

  public restore(snapshot: HybridWindowManagerSnapshot) {
    hotReloadDebug("hybrid-restore", {
      currentMonitor: snapshot.currentMonitor,
      workspaceCount: snapshot.workspaces.length,
      workspaces: snapshot.workspaces.map((workspace) => ({
        monitor: workspace.monitor,
        index: workspace.index,
        isTiled: workspace.isTiled,
        activeWindowId: workspace.activeWindowId,
        windowIds: workspace.windows.map((window) => window.id),
      })),
    });
    this.currentMonitor = snapshot.currentMonitor;
    this.activeWorkspaceByMonitor.clear();
    for (const [monitor, index] of snapshot.activeWorkspaceByMonitor) {
      this.activeWorkspaceByMonitor.set(monitor, index);
    }
    this.workspaces.clear();
    for (const workspaceSnapshot of snapshot.workspaces) {
      const workspace = this.ensureWorkspace(
        workspaceSnapshot.monitor,
        workspaceSnapshot.index,
      );
      workspace.restore(workspaceSnapshot);
    }
  }

  public onFirstCommit(window: WaylandWindow) {
    if (!this.windowStack.has(window)) {
      this.windowStack.add(window, { at: "back" });
    }
    window.setCloseAnimationDuration(OPEN_CLOSE_ANIMATION_DURATION);

    let restoredExistingWindow = false;
    const workspace =
      this.findWorkspaceRestoringWindow(window) ?? this.getCurrentWorkspace();
    if (workspace) {
      restoredExistingWindow = workspace.addWindow(window);
      if (
        !restoredExistingWindow &&
        workspace.isTiled &&
        workspace.shouldTile(window)
      ) {
        this.trackPendingInitialFocus(window);
      }
      this.applyWorkspaceStackPolicy(workspace);
      this.syncWorkspaceVisibility();
    } else {
      window.state[WINDOW_STATE_RECT].set(this.naturalRootRect(window));
    }

    if (window.isMaximized()) {
      window.state[WINDOW_STATE_RESTORE_RECT].set(
        this.initialRestoreRectForMaximizedWindow(window),
      );
      window.state[WINDOW_STATE_RECT].set(this.maximizedRectForWindow(window));
      window.state[WINDOW_STATE_MAXIMIZED].set(true);
    }
    if (!restoredExistingWindow) {
      scheduleOpenAnimation(window);
    }
    hotReloadDebug("hybrid-first-commit", {
      windowId: window.id,
      title: window.title.peek(),
      workspace: workspace
        ? { monitor: workspace.monitor, index: workspace.index }
        : null,
      restoredExistingWindow,
      scheduledOpenAnimation: !restoredExistingWindow,
    });
  }

  public onStartClose(window: WaylandWindow) {
    scheduleCloseAnimation(window);

    for (const workspace of this.workspaces.values()) {
      const nextFocus = workspace.removeWindow(window);
      if (nextFocus !== undefined) {
        workspace.applyLayout();
        nextFocus?.focus();
        break;
      }
    }
    this.syncWorkspaceVisibility();
  }

  public onClose(window: WaylandWindow) {
    this.windowStack.remove(window);
    for (const workspace of this.workspaces.values()) {
      if (workspace.removeWindow(window) !== undefined) {
        workspace.applyLayout();
      }
    }
    this.syncWorkspaceVisibility();
  }

  public onFocus(window: WaylandWindow, focused: boolean) {
    if (focused) {
      this.windowStack.raise(window);
      const workspace = this.findWorkspaceForWindow(window);
      if (this.shouldDeferFocusLayoutForInitialOpen(window, workspace)) {
        this.applyWorkspaceStackPolicy(workspace);
        return;
      }
      if (workspace?.isTiled && workspace.isActive()) {
        workspace.focusWindow(window);
        this.applyWorkspaceStackPolicy(workspace);
      }
    }
  }

  public onWindowResize(event: WindowResizeEvent) {
    if (!read(event.window.isResizable)) {
      return;
    }

    const workspace = this.findWorkspaceForWindow(event.window);
    if (event.phase === "start" || event.phase === "update") {
      this.beginInteractiveUnmaximize(event.window);
    }

    if (workspace?.isTiled && workspace.shouldTile(event.window)) {
      workspace.resizeTile(event);
      this.applyWorkspaceStackPolicy(workspace);
      return;
    }

    const nextRect = this.constrainResizeRect(event);
    if (workspace && this.resizeFloatingSnapLayout(event, workspace, nextRect)) {
      this.applyWorkspaceStackPolicy(workspace);
      return;
    }

    stopRectAnimation(event.window, WINDOW_STATE_RECT);
    event.window.state[WINDOW_STATE_RECT].set(nextRect);
    workspace?.syncFloatingWindowRect(event.window, nextRect);
    this.applyWorkspaceStackPolicy(workspace);
  }

  public onWindowMove(event: WindowMoveEvent) {
    const workspace = this.findWorkspaceForWindow(event.window);
    if (workspace?.isTiled && workspace.shouldTile(event.window)) {
      this.onTileWindowMove(event, workspace);
      this.applyWorkspaceStackPolicy(workspace);
      return;
    }

    if (workspace) {
      this.onFloatingWindowMove(event, workspace);
      return;
    }

    const window = event.window;
    if (event.phase === "start" && window.state[WINDOW_STATE_MAXIMIZED]()) {
      const restoreRect =
        window.state[WINDOW_STATE_RESTORE_RECT]() ?? event.currentRect;
      this.maximizedMoveDrag = {
        windowId: window.id,
        width: read(restoreRect.width),
        height: read(restoreRect.height),
      };
      this.beginInteractiveUnmaximize(window);
    }
    if (event.phase === "start") {
      this.isGrabbing = true;
      this.clearWindowSnapState(window);
    }

    const maximizedMoveDrag =
      this.maximizedMoveDrag?.windowId === window.id
        ? this.maximizedMoveDrag
        : null;
    if (maximizedMoveDrag) {
      const nextRect = this.restoreRectForMaximizedMove(
        event,
        maximizedMoveDrag.width,
        maximizedMoveDrag.height,
      );
      if (event.phase === "start") {
        playRectAnimation(
          window,
          WINDOW_STATE_RECT,
          nextRect,
          WINDOW_MANAGEMENT_EASING,
          UNMAXIMIZE_GRAB_ANIMATION_DURATION,
        );
      } else {
        stopRectAnimation(window, WINDOW_STATE_RECT);
        window.state[WINDOW_STATE_RECT].set(nextRect);
      }
      if (event.phase === "end") {
        this.isGrabbing = false;
        this.maximizedMoveDrag = null;
        this.finishFloatingDragSnap(event, workspace);
      } else if (event.phase === "cancel") {
        this.isGrabbing = false;
        this.maximizedMoveDrag = null;
        this.finishFloatingDragSnap(event, workspace);
      } else {
        this.updateFloatingDragSnap(event);
      }
      return;
    }

    if (event.phase === "end" || event.phase === "cancel") {
      this.isGrabbing = false;
      const snapped = this.finishFloatingDragSnap(event, workspace);
      if (!snapped) {
        stopRectAnimation(window, WINDOW_STATE_RECT);
        window.state[WINDOW_STATE_RECT].set(event.currentRect);
      }
      this.applyWorkspaceStackPolicy(workspace);
      return;
    }

    this.updateFloatingDragSnap(event);
    stopRectAnimation(window, WINDOW_STATE_RECT);
    window.state[WINDOW_STATE_RECT].set(event.currentRect);
    this.applyWorkspaceStackPolicy(workspace);
  }

  private onFloatingWindowMove(event: WindowMoveEvent, workspace: Workspace) {
    const window = event.window;
    if (
      event.phase === "start" ||
      !this.floatingDrag ||
      this.floatingDrag.window.id !== window.id
    ) {
      this.isGrabbing = true;
      this.floatingDrag = {
        window,
        workspace,
        lastWorkspaceSwitchAt: event.timestamp,
      };
      if (window.state[WINDOW_STATE_MAXIMIZED]()) {
        const restoreRect =
          window.state[WINDOW_STATE_RESTORE_RECT]() ?? event.currentRect;
        this.maximizedMoveDrag = {
          windowId: window.id,
          width: read(restoreRect.width),
          height: read(restoreRect.height),
        };
        this.beginInteractiveUnmaximize(window);
      }
      this.clearWindowSnapState(window);
    }

    const drag = this.floatingDrag;
    if (!drag) {
      return;
    }

    const maximizedMoveDrag =
      this.maximizedMoveDrag?.windowId === window.id
        ? this.maximizedMoveDrag
        : null;
    const nextRect: ManagedWindowRect = maximizedMoveDrag
      ? this.restoreRectForMaximizedMove(
          event,
          maximizedMoveDrag.width,
          maximizedMoveDrag.height,
        )
      : event.currentRect;

    if (maximizedMoveDrag && event.phase === "start") {
      playRectAnimation(
        window,
        WINDOW_STATE_RECT,
        nextRect,
        WINDOW_MANAGEMENT_EASING,
        UNMAXIMIZE_GRAB_ANIMATION_DURATION,
      );
    } else {
      stopRectAnimation(window, WINDOW_STATE_RECT);
      window.state[WINDOW_STATE_RECT].set(nextRect);
    }

    if (event.phase !== "cancel") {
      const targetWorkspace = this.workspaceForFloatingDrag(event, drag);
      if (targetWorkspace !== drag.workspace) {
        drag.workspace.removeFloatingWindow(window);
        drag.workspace.applyLayout();
        if (targetWorkspace.isTiled && targetWorkspace.shouldTile(window)) {
          this.clearFloatingSnapPreview();
          targetWorkspace.adoptTileDragWindow(window, nextRect);
          drag.workspace = targetWorkspace;
          this.floatingDrag = null;
          this.tileDrag = {
            window,
            workspace: targetWorkspace,
            lastWorkspaceSwitchAt: event.timestamp,
          };
          this.syncWorkspaceVisibility();
          targetWorkspace.updateTileDrag(
            window,
            nextRect,
            event.currentPointer.x,
          );
          this.emitSnapPreview(
            targetWorkspace.monitor,
            targetWorkspace.draggingSlotRect(),
            "tiling",
          );
          this.applyWorkspaceStackPolicy(targetWorkspace);
          if (event.phase === "end") {
            targetWorkspace.endTileDrag(window, false);
            this.tileDrag = null;
            this.maximizedMoveDrag = null;
            this.isGrabbing = false;
          }
          window.focus();
          return;
        }
        targetWorkspace.adoptFloatingWindow(window, nextRect);
        drag.workspace = targetWorkspace;
        this.syncWorkspaceVisibility();
        window.focus();
      } else {
        targetWorkspace.syncFloatingWindowRect(window, nextRect);
      }

      this.applyWorkspaceStackPolicy(targetWorkspace);
      this.updateFloatingDragSnap(event);
    }

    if (event.phase === "end" || event.phase === "cancel") {
      const snapped = this.finishFloatingDragSnap(event, drag.workspace);
      if (!snapped) {
        stopRectAnimation(window, WINDOW_STATE_RECT);
        window.state[WINDOW_STATE_RECT].set(nextRect);
        drag.workspace.syncFloatingWindowRect(window, nextRect);
      }
      this.applyWorkspaceStackPolicy(drag.workspace);
      this.floatingDrag = null;
      if (maximizedMoveDrag) {
        this.maximizedMoveDrag = null;
      }
      this.isGrabbing = false;
    }
  }

  private onTileWindowMove(event: WindowMoveEvent, workspace: Workspace) {
    const window = event.window;
    if (
      event.phase === "start" ||
      !this.tileDrag ||
      this.tileDrag.window.id !== window.id
    ) {
      this.isGrabbing = true;
      workspace.beginTileDrag(window, event.currentRect);
      this.tileDrag = {
        window,
        workspace,
        lastWorkspaceSwitchAt: event.timestamp,
      };
    }

    const drag = this.tileDrag;
    if (!drag) {
      return;
    }

    if (event.phase === "end" || event.phase === "cancel") {
      this.emitSnapPreview(drag.workspace.monitor, null, "tiling");
      drag.workspace.endTileDrag(window, event.phase === "cancel");
      this.tileDrag = null;
      this.isGrabbing = false;
      return;
    }

    let targetWorkspace = this.workspaceForTileDrag(event, drag);
    if (targetWorkspace !== drag.workspace) {
      this.emitSnapPreview(drag.workspace.monitor, null, "tiling");
      drag.workspace.removeTileDragWindow(window);
      drag.workspace.applyLayout();
      if (!targetWorkspace.isTiled || !targetWorkspace.shouldTile(window)) {
        window.state[WINDOW_STATE_TILE_DRAGGING].set(false);
        targetWorkspace.adoptFloatingWindow(window, event.currentRect);
        this.tileDrag = null;
        this.floatingDrag = {
          window,
          workspace: targetWorkspace,
          lastWorkspaceSwitchAt: event.timestamp,
        };
        this.syncWorkspaceVisibility();
        this.applyWorkspaceStackPolicy(targetWorkspace);
        this.updateFloatingDragSnap(event);
        return;
      }
      targetWorkspace.adoptTileDragWindow(window, event.currentRect);
      drag.workspace = targetWorkspace;
      this.syncWorkspaceVisibility();
    }

    targetWorkspace.updateTileDrag(
      window,
      event.currentRect,
      event.currentPointer.x,
    );
    this.emitSnapPreview(
      targetWorkspace.monitor,
      targetWorkspace.draggingSlotRect(),
      "tiling",
    );
  }

  public onWindowMaximizeRequest(event: WindowMaximizeRequestEvent) {
    const workspace = this.findWorkspaceForWindow(event.window);
    if (this.isGrabbing) {
      return;
    }

    const window = event.window;
    window.state[WINDOW_STATE_MINIMIZED].set(false);
    this.clearWindowSnapState(window);

    if (workspace?.isTiled && workspace.shouldTile(window)) {
      if (!event.maximized) {
        window.state[WINDOW_STATE_RESTORE_RECT].set(null);
        window.state[WINDOW_STATE_MAXIMIZED].set(false);
        workspace.applyLayout();
        this.applyWorkspaceStackPolicy(workspace);
        return;
      }

      window.state[WINDOW_STATE_RESTORE_RECT].set(null);
      window.state[WINDOW_STATE_MAXIMIZED].set(true);
      workspace.focusWindow(window);
      workspace.applyLayout();
      this.applyWorkspaceStackPolicy(workspace);
      window.focus();
      return;
    }

    if (!event.maximized) {
      const restoreRect = window.state[WINDOW_STATE_RESTORE_RECT]();
      if (restoreRect) {
        workspace?.syncFloatingWindowRect(window, restoreRect);
        playRectAnimation(
          window,
          WINDOW_STATE_RECT,
          restoreRect,
          WINDOW_MANAGEMENT_EASING,
          WINDOW_MANAGEMENT_ANIMATION_DURATION,
        );
      }
      window.state[WINDOW_STATE_RESTORE_RECT].set(null);
      window.state[WINDOW_STATE_MAXIMIZED].set(false);
      return;
    }

    if (!window.state[WINDOW_STATE_MAXIMIZED]()) {
      const currentRect = window.state[WINDOW_STATE_RECT]();
      const currentWidth = read(currentRect.width);
      const currentHeight = read(currentRect.height);
      if (currentWidth > 1 && currentHeight > 1) {
        window.state[WINDOW_STATE_RESTORE_RECT].set(currentRect);
      }
    }
    const maximizedRect = this.maximizedRectForWindow(window);
    workspace?.syncFloatingWindowRect(window, maximizedRect);
    playRectAnimation(
      window,
      WINDOW_STATE_RECT,
      maximizedRect,
      WINDOW_MANAGEMENT_EASING,
      WINDOW_MANAGEMENT_ANIMATION_DURATION,
    );
    window.state[WINDOW_STATE_MAXIMIZED].set(true);
    this.applyWorkspaceStackPolicy(workspace);
  }

  public onWindowMinimizeRequest(event: WindowMinimizeRequestEvent) {
    const wasMinimized = event.window.state[WINDOW_STATE_MINIMIZED]();
    const workspace = this.findWorkspaceForWindow(event.window);
    if (wasMinimized !== event.minimized) {
      stopRectAnimation(event.window, WINDOW_STATE_RECT);
      if (!event.minimized) {
        event.window.state[WINDOW_STATE_MINIMIZE_VISUAL_IDLE].set(false);
      }
      event.window.state[WINDOW_STATE_MINIMIZED].set(event.minimized);
      if (event.minimized) {
        event.window.state[WINDOW_STATE_MINIMIZE_VISUAL_IDLE].set(true);
      }
      markWindowCompositionDirty(event.window);
      scheduleMinimizeAnimation(event.window, event.minimized);
    }
    if (workspace?.isTiled) {
      if (!event.minimized && workspace.shouldTile(event.window)) {
        workspace.focusWindow(event.window);
      } else {
        workspace.applyLayout();
      }
      this.applyWorkspaceStackPolicy(workspace);
    }
  }

  public onWindowActivateRequest(event: WindowActivateRequestEvent) {
    const wasMinimized = event.window.state[WINDOW_STATE_MINIMIZED]();
    if (wasMinimized) {
      this.onWindowMinimizeRequest({
        window: event.window,
        minimized: false,
        source:
          event.source === "xdg-activation" ||
          event.source === "xwayland" ||
          event.source === "keybind"
            ? event.source
            : "api",
        timestamp: event.timestamp,
      });
    }
    const workspace = this.findWorkspaceForWindow(event.window);
    if (workspace) {
      // If the window is on another workspace, switch with the same
      // slide/fade animation as keyboard/gesture switching (no-op if same).
      this.switchWorkspaceTo(workspace.monitor, workspace.index, {
        focusActiveAfter: false,
      });
    }
    // Focus the target window after switching (overrides switchWorkspaceTo's focusActiveWindow).
    event.window.focus();
  }

  public toggleCurrentWorkspaceTiling() {
    withManagedWindowOnlySSDRebuildSuppressed(() => {
      const workspace = this.getCurrentWorkspace();
      if (!workspace) {
        return;
      }
      workspace.setTiled(!workspace.isTiled);
      this.applyWorkspaceStackPolicy(workspace);
    });
  }

  public toggleWorkspaceTilingForMonitor(monitor: string) {
    withManagedWindowOnlySSDRebuildSuppressed(() => {
      this.syncWorkspaces();
      const workspace = this.workspaceForMonitor(monitor);
      if (!workspace) {
        return;
      }
      workspace.setTiled(!workspace.isTiled);
      this.applyWorkspaceStackPolicy(workspace);
    });
  }

  public focusTile(direction: -1 | 1) {
    withManagedWindowOnlySSDRebuildSuppressed(() => {
      const workspace = this.getCurrentWorkspace();
      if (!workspace?.isTiled) {
        return;
      }
      workspace.focusRelative(direction);
      this.applyWorkspaceStackPolicy(workspace);
    });
  }

  public moveFocusedTile(direction: -1 | 1) {
    withManagedWindowOnlySSDRebuildSuppressed(() => {
      const workspace = this.getCurrentWorkspace();
      if (!workspace?.isTiled) {
        return;
      }
      if (!workspace.moveFocusedTile(direction)) {
        return;
      }
      this.applyWorkspaceStackPolicy(workspace);
    });
  }

  /**
   * Switch the current workspace's tiling layout. The retile (and any broadcast)
   * is driven by the caller in index.tsx via scheduleWorkspaceBroadcast so the
   * bar and IPC clients stay in sync.
   */
  public setTileLayout(layout: TileLayout): void {
    withManagedWindowOnlySSDRebuildSuppressed(() => {
      const workspace = this.getCurrentWorkspace();
      if (!workspace) {
        return;
      }
      workspace.setTileLayout(layout);
      this.applyWorkspaceStackPolicy(workspace);
    });
  }

  /** scrolling -> dwindle -> master -> scrolling. */
  public cycleTileLayout(): void {
    withManagedWindowOnlySSDRebuildSuppressed(() => {
      const workspace = this.getCurrentWorkspace();
      if (!workspace) {
        return;
      }
      const current = workspace.getTileLayout();
      const nextIndex =
        (TILE_LAYOUT_ORDER.indexOf(current) + 1) % TILE_LAYOUT_ORDER.length;
      workspace.setTileLayout(
        TILE_LAYOUT_ORDER[nextIndex] ?? TILE_LAYOUT_ORDER[0],
      );
      this.applyWorkspaceStackPolicy(workspace);
    });
  }

  public moveFocusedWindowToWorkspace(direction: -1 | 1) {
    withManagedWindowOnlySSDRebuildSuppressed(() => {
      this.syncWorkspaces();

      const focused = Array.from(this.workspaces.values())
        .map((workspace) => ({
          workspace,
          window: workspace.focusedWindow(),
        }))
        .find(({ window }) => window !== undefined);
      const window = focused?.window;
      if (!window) {
        return;
      }

      const fromWorkspace = focused.workspace;
      const targetIndex = Math.max(1, fromWorkspace.index + direction);
      if (targetIndex === fromWorkspace.index) {
        return;
      }

      const targetWorkspace = this.ensureWorkspace(
        fromWorkspace.monitor,
        targetIndex,
      );
      const moved = fromWorkspace.takeWindowForMove(window);
      if (!moved) {
        return;
      }

      targetWorkspace.addMovedWindow(window, moved.snapshot);
      fromWorkspace.applyLayout();
      targetWorkspace.applyLayout();
      this.switchWorkspaceTo(fromWorkspace.monitor, targetIndex, {
        focusActiveAfter: false,
      });
      if (targetWorkspace.isTiled) {
        targetWorkspace.panToWindow(window);
      }
      window.focus();
      this.applyWorkspaceStackPolicy(fromWorkspace);
      this.applyWorkspaceStackPolicy(targetWorkspace);
      this.syncWorkspaceVisibility();
    });
  }

  public closeFocusedWindow() {
    for (const workspace of this.workspaces.values()) {
      const focused = workspace.focusedWindow();
      if (focused) {
        focused.close();
        return;
      }
    }
  }

  public toggleFocusedWindowMaximize() {
    for (const workspace of this.workspaces.values()) {
      const focused = workspace.focusedWindow();
      if (!focused || !read(focused.isResizable)) {
        continue;
      }

      if (focused.state[WINDOW_STATE_MAXIMIZED]()) {
        focused.unmaximize();
      } else {
        focused.maximize();
      }
      return;
    }
  }

  public refreshUsableAreaLayouts() {
    this.syncWorkspaces();
    // While a window is being interactively dragged, do not re-apply the
    // usable-area layout. It would clobber the in-flight drag — most visibly,
    // a maximized window (which stays WINDOW_STATE_MAXIMIZED during the
    // unmaximize-grab) gets snapped back to its full rect, flashing maximized
    // whenever a layer surface mounts/unmounts (e.g. the snap-preview overlay).
    // The layout is re-applied by the next layout event once the drag ends.
    if (this.isGrabbing) {
      return;
    }
    for (const workspace of this.workspaces.values()) {
      workspace.refreshUsableAreaLayout();
    }
    this.syncWorkspaceVisibility();
  }

  public switchWorkspace(direction: -1 | 1) {
    const monitor = this.currentMonitor || COMPOSITOR.output.list.at(0);
    if (!monitor) {
      return;
    }
    const currentIndex = this.activeWorkspaceByMonitor.get(monitor) ?? 1;
    this.switchWorkspaceTo(monitor, Math.max(1, currentIndex + direction));
  }

  /**
   * Animated switch to an explicit workspace index on a monitor. Direction is
   * inferred from the current index so the same vertical slide/fade transition
   * as keyboard/gesture switching plays (used by the IPC `workspaces.activate`).
   */
  public switchWorkspaceTo(
    monitor: string,
    targetIndex: number,
    options: { focusActiveAfter?: boolean } = {},
  ) {
    this.workspaceGesture = null;
    this.syncWorkspaces();
    if (!monitor || targetIndex < 1) {
      return;
    }

    const currentIndex = this.activeWorkspaceByMonitor.get(monitor) ?? 1;
    if (targetIndex === currentIndex) {
      return;
    }
    const direction: -1 | 1 = targetIndex > currentIndex ? 1 : -1;

    const fromWorkspace = this.ensureWorkspace(monitor, currentIndex);
    const toWorkspace = this.ensureWorkspace(monitor, targetIndex);
    const distance = this.workspaceTransitionDistance(monitor);

    this.activeWorkspaceByMonitor.set(monitor, targetIndex);
    this.currentMonitor = monitor;

    for (const workspace of this.workspaces.values()) {
      if (workspace === fromWorkspace || workspace === toWorkspace) {
        continue;
      }
      workspace.setVisible(workspace.isActive());
    }

    fromWorkspace.animateWorkspaceTransition({
      fromOffsetY: 0,
      toOffsetY: -direction * distance,
      fromOpacity: 1,
      toOpacity: 0,
      visibleAfter: false,
    });
    toWorkspace.prepareWorkspaceTransition(direction * distance, 0);
    toWorkspace.applyLayout();
    toWorkspace.animateWorkspaceTransition({
      fromOffsetY: direction * distance,
      toOffsetY: 0,
      fromOpacity: 0,
      toOpacity: 1,
      visibleAfter: true,
    });
    // Callers that explicitly want to focus a *different* window after the
    // transition (e.g. dock activation) opt out of the implicit focus so the
    // resulting onFocus callback chain does not stomp on their pan.
    if (options.focusActiveAfter !== false) {
      toWorkspace.focusActiveWindow();
    }
    this.applyWorkspaceStackPolicy(fromWorkspace);
    this.applyWorkspaceStackPolicy(toWorkspace);
    this.workspaceChangeBroadcaster?.();
  }

  public getCurrentWorkspace(): Workspace | undefined {
    this.syncWorkspaces();
    return (
      this.workspaceForMonitor(this.currentMonitor) ??
      this.workspaces.values().next().value
    );
  }

  /** Name (connector) of the monitor under the cursor; updated on pointer move. */
  public getCurrentMonitorName(): string {
    this.syncWorkspaces();
    return this.currentMonitor || COMPOSITOR.output.list.at(0) || "";
  }

  /**
   * Compact per-monitor workspace view for external clients (the bar) over IPC.
   */
  public viewForIpc(): WorkspacesView {
    this.syncWorkspaces();

    const byMonitor = new Map<string, WorkspacesViewWorkspace[]>();
    for (const workspace of this.workspaces.values()) {
      const active =
        this.activeWorkspaceByMonitor.get(workspace.monitor) ===
        workspace.index;
      const list = byMonitor.get(workspace.monitor) ?? [];
      const windows: WorkspacesViewWindow[] = workspace
        .listWindows()
        .map((window) => ({
          id: window.id,
          appId: window.appId(),
          title: window.title(),
          focused: window.isFocused(),
          lastFocusedAt: this.lastFocusedAt.get(window.id) ?? 0,
        }));
      list.push({
        index: workspace.index,
        windowCount: workspace.windowCount(),
        isTiled: workspace.isTiled,
        tileLayout: workspace.getTileLayout(),
        active,
        windows,
      });
      byMonitor.set(workspace.monitor, list);
    }

    const monitors: WorkspacesViewMonitor[] = COMPOSITOR.output.list.map(
      (name) => {
        const active = this.activeWorkspaceByMonitor.get(name) ?? 1;
        // Only surface workspaces that have windows, plus the active one
        // (which stays visible even while empty). Bare, empty, non-active
        // workspaces are hidden from the list.
        const workspaces = (byMonitor.get(name) ?? []).filter(
          (workspace) => workspace.windowCount > 0 || workspace.active,
        );
        if (!workspaces.some((workspace) => workspace.index === active)) {
          workspaces.push({
            index: active,
            windowCount: 0,
            isTiled: false,
            tileLayout: "scrolling",
            active: true,
            windows: [],
          });
        }
        workspaces.sort((a, b) => a.index - b.index);
        return { name, active, workspaces };
      },
    );

    return { currentMonitor: this.currentMonitor, monitors };
  }

  /**
   * Update MRU stamp for `windowId`. The dock uses this to pick the "most
   * recently used" window per app for left-click focus.
   */
  public recordFocus(windowId: string) {
    this.lastFocusedAt.set(windowId, Date.now());
  }

  private trackPendingInitialFocus(window: WaylandWindow) {
    const token = Date.now();
    this.pendingInitialFocusByWindowId.set(window.id, token);
    setTimeout(() => {
      if (this.pendingInitialFocusByWindowId.get(window.id) === token) {
        this.pendingInitialFocusByWindowId.delete(window.id);
      }
    }, WINDOW_MANAGEMENT_ANIMATION_DURATION);
  }

  private shouldDeferFocusLayoutForInitialOpen(
    window: WaylandWindow,
    workspace: Workspace | undefined,
  ): boolean {
    if (!workspace?.isTiled || !workspace.isActive()) {
      return false;
    }
    if (this.pendingInitialFocusByWindowId.delete(window.id)) {
      return false;
    }
    for (const pendingWindowId of this.pendingInitialFocusByWindowId.keys()) {
      if (
        workspace.isActiveWindowId(pendingWindowId) &&
        workspace.findWindowById(pendingWindowId)
      ) {
        return true;
      }
    }
    return false;
  }

  /**
   * Find a managed window by id by scanning every workspace. Used by the
   * `windows.activate` IPC handler to bridge bar clicks to focus + workspace
   * switch.
   */
  public findWindowById(windowId: string): WaylandWindow | undefined {
    for (const workspace of this.workspaces.values()) {
      const found = workspace.findWindowById(windowId);
      if (found) {
        return found;
      }
    }
    return undefined;
  }

  /**
   * Activate window by id (dock-style "go to this window"). Plays a unified
   * sequence: unminimize → switch workspace (if different) → pan within the
   * workspace so the target is centered → focus. Doing the pan synchronously
   * here (instead of relying on the onFocus → focusWindow callback) gives the
   * same in-sync animation as `Super+Ctrl+Left/Right` and guarantees a visible
   * pan even when the target is already in the viewport.
   *
   * Returns true if the window existed.
   */
  public activateWindowById(windowId: string): boolean {
    const window = this.findWindowById(windowId);
    if (!window) {
      return false;
    }
    const workspace = this.findWorkspaceForWindow(window);
    if (!workspace) {
      return false;
    }

    if (window.state[WINDOW_STATE_MINIMIZED]()) {
      this.onWindowMinimizeRequest({
        window,
        minimized: false,
        source: "api",
        timestamp: Date.now(),
      });
    }

    // Cross-workspace: switch first with the existing slide/fade. Skip the
    // implicit focusActiveWindow — we explicitly focus the target below, and
    // letting switchWorkspaceTo focus the previous active would queue an
    // onFocus → focusWindow → applyLayout cycle that overrides our pan.
    this.switchWorkspaceTo(workspace.monitor, workspace.index, {
      focusActiveAfter: false,
    });

    // Always pan to the target inside the workspace (force-center even if it
    // is already on-screen). This is the "go to this window" gesture.
    if (workspace.isTiled) {
      workspace.panToWindow(window);
    }

    // Focus last so it overrides switchWorkspaceTo's focusActiveWindow().
    window.focus();
    return true;
  }

  /**
   * Activate a specific workspace on a monitor (external/IPC entry point).
   * Plays the same slide/fade transition as keyboard/gesture switching.
   */
  public activate(monitor: string, index: number) {
    if (!monitor || index < 1) {
      return;
    }
    this.switchWorkspaceTo(monitor, index);
  }

  public getWindowZIndex(window: WaylandWindow): ReadonlySignal<number> {
    return this.windowStack.zIndex(window);
  }

  private beginInteractiveUnmaximize(window: WaylandWindow): boolean {
    if (!window.state[WINDOW_STATE_MAXIMIZED]()) {
      return false;
    }

    window.state[WINDOW_STATE_MAXIMIZED].set(false);
    window.state[WINDOW_STATE_RESTORE_RECT].set(null);
    this.clearWindowSnapState(window);
    window.unmaximize();
    return true;
  }

  private applyWorkspaceStackPolicy(workspace: Workspace | undefined) {
    if (!workspace) {
      return;
    }

    if (!workspace.isTiled) {
      // Leaving tiling mode removes the "floating windows stay above tiles"
      // policy. Restore normal focus-based stacking explicitly because
      // focusing an already-focused window does not emit another focus event.
      const focusedWindow = workspace.focusedWindow();
      if (focusedWindow && this.windowStack.has(focusedWindow)) {
        this.windowStack.raise(focusedWindow);
      }
      return;
    }

    const floating = workspace
      .floatingWindows()
      .filter((window) => this.windowStack.has(window))
      .sort(
        (a, b) =>
          this.windowStack.zIndexValue(a) - this.windowStack.zIndexValue(b),
      );

    for (const window of floating) {
      this.windowStack.raise(window);
    }
  }

  private syncWorkspaces() {
    for (const monitor of COMPOSITOR.output.list) {
      if (!this.activeWorkspaceByMonitor.has(monitor)) {
        this.activeWorkspaceByMonitor.set(monitor, 1);
      }
      this.ensureWorkspace(
        monitor,
        this.activeWorkspaceByMonitor.get(monitor) ?? 1,
      );
    }

    if (
      !this.currentMonitor ||
      !COMPOSITOR.output.list.includes(this.currentMonitor)
    ) {
      this.currentMonitor = COMPOSITOR.output.list.at(0) ?? "";
    }
  }

  private workspaceForMonitor(monitor: string): Workspace | undefined {
    if (!monitor) {
      return undefined;
    }
    return this.ensureWorkspace(
      monitor,
      this.activeWorkspaceByMonitor.get(monitor) ?? 1,
    );
  }

  private ensureWorkspace(monitor: string, index: number): Workspace {
    const key = workspaceKey(monitor, index);
    let workspace = this.workspaces.get(key);
    if (!workspace) {
      workspace = new Workspace(
        index,
        monitor,
        this.naturalRootRect,
        (window) => this.maximizedRectForWindow(window, monitor),
        (monitor) => this.getActiveWorkspaceIndex(monitor),
      );
      this.workspaces.set(key, workspace);
    }
    return workspace;
  }

  private getActiveWorkspaceIndex(monitor: string): number {
    return this.activeWorkspaceByMonitor.get(monitor) ?? 1;
  }

  private gestureMonitor(event: GestureSwipeEvent): string {
    const outputName = event.outputName;
    if (outputName && COMPOSITOR.output.list.includes(outputName)) {
      return outputName;
    }
    return this.currentMonitor || COMPOSITOR.output.list.at(0) || "";
  }

  private resolveWorkspaceGestureMode(
    event: GestureSwipeEvent,
  ): WorkspaceGestureMode | null {
    if (this.workspaceGestureMode) {
      return this.workspaceGestureMode;
    }

    const absX = Math.abs(event.totalX);
    const absY = Math.abs(
      event.totalY * this.workspaceGestureSpeed.workspaceSwitchFactor,
    );
    const scaledAbsX = absX * this.workspaceGestureSpeed.workspaceScrollFactor;
    if (Math.max(scaledAbsX, absY) < WORKSPACE_GESTURE_AXIS_LOCK_PX) {
      return null;
    }

    this.workspaceGestureMode =
      scaledAbsX > absY ? "workspace-scroll" : "workspace-switch";
    if (this.workspaceGestureMode === "workspace-scroll") {
      this.workspaceScrollGestureRectAnimationsCancelled = false;
    }
    return this.workspaceGestureMode;
  }

  private updateWorkspaceScrollGesture(event: GestureSwipeEvent) {
    const monitor = this.gestureMonitor(event);
    const workspace = this.workspaceForMonitor(monitor);
    if (!workspace?.isTiled) {
      return;
    }

    this.currentMonitor = monitor;
    workspace.stopKineticScroll();
    const deltaX =
      -event.deltaX * this.workspaceGestureSpeed.workspaceScrollFactor;
    const shouldCancelRectAnimations =
      !this.workspaceScrollGestureRectAnimationsCancelled;
    const scrolled = workspace.scrollBy(deltaX, {
      stopKinetic: false,
      cancelRectAnimations: shouldCancelRectAnimations,
    });
    if (scrolled && shouldCancelRectAnimations) {
      this.workspaceScrollGestureRectAnimationsCancelled = true;
    }
    this.focusWindowAtPointerPosition(
      event.position ?? this.lastPointerPosition,
      monitor,
    );
    this.applyWorkspaceStackPolicy(workspace);
  }

  private finishWorkspaceScrollGesture(event: GestureSwipeEvent) {
    if (event.phase !== "end") {
      return;
    }

    const monitor = this.gestureMonitor(event);
    const workspace = this.workspaceForMonitor(monitor);
    if (!workspace?.isTiled) {
      return;
    }

    workspace.startKineticScroll(
      -event.velocityX *
        this.workspaceGestureSpeed.workspaceScrollKineticFactor,
      () => {
        this.focusWindowAtPointerPosition(
          event.position ?? this.lastPointerPosition,
          monitor,
        );
        this.applyWorkspaceStackPolicy(workspace);
      },
    );
  }

  private updateWorkspaceGesture(event: GestureSwipeEvent) {
    const monitor = this.gestureMonitor(event);
    if (!monitor) {
      return;
    }

    const distance = Math.max(1, this.workspaceTransitionDistance(monitor));
    const scaledTotalY =
      event.totalY * this.workspaceGestureSpeed.workspaceSwitchFactor;
    const rawOffsetY = clamp(scaledTotalY, -distance, distance);
    if (Math.abs(rawOffsetY) < 1) {
      return;
    }

    const direction: -1 | 1 = rawOffsetY < 0 ? 1 : -1;
    const currentIndex = this.activeWorkspaceByMonitor.get(monitor) ?? 1;
    const nextIndex = currentIndex + direction;
    const fromWorkspace = this.ensureWorkspace(monitor, currentIndex);
    const toWorkspace =
      nextIndex >= 1 ? this.ensureWorkspace(monitor, nextIndex) : null;
    const targetChanged =
      this.workspaceGesture?.monitor !== monitor ||
      this.workspaceGesture.currentIndex !== currentIndex ||
      this.workspaceGesture.toWorkspace !== toWorkspace;

    this.currentMonitor = monitor;

    if (!toWorkspace) {
      if (targetChanged) {
        for (const workspace of this.workspaces.values()) {
          if (workspace === fromWorkspace) {
            continue;
          }
          workspace.setVisible(workspace.isActive());
        }
      }
      const resistanceOffsetY = rawOffsetY * 0.25;
      fromWorkspace.setWorkspaceGestureVisual(resistanceOffsetY, 1);
      this.workspaceGesture = {
        monitor,
        currentIndex,
        direction,
        distance,
        fromWorkspace,
        toWorkspace: null,
        fromOffsetY: resistanceOffsetY,
        toOffsetY: direction * distance,
        fromOpacity: 1,
        toOpacity: 0,
      };
      return;
    }

    const progress = clamp(Math.abs(rawOffsetY) / distance, 0, 1);
    const toOffsetY = direction * distance + rawOffsetY;
    const fromOpacity = 1 - progress;
    const toOpacity = progress;

    if (targetChanged) {
      for (const workspace of this.workspaces.values()) {
        if (workspace === fromWorkspace || workspace === toWorkspace) {
          continue;
        }
        workspace.setVisible(workspace.isActive());
      }
      toWorkspace.applyLayout();
    }

    fromWorkspace.setWorkspaceGestureVisual(rawOffsetY, fromOpacity);
    toWorkspace.setWorkspaceGestureVisual(toOffsetY, toOpacity);
    this.applyWorkspaceStackPolicy(fromWorkspace);
    this.applyWorkspaceStackPolicy(toWorkspace);

    this.workspaceGesture = {
      monitor,
      currentIndex,
      direction,
      distance,
      fromWorkspace,
      toWorkspace,
      fromOffsetY: rawOffsetY,
      toOffsetY,
      fromOpacity,
      toOpacity,
    };
  }

  private finishWorkspaceGesture(event: GestureSwipeEvent) {
    const gesture = this.workspaceGesture;
    this.workspaceGesture = null;
    if (!gesture) {
      return;
    }

    const shouldCommit =
      event.phase === "end" &&
      gesture.toWorkspace !== null &&
      (Math.abs(
        event.totalY * this.workspaceGestureSpeed.workspaceSwitchFactor,
      ) >=
        gesture.distance * WORKSPACE_GESTURE_THRESHOLD_RATIO ||
        Math.abs(
          event.velocityY *
            this.workspaceGestureSpeed.workspaceSwitchVelocityFactor,
        ) >= WORKSPACE_GESTURE_VELOCITY_THRESHOLD);

    if (shouldCommit && gesture.toWorkspace) {
      this.activeWorkspaceByMonitor.set(
        gesture.monitor,
        gesture.currentIndex + gesture.direction,
      );
      this.currentMonitor = gesture.monitor;
      gesture.fromWorkspace.animateWorkspaceTransition({
        fromOffsetY: gesture.fromOffsetY,
        toOffsetY: -gesture.direction * gesture.distance,
        fromOpacity: gesture.fromOpacity,
        toOpacity: 0,
        visibleAfter: false,
      });
      gesture.toWorkspace.animateWorkspaceTransition({
        fromOffsetY: gesture.toOffsetY,
        toOffsetY: 0,
        fromOpacity: gesture.toOpacity,
        toOpacity: 1,
        visibleAfter: true,
      });
      gesture.toWorkspace.focusActiveWindow();
      this.applyWorkspaceStackPolicy(gesture.fromWorkspace);
      this.applyWorkspaceStackPolicy(gesture.toWorkspace);
      return;
    }

    gesture.fromWorkspace.animateWorkspaceTransition({
      fromOffsetY: gesture.fromOffsetY,
      toOffsetY: 0,
      fromOpacity: gesture.fromOpacity,
      toOpacity: 1,
      visibleAfter: true,
    });
    if (gesture.toWorkspace) {
      gesture.toWorkspace.animateWorkspaceTransition({
        fromOffsetY: gesture.toOffsetY,
        toOffsetY: gesture.direction * gesture.distance,
        fromOpacity: gesture.toOpacity,
        toOpacity: 0,
        visibleAfter: false,
      });
    }
    this.applyWorkspaceStackPolicy(gesture.fromWorkspace);
  }

  private focusWindowAtPointerTarget(
    target: PointerMoveEvent["target"],
    monitorHint?: string,
  ) {
    if (target.kind !== "window") {
      return;
    }

    const workspace = Array.from(this.workspaces.values()).find((workspace) =>
      workspace.findWindowById(target.windowId),
    );
    const window = workspace?.findWindowById(target.windowId);
    if (!workspace || !window) {
      return;
    }

    if (!workspace.isTiled || !workspace.isActive()) {
      return;
    }

    const focused = workspace.focusWindowUnderPointer(window);
    if (!focused) {
      return;
    }

    this.currentMonitor =
      monitorHint && COMPOSITOR.output.list.includes(monitorHint)
        ? monitorHint
        : workspace.monitor;
  }

  private focusWindowAtPointerPosition(
    position: PointerMoveEvent["position"] | null | undefined,
    monitorHint?: string,
  ) {
    if (!position || this.lastPointerTarget.kind === "layer") {
      return;
    }

    const monitor =
      monitorHint && COMPOSITOR.output.list.includes(monitorHint)
        ? monitorHint
        : (this.outputNameAt(position.x, position.y) ?? this.currentMonitor);
    const workspace = this.workspaceForMonitor(monitor);
    if (!workspace?.isTiled || !workspace.isActive()) {
      return;
    }

    const window = workspace
      .listWindows()
      .filter(
        (window) =>
          !window.state[WINDOW_STATE_MINIMIZED]() &&
          this.windowStack.has(window) &&
          managedRectContainsPoint(
            window.state[WINDOW_STATE_RECT](),
            position.x,
            position.y -
              window.state[WINDOW_STATE_WORKSPACE_OFFSET_Y](),
          ),
      )
      .sort(
        (a, b) =>
          this.windowStack.zIndexValue(b) - this.windowStack.zIndexValue(a),
      )[0];
    if (!window || !workspace.focusWindowUnderPointer(window)) {
      return;
    }
    this.currentMonitor = monitor;
  }

  private availableWorkspaceIndex(monitor: string, preferredIndex: number) {
    if (!this.workspaces.has(workspaceKey(monitor, preferredIndex))) {
      return preferredIndex;
    }
    let index = 1;
    while (this.workspaces.has(workspaceKey(monitor, index))) {
      index += 1;
    }
    return index;
  }

  private syncWorkspaceVisibility() {
    for (const workspace of this.workspaces.values()) {
      workspace.setVisible(workspace.isActive());
    }
  }

  private findWorkspaceForWindow(window: WaylandWindow): Workspace | undefined {
    for (const workspace of this.workspaces.values()) {
      if (workspace.hasWindow(window)) {
        return workspace;
      }
    }
    return undefined;
  }

  private findWorkspaceRestoringWindow(
    window: WaylandWindow,
  ): Workspace | undefined {
    for (const workspace of this.workspaces.values()) {
      if (workspace.isRestoringWindow(window.id)) {
        return workspace;
      }
    }
    return undefined;
  }

  private workspaceForTileDrag(
    event: WindowMoveEvent,
    drag: NonNullable<HybridWindowManager["tileDrag"]>,
  ): Workspace {
    const monitor =
      event.outputName && COMPOSITOR.output.list.includes(event.outputName)
        ? event.outputName
        : drag.workspace.monitor;
    let index = this.activeWorkspaceByMonitor.get(monitor) ?? 1;
    const edgeDirection = this.tileDragWorkspaceEdgeDirection(
      monitor,
      event.currentPointer.y,
    );

    if (
      event.modifiers.shift &&
      edgeDirection !== 0 &&
      event.timestamp - drag.lastWorkspaceSwitchAt >=
        TILE_DRAG_WORKSPACE_SWITCH_INTERVAL_MS
    ) {
      const nextIndex = Math.max(1, index + edgeDirection);
      if (nextIndex !== index) {
        this.currentMonitor = monitor;
        this.switchWorkspace(edgeDirection);
        drag.lastWorkspaceSwitchAt = event.timestamp;
        index = this.activeWorkspaceByMonitor.get(monitor) ?? nextIndex;
      }
    }

    return this.ensureWorkspace(monitor, index);
  }

  private workspaceForFloatingDrag(
    event: WindowMoveEvent,
    drag: NonNullable<HybridWindowManager["floatingDrag"]>,
  ): Workspace {
    const monitor =
      event.outputName && COMPOSITOR.output.list.includes(event.outputName)
        ? event.outputName
        : drag.workspace.monitor;
    let index = this.activeWorkspaceByMonitor.get(monitor) ?? 1;
    const edgeDirection = this.tileDragWorkspaceEdgeDirection(
      monitor,
      event.currentPointer.y,
    );

    if (
      event.modifiers.shift &&
      edgeDirection !== 0 &&
      event.timestamp - drag.lastWorkspaceSwitchAt >=
        TILE_DRAG_WORKSPACE_SWITCH_INTERVAL_MS
    ) {
      const nextIndex = Math.max(1, index + edgeDirection);
      if (nextIndex !== index) {
        this.currentMonitor = monitor;
        this.switchWorkspace(edgeDirection);
        drag.lastWorkspaceSwitchAt = event.timestamp;
        index = this.activeWorkspaceByMonitor.get(monitor) ?? nextIndex;
      }
    }

    return this.ensureWorkspace(monitor, index);
  }

  private tileDragWorkspaceEdgeDirection(
    monitor: string,
    y: number,
  ): -1 | 0 | 1 {
    const rect = this.workspaceViewportRect(monitor);
    const top = read(rect.y);
    const height = read(rect.height);
    if (y < top + TILE_DRAG_WORKSPACE_EDGE_PX) {
      return -1;
    }
    if (y > top + height - TILE_DRAG_WORKSPACE_EDGE_PX) {
      return 1;
    }
    return 0;
  }

  private workspaceTransitionDistance(monitor: string): number {
    return read(this.workspaceViewportRect(monitor).height);
  }

  private workspaceViewportRect(monitor: string): ManagedWindowRect {
    const usable = COMPOSITOR.layer.usableArea(monitor);
    if (usable) {
      return usable;
    }

    const output = COMPOSITOR.output.current[monitor];
    if (output?.resolution) {
      return {
        x: output.position.x,
        y: output.position.y,
        width: output.resolution.width / output.scale,
        height: output.resolution.height / output.scale,
      };
    }

    return {
      x: 0,
      y: 0,
      width: 1280,
      height: 720,
    };
  }

  private constrainResizeRect(event: WindowResizeEvent): ManagedWindowRect {
    const constraints = event.window.sizeConstraints();
    const extra = this.clientToRootSizeExtra(event.window);
    const minWidth = Math.max(1, constraints.min?.width ?? 1) + extra.width;
    const minHeight = Math.max(1, constraints.min?.height ?? 1) + extra.height;
    const maxWidth = constrainedMax(constraints, "width", extra.width);
    const maxHeight = constrainedMax(constraints, "height", extra.height);

    const width = clamp(
      event.currentRect.width,
      minWidth,
      Math.max(minWidth, maxWidth),
    );
    const height = clamp(
      event.currentRect.height,
      minHeight,
      Math.max(minHeight, maxHeight),
    );

    return {
      x: resizeOriginForAxis(
        event.startRect,
        event.currentRect,
        width,
        event.edges.left,
        "x",
      ),
      y: resizeOriginForAxis(
        event.startRect,
        event.currentRect,
        height,
        event.edges.top,
        "y",
      ),
      width,
      height,
    };
  }

  private clientToRootSizeExtra(window: WaylandWindow): {
    width: number;
    height: number;
  } {
    const natural = this.naturalRootRect(window);
    return {
      width: Math.max(0, read(natural.width) - window.position.width),
      height: Math.max(0, read(natural.height) - window.position.height),
    };
  }

  private maximizedRectForWindow(
    window: WaylandWindow,
    preferredOutput?: string,
  ): ManagedWindowRect {
    const rect = window.state[WINDOW_STATE_RECT]();
    const centerX = read(rect.x) + read(rect.width) / 2;
    const centerY = read(rect.y) + read(rect.height) / 2;
    const outputName =
      preferredOutput ??
      this.outputNameAt(centerX, centerY) ??
      this.currentMonitor;
    const output = outputName
      ? COMPOSITOR.output.current[outputName]
      : undefined;
    const usable = outputName
      ? COMPOSITOR.layer.usableArea(outputName)
      : undefined;

    if (usable) {
      return insetRect(
        {
          x: usable.x,
          y: usable.y,
          width: usable.width,
          height: usable.height,
        },
        MAXIMIZED_WINDOW_PADDING,
      );
    }
    if (output?.resolution) {
      return insetRect(
        {
          x: output.position.x,
          y: output.position.y,
          width: output.resolution.width / output.scale,
          height: output.resolution.height / output.scale,
        },
        MAXIMIZED_WINDOW_PADDING,
      );
    }
    return rect;
  }

  // Fullscreen covers the entire output: unlike maximize it ignores the
  // usable area (exclusive-zone bars) and applies no padding, so the client
  // surface spans edge to edge. This is also what lets the tty backend
  // collapse the frame to a single scanout-capable element.
  private fullscreenRectForWindow(
    window: WaylandWindow,
    preferredOutput?: string,
  ): ManagedWindowRect {
    const rect = window.state[WINDOW_STATE_RECT]();
    const centerX = read(rect.x) + read(rect.width) / 2;
    const centerY = read(rect.y) + read(rect.height) / 2;
    const outputName =
      preferredOutput ??
      this.outputNameAt(centerX, centerY) ??
      this.currentMonitor;
    const output = outputName
      ? COMPOSITOR.output.current[outputName]
      : undefined;
    if (output?.resolution) {
      return {
        x: output.position.x,
        y: output.position.y,
        width: output.resolution.width / output.scale,
        height: output.resolution.height / output.scale,
      };
    }
    return rect;
  }

  public onWindowFullscreenRequest(event: WindowFullscreenRequestEvent) {
    if (this.isGrabbing) {
      return;
    }
    const window = event.window;
    const workspace = this.findWorkspaceForWindow(window);
    window.state[WINDOW_STATE_MINIMIZED].set(false);
    this.clearWindowSnapState(window);

    if (!event.fullscreen) {
      const restoreRect = window.state[WINDOW_STATE_FULLSCREEN_RESTORE_RECT]();
      window.state[WINDOW_STATE_FULLSCREEN].set(false);
      window.state[WINDOW_STATE_FULLSCREEN_RESTORE_RECT].set(null);
      // A tiled window returns to its computed slot; a floating one animates
      // back to where it was before going fullscreen.
      if (workspace?.isTiled && workspace.shouldTile(window)) {
        workspace.applyLayout();
        this.applyWorkspaceStackPolicy(workspace);
        return;
      }
      if (restoreRect) {
        workspace?.syncFloatingWindowRect(window, restoreRect);
        playRectAnimation(
          window,
          WINDOW_STATE_RECT,
          restoreRect,
          WINDOW_MANAGEMENT_EASING,
          WINDOW_MANAGEMENT_ANIMATION_DURATION,
        );
      }
      this.applyWorkspaceStackPolicy(workspace);
      return;
    }

    if (!window.state[WINDOW_STATE_FULLSCREEN]()) {
      const currentRect = window.state[WINDOW_STATE_RECT]();
      const currentWidth = read(currentRect.width);
      const currentHeight = read(currentRect.height);
      if (currentWidth > 1 && currentHeight > 1) {
        window.state[WINDOW_STATE_FULLSCREEN_RESTORE_RECT].set(currentRect);
      }
    }
    const fullscreenRect = this.fullscreenRectForWindow(
      window,
      event.outputName,
    );
    window.state[WINDOW_STATE_FULLSCREEN].set(true);
    workspace?.focusWindow(window);
    workspace?.syncFloatingWindowRect(window, fullscreenRect);
    playRectAnimation(
      window,
      WINDOW_STATE_RECT,
      fullscreenRect,
      WINDOW_MANAGEMENT_EASING,
      WINDOW_MANAGEMENT_ANIMATION_DURATION,
    );
    this.applyWorkspaceStackPolicy(workspace);
    window.focus();
  }

  private initialRestoreRectForMaximizedWindow(
    window: WaylandWindow,
  ): ManagedWindowRect {
    const maximizedRect = this.maximizedRectForWindow(window);
    const width = Math.max(1, read(maximizedRect.width) * 0.7);
    const height = Math.max(1, read(maximizedRect.height) * 0.7);
    return {
      x: read(maximizedRect.x) + (read(maximizedRect.width) - width) / 2,
      y: read(maximizedRect.y) + (read(maximizedRect.height) - height) / 2,
      width,
      height,
    };
  }

  private restoreRectForMaximizedMove(
    event: WindowMoveEvent,
    width: number,
    height: number,
  ): ManagedWindowRect {
    const pointer = event.currentPointer;
    const titlebarCenterY = WINDOW_BORDER_PX + TITLEBAR_HEIGHT / 2;
    const pointerOffsetY =
      event.source === "modifier"
        ? height / 2
        : Math.min(height / 2, titlebarCenterY);

    return {
      x: pointer.x - width / 2,
      y: pointer.y - pointerOffsetY,
      width,
      height,
    };
  }

  private outputNameAt(x: number, y: number): string | undefined {
    for (const name of COMPOSITOR.output.list) {
      const output = COMPOSITOR.output.current[name];
      if (!output?.resolution) {
        continue;
      }
      const width = output.resolution.width / output.scale;
      const height = output.resolution.height / output.scale;
      if (
        x >= output.position.x &&
        y >= output.position.y &&
        x < output.position.x + width &&
        y < output.position.y + height
      ) {
        return name;
      }
    }
    return undefined;
  }

  // -------------------------------------------------------------------------
  // Snap zones (Windows-style edge snapping for floating drags + tiling drag
  // slot preview). The bar renders the preview rect; this side decides the
  // zone, broadcasts the preview, and applies the snap on drop.
  // -------------------------------------------------------------------------

  public setSnapPreviewBroadcaster(broadcaster: SnapPreviewBroadcaster | null) {
    this.snapPreviewBroadcaster = broadcaster;
  }

  public setWorkspaceChangeBroadcaster(
    broadcaster: WorkspaceChangeBroadcaster | null,
  ) {
    this.workspaceChangeBroadcaster = broadcaster;
  }

  /** Full logical rect of a monitor (ignores reserved insets). */
  private monitorFullRect(monitor: string): ManagedWindowRect | null {
    const output = COMPOSITOR.output.current[monitor];
    if (!output?.resolution) {
      return null;
    }
    return {
      x: output.position.x,
      y: output.position.y,
      width: output.resolution.width / output.scale,
      height: output.resolution.height / output.scale,
    };
  }

  /** Usable area inset by the maximized padding — the base for all snap rects. */
  private monitorSnapBaseRect(monitor: string): ManagedWindowRect | null {
    const usable =
      COMPOSITOR.layer.usableArea(monitor) ??
      this.monitorFullRect(monitor);
    if (!usable) {
      return null;
    }
    return insetRect(usable, MAXIMIZED_WINDOW_PADDING);
  }

  /** Resolve the snap zone for a pointer near the physical screen edges. */
  private floatingSnapZoneAt(
    monitor: string,
    px: number,
    py: number,
  ): SnapZone | null {
    const full = this.monitorFullRect(monitor);
    if (!full) {
      return null;
    }
    const left = read(full.x);
    const top = read(full.y);
    const right = left + read(full.width);
    const bottom = top + read(full.height);

    const nearLeft = px <= left + SNAP_EDGE_PX;
    const nearRight = px >= right - SNAP_EDGE_PX;
    const nearTop = py <= top + SNAP_EDGE_PX;

    // Corners win over edges so the quarters stay reachable.
    if (nearLeft && py <= top + SNAP_CORNER_PX) return "top-left";
    if (nearLeft && py >= bottom - SNAP_CORNER_PX) return "bottom-left";
    if (nearRight && py <= top + SNAP_CORNER_PX) return "top-right";
    if (nearRight && py >= bottom - SNAP_CORNER_PX) return "bottom-right";
    if (nearTop) return "maximize";
    if (nearLeft) return "left";
    if (nearRight) return "right";
    return null;
  }

  /** Target rect (global logical coords) for a snap zone on a monitor. */
  private snapZoneRect(
    monitor: string,
    zone: SnapZone,
  ): ManagedWindowRect | null {
    const base = this.monitorSnapBaseRect(monitor);
    if (!base) {
      return null;
    }
    const bx = read(base.x);
    const by = read(base.y);
    const bw = read(base.width);
    const bh = read(base.height);
    const halfW = (bw - SNAP_GAP_PX) / 2;
    const halfH = (bh - SNAP_GAP_PX) / 2;
    const rightX = bx + halfW + SNAP_GAP_PX;
    const bottomY = by + halfH + SNAP_GAP_PX;

    switch (zone) {
      case "maximize":
        return { x: bx, y: by, width: bw, height: bh };
      case "left":
        return { x: bx, y: by, width: halfW, height: bh };
      case "right":
        return { x: rightX, y: by, width: halfW, height: bh };
      case "top-left":
        return { x: bx, y: by, width: halfW, height: halfH };
      case "top-right":
        return { x: rightX, y: by, width: halfW, height: halfH };
      case "bottom-left":
        return { x: bx, y: bottomY, width: halfW, height: halfH };
      case "bottom-right":
        return { x: rightX, y: bottomY, width: halfW, height: halfH };
    }
  }

  private resizeFloatingSnapLayout(
    event: WindowResizeEvent,
    workspace: Workspace,
    nextRect: ManagedWindowRect,
  ): boolean {
    if (workspace.isTiled) {
      return false;
    }

    const zone = event.window.state[WINDOW_STATE_SNAP_ZONE]();
    if (!isLayoutSnapZone(zone)) {
      return false;
    }

    const monitor =
      event.window.state[WINDOW_STATE_SNAP_MONITOR]() || workspace.monitor;
    const base = this.monitorSnapBaseRect(monitor);
    if (!base) {
      return false;
    }

    const snappedWindows = workspace
      .listWindows()
      .filter((window) => this.isWindowInFloatingSnapLayout(window, monitor));
    if (!snappedWindows.some((window) => window.id === event.window.id)) {
      return false;
    }

    const layout = this.floatingSnapLayoutFromWindows(base, snappedWindows);
    let changed = false;

    if (event.edges.right && isLeftSnapZone(zone)) {
      layout.splitX = read(nextRect.x) + read(nextRect.width);
      changed = true;
    } else if (event.edges.left && isRightSnapZone(zone)) {
      layout.splitX = read(nextRect.x) - SNAP_GAP_PX;
      changed = true;
    }

    if (event.edges.bottom && isTopSnapZone(zone)) {
      if (snapColumn(zone) === "left") {
        layout.leftSplitY = read(nextRect.y) + read(nextRect.height);
      } else {
        layout.rightSplitY = read(nextRect.y) + read(nextRect.height);
      }
      changed = true;
    } else if (event.edges.top && isBottomSnapZone(zone)) {
      if (snapColumn(zone) === "left") {
        layout.leftSplitY = read(nextRect.y) - SNAP_GAP_PX;
      } else {
        layout.rightSplitY = read(nextRect.y) - SNAP_GAP_PX;
      }
      changed = true;
    }

    if (!changed) {
      return false;
    }

    this.clampFloatingSnapLayout(base, layout, snappedWindows);
    this.applyFloatingSnapLayout(base, layout, snappedWindows);
    return true;
  }

  private isWindowInFloatingSnapLayout(
    window: WaylandWindow,
    monitor: string,
  ): boolean {
    return (
      isLayoutSnapZone(window.state[WINDOW_STATE_SNAP_ZONE]()) &&
      window.state[WINDOW_STATE_SNAP_MONITOR]() === monitor &&
      !window.state[WINDOW_STATE_MINIMIZED]() &&
      !window.state[WINDOW_STATE_MAXIMIZED]()
    );
  }

  private floatingSnapLayoutFromWindows(
    base: ManagedWindowRect,
    windows: WaylandWindow[],
  ): FloatingSnapLayout {
    const bx = read(base.x);
    const by = read(base.y);
    const bw = read(base.width);
    const bh = read(base.height);
    const defaultSplitX = bx + (bw - SNAP_GAP_PX) / 2;
    const defaultSplitY = by + (bh - SNAP_GAP_PX) / 2;
    const splitXSamples: number[] = [];
    const leftSplitYSamples: number[] = [];
    const rightSplitYSamples: number[] = [];

    for (const window of windows) {
      const zone = window.state[WINDOW_STATE_SNAP_ZONE]();
      if (!isLayoutSnapZone(zone)) {
        continue;
      }
      const rect = window.state[WINDOW_STATE_RECT]();
      if (isLeftSnapZone(zone)) {
        splitXSamples.push(read(rect.x) + read(rect.width));
      } else {
        splitXSamples.push(read(rect.x) - SNAP_GAP_PX);
      }

      if (isTopSnapZone(zone)) {
        const samples =
          snapColumn(zone) === "left" ? leftSplitYSamples : rightSplitYSamples;
        samples.push(read(rect.y) + read(rect.height));
      } else if (isBottomSnapZone(zone)) {
        const samples =
          snapColumn(zone) === "left" ? leftSplitYSamples : rightSplitYSamples;
        samples.push(read(rect.y) - SNAP_GAP_PX);
      }
    }

    return {
      splitX: averageOr(splitXSamples, defaultSplitX),
      leftSplitY: averageOr(leftSplitYSamples, defaultSplitY),
      rightSplitY: averageOr(rightSplitYSamples, defaultSplitY),
    };
  }

  private clampFloatingSnapLayout(
    base: ManagedWindowRect,
    layout: FloatingSnapLayout,
    windows: WaylandWindow[],
  ): void {
    const bx = read(base.x);
    const by = read(base.y);
    const bw = read(base.width);
    const bh = read(base.height);
    const groups = this.floatingSnapMinSizeGroups(windows);

    layout.splitX = clamp(
      layout.splitX,
      bx + groups.leftWidth,
      bx + bw - SNAP_GAP_PX - groups.rightWidth,
    );
    layout.leftSplitY = clamp(
      layout.leftSplitY,
      by + groups.leftTopHeight,
      by + bh - SNAP_GAP_PX - groups.leftBottomHeight,
    );
    layout.rightSplitY = clamp(
      layout.rightSplitY,
      by + groups.rightTopHeight,
      by + bh - SNAP_GAP_PX - groups.rightBottomHeight,
    );
  }

  private floatingSnapMinSizeGroups(windows: WaylandWindow[]): {
    leftWidth: number;
    rightWidth: number;
    leftTopHeight: number;
    leftBottomHeight: number;
    rightTopHeight: number;
    rightBottomHeight: number;
  } {
    const groups = {
      leftWidth: 1,
      rightWidth: 1,
      leftTopHeight: 1,
      leftBottomHeight: 1,
      rightTopHeight: 1,
      rightBottomHeight: 1,
    };

    for (const window of windows) {
      const zone = window.state[WINDOW_STATE_SNAP_ZONE]();
      if (!isLayoutSnapZone(zone)) {
        continue;
      }

      const minSize = this.floatingSnapMinSize(window);
      if (isLeftSnapZone(zone)) {
        groups.leftWidth = Math.max(groups.leftWidth, minSize.width);
      } else {
        groups.rightWidth = Math.max(groups.rightWidth, minSize.width);
      }

      if (isTopSnapZone(zone)) {
        if (snapColumn(zone) === "left") {
          groups.leftTopHeight = Math.max(groups.leftTopHeight, minSize.height);
        } else {
          groups.rightTopHeight = Math.max(
            groups.rightTopHeight,
            minSize.height,
          );
        }
      } else if (isBottomSnapZone(zone)) {
        if (snapColumn(zone) === "left") {
          groups.leftBottomHeight = Math.max(
            groups.leftBottomHeight,
            minSize.height,
          );
        } else {
          groups.rightBottomHeight = Math.max(
            groups.rightBottomHeight,
            minSize.height,
          );
        }
      }
    }

    return groups;
  }

  private floatingSnapMinSize(window: WaylandWindow): {
    width: number;
    height: number;
  } {
    const constraints = window.sizeConstraints();
    const extra = this.clientToRootSizeExtra(window);
    return {
      width: Math.max(1, constraints.min?.width ?? 1) + extra.width,
      height: Math.max(1, constraints.min?.height ?? 1) + extra.height,
    };
  }

  private applyFloatingSnapLayout(
    base: ManagedWindowRect,
    layout: FloatingSnapLayout,
    windows: WaylandWindow[],
  ): void {
    for (const window of windows) {
      const zone = window.state[WINDOW_STATE_SNAP_ZONE]();
      if (!isLayoutSnapZone(zone)) {
        continue;
      }

      const rect = this.floatingSnapRectForZone(base, layout, zone);
      stopRectAnimation(window, WINDOW_STATE_RECT);
      window.state[WINDOW_STATE_RECT].set(rect);
    }
  }

  private floatingSnapRectForZone(
    base: ManagedWindowRect,
    layout: FloatingSnapLayout,
    zone: LayoutSnapZone,
  ): ManagedWindowRect {
    const bx = read(base.x);
    const by = read(base.y);
    const bw = read(base.width);
    const bh = read(base.height);
    const rightX = layout.splitX + SNAP_GAP_PX;
    const leftWidth = Math.max(1, layout.splitX - bx);
    const rightWidth = Math.max(1, bx + bw - rightX);

    switch (zone) {
      case "left":
        return { x: bx, y: by, width: leftWidth, height: bh };
      case "right":
        return { x: rightX, y: by, width: rightWidth, height: bh };
      case "top-left":
        return {
          x: bx,
          y: by,
          width: leftWidth,
          height: Math.max(1, layout.leftSplitY - by),
        };
      case "bottom-left": {
        const y = layout.leftSplitY + SNAP_GAP_PX;
        return {
          x: bx,
          y,
          width: leftWidth,
          height: Math.max(1, by + bh - y),
        };
      }
      case "top-right":
        return {
          x: rightX,
          y: by,
          width: rightWidth,
          height: Math.max(1, layout.rightSplitY - by),
        };
      case "bottom-right": {
        const y = layout.rightSplitY + SNAP_GAP_PX;
        return {
          x: rightX,
          y,
          width: rightWidth,
          height: Math.max(1, by + bh - y),
        };
      }
    }
  }

  private setWindowSnapState(
    workspace: Workspace | undefined,
    window: WaylandWindow,
    monitor: string,
    zone: LayoutSnapZone,
  ): void {
    if (workspace) {
      for (const other of workspace.listWindows()) {
        if (other.id === window.id) {
          continue;
        }
        if (
          other.state[WINDOW_STATE_SNAP_MONITOR]() === monitor &&
          snapZonesConflict(other.state[WINDOW_STATE_SNAP_ZONE](), zone)
        ) {
          this.clearWindowSnapState(other);
        }
      }
    }

    window.state[WINDOW_STATE_SNAP_ZONE].set(zone);
    window.state[WINDOW_STATE_SNAP_MONITOR].set(monitor);
  }

  private clearWindowSnapState(window: WaylandWindow): void {
    window.state[WINDOW_STATE_SNAP_ZONE].set(null);
    window.state[WINDOW_STATE_SNAP_MONITOR].set(null);
  }

  /** Broadcast a preview rect (converted to monitor-local) or a hide (null). */
  private emitSnapPreview(
    monitor: string,
    rect: ManagedWindowRect | null,
    kind: "floating" | "tiling",
  ) {
    if (!this.snapPreviewBroadcaster) {
      return;
    }
    if (!rect) {
      this.snapPreviewBroadcaster({ monitor, rect: null, kind });
      return;
    }
    const output = COMPOSITOR.output.current[monitor];
    const ox = output?.position.x ?? 0;
    const oy = output?.position.y ?? 0;
    this.snapPreviewBroadcaster({
      monitor,
      kind,
      rect: {
        x: read(rect.x) - ox,
        y: read(rect.y) - oy,
        width: read(rect.width),
        height: read(rect.height),
      },
    });
  }

  /** Update the floating-drag snap candidate + preview during a move. */
  private updateFloatingDragSnap(event: WindowMoveEvent) {
    if (event.modifiers.shift) {
      this.clearFloatingSnapPreview();
      return;
    }
    if (event.phase === "start") {
      this.clearFloatingSnapPreview();
      return;
    }
    if (event.phase === "end" || event.phase === "cancel") {
      return;
    }

    const monitor =
      event.outputName &&
      COMPOSITOR.output.list.includes(event.outputName)
        ? event.outputName
        : this.currentMonitor;
    const zone = monitor
      ? this.floatingSnapZoneAt(
          monitor,
          event.currentPointer.x,
          event.currentPointer.y,
        )
      : null;

    if (!monitor || !zone) {
      this.clearFloatingSnapPreview();
      return;
    }

    const rect = this.snapZoneRect(monitor, zone);
    if (!rect) {
      this.clearFloatingSnapPreview();
      return;
    }
    if (
      this.floatingSnap &&
      (this.floatingSnap.windowId !== event.window.id ||
        this.floatingSnap.monitor !== monitor)
    ) {
      this.emitSnapPreview(this.floatingSnap.monitor, null, "floating");
    }
    this.floatingSnap = { windowId: event.window.id, monitor, zone, rect };
    this.emitSnapPreview(monitor, rect, "floating");
  }

  private clearFloatingSnapPreview() {
    if (!this.floatingSnap) {
      return;
    }
    this.emitSnapPreview(this.floatingSnap.monitor, null, "floating");
    this.floatingSnap = null;
  }

  /**
   * Apply the pending snap on drop (or clear it on cancel). Returns true if the
   * window was snapped, so the caller skips leaving it at the drop position.
   */
  private finishFloatingDragSnap(
    event: WindowMoveEvent,
    workspace: Workspace | undefined,
  ): boolean {
    if (event.modifiers.shift) {
      this.clearFloatingSnapPreview();
      return false;
    }
    const snap = this.floatingSnap;
    this.floatingSnap = null;
    if (!snap || snap.windowId !== event.window.id) {
      if (snap) {
        this.emitSnapPreview(snap.monitor, null, "floating");
      }
      return false;
    }

    this.emitSnapPreview(snap.monitor, null, "floating");
    if (event.phase !== "end") {
      return false;
    }

    const window = event.window;
    const isMaximized = window.state[WINDOW_STATE_MAXIMIZED]();

    if (snap.zone === "maximize") {
      this.clearWindowSnapState(window);
      // Route through the real maximize so the compositor `isMaximized` state
      // (and therefore the SSD maximize/restore icon) stays in sync. Calling
      // maximize() fires onWindowMaximizeRequest, which applies the rect.
      if (!isMaximized) {
        window.maximize();
      } else {
        // Already maximized (e.g. re-dropped on the top edge): just re-apply
        // the maximized rect for the monitor under the cursor.
        const rect = this.maximizedRectForWindow(window);
        playRectAnimation(
          window,
          WINDOW_STATE_RECT,
          rect,
          WINDOW_MANAGEMENT_EASING,
          WINDOW_MANAGEMENT_ANIMATION_DURATION,
        );
        workspace?.syncFloatingWindowRect(window, rect);
      }
    } else {
      // Half / quarter: ensure the window is unmaximized first (syncs the SSD
      // icon), with the restore rect cleared so unmaximize() does not animate
      // back to it and fight the snap, then place it at the zone rect.
      if (isMaximized) {
        window.state[WINDOW_STATE_RESTORE_RECT].set(null);
        window.state[WINDOW_STATE_MAXIMIZED].set(false);
        window.unmaximize();
      }
      playRectAnimation(
        window,
        WINDOW_STATE_RECT,
        snap.rect,
        WINDOW_MANAGEMENT_EASING,
        WINDOW_MANAGEMENT_ANIMATION_DURATION,
      );
      this.setWindowSnapState(workspace, window, snap.monitor, snap.zone);
      workspace?.syncFloatingWindowRect(window, snap.rect);
    }
    this.applyWorkspaceStackPolicy(workspace);
    return true;
  }
}

export class Workspace {
  public index: number;
  private readonly windows: WaylandWindow[] = [];
  private readonly naturalRootRect: (
    window: WaylandWindow,
  ) => ManagedWindowRect;
  private readonly maximizedRootRect: (
    window: WaylandWindow,
  ) => ManagedWindowRect;
  private readonly activeWorkspaceIndex: (monitor: string) => number;
  private readonly tileWidthByWindowId = new Map<string, number>();
  private readonly restoredWindowStateById = new Map<
    string,
    WorkspaceWindowSnapshot
  >();
  private activeWindowId: string | null = null;
  private visibilityAnimationToken = 0;
  private draggingWindowId: string | null = null;
  // Layout slot reserved for the tile being dragged (the gap opened in the row).
  // Captured during applyLayout so the bar can preview where the tile will land.
  private lastDraggingSlotRect: ManagedWindowRect | null = null;
  private lastAppliedTileViewportRect: ManagedWindowRect | null = null;
  private scrollOffset = 0;
  private kineticScrollPoll: PollHandle | null = null;
  private kineticScrollToken = 0;
  // Current tiling layout. Persists across tile on/off toggles (setTiled only
  // flips isTiled, never this field) and across hot-reload (see snapshot/restore).
  private tileLayout: TileLayout = "scrolling";
  public monitor: string;
  public isTiled = false;

  public constructor(
    index: number,
    monitor: string,
    naturalRootRect: (window: WaylandWindow) => ManagedWindowRect,
    maximizedRootRect: (window: WaylandWindow) => ManagedWindowRect,
    activeWorkspaceIndex: (monitor: string) => number,
  ) {
    this.index = index;
    this.monitor = monitor;
    this.naturalRootRect = naturalRootRect;
    this.maximizedRootRect = maximizedRootRect;
    this.activeWorkspaceIndex = activeWorkspaceIndex;
  }

  public moveToMonitor(monitor: string, index: number) {
    this.monitor = monitor;
    this.index = index;
    for (const window of this.windows) {
      this.syncWindowVisibleOutputs(window);
      if (window.state[WINDOW_STATE_FULLSCREEN]()) {
        window.state[WINDOW_STATE_RECT].set(this.fullscreenRootRect(window));
        continue;
      }
      if (window.state[WINDOW_STATE_MAXIMIZED]()) {
        window.state[WINDOW_STATE_RECT].set(this.maximizedRootRect(window));
        continue;
      }

      if (!this.isTiled || !this.shouldTile(window)) {
        const rect = this.clampRectToViewport(
          window.state[WINDOW_STATE_RECT](),
        );
        window.state[WINDOW_STATE_RECT].set(rect);
        window.state[WINDOW_STATE_FLOATING_RECT].set(
          this.isTiled ? this.viewportRectToFloatingContentRect(rect) : null,
        );
      }
    }
  }

  public addWindow(window: WaylandWindow): boolean {
    if (this.windows.map((window) => window.id).includes(window.id)) {
      hotReloadDebug("workspace-add-existing-skip", {
        monitor: this.monitor,
        index: this.index,
        windowId: window.id,
        windowIds: this.windows.map((window) => window.id),
      });
      return false;
    }
    this.windows.push(window);
    const restored = this.restoredWindowStateById.get(window.id);
    const isTileableInCurrentMode = !this.isTiled || this.shouldTile(window);
    if (!restored && isTileableInCurrentMode) {
      this.activeWindowId = window.id;
    }
    if (restored) {
      window.cancelAnimation();
      hotReloadDebug("workspace-add-restored-cancel-animation", {
        monitor: this.monitor,
        index: this.index,
        windowId: window.id,
        activeWindowId: this.activeWindowId,
        restoredWindowIds: Array.from(this.restoredWindowStateById.keys()),
        windowIds: this.windows.map((window) => window.id),
      });
      this.restoredWindowStateById.delete(window.id);
      window.state[WINDOW_STATE_FLOATING_RECT].set(
        restored.floatingRect ?? null,
      );
      window.state[WINDOW_STATE_RESTORE_RECT].set(restored.restoreRect ?? null);
      window.state[WINDOW_STATE_SNAP_ZONE].set(restored.snapZone ?? null);
      window.state[WINDOW_STATE_SNAP_MONITOR].set(
        restored.snapMonitor ?? null,
      );
      window.state[WINDOW_STATE_MINIMIZED].set(restored.minimized);
      window.state[WINDOW_STATE_MINIMIZE_VISUAL_IDLE].set(restored.minimized);
      window.state[WINDOW_STATE_MAXIMIZED].set(restored.maximized);
      if (restored.tileWidth !== undefined) {
        this.tileWidthByWindowId.set(window.id, restored.tileWidth);
      }
    }
    const visible = this.isActive();
    window.state[WINDOW_STATE_WORKSPACE_VISIBLE].set(visible);
    window.state[WINDOW_STATE_WORKSPACE_OFFSET_Y].set(0);
    window.state[WINDOW_STATE_WORKSPACE_OPACITY].set(visible ? 1 : 0);
    this.syncWindowVisibleOutputs(window);

    if (!COMPOSITOR.output.list.includes(this.monitor)) {
      return restored !== undefined;
    }

    if (restored?.floatingRect && !this.isTiled) {
      window.state[WINDOW_STATE_RECT].set(restored.floatingRect);
    } else if (this.isTiled && this.shouldTile(window)) {
      const initialRect = this.centeredFloatingRect(window);
      window.state[WINDOW_STATE_FLOATING_RECT].set(
        restored?.floatingRect ?? initialRect,
      );
      if (restored?.tileWidth === undefined) {
        this.setTileWidthFromRect(window, initialRect, true);
        this.scrollToWindow(window);
      }
      this.applyLayout({
        suppressSSDRebuild: false,
        animate: restored === undefined,
        preserveMissingActive: restored !== undefined,
      });
    } else if (this.isTiled) {
      const initialRect = this.centeredFloatingRect(window);
      const contentRect =
        restored?.floatingRect ??
        this.viewportRectToFloatingContentRect(initialRect);
      window.state[WINDOW_STATE_FLOATING_RECT].set(contentRect);
      window.state[WINDOW_STATE_RECT].set(
        this.floatingContentRectToViewportRect(contentRect),
      );
    } else {
      window.state[WINDOW_STATE_RECT].set(this.centeredFloatingRect(window));
    }
    hotReloadDebug("workspace-add-window", {
      monitor: this.monitor,
      index: this.index,
      windowId: window.id,
      restored: restored !== undefined,
      isTiledWorkspace: this.isTiled,
      shouldTile: this.shouldTile(window),
      activeWindowId: this.activeWindowId,
      windowIds: this.windows.map((window) => window.id),
      rect: window.state[WINDOW_STATE_RECT](),
      floatingRect: window.state[WINDOW_STATE_FLOATING_RECT](),
    });
    return restored !== undefined;
  }

  public removeWindow(window: WaylandWindow): WaylandWindow | null | undefined {
    const index = this.windows.findIndex((current) => current.id === window.id);
    if (index >= 0) {
      this.windows.splice(index, 1);
      this.tileWidthByWindowId.delete(window.id);
      if (this.draggingWindowId === window.id) {
        this.draggingWindowId = null;
        window.state[WINDOW_STATE_TILE_DRAGGING].set(false);
      }
      let nextFocus: WaylandWindow | null = null;
      if (this.activeWindowId === window.id) {
        nextFocus =
          this.tileableWindows()[
            Math.min(index, this.tileableWindows().length - 1)
          ] ?? null;
        this.activeWindowId = nextFocus?.id ?? null;
      }
      return nextFocus;
    }
    return undefined;
  }

  public removeTileDragWindow(window: WaylandWindow) {
    const index = this.windows.findIndex((current) => current.id === window.id);
    if (index < 0) {
      return;
    }
    this.windows.splice(index, 1);
    this.draggingWindowId = null;
  }

  public removeFloatingWindow(window: WaylandWindow) {
    const index = this.windows.findIndex((current) => current.id === window.id);
    if (index < 0) {
      return;
    }
    this.windows.splice(index, 1);
    if (this.activeWindowId === window.id) {
      this.activeWindowId =
        this.activeWindow(this.tileableWindows())?.id ?? null;
    }
  }

  public hasWindow(window: WaylandWindow): boolean {
    return this.windows.some((current) => current.id === window.id);
  }

  public windowCount(): number {
    return this.windows.length;
  }

  /**
   * Snapshot of every window currently in this workspace. The returned array
   * is a copy; mutating it is safe and won't affect the workspace state.
   */
  public listWindows(): WaylandWindow[] {
    return this.windows.slice();
  }

  public findWindowById(windowId: string): WaylandWindow | undefined {
    return this.windows.find((window) => window.id === windowId);
  }

  public isActiveWindowId(windowId: string): boolean {
    return this.activeWindowId === windowId;
  }

  public moveFocusedTile(direction: -1 | 1): boolean {
    if (!this.isTiled) {
      return false;
    }

    const focused = this.focusedWindow();
    if (!focused || !this.shouldTile(focused)) {
      return false;
    }

    const tileable = this.tileableWindows();
    const currentIndex = tileable.findIndex(
      (window) => window.id === focused.id,
    );
    if (currentIndex < 0) {
      return false;
    }

    const nextIndex = currentIndex + direction;
    if (nextIndex < 0 || nextIndex >= tileable.length) {
      return false;
    }

    this.stopKineticScroll();
    this.activeWindowId = focused.id;
    this.moveTileWindowToIndex(focused, nextIndex);
    this.scrollToWindow(focused);
    this.applyLayout();
    focused.focus();
    return true;
  }

  public takeWindowForMove(
    window: WaylandWindow,
  ): { window: WaylandWindow; snapshot: WorkspaceWindowSnapshot } | null {
    if (!this.hasWindow(window)) {
      return null;
    }

    const snapshot = this.snapshotWindow(window);
    this.removeWindow(window);
    return { window, snapshot };
  }

  public addMovedWindow(
    window: WaylandWindow,
    snapshot: WorkspaceWindowSnapshot,
  ): boolean {
    this.restoredWindowStateById.set(window.id, snapshot);
    return this.addWindow(window);
  }

  public isRestoringWindow(windowId: string): boolean {
    return this.restoredWindowStateById.has(windowId);
  }

  public isActive(): boolean {
    return this.activeWorkspaceIndex(this.monitor) === this.index;
  }

  public refreshUsableAreaLayout() {
    if (!COMPOSITOR.output.list.includes(this.monitor)) {
      return;
    }

    if (this.isTiled) {
      const nextViewportRect = this.tileViewportRect();
      if (
        this.lastAppliedTileViewportRect &&
        managedRectEquals(this.lastAppliedTileViewportRect, nextViewportRect)
      ) {
        return;
      }
      this.applyLayout({
        suppressSSDRebuild: false,
        animate: false,
        preserveMissingActive: true,
      });
      return;
    }

    for (const window of this.windows) {
      if (!window.state[WINDOW_STATE_MAXIMIZED]()) {
        continue;
      }
      stopRectAnimation(window, WINDOW_STATE_RECT);
      window.state[WINDOW_STATE_RECT].set(this.maximizedRootRect(window));
    }
  }

  public setVisible(visible: boolean) {
    this.visibilityAnimationToken += 1;
    for (const window of this.windows) {
      this.syncWindowVisibleOutputs(window);
      if (window.state[WINDOW_STATE_TILE_DRAGGING]()) {
        window.state[WINDOW_STATE_WORKSPACE_VISIBLE].set(true);
        window.state[WINDOW_STATE_WORKSPACE_OFFSET_Y].set(0);
        window.state[WINDOW_STATE_WORKSPACE_OPACITY].set(1);
        continue;
      }
      window.state[WINDOW_STATE_WORKSPACE_VISIBLE].set(visible);
      window.state[WINDOW_STATE_WORKSPACE_OFFSET_Y].set(0);
      window.state[WINDOW_STATE_WORKSPACE_OPACITY].set(visible ? 1 : 0);
    }
  }

  public prepareWorkspaceTransition(offsetY: number, opacity: number) {
    this.visibilityAnimationToken += 1;
    for (const window of this.windows) {
      this.syncWindowVisibleOutputs(window);
      if (window.state[WINDOW_STATE_TILE_DRAGGING]()) {
        window.state[WINDOW_STATE_WORKSPACE_VISIBLE].set(true);
        window.state[WINDOW_STATE_WORKSPACE_OFFSET_Y].set(0);
        window.state[WINDOW_STATE_WORKSPACE_OPACITY].set(1);
        continue;
      }
      window.state[WINDOW_STATE_WORKSPACE_VISIBLE].set(true);
      window.state[WINDOW_STATE_WORKSPACE_OFFSET_Y].set(offsetY);
      window.state[WINDOW_STATE_WORKSPACE_OPACITY].set(opacity);
    }
  }

  public setWorkspaceGestureVisual(offsetY: number, opacity: number) {
    this.visibilityAnimationToken += 1;
    for (const window of this.windows) {
      this.syncWindowVisibleOutputs(window);
      cancelWorkspaceVisualAnimation(window);
      if (window.state[WINDOW_STATE_TILE_DRAGGING]()) {
        window.state[WINDOW_STATE_WORKSPACE_VISIBLE].set(true);
        window.state[WINDOW_STATE_WORKSPACE_OFFSET_Y].set(0);
        window.state[WINDOW_STATE_WORKSPACE_OPACITY].set(1);
        continue;
      }
      window.state[WINDOW_STATE_WORKSPACE_VISIBLE].set(true);
      window.state[WINDOW_STATE_WORKSPACE_OFFSET_Y].set(offsetY);
      window.state[WINDOW_STATE_WORKSPACE_OPACITY].set(opacity);
    }
  }

  public animateWorkspaceTransition(options: {
    fromOffsetY: number;
    toOffsetY: number;
    fromOpacity: number;
    toOpacity: number;
    visibleAfter: boolean;
  }) {
    const token = this.visibilityAnimationToken + 1;
    this.visibilityAnimationToken = token;

    for (const window of this.windows) {
      this.syncWindowVisibleOutputs(window);
      if (window.state[WINDOW_STATE_TILE_DRAGGING]()) {
        window.state[WINDOW_STATE_WORKSPACE_VISIBLE].set(true);
        window.state[WINDOW_STATE_WORKSPACE_OFFSET_Y].set(0);
        window.state[WINDOW_STATE_WORKSPACE_OPACITY].set(1);
        continue;
      }
      // Same ordering rule as prepare: schedule first, then flip
      // VISIBLE. For from-workspace this is mostly a no-op (VISIBLE was
      // already true), but for to-workspace's second call this keeps
      // the same invariant in case prepareWorkspaceTransition's hold
      // animation has already completed (e.g., rapid switches).
      scheduleWorkspaceVisualAnimation(
        window,
        options.fromOffsetY,
        options.toOffsetY,
        options.fromOpacity,
        options.toOpacity,
        WINDOW_MANAGEMENT_EASING,
        WORKSPACE_SWITCH_ANIMATION_DURATION,
      );
      window.state[WINDOW_STATE_WORKSPACE_VISIBLE].set(true);
    }

    const VISIBILITY_COMMIT_BEFORE_END_MS = 32;

    setTimeout(
      () => {
        if (this.visibilityAnimationToken !== token) {
          return;
        }
        withManagedWindowOnlySSDRebuildSuppressed(() => {
          this.setVisible(options.visibleAfter);
        });
      },
      Math.max(
        0,
        WORKSPACE_SWITCH_ANIMATION_DURATION - VISIBILITY_COMMIT_BEFORE_END_MS,
      ),
    );
  }

  public setTiled(tiled: boolean) {
    if (this.isTiled === tiled) {
      return;
    }

    this.stopKineticScroll();

    const focusedWindow = this.focusedWindow();
    const focusedTileableWindow =
      focusedWindow &&
      this.shouldTile(focusedWindow) &&
      !focusedWindow.state[WINDOW_STATE_MINIMIZED]()
        ? focusedWindow
        : undefined;
    this.isTiled = tiled;
    if (tiled) {
      this.scrollOffset = 0;
      for (const window of this.windows) {
        this.syncWindowVisibleOutputs(window);
      }
      for (const window of this.tileableWindows()) {
        this.captureFloatingRect(window);
        this.setTileWidthFromRect(
          window,
          window.state[WINDOW_STATE_FLOATING_RECT]() ??
            window.state[WINDOW_STATE_RECT](),
          true,
        );
      }
      for (const window of this.floatingWindows()) {
        this.captureFloatingRect(window);
      }
      const tileable = this.tileableWindows();
      const previousActiveWindow = this.activeWindow(tileable);
      this.activeWindowId =
        (focusedTileableWindow ?? previousActiveWindow ?? tileable.at(0))?.id ??
        null;
      if (focusedTileableWindow) {
        this.scrollToWindow(focusedTileableWindow);
      }
      this.applyLayout();
      focusedTileableWindow?.focus();
      return;
    }

    for (const window of this.windows) {
      // A window that is still maximized across the mode switch keeps its
      // maximized rect. Restoring FLOATING_RECT here would configure the
      // client to the rect captured at first commit (often degenerate),
      // collapsing it to its minimum size. (Chrome and friends remember
      // their previous session state and start maximized, which makes this
      // easy to hit.)
      if (window.state[WINDOW_STATE_MAXIMIZED]()) {
        playRectAnimation(
          window,
          WINDOW_STATE_RECT,
          this.maximizedRootRect(window),
          WINDOW_MANAGEMENT_EASING,
          WINDOW_MANAGEMENT_ANIMATION_DURATION,
          MANAGED_WINDOW_ONLY_ANIMATION,
        );
        window.state[WINDOW_STATE_FLOATING_RECT].set(null);
        this.syncWindowVisibleOutputs(window);
        continue;
      }
      const rect = window.state[WINDOW_STATE_FLOATING_RECT]();
      if (rect) {
        const viewportRect = this.shouldTile(window)
          ? rect
          : this.floatingContentRectToViewportRect(rect);
        playRectAnimation(
          window,
          WINDOW_STATE_RECT,
          viewportRect,
          WINDOW_MANAGEMENT_EASING,
          WINDOW_MANAGEMENT_ANIMATION_DURATION,
          MANAGED_WINDOW_ONLY_ANIMATION,
        );
      }
      window.state[WINDOW_STATE_FLOATING_RECT].set(null);
      this.syncWindowVisibleOutputs(window);
    }
    if (focusedTileableWindow) {
      this.activeWindowId = focusedTileableWindow.id;
      focusedTileableWindow.focus();
    }
  }

  public applyLayout(options: LayoutOptions = {}) {
    if (!this.isTiled) {
      return;
    }
    switch (this.tileLayout) {
      case "dwindle":
        this.applyDwindleLayout(options);
        return;
      case "master":
        this.applyMasterLayout(options);
        return;
      default:
        this.applyScrollingLayout(options);
        return;
    }
  }

  public getTileLayout(): TileLayout {
    return this.tileLayout;
  }

  public setTileLayout(layout: TileLayout) {
    if (this.tileLayout === layout) {
      return;
    }
    this.tileLayout = layout;
    if (this.isTiled) {
      this.applyLayout();
    }
  }

  /**
   * Shared layout setup: resolves animation options, handles the empty-tiles
   * short-circuit, and selects a fallback active window when the current one is
   * no longer present. Returns null when the layout should bail out early (no
   * tileable windows); otherwise the resolved context the chosen layout method
   * uses to place tiles.
   */
  private resolveLayoutContext(options: LayoutOptions): {
    tileable: WaylandWindow[];
    animate: boolean;
    suppressSSDRebuild: boolean;
    canSuppress: boolean;
    animationOptions: RectAnimationOptions | undefined;
  } | null {
    const tileable = this.tileableWindows();
    const animate = options.animate ?? true;
    const suppressSSDRebuild = options.suppressSSDRebuild ?? true;
    const canSuppress = this.canSuppressLayoutSSDRebuild(tileable);
    const animationOptions =
      animate && suppressSSDRebuild && canSuppress
        ? MANAGED_WINDOW_ONLY_ANIMATION
        : undefined;

    if (tileable.length === 0) {
      this.activeWindowId = null;
      hotReloadDebug("workspace-apply-layout-empty", {
        monitor: this.monitor,
        index: this.index,
        animate,
        suppressSSDRebuild,
        canSuppress,
        floatingWindowIds: this.floatingWindows().map((window) => window.id),
      });
      this.applyFloatingLayout(animationOptions, animate);
      return null;
    }

    if (
      !this.activeWindowId ||
      !tileable.some((window) => window.id === this.activeWindowId)
    ) {
      if (!options.preserveMissingActive) {
        this.activeWindowId = tileable.at(-1)?.id ?? null;
      }
    }

    return { tileable, animate, suppressSSDRebuild, canSuppress, animationOptions };
  }

  /**
   * Place a single tile, honoring the drag-slot reservation (the dragged window
   * keeps its live pointer rect; we just record the reserved slot for preview)
   * and the animate/set toggle. `appliedRects` records every window's target rect
   * for the hot-reload debug log, including the dragged one.
   */
  private applyTileRect(
    window: WaylandWindow,
    rect: ManagedWindowRect,
    appliedRects: Record<string, ManagedWindowRect>,
    animate: boolean,
    animationOptions: RectAnimationOptions | undefined,
    cancelRectAnimations: boolean,
  ): void {
    appliedRects[window.id] = rect;
    if (window.id === this.draggingWindowId) {
      this.lastDraggingSlotRect = rect;
      return;
    }
    if (animate) {
      playRectAnimation(
        window,
        WINDOW_STATE_RECT,
        rect,
        WINDOW_MANAGEMENT_EASING,
        TILE_ANIMATION_DURATION,
        animationOptions,
      );
    } else {
      if (cancelRectAnimations) {
        stopRectAnimation(window, WINDOW_STATE_RECT);
      }
      window.state[WINDOW_STATE_RECT].set(rect);
    }
  }

  /** Original horizontal scrolling row layout — the default. */
  private applyScrollingLayout(options: LayoutOptions = {}) {
    const context = this.resolveLayoutContext(options);
    if (!context) {
      return;
    }
    const { tileable, animate, suppressSSDRebuild, canSuppress, animationOptions } =
      context;

    this.clampScrollOffset(tileable.length);

    const viewportRect = this.tileViewportRect();
    this.lastAppliedTileViewportRect = snapshotManagedRect(viewportRect);
    const tileHeight = read(viewportRect.height);
    let nextX = read(viewportRect.x) - this.scrollOffset;
    const appliedRects: Record<string, ManagedWindowRect> = {};
    this.lastDraggingSlotRect = null;

    tileable.forEach((window, index) => {
      const tileWidth = this.tileWidthForWindow(window, viewportRect);
      const rect = window.state[WINDOW_STATE_FULLSCREEN]()
        ? this.fullscreenRootRect(window)
        : window.state[WINDOW_STATE_MAXIMIZED]()
          ? this.maximizedTileRect(window, nextX)
          : {
              x: nextX,
              y: read(viewportRect.y),
              width: tileWidth,
              height: tileHeight,
            };
      this.applyTileRect(
        window,
        rect,
        appliedRects,
        animate,
        animationOptions,
        options.cancelRectAnimations !== false,
      );
      nextX += tileWidth + (index === tileable.length - 1 ? 0 : TILE_GAP);
    });

    hotReloadDebug("workspace-apply-layout", {
      monitor: this.monitor,
      index: this.index,
      layout: "scrolling",
      animate,
      suppressSSDRebuild,
      canSuppress,
      activeWindowId: this.activeWindowId,
      scrollOffset: this.scrollOffset,
      tileableWindowIds: tileable.map((window) => window.id),
      floatingWindowIds: this.floatingWindows().map((window) => window.id),
      appliedRects,
    });
    this.applyFloatingLayout(animationOptions, animate);
  }

  /**
   * Hyprland-style dwindle: a binary tree of tiles. Each level halves its
   * assigned rect, alternating the split axis by depth (depth 0 = horizontal
   * left/right split, depth 1 = vertical top/bottom, depth 2 = horizontal, …)
   * until a subtree holds a single window. The window list is split into two
   * halves so the first half fills the first child and the rest the second —
   * deterministic for a given window order, so focus cycling (which reorders
   * tileableWindows) visibly reshuffles the tree.
   */
  private applyDwindleLayout(options: LayoutOptions = {}) {
    const context = this.resolveLayoutContext(options);
    if (!context) {
      return;
    }
    const { tileable, animate, suppressSSDRebuild, canSuppress, animationOptions } =
      context;

    const viewportRect = this.tileViewportRect();
    const appliedRects: Record<string, ManagedWindowRect> = {};
    this.lastDraggingSlotRect = null;

    interface DwindleRect {
      x: number;
      y: number;
      width: number;
      height: number;
    }
    const build = (windows: WaylandWindow[], rect: DwindleRect, depth: number) => {
      if (windows.length === 0) {
        return;
      }
      if (windows.length === 1) {
        const window = windows[0];
        const rectToApply: ManagedWindowRect = window.state[
          WINDOW_STATE_MAXIMIZED
        ]()
          ? this.maximizedTileRect(window, rect.x)
          : { x: rect.x, y: rect.y, width: rect.width, height: rect.height };
        this.applyTileRect(
          window,
          rectToApply,
          appliedRects,
          animate,
          animationOptions,
          options.cancelRectAnimations !== false,
        );
        return;
      }

      // Bias the split toward the first half so a single new window always
      // lands in its own child rather than disturbing an already-balanced pair.
      const splitIndex = Math.ceil(windows.length / 2);
      const left = windows.slice(0, splitIndex);
      const right = windows.slice(splitIndex);
      const horizontal = depth % 2 === 0;
      const gap = TILE_GAP;
      if (horizontal) {
        const half = (rect.width - gap) / 2;
        build(left, { x: rect.x, y: rect.y, width: half, height: rect.height }, depth + 1);
        build(
          right,
          { x: rect.x + half + gap, y: rect.y, width: half, height: rect.height },
          depth + 1,
        );
      } else {
        const half = (rect.height - gap) / 2;
        build(
          left,
          { x: rect.x, y: rect.y, width: rect.width, height: half },
          depth + 1,
        );
        build(
          right,
          {
            x: rect.x,
            y: rect.y + half + gap,
            width: rect.width,
            height: half,
          },
          depth + 1,
        );
      }
    };

    build(
      tileable,
      {
        x: read(viewportRect.x),
        y: read(viewportRect.y),
        width: read(viewportRect.width),
        height: read(viewportRect.height),
      },
      0,
    );

    hotReloadDebug("workspace-apply-layout", {
      monitor: this.monitor,
      index: this.index,
      layout: "dwindle",
      animate,
      suppressSSDRebuild,
      canSuppress,
      activeWindowId: this.activeWindowId,
      tileableWindowIds: tileable.map((window) => window.id),
      floatingWindowIds: this.floatingWindows().map((window) => window.id),
      appliedRects,
    });
    this.applyFloatingLayout(animationOptions, animate);
  }

  /**
   * Hyprland-style master: one master window on the left half, the remaining
   * windows ("slaves") stacked vertically in the right half. With a single
   * window it fills the whole area. The master is the first tileable window;
   * focus cycling (which reorders tileableWindows) therefore rotates which
   * window holds the master slot.
   */
  private applyMasterLayout(options: LayoutOptions = {}) {
    const context = this.resolveLayoutContext(options);
    if (!context) {
      return;
    }
    const { tileable, animate, suppressSSDRebuild, canSuppress, animationOptions } =
      context;

    const viewportRect = this.tileViewportRect();
    const x = read(viewportRect.x);
    const y = read(viewportRect.y);
    const width = read(viewportRect.width);
    const height = read(viewportRect.height);
    const appliedRects: Record<string, ManagedWindowRect> = {};
    this.lastDraggingSlotRect = null;

    const cancelRectAnimations = options.cancelRectAnimations !== false;
    const assignRect = (window: WaylandWindow, rect: ManagedWindowRect) => {
      this.applyTileRect(
        window,
        rect,
        appliedRects,
        animate,
        animationOptions,
        cancelRectAnimations,
      );
    };

    if (tileable.length === 1) {
      const window = tileable[0];
      assignRect(
        window,
        window.state[WINDOW_STATE_MAXIMIZED]()
          ? this.maximizedTileRect(window, x)
          : { x, y, width, height },
      );
    } else {
      const master = tileable[0];
      const slaves = tileable.slice(1);
      const masterWidth = (width - TILE_GAP) / 2;
      const slaveX = x + masterWidth + TILE_GAP;
      const slaveWidth = masterWidth;
      const slaveCount = slaves.length;
      const slaveHeight = (height - (slaveCount - 1) * TILE_GAP) / slaveCount;

      assignRect(
        master,
        master.state[WINDOW_STATE_MAXIMIZED]()
          ? this.maximizedTileRect(master, x)
          : { x, y, width: masterWidth, height },
      );

      slaves.forEach((window, index) => {
        const slaveY = y + index * (slaveHeight + TILE_GAP);
        assignRect(
          window,
          window.state[WINDOW_STATE_MAXIMIZED]()
            ? this.maximizedTileRect(window, slaveX)
            : { x: slaveX, y: slaveY, width: slaveWidth, height: slaveHeight },
        );
      });
    }

    hotReloadDebug("workspace-apply-layout", {
      monitor: this.monitor,
      index: this.index,
      layout: "master",
      animate,
      suppressSSDRebuild,
      canSuppress,
      activeWindowId: this.activeWindowId,
      tileableWindowIds: tileable.map((window) => window.id),
      floatingWindowIds: this.floatingWindows().map((window) => window.id),
      appliedRects,
    });
    this.applyFloatingLayout(animationOptions, animate);
  }

  public resizeTile(event: WindowResizeEvent) {
    const tileable = this.tileableWindows();
    if (!tileable.some((window) => window.id === event.window.id)) {
      return;
    }

    if (
      (event.phase === "start" || event.phase === "update") &&
      event.window.state[WINDOW_STATE_MAXIMIZED]()
    ) {
      event.window.state[WINDOW_STATE_MAXIMIZED].set(false);
      event.window.state[WINDOW_STATE_RESTORE_RECT].set(null);
      event.window.unmaximize();
    }

    stopRectAnimation(event.window, WINDOW_STATE_RECT);
    this.activeWindowId = event.window.id;

    const viewportRect = this.tileViewportRect();
    const minWidth = this.minTileWidth(event.window, viewportRect);
    const maxWidth = this.maxTileWidth(event.window);
    const width = clamp(
      event.currentRect.width,
      minWidth,
      Math.max(minWidth, maxWidth),
    );
    this.tileWidthByWindowId.set(event.window.id, width);
    this.scrollToWindow(event.window);
    this.applyLayout();
  }

  /** Layout slot reserved for the tile being dragged, or null when not dragging. */
  public draggingSlotRect(): ManagedWindowRect | null {
    return this.draggingWindowId ? this.lastDraggingSlotRect : null;
  }

  public beginTileDrag(window: WaylandWindow, rect: ManagedWindowRect) {
    if (!this.shouldTile(window)) {
      return;
    }
    this.activeWindowId = window.id;
    this.draggingWindowId = window.id;
    const wasMaximized = window.state[WINDOW_STATE_MAXIMIZED]();
    window.state[WINDOW_STATE_MAXIMIZED].set(false);
    window.state[WINDOW_STATE_RESTORE_RECT].set(null);
    if (wasMaximized) {
      window.unmaximize();
    }
    window.state[WINDOW_STATE_TILE_DRAGGING].set(true);
    this.syncWindowVisibleOutputs(window);
    window.state[WINDOW_STATE_WORKSPACE_VISIBLE].set(true);
    window.state[WINDOW_STATE_WORKSPACE_OFFSET_Y].set(0);
    window.state[WINDOW_STATE_WORKSPACE_OPACITY].set(1);
    this.setTileWidthFromRect(window, window.state[WINDOW_STATE_RECT](), false);
    stopRectAnimation(window, WINDOW_STATE_RECT);
    window.state[WINDOW_STATE_RECT].set(rect);
    this.applyLayout();
  }

  public adoptTileDragWindow(window: WaylandWindow, rect: ManagedWindowRect) {
    if (!this.hasWindow(window)) {
      this.windows.push(window);
    }
    const visible = this.isActive();
    this.activeWindowId = window.id;
    this.draggingWindowId = window.id;
    this.setTileWidthFromRect(window, rect, false);
    window.state[WINDOW_STATE_TILE_DRAGGING].set(true);
    this.syncWindowVisibleOutputs(window);
    window.state[WINDOW_STATE_WORKSPACE_VISIBLE].set(true);
    window.state[WINDOW_STATE_WORKSPACE_OFFSET_Y].set(0);
    window.state[WINDOW_STATE_WORKSPACE_OPACITY].set(visible ? 1 : 0);
    stopRectAnimation(window, WINDOW_STATE_RECT);
    window.state[WINDOW_STATE_RECT].set(rect);
  }

  public adoptFloatingWindow(window: WaylandWindow, rect: ManagedWindowRect) {
    if (!this.hasWindow(window)) {
      this.windows.push(window);
    }
    const visible = this.isActive();
    this.activeWindowId = window.id;
    this.syncWindowVisibleOutputs(window);
    resetWorkspaceVisualState(window, visible);
    window.state[WINDOW_STATE_FLOATING_RECT].set(
      this.isTiled ? this.viewportRectToFloatingContentRect(rect) : rect,
    );
    stopRectAnimation(window, WINDOW_STATE_RECT);
    window.state[WINDOW_STATE_RECT].set(rect);
  }

  public updateTileDrag(
    window: WaylandWindow,
    rect: ManagedWindowRect,
    pointerX: number,
  ) {
    if (this.draggingWindowId !== window.id) {
      this.beginTileDrag(window, rect);
    }
    this.activeWindowId = window.id;
    this.moveTileWindowToIndex(
      window,
      this.tileInsertionIndexForPointer(window, pointerX),
    );
    stopRectAnimation(window, WINDOW_STATE_RECT);
    window.state[WINDOW_STATE_RECT].set(rect);
    this.scrollToWindow(window);
    this.applyLayout();
  }

  public endTileDrag(window: WaylandWindow, cancelled: boolean) {
    if (this.draggingWindowId !== window.id) {
      return;
    }
    this.draggingWindowId = null;
    window.state[WINDOW_STATE_TILE_DRAGGING].set(false);
    this.syncWindowVisibleOutputs(window);
    window.state[WINDOW_STATE_WORKSPACE_OFFSET_Y].set(0);
    window.state[WINDOW_STATE_WORKSPACE_OPACITY].set(this.isActive() ? 1 : 0);
    if (!cancelled) {
      this.activeWindowId = window.id;
      this.scrollToWindow(window);
    }
    this.applyLayout();
    if (!cancelled && this.isActive()) {
      window.focus();
    }
  }

  public focusWindow(window: WaylandWindow) {
    if (!this.shouldTile(window)) {
      return;
    }
    if (this.activeWindowId === window.id) {
      return;
    }
    this.activeWindowId = window.id;
    this.scrollToWindow(window);
    this.applyLayout();
  }

  /**
   * "Go to this window": center the target in the viewport (overriding the
   * normal `scrollToWindow` which is a no-op when already visible) and animate
   * the layout. Used by dock clicks and any other "jump to window" gesture.
   */
  public panToWindow(window: WaylandWindow) {
    if (!this.isTiled) {
      return;
    }
    if (!this.shouldTile(window)) {
      return;
    }
    this.activeWindowId = window.id;
    this.scrollToWindow(window, { force: true });
    this.applyLayout();
  }

  public focusWindowUnderPointer(window: WaylandWindow): WaylandWindow | undefined {
    if (
      !this.isTiled ||
      !this.hasWindow(window) ||
      window.state[WINDOW_STATE_MINIMIZED]()
    ) {
      return undefined;
    }

    const focused = this.focusedWindow();
    if (
      focused &&
      focused.id !== window.id &&
      this.areTransientRelatives(focused, window)
    ) {
      return undefined;
    }

    if (read(window.isFocused)) {
      return undefined;
    }

    if (this.shouldTile(window)) {
      const previousActiveWindowId = this.activeWindowId;
      this.activeWindowId = window.id;
      if (previousActiveWindowId !== window.id) {
        this.reapplyStaticManagedLayout();
      }
    }
    window.focus();
    return window;
  }

  private areTransientRelatives(a: WaylandWindow, b: WaylandWindow): boolean {
    return (
      this.isTransientChildOf(a, b) ||
      this.isTransientChildOf(b, a) ||
      this.hasUnparentedTransientAffinity(a, b)
    );
  }

  private isTransientChildOf(
    child: WaylandWindow,
    parent: WaylandWindow,
  ): boolean {
    return child.isTransient() && child.parentId() === parent.id;
  }

  private hasUnparentedTransientAffinity(
    a: WaylandWindow,
    b: WaylandWindow,
  ): boolean {
    const transient = !this.shouldTile(a) && a.isTransient() ? a : null;
    const other =
      transient === a ? b : !this.shouldTile(b) && b.isTransient() ? b : null;
    if (!transient || !other || transient.parentId()) {
      return false;
    }

    const transientAppId = transient.appId();
    return transientAppId !== undefined && transientAppId === other.appId();
  }

  private reapplyStaticManagedLayout(): void {
    if (!this.isTiled) {
      return;
    }

    const tileable = this.tileableWindows();
    if (tileable.length === 0) {
      return;
    }

    withManagedWindowOnlySSDRebuildSuppressed(
      () => {
        this.applyLayout({
          animate: false,
          preserveMissingActive: true,
          cancelRectAnimations: false,
        });
      },
      { strict: true },
    );
    for (const window of tileable) {
      markManagedWindowDirty(window.id);
    }
  }

  public scrollBy(
    deltaX: number,
    options: {
      stopKinetic?: boolean;
      suppressSSDRebuild?: boolean;
      cancelRectAnimations?: boolean;
    } = {},
  ): boolean {
    if (!this.isTiled || deltaX === 0) {
      return false;
    }
    if (options.stopKinetic !== false) {
      this.stopKineticScroll();
    }

    const before = this.scrollOffset;
    this.scrollOffset += deltaX;
    const tileable = this.tileableWindows();
    this.clampScrollOffset(tileable.length);
    if (this.scrollOffset === before) {
      return false;
    }

    const cancelRectAnimations = options.cancelRectAnimations ?? true;
    const apply = () =>
      this.applyLayout({
        animate: false,
        preserveMissingActive: true,
        cancelRectAnimations,
      });
    if (options.suppressSSDRebuild === false) {
      apply();
    } else {
      withManagedWindowOnlySSDRebuildSuppressed(apply, { strict: true });
    }
    for (const window of tileable) {
      markManagedWindowDirty(window.id);
    }
    return true;
  }

  public startKineticScroll(
    initialVelocityX: number,
    onFrame?: () => void,
  ): void {
    this.stopKineticScroll();
    if (
      !this.isTiled ||
      Math.abs(initialVelocityX) < WORKSPACE_KINETIC_SCROLL_MIN_VELOCITY
    ) {
      return;
    }

    let velocityX = clamp(
      initialVelocityX,
      -WORKSPACE_KINETIC_SCROLL_MAX_VELOCITY,
      WORKSPACE_KINETIC_SCROLL_MAX_VELOCITY,
    );
    let lastTime: number | null = null;
    const token = this.kineticScrollToken + 1;
    this.kineticScrollToken = token;
    const intervalMs = this.kineticScrollIntervalMs();
    let firstStep = true;

    const step = (dtMs: number): boolean => {
      const deltaX = (velocityX * dtMs) / 1000;
      const scrolled = this.scrollBy(deltaX, {
        stopKinetic: false,
        cancelRectAnimations: firstStep,
      });
      firstStep = false;
      if (!scrolled) {
        this.stopKineticScroll();
        return false;
      }

      onFrame?.();

      velocityX *= Math.exp(-dtMs / WORKSPACE_KINETIC_SCROLL_TIME_CONSTANT_MS);
      if (Math.abs(velocityX) < WORKSPACE_KINETIC_SCROLL_STOP_VELOCITY) {
        this.stopKineticScroll();
        return false;
      }

      return true;
    };

    if (!step(intervalMs)) {
      return;
    }

    this.kineticScrollPoll = createManagedPoll(
      intervalMs,
      (handle) => {
        if (this.kineticScrollToken !== token || !this.isTiled) {
          handle.cancel();
          if (this.kineticScrollPoll === handle) {
            this.kineticScrollPoll = null;
          }
          return;
        }

        const now = handle.nowMs;
        const dtMs = Math.max(
          1,
          lastTime === null ? intervalMs : now - lastTime,
        );
        lastTime = now;
        step(dtMs);
      },
      "none",
    );
  }

  public stopKineticScroll(): void {
    this.kineticScrollToken += 1;
    if (this.kineticScrollPoll) {
      this.kineticScrollPoll.cancel();
      this.kineticScrollPoll = null;
    }
  }

  private kineticScrollIntervalMs(): number {
    const refreshRate =
      COMPOSITOR.output.current[this.monitor]?.resolution?.refreshRate ??
      WORKSPACE_KINETIC_SCROLL_FALLBACK_REFRESH_RATE;
    return 1000 / Math.max(1, refreshRate);
  }

  public focusRelative(direction: -1 | 1) {
    const tileable = this.tileableWindows();
    if (tileable.length === 0) {
      return;
    }
    const activeIndex = tileable.findIndex(
      (window) => window.id === this.activeWindowId,
    );
    const fallbackIndex = this.focusFallbackTileIndex(tileable, direction);
    const currentIndex =
      activeIndex >= 0
        ? activeIndex
        : (fallbackIndex ?? (direction < 0 ? tileable.length : -1));
    const nextIndex = clamp(currentIndex + direction, 0, tileable.length - 1);
    this.activeWindowId = tileable[nextIndex].id;
    this.scrollToWindow(tileable[nextIndex]);
    this.applyLayout();
    this.focusActiveWindow();
  }

  private focusFallbackTileIndex(
    tileable: WaylandWindow[],
    direction: -1 | 1,
  ): number | undefined {
    const focused = this.focusedWindow();
    if (!focused || this.shouldTile(focused)) {
      return undefined;
    }

    const focusedCenter = rectCenterX(focused.state[WINDOW_STATE_RECT]());
    const candidates = tileable
      .map((window, index) => ({
        index,
        center: rectCenterX(window.state[WINDOW_STATE_RECT]()),
      }))
      .filter(({ center }) =>
        direction < 0 ? center < focusedCenter : center > focusedCenter,
      );
    if (candidates.length === 0) {
      return undefined;
    }

    candidates.sort((a, b) =>
      direction < 0 ? b.center - a.center : a.center - b.center,
    );
    return candidates[0].index - direction;
  }

  public focusActiveWindow() {
    const active = this.windows.find(
      (window) => window.id === this.activeWindowId,
    );
    active?.focus();
  }

  public shouldTile(window: WaylandWindow): boolean {
    return window.isResizable() && !window.isTransient();
  }

  public snapshot(): WorkspaceSnapshot {
    return {
      monitor: this.monitor,
      index: this.index,
      isTiled: this.isTiled,
      activeWindowId: this.activeWindowId,
      scrollOffset: this.scrollOffset,
      tileLayout: this.tileLayout,
      windows: this.windows.map((window) => this.snapshotWindow(window)),
    };
  }

  public restore(snapshot: WorkspaceSnapshot) {
    this.isTiled = snapshot.isTiled;
    this.activeWindowId = snapshot.activeWindowId;
    this.scrollOffset = snapshot.scrollOffset;
    this.tileLayout = snapshot.tileLayout ?? "scrolling";
    this.tileWidthByWindowId.clear();
    this.restoredWindowStateById.clear();
    for (const window of snapshot.windows) {
      if (window.tileWidth !== undefined) {
        this.tileWidthByWindowId.set(window.id, window.tileWidth);
      }
      this.restoredWindowStateById.set(window.id, window);
    }
    hotReloadDebug("workspace-restore", {
      monitor: this.monitor,
      index: this.index,
      isTiled: this.isTiled,
      activeWindowId: this.activeWindowId,
      scrollOffset: this.scrollOffset,
      restoredWindowIds: Array.from(this.restoredWindowStateById.keys()),
    });
  }

  public getWindows(): WaylandWindow[] {
    return Array.from(this.windows);
  }

  private snapshotWindow(window: WaylandWindow): WorkspaceWindowSnapshot {
    return {
      id: window.id,
      tileWidth: this.tileWidthByWindowId.get(window.id),
      floatingRect: window.state[WINDOW_STATE_FLOATING_RECT](),
      restoreRect: window.state[WINDOW_STATE_RESTORE_RECT](),
      snapZone: window.state[WINDOW_STATE_SNAP_ZONE](),
      snapMonitor: window.state[WINDOW_STATE_SNAP_MONITOR](),
      minimized: window.state[WINDOW_STATE_MINIMIZED](),
      maximized: window.state[WINDOW_STATE_MAXIMIZED](),
    };
  }

  private syncWindowVisibleOutputs(window: WaylandWindow) {
    window.state[WINDOW_STATE_TILED].set(this.isTiled && this.shouldTile(window));
    window.state[WINDOW_STATE_VISIBLE_OUTPUTS].set(
      this.isTiled ? [this.monitor] : null,
    );
  }

  private canSuppressLayoutSSDRebuild(_tileable: WaylandWindow[]): boolean {
    // Opening windows may still be building decoration structure, labels,
    // icons, and shader inputs. SSD rebuild suppression is global, so using
    // it for existing windows' layout animation would also hide those
    // initial decoration updates until an unrelated interaction occurs.
    return true;
  }

  private tileableWindows(): WaylandWindow[] {
    return this.windows.filter(
      (window) =>
        this.shouldTile(window) && !window.state[WINDOW_STATE_MINIMIZED](),
    );
  }

  public focusedWindow(): WaylandWindow | undefined {
    return this.windows.find((window) => read(window.isFocused));
  }

  public syncFloatingWindowRect(
    window: WaylandWindow,
    viewportRect: ManagedWindowRect,
  ) {
    if (!this.isTiled) {
      window.state[WINDOW_STATE_FLOATING_RECT].set(viewportRect);
      return;
    }
    if (this.shouldTile(window)) {
      return;
    }
    window.state[WINDOW_STATE_FLOATING_RECT].set(
      this.viewportRectToFloatingContentRect(viewportRect),
    );
  }

  private activeWindow(windows = this.windows): WaylandWindow | undefined {
    return windows.find((window) => window.id === this.activeWindowId);
  }

  private tileInsertionIndexForPointer(
    window: WaylandWindow,
    pointerX: number,
  ): number {
    const tileable = this.tileableWindows().filter(
      (current) => current.id !== window.id,
    );
    const viewportRect = this.tileViewportRect();
    const contentX = pointerX - read(viewportRect.x) + this.scrollOffset;
    let left = 0;

    for (let index = 0; index < tileable.length; index++) {
      const width = this.tileWidthForWindow(tileable[index], viewportRect);
      if (contentX < left + width / 2) {
        return index;
      }
      left += width + TILE_GAP;
    }

    return tileable.length;
  }

  private moveTileWindowToIndex(window: WaylandWindow, tileIndex: number) {
    const currentIndex = this.windows.findIndex(
      (current) => current.id === window.id,
    );
    if (currentIndex < 0) {
      return;
    }

    this.windows.splice(currentIndex, 1);
    const tileableWithoutWindow = this.tileableWindows();
    const beforeWindow = tileableWithoutWindow[tileIndex];

    if (beforeWindow) {
      const insertIndex = this.windows.findIndex(
        (current) => current.id === beforeWindow.id,
      );
      this.windows.splice(Math.max(0, insertIndex), 0, window);
      return;
    }

    let lastTileableIndex = -1;
    for (let index = 0; index < this.windows.length; index++) {
      if (this.shouldTile(this.windows[index])) {
        lastTileableIndex = index;
      }
    }
    this.windows.splice(lastTileableIndex + 1, 0, window);
  }

  private captureFloatingRect(window: WaylandWindow) {
    if (!window.state[WINDOW_STATE_FLOATING_RECT]()) {
      const rect = this.isTiled
        ? this.viewportRectToFloatingContentRect(
            window.state[WINDOW_STATE_RECT](),
          )
        : window.state[WINDOW_STATE_RECT]();
      window.state[WINDOW_STATE_FLOATING_RECT].set(rect);
    }
  }

  public floatingWindows(): WaylandWindow[] {
    return this.windows.filter(
      (window) =>
        !this.shouldTile(window) && !window.state[WINDOW_STATE_MINIMIZED](),
    );
  }

  private applyFloatingLayout(
    animationOptions: LayoutOptions | undefined,
    animate = true,
  ) {
    for (const window of this.floatingWindows()) {
      // A maximized window's rect is owned by the maximize flow. Rolling it
      // back to FLOATING_RECT here could hit a degenerate rect (same reason
      // as in setTiled(false)).
      if (window.state[WINDOW_STATE_MAXIMIZED]()) {
        continue;
      }
      const contentRect =
        window.state[WINDOW_STATE_FLOATING_RECT]() ??
        this.viewportRectToFloatingContentRect(
          this.centeredFloatingRect(window),
        );
      window.state[WINDOW_STATE_FLOATING_RECT].set(contentRect);
      const rect = this.floatingContentRectToViewportRect(contentRect);
      if (animate) {
        playRectAnimation(
          window,
          WINDOW_STATE_RECT,
          rect,
          WINDOW_MANAGEMENT_EASING,
          TILE_ANIMATION_DURATION,
          animationOptions,
        );
      } else {
        stopRectAnimation(window, WINDOW_STATE_RECT);
        window.state[WINDOW_STATE_RECT].set(rect);
      }
      hotReloadDebug("workspace-apply-floating-layout", {
        monitor: this.monitor,
        index: this.index,
        animate,
        windowId: window.id,
        rect,
        contentRect,
      });
    }
  }

  private centeredFloatingRect(window: WaylandWindow): ManagedWindowRect {
    const sizeRect = this.naturalRootRect(window);
    const monitor = COMPOSITOR.output.current[this.monitor];
    if (!monitor?.resolution) {
      return sizeRect;
    }

    const usableRect = COMPOSITOR.layer.usableArea(this.monitor);
    const logicalWidth =
      usableRect?.width ?? monitor.resolution.width / monitor.scale;
    const logicalHeight =
      usableRect?.height ?? monitor.resolution.height / monitor.scale;
    const logicalX = usableRect?.x ?? monitor.position.x;
    const logicalY = usableRect?.y ?? monitor.position.y;

    let width = read(sizeRect.width);
    let height = read(sizeRect.height);
    // Reading the natural size while the client geometry is still unsettled
    // (≈0, e.g. right after the first commit) yields a degenerate rect that
    // is nothing but the SSD frame. Freezing that as the floating restore
    // rect would later configure the client to a tiny size when switching to
    // floating mode. Fall back to a default size based on the usable area
    // only when the rect is clearly degenerate (frame + titlebar at most).
    // 50px is a conservative threshold that no real app's natural size ever
    // falls under.
    const DEGENERATE_SIZE_PX = 50;
    if (width < DEGENERATE_SIZE_PX || height < DEGENERATE_SIZE_PX) {
      width = Math.round(logicalWidth * 0.6);
      height = Math.round(logicalHeight * 0.7);
    }

    return {
      x: logicalX + (logicalWidth - width) / 2,
      y: logicalY + (logicalHeight - height) / 2,
      width,
      height,
    };
  }

  private viewportRectToFloatingContentRect(
    rect: ManagedWindowRect,
  ): ManagedWindowRect {
    return {
      x: read(rect.x) + this.scrollOffset,
      y: read(rect.y),
      width: read(rect.width),
      height: read(rect.height),
    };
  }

  private floatingContentRectToViewportRect(
    rect: ManagedWindowRect,
  ): ManagedWindowRect {
    return {
      x: read(rect.x) - this.scrollOffset,
      y: read(rect.y),
      width: read(rect.width),
      height: read(rect.height),
    };
  }

  private clampRectToViewport(rect: ManagedWindowRect): ManagedWindowRect {
    const viewport = this.tileViewportRect();
    const width = read(rect.width);
    const height = read(rect.height);
    const minX = read(viewport.x);
    const minY = read(viewport.y);
    const maxX = minX + Math.max(0, read(viewport.width) - width);
    const maxY = minY + Math.max(0, read(viewport.height) - height);
    return {
      x: clamp(read(rect.x), minX, maxX),
      y: clamp(read(rect.y), minY, maxY),
      width,
      height,
    };
  }

  private scrollToWindow(
    window: WaylandWindow,
    options: { force?: boolean } = {},
  ) {
    this.stopKineticScroll();

    const tileable = this.tileableWindows();
    const index = tileable.findIndex((current) => current.id === window.id);
    if (index < 0) {
      return;
    }

    const viewportRect = this.tileViewportRect();
    const viewportWidth = read(viewportRect.width);
    const windowLeft = this.tileLeftForIndex(tileable, index, viewportRect);
    const windowRight =
      windowLeft + this.tileWidthForWindow(window, viewportRect);

    if (window.state[WINDOW_STATE_MAXIMIZED]() || options.force) {
      // Center the window in the viewport. `force` is set by dock-style "go to
      // this window" requests where we always want a visible pan, even when
      // the target is already on-screen.
      this.scrollOffset =
        windowLeft + (windowRight - windowLeft) / 2 - viewportWidth / 2;
    } else if (windowLeft < this.scrollOffset) {
      this.scrollOffset = windowLeft;
    } else if (windowRight > this.scrollOffset + viewportWidth) {
      this.scrollOffset = windowRight - viewportWidth;
    }

    this.clampScrollOffset(tileable.length);
  }

  private clampScrollOffset(tileCount: number) {
    const tileable = this.tileableWindows();
    const viewportRect = this.tileViewportRect();
    const viewportWidth = read(viewportRect.width);
    const contentWidth = this.tileContentWidth(
      tileable.slice(0, tileCount),
      viewportRect,
    );
    const maxScrollOffset = Math.max(0, contentWidth - viewportWidth);
    this.scrollOffset = clamp(this.scrollOffset, 0, maxScrollOffset);
  }

  private tileWidthForWindow(
    window: WaylandWindow,
    viewportRect: ManagedWindowRect,
  ): number {
    if (window.state[WINDOW_STATE_MAXIMIZED]()) {
      return read(this.maximizedRootRect(window).width);
    }

    const width =
      this.tileWidthByWindowId.get(window.id) ??
      this.defaultTileWidth(viewportRect);
    return clamp(
      width,
      this.minTileWidth(window, viewportRect),
      Math.max(
        this.minTileWidth(window, viewportRect),
        this.maxTileWidth(window),
      ),
    );
  }

  private maximizedTileRect(
    window: WaylandWindow,
    x: number,
  ): ManagedWindowRect {
    const maximizedRect = this.maximizedRootRect(window);
    return {
      x,
      y: read(maximizedRect.y),
      width: read(maximizedRect.width),
      height: read(maximizedRect.height),
    };
  }

  private setTileWidthFromRect(
    window: WaylandWindow,
    rect: ManagedWindowRect,
    overwrite: boolean,
  ) {
    if (!overwrite && this.tileWidthByWindowId.has(window.id)) {
      return;
    }
    const viewportRect = this.tileViewportRect();
    this.tileWidthByWindowId.set(
      window.id,
      clamp(
        read(rect.width),
        this.minTileWidth(window, viewportRect),
        Math.max(
          this.minTileWidth(window, viewportRect),
          this.maxTileWidth(window),
        ),
      ),
    );
  }

  private defaultTileWidth(viewportRect: ManagedWindowRect): number {
    return Math.max(
      TILE_MIN_WIDTH,
      read(viewportRect.width) * TILE_WIDTH_RATIO,
    );
  }

  private minTileWidth(
    window: WaylandWindow,
    viewportRect: ManagedWindowRect,
  ): number {
    const constraints = window.sizeConstraints();
    const extra = this.rootClientWidthExtra(window);
    return Math.max(
      TILE_MIN_WIDTH,
      (constraints.min?.width ?? 1) + extra,
      read(viewportRect.width) * 0.2,
    );
  }

  private maxTileWidth(window: WaylandWindow): number {
    const constraints = window.sizeConstraints();
    const extra = this.rootClientWidthExtra(window);
    const max = constraints.max?.width;
    return max && max > 0 ? max + extra : Number.POSITIVE_INFINITY;
  }

  private rootClientWidthExtra(window: WaylandWindow): number {
    const natural = this.naturalRootRect(window);
    return Math.max(0, read(natural.width) - window.position.width);
  }

  private tileLeftForIndex(
    tileable: WaylandWindow[],
    index: number,
    viewportRect: ManagedWindowRect,
  ): number {
    let left = 0;
    for (let i = 0; i < index; i++) {
      left += this.tileWidthForWindow(tileable[i], viewportRect) + TILE_GAP;
    }
    return left;
  }

  private tileContentWidth(
    tileable: WaylandWindow[],
    viewportRect: ManagedWindowRect,
  ): number {
    if (tileable.length === 0) {
      return 0;
    }
    return (
      tileable.reduce(
        (sum, window) => sum + this.tileWidthForWindow(window, viewportRect),
        0,
      ) +
      (tileable.length - 1) * TILE_GAP
    );
  }

  private tileViewportRect(): ManagedWindowRect {
    const monitor = COMPOSITOR.output.current[this.monitor];
    const usableRect = COMPOSITOR.layer.usableArea(this.monitor);
    const base =
      usableRect ??
      (monitor?.resolution
        ? {
            x: monitor.position.x,
            y: monitor.position.y,
            width: monitor.resolution.width / monitor.scale,
            height: monitor.resolution.height / monitor.scale,
          }
        : {
            x: 0,
            y: 0,
            width: 1280,
            height: 720,
          });

    return insetRect(base, {
      top: TILE_MARGIN,
      right: TILE_MARGIN,
      bottom: TILE_MARGIN,
      left: TILE_MARGIN,
    });
  }

  private fullscreenRootRect(window: WaylandWindow): ManagedWindowRect {
    const monitor = COMPOSITOR.output.current[this.monitor];
    if (monitor?.resolution) {
      return {
        x: monitor.position.x,
        y: monitor.position.y,
        width: monitor.resolution.width / monitor.scale,
        height: monitor.resolution.height / monitor.scale,
      };
    }
    return window.state[WINDOW_STATE_RECT]();
  }
}

function workspaceKey(monitor: string, index: number): string {
  return `${monitor}:${index}`;
}

function constrainedMax(
  constraints: WindowSizeConstraints,
  axis: "width" | "height",
  extra: number,
): number {
  const max = constraints.max?.[axis];
  return max && max > 0 ? max + extra : Number.POSITIVE_INFINITY;
}

function resizeOriginForAxis(
  start: WindowResizeRect,
  current: WindowResizeRect,
  constrainedSize: number,
  negativeEdge: boolean,
  axis: "x" | "y",
): number {
  if (!negativeEdge) {
    return current[axis];
  }

  const startSize = axis === "x" ? start.width : start.height;
  return start[axis] + startSize - constrainedSize;
}

function clamp(value: number, min: number, max: number): number {
  return Math.min(Math.max(value, min), max);
}

function managedRectContainsPoint(
  rect: ManagedWindowRect,
  x: number,
  y: number,
): boolean {
  const left = read(rect.x);
  const top = read(rect.y);
  return (
    x >= left &&
    x < left + read(rect.width) &&
    y >= top &&
    y < top + read(rect.height)
  );
}

function rectCenterX(rect: ManagedWindowRect): number {
  return read(rect.x) + read(rect.width) / 2;
}

function insetRect(
  rect: ManagedWindowRect,
  padding: { top: number; right: number; bottom: number; left: number },
): ManagedWindowRect {
  const width = Math.max(1, read(rect.width) - padding.left - padding.right);
  const height = Math.max(1, read(rect.height) - padding.top - padding.bottom);
  return {
    x: read(rect.x) + padding.left,
    y: read(rect.y) + padding.top,
    width,
    height,
  };
}

function snapshotManagedRect(rect: ManagedWindowRect): ManagedWindowRect {
  return {
    x: read(rect.x),
    y: read(rect.y),
    width: read(rect.width),
    height: read(rect.height),
  };
}

function managedRectEquals(
  a: ManagedWindowRect,
  b: ManagedWindowRect,
): boolean {
  return (
    read(a.x) === read(b.x) &&
    read(a.y) === read(b.y) &&
    read(a.width) === read(b.width) &&
    read(a.height) === read(b.height)
  );
}

function isLayoutSnapZone(zone: SnapZone | null): zone is LayoutSnapZone {
  return zone !== null && zone !== "maximize";
}

function isLeftSnapZone(zone: LayoutSnapZone): boolean {
  return zone === "left" || zone === "top-left" || zone === "bottom-left";
}

function isRightSnapZone(zone: LayoutSnapZone): boolean {
  return zone === "right" || zone === "top-right" || zone === "bottom-right";
}

function isTopSnapZone(zone: LayoutSnapZone): boolean {
  return zone === "top-left" || zone === "top-right";
}

function isBottomSnapZone(zone: LayoutSnapZone): boolean {
  return zone === "bottom-left" || zone === "bottom-right";
}

function snapColumn(zone: LayoutSnapZone): SnapColumn {
  return isLeftSnapZone(zone) ? "left" : "right";
}

function snapZonesConflict(
  current: SnapZone | null,
  next: LayoutSnapZone,
): boolean {
  if (!isLayoutSnapZone(current)) {
    return false;
  }
  if (current === next) {
    return true;
  }

  if (next === "left") {
    return current === "top-left" || current === "bottom-left";
  }
  if (next === "right") {
    return current === "top-right" || current === "bottom-right";
  }
  if (current === "left") {
    return next === "top-left" || next === "bottom-left";
  }
  if (current === "right") {
    return next === "top-right" || next === "bottom-right";
  }
  return false;
}

function averageOr(values: number[], fallback: number): number {
  if (values.length === 0) {
    return fallback;
  }
  return values.reduce((sum, value) => sum + value, 0) / values.length;
}

function withManagedWindowOnlySSDRebuildSuppressed<T>(
  callback: () => T,
  options: { strict?: boolean } = {},
): T {
  return COMPOSITOR.runtime.withSSDRebuildSuppressed(
    options.strict
      ? STRICT_MANAGED_WINDOW_ONLY_REBUILD_SUPPRESSION
      : MANAGED_WINDOW_ONLY_REBUILD_SUPPRESSION,
    callback,
  );
}

// Rect deltas use `add` so open/close/workspace motion can layer on top of
// override-mode layout animation. Open/close opacity uses `multiply`; workspace
// opacity is a separate override channel so an inactive workspace whose base
// opacity is already 0 can still fade back in deterministically.

function scheduleOpenAnimation(window: WaylandWindow): void {
  window.scheduleAnimation({
    channel: OPEN_ANIMATION_CHANNEL,
    rect: {
      from: { x: 0, y: 200, width: 0, height: 0 },
      to: { x: 0, y: 0, width: 0, height: 0 },
      duration: OPEN_CLOSE_ANIMATION_DURATION,
      easing: WINDOW_OPEN_EASING,
      mode: "add",
    },
    opacity: {
      from: 0,
      to: 1,
      duration: OPEN_CLOSE_ANIMATION_DURATION,
      easing: WINDOW_OPEN_EASING,
      mode: "multiply",
    },
  });
}

function scheduleCloseAnimation(window: WaylandWindow): void {
  window.scheduleAnimation({
    channel: CLOSE_ANIMATION_CHANNEL,
    rect: {
      from: { x: 0, y: 0, width: 0, height: 0 },
      to: { x: 0, y: 120, width: 0, height: 0 },
      duration: OPEN_CLOSE_ANIMATION_DURATION,
      easing: WINDOW_CLOSE_EASING,
      mode: "add",
    },
    opacity: {
      from: 1,
      to: 0,
      duration: OPEN_CLOSE_ANIMATION_DURATION,
      easing: WINDOW_CLOSE_EASING,
      mode: "multiply",
    },
  });
}

function scheduleMinimizeAnimation(
  window: WaylandWindow,
  minimized: boolean,
): void {
  window.scheduleAnimation({
    channel: MINIMIZE_ANIMATION_CHANNEL,
    rect: {
      from: minimized
        ? { x: 0, y: 0, width: 0, height: 0 }
        : { x: 0, y: 200, width: 0, height: 0 },
      to: minimized
        ? { x: 0, y: 120, width: 0, height: 0 }
        : { x: 0, y: 0, width: 0, height: 0 },
      duration: OPEN_CLOSE_ANIMATION_DURATION,
      easing: minimized
        ? WINDOW_MINIMIZE_RECT_EASING
        : WINDOW_UNMINIMIZE_RECT_EASING,
      mode: "add",
    },
    opacity: {
      from: minimized ? 1 : 0,
      to: minimized ? 0 : 1,
      duration: OPEN_CLOSE_ANIMATION_DURATION,
      easing: minimized
        ? WINDOW_MINIMIZE_OPACITY_EASING
        : WINDOW_UNMINIMIZE_OPACITY_EASING,
      mode: "override",
    },
  });
}

function scheduleWorkspaceVisualAnimation(
  window: WaylandWindow,
  fromOffsetY: number,
  toOffsetY: number,
  fromOpacity: number,
  toOpacity: number,
  easing: EasingFunction,
  duration: number,
): void {
  cancelWorkspaceVisualAnimation(window);
  window.state[WINDOW_STATE_WORKSPACE_OFFSET_Y].set(0);
  window.state[WINDOW_STATE_WORKSPACE_OPACITY].set(toOpacity);

  window.scheduleAnimation({
    channel: WORKSPACE_VISUAL_RECT_ANIMATION_CHANNEL,
    rect: {
      from: { x: 0, y: fromOffsetY, width: 0, height: 0 },
      to: { x: 0, y: toOffsetY, width: 0, height: 0 },
      duration,
      easing,
      mode: "add",
    },
  });
  window.scheduleAnimation({
    channel: WORKSPACE_VISUAL_OPACITY_ANIMATION_CHANNEL,
    opacity: {
      from: fromOpacity,
      to: toOpacity,
      duration,
      easing,
      mode: "override",
    },
  });
}

function resetWorkspaceVisualState(
  window: WaylandWindow,
  visible: boolean,
): void {
  cancelWorkspaceVisualAnimation(window);
  window.state[WINDOW_STATE_WORKSPACE_VISIBLE].set(visible);
  window.state[WINDOW_STATE_WORKSPACE_OFFSET_Y].set(0);
  window.state[WINDOW_STATE_WORKSPACE_OPACITY].set(visible ? 1 : 0);
}

function cancelWorkspaceVisualAnimation(window: WaylandWindow): void {
  window.cancelAnimation(WORKSPACE_VISUAL_ANIMATION_CHANNEL);
  window.cancelAnimation(WORKSPACE_VISUAL_RECT_ANIMATION_CHANNEL);
  window.cancelAnimation(WORKSPACE_VISUAL_OPACITY_ANIMATION_CHANNEL);
}
