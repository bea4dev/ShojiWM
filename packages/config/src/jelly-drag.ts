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
import type { WindowEffectAssignment, WindowEffectHandle } from "shoji_wm/types";

// Jelly drag: a light copy of KDE's Wobbly Windows.
//
// While you drag a window, its far side trails behind the pointer, and it
// wobbles back into shape when you stop. A spring gives the lag of the far
// side; the jelly-drag shader shifts the pixels by it. The effect exists only
// while a window moves or settles, so a still desktop pays nothing for it.

/** The farthest the far side may trail, in logical px. Also the effect margin. */
const MAX_LAG_PX = 40;
/** Spring pull, 1/s². Higher snaps back faster. */
const STIFFNESS = 260;
/** Spring friction, 1/s. Lower wobbles longer. */
const DAMPING = 12;
/** Physics step, in seconds. Small steps keep the spring stable. */
const MAX_STEP_S = 1 / 240;

interface Jelly {
  active: ReturnType<typeof signal<boolean>>;
  lagX: ReturnType<typeof signal<number>>;
  lagY: ReturnType<typeof signal<number>>;
  grabX: ReturnType<typeof signal<number>>;
  grabY: ReturnType<typeof signal<number>>;
  effect: WindowEffectHandle;
  dragging: boolean;
  // Where the window is (target) and where its far side is (far), logical px.
  targetX: number;
  targetY: number;
  farX: number;
  farY: number;
  velocityX: number;
  velocityY: number;
  poll: PollHandle | null;
  lastMs: number | null;
}

const jellies = new Map<string, Jelly>();

function jellyFor(window: WaylandWindow): Jelly {
  let jelly = jellies.get(window.id);
  if (jelly) {
    return jelly;
  }
  const lagX = signal(0);
  const lagY = signal(0);
  const grabX = signal(0);
  const grabY = signal(0);
  jelly = {
    active: signal(false),
    lagX,
    lagY,
    grabX,
    grabY,
    effect: compileWindowEffect({
      input: windowSource(),
      outsets: MAX_LAG_PX,
      alpha: "preserve",
      invalidate: { kind: "always" },
      pipeline: [
        shaderStage(loadShader("./src/effect/jelly-drag.frag"), {
          uniforms: { lag_px: [lagX, lagY], grab_px: [grabX, grabY] },
        }),
      ],
    }),
    dragging: false,
    targetX: 0,
    targetY: 0,
    farX: 0,
    farY: 0,
    velocityX: 0,
    velocityY: 0,
    poll: null,
    lastMs: null,
  };
  jellies.set(window.id, jelly);
  return jelly;
}

/** Advance the spring by `dt` seconds. Returns false once it has settled. */
function step(jelly: Jelly, dt: number): boolean {
  for (let left = dt; left > 0; left -= MAX_STEP_S) {
    const h = Math.min(left, MAX_STEP_S);
    jelly.velocityX +=
      (STIFFNESS * (jelly.targetX - jelly.farX) - DAMPING * jelly.velocityX) * h;
    jelly.velocityY +=
      (STIFFNESS * (jelly.targetY - jelly.farY) - DAMPING * jelly.velocityY) * h;
    jelly.farX += jelly.velocityX * h;
    jelly.farY += jelly.velocityY * h;
  }

  // Keep the lag inside the effect margin.
  let lagX = jelly.farX - jelly.targetX;
  let lagY = jelly.farY - jelly.targetY;
  const length = Math.hypot(lagX, lagY);
  if (length > MAX_LAG_PX) {
    lagX *= MAX_LAG_PX / length;
    lagY *= MAX_LAG_PX / length;
    jelly.farX = jelly.targetX + lagX;
    jelly.farY = jelly.targetY + lagY;
  }

  const speed = Math.hypot(jelly.velocityX, jelly.velocityY);
  if (!jelly.dragging && Math.hypot(lagX, lagY) < 0.1 && speed < 5) {
    jelly.lagX[1](0);
    jelly.lagY[1](0);
    return false;
  }
  jelly.lagX[1](lagX);
  jelly.lagY[1](lagY);
  return true;
}

function startPoll(window: WaylandWindow, jelly: Jelly) {
  if (jelly.poll) {
    return;
  }
  jelly.lastMs = null;
  jelly.poll = window.createPoll(
    1,
    (handle) => {
      // Cap the step so a stalled frame does not fling the window.
      const dt =
        jelly.lastMs === null
          ? 0
          : Math.min((handle.nowMs - jelly.lastMs) / 1000, 0.05);
      jelly.lastMs = handle.nowMs;
      if (!step(jelly, dt)) {
        handle.cancel();
        jelly.poll = null;
        jelly.active[1](false);
      }
    },
    { dirty: "none" },
  );
}

/** Feed every interactive window move here. */
export function jellyDragOnWindowMove(event: WindowMoveEvent) {
  const jelly = jellyFor(event.window);
  if (event.phase === "start") {
    jelly.grabX[1](event.startPointer.x - event.startRect.x);
    jelly.grabY[1](event.startPointer.y - event.startRect.y);
    jelly.farX = event.currentRect.x;
    jelly.farY = event.currentRect.y;
    jelly.velocityX = 0;
    jelly.velocityY = 0;
  }
  jelly.targetX = event.currentRect.x;
  jelly.targetY = event.currentRect.y;
  jelly.dragging = event.phase === "start" || event.phase === "update";
  if (jelly.dragging) {
    jelly.active[1](true);
    startPoll(event.window, jelly);
  }
}

/** The window effect: the jelly while the window moves or settles, else none. */
export function jellyDragEffect(
  window: WaylandWindow,
): WindowEffectAssignment | null {
  const jelly = jellyFor(window);
  return jelly.active() ? { replace: jelly.effect } : null;
}

/** Forget a closed window. */
export function jellyDragForget(window: WaylandWindow) {
  jellies.get(window.id)?.poll?.cancel();
  jellies.delete(window.id);
}
