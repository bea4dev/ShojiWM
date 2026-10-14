import { computed, signal, type ReadonlySignal, type Signal } from "./signals";
import { markWindowDirty, withoutCompositionOwnership } from "./runtime-hooks";

/**
 * Stable token used to address a logical animation track on a per-window basis.
 *
 * Create the token once at module scope, then reuse it from event handlers and
 * composition code.
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

export type AnimationRepeatMode = "loop" | "ping-pong";

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
  /** Repeat behavior after reaching the target value. */
  repeat?: AnimationRepeatMode;
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
   * COMPOSITOR.event.onOpen((window) => {
   *   window.animation.start(open, {
   *     duration: seconds(0.18),
   *     from: 0,
   *     to: 1,
   *   })
   * })
   *
   * COMPOSITOR.event.onFocus((window, focused) => {
   *   window.animation.start(open, {
   *     duration: milliseconds(120),
   *     to: focused ? 1 : 0,
   *   })
   * })
   * ```
   *
   * The returned promise resolves with `true` when the animation reaches its
   * target, or `false` when `start`, `stop` or `set` cuts it short (a
   * repeating animation only ends that way).
   * 返る Promise は目標値に達すると `true`、`start`・`stop`・`set` で中断されると
   * `false` で解決します（繰り返しアニメーションは中断でのみ終わります）。
   *
   * @example
   * ```ts
   * drag.animation.start(fade, { to: 0, duration: 150 }).then((done) => {
   *   if (done) view.dispose();
   * });
   * ```
   */
  start(variable: AnimationVariable, options: AnimationStartOptions): Promise<boolean>;

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
  /** Resolves the promise `start()` returned for the current timeline. */
  settle?: (completed: boolean) => void;
  /** The output whose clock drives this entry; see {@link AnimationClock}. */
  output?: () => string | undefined;
}

/**
 * Where animations take their time from. Time is kept per output (the
 * presentation time of the frames it shows), so an animation owned by a
 * window or layer starts on, and advances with, the clock of the output it is
 * on. Entries without an owner use the shared clock.
 */
export interface AnimationClock {
  /** The time an animation starting now on `output` is anchored at. */
  startTimeMs(output: string | undefined): number;
}

interface AnimationTimeline {
  startedAtMs: number;
  durationMs: number;
  from: number;
  to: number;
  easing: (progress: number) => number;
  repeat?: AnimationRepeatMode;
}

const linear = (value: number) => value;
const activeAnimationEntries = new Set<AnimationEntry>();
let currentAnimationFrameMs = 0;
let animationClock: AnimationClock | null = null;

export function installAnimationClock(clock: AnimationClock | null): void {
  animationClock = clock;
}

function animationStartTimeMs(entry: AnimationEntry): number {
  return animationClock
    ? animationClock.startTimeMs(entry.output?.())
    : currentAnimationFrameMs;
}
const animationSnapEpsilon = 1e-4;

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
  markDirty: () => void,
  entries: Map<symbol, AnimationEntry>,
  output?: () => string | undefined,
): AnimationController {

  const ensureEntry = (variable: AnimationVariable): AnimationEntry => {
    let entry = entries.get(variable.id);
    if (!entry) {
      entry = { progress: signal(0), output };
      entries.set(variable.id, entry);
    }
    return entry;
  };

  // Cache the read-only computed wrapper per variable so repeated calls (e.g.
  // `window.animation.signal(OPEN_ANIMATION)` inside window.composition) return
  // the SAME ComputedSignal. Without this, each composition pass produced a
  // fresh ComputedSignal that became a permanent dependent of the underlying
  // progress signal — the progress.dependents Set then grew without bound and
  // every animation tick had to iterate every leaked instance.
  const variableSignalCache = new Map<symbol, ReadonlySignal<number>>();
  const variableSignal = (variable: AnimationVariable): ReadonlySignal<number> => {
    let cached = variableSignalCache.get(variable.id);
    if (!cached) {
      // Construct the cached computed outside composition ownership: it must
      // outlive the composition pass it was first requested from (otherwise
      // the next pass would auto-dispose it and the cache would hand back a
      // detached, stale ComputedSignal).
      cached = withoutCompositionOwnership(() =>
        computed(() => ensureEntry(variable).progress()),
      );
      variableSignalCache.set(variable.id, cached);
    }
    return cached;
  };

  return {
    variable: variableSignal,
    signal: variableSignal,
    start(variable, options) {
      const entry = ensureEntry(variable);

      const duration = Math.max(1, Math.floor(options.duration));
      const easing = options.easing ?? linear;
      const from = options.from ?? entry.progress.peek();
      const to = options.to ?? 1;

      settleEntry(entry, false);
      entry.progress.value = from;
      const finished = new Promise<boolean>((resolve) => {
        entry.settle = resolve;
      });
      entry.timeline = {
        startedAtMs: animationStartTimeMs(entry),
        durationMs: duration,
        from,
        to,
        easing,
        repeat: options.repeat,
      };
      activeAnimationEntries.add(entry);
      markDirty();
      return finished;
    },
    stop(variable) {
      const entry = entries.get(variable.id);
      if (entry) {
        const wasRunning = entry.timeline !== undefined;
        entry.timeline = undefined;
        settleEntry(entry, false);
        activeAnimationEntries.delete(entry);
        if (wasRunning) {
          markDirty();
        }
      }
    },
    set(variable, value) {
      const entry = ensureEntry(variable);
      const changed = !Object.is(entry.progress.peek(), value);
      entry.timeline = undefined;
      settleEntry(entry, false);
      activeAnimationEntries.delete(entry);
      entry.progress.value = value;
      if (changed) {
        markDirty();
      }
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
  output?: () => string | undefined,
): WindowAnimationController {
  return createAnimationControllerWithStore(
    () => markWindowDirty(windowId),
    entries,
    output,
  );
}

/** Advance every running animation to `nowMs`. */
export function advanceAnimationFrame(nowMs: number): boolean {
  return advanceAnimations(nowMs, () => true);
}

/**
 * Advance the running animations whose output `select` accepts (`undefined`
 * for animations without an owner) to `nowMs`; returns whether any animation
 * is still running afterwards.
 */
export function advanceAnimations(
  nowMs: number,
  select: (output: string | undefined) => boolean,
): boolean {
  currentAnimationFrameMs = Math.max(currentAnimationFrameMs, nowMs);
  if (activeAnimationEntries.size === 0) {
    return false;
  }

  for (const entry of Array.from(activeAnimationEntries)) {
    if (!select(entry.output?.())) {
      continue;
    }
    const timeline = entry.timeline;
    if (!timeline) {
      activeAnimationEntries.delete(entry);
      continue;
    }

    const elapsed = Math.max(0, nowMs - timeline.startedAtMs);
    const rawProgress = elapsed / timeline.durationMs;
    const progress = repeatedProgress(rawProgress, timeline.repeat);
    const eased = normalizeEasedProgress(timeline.easing(progress), progress);
    const nextValue = timeline.from + (timeline.to - timeline.from) * eased;
    if (
      !timeline.repeat &&
      (rawProgress >= 1 || Math.abs(nextValue - timeline.to) <= animationSnapEpsilon)
    ) {
      entry.progress.value = timeline.to;
      entry.timeline = undefined;
      activeAnimationEntries.delete(entry);
      settleEntry(entry, true);
      continue;
    }

    entry.progress.value = nextValue;

    if (!timeline.repeat && rawProgress >= 1) {
      entry.progress.value = timeline.to;
      entry.timeline = undefined;
      activeAnimationEntries.delete(entry);
      settleEntry(entry, true);
    }
  }

  return activeAnimationEntries.size > 0;
}

function settleEntry(entry: AnimationEntry, completed: boolean): void {
  const settle = entry.settle;
  entry.settle = undefined;
  settle?.(completed);
}

export function hasActiveAnimations(): boolean {
  return activeAnimationEntries.size > 0;
}

/**
 * The outputs running animations are bound to; `undefined` stands for
 * animations without an owner.
 */
export function activeAnimationOutputs(): Set<string | undefined> {
  const outputs = new Set<string | undefined>();
  for (const entry of activeAnimationEntries) {
    outputs.add(entry.output?.());
  }
  return outputs;
}

/**
 * Returns whether a specific controller store owns a running animation.
 *
 * The runtime uses this to keep window and layer repaint domains independent:
 * a window animation must not make every layer or popup effect look animated.
 */
export function hasActiveAnimationsInStore(
  entries: ReadonlyMap<symbol, unknown>,
): boolean {
  for (const value of entries.values()) {
    if (
      typeof value === "object" &&
      value !== null &&
      "timeline" in value &&
      value.timeline !== undefined
    ) {
      return true;
    }
  }
  return false;
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

function repeatedProgress(progress: number, repeat: AnimationRepeatMode | undefined): number {
  if (!Number.isFinite(progress) || progress <= 0) {
    return 0;
  }

  if (!repeat) {
    return clampUnit(progress);
  }

  if (repeat === "loop") {
    return progress - Math.floor(progress);
  }

  const cycle = progress % 2;
  return cycle <= 1 ? cycle : 2 - cycle;
}
