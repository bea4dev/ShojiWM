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
  TRANSITION_MS,
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
/** The stacks keep this far from the screen's left and right edges (px). */
const SCREEN_MARGIN = 48;
/**
 * Opening and closing: a turned card fades out (or in) at its place during
 * this much of the eased progress, and its window fades in (or out) flat at
 * its real place during the rest. Turning it in 3D instead would sweep it
 * through the cards beside it.
 */
const SWAP_AT = 0.5;
/**
 * start-with-next, made visible: the switcher opens on the current window
 * (it zooms out into the middle card), then steps to the next one this long
 * after opening, near the end of the opening animation.
 */
const INTRO_STEP_AT_MS = TRANSITION_MS * 0.8;
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
  /** The windows' ids when the cards were last laid out (to spot a closed one). */
  ids: string[];
  /**
   * The whole layout's scale (1 or less), so no card leaves the screen for
   * any selection. Worked out once, on the first frame.
   */
  layoutScale: number | null;
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
  /** The opening step to the next window has not happened yet. */
  introStep: boolean;
}

export function createCoverflow(options: WindowSwitcherOptions): WindowSwitcher {
  return createWindowSwitcher(COVERFLOW_LAYOUT, options);
}

/** The opening step to the next window, if it is still due. */
function takeIntroStep(state: CoverflowState, controls: SwitcherControls) {
  if (state.introStep) {
    state.introStep = false;
    nextOnce(state, controls);
  }
}

/** One window on; a key pressed during the opening comes after its step. */
function next(state: CoverflowState, controls: SwitcherControls) {
  takeIntroStep(state, controls);
  nextOnce(state, controls);
}

/** `_next`: one window on, from the last back to the first. */
function nextOnce(state: CoverflowState, controls: SwitcherControls) {
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
  takeIntroStep(state, controls);
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
  // The selected card's traffic lights work, and a middle click closes a card.
  cardActions: true,

  // A window closed: drop its card. The window after it (or the last one)
  // slides into the middle and is selected.
  windowsChanged(state, controls) {
    const alive = new Set(state.windows.map((window) => window.id));
    const removed = state.ids.findIndex((id) => !alive.has(id));
    if (removed < 0) {
      return;
    }
    state.ids.splice(removed, 1);
    state.cards.splice(removed, 1);
    state.count = state.windows.length;
    if (state.count === 0) {
      return;
    }
    let selection = state.selection;
    if (removed < selection) {
      selection -= 1;
    }
    selection = Math.min(selection, state.count - 1);
    // Every card moves from where it is to its new place.
    for (const card of state.cards) {
      if (card) {
        card.selection = -1;
      }
    }
    state.selection = selection;
    state.changedAtMs = state.nowMs;
    state.flip = null;
    state.introStep = false;
    controls.select(selection);
  },
  cameraFovDegrees: CLUTTER_FOV_DEGREES,

  begin: (windows) => ({
    windows,
    ids: windows.map((window) => window.id),
    layoutScale: null,
    count: windows.length,
    nowMs: 0,
    selection: COVERFLOW_LAYOUT.initialSelection(windows.length),
    changedAtMs: 0,
    flip: null,
    pendingFlip: null,
    cards: windows.map(() => null),
    scrollRemainder: 0,
    swipeAppliedSteps: 0,
    introStep: windows.length > 1,
  }),

  // Opens on the current window; `tick` then steps to the next one.
  initialSelection: () => 0,

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

  tick(state, selected, dtMs, controls) {
    state.nowMs += dtMs;
    if (state.introStep && state.nowMs >= INTRO_STEP_AT_MS) {
      takeIntroStep(state, controls);
      selected = controls.selected;
    }
    const selection = mod(selected, state.count);
    if (selection !== state.selection) {
      state.selection = selection;
      state.changedAtMs = state.nowMs;
      state.flip = state.pendingFlip;
    }
    state.pendingFlip = null;
    // A flip takes up to twice the animation time.
    const longestS = state.flip ? ANIMATION_TIME_S * 2 : ANIMATION_TIME_S;
    return state.introStep || state.nowMs < state.changedAtMs + longestS * 1000;
  },

  settle(state) {
    // Closing: stop every card where it is now. The closing animation starts
    // from these poses, so a card mid-step does not jump.
    for (const card of state.cards) {
      if (card) {
        card.pivotX = still(valueAt(card.pivotX, state.nowMs));
        card.translationX = still(valueAt(card.translationX, state.nowMs));
        card.scale = still(valueAt(card.scale, state.nowMs));
        card.angle = still(valueAt(card.angle, state.nowMs));
        card.selection = state.selection;
      }
    }
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
      case "q": {
        // As in the extension: q closes the selected window.
        const window = controls.windows[mod(controls.selected, controls.windows.length)];
        window?.close();
        return true;
      }
    }
    return false;
  },

  // A quick Alt+Tab still switches to the next window.
  onSuperReleased: (state, controls) => {
    takeIntroStep(state, controls);
    controls.commit();
  },

  morph(_state, _index, from, to, t) {
    const turned = Math.abs(from.rotateY) > 0.5 || Math.abs(to.rotateY) > 0.5;
    if (!turned) {
      // The selected card is flat: it flies straight to (from) its window.
      return null;
    }
    if (t < SWAP_AT) {
      return { ...from, opacity: from.opacity * (1 - t / SWAP_AT) };
    }
    return { ...to, opacity: to.opacity * ((t - SWAP_AT) / (1 - SWAP_AT)) };
  },

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

/**
 * How far from the screen's centre the near edge of the card `steps` out
 * lands: the first neighbour's far end stops `NEIGHBOUR_GAP` beside the
 * selected card (seen from `cameraZ`), and each card further out is
 * `WINDOW_OFFSET_WIDTH` further. Gaps shrink with the layout.
 */
function stackOffset(
  selectedHalf: number,
  firstLength: number,
  steps: number,
  layout: number,
  cameraZ: number,
): number {
  const first = ((selectedHalf + NEIGHBOUR_GAP * layout) * (cameraZ + firstLength)) / cameraZ;
  return first + WINDOW_OFFSET_WIDTH * layout * (steps - 1);
}

/**
 * The largest layout scale (up to 1) at which, for every selection, the
 * outermost near edge on each side stays `SCREEN_MARGIN` inside the screen.
 * Only the sides can overflow: every card is at most half the screen tall
 * and recedes from its near edge, so the top and bottom always fit.
 */
function layoutScaleFor(windows: readonly WaylandWindow[], viewport: Viewport): number {
  const { width, height } = viewport;
  const cameraZ = cameraDistance(height, CLUTTER_FOV_DEGREES);
  const room = width / 2 - SCREEN_MARGIN;
  const widths = windows.map((window) => window.rect.width * fitScale(window.rect, viewport));
  const fits = (layout: number) =>
    widths.every((selectedWidth, selection) => {
      const left = selection;
      const right = widths.length - 1 - selection;
      const sides: [number, number][] = [
        [left, widths[selection - 1] ?? 0],
        [right, widths[selection + 1] ?? 0],
      ];
      return sides.every(
        ([cards, firstWidth]) =>
          cards === 0 ||
          stackOffset(
            (selectedWidth * layout) / 2,
            firstWidth * layout * FIRST_NEIGHBOUR_SCALE,
            cards,
            layout,
            cameraZ,
          ) <= room,
      );
    });
  if (fits(1)) {
    return 1;
  }
  // The offsets grow with the scale: search for the largest one that fits.
  let low = 0.05;
  let high = 1;
  for (let i = 0; i < 20; i++) {
    const middle = (low + high) / 2;
    if (fits(middle)) {
      low = middle;
    } else {
      high = middle;
    }
  }
  return low;
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
  const layout = (state.layoutScale ??= layoutScaleFor(state.windows, viewport));
  const fit = fitScale(rect, viewport) * layout;
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
  const selectedHalf = (selectedRect.width * fitScale(selectedRect, viewport) * layout) / 2;
  // The first neighbour on this side: its length into the screen.
  const firstRect = windowRect(state.windows[selection + side]) ?? rect;
  const firstLength =
    firstRect.width * fitScale(firstRect, viewport) * layout * FIRST_NEIGHBOUR_SCALE;
  // Its near edge, placed so its far end (seen through the 60° camera at
  // distance D) lands at the selected card's edge plus the gap:
  // centre + (near - centre) * D / (D + length) = edge.
  const cameraZ = cameraDistance(height, CLUTTER_FOV_DEGREES);
  const nearEdge =
    centerX + side * stackOffset(selectedHalf, firstLength, steps, layout, cameraZ);
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
