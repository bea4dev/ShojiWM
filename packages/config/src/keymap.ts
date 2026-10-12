/**
 * Every key binding of the session, in one table.
 *
 * The table binds the keys, and the same table feeds the cheat sheet
 * (Super+/, ./helpers/key-sheet.py) and the `shoji-keys` command
 * (./helpers/shoji-keys.py). So the three never disagree.
 *
 *   createKeymap(deps) ──> bindKeymap()       COMPOSITOR.key.bind for each chord
 *                     ├──> keymapRows()      rows for "keys.list" over IPC
 *                     └──> writeKeymapFile() $XDG_RUNTIME_DIR/shojiwm-keys.json
 *
 * The idea of the layout:
 *
 * - Super + a letter: an app or a tool (T terminal, B browser, Q close, ...).
 * - Super + arrows: the focused window. Tiling: move focus between tiles.
 *   Floating: put the window in a half of the screen (KDE's quick tile).
 * - Super + Shift + arrows: move the window (tile order, or a half).
 * - Super + Ctrl + arrows: workspaces. Add Shift to take the window along.
 * - Super + number: workspace N. Add Shift to send the window there.
 */
import { COMPOSITOR } from "shoji_wm";
import type { HybridWindowManager } from "./window-manager";
import type { WindowSwitcher } from "./window-switcher";

/** The groups of the cheat sheet, in the order it shows them. */
export const KEY_GROUPS = [
  "Apps",
  "Windows",
  "Tiling",
  "Floating",
  "Workspaces",
  "Switchers",
  "Screenshots",
  "Session",
  "Debug",
] as const;
export type KeyGroup = (typeof KEY_GROUPS)[number];

interface KeyBindOptions {
  on: "release";
  within: number;
}

/** One chord and what it does. */
interface KeyBind {
  keys: string;
  action: () => void;
  options?: KeyBindOptions;
  /** The binding's name (default: from the entry id). Keeps upstream's names. */
  name?: string;
}

/** One line of the cheat sheet: one or more chords with one meaning. */
export interface KeyEntry {
  id: string;
  group: KeyGroup;
  binds: KeyBind[];
  /** How the sheet writes the keys. Default: the chords joined by " / ". */
  label?: string;
  description: string;
  /**
   * The keys do different things in a tiled and a floating workspace. The
   * sheet then shows the entry under "Tiling" with `tiling`, and under
   * "Floating" with `floating`; `group` and `description` are not shown.
   */
  tiling?: string;
  floating?: string;
  /** Bound, but not in the sheet or `shoji-keys` (the KDE preview keys). */
  hidden?: boolean;
}

/** A line as the sheet and `shoji-keys` read it. */
export interface KeyRow {
  group: KeyGroup;
  keys: string;
  description: string;
}

export interface KeymapDeps {
  windowManager: HybridWindowManager;
  /** Alt+Tab (Coverflow). */
  switcher: WindowSwitcher;
  /** Super+Tab. */
  grid: WindowSwitcher;
  /** KDE's Window View: the windows of one app. */
  windowView: WindowSwitcher;
  /** A switcher is open. */
  switcherOpen: () => boolean;
  /** Tell the taskbar and the Pager about the workspaces. */
  broadcastWorkspaces: () => void;
  /** Open or close Kickoff. */
  toggleLauncher: () => void;
  /** Open or close the cheat sheet. */
  toggleKeySheet: () => void;
  lockCommand: string;
  /** The session runs as a window inside KDE (a preview). */
  insideKde: boolean;
}

export function createKeymap(deps: KeymapDeps): KeyEntry[] {
  const wm = deps.windowManager;
  const monitor = () => wm.getCurrentMonitorName();
  const tiled = () => wm.isCurrentWorkspaceTiled();
  const spawn = (command: string | string[]) => () => {
    COMPOSITOR.process.spawn({ command });
  };
  // A key that does one thing in a tiled workspace and another when floating.
  const byMode = (tiling: () => void, floating: () => void) => () => {
    if (tiled()) {
      tiling();
    } else {
      floating();
    }
  };
  const focusedWindow = () => wm.listWindows().find((window) => window.isFocused.peek());
  const goToWorkspace = (n: number) => () => {
    wm.activate(monitor(), n);
    deps.broadcastWorkspaces();
  };
  const sendToWorkspace = (n: number) => () => {
    const focused = focusedWindow();
    if (focused) {
      wm.moveWindowToWorkspace(focused, monitor(), n);
      deps.broadcastWorkspaces();
    }
  };
  const switchWorkspace = (direction: -1 | 1) => () => {
    wm.switchWorkspace(direction);
    deps.broadcastWorkspaces();
  };
  const moveToWorkspace = (direction: -1 | 1) => () => {
    wm.moveFocusedWindowToWorkspace(direction);
    deps.broadcastWorkspaces();
  };
  const changesLayout = (action: () => void) => () => {
    action();
    deps.broadcastWorkspaces();
  };
  // The side-by-side Window View of the app you used last (every window of
  // that app), the same view as a click on its taskbar group.
  const openWindowViewOfLastApp = () => {
    if (deps.switcherOpen()) {
      return;
    }
    const recent = wm.listWindowsByRecentUse();
    const appId = recent[0]?.appId.peek();
    if (!appId) {
      return;
    }
    deps.windowView.open(
      monitor(),
      recent.filter((window) => window.appId.peek() === appId),
    );
  };
  const numbers = Array.from({ length: 9 }, (_, i) => i + 1);

  const entries: KeyEntry[] = [
    // Apps
    {
      id: "launcher",
      group: "Apps",
      label: "Super / Super+A",
      description: "Start menu (Kickoff)",
      binds: [
        // A tap: a quick release (300 ms) with no other key or button between.
        { keys: "Super", action: deps.toggleLauncher, options: { on: "release", within: 300 } },
        { keys: "Super+A", action: deps.toggleLauncher },
      ],
    },
    { id: "terminal", group: "Apps", description: "Terminal (Konsole)", binds: [{ keys: "Super+T", action: spawn(["konsole"]) }] },
    {
      id: "chrome",
      group: "Apps",
      description: "Browser (Chrome)",
      binds: [
        {
          keys: "Super+B",
          action: spawn("google-chrome-stable --enable-features=OzonePlatform --ozone-platform=wayland"),
        },
      ],
    },
    {
      id: "discord",
      group: "Apps",
      description: "Discord",
      binds: [
        {
          keys: "Super+D",
          action: spawn(
            "discord --enable-features=UseOzonePlatform --ozone-platform=wayland --enable-wayland-ime --disable-gpu",
          ),
        },
      ],
    },
    { id: "dolphin", group: "Apps", description: "Files (Dolphin)", binds: [{ keys: "Super+E", action: spawn("dolphin") }] },
    {
      id: "clipboard",
      group: "Apps",
      description: "Clipboard history",
      binds: [{ keys: "Super+V", action: spawn("qdbus6 org.kde.klipper /klipper showKlipperPopupMenu") }],
    },
    { id: "settings", group: "Apps", description: "System Settings", binds: [{ keys: "Super+I", action: spawn("systemsettings") }] },

    // Windows
    { id: "close", group: "Windows", description: "Close the window", binds: [{ keys: "Super+Q", action: () => wm.closeFocusedWindow() }] },
    {
      id: "minimize",
      group: "Windows",
      description: "Minimize (Magic Lamp)",
      binds: [{ keys: "Super+H", action: () => focusedWindow()?.minimize() }],
    },
    {
      id: "maximize",
      group: "Windows",
      description: "Maximize / restore",
      binds: [{ keys: "Super+M", action: () => wm.toggleFocusedWindowMaximize() }],
    },
    {
      id: "fullscreen",
      group: "Windows",
      description: "Full screen",
      binds: [{ keys: "Super+F", action: () => wm.toggleFocusedWindowFullscreen() }],
    },
    {
      id: "toggle-tiling",
      group: "Windows",
      description: "Tiling on / off for this workspace",
      binds: [{ keys: "Super+S", action: changesLayout(() => wm.toggleCurrentWorkspaceTiling()) }],
    },

    // Arrow keys: tiling and floating
    {
      id: "arrows-left-right",
      group: "Tiling",
      label: "Super+Left / Right",
      description: "",
      tiling: "Focus the tile on the left / right",
      floating: "Left / right half of the screen",
      binds: [
        {
          keys: "Super+Left",
          name: "tile-focus-left-quick",
          action: byMode(() => wm.focusTile(-1), () => wm.placeFocusedWindowInHalf("left")),
        },
        {
          keys: "Super+Right",
          name: "tile-focus-right-quick",
          action: byMode(() => wm.focusTile(1), () => wm.placeFocusedWindowInHalf("right")),
        },
      ],
    },
    {
      id: "arrow-up",
      group: "Tiling",
      label: "Super+Up",
      description: "",
      tiling: "Focus the tile above (in a stack)",
      floating: "Maximize / restore",
      binds: [{ keys: "Super+Up", action: byMode(() => wm.focusInColumn(-1), () => wm.toggleFocusedWindowMaximize()) }],
    },
    {
      id: "arrow-down",
      group: "Tiling",
      label: "Super+Down",
      description: "",
      tiling: "Focus the tile below (in a stack)",
      floating: "Bottom half; again: top half",
      binds: [
        {
          keys: "Super+Down",
          action: byMode(() => wm.focusInColumn(1), () => wm.placeFocusedWindowInHalf("bottom-then-top")),
        },
      ],
    },
    {
      id: "shift-arrows-left-right",
      group: "Tiling",
      label: "Super+Shift+Left / Right",
      description: "",
      tiling: "Move the tile left / right",
      floating: "Left / right half of the screen",
      binds: [
        {
          keys: "Super+Shift+Left",
          action: byMode(changesLayout(() => wm.moveFocusedTile(-1)), () => wm.placeFocusedWindowInHalf("left")),
        },
        {
          keys: "Super+Shift+Right",
          action: byMode(changesLayout(() => wm.moveFocusedTile(1)), () => wm.placeFocusedWindowInHalf("right")),
        },
      ],
    },
    {
      id: "shift-arrows-up-down",
      group: "Tiling",
      label: "Super+Shift+Up / Down",
      description: "",
      tiling: "Move the tile up / down in its stack",
      floating: "Top / bottom half of the screen",
      binds: [
        {
          keys: "Super+Shift+Up",
          action: byMode(changesLayout(() => wm.moveFocusedInColumn(-1)), () => wm.placeFocusedWindowInHalf("top")),
        },
        {
          keys: "Super+Shift+Down",
          action: byMode(changesLayout(() => wm.moveFocusedInColumn(1)), () => wm.placeFocusedWindowInHalf("bottom")),
        },
      ],
    },

    // Tiling only
    {
      id: "stack",
      group: "Tiling",
      label: "Super+Comma",
      description: "Stack the tile under the column on its left",
      binds: [{ keys: "Super+Comma", action: changesLayout(() => wm.stackFocusedIntoLeftColumn()) }],
    },
    {
      id: "unstack",
      group: "Tiling",
      label: "Super+Period",
      description: "Take the tile out of its stack",
      binds: [{ keys: "Super+Period", action: changesLayout(() => wm.unstackFocusedWindow()) }],
    },
    {
      id: "tile-scroll",
      group: "Tiling",
      label: "Super+Wheel",
      description: "Scroll along the tiles",
      binds: [
        { keys: "Super+WheelScrollUp", action: () => wm.focusTile(-1) },
        { keys: "Super+WheelScrollDown", action: () => wm.focusTile(1) },
      ],
    },

    // Workspaces
    {
      id: "workspace",
      group: "Workspaces",
      label: "Super+1..9 / Super+F1..F9 / Ctrl+F1..F9",
      description: "Go to workspace N",
      binds: numbers.flatMap((n) => [
        { keys: `Super+${n}`, action: goToWorkspace(n) },
        { keys: `Super+F${n}`, action: goToWorkspace(n) },
        { keys: `Ctrl+F${n}`, action: goToWorkspace(n) },
      ]),
    },
    {
      id: "window-to-workspace",
      group: "Workspaces",
      label: "Super+Shift+1..9",
      description: "Send the window to workspace N",
      binds: numbers.map((n) => ({ keys: `Super+Shift+${n}`, action: sendToWorkspace(n) })),
    },
    {
      id: "workspace-prev-next",
      group: "Workspaces",
      label: "Super+Ctrl+Arrows",
      description: "Previous / next workspace",
      binds: [
        { keys: "Super+Ctrl+Up", action: switchWorkspace(-1) },
        { keys: "Super+Ctrl+Left", action: switchWorkspace(-1) },
        { keys: "Super+Ctrl+Down", action: switchWorkspace(1) },
        { keys: "Super+Ctrl+Right", action: switchWorkspace(1) },
      ],
    },
    {
      id: "window-workspace-prev-next",
      group: "Workspaces",
      label: "Super+Ctrl+Shift+Up / Down",
      description: "Take the window to the previous / next workspace",
      binds: [
        { keys: "Super+Ctrl+Shift+Up", action: moveToWorkspace(-1) },
        { keys: "Super+Ctrl+Shift+Down", action: moveToWorkspace(1) },
      ],
    },

    // Switchers
    {
      id: "alt-tab",
      group: "Switchers",
      description: "Switch windows (Coverflow); Q closes one",
      binds: [{ keys: "Alt+Tab", action: () => deps.switcher.open(monitor()) }],
    },
    {
      id: "window-grid",
      group: "Switchers",
      description: "All windows in a grid",
      binds: [{ keys: "Super+Tab", action: () => deps.grid.open(monitor()) }],
    },
    {
      id: "window-view",
      group: "Switchers",
      description: "All windows of the last app",
      binds: [{ keys: "Super+W", action: openWindowViewOfLastApp }],
    },

    // Screenshots (Spectacle, through ./helpers/screenshot-dbus.py)
    { id: "screenshot", group: "Screenshots", description: "Spectacle", binds: [{ keys: "Print", action: spawn("spectacle") }] },
    {
      id: "screenshot-window",
      group: "Screenshots",
      description: "The active window",
      binds: [{ keys: "Super+Print", action: spawn("spectacle -a") }],
    },
    {
      id: "screenshot-full",
      group: "Screenshots",
      description: "The full screen",
      binds: [{ keys: "Shift+Print", action: spawn("spectacle -f") }],
    },
    {
      id: "screenshot-region",
      group: "Screenshots",
      label: "Super+Shift+S / Super+Shift+Print",
      description: "A region",
      binds: [
        { keys: "Super+Shift+S", action: spawn("spectacle -r") },
        { keys: "Super+Shift+Print", action: spawn("spectacle -r") },
      ],
    },
    {
      id: "screenshot-edit",
      group: "Screenshots",
      label: "Super+P / Super+Ctrl+P",
      description: "A region into the editor (Ctrl: freeze first)",
      binds: [
        { keys: "Super+P", action: spawn("hyprshot -m region --raw | swappy -f -") },
        { keys: "Super+Ctrl+P", action: spawn("hyprshot -m region --freeze --raw | swappy -f -") },
      ],
    },

    // Session
    {
      id: "key-sheet",
      group: "Session",
      label: "Super+/",
      description: "This cheat sheet",
      binds: [
        { keys: "Super+Slash", action: deps.toggleKeySheet },
        { keys: "Super+Shift+Slash", action: deps.toggleKeySheet },
      ],
    },
    { id: "lock", group: "Session", description: "Lock the screen", binds: [{ keys: "Super+L", action: spawn(deps.lockCommand) }] },
    {
      id: "logout",
      group: "Session",
      description: "Log out",
      // Plasma's own "Log Out" button needs KDE's session manager, which
      // does not run under ShojiWM.
      binds: [{ keys: "Ctrl+Alt+Delete", action: spawn("shoji_wm --quit") }],
    },
    {
      id: "media",
      group: "Session",
      label: "Media keys",
      description: "Play / pause, next, previous",
      binds: [
        { keys: "XF86AudioPlay", action: spawn("playerctl play-pause") },
        { keys: "XF86AudioPause", action: spawn("playerctl play-pause") },
        { keys: "XF86AudioNext", action: spawn("playerctl next") },
        { keys: "XF86AudioPrev", action: spawn("playerctl previous") },
      ],
    },

    // Debug
    { id: "fps", group: "Debug", description: "FPS counter on / off", binds: [{ keys: "Super+Shift+F", action: toggleFps }] },
    { id: "profile", group: "Debug", description: "Profiler on / off", binds: [{ keys: "Super+Shift+T", action: toggleProfile }] },
  ];

  // Alt copies of the main keys for the preview inside KDE: KDE takes every
  // Super key before the preview window sees it. Only inside KDE, so a real
  // session keeps Alt for the apps.
  if (deps.insideKde) {
    entries.push({
      id: "preview",
      group: "Debug",
      description: "Alt copies of the Super keys for the preview inside KDE",
      hidden: true,
      binds: [
        { keys: "Alt+T", action: spawn(["konsole"]) },
        { keys: "Alt+S", action: changesLayout(() => wm.toggleCurrentWorkspaceTiling()) },
        { keys: "Alt+W", action: () => deps.switcher.open(monitor()) },
        { keys: "Alt+Q", action: () => wm.closeFocusedWindow() },
        { keys: "Alt+M", action: () => wm.toggleFocusedWindowMaximize() },
        { keys: "Alt+Left", action: () => wm.focusTile(-1) },
        { keys: "Alt+Right", action: () => wm.focusTile(1) },
        { keys: "Alt+Shift+Left", action: changesLayout(() => wm.moveFocusedTile(-1)) },
        { keys: "Alt+Shift+Right", action: changesLayout(() => wm.moveFocusedTile(1)) },
        { keys: "Alt+Comma", action: changesLayout(() => wm.stackFocusedIntoLeftColumn()) },
        { keys: "Alt+Period", action: changesLayout(() => wm.unstackFocusedWindow()) },
        // Alt+Up and Alt+Down switch workspaces, so column focus takes Shift.
        { keys: "Alt+Shift+Up", action: () => wm.focusInColumn(-1) },
        { keys: "Alt+Shift+Down", action: () => wm.focusInColumn(1) },
        { keys: "Alt+WheelScrollUp", action: () => wm.focusTile(-1) },
        { keys: "Alt+WheelScrollDown", action: () => wm.focusTile(1) },
        { keys: "Alt+Up", action: switchWorkspace(-1) },
        { keys: "Alt+Down", action: switchWorkspace(1) },
      ],
    });
  }
  return entries;
}

let fpsCounter = false;
function toggleFps() {
  fpsCounter = !fpsCounter;
  COMPOSITOR.debug.fpsCounter = fpsCounter;
}

let profileEnabled = false;
function toggleProfile() {
  profileEnabled = !profileEnabled;
  COMPOSITOR.debug.enableProfile(profileEnabled);
}

/** Bind every chord of the table. A chord that is in the table twice is an error. */
export function bindKeymap(entries: readonly KeyEntry[]) {
  const seen = new Map<string, string>();
  for (const entry of entries) {
    entry.binds.forEach((bind, index) => {
      const chord = normalizeChord(bind.keys);
      const owner = seen.get(chord);
      if (owner) {
        throw new Error(`keymap: ${bind.keys} is in "${owner}" and in "${entry.id}"`);
      }
      seen.set(chord, entry.id);
      const id = bind.name ?? (entry.binds.length === 1 ? entry.id : `${entry.id}-${index}`);
      if (bind.options) {
        COMPOSITOR.key.bind(id, bind.keys, bind.action, bind.options);
      } else {
        COMPOSITOR.key.bind(id, bind.keys, bind.action);
      }
    });
  }
}

/** The lines of the cheat sheet. Keys with two meanings give two lines. */
export function keymapRows(entries: readonly KeyEntry[]): KeyRow[] {
  const rows: KeyRow[] = [];
  for (const entry of entries) {
    if (entry.hidden) {
      continue;
    }
    const keys = entry.label ?? entry.binds.map((bind) => bind.keys).join(" / ");
    if (entry.tiling !== undefined && entry.floating !== undefined) {
      rows.push({ group: "Tiling", keys, description: entry.tiling });
      rows.push({ group: "Floating", keys, description: entry.floating });
    } else {
      rows.push({ group: entry.group, keys, description: entry.description });
    }
  }
  return rows.sort((a, b) => KEY_GROUPS.indexOf(a.group) - KEY_GROUPS.indexOf(b.group));
}

/** Write the rows to $XDG_RUNTIME_DIR/shojiwm-keys.json for the helpers. */
export function writeKeymapFile(rows: readonly KeyRow[]) {
  COMPOSITOR.process.spawn({
    command: [
      "sh",
      "-c",
      'printf "%s\\n" "$1" > "${XDG_RUNTIME_DIR:-/tmp}/shojiwm-keys.json"',
      "write-keymap",
      JSON.stringify({ groups: KEY_GROUPS, rows }),
    ],
  });
}

/** Run the action of a chord, as if the key went down (for tests over IPC). */
export function runChord(entries: readonly KeyEntry[], keys: string): boolean {
  const chord = normalizeChord(keys);
  for (const entry of entries) {
    for (const bind of entry.binds) {
      if (normalizeChord(bind.keys) === chord) {
        bind.action();
        return true;
      }
    }
  }
  return false;
}

/** "shift+super+left" and "Super+Shift+Left" are the same chord. */
function normalizeChord(keys: string): string {
  const parts = keys.split("+").map((part) => part.toLowerCase());
  const key = parts.pop() ?? "";
  const modifiers = parts.map((part) => (part === "meta" || part === "logo" ? "super" : part)).sort();
  return [...modifiers, key].join("+");
}
