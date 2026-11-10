/**
 * A sandbox for looking at the window decorations in a nested ShojiWM
 * (winit backend) without the session around them: no shell, no services,
 * no environment published, nothing that would reach the real session.
 *
 *   dev/preview.sh                # start it
 *   dev/preview.sh run kwrite     # put a window in it
 *   dev/preview.sh theme light    # send it a scheme, as shoji-bar-4 would
 *
 * `./preview-state.ts` holds a few knobs, re-read on every reload.
 */
import { COMPOSITOR, DefaultComposition, createPoll, type WaylandWindow } from "shoji_wm";
import { HybridWindowManager, WINDOW_STATE_RECT } from "../src/window-manager";
import {
  captionButtonAt,
  decorationPolicy,
  forceCaptionButtonState,
  installFocusFade,
  naturalRootRect,
} from "../src/md3e/decoration";
import { schemeFrom, setTheme } from "../src/md3e/theme";
import { createIpcServer, defaultSocketPath } from "shoji_wm/ipc";
import { createWindowGrid } from "../src/window-grid";
import { createFlip3D } from "../src/flip-3d";
import { createWindowComposition, windowStackOrder } from "../src/window-composition";
import { PREVIEW } from "./preview-state";
import type { CaptionButtonKind } from "../src/md3e/decoration";

const WM = new HybridWindowManager(naturalRootRect);
const SWITCHER = (PREVIEW.switcher === "grid" ? createWindowGrid : createFlip3D)({
  windowManager: WM,
  stackOrder: (window) => windowStackOrder(WM, window),
  titlebarButtonAt: captionButtonAt,
});

COMPOSITOR.window.decoration.configure(decorationPolicy);
installFocusFade();
COMPOSITOR.rendering.composition = (output) =>
  SWITCHER.compose(output) ?? <DefaultComposition />;
COMPOSITOR.window.composition = createWindowComposition(WM, SWITCHER);
COMPOSITOR.effect.layer = () => ({});
// A socket of its own: a nested compositor sees the host's WAYLAND_DISPLAY,
// and the default path would be the host session's.
const PREVIEW_IPC = createIpcServer(
  defaultSocketPath().replace(/shojiwm-[^/]*\.sock$/, "shojiwm-md3e-preview.sock"),
);
PREVIEW_IPC.handle("theme.set", (params) => {
  const scheme = schemeFrom(params);
  if (scheme) {
    setTheme(scheme);
  }
});
COMPOSITOR.onDisable(() => PREVIEW_IPC.close());

COMPOSITOR.output.configure(() => ({
  winit: { mode: "extend", resolution: "best", position: "auto", scale: PREVIEW.scale },
}));

const [forcedKind, forcedState] = PREVIEW.button.split(":");
forceCaptionButtonState((forcedKind || null) as CaptionButtonKind | null, forcedState === "pressed");

if (PREVIEW.action) {
  // A poll, not setTimeout: window actions are flushed with the runtime's
  // responses, which a bare timer does not produce in an idle session.
  createPoll(
    300,
    (handle) => {
      handle.cancel();
      if (PREVIEW.action === "maximize") {
        WM.toggleFocusedWindowMaximize();
      } else if (PREVIEW.action === "fullscreen") {
        WM.toggleFocusedWindowFullscreen();
      } else if (PREVIEW.action === "tile") {
        WM.toggleCurrentWorkspaceTiling();
      } else if (PREVIEW.action === "switcher") {
        SWITCHER.open(WM.getCurrentMonitorName());
      } else if (PREVIEW.action === "drag") {
        dragWindows(WM.listWindowsByRecentUse().slice(0, 1));
      } else if (PREVIEW.action === "drag-all") {
        dragWindows(WM.listWindowsByRecentUse());
      } else if (PREVIEW.action === "focus-cycle") {
        focusCycle();
      } else if (PREVIEW.action === "switcher-cycle") {
        switcherCycle();
      }
    },
    { output: "winit" },
  );
}

// Load for profiling: moves windows around in a circle every frame for
// `PREVIEW.dragSeconds`, the way an interactive move (one window) or a tiled
// workspace scrolling (all of them) does.
function dragWindows(windows: WaylandWindow[]) {
  let pending = windows.length;
  for (const window of windows) {
    const start = window.state[WINDOW_STATE_RECT]();
    if (!start) {
      pending -= 1;
      continue;
    }
    const origin = { x: Number(start.x), y: Number(start.y) };
    let startedAt: number | null = null;
    window.createPoll(8, (handle) => {
      startedAt ??= handle.nowMs;
      const t = (handle.nowMs - startedAt) / 1000;
      const done = t >= PREVIEW.dragSeconds;
      const angle = done ? 0 : t * 2.5;
      window.state[WINDOW_STATE_RECT].set({
        ...start,
        x: Math.round(origin.x + 160 * Math.sin(angle)),
        y: Math.round(origin.y + 90 * (1 - Math.cos(angle))),
      });
      if (done) {
        handle.cancel();
        pending -= 1;
        if (pending === 0) {
          COMPOSITOR.debug.enableProfile(false);
        }
      }
    });
  }
}

// Load for profiling: moves the tile focus back and forth every half second
// for `PREVIEW.dragSeconds`, the way Super+Ctrl+Left/Right does.
function focusCycle() {
  let startedAt: number | null = null;
  let step = 0;
  createPoll(
    150,
    (handle) => {
      startedAt ??= handle.nowMs;
      if (handle.nowMs - startedAt >= PREVIEW.dragSeconds * 1000) {
        handle.cancel();
        COMPOSITOR.debug.enableProfile(false);
        return;
      }
      WM.focusTile(step % 4 < 2 ? 1 : -1);
      step += 1;
    },
    { output: "winit" },
  );
}

// Load for profiling: opens the switcher and flips it forward every 300 ms
// for `PREVIEW.dragSeconds`, then switches.
function switcherCycle() {
  SWITCHER.open(WM.getCurrentMonitorName());
  let startedAt: number | null = null;
  createPoll(
    300,
    (handle) => {
      startedAt ??= handle.nowMs;
      if (handle.nowMs - startedAt >= PREVIEW.dragSeconds * 1000) {
        handle.cancel();
        COMPOSITOR.debug.enableProfile(false);
        return;
      }
      SWITCHER.flip(1);
    },
    // Like a key press: only the signals it writes make anything dirty.
    { output: "winit", dirty: "none" },
  );
}

// Timescope, for the length of a "drag": dumped to
// $HOME/shoji_wm/logs/timescope-latest when the drag ends.
COMPOSITOR.debug.enableProfile(
  PREVIEW.profile &&
    (PREVIEW.action.startsWith("drag") ||
      PREVIEW.action === "focus-cycle" ||
      PREVIEW.action === "switcher-cycle"),
);

COMPOSITOR.key.bind("window-switcher", "Super+Tab", () => {
  SWITCHER.open(WM.getCurrentMonitorName());
});
COMPOSITOR.key.bind("toggle-tiling-mode", "Super+S", () => {
  WM.toggleCurrentWorkspaceTiling();
});
COMPOSITOR.pointer.bindWindowMoveModifier("Super");
COMPOSITOR.pointer.bindWindowResizeModifier("Super");

COMPOSITOR.event.onOpen((window) => WM.onOpen(window));
COMPOSITOR.event.onInitialConfigure((window) => WM.onInitialConfigure(window));
COMPOSITOR.event.onFirstCommit((window) => {
  WM.onFirstCommit(window);
  console.info(`preview window ${window.title.peek()} app=${window.appId.peek()} icon=${JSON.stringify(window.icon.peek())}`);
});
COMPOSITOR.event.onStartClose((window) => WM.onStartClose(window));
COMPOSITOR.event.onClose((window) => WM.onClose(window));
COMPOSITOR.event.onFocus((window, focused) => {
  WM.onFocus(window, focused);
  if (focused) {
    WM.recordFocus(window.id);
  }
});
COMPOSITOR.event.onPointerMove((event) => WM.onPointerMove(event));
COMPOSITOR.event.onGestureSwipe((event) => WM.onGestureSwipe(event));
COMPOSITOR.event.onOutputChange((event) => WM.onOutputChange(event));
COMPOSITOR.event.onCreateLayer(() => WM.refreshUsableAreaLayouts());
COMPOSITOR.event.onUpdateLayer(() => WM.refreshUsableAreaLayouts());
COMPOSITOR.event.onDestroyLayer(() => WM.refreshUsableAreaLayouts());
COMPOSITOR.event.onWindowResize((event) => WM.onWindowResize(event));
COMPOSITOR.event.onWindowMove((event) => WM.onWindowMove(event));
COMPOSITOR.event.onWindowMaximizeRequest((event) => WM.onWindowMaximizeRequest(event));
COMPOSITOR.event.onWindowMinimizeRequest((event) => WM.onWindowMinimizeRequest(event));
COMPOSITOR.event.onWindowFullscreenRequest((event) => WM.onWindowFullscreenRequest(event));
COMPOSITOR.event.onWindowActivateRequest((event) => WM.onWindowActivateRequest(event));

const SNAPSHOT_KEY = "preview.window-manager";
COMPOSITOR.onDisable((event) => {
  if (event.isReloading) {
    event.persist(SNAPSHOT_KEY, WM.snapshot());
  }
  WM.dispose();
});
COMPOSITOR.onEnable((event) => {
  if (event.isReloading) {
    const snapshot = event.restore<ReturnType<typeof WM.snapshot>>(SNAPSHOT_KEY);
    if (snapshot) {
      WM.restore(snapshot);
    }
  }
});

export default COMPOSITOR;
