/**
 * Coverflow: a port of the GNOME extension "Coverflow Alt-Tab"
 * (github.com/dsheeler/CoverflowAltTab, src/coverflowSwitcher.js) as a
 * `SwitcherLayout` (see `./window-switcher.tsx` for what every switcher
 * shares: the opening and closing, the dimmed backdrop, the white glow
 * around the selected window).
 *
 * The selected window faces you in the centre. The windows before it turn
 * 90° and stack to the left, the windows after it stack to the right, each
 * one further out smaller and 50 px further from the centre. (Changed from
 * the extension: the stacks start right beside the selected card, and the
 * first neighbour keeps full size; see `cardTarget`.) Past the last
 * window the whole stack flips back to the first one ("Flip Stack", the
 * extension's default), and the other way round.
 *
 * The extension moves each preview with Clutter: a pivot point, a
 * translation, a scale and a turn about the pivot, each eased on its own.
 * This file keeps those four values per window, eases them the same way, and
 * turns them into a `Pose` every frame.
 */
import type { WaylandWindow } from "shoji_wm";
import {
  cameraDistance,
  createWindowSwitcher,
  mod,
  type Pose,
  type Rect,
  type SwitcherControls,
  type SwitcherLayout,
  type Viewport,
  type WindowSwitcher,
  type WindowSwitcherOptions,
  SWITCHER_DIM_ALPHA,
} from "./window-switcher";

// The extension's defaults (schemas/org.gnome.shell.extensions.coverflowalttab.gschema.xml).
/** animation-time: one step, in seconds. */
const ANIMATION_TIME_S = 0.2;
/** preview-to-monitor-ratio: the largest preview, as a fraction of the monitor. */
const PREVIEW_TO_MONITOR_RATIO = 0.5;
/** coverflow-preview-scaling-factor: each window further out is this much smaller. */
const PREVIEW_SCALING_FACTOR = 0.8;
/** coverflow-window-angle: the side windows' turn (degrees). */
const WINDOW_ANGLE = 90;
/** coverflow-window-offset-width: each window further out moves this far (px). */
const WINDOW_OFFSET_WIDTH = 50;
/** offset: the previews' centre below the monitor's centre (px). */
const OFFSET = 0;
/**
 * GNOME Shell's stage camera: Clutter's 60° field of view. It gives the side
 * windows their strong perspective.
 */
const CLUTTER_FOV_DEGREES = 60;
// Our own changes to the extension's layout (Abhishek asked for a tighter,
// bigger stack than the defaults give with narrow tiled windows):
/** The first neighbour's inner end stops this far from the selected card (px). */
const NEIGHBOUR_GAP = 15;
/** The first neighbour keeps the selected card's size (no 0.8 shrink). */
const FIRST_NEIGHBOUR_SCALE = 1;
/** A small window may grow this much to fill its half-screen box. */
const MAX_UPSCALE = 1.5;
/** Touchpad scrolling / swiping distance per window (logical px). */
const SCROLL_STEP_PX = 80;
const SWIPE_STEP_PX = 140;

/** One eased value, like a Clutter property transition. */
interface Tween {
  from: number;
  to: number;
  startMs: number;
  durationMs: number;
}

/** The Clutter values the extension animates on a preview. */
interface Card {
  /** pivot_point.x: 0 = left edge, 0.5 = centre, 1 = right edge. */
  pivotX: Tween;
  translationX: Tween;
  scale: Tween;
  /** rotation_angle_y (degrees). */
  angle: Tween;
  /** The selection this card heads for. */
  selection: number;
}

type FlipDirection = "to-left" | "to-right";

interface CoverflowState {
  windows: readonly WaylandWindow[];
  count: number;
  /** The layout's own clock (ms). */
  nowMs: number;
  /** The selection the cards head for, and when it changed. */
  selection: number;
  changedAtMs: number;
  /** The last change wrapped round, and which way the stack flips. */
  flip: FlipDirection | null;
  /** The next change wraps round (set just before it is made). */
  pendingFlip: FlipDirection | null;
  cards: (Card | null)[];
  scrollRemainder: number;
  swipeAppliedSteps: number;
}

export function createCoverflow(options: WindowSwitcherOptions): WindowSwitcher {
  return createWindowSwitcher(COVERFLOW_LAYOUT, options);
}

/** `_next`: one window on, from the last back to the first. */
function next(state: CoverflowState, controls: SwitcherControls) {
  const count = controls.windows.length;
  if (count <= 1) {
    return;
  }
  const current = mod(controls.selected, count);
  if (current === count - 1) {
    state.pendingFlip = "to-left";
    controls.select(0);
  } else {
    controls.select(current + 1);
  }
}

/** `_previous`: one window back, from the first round to the last. */
function previous(state: CoverflowState, controls: SwitcherControls) {
  const count = controls.windows.length;
  if (count <= 1) {
    return;
  }
  const current = mod(controls.selected, count);
  if (current === 0) {
    state.pendingFlip = "to-right";
    controls.select(count - 1);
  } else {
    controls.select(current - 1);
  }
}

function step(state: CoverflowState, controls: SwitcherControls, steps: number) {
  for (let i = 0; i < Math.abs(steps); i++) {
    if (steps > 0) {
      next(state, controls);
    } else {
      previous(state, controls);
    }
  }
}

const COVERFLOW_LAYOUT: SwitcherLayout<CoverflowState> = {
  name: "coverflow",
  dimAlpha: SWITCHER_DIM_ALPHA,
  cameraFovDegrees: CLUTTER_FOV_DEGREES,

  begin: (windows) => ({
    windows,
    count: windows.length,
    nowMs: 0,
    selection: COVERFLOW_LAYOUT.initialSelection(windows.length),
    changedAtMs: 0,
    flip: null,
    pendingFlip: null,
    cards: windows.map(() => null),
    scrollRemainder: 0,
    swipeAppliedSteps: 0,
  }),

  // start-with-next: the switcher opens on the previous window.
  initialSelection: (count) => (count > 1 ? 1 : 0),

  pose(state, index, rect, viewport) {
    const target = cardTarget(index, state, rect, viewport);
    let card = state.cards[index];
    if (!card) {
      // First frame: start where the selection is, without moving.
      card = {
        pivotX: still(target.pivotX),
        translationX: still(target.translationX),
        scale: still(target.scale),
        angle: still(target.angle),
        selection: state.selection,
      };
      state.cards[index] = card;
    } else if (card.selection !== state.selection) {
      // The selection moved: ease from where the card is now.
      const durationMs = cardDuration(index, state) * 1000;
      const at = state.changedAtMs;
      card.pivotX = retarget(card.pivotX, target.pivotX, at, durationMs);
      card.translationX = retarget(card.translationX, target.translationX, at, durationMs);
      card.scale = retarget(card.scale, target.scale, at, durationMs);
      card.angle = retarget(card.angle, target.angle, at, durationMs);
      card.selection = state.selection;
    }
    return clutterPose(
      valueAt(card.pivotX, state.nowMs),
      valueAt(card.translationX, state.nowMs),
      valueAt(card.scale, state.nowMs),
      valueAt(card.angle, state.nowMs),
      rect,
    );
  },

  tick(state, selected, dtMs) {
    state.nowMs += dtMs;
    const selection = mod(selected, state.count);
    if (selection !== state.selection) {
      state.selection = selection;
      state.changedAtMs = state.nowMs;
      state.flip = state.pendingFlip;
    }
    state.pendingFlip = null;
    // A flip takes up to twice the animation time.
    const longestS = state.flip ? ANIMATION_TIME_S * 2 : ANIMATION_TIME_S;
    return state.nowMs < state.changedAtMs + longestS * 1000;
  },

  settle(state) {
    // Closing: jump to the end of every running transition.
    state.nowMs = Number.MAX_SAFE_INTEGER / 2;
  },

  onKey(state, event, controls) {
    switch (event.key) {
      case "Tab":
      case "ISO_Left_Tab":
        if (event.modifiers.shift) {
          previous(state, controls);
        } else {
          next(state, controls);
        }
        return true;
      case "Right":
      case "Down":
        next(state, controls);
        return true;
      case "Left":
      case "Up":
        previous(state, controls);
        return true;
    }
    return false;
  },

  onSuperReleased: (_state, controls) => controls.commit(),

  onScroll(state, event, controls) {
    if (event.source === "wheel") {
      const clicks = (event.discreteY ?? Math.sign(event.deltaY) * 120) / 120;
      step(state, controls, Math.sign(clicks) * Math.max(1, Math.round(Math.abs(clicks))));
      return;
    }
    state.scrollRemainder += event.deltaY;
    const steps = Math.trunc(state.scrollRemainder / SCROLL_STEP_PX);
    if (steps !== 0) {
      state.scrollRemainder -= steps * SCROLL_STEP_PX;
      step(state, controls, steps);
    }
  },

  onSwipe(state, event, controls) {
    if (event.phase === "begin") {
      state.swipeAppliedSteps = 0;
      return;
    }
    const travel = Math.abs(event.totalX) > Math.abs(event.totalY) ? -event.totalX : event.totalY;
    const steps = Math.trunc(travel / SWIPE_STEP_PX);
    if (steps !== state.swipeAppliedSteps) {
      step(state, controls, steps - state.swipeAppliedSteps);
      state.swipeAppliedSteps = steps;
    }
  },
};

/**
 * How long card `index` takes to reach its new place (seconds). A plain step
 * takes the animation time. A flip (`_onFlipIn`) staggers the side cards: the
 * cards that travel furthest take up to twice as long. The selected card
 * always takes the animation time (`_animatePreviewToMid`).
 */
function cardDuration(index: number, state: CoverflowState): number {
  if (index === state.selection || !state.flip) {
    return ANIMATION_TIME_S;
  }
  const count = state.count;
  return (
    ANIMATION_TIME_S *
    2 *
    (state.flip === "to-right" ? (index + 1) / count : 1 - index / count)
  );
}

interface CardValues {
  pivotX: number;
  translationX: number;
  scale: number;
  angle: number;
}

/** The window's scale: it fills its half-screen box, by width or by height. */
function fitScale(rect: Rect, { width, height }: Viewport): number {
  const boxWidth = width * PREVIEW_TO_MONITOR_RATIO;
  const boxHeight = height * PREVIEW_TO_MONITOR_RATIO;
  return Math.min(MAX_UPSCALE, boxWidth / rect.width, boxHeight / rect.height);
}

function windowRect(window: WaylandWindow | undefined): Rect | null {
  return window ? { ...window.rect } : null;
}

/**
 * Where card `index` goes for the current selection. The selected card faces
 * you in the middle (`_animatePreviewToMid`). The side cards turn 90° about
 * their outer edge, which stays on the screen plane, so it is their nearest
 * and tallest edge (`_animatePreviewToSide`).
 *
 * Unlike the extension, the side stacks follow the selected card: the first
 * neighbour is placed so its far (inner) end ends just beside the selected
 * card's edge, and it keeps full size. The cards further out step 50 px
 * outwards and shrink 0.8 per step, as in the extension.
 *
 *      near edge                          near edge
 *         │╲     ┌──────────────┐      ╱│
 *         │ ╲ gap│   selected   │gap  ╱ │
 *         │ ╱    │     card     │     ╲ │
 *         │╱     └──────────────┘      ╲│
 */
function cardTarget(
  index: number,
  state: CoverflowState,
  rect: Rect,
  viewport: Viewport,
): CardValues {
  const { selection, count } = state;
  const fit = fitScale(rect, viewport);
  if (index === selection || count <= 1) {
    return { pivotX: 0.5, translationX: 0, scale: fit, angle: 0 };
  }
  const { width, height } = viewport;
  const centerX = width / 2;
  const distance = index - selection;
  const side = distance < 0 ? -1 : 1;
  const steps = Math.abs(distance);
  // Half the selected card's width on screen.
  const selectedRect = windowRect(state.windows[selection]) ?? rect;
  const selectedHalf = (selectedRect.width * fitScale(selectedRect, viewport)) / 2;
  // The first neighbour on this side: its length into the screen.
  const firstRect = windowRect(state.windows[selection + side]) ?? rect;
  const firstLength = firstRect.width * fitScale(firstRect, viewport) * FIRST_NEIGHBOUR_SCALE;
  // Its near edge, placed so its far end (seen through the 60° camera at
  // distance D) lands at the selected card's edge plus the gap:
  // centre + (near - centre) * D / (D + length) = edge.
  const cameraZ = cameraDistance(height, CLUTTER_FOV_DEGREES);
  const nearOffset = ((selectedHalf + NEIGHBOUR_GAP) * (cameraZ + firstLength)) / cameraZ;
  const nearEdge =
    centerX + side * (nearOffset + WINDOW_OFFSET_WIDTH * (steps - 1));
  const scale = fit * FIRST_NEIGHBOUR_SCALE * Math.pow(PREVIEW_SCALING_FACTOR, steps - 1);
  if (side < 0) {
    // Pivot on the left (outer) edge.
    return {
      pivotX: 0,
      translationX: nearEdge - (centerX - rect.width / 2),
      scale,
      angle: WINDOW_ANGLE,
    };
  }
  // Pivot on the right (outer) edge.
  return {
    pivotX: 1,
    translationX: nearEdge - (centerX + rect.width / 2),
    scale,
    angle: -WINDOW_ANGLE,
  };
}

/**
 * The pose of a Clutter actor centred on the monitor (its top-left corner at
 * the centre minus half its size), moved by `translationX`, then scaled and
 * turned about its pivot point.
 */
function clutterPose(
  pivotX: number,
  translationX: number,
  scale: number,
  angle: number,
  rect: Rect,
): Pose {
  // The pivot point, in world units from the monitor's centre.
  const pivot = -rect.width / 2 + translationX + pivotX * rect.width;
  // From the pivot to the window's centre, scaled, then turned.
  const lever = scale * (0.5 - pivotX) * rect.width;
  const radians = (angle * Math.PI) / 180;
  return {
    x: pivot + lever * Math.cos(radians),
    y: -OFFSET,
    z: -lever * Math.sin(radians),
    rotateX: 0,
    rotateY: angle,
    scale,
    opacity: 1,
  };
}

function still(value: number): Tween {
  return { from: value, to: value, startMs: 0, durationMs: 0 };
}

/** A new transition from where the old one is at `atMs` (as Clutter does). */
function retarget(tween: Tween, to: number, atMs: number, durationMs: number): Tween {
  return { from: valueAt(tween, atMs), to, startMs: atMs, durationMs };
}

/** The extension's default easing ("ease-out-cubic"). */
function valueAt(tween: Tween, nowMs: number): number {
  if (tween.durationMs <= 0 || nowMs >= tween.startMs + tween.durationMs) {
    return tween.to;
  }
  const t = Math.max(0, (nowMs - tween.startMs) / tween.durationMs);
  const eased = 1 - Math.pow(1 - t, 3);
  return tween.from + (tween.to - tween.from) * eased;
}
