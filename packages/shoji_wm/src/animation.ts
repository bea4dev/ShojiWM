import { computed, signal, type ReadonlySignal, type Signal } from "./signals";
import { markWindowDirty } from "./runtime-hooks";

/**
 * Stable token used to address a logical animation track on a per-window basis.
 *
 * Create the token once at module scope, then reuse it from event handlers and
 * decoration code.
 *
 * @example
 * ```ts
 * const open = animationVariable("open")
 * ```
 */
export interface AnimationVariable {
  readonly id: symbol;
  readonly debugName?: string;
}

/**
 * Parameters describing how an animation should advance from `from` to `to`.
 *
 * Values are interpreted as normalized progress in the `0..1` range by default,
 * but any finite numeric range is allowed.
 */
export interface AnimationStartOptions {
  /** Total animation time in milliseconds. */
  duration: number;
  /** Starting value. Defaults to the variable's current value. */
  from?: number;
  /** Target value. Defaults to `1`. */
  to?: number;
  /** Optional easing function applied to normalized progress before interpolation. */
  easing?: (progress: number) => number;
}

/**
 * Per-window animation controller.
 *
 * The same {@link AnimationVariable} can be used across many windows; each
 * window keeps its own progress value and scheduled task.
 */
export interface AnimationController {
  /**
   * Returns a readonly progress signal for the given animation variable.
   *
   * @example
   * ```ts
   * const t = window.animation.variable(open)
   * const scale = t((x) => 0.8 + x * 0.2)
   * ```
   */
  variable(variable: AnimationVariable): ReadonlySignal<number>;

  /**
   * Alias for {@link variable}.
   */
  signal(variable: AnimationVariable): ReadonlySignal<number>;

  /**
   * Starts or restarts an animation for this window.
   *
   * If the same variable is already animating, the previous scheduled task is
   * cancelled and replaced by the new one.
   *
   * If `from` is omitted, the animation continues from the variable's current
   * value. This makes direction changes and retargeting smooth.
   *
   * @example
   * ```ts
   * const open = animationVariable("open")
   *
   * WINDOW_MANAGER.event.onOpen((window) => {
   *   window.animation.start(open, {
   *     duration: seconds(0.18),
   *     from: 0,
   *     to: 1,
   *   })
   * })
   *
   * WINDOW_MANAGER.event.onFocus((window, focused) => {
   *   window.animation.start(open, {
   *     duration: milliseconds(120),
   *     to: focused ? 1 : 0,
   *   })
   * })
   * ```
   */
  start(variable: AnimationVariable, options: AnimationStartOptions): void;

  /**
   * Stops the scheduled task for the given variable, preserving the current
   * value.
   */
  stop(variable: AnimationVariable): void;

  /**
   * Immediately sets the variable to a value and cancels any running task for
   * the same variable.
   */
  set(variable: AnimationVariable, value: number): void;

  /**
   * Returns `true` while the variable currently has an active scheduled task.
   */
  running(variable: AnimationVariable): boolean;
}

export type WindowAnimationController = AnimationController;

interface AnimationEntry {
  progress: Signal<number>;
  timeline?: AnimationTimeline;
}

interface AnimationTimeline {
  startedAtMs: number;
  durationMs: number;
  from: number;
  to: number;
  easing: (progress: number) => number;
}

const linear = (value: number) => value;
const activeAnimationEntries = new Set<AnimationEntry>();
let currentAnimationFrameMs = 0;

/**
 * Creates a stable animation token.
 *
 * @param debugName Optional debug-facing name to help identify the variable in
 * logs or tooling.
 */
export function animationVariable(debugName?: string): AnimationVariable {
  return {
    id: Symbol(debugName ?? "animation-variable"),
    debugName,
  };
}

/**
 * Convenience helper for explicit millisecond literals.
 */
export function milliseconds(value: number): number {
  return value;
}

/**
 * Convenience helper for second-based durations.
 *
 * @example
 * ```ts
 * window.animation.start(open, {
 *   duration: seconds(0.25),
 * })
 * ```
 */
export function seconds(value: number): number {
  return value * 1000;
}

/**
 * Creates the animation controller bound to a specific window id.
 *
 * Consumers usually receive this indirectly through `window.animation`.
 */
export function createAnimationController(markDirty: () => void): AnimationController {
  return createAnimationControllerWithStore(markDirty, new Map());
}

export function createAnimationControllerWithStore(
  _markDirty: () => void,
  entries: Map<symbol, AnimationEntry>,
): AnimationController {

  const ensureEntry = (variable: AnimationVariable): AnimationEntry => {
    let entry = entries.get(variable.id);
    if (!entry) {
      entry = { progress: signal(0) };
      entries.set(variable.id, entry);
    }
    return entry;
  };

  const variableSignal = (variable: AnimationVariable): ReadonlySignal<number> =>
    computed(() => ensureEntry(variable).progress());

  return {
    variable: variableSignal,
    signal: variableSignal,
    start(variable, options) {
      const entry = ensureEntry(variable);

      const duration = Math.max(1, Math.floor(options.duration));
      const easing = options.easing ?? linear;
      const from = options.from ?? entry.progress.peek();
      const to = options.to ?? 1;

      entry.progress.value = from;
      entry.timeline = {
        startedAtMs: currentAnimationFrameMs,
        durationMs: duration,
        from,
        to,
        easing,
      };
      activeAnimationEntries.add(entry);
    },
    stop(variable) {
      const entry = entries.get(variable.id);
      if (entry) {
        entry.timeline = undefined;
        activeAnimationEntries.delete(entry);
      }
    },
    set(variable, value) {
      const entry = ensureEntry(variable);
      entry.timeline = undefined;
      activeAnimationEntries.delete(entry);
      entry.progress.value = value;
    },
    running(variable) {
      return entries.get(variable.id)?.timeline !== undefined;
    },
  };
}

export function createWindowAnimationController(windowId: string): WindowAnimationController {
  return createWindowAnimationControllerWithStore(windowId, new Map());
}

export function createWindowAnimationControllerWithStore(
  windowId: string,
  entries: Map<symbol, AnimationEntry>,
): WindowAnimationController {
  return createAnimationControllerWithStore(() => markWindowDirty(windowId), entries);
}

export function advanceAnimationFrame(nowMs: number): boolean {
  currentAnimationFrameMs = nowMs;
  if (activeAnimationEntries.size === 0) {
    return false;
  }

  for (const entry of Array.from(activeAnimationEntries)) {
    const timeline = entry.timeline;
    if (!timeline) {
      activeAnimationEntries.delete(entry);
      continue;
    }

    const elapsed = Math.max(0, nowMs - timeline.startedAtMs);
    const progress = Math.min(1, elapsed / timeline.durationMs);
    const eased = normalizeEasedProgress(timeline.easing(progress), progress);
    entry.progress.value = timeline.from + (timeline.to - timeline.from) * eased;

    if (progress >= 1) {
      entry.progress.value = timeline.to;
      entry.timeline = undefined;
      activeAnimationEntries.delete(entry);
    }
  }

  return activeAnimationEntries.size > 0;
}

export function hasActiveAnimations(): boolean {
  return activeAnimationEntries.size > 0;
}

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

function normalizeEasedProgress(value: number, fallback: number): number {
  if (!Number.isFinite(value)) {
    return fallback;
  }

  return value;
}
