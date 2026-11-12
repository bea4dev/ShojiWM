import {
  COMPOSITOR,
  backdropSource,
  compileEffect,
  compileLayerEffect,
  dualKawaseBlur,
  computed,
  shaderStage,
  loadShader,
  layerSource,
  type DisplayConfigDraft,
  compilePopupEffect,
  popupSource,
  DefaultComposition,
} from "shoji_wm";
import { createIpcServer } from "shoji_wm/ipc";
import {
  HybridWindowManager,
  WINDOW_STATE_MINIMIZED,
  WINDOW_STATE_MINIMIZE_VISUAL_IDLE,
} from "./window-manager";
import { createWindowGrid } from "./window-grid";
import { createFlip3D } from "./flip-3d";
import { installDragAndDrop } from "./drag-and-drop";
import { bindLidPolicy } from "./lid-policy";
import { createWindowComposition, windowStackOrder } from "./window-composition";
import {
  captionButtonAt,
  decorationPolicy,
  installFocusFade,
  naturalRootRect,
} from "./md3e/decoration";
import { currentScheme, schemeFrom, setTheme, type Scheme } from "./md3e/theme";

COMPOSITOR.env.apply({
  QT_QPA_PLATFORM: "wayland;xcb",
  QT_QPA_PLATFORMTHEME: "qt6ct",
  QT_IM_MODULE: "fcitx",
  XMODIFIERS: "@im=fcitx",
  SDL_IM_MODULE: "fcitx",
  GLFW_IM_MODULE: "ibus",
  ELECTRON_OZONE_PLATFORM_HINT: "wayland",
});
COMPOSITOR.env.publish();

COMPOSITOR.cursor.configure({
  theme: "Bibata-Modern-Ice",
  size: 24,
});

// Closing the lid does what logind.conf says ("logind"); set onBattery or
// onExternalPower to "suspend", "hibernate", "ignore", ... to choose. While it
// is shut with another display on, the built-in panel stays dark. See
// ./lid-policy.ts for the callbacks and the guards against a stuck lid switch.
bindLidPolicy({
  onBattery: (_context) => {
    COMPOSITOR.power.suspend();
  },
  onExternalPower: "ignore",
  internalPanelOff: true,
});

COMPOSITOR.process.once("GTK-CSD-control-buttons", {
  command: "gsettings set org.gnome.desktop.wm.preferences button-layout ':minimize,maximize,close'",
  runPolicy: "once-per-session",
});

// Material 3 Expressive frames (./md3e) for clients that want server-side
// decorations; the rest keep their own and get our corners and shadow.
COMPOSITOR.window.decoration.configure(decorationPolicy);
installFocusFade();

// GTK 3 applications (Thunar, ...) choose between their own title bar and
// ours once, at startup, from this default alone: "server" drops theirs.
// Firefox and GTK header bars still ask for CSD, and the policy above keeps
// honoring that. Forcing "server" on Firefox makes it re-request CSD over and
// over (ShojiWM stops answering after a few, but this loop once pinned
// Firefox's CPU) and leaves two title bars.
COMPOSITOR.window.decoration.kdeDefaultMode(() => "server");

// enable triple buffer mode for massive performance
COMPOSITOR.rendering.framePacing = "throughput";

// select your GPU
COMPOSITOR.gpu.selectRender(gpus => gpus.find(gpu => gpu.bootVga));

const HYBRID_WINDOW_MANAGER = new HybridWindowManager(naturalRootRect);

// Window switcher (Super+Tab): "flip-3d" stacks the windows in 3D
// (./flip-3d.tsx), "grid" lays them out side by side (./window-grid.tsx).
const WINDOW_SWITCHER_STYLE = "flip-3d" as "flip-3d" | "grid";
const WINDOW_SWITCHER = (
  WINDOW_SWITCHER_STYLE === "grid" ? createWindowGrid : createFlip3D
)({
  windowManager: HYBRID_WINDOW_MANAGER,
  stackOrder: (window) => windowStackOrder(HYBRID_WINDOW_MANAGER, window),
  // The grid's selected card: its title bar buttons work.
  titlebarButtonAt: captionButtonAt,
});

// Drag and drop: the dragged image on a card with the action and the kind of
// data, flying back when a drop is refused (./drag-and-drop.tsx).
installDragAndDrop();
COMPOSITOR.rendering.composition = (output) =>
  WINDOW_SWITCHER.compose(output) ?? <DefaultComposition />;

// Window backdrop effects end with this stage so they can fade back in after
// the switcher closes.
const backdropFade = () =>
  shaderStage(loadShader("./src/effect/backdrop-fade.frag"), {
    uniforms: { strength: WINDOW_SWITCHER.backdropStrength },
  });
const HOT_RELOAD_WINDOW_MANAGER_STATE = "config.hybrid-window-manager";
const HOT_RELOAD_THEME = "config.md3e-theme";

COMPOSITOR.onDisable((event) => {
  if (event.isReloading) {
    const snapshot = HYBRID_WINDOW_MANAGER.snapshot();
    event.persist(HOT_RELOAD_WINDOW_MANAGER_STATE, snapshot);
    event.persist(HOT_RELOAD_THEME, currentScheme.peek());
  }
  HYBRID_WINDOW_MANAGER.dispose();
});

COMPOSITOR.onEnable((event) => {
  if (event.isReloading) {
    const theme = event.restore<Scheme>(HOT_RELOAD_THEME);
    if (theme) {
      setTheme(theme);
    }
    const snapshot = event.restore<
      ReturnType<typeof HYBRID_WINDOW_MANAGER.snapshot>
    >(HOT_RELOAD_WINDOW_MANAGER_STATE);
    if (snapshot) {
      HYBRID_WINDOW_MANAGER.restore(snapshot);
    }
  }
});

// ---------------------------------------------------------------------------
// External IPC: expose the workspace layout to clients such as the bar.
//   workspaces.get           -> WorkspacesView                     (request/response)
//   workspaces.switch        { direction: -1 | 1 }                 (command)
//   workspaces.activate      { monitor: string, index: number }    (command)
//   workspaces.toggleTiling  { monitor?: string }                  (command)
//   workspaces.changed       -> WorkspacesView                     (broadcast)
//   windows.activate         { windowId: string }                  (command)
//   dock.proximity           { monitor: string, inside: bool }    (broadcast)
//   theme.set                { dark, primary, surface, ... }       (command)
// ---------------------------------------------------------------------------
const WORKSPACE_IPC = createIpcServer();
let lastWorkspacesJson = "";
let workspaceBroadcastQueued = false;

function broadcastWorkspaces() {
  const view = HYBRID_WINDOW_MANAGER.viewForIpc();
  const json = JSON.stringify(view);
  if (json === lastWorkspacesJson) {
    return;
  }
  lastWorkspacesJson = json;
  WORKSPACE_IPC.broadcast("workspaces.changed", view);
}

function reconfigureProtocolWorkspaces() {
  COMPOSITOR.workspace.reconfigure();
}

// Coalesce many state mutations within one tick into a single diffed broadcast.
function scheduleWorkspaceBroadcast() {
  // Protocol state must be staged before the current runtime response is
  // written; otherwise key bindings/Waybar activations only reach external
  // bars on a later, unrelated runtime request.
  reconfigureProtocolWorkspaces();
  if (workspaceBroadcastQueued) {
    return;
  }
  workspaceBroadcastQueued = true;
  void Promise.resolve().then(() => {
    workspaceBroadcastQueued = false;
    broadcastWorkspaces();
  });
}

// Workspaces a bar asked to create (by number). The view lists only
// workspaces with windows plus the active one, so these are listed as well
// until they show up there on their own.
const requestedWorkspaces = new Map<string, Set<number>>();

COMPOSITOR.workspace.configure(() => {
  const view = HYBRID_WINDOW_MANAGER.viewForIpc();
  return {
    groups: view.monitors.map((monitor) => {
      const listed = new Set(monitor.workspaces.map((workspace) => workspace.index));
      const requested = requestedWorkspaces.get(monitor.name) ?? new Set();
      for (const index of requested) {
        if (listed.has(index)) {
          requested.delete(index);
        }
      }
      const workspaces = [
        ...monitor.workspaces,
        ...[...requested].map((index) => ({ index, active: false, windowCount: 0 })),
      ].sort((a, b) => a.index - b.index);
      return {
        id: monitor.name,
        outputs: [monitor.name],
        workspaces: workspaces.map((workspace) => ({
          id: `${monitor.name}:${workspace.index}`,
          name: String(workspace.index),
          coordinates: [Math.max(0, workspace.index - 1)],
          active: workspace.active,
          hidden:
            !workspace.active &&
            workspace.windowCount === 0 &&
            !requested.has(workspace.index),
        })),
      };
    }),
  };
});

// ext-workspace `create_workspace`: the name is the workspace number.
COMPOSITOR.workspace.event.onCreate((event) => {
  const index = Number(event.name);
  if (!event.groupId || !Number.isInteger(index) || index < 1) {
    return;
  }
  const requested = requestedWorkspaces.get(event.groupId) ?? new Set<number>();
  requested.add(index);
  requestedWorkspaces.set(event.groupId, requested);
  scheduleWorkspaceBroadcast();
});

COMPOSITOR.workspace.event.onActivate((event) => {
  const [monitor, rawIndex] = event.workspaceId.split(":");
  const index = Number(rawIndex);
  if (!monitor || !Number.isInteger(index) || index < 1) {
    return;
  }
  HYBRID_WINDOW_MANAGER.activate(monitor, index);
  scheduleWorkspaceBroadcast();
});

// The color scheme of the frames, sent by shoji-bar-4 (./md3e/theme.ts).
WORKSPACE_IPC.handle("theme.set", (params) => {
  const scheme = schemeFrom(params);
  if (scheme) {
    setTheme(scheme);
  }
});

WORKSPACE_IPC.handle("workspaces.get", () =>
  HYBRID_WINDOW_MANAGER.viewForIpc(),
);
WORKSPACE_IPC.handle("workspaces.switch", (params) => {
  const direction = (params as { direction?: number } | undefined)?.direction;
  HYBRID_WINDOW_MANAGER.switchWorkspace(direction === -1 ? -1 : 1);
  scheduleWorkspaceBroadcast();
});
WORKSPACE_IPC.handle("workspaces.activate", (params) => {
  const request = params as { monitor?: string; index?: number } | undefined;
  if (request?.monitor && typeof request.index === "number") {
    HYBRID_WINDOW_MANAGER.activate(request.monitor, request.index);
    scheduleWorkspaceBroadcast();
  }
});
WORKSPACE_IPC.handle("workspaces.toggleTiling", (params) => {
  const monitor = (params as { monitor?: string } | undefined)?.monitor;
  if (monitor) {
    HYBRID_WINDOW_MANAGER.toggleWorkspaceTilingForMonitor(monitor);
  } else {
    HYBRID_WINDOW_MANAGER.toggleCurrentWorkspaceTiling();
  }
  scheduleWorkspaceBroadcast();
});
WORKSPACE_IPC.handle("windows.activate", (params) => {
  const windowId = (params as { windowId?: string } | undefined)?.windowId;
  if (typeof windowId === "string") {
    HYBRID_WINDOW_MANAGER.activateWindowById(windowId);
    scheduleWorkspaceBroadcast();
  }
});

// ---------------------------------------------------------------------------
// Dock proximity: watch the pointer and broadcast enter/leave for the bottom
// strip of each monitor. The bar uses this in place of a layer-shell trigger
// surface (which would otherwise capture clicks meant for the windows below).
// ---------------------------------------------------------------------------
// Two thresholds with hysteresis:
//   - SHOW: pointer must be in the bottom 10px to trigger reveal
//   - HIDE: once visible, pointer must leave the bottom 120px to dismiss
// This gives a precise "reach for the dock" trigger while keeping the dock
// stable once the user is interacting with it (so brushing the cursor a few
// dozen pixels above the dock body does not flicker it away).
const DOCK_SHOW_ZONE_PX = 10;
const DOCK_HIDE_ZONE_PX = 120;
const dockProximityByMonitor = new Map<string, boolean>();

function pointerInBottomStrip(
  monitor: string,
  pointerX: number,
  pointerY: number,
  stripPx: number,
): boolean {
  const output = COMPOSITOR.output.get(monitor);
  if (!output || !output.resolution) {
    return false;
  }
  const width = output.resolution.width / output.scale;
  const height = output.resolution.height / output.scale;
  const left = output.position.x;
  const top = output.position.y;
  const right = left + width;
  const bottom = top + height;
  return (
    pointerX >= left &&
    pointerX < right &&
    pointerY >= bottom - stripPx &&
    pointerY < bottom
  );
}

function nextDockProximity(
  monitor: string,
  pointerX: number,
  pointerY: number,
  onTrackedMonitor: boolean,
): boolean {
  if (!onTrackedMonitor) return false;
  const wasInside = dockProximityByMonitor.get(monitor) === true;
  // While outside, only the narrow show-zone counts (10px).
  // While inside, the wide hide-zone keeps it open (120px).
  return pointerInBottomStrip(
    monitor,
    pointerX,
    pointerY,
    wasInside ? DOCK_HIDE_ZONE_PX : DOCK_SHOW_ZONE_PX,
  );
}

function updateDockProximity(monitor: string, inside: boolean) {
  if (dockProximityByMonitor.get(monitor) === inside) {
    return;
  }
  dockProximityByMonitor.set(monitor, inside);
  WORKSPACE_IPC.broadcast("dock.proximity", { monitor, inside });
}

// Snap-zone preview: broadcast the active snap rect (floating edge zones, or the
// opened tiling slot) to the bar, which renders the rounded preview overlay.
//   snap.preview  { monitor, rect: {x,y,w,h} | null, kind: "floating"|"tiling" }
let lastSnapJson = "";
HYBRID_WINDOW_MANAGER.setSnapPreviewBroadcaster((preview) => {
  const json = JSON.stringify(preview);
  if (json === lastSnapJson) {
    return;
  }
  lastSnapJson = json;
  WORKSPACE_IPC.broadcast("snap.preview", preview);
});

HYBRID_WINDOW_MANAGER.setWorkspaceChangeBroadcaster(() => {
  scheduleWorkspaceBroadcast();
});

COMPOSITOR.onDisable(() => {
  WORKSPACE_IPC.close();
});

COMPOSITOR.process.once("fcitx5", {
  command: "fcitx5 -d",
  runPolicy: "once-per-session",
});

// The session's shell: shoji-bar-4 (~/.config/shoji-bar-4), the bar, dock,
// panels, launcher, notifications and wallpaper, in the same Material 3
// Expressive style. It sends its color scheme here (`theme.set`).
const SHELL = "~/.config/shoji-bar-4/target/release/shoji-bar-4";
COMPOSITOR.process.once("shell", {
  command: `exec ${SHELL}`,
  runPolicy: "once-per-session",
});
// cliphist clipboard history watchers. Text and image need separate watchers;
// run as services so they are restarted if they ever exit.
COMPOSITOR.process.service("cliphist-text", {
  command: ["wl-paste", "--type", "text", "--watch", "cliphist", "store"],
  restart: "on-exit",
});
COMPOSITOR.process.service("cliphist-image", {
  command: ["wl-paste", "--type", "image", "--watch", "cliphist", "store"],
  restart: "on-exit",
});

COMPOSITOR.key.bind("terminal", "Super+T", () => {
  COMPOSITOR.process.spawn({ command: ["kitty"] });
});

// if kwallet6 is used as the password store, be sure to add the --password-store=kwallet6 flag
COMPOSITOR.key.bind("chrome", "Super+B", () => {
  COMPOSITOR.process.spawn({
    command:
      "google-chrome-stable --enable-features=OzonePlatform --ozone-platform=wayland",
  });
});

COMPOSITOR.key.bind("discord", "Super+D", () => {
  COMPOSITOR.process.spawn({
    command:
      "discord --enable-features=UseOzonePlatform --ozone-platform=wayland --enable-wayland-ime --disable-gpu",
  });
});

COMPOSITOR.key.bind("dolphin", "Super+E", () => {
  COMPOSITOR.process.spawn({ command: "dolphin" });
});

COMPOSITOR.key.bind("play", "XF86AudioPlay", () => {
  COMPOSITOR.process.spawn({ command: "playerctl play-pause" });
});
COMPOSITOR.key.bind("pause", "XF86AudioPause", () => {
  COMPOSITOR.process.spawn({ command: "playerctl play-pause" });
});
COMPOSITOR.key.bind("next", "XF86AudioNext", () => {
  COMPOSITOR.process.spawn({ command: "playerctl next" });
});
COMPOSITOR.key.bind("prev", "XF86AudioPrev", () => {
  COMPOSITOR.process.spawn({ command: "playerctl previous" });
});

// shoji-bar-4's launcher and panels, on the monitor under the cursor
// (`shoji-bar-4 msg ...`, see ~/.config/shoji-bar-4/src/ipc.rs).
function toggleOnCurrentMonitor(panel: string) {
  const monitor = HYBRID_WINDOW_MANAGER.getCurrentMonitorName();
  COMPOSITOR.process.spawn({ command: `${SHELL} msg toggle ${panel} "${monitor}"` });
}
COMPOSITOR.key.bind("launcher", "Super+A", () => toggleOnCurrentMonitor("launcher"));
// Super tap: fires on a quick release (within 300 ms) with no other key/button pressed in between.
COMPOSITOR.key.bind("launcher-tap", "Super", () => toggleOnCurrentMonitor("launcher"), {
  on: "release",
  within: 300,
});
COMPOSITOR.key.bind("quick-settings", "Super+N", () => toggleOnCurrentMonitor("quick-settings"));
COMPOSITOR.key.bind("calendar", "Super+C", () => toggleOnCurrentMonitor("calendar"));
// The clipboard's history, as the cliphist watchers above keep it.
COMPOSITOR.key.bind("clipboard", "Super+V", () => toggleOnCurrentMonitor("clipboard"));
// Lock the session. No monitor argument: a lock covers every screen there is,
// and the shell refuses to expose an unlock over the same socket.
COMPOSITOR.key.bind("lock", "Super+L", () => {
  COMPOSITOR.process.spawn({ command: `${SHELL} msg lock` });
});

COMPOSITOR.key.bind("screenshot", "Super+P", () => {
  COMPOSITOR.process.spawn({
    command: "hyprshot -m region --raw | swappy -f -",
  });
});
COMPOSITOR.key.bind("screenshot-freeze", "Super+Ctrl+P", () => {
  COMPOSITOR.process.spawn({
    command: "hyprshot -m region --freeze --raw | swappy -f -",
  });
});
COMPOSITOR.key.bind("toggle-tiling-mode", "Super+S", () => {
  HYBRID_WINDOW_MANAGER.toggleCurrentWorkspaceTiling();
  scheduleWorkspaceBroadcast();
});
COMPOSITOR.key.bind("window-switcher", "Super+Tab", () => {
  WINDOW_SWITCHER.open(HYBRID_WINDOW_MANAGER.getCurrentMonitorName());
});
COMPOSITOR.key.bind("close-focused-window", "Super+Q", () => {
  HYBRID_WINDOW_MANAGER.closeFocusedWindow();
});
COMPOSITOR.key.bind("toggle-focused-window-maximize", "Super+M", () => {
  HYBRID_WINDOW_MANAGER.toggleFocusedWindowMaximize();
});
COMPOSITOR.key.bind("toggle-focused-window-fullscreen", "Super+F", () => {
  HYBRID_WINDOW_MANAGER.toggleFocusedWindowFullscreen();
});
COMPOSITOR.key.bind("tile-focus-left-quick", "Super+Left", () => {
  HYBRID_WINDOW_MANAGER.focusTile(-1);
});
COMPOSITOR.key.bind("tile-focus-right-quick", "Super+Right", () => {
  HYBRID_WINDOW_MANAGER.focusTile(1);
});
COMPOSITOR.key.bind("tile-focus-left", "Super+Ctrl+Left", () => {
  HYBRID_WINDOW_MANAGER.focusTile(-1);
});
COMPOSITOR.key.bind("tile-focus-right", "Super+Ctrl+Right", () => {
  HYBRID_WINDOW_MANAGER.focusTile(1);
});
COMPOSITOR.key.bind("tile-move-left", "Super+Shift+Left", () => {
  HYBRID_WINDOW_MANAGER.moveFocusedTile(-1);
  scheduleWorkspaceBroadcast();
});
COMPOSITOR.key.bind("tile-move-right", "Super+Shift+Right", () => {
  HYBRID_WINDOW_MANAGER.moveFocusedTile(1);
  scheduleWorkspaceBroadcast();
});
COMPOSITOR.key.bind("window-move-workspace-prev", "Super+Shift+Up", () => {
  HYBRID_WINDOW_MANAGER.moveFocusedWindowToWorkspace(-1);
  scheduleWorkspaceBroadcast();
});
COMPOSITOR.key.bind("window-move-workspace-next", "Super+Shift+Down", () => {
  HYBRID_WINDOW_MANAGER.moveFocusedWindowToWorkspace(1);
  scheduleWorkspaceBroadcast();
});
COMPOSITOR.key.bind("workspace-prev", "Super+Ctrl+Up", () => {
  HYBRID_WINDOW_MANAGER.switchWorkspace(-1);
  scheduleWorkspaceBroadcast();
});
COMPOSITOR.key.bind("workspace-next", "Super+Ctrl+Down", () => {
  HYBRID_WINDOW_MANAGER.switchWorkspace(1);
  scheduleWorkspaceBroadcast();
});

let fpsCounter = false;
COMPOSITOR.key.bind("fps", "Super+Shift+F", () => {
  fpsCounter = !fpsCounter;
  COMPOSITOR.debug.fpsCounter = fpsCounter;
});

let profileEnabled = false;
COMPOSITOR.key.bind("profile", "Super+Shift+T", () => {
  profileEnabled = !profileEnabled;
  COMPOSITOR.debug.enableProfile(profileEnabled);
});

COMPOSITOR.output.configure((context) => {
  const display: DisplayConfigDraft = {};

  display["eDP-1"] = {
    mode: "extend",
    resolution: "best",
    position: "auto",
    scale: 1.8,
    transform: "normal",
    hdr: false,
  };
  display["eDP-2"] = {
    mode: "extend",
    resolution: "best",
    position: "auto",
    scale: 1.8,
    hdr: false,
  };
  display["HDMI-A-1"] = {
    mode: "extend",
    resolution: "best",
    position: "auto",
    scale: 1.5,
  };
  display["DP-1"] = {
    mode: "extend",
    resolution: "best",
    position: "auto",
    scale: 1.5,
  };
  display["DP-4"] = {
    mode: "extend",
    resolution: "best",
    position: "auto",
    scale: 1.5,
  };
  display["DP-2"] = {
    mode: "extend",
    resolution: "best",
    position: "auto",
    scale: 1.6,
  };

  const isDocked = context.connected.some(
    (output) => output.name === "HDMI-A-1",
  );
  if (isDocked) {
    display["eDP-1"] = { mode: "disabled" };
    display["eDP-2"] = { mode: "disabled" };
  }

  return display;
});

COMPOSITOR.input.configure((input, _context) => {
  input.global = {
    touchpad: {
      tapToClick: true,
      naturalScroll: true,
      scrollMethod: "twoFinger",
      disableWhileTyping: true,
      scrollFactor: 0.3,
    },
    pointer: {
      pointerAccel: 0.0,
      accelProfile: "flat",
      motionSpace: "logical",
    },
    keyboard: {
      options: "caps:ctrl_modifier",
      repeatRate: 60,
      repeatDelay: 250,
    },
  };

  input.device["Razer Razer Blade Keyboard"] = {
    keyboard: {
      layout: "us",
    },
  };
});

HYBRID_WINDOW_MANAGER.configureWorkspaceGestureSpeed({
  workspaceScrollFactor: 1.5,
  workspaceScrollKineticFactor: 1,
  workspaceSwitchFactor: 1,
  workspaceSwitchVelocityFactor: 1,
  // At or below this scroll speed (logical px/s) the workspace scroll
  // catches on tile snap positions (fully-on-screen edges; center for
  // maximized tiles). 0 disables snapping.
  workspaceScrollSnapMaxVelocity: 600,
  // Finger travel (logical px) needed to break out of a caught position.
  workspaceScrollSnapBreakoutPx: 48,
});

// Blur behind the regions clients ask for (ext-background-effect). Off while
// the window switcher is shown, then faded back in like the window backdrops.
const BACKGROUND_BLUR = compileEffect({
  input: backdropSource(),
  capturePadding: 24,
  invalidate: { kind: "on-source-damage-box", damagePadding: 8 },
  alpha: "preserve",
  pipeline: [dualKawaseBlur({ radius: 4, passes: 2 }), backdropFade()],
});
COMPOSITOR.effect.background_effect = computed(() =>
  WINDOW_SWITCHER.blurSuspended() ? null : BACKGROUND_BLUR,
);

const LAYER_BLUR_MASK = compileLayerEffect({
  input: backdropSource(),
  capturePadding: 24,
  invalidate: { kind: "on-source-damage-box", damagePadding: 8 },
  // The mask stage intentionally outputs transparency (the blur is clipped
  // to the layer's own alpha), so the pipeline's alpha must survive the
  // finish/display passes instead of being forced opaque.
  alpha: "preserve",
  pipeline: [
    dualKawaseBlur({ radius: 4, passes: 2 }),
    shaderStage(loadShader("./src/effect/layer-blur-mask.frag"), {
      textures: {
        layer_mask: layerSource(),
      },
      uniforms: {
        opacity_threshold: 0.25,
        mask_feather: 0.04,
      },
    }),
  ],
});

COMPOSITOR.effect.layer = (layer) => {
  const namespace = layer.namespace();
  // Nothing is behind the wallpaper.
  if (namespace === "shoji-bar-4-wallpaper" || namespace === "no_blur") {
    return {};
  }

  return {
    behind: LAYER_BLUR_MASK,
  };
};

const POPUP_BLUR = compilePopupEffect({
  input: backdropSource(),
  capturePadding: 4 * 2 * 2 + 24 + 32,
  invalidate: { kind: "on-source-damage-box", damagePadding: 8 },
  // The mask stage intentionally outputs transparency (the blur is clipped
  // to the layer's own alpha), so the pipeline's alpha must survive the
  // finish/display passes instead of being forced opaque.
  alpha: "preserve",
  pipeline: [
    dualKawaseBlur({ radius: 4, passes: 2 }),
    shaderStage(loadShader("./src/effect/layer-blur-mask.frag"), {
      textures: {
        layer_mask: popupSource(),
      },
      uniforms: {
        opacity_threshold: 0.25,
        mask_feather: 0.04,
      },
    }),
  ],
});

COMPOSITOR.effect.popup = (popup) => {
  if (popup.parentKind === "window") {
    return {};
  }

  return {
    behind: POPUP_BLUR,
  };
};

// Chromium-family clients repaint their CSD shadow margins as transparent
// black — while still declaring the whole surface opaque — the moment they
// send set_minimized, assuming the surface will never be shown again. Honoring
// that declaration skips blending and paints the margins as a solid black
// ring during the minimize animation.
const isChromiumFamily = (appId: string): boolean => {
  const id = appId.toLowerCase();
  return (
    id.includes("chrome") || id.includes("chromium") || id.includes("electron")
  );
};

// GTK3 tooltips (waybar) declare their whole rect opaque despite transparent
// rounded corners, which paints the corners as a solid fill and culls the
// behind-blur. Ignore the declaration for layer-shell popups.
COMPOSITOR.rendering.surfacePolicy = (surface) => {
  if (surface.kind === "popup" && surface.parentKind === "layer") {
    return { opaqueRegion: "ignore" };
  }
  if (surface.kind === "toplevel") {
    const window = surface.window;
    // Minimized only: the restore animation fades in from opacity 0, so the
    // few frames where a stale black-margin buffer could still be on screen
    // after unminimize are effectively invisible.
    if (
      isChromiumFamily(window.appId() ?? "") &&
      (window.state[WINDOW_STATE_MINIMIZED]() ||
        window.state[WINDOW_STATE_MINIMIZE_VISUAL_IDLE]())
    ) {
      return { opaqueRegion: "ignore" };
    }
  }
  return null;
};

COMPOSITOR.event.onOpen((window) => {
  HYBRID_WINDOW_MANAGER.onOpen(window);
});

COMPOSITOR.event.onInitialConfigure((window) => {
  HYBRID_WINDOW_MANAGER.onInitialConfigure(window);
});

COMPOSITOR.event.onFirstCommit((window) => {
  HYBRID_WINDOW_MANAGER.onFirstCommit(window);
  scheduleWorkspaceBroadcast();
});

COMPOSITOR.event.onStartClose((window) => {
  HYBRID_WINDOW_MANAGER.onStartClose(window);
  scheduleWorkspaceBroadcast();
});

COMPOSITOR.event.onClose((window) => {
  HYBRID_WINDOW_MANAGER.onClose(window);
  scheduleWorkspaceBroadcast();
});

COMPOSITOR.event.onFocus((window, focused) => {
  HYBRID_WINDOW_MANAGER.onFocus(window, focused);
  if (focused) {
    HYBRID_WINDOW_MANAGER.recordFocus(window.id);
    scheduleWorkspaceBroadcast();
  }
});

COMPOSITOR.event.onPointerMove((event) => {
  HYBRID_WINDOW_MANAGER.onPointerMove(event);

  // Dock proximity: update only the monitor the pointer is currently on,
  // and emit "leave" for other monitors that were previously inside. The
  // narrow/wide threshold is hysteretic per current state.
  const pointerX = event.position.x;
  const pointerY = event.position.y;
  for (const monitor of COMPOSITOR.output.list) {
    const inside = nextDockProximity(
      monitor,
      pointerX,
      pointerY,
      monitor === event.outputName,
    );
    updateDockProximity(monitor, inside);
  }
});

COMPOSITOR.event.onGestureSwipe((event) => {
  HYBRID_WINDOW_MANAGER.onGestureSwipe(event);
  scheduleWorkspaceBroadcast();
});

COMPOSITOR.event.onOutputChange((event) => {
  HYBRID_WINDOW_MANAGER.onOutputChange(event);
  scheduleWorkspaceBroadcast();
});

COMPOSITOR.event.onCreateLayer(() => {
  HYBRID_WINDOW_MANAGER.refreshUsableAreaLayouts();
});

COMPOSITOR.event.onUpdateLayer(() => {
  HYBRID_WINDOW_MANAGER.refreshUsableAreaLayouts();
});

COMPOSITOR.event.onDestroyLayer(() => {
  HYBRID_WINDOW_MANAGER.refreshUsableAreaLayouts();
});

COMPOSITOR.event.onWindowResize((event) => {
  HYBRID_WINDOW_MANAGER.onWindowResize(event);
});

COMPOSITOR.pointer.bindWindowMoveModifier("Super");
COMPOSITOR.pointer.bindWindowResizeModifier("Super");
COMPOSITOR.cursor.bindAppLockIgnoreModifier("Super");

COMPOSITOR.event.onWindowMove((event) => {
  HYBRID_WINDOW_MANAGER.onWindowMove(event);
});

COMPOSITOR.event.onWindowMaximizeRequest((event) => {
  HYBRID_WINDOW_MANAGER.onWindowMaximizeRequest(event);
});

COMPOSITOR.event.onWindowMinimizeRequest((event) => {
  HYBRID_WINDOW_MANAGER.onWindowMinimizeRequest(event);
});

COMPOSITOR.event.onWindowFullscreenRequest((event) => {
  HYBRID_WINDOW_MANAGER.onWindowFullscreenRequest(event);
});

COMPOSITOR.event.onWindowActivateRequest((event) => {
  HYBRID_WINDOW_MANAGER.onWindowActivateRequest(event);
  scheduleWorkspaceBroadcast();
});

COMPOSITOR.window.composition = createWindowComposition(
  HYBRID_WINDOW_MANAGER,
  WINDOW_SWITCHER,
);

export default COMPOSITOR;
