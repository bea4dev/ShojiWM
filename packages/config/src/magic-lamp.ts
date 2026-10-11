import {
  COMPOSITOR,
  compileWindowEffect,
  loadShader,
  outputLogicalSize,
  shaderStage,
  signal,
  windowSource,
  type OutputInfo,
  type PollHandle,
  type WaylandWindow,
} from "shoji_wm";
import type {
  WindowEffectAssignment,
  WindowEffectHandle,
  WindowPosition,
} from "shoji_wm/types";

// Magic Lamp: a port of KWin's magiclamp effect
// (kwin/src/plugins/magiclamp/magiclamp.cpp), with the KDE settings of this
// machine.
//
// A window that minimizes pours into its taskbar entry like a genie into
// its lamp, and pours back out when it comes back. The taskbar entry comes
// from KDE's `set_minimized_geometry` (`window.minimizedGeometry`). Without
// one, the window pours towards the cursor, as in KWin.
//
//   progress 0 ──────────────> 1        (minimize, linear, KWin's timeline)
//   window        bends         in the entry
//   progress 1 ──────────────> 0        (restore: the same, backwards)
//
// The magic-lamp shader does the drawing. The effect exists only while a
// window animates, so it costs nothing at rest.

/**
 * KWin's `animationTime(AnimationDuration)`: kwinrc [Effect-magiclamp]
 * AnimationDuration=700 times kdeglobals AnimationDurationFactor=0.25 on this
 * machine. KWin's default is 250 ms times the factor.
 */
const DURATION_MS = 700 * 0.25;
/** A slower lamp for testing (debug IPC); `null` = `DURATION_MS`. */
let debugDurationMs: number | null = null;
/**
 * The longest an opening window waits for plasmashell to lay out its taskbar
 * entry (measured: 100-200 ms after the window maps).
 */
const OPEN_ICON_WAIT_MS = 300;
/** The longest frame the lamp advances by; longer ones (stalls) slow it down. */
const MAX_FRAME_STEP_MS = 1000 / 30;
/** Room for the window's shadow, which KWin bends with the window (logical px). */
const SHADOW_MARGIN = 48;

type Side = "top" | "bottom" | "left" | "right";
const SIDE_INDEX: Record<Side, number> = { top: 0, bottom: 1, left: 2, right: 3 };

type NumberSignal = ReturnType<typeof signal<number>>;
type Outsets = { left: number; top: number; right: number; bottom: number };

interface Lamp {
  window: WaylandWindow;
  active: ReturnType<typeof signal<boolean>>;
  /** How far the effect may draw past the window: from the window to the icon. */
  outsets: ReturnType<typeof signal<Outsets>>;
  /** The icon relative to the window frame: x, y, width, height. */
  icon: NumberSignal[];
  progressSignal: NumberSignal;
  sideSignal: NumberSignal;
  effect: WindowEffectHandle;
  /** 0 = the window, 1 = in the icon. */
  progress: number;
  /** true: towards the icon (minimize), false: out of it (restore). */
  forward: boolean;
  /** The icon in global logical px, fixed when the animation starts (like KWin per frame). */
  target: WindowPosition;
  side: Side;
  poll: PollHandle | null;
  lastMs: number | null;
  /** Called once when the animation reaches its end. */
  onDone: (() => void) | null;
  /** An opening window waits (drawn as nothing) for its taskbar entry. */
  waitingForIcon: boolean;
  run: LampRun | null;
}

/** One animation, for `magicLampDebug` (frame times in ms). */
interface LampRun {
  windowId: string;
  minimized: boolean;
  side: Side;
  target: WindowPosition;
  startMs: number;
  /** How long an opening window waited for its taskbar entry. */
  waitedMs?: number;
  deltas: number[];
}

const debugRuns: LampRun[] = [];

function recordRun(run: LampRun) {
  debugRuns.push(run);
  if (debugRuns.length > 8) {
    debugRuns.shift();
  }
}

/**
 * The last few animations with their frame times (debug IPC). A number sets
 * a slow test duration, `null` goes back to the real one.
 */
export function magicLampDebug(durationMs?: number | null): readonly LampRun[] {
  if (durationMs !== undefined) {
    debugDurationMs = durationMs;
  }
  return debugRuns;
}

const lamps = new Map<string, Lamp>();

/**
 * Per window: moves the icon for a lamp drawn somewhere else than the
 * window's real place (the Window View pours a card, drawn scaled).
 */
const redirects = new Map<string, (icon: WindowPosition) => WindowPosition>();

/** Set (or clear, `null`) where the window's next lamp aims; see `redirects`. */
export function magicLampRedirect(
  window: WaylandWindow,
  redirect: ((icon: WindowPosition) => WindowPosition) | null,
) {
  if (redirect) {
    redirects.set(window.id, redirect);
  } else {
    redirects.delete(window.id);
  }
}

/** The plasma panels' rects (KWin looks for the dock that holds the icon). */
let panelRects: () => readonly WindowPosition[] = () => [];

export function configureMagicLamp(options: { panels: () => readonly WindowPosition[] }) {
  panelRects = options.panels;
}

function lampFor(window: WaylandWindow): Lamp {
  let lamp = lamps.get(window.id);
  if (lamp) {
    return lamp;
  }
  const icon = [signal(0), signal(0), signal(0), signal(0)];
  const progressSignal = signal(0);
  const sideSignal = signal(0);
  const margin = signal(SHADOW_MARGIN);
  lamp = {
    window,
    active: signal(false),
    outsets: signal<Outsets>({
      left: SHADOW_MARGIN,
      top: SHADOW_MARGIN,
      right: SHADOW_MARGIN,
      bottom: SHADOW_MARGIN,
    }),
    icon,
    progressSignal,
    sideSignal,
    effect: compileWindowEffect({
      input: windowSource(),
      outsets: SHADOW_MARGIN,
      alpha: "preserve",
      invalidate: { kind: "always" },
      pipeline: [
        shaderStage(loadShader("./src/effect/magic-lamp.frag"), {
          uniforms: {
            icon: [icon[0], icon[1], icon[2], icon[3]],
            params: [progressSignal, sideSignal, margin, margin],
          },
        }),
      ],
    }),
    progress: 0,
    forward: true,
    target: { x: 0, y: 0, width: 0, height: 0 },
    side: "bottom",
    poll: null,
    lastMs: null,
    onDone: null,
    waitingForIcon: false,
    run: null,
  };
  lamps.set(window.id, lamp);
  return lamp;
}

function contains(rect: WindowPosition, x: number, y: number) {
  return x >= rect.x && x < rect.x + rect.width && y >= rect.y && y < rect.y + rect.height;
}

function intersects(a: WindowPosition, b: WindowPosition) {
  // Qt's QRect::intersects: an empty rect intersects nothing.
  if (a.width <= 0 || a.height <= 0 || b.width <= 0 || b.height <= 0) {
    return false;
  }
  return a.x < b.x + b.width && b.x < a.x + a.width && a.y < b.y + b.height && b.y < a.y + a.height;
}

function outputRect(output: OutputInfo | undefined): WindowPosition | null {
  if (!output) {
    return null;
  }
  const size = outputLogicalSize(output);
  return { x: output.position.x, y: output.position.y, width: size.width, height: size.height };
}

function screenOf(window: WaylandWindow): WindowPosition | null {
  const name = window.output.peek();
  const output = name ? COMPOSITOR.output.get(name) : undefined;
  return outputRect(output) ?? screenAt(window.rect.x, window.rect.y);
}

function screenAt(x: number, y: number): WindowPosition | null {
  return outputRect(
    COMPOSITOR.output.find((output) => {
      const rect = outputRect(output);
      return rect !== null && contains(rect, x, y);
    }),
  );
}

type Target = { icon: WindowPosition; side: Side };

/** A laid-out taskbar entry (plasmashell sends nothing, or 0x0, before). */
function isEntry(rect: WindowPosition | null | undefined): rect is WindowPosition {
  return rect !== null && rect !== undefined && rect.width > 0 && rect.height > 0;
}

/**
 * The window's taskbar entry, else the entry of another window of the same
 * app: plasmashell groups an app's windows under one icon.
 */
function taskbarEntry(window: WaylandWindow): WindowPosition | null {
  const own = window.minimizedGeometry.peek();
  if (isEntry(own)) {
    return own;
  }
  const appId = window.appId.peek();
  if (!appId) {
    return null;
  }
  for (const other of lamps.values()) {
    if (other.window.id !== window.id && other.window.appId.peek() === appId) {
      const entry = other.window.minimizedGeometry.peek();
      if (isEntry(entry)) {
        return entry;
      }
    }
  }
  return null;
}

/** KWin's choice of icon and side (MagicLampEffect::apply, top half). */
function pickTarget(window: WaylandWindow): Target {
  const icon = taskbarEntry(window);
  return icon ? iconTarget(window, icon) : cursorTarget(window);
}

/** KWin without a taskbar entry: the cursor, as an empty icon. */
function cursorTarget(window: WaylandWindow): Target {
  const geo = window.rect;
  const right = geo.x + geo.width - 1; // QRect::right()
  const bottom = geo.y + geo.height - 1; // QRect::bottom()
  const cursor = COMPOSITOR.cursor.getPosition();
  let x = Math.round(cursor?.x ?? geo.x + geo.width / 2);
  let y = Math.round(cursor?.y ?? bottom);
  let side: Side = "top";
  if (contains(geo, x, y)) {
    // Inside the window: go to the nearest border, Top first on ties.
    let best = y - geo.y;
    if (x - geo.x < best) {
      best = x - geo.x;
      side = "left";
    }
    if (bottom - y < best) {
      best = bottom - y;
      side = "bottom";
    }
    if (right - x < best) {
      side = "right";
    }
    if (side === "top") y = geo.y;
    if (side === "left") x = geo.x;
    if (side === "bottom") y = bottom;
    if (side === "right") x = right;
  } else if (y < geo.y) {
    side = "top";
  } else if (x < geo.x) {
    side = "left";
  } else if (y > bottom) {
    side = "bottom";
  } else if (x > right) {
    side = "right";
  }
  return { icon: { x, y, width: 0, height: 0 }, side };
}

/** KWin with a taskbar entry: the panel that holds it decides the side. */
function iconTarget(window: WaylandWindow, icon: WindowPosition): Target {
  const panel = panelRects().find((rect) => intersects(rect, icon));
  if (panel) {
    const screen = screenOf(window) ?? panel;
    if (panel.width >= panel.height) {
      const centerY = icon.y + icon.height / 2;
      return { icon, side: centerY <= screen.y + screen.height / 2 ? "top" : "bottom" };
    }
    const centerX = icon.x + icon.width / 2;
    return { icon, side: centerX <= screen.x + screen.width / 2 ? "left" : "right" };
  }

  // No panel (auto-hidden?): the screen edge the icon touches.
  const screen = screenAt(icon.x, icon.y);
  if (!screen) {
    return { icon, side: "bottom" };
  }
  const left = Math.max(screen.x, icon.x);
  const top = Math.max(screen.y, icon.y);
  const rightEdge = Math.min(screen.x + screen.width, icon.x + icon.width);
  let side: Side = "bottom";
  if (left === screen.x) side = "left";
  else if (rightEdge === screen.x + screen.width) side = "right";
  else if (top === screen.y) side = "top";
  return { icon, side };
}

/** The panel along the bottom of the window's screen, if any. */
function bottomPanel(window: WaylandWindow): WindowPosition | null {
  const screen = screenOf(window);
  if (!screen) {
    return null;
  }
  const screenBottom = screen.y + screen.height;
  return (
    panelRects().find(
      (rect) =>
        rect.width >= rect.height &&
        rect.y + rect.height >= screenBottom - 1 &&
        rect.x < screen.x + screen.width &&
        screen.x < rect.x + rect.width,
    ) ?? null
  );
}

/**
 * Not KWin (KWin has no lamp on open): where a new window without a taskbar
 * entry comes out of. A click on the bottom panel (a launcher) gives the
 * spot; `orMiddle` falls back to the panel's middle.
 */
function launcherTarget(window: WaylandWindow, orMiddle: boolean): Target | null {
  const panel = bottomPanel(window);
  if (!panel) {
    return null;
  }
  const cursor = COMPOSITOR.cursor.getPosition();
  const onPanel = cursor !== undefined && contains(panel, cursor.x, cursor.y);
  if (!onPanel && !orMiddle) {
    return null;
  }
  const x = onPanel ? cursor.x : panel.x + panel.width / 2;
  return { icon: { x: Math.round(x), y: panel.y, width: 0, height: 0 }, side: "bottom" };
}

/** Where a window opening now comes out of; `null` while it is not known. */
function openTarget(window: WaylandWindow): Target | null {
  const icon = taskbarEntry(window);
  return icon ? iconTarget(window, icon) : launcherTarget(window, false);
}

/** Send this frame's numbers to the shader. */
function publish(lamp: Lamp) {
  const geo = lamp.window.rect;
  const icon = lamp.target;
  lamp.icon[0][1](icon.x - geo.x);
  lamp.icon[1][1](icon.y - geo.y);
  lamp.icon[2][1](icon.width);
  lamp.icon[3][1](icon.height);
  lamp.progressSignal[1](lamp.progress);
  lamp.sideSignal[1](SIDE_INDEX[lamp.side]);
  // The window may move meanwhile (a tile making room on restore): grow the
  // drawing area when it no longer reaches the icon. Only growing keeps the
  // offscreen texture from being made again every frame.
  const need = outsetsFor(lamp.window, icon);
  const have = lamp.outsets.peek();
  if (
    need.left > have.left ||
    need.top > have.top ||
    need.right > have.right ||
    need.bottom > have.bottom
  ) {
    lamp.outsets[1]({
      left: Math.max(need.left, have.left),
      top: Math.max(need.top, have.top),
      right: Math.max(need.right, have.right),
      bottom: Math.max(need.bottom, have.bottom),
    });
  }
}

/** Room around the window for the drawing: up to the icon, plus the shadow. */
function outsetsFor(window: WaylandWindow, icon: WindowPosition): Outsets {
  const geo = window.rect;
  return {
    left: Math.ceil(Math.max(0, geo.x - icon.x) + SHADOW_MARGIN),
    top: Math.ceil(Math.max(0, geo.y - icon.y) + SHADOW_MARGIN),
    right: Math.ceil(Math.max(0, icon.x + icon.width - (geo.x + geo.width)) + SHADOW_MARGIN),
    bottom: Math.ceil(Math.max(0, icon.y + icon.height - (geo.y + geo.height)) + SHADOW_MARGIN),
  };
}

/** End the animation where it is, and tell its owner (`onDone`). */
function stopLamp(lamp: Lamp) {
  lamp.poll?.cancel();
  lamp.poll = null;
  lamp.active[1](false);
  const done = lamp.onDone;
  lamp.onDone = null;
  done?.();
}

/**
 * Start (or turn around) the animation for a minimize (`minimized` true) or
 * a restore. Returns how long it runs, in ms.
 */
export function magicLampStart(
  window: WaylandWindow,
  minimized: boolean,
  onDone: () => void,
  options: { opening?: boolean } = {},
): number {
  const lamp = lampFor(window);
  if (!lamp.poll) {
    // A fresh animation (KWin: a running one only turns around).
    lamp.progress = minimized ? 0 : 1;
    lamp.waitingForIcon = false;
    let target: Target | null = pickTarget(window);
    if (options.opening) {
      target = openTarget(window);
      if (!target) {
        // plasmashell lays out a new window's taskbar entry 100-200 ms
        // after the window maps. Wait for it, the window still inside an
        // empty icon (so not drawn).
        lamp.waitingForIcon = true;
        target = launcherTarget(window, true) ?? cursorTarget(window);
      }
    }
    // The side stays the real icon's: a redirect only scales and moves.
    lamp.target = redirects.get(window.id)?.(target.icon) ?? target.icon;
    lamp.side = target.side;
  }
  lamp.forward = minimized;
  // A turn-around ends the previous run for its owner first.
  const previous = lamp.onDone;
  lamp.onDone = onDone;
  previous?.();
  lamp.outsets[1](outsetsFor(window, lamp.target));
  publish(lamp);
  lamp.active[1](true);
  const run: LampRun = {
    windowId: window.id,
    minimized,
    side: lamp.side,
    target: lamp.target,
    startMs: Date.now(),
    deltas: [],
  };
  recordRun(run);
  lamp.run = run;

  if (!lamp.poll) {
    lamp.lastMs = null;
    let waitStartMs: number | null = null;
    lamp.poll = window.createPoll(
      1,
      (handle) => {
        if (lamp.waitingForIcon) {
          waitStartMs ??= handle.nowMs;
          const entry = taskbarEntry(window);
          if (!entry && handle.nowMs - waitStartMs < OPEN_ICON_WAIT_MS) {
            publish(lamp);
            return;
          }
          lamp.waitingForIcon = false;
          const target = entry
            ? iconTarget(window, entry)
            : (launcherTarget(window, true) ?? cursorTarget(window));
          lamp.target = target.icon;
          lamp.side = target.side;
          run.target = target.icon;
          run.side = target.side;
          run.waitedMs = handle.nowMs - waitStartMs;
        }
        const raw = lamp.lastMs === null ? 0 : handle.nowMs - lamp.lastMs;
        lamp.lastMs = handle.nowMs;
        lamp.run?.deltas.push(Math.round(raw * 10) / 10);
        // A stalled frame (the shader's first compile, a busy main loop)
        // slows the lamp down instead of skipping it.
        const delta = Math.min(raw, MAX_FRAME_STEP_MS);
        const step = delta / (debugDurationMs ?? DURATION_MS);
        lamp.progress = Math.min(1, Math.max(0, lamp.progress + (lamp.forward ? step : -step)));
        publish(lamp);
        if (lamp.forward ? lamp.progress >= 1 : lamp.progress <= 0) {
          stopLamp(lamp);
        }
      },
      { dirty: "none" },
    );
  }
  const left = minimized ? 1 - lamp.progress : lamp.progress;
  return Math.max(1, Math.round(left * (debugDurationMs ?? DURATION_MS)));
}

/** Drop the animation at once (instant minimize or restore). */
export function magicLampStop(window: WaylandWindow) {
  const lamp = lamps.get(window.id);
  if (lamp) {
    stopLamp(lamp);
  }
}

/** The window effect: the lamp while the window minimizes or restores, else none. */
export function magicLampEffect(window: WaylandWindow): WindowEffectAssignment | null {
  // Always read the signals, so the effect turns on when an animation starts.
  const lamp = lampFor(window);
  if (!lamp.active()) {
    return null;
  }
  return { replace: { ...lamp.effect, outsets: lamp.outsets() } };
}

/** True while the window pours into or out of its icon (reactive). */
export function magicLampRunning(window: WaylandWindow): boolean {
  return lampFor(window).active();
}

/** Forget a closed window. */
export function magicLampForget(window: WaylandWindow) {
  const lamp = lamps.get(window.id);
  if (lamp) {
    stopLamp(lamp);
  }
  lamps.delete(window.id);
  redirects.delete(window.id);
}
