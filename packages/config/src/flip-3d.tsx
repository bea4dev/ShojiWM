/**
 * Flip 3D: a Windows Vista style window switcher (a `SwitcherLayout`, see
 * `./window-switcher.tsx` for what every switcher shares).
 *
 * `Super+Tab` lifts every window into a receding diagonal stack, most
 * recently used in front. Hold `Super` and press `Tab` to flip through it,
 * release `Super` to switch to the front window. While it is open:
 *
 * - `Tab` / `Shift+Tab`, arrow keys, the mouse wheel, touchpad scrolling and
 *   three-finger swipes flip the stack
 * - `Return` / `Space`, a click on a window, or releasing `Super` switches
 * - `Escape` returns to the desktop unchanged
 */
import {
  createWindowSwitcher,
  cameraDistance,
  clamp01,
  mod,
  type Pose,
  type Rect,
  type SwitcherControls,
  type SwitcherLayout,
  type WindowSwitcher,
  type WindowSwitcherOptions,
} from "./window-switcher";

/** How quickly flipping follows the selection (time constant, ms). */
const SCROLL_TIME_CONSTANT_MS = 70;
/** Windows shown in the stack at once. */
const VISIBLE_WINDOWS = 7;
/** How far the desktop dims behind the stack. */
const DIM_ALPHA = 0.45;
// The stack, after Windows Vista: the front window low on the right, the rest
// receding to the upper left, every window turned about 40° and seen slightly
// from above through a long lens (almost no perspective within a window).
/** Each window's turn about the vertical axis (degrees; the right edge recedes). */
const TURN_DEGREES = 20;
/** Each window's tilt about the horizontal axis (degrees; far edges rise). */
const PITCH_DEGREES = 6;
/** Where the front window's centre lands, as fractions of the output from its centre (+y up). */
const FRONT_X = 0.13;
const FRONT_Y = -0.05;
/** Each window further back moves on screen by this much (fractions of the output). */
const STEP_X = -0.085;
const STEP_Y = 0.048;
/** Each window further back is drawn this much smaller: scale 1 / (1 + slot × this). */
const SHRINK_PER_SLOT = 0.3;
/** The front window's largest size, as fractions of the output. */
const FRONT_MAX_WIDTH = 0.5;
const FRONT_MAX_HEIGHT = 0.5;
/** Touchpad scrolling / swiping distance per window (logical px). */
const SCROLL_STEP_PX = 80;
const SWIPE_STEP_PX = 140;

interface FlipState {
  count: number;
  /** Where the stack is, easing towards the selection. */
  scroll: number;
  scrollRemainder: number;
  swipeAppliedSteps: number;
}

export function createFlip3D(options: WindowSwitcherOptions): WindowSwitcher {
  return createWindowSwitcher(FLIP_3D_LAYOUT, options);
}

function flip(controls: SwitcherControls, steps: number) {
  if (steps !== 0) {
    controls.select(controls.selected + steps);
  }
}

const FLIP_3D_LAYOUT: SwitcherLayout<FlipState> = {
  name: "flip-3d",
  dimAlpha: DIM_ALPHA,

  begin: (windows) => ({
    count: windows.length,
    scroll: 0,
    scrollRemainder: 0,
    swipeAppliedSteps: 0,
  }),

  // Like Alt+Tab, the previous window comes to the front: the stack flips
  // once as it opens.
  initialSelection: (count) => (count > 1 ? 1 : 0),

  pose: (state, index, rect, { width, height }) =>
    slotPose(slotOf(index, state.scroll, state.count), state.count, rect, width, height),

  tick(state, selected, dtMs) {
    const distance = selected - state.scroll;
    if (Math.abs(distance) < 0.001) {
      state.scroll = selected;
    } else {
      state.scroll += distance * (1 - Math.exp(-dtMs / SCROLL_TIME_CONSTANT_MS));
    }
    return state.scroll !== selected;
  },

  settle(state, selected) {
    state.scroll = selected;
  },

  onKey(_state, event, controls) {
    switch (event.key) {
      case "Tab":
      case "ISO_Left_Tab":
        flip(controls, event.modifiers.shift ? -1 : 1);
        return true;
      case "Right":
      case "Down":
        flip(controls, 1);
        return true;
      case "Left":
      case "Up":
        flip(controls, -1);
        return true;
    }
    return false;
  },

  onSuperReleased: (_state, controls) => controls.commit(),

  onScroll(state, event, controls) {
    if (event.source === "wheel") {
      const clicks = (event.discreteY ?? Math.sign(event.deltaY) * 120) / 120;
      flip(controls, Math.sign(clicks) * Math.max(1, Math.round(Math.abs(clicks))));
      return;
    }
    state.scrollRemainder += event.deltaY;
    const steps = Math.trunc(state.scrollRemainder / SCROLL_STEP_PX);
    if (steps !== 0) {
      state.scrollRemainder -= steps * SCROLL_STEP_PX;
      flip(controls, steps);
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
      flip(controls, steps - state.swipeAppliedSteps);
      state.swipeAppliedSteps = steps;
    }
  },
};

/**
 * The window's place in the stack. `slot` 0 is the front; the stack recedes
 * up and to the left. Slots below 0 have flipped past the front and fade
 * out; the last visible slot fades in.
 */
function slotPose(slot: number, count: number, rect: Rect, width: number, height: number): Pose {
  const fit = Math.min(
    1,
    (width * FRONT_MAX_WIDTH) / rect.width,
    (height * FRONT_MAX_HEIGHT) / rect.height,
  );
  // Depth that draws the slot 1 / (1 + slot × SHRINK_PER_SLOT) as large,
  // and the world position that puts its centre on its spot on screen.
  const depth = slot * SHRINK_PER_SLOT * cameraDistance(height);
  const depthScale = 1 + slot * SHRINK_PER_SLOT;
  const last = Math.min(count, VISIBLE_WINDOWS);
  let opacity = 1;
  if (slot < 0) {
    opacity = clamp01(1 + slot * 2);
  } else if (slot > last - 1) {
    opacity = clamp01((last - 0.5 - slot) * 2);
  }
  return {
    x: (FRONT_X + slot * STEP_X) * width * depthScale,
    y: (FRONT_Y + slot * STEP_Y) * height * depthScale,
    z: -depth,
    rotateX: PITCH_DEGREES,
    rotateY: TURN_DEGREES,
    scale: fit,
    opacity,
  };
}

function slotOf(index: number, scroll: number, count: number): number {
  return mod(index - scroll + 0.5, count) - 0.5;
}
