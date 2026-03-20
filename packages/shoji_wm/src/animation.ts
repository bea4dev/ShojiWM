import { computed, signal, type ReadonlySignal, type Signal } from "./signals";
import { markWindowDirty } from "./runtime-hooks";
import { createManagedPoll, type PollHandle } from "./scheduler";

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
  /** Scheduler tick interval in milliseconds. Defaults to `16`. */
  intervalMs?: number;
  /** Optional easing function applied to normalized progress before interpolation. */
  easing?: (progress: number) => number;
}

/**
 * Per-window animation controller.
 *
 * The same {@link AnimationVariable} can be used across many windows; each
 * window keeps its own progress value and scheduled task.
 */
export interface WindowAnimationController {
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

interface AnimationEntry {
  progress: Signal<number>;
  poll?: PollHandle;
}

const linear = (value: number) => value;

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
export function createWindowAnimationController(windowId: string): WindowAnimationController {
  return createWindowAnimationControllerWithStore(windowId, new Map());
}

export function createWindowAnimationControllerWithStore(
  windowId: string,
  entries: Map<symbol, AnimationEntry>,
): WindowAnimationController {

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
      entry.poll?.cancel();

      const duration = Math.max(1, Math.floor(options.duration));
      const easing = options.easing ?? linear;
      const intervalMs = Math.max(1, Math.floor(options.intervalMs ?? 16));
      const from = options.from ?? entry.progress.peek();
      const to = options.to ?? 1;

      entry.progress.value = from;
      markWindowDirty(windowId);

      let startedAt: number | null = null;
      const startPoll = createManagedPoll(intervalMs, (handle) => {
        if (startedAt === null) {
          startedAt = handle.nowMs;
        }

        const elapsed = Math.max(0, handle.nowMs - startedAt);
        const progress = Math.min(1, elapsed / duration);
        const eased = normalizeEasedProgress(easing(progress), progress);
        entry.progress.value = from + (to - from) * eased;
        markWindowDirty(windowId);

        if (progress >= 1) {
          entry.progress.value = to;
          markWindowDirty(windowId);
          handle.cancel();
          entry.poll = undefined;
        }
      }, "none");
      entry.poll = startPoll;
    },
    stop(variable) {
      const entry = entries.get(variable.id);
      entry?.poll?.cancel();
      if (entry) {
        entry.poll = undefined;
        markWindowDirty(windowId);
      }
    },
    set(variable, value) {
      const entry = ensureEntry(variable);
      entry.poll?.cancel();
      entry.poll = undefined;
      entry.progress.value = value;
      markWindowDirty(windowId);
    },
    running(variable) {
      return entries.get(variable.id)?.poll !== undefined;
    },
  };
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
