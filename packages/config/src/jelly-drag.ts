import {
  compileWindowEffect,
  loadShader,
  shaderStage,
  signal,
  windowSource,
  type PollHandle,
  type WaylandWindow,
  type WindowMoveEvent,
} from "shoji_wm";
import type {
  ShaderUniformMap,
  WindowEffectAssignment,
  WindowEffectHandle,
} from "shoji_wm/types";

// Wobbly windows, the KDE way.
//
// A 4x4 grid of points sits on the window, joined to its neighbours by
// springs. While you drag, the point under your grab is pinned to the
// pointer and the rest lag behind and pull on each other, so the window bends
// like a rubber sheet. When you let go, every point springs back to its rest
// place and the window wobbles to a stop. The jelly-drag shader bends the
// window's pixels with the grid. Nothing runs while no window moves.

const GRID = 4;
/** The farthest a grid point may bend, in logical px. Also the effect margin. */
const MAX_BEND_PX = 120;
/** Pull between neighbouring points, 1/s². Higher is stiffer. */
const SPRING = 450;
/** Pull of every point back to its rest place, 1/s². Keeps the shape. */
const SETTLE = 140;
/**
 * Share of the window's motion that becomes bend (KDE's "move factor").
 * Low: the window moves as one piece and only wobbles. High: it trails like jelly.
 */
const MOVE_FACTOR = 0.15;
/** Friction, 1/s. Lower wobbles longer. */
const FRICTION = 6.5;
/** Physics step, in seconds. Small steps keep the springs stable. */
const STEP_S = 1 / 480;
/** Window opacity while dragged, like KDE's Translucency effect (80%). */
const DRAG_OPACITY = 0.8;
/** How long the fade to and from DRAG_OPACITY takes, in seconds (KDE: 250 ms). */
const FADE_S = 0.25;

type NumberSignal = ReturnType<typeof signal<number>>;

/** The left, right, upper and lower neighbours of every grid point. */
const NEIGHBOURS: number[][] = Array.from({ length: GRID * GRID }, (_, i) => {
  const col = i % GRID;
  const row = (i - col) / GRID;
  const list: number[] = [];
  if (col > 0) list.push(i - 1);
  if (col < GRID - 1) list.push(i + 1);
  if (row > 0) list.push(i - GRID);
  if (row < GRID - 1) list.push(i + GRID);
  return list;
});

interface Wobble {
  active: ReturnType<typeof signal<boolean>>;
  /** 16 bends (x and y), as signals for the shader. */
  bendSignals: NumberSignal[];
  opacity: NumberSignal;
  effect: WindowEffectHandle;
  dragging: boolean;
  /** The grid point pinned to the pointer while dragging. */
  pinned: number;
  // Per point, relative to its rest place (logical px): bend and speed.
  bendX: Float64Array;
  bendY: Float64Array;
  speedX: Float64Array;
  speedY: Float64Array;
  // Window position last frame, to turn window motion into bend.
  lastX: number;
  lastY: number;
  targetX: number;
  targetY: number;
  // Rest spacing between grid points, logical px.
  cellW: number;
  cellH: number;
  poll: PollHandle | null;
  lastMs: number | null;
}

const wobbles = new Map<string, Wobble>();

function wobbleFor(window: WaylandWindow): Wobble {
  let wobble = wobbles.get(window.id);
  if (wobble) {
    return wobble;
  }
  const bendSignals = Array.from({ length: GRID * GRID * 2 }, () => signal(0));
  // Pack two points per vec4: d0 = points 0 and 1, d1 = points 2 and 3, ...
  const opacity = signal(1);
  const uniforms: ShaderUniformMap = { opacity };
  for (let i = 0; i < 8; i++) {
    const s = bendSignals;
    uniforms[`d${i}`] = [s[i * 4], s[i * 4 + 1], s[i * 4 + 2], s[i * 4 + 3]];
  }
  wobble = {
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
    pinned: 0,
    bendX: new Float64Array(GRID * GRID),
    bendY: new Float64Array(GRID * GRID),
    speedX: new Float64Array(GRID * GRID),
    speedY: new Float64Array(GRID * GRID),
    lastX: 0,
    lastY: 0,
    targetX: 0,
    targetY: 0,
    cellW: 1,
    cellH: 1,
    poll: null,
    lastMs: null,
  };
  wobbles.set(window.id, wobble);
  return wobble;
}

/** Advance the springs by `dt` seconds. Returns false once settled. */
function step(wobble: Wobble, dt: number) {
  const { bendX, bendY, speedX, speedY } = wobble;

  // The window moved since last frame: a small share of that motion kicks
  // every point backwards. A kick on the speed (not a jump in position)
  // keeps uneven pointer events from showing as jerks.
  const moveX = wobble.targetX - wobble.lastX;
  const moveY = wobble.targetY - wobble.lastY;
  wobble.lastX = wobble.targetX;
  wobble.lastY = wobble.targetY;
  if (dt > 0) {
    for (let i = 0; i < GRID * GRID; i++) {
      speedX[i] -= (moveX * MOVE_FACTOR) / dt;
      speedY[i] -= (moveY * MOVE_FACTOR) / dt;
    }
  }

  for (let left = dt; left > 0; left -= STEP_S) {
    const h = Math.min(left, STEP_S);
    for (let i = 0; i < GRID * GRID; i++) {
      if (wobble.dragging && i === wobble.pinned) {
        // Pinned under the pointer: no bend, no speed.
        bendX[i] = 0;
        bendY[i] = 0;
        speedX[i] = 0;
        speedY[i] = 0;
        continue;
      }
      // Springs to the neighbours: pull towards the neighbours' bend.
      const links = NEIGHBOURS[i];
      let forceX = 0;
      let forceY = 0;
      for (const j of links) {
        forceX += bendX[j] - bendX[i];
        forceY += bendY[j] - bendY[i];
      }
      forceX *= SPRING / links.length;
      forceY *= SPRING / links.length;
      // Always pulled back to the rest place, so the shape holds.
      forceX -= SETTLE * bendX[i];
      forceY -= SETTLE * bendY[i];
      speedX[i] += (forceX - FRICTION * speedX[i]) * h;
      speedY[i] += (forceY - FRICTION * speedY[i]) * h;
    }
    for (let i = 0; i < GRID * GRID; i++) {
      bendX[i] += speedX[i] * h;
      bendY[i] += speedY[i] * h;
    }
  }

  // Keep every bend inside the effect margin, and publish it.
  let biggest = 0;
  let fastest = 0;
  for (let i = 0; i < GRID * GRID; i++) {
    const length = Math.hypot(bendX[i], bendY[i]);
    if (length > MAX_BEND_PX) {
      bendX[i] *= MAX_BEND_PX / length;
      bendY[i] *= MAX_BEND_PX / length;
    }
    biggest = Math.max(biggest, Math.min(length, MAX_BEND_PX));
    fastest = Math.max(fastest, Math.hypot(speedX[i], speedY[i]));
  }
  // Fade towards the drag opacity while dragging, back to opaque after.
  const fadeTarget = wobble.dragging ? DRAG_OPACITY : 1;
  const fadeStep = ((1 - DRAG_OPACITY) * dt) / FADE_S;
  const current = wobble.opacity();
  const faded =
    current < fadeTarget
      ? Math.min(fadeTarget, current + fadeStep)
      : Math.max(fadeTarget, current - fadeStep);
  wobble.opacity[1](faded);

  const settled =
    !wobble.dragging && biggest < 0.1 && fastest < 5 && faded === 1;
  for (let i = 0; i < GRID * GRID; i++) {
    wobble.bendSignals[i * 2][1](settled ? 0 : bendX[i]);
    wobble.bendSignals[i * 2 + 1][1](settled ? 0 : bendY[i]);
  }
  return !settled;
}

function startPoll(window: WaylandWindow, wobble: Wobble) {
  if (wobble.poll) {
    return;
  }
  wobble.lastMs = null;
  wobble.poll = window.createPoll(
    1,
    (handle) => {
      // Cap the step so a stalled frame does not fling the window.
      const dt =
        wobble.lastMs === null
          ? 0
          : Math.min((handle.nowMs - wobble.lastMs) / 1000, 0.05);
      wobble.lastMs = handle.nowMs;
      if (!step(wobble, dt)) {
        handle.cancel();
        wobble.poll = null;
        wobble.active[1](false);
      }
    },
    { dirty: "none" },
  );
}

/** Feed every interactive window move here. */
export function jellyDragOnWindowMove(event: WindowMoveEvent) {
  const wobble = wobbleFor(event.window);
  const rect = event.currentRect;
  if (event.phase === "start") {
    // Pin the grid point nearest to where the window was grabbed.
    const grabX = event.startPointer.x - event.startRect.x;
    const grabY = event.startPointer.y - event.startRect.y;
    wobble.cellW = Math.max(rect.width / (GRID - 1), 1);
    wobble.cellH = Math.max(rect.height / (GRID - 1), 1);
    const col = Math.min(GRID - 1, Math.max(0, Math.round(grabX / wobble.cellW)));
    const row = Math.min(GRID - 1, Math.max(0, Math.round(grabY / wobble.cellH)));
    wobble.pinned = row * GRID + col;
    if (!wobble.poll) {
      wobble.lastX = rect.x;
      wobble.lastY = rect.y;
    }
  }
  wobble.targetX = rect.x;
  wobble.targetY = rect.y;
  wobble.dragging = event.phase === "start" || event.phase === "update";
  if (wobble.dragging) {
    wobble.active[1](true);
    startPoll(event.window, wobble);
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
