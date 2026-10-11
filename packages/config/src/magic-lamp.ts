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
  EffectOutsets,
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
/** Room for the window's shadow, which KWin bends with the window (logical px). */
const SHADOW_MARGIN = 48;

type Side = "top" | "bottom" | "left" | "right";
const SIDE_INDEX: Record<Side, number> = { top: 0, bottom: 1, left: 2, right: 3 };

type NumberSignal = ReturnType<typeof signal<number>>;

interface Lamp {
  window: WaylandWindow;
  active: ReturnType<typeof signal<boolean>>;
  /** How far the effect may draw past the window: from the window to the icon. */
  outsets: ReturnType<typeof signal<EffectOutsets>>;
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
}

const lamps = new Map<string, Lamp>();

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
    outsets: signal<EffectOutsets>(SHADOW_MARGIN),
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

/** KWin's choice of icon and side (MagicLampEffect::apply, top half). */
function pickTarget(window: WaylandWindow): { icon: WindowPosition; side: Side } {
  const geo = window.rect;
  const right = geo.x + geo.width - 1; // QRect::right()
  const bottom = geo.y + geo.height - 1; // QRect::bottom()
  // A ShojiWM installed before `minimizedGeometry` existed lacks the signal.
  const geometrySignal = window.minimizedGeometry as typeof window.minimizedGeometry | undefined;
  const icon = geometrySignal?.peek();

  if (!geometrySignal) {
    const guess = taskbarGuess(window);
    if (guess) {
      return guess;
    }
  }

  if (!icon || icon.width <= 0 || icon.height <= 0) {
    // No taskbar entry: the cursor, as an empty icon.
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

  // The panel that holds the icon decides the side.
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

/**
 * Not KWin: a ShojiWM too old to know the taskbar entries still aims at the
 * panel at the bottom of the window's screen. The cursor's x when the
 * cursor is on that panel (a taskbar click), else the panel's middle.
 */
function taskbarGuess(window: WaylandWindow): { icon: WindowPosition; side: Side } | null {
  const screen = screenOf(window);
  if (!screen) {
    return null;
  }
  const screenBottom = screen.y + screen.height;
  const panel = panelRects().find(
    (rect) =>
      rect.width >= rect.height &&
      rect.y + rect.height >= screenBottom - 1 &&
      rect.x < screen.x + screen.width &&
      screen.x < rect.x + rect.width,
  );
  if (!panel) {
    return null;
  }
  const cursor = COMPOSITOR.cursor.getPosition();
  const x =
    cursor && contains(panel, cursor.x, cursor.y) ? cursor.x : panel.x + panel.width / 2;
  return { icon: { x: Math.round(x), y: panel.y, width: 0, height: 0 }, side: "bottom" };
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
}

/** Room around the window for the drawing: up to the icon, plus the shadow. */
function outsetsFor(window: WaylandWindow, icon: WindowPosition): EffectOutsets {
  const geo = window.rect;
  return {
    left: Math.ceil(Math.max(0, geo.x - icon.x) + SHADOW_MARGIN),
    top: Math.ceil(Math.max(0, geo.y - icon.y) + SHADOW_MARGIN),
    right: Math.ceil(Math.max(0, icon.x + icon.width - (geo.x + geo.width)) + SHADOW_MARGIN),
    bottom: Math.ceil(Math.max(0, icon.y + icon.height - (geo.y + geo.height)) + SHADOW_MARGIN),
  };
}

function stopLamp(lamp: Lamp) {
  lamp.poll?.cancel();
  lamp.poll = null;
  lamp.active[1](false);
}

/**
 * Start (or turn around) the animation for a minimize (`minimized` true) or
 * a restore. Returns how long it runs, in ms.
 */
export function magicLampStart(window: WaylandWindow, minimized: boolean): number {
  const lamp = lampFor(window);
  if (!lamp.poll) {
    // A fresh animation (KWin: a running one only turns around).
    lamp.progress = minimized ? 0 : 1;
    const { icon, side } = pickTarget(window);
    lamp.target = icon;
    lamp.side = side;
  }
  lamp.forward = minimized;
  lamp.outsets[1](outsetsFor(window, lamp.target));
  publish(lamp);
  lamp.active[1](true);

  if (!lamp.poll) {
    lamp.lastMs = null;
    lamp.poll = window.createPoll(
      1,
      (handle) => {
        const delta = lamp.lastMs === null ? 0 : handle.nowMs - lamp.lastMs;
        lamp.lastMs = handle.nowMs;
        const step = delta / DURATION_MS;
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
  return Math.max(1, Math.round(left * DURATION_MS));
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

/** Forget a closed window. */
export function magicLampForget(window: WaylandWindow) {
  lamps.get(window.id)?.poll?.cancel();
  lamps.delete(window.id);
}
