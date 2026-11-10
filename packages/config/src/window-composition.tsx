/**
 * How every window is placed and decorated (`COMPOSITOR.window.composition`):
 * the window manager decides where it goes, `./md3e` what it looks like.
 */
import {
  ClientWindow,
  ManagedWindow,
  computed,
  read,
  type CompositionRenderable,
  type WaylandWindow,
} from "shoji_wm";
import {
  HybridWindowManager,
  WINDOW_STATE_FULLSCREEN,
  WINDOW_STATE_MINIMIZE_VISUAL_IDLE,
  WINDOW_STATE_RECT,
  WINDOW_STATE_TILE_DRAGGING,
  WINDOW_STATE_TILE_REORDERING,
  WINDOW_STATE_TILED,
  WINDOW_STATE_VISIBLE_OUTPUTS,
  WINDOW_STATE_WORKSPACE_OFFSET_Y,
  WINDOW_STATE_WORKSPACE_OPACITY,
  WINDOW_STATE_WORKSPACE_TILED,
  WINDOW_STATE_WORKSPACE_VISIBLE,
  WINDOW_ZOOM_OUT,
} from "./window-manager";
import type { WindowSwitcher } from "./window-switcher";
import { Md3Frame, Md3Rim, usesClientDecoration } from "./md3e/decoration";
import { color } from "./md3e/theme";

export const FULLSCREEN_Z_INDEX = 2_000_000_000;
const FLOATING_WINDOW_Z_INDEX_BASE = 1_500_000_000;
const WINDOW_STACK_Z_INDEX_RANGE = 100_000_000;
const FOCUSED_TILED_WINDOW_Z_INDEX = 1_000_000_000;
const REORDERING_TILED_WINDOW_Z_INDEX = -2_000_000_000;
/** The window picked in the switcher: a ring of the primary color. */
const SWITCHER_SELECTION = computed(() => [
  { blur: 0, spread: 4, color: color("primary")() },
  { y: 12, blur: 36, spread: 2, color: "#00000080" },
]);

/** Where a (non-fullscreen) window sits in the stack; higher is on top. */
export function windowZIndex(
  windowManager: HybridWindowManager,
  window: WaylandWindow,
): number {
  const stackZIndex = windowManager.getWindowZIndex(window)();
  if (!window.state[WINDOW_STATE_WORKSPACE_TILED]()) {
    return stackZIndex;
  }
  const stackOffset = Math.max(
    -WINDOW_STACK_Z_INDEX_RANGE,
    Math.min(WINDOW_STACK_Z_INDEX_RANGE, stackZIndex),
  );
  if (!window.state[WINDOW_STATE_TILED]()) {
    return FLOATING_WINDOW_Z_INDEX_BASE + stackOffset;
  }
  if (window.state[WINDOW_STATE_TILE_REORDERING]()) {
    return REORDERING_TILED_WINDOW_Z_INDEX;
  }
  return window.isFocused() ? FOCUSED_TILED_WINDOW_Z_INDEX : stackOffset;
}

/** `windowZIndex`, with fullscreen windows above everything. */
export function windowStackOrder(
  windowManager: HybridWindowManager,
  window: WaylandWindow,
): number {
  return window.state[WINDOW_STATE_FULLSCREEN]()
    ? FULLSCREEN_Z_INDEX
    : windowZIndex(windowManager, window);
}

export function createWindowComposition(
  windowManager: HybridWindowManager,
  switcher: WindowSwitcher,
): (window: WaylandWindow) => CompositionRenderable {
  return (window) => {
    const workspaceVisible = window.state[WINDOW_STATE_WORKSPACE_VISIBLE];
    const workspaceOffsetY = window.state[WINDOW_STATE_WORKSPACE_OFFSET_Y];
    const tileDragging = window.state[WINDOW_STATE_TILE_DRAGGING];
    const managedRect = computed(() => {
      const rect = window.state[WINDOW_STATE_RECT]();
      return {
        x: read(rect.x),
        y: read(rect.y) + workspaceOffsetY(),
        width: read(rect.width),
        height: read(rect.height),
      };
    });
    // Opening and closing zoom the window slightly (see WINDOW_ZOOM_OUT).
    const zoom = window.animation.variable(WINDOW_ZOOM_OUT)((zoomOut) => 1 - zoomOut);
    window.transform.scaleX = zoom;
    window.transform.scaleY = zoom;
    const minimizeVisualIdle = window.state[WINDOW_STATE_MINIMIZE_VISUAL_IDLE];
    const inactive = computed(
      () => minimizeVisualIdle() || (!workspaceVisible() && !tileDragging()),
    );
    const workspaceOpacity = window.state[WINDOW_STATE_WORKSPACE_OPACITY];
    const common = {
      rect: managedRect,
      visibleOutputs: window.state[WINDOW_STATE_VISIBLE_OUTPUTS],
      opacity: computed(() => read(workspaceOpacity())),
      forceRectSize: computed(() => window.isResizable() && !window.isTransient()),
      // Clients draw square corners and no shadow of their own.
      tiled: true,
      idle: inactive,
      interactive: inactive((value) => !value),
    };

    // Fullscreen: nothing but the client, edge to edge. Rendering only the
    // bare ClientWindow is also what lets the tty backend promote the client
    // buffer to the primary plane (direct scanout). Tearing only happens on
    // that fast path while the client outruns the refresh rate (games).
    if (window.state[WINDOW_STATE_FULLSCREEN]()) {
      return (
        <ManagedWindow {...common} zIndex={FULLSCREEN_Z_INDEX} allowTearing={true}>
          <ClientWindow />
        </ManagedWindow>
      );
    }

    const options = {
      window,
      rect: managedRect,
      // Inside the switcher's per-window textures there is nothing behind a
      // window.
      backdropless: switcher.blurSuspended(),
      // The window picked in the switcher has a ring. A signal of its own: flipping
      // through the switcher then patches the shadows of the two windows
      // involved, instead of rebuilding every window.
      shadow: switcher
        .isSelected(window.id)((selected) => (selected ? SWITCHER_SELECTION() : undefined)),
    };
    // Read reactively: the frame follows the decoration mode.
    const clientDecorated = usesClientDecoration(window.decoration());

    return (
      <ManagedWindow {...common} zIndex={computed(() => windowZIndex(windowManager, window))}>
        {clientDecorated ? Md3Rim(options) : Md3Frame(options)}
      </ManagedWindow>
    );
  };
}
