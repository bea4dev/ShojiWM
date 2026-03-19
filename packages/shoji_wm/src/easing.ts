/**
 * Easing function mapping normalized progress in the `0..1` range to another
 * normalized progress value.
 */
export type EasingFunction = (progress: number) => number;

const NEWTON_ITERATIONS = 8;
const NEWTON_EPSILON = 1e-6;
const SUBDIVISION_EPSILON = 1e-7;
const SUBDIVISION_MAX_ITERATIONS = 12;

/**
 * Creates a CSS-compatible cubic-bezier easing function.
 *
 * Control points use the same coordinate system as CSS:
 * `cubic-bezier(x1, y1, x2, y2)`.
 *
 * @example
 * ```ts
 * const ease = cubicBezier(0.25, 0.1, 0.25, 1.0)
 * window.animation.start(open, {
 *   duration: seconds(0.25),
 *   easing: ease,
 * })
 * ```
 */
export function cubicBezier(
  x1: number,
  y1: number,
  x2: number,
  y2: number,
): EasingFunction {
  const sampleCurveX = (t: number) =>
    ((ax * t + bx) * t + cx) * t;
  const sampleCurveY = (t: number) =>
    ((ay * t + by) * t + cy) * t;
  const sampleCurveDerivativeX = (t: number) =>
    (3 * ax * t + 2 * bx) * t + cx;

  const cx = 3 * x1;
  const bx = 3 * (x2 - x1) - cx;
  const ax = 1 - cx - bx;

  const cy = 3 * y1;
  const by = 3 * (y2 - y1) - cy;
  const ay = 1 - cy - by;

  const solveCurveX = (x: number): number => {
    let t = x;

    for (let i = 0; i < NEWTON_ITERATIONS; i += 1) {
      const xEstimate = sampleCurveX(t) - x;
      if (Math.abs(xEstimate) < NEWTON_EPSILON) {
        return t;
      }

      const derivative = sampleCurveDerivativeX(t);
      if (Math.abs(derivative) < NEWTON_EPSILON) {
        break;
      }

      t -= xEstimate / derivative;
    }

    let lower = 0;
    let upper = 1;
    t = x;

    for (let i = 0; i < SUBDIVISION_MAX_ITERATIONS; i += 1) {
      const xEstimate = sampleCurveX(t);
      if (Math.abs(xEstimate - x) < SUBDIVISION_EPSILON) {
        return t;
      }

      if (x > xEstimate) {
        lower = t;
      } else {
        upper = t;
      }

      t = (upper - lower) * 0.5 + lower;
    }

    return t;
  };

  return (progress: number) => {
    const clamped = clampUnit(progress);
    if (clamped === 0 || clamped === 1) {
      return clamped;
    }

    return sampleCurveY(solveCurveX(clamped));
  };
}

/**
 * No easing.
 */
export const linear: EasingFunction = (progress) => clampUnit(progress);

/**
 * CSS `ease`: `cubic-bezier(0.25, 0.1, 0.25, 1.0)`
 */
export const ease = cubicBezier(0.25, 0.1, 0.25, 1);

/**
 * CSS `ease-in`: `cubic-bezier(0.42, 0, 1, 1)`
 */
export const easeIn = cubicBezier(0.42, 0, 1, 1);

/**
 * CSS `ease-out`: `cubic-bezier(0, 0, 0.58, 1)`
 */
export const easeOut = cubicBezier(0, 0, 0.58, 1);

/**
 * CSS `ease-in-out`: `cubic-bezier(0.42, 0, 0.58, 1)`
 */
export const easeInOut = cubicBezier(0.42, 0, 0.58, 1);

/**
 * A stronger ease-out useful for pop-in style window animations.
 */
export const easeOutCubic = cubicBezier(0.22, 1, 0.36, 1);

/**
 * A stronger ease-in-out useful for symmetric focus transitions.
 */
export const easeInOutCubic = cubicBezier(0.65, 0, 0.35, 1);

/**
 * A more dramatic overshoot-like entrance without leaving the `0..1` range.
 */
export const easeOutExpo = cubicBezier(0.16, 1, 0.3, 1);

function clampUnit(value: number): number {
  if (!Number.isFinite(value)) {
    return 0;
  }
  if (value < 0) {
    return 0;
  }
  if (value > 1) {
    return 1;
  }
  return value;
}
