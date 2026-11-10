/**
 * Material 3 Expressive motion.
 *
 * Material 3 moves things on springs. An animation run by the config
 * (`window.animation`, a poll) can follow one: a spring is turned into an
 * easing over the time it takes to settle. Animations the compositor plays
 * itself (`scheduleAnimation`) take cubic Béziers only, so for those there are
 * the Béziers of Material 3 that come closest.
 */
import { cubicBezier, type EasingFunction } from "shoji_wm";

/** A spring from 0 to 1 as an easing, with the time it takes to settle. */
export interface Spring {
  easing: EasingFunction;
  /** Milliseconds. */
  duration: number;
}

/**
 * A spring with a damping ratio and a stiffness (unit mass), from rest at 0 to
 * rest at 1. It is cut where it stays within a thousandth of 1.
 */
export function spring(dampingRatio: number, stiffness: number): Spring {
  const omega = Math.sqrt(stiffness);
  const zeta = dampingRatio;
  const at = (t: number): number => {
    if (zeta < 1) {
      const damped = omega * Math.sqrt(1 - zeta * zeta);
      return (
        1 -
        Math.exp(-zeta * omega * t) *
          (Math.cos(damped * t) + ((zeta * omega) / damped) * Math.sin(damped * t))
      );
    }
    return 1 - Math.exp(-omega * t) * (1 + omega * t);
  };
  // The last moment it is more than a thousandth away, found from the end.
  let settle = 0;
  const step = 1 / 600;
  for (let t = 4; t > 0; t -= step) {
    if (Math.abs(at(t) - 1) > 0.001) {
      settle = t + step;
      break;
    }
  }
  return {
    easing: (p: number) => (p >= 1 ? 1 : p <= 0 ? 0 : at(p * settle)),
    duration: Math.round(settle * 1000),
  };
}

// The springs of Material 3 Expressive's motion scheme. Spatial ones move,
// resize and turn things and may overshoot; effects ones change colors and
// opacity and never do.
export const SPATIAL_FAST = spring(0.6, 800);
export const SPATIAL_DEFAULT = spring(0.8, 380);
export const SPATIAL_SLOW = spring(0.8, 200);
export const EFFECTS_FAST = spring(1, 3800);
export const EFFECTS_DEFAULT = spring(1, 1600);
export const EFFECTS_SLOW = spring(1, 800);

// Easing curves of Material 3, for the compositor's own animations.
export const EMPHASIZED_DECELERATE = cubicBezier(0.05, 0.7, 0.1, 1);
export const EMPHASIZED_ACCELERATE = cubicBezier(0.3, 0, 0.8, 0.15);
export const STANDARD = cubicBezier(0.2, 0, 0, 1);
/** Comes in a little past where it lands and back, like an expressive spatial spring. */
export const EXPRESSIVE_SPATIAL = cubicBezier(0.3, 1.25, 0.45, 1);

// The windows' own motion: quick out of the start and a long, soft landing,
// or the same going a little past and back.
export const WINDOW_EASE = cubicBezier(0.1, 0.9, 0.2, 1);
export const WINDOW_EASE_OVERSHOOT = cubicBezier(0.1, 1.1, 0.1, 1.1);
/** Milliseconds. */
export const WINDOW_DURATION = 250;
/** Opening and closing take longer, so the rise and the growing can be followed. Milliseconds. */
export const WINDOW_OPEN_CLOSE_DURATION = 350;
