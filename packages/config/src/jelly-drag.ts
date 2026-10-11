import {
  compileWindowEffect,
  loadShader,
  shaderStage,
  signal,
  windowSource,
  type PollHandle,
  type WaylandWindow,
  type WindowMoveEvent,
  type WindowPosition,
} from "shoji_wm";
import type {
  ShaderUniformMap,
  WindowEffectAssignment,
  WindowEffectHandle,
} from "shoji_wm/types";

// Wobbly windows: a line-for-line port of KWin's wobblywindows effect
// (kwin/src/plugins/wobblywindows/wobblywindows.cpp), with KDE's defaults.
//
// A 4x4 grid of points sits on the window. Each point is joined to its
// neighbours by springs that want to keep the grid spacing. The point you
// grab is pulled towards its place on the window; the others only follow
// their neighbours. So the window trails the pointer and wobbles to a stop.
// After every step, speed and acceleration are averaged with the eight points
// around each point: that keeps the wobble soft and clean.
//
// The jelly-drag shader bends the window's pixels with the grid. Nothing
// runs while no window moves.

const GRID = 4;
const COUNT = GRID * GRID;
/**
 * KDE's "Wobbliness" slider, 0 (stiff, KDE default) to 4 (very wobbly).
 * Level 3 and 4 stop with a small visible jump.
 */
const WOBBLINESS = 2;
// KWin's parameter sets: stiffness, drag (speed kept after each step), move factor.
const LEVELS = [
  [0.15, 0.8, 0.1],
  [0.1, 0.85, 0.1],
  [0.06, 0.9, 0.1],
  [0.03, 0.92, 0.2],
  [0.01, 0.97, 0.25],
];
const [STIFFNESS, DRAG, MOVE_FACTOR] = LEVELS[WOBBLINESS];
const MIN_VELOCITY = 0;
const MAX_VELOCITY = 1000;
const STOP_VELOCITY = 0.5;
const MIN_ACCELERATION = 0;
const MAX_ACCELERATION = 1000;
const STOP_ACCELERATION = 0.5;
/** KWin's physics step, in ms. */
const STEP_MS = 10;
/** The farthest the drawing may bend, in logical px. Also the effect margin. */
const MAX_BEND_PX = 250;
/** Window opacity while dragged, like KDE's Translucency effect (80%). */
const DRAG_OPACITY = 0.8;
/** How long the fade to and from DRAG_OPACITY takes, in ms (KDE: 250 ms). */
const FADE_MS = 250;

type NumberSignal = ReturnType<typeof signal<number>>;

/** Springs of every point: the neighbour, and the rest offset to it. */
const SPRINGS: { j: number; dx: number; dy: number }[][] = Array.from(
  { length: COUNT },
  (_, i) => {
    const col = i % GRID;
    const row = (i - col) / GRID;
    const list: { j: number; dx: number; dy: number }[] = [];
    if (col > 0) list.push({ j: i - 1, dx: -1, dy: 0 });
    if (col < GRID - 1) list.push({ j: i + 1, dx: 1, dy: 0 });
    if (row > 0) list.push({ j: i - GRID, dx: 0, dy: -1 });
    if (row < GRID - 1) list.push({ j: i + GRID, dx: 0, dy: 1 });
    return list;
  },
);

/** The up to eight points around every point (KWin's "height ring"). */
const RING: number[][] = Array.from({ length: COUNT }, (_, i) => {
  const col = i % GRID;
  const row = (i - col) / GRID;
  const list: number[] = [];
  for (let dr = -1; dr <= 1; dr++) {
    for (let dc = -1; dc <= 1; dc++) {
      const r = row + dr;
      const c = col + dc;
      if ((dr || dc) && r >= 0 && r < GRID && c >= 0 && c < GRID) {
        list.push(r * GRID + c);
      }
    }
  }
  return list;
});

interface Wobble {
  window: WaylandWindow;
  active: ReturnType<typeof signal<boolean>>;
  /** 16 bends (x and y) for the shader, relative to the rest grid. */
  bendSignals: NumberSignal[];
  opacity: NumberSignal;
  effect: WindowEffectHandle;
  dragging: boolean;
  /** The window rect from the last move event. */
  dragRect: WindowPosition | null;
  /** Points pulled towards their place on the window (the grab points). */
  constraint: boolean[];
  /** Absolute logical positions, speeds and accelerations, KWin style. */
  posX: Float64Array;
  posY: Float64Array;
  velX: Float64Array;
  velY: Float64Array;
  accX: Float64Array;
  accY: Float64Array;
  /** Set once the springs have reset for a fresh wobble. */
  started: boolean;
  poll: PollHandle | null;
  lastMs: number | null;
}

const wobbles = new Map<string, Wobble>();

function wobbleFor(window: WaylandWindow): Wobble {
  let wobble = wobbles.get(window.id);
  if (wobble) {
    return wobble;
  }
  const bendSignals = Array.from({ length: COUNT * 2 }, () => signal(0));
  // Pack two points per vec4: d0 = points 0 and 1, d1 = points 2 and 3, ...
  const opacity = signal(1);
  const uniforms: ShaderUniformMap = { opacity };
  for (let i = 0; i < 8; i++) {
    const s = bendSignals;
    uniforms[`d${i}`] = [s[i * 4], s[i * 4 + 1], s[i * 4 + 2], s[i * 4 + 3]];
  }
  wobble = {
    window,
    active: signal(false),
    bendSignals,
    opacity,
    effect: compileWindowEffect({
      input: windowSource(),
      outsets: MAX_BEND_PX,
      alpha: "preserve",
      invalidate: { kind: "always" },
      pipeline: [
        shaderStage(loadShader("./src/effect/jelly-drag.frag"), { uniforms }),
      ],
    }),
    dragging: false,
    dragRect: null,
    constraint: new Array<boolean>(COUNT).fill(false),
    posX: new Float64Array(COUNT),
    posY: new Float64Array(COUNT),
    velX: new Float64Array(COUNT),
    velY: new Float64Array(COUNT),
    accX: new Float64Array(COUNT),
    accY: new Float64Array(COUNT),
    started: false,
    poll: null,
    lastMs: null,
  };
  wobbles.set(window.id, wobble);
  return wobble;
}

/** The window rect now: the move event while dragging, else the live rect. */
function rectOf(wobble: Wobble): WindowPosition {
  return (wobble.dragging && wobble.dragRect) || wobble.window.rect;
}

/** The rest place of grid point `i` on `rect`. */
function originX(rect: WindowPosition, i: number) {
  return rect.x + ((i % GRID) * rect.width) / (GRID - 1);
}
function originY(rect: WindowPosition, i: number) {
  return rect.y + (Math.floor(i / GRID) * rect.height) / (GRID - 1);
}

/** Put every point at its rest place, at rest (KWin: initWobblyInfo). */
function resetGrid(wobble: Wobble, rect: WindowPosition) {
  for (let i = 0; i < COUNT; i++) {
    wobble.posX[i] = originX(rect, i);
    wobble.posY[i] = originY(rect, i);
    wobble.velX[i] = 0;
    wobble.velY[i] = 0;
    wobble.constraint[i] = false;
  }
}

/** Average every value with the points around it (KWin: heightRingLinearMean). */
function ringMean(x: Float64Array, y: Float64Array) {
  const outX = new Float64Array(COUNT);
  const outY = new Float64Array(COUNT);
  for (let i = 0; i < COUNT; i++) {
    const ring = RING[i];
    let sumX = ring.length * x[i];
    let sumY = ring.length * y[i];
    for (const j of ring) {
      sumX += x[j];
      sumY += y[j];
    }
    outX[i] = sumX / (2 * ring.length);
    outY[i] = sumY / (2 * ring.length);
  }
  x.set(outX);
  y.set(outY);
}

/** Zero values under `min`, cap values over `max` (KWin: fixVectorBounds). */
function bound(value: number, min: number, max: number) {
  const size = Math.abs(value);
  if (size < min) return 0;
  if (size > max) return Math.sign(value) * max;
  return value;
}

/** One physics step of `time` ms (KWin: updateWindowWobblyDatas). Returns true while wobbling. */
function step(wobble: Wobble, rect: WindowPosition, time: number) {
  const { posX, posY, velX, velY, accX, accY } = wobble;
  const xLength = rect.width / (GRID - 1);
  const yLength = rect.height / (GRID - 1);

  for (let i = 0; i < COUNT; i++) {
    if (wobble.constraint[i]) {
      // A grab point: pulled towards its place on the window.
      accX[i] = (originX(rect, i) - posX[i]) * STIFFNESS;
      accY[i] = (originY(rect, i) - posY[i]) * STIFFNESS;
      continue;
    }
    // Springs to the neighbours, which want the grid spacing between them.
    const springs = SPRINGS[i];
    let ax = 0;
    let ay = 0;
    for (const { j, dx, dy } of springs) {
      ax += (posX[j] - posX[i] - dx * xLength) * STIFFNESS;
      ay += (posY[j] - posY[i] - dy * yLength) * STIFFNESS;
    }
    accX[i] = ax / springs.length;
    accY[i] = ay / springs.length;
  }
  ringMean(accX, accY);

  let accSum = 0;
  for (let i = 0; i < COUNT; i++) {
    const ax = bound(accX[i], MIN_ACCELERATION, MAX_ACCELERATION);
    const ay = bound(accY[i], MIN_ACCELERATION, MAX_ACCELERATION);
    velX[i] = ax * time + velX[i] * DRAG;
    velY[i] = ay * time + velY[i] * DRAG;
    accSum += Math.abs(ax) + Math.abs(ay);
  }
  ringMean(velX, velY);

  let velSum = 0;
  for (let i = 0; i < COUNT; i++) {
    velX[i] = bound(velX[i], MIN_VELOCITY, MAX_VELOCITY);
    velY[i] = bound(velY[i], MIN_VELOCITY, MAX_VELOCITY);
    posX[i] += velX[i] * time * MOVE_FACTOR;
    posY[i] += velY[i] * time * MOVE_FACTOR;
    velSum += Math.abs(velX[i]) + Math.abs(velY[i]);
  }

  return !(accSum < STOP_ACCELERATION && velSum < STOP_VELOCITY);
}

/** Send the bend of every point to the shader. */
function publish(wobble: Wobble, rect: WindowPosition, still: boolean) {
  let biggest = 0;
  const bendX = new Float64Array(COUNT);
  const bendY = new Float64Array(COUNT);
  if (!still) {
    for (let i = 0; i < COUNT; i++) {
      bendX[i] = wobble.posX[i] - originX(rect, i);
      bendY[i] = wobble.posY[i] - originY(rect, i);
      biggest = Math.max(biggest, Math.hypot(bendX[i], bendY[i]));
    }
  }
  // Keep the drawing inside the effect margin. Scale the whole bend, so the
  // shape stays smooth; the physics itself keeps the full bend.
  const scale = biggest > MAX_BEND_PX ? MAX_BEND_PX / biggest : 1;
  for (let i = 0; i < COUNT; i++) {
    wobble.bendSignals[i * 2][1](bendX[i] * scale);
    wobble.bendSignals[i * 2 + 1][1](bendY[i] * scale);
  }
}

/** Fade towards the drag opacity while dragging, back to opaque after. */
function fade(wobble: Wobble, ms: number) {
  const target = wobble.dragging ? DRAG_OPACITY : 1;
  const change = ((1 - DRAG_OPACITY) * ms) / FADE_MS;
  const current = wobble.opacity();
  const next =
    current < target
      ? Math.min(target, current + change)
      : Math.max(target, current - change);
  wobble.opacity[1](next);
  return next;
}

/** Run the physics for one frame (KWin: prePaintWindow). Returns false once done. */
function frame(wobble: Wobble, deltaMs: number) {
  const rect = rectOf(wobble);
  let wobbling = true;
  // KWin splits the frame time into steps of at most 10 ms.
  for (let left = deltaMs; left > 0; left -= STEP_MS) {
    wobbling = step(wobble, rect, Math.min(left, STEP_MS));
    if (!wobbling) {
      break;
    }
  }
  const opacity = fade(wobble, deltaMs);
  const done = !wobble.dragging && !wobbling && opacity === 1;
  // Like KWin, draw the window flat while the springs are at rest.
  publish(wobble, rect, !wobbling);
  return !done;
}

function startPoll(wobble: Wobble) {
  if (wobble.poll) {
    return;
  }
  wobble.lastMs = null;
  wobble.poll = wobble.window.createPoll(
    1,
    (handle) => {
      // Cap a stalled frame, so the window does not jump.
      const delta =
        wobble.lastMs === null ? 0 : Math.min(handle.nowMs - wobble.lastMs, 100);
      wobble.lastMs = handle.nowMs;
      if (!frame(wobble, delta)) {
        handle.cancel();
        wobble.poll = null;
        wobble.started = false;
        wobble.active[1](false);
      }
    },
    { dirty: "none" },
  );
}

/** Feed every interactive window move here (KWin: start/step/finish UserMovedResized). */
export function jellyDragOnWindowMove(event: WindowMoveEvent) {
  const wobble = wobbleFor(event.window);
  const rect = event.currentRect;
  wobble.dragRect = rect;
  if (event.phase === "start") {
    if (!wobble.started) {
      resetGrid(wobble, event.startRect);
      wobble.started = true;
    }
    // Constrain the grid point nearest to the grab, like KWin.
    const xIncrement = event.startRect.width / (GRID - 1);
    const yIncrement = event.startRect.height / (GRID - 1);
    const col = Math.floor((event.startPointer.x - event.startRect.x) / xIncrement + 0.5);
    const row = Math.floor((event.startPointer.y - event.startRect.y) / yIncrement + 0.5);
    const picked = Math.min(COUNT - 1, Math.max(0, row * GRID + col));
    wobble.constraint[picked] = true;
  }
  wobble.dragging = event.phase === "start" || event.phase === "update";
  if (wobble.dragging) {
    wobble.active[1](true);
    startPoll(wobble);
  }
}

/** The window effect: the wobble while the window moves or settles, else none. */
export function jellyDragEffect(
  window: WaylandWindow,
): WindowEffectAssignment | null {
  const wobble = wobbleFor(window);
  return wobble.active() ? { replace: wobble.effect } : null;
}

/** Forget a closed window. */
export function jellyDragForget(window: WaylandWindow) {
  wobbles.get(window.id)?.poll?.cancel();
  wobbles.delete(window.id);
}
