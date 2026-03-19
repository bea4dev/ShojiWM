import { createReactiveWindow } from "./reactive-window";
import { serializeDecorationTree } from "./serialize";
import type {
  DecorationChild,
  DecorationFunction,
  ReactiveWaylandWindowHandle,
  SerializableDecorationChild,
  WaylandWindowActions,
  WaylandWindowSnapshot,
  WindowTransform,
} from "./types";

export interface WindowSnapshotDiff {
  changed: boolean;
  title: boolean;
  appId: boolean;
  position: boolean;
  focus: boolean;
  floating: boolean;
  maximized: boolean;
  fullscreen: boolean;
  icon: boolean;
  interaction: boolean;
  xwayland: boolean;
}

export interface DecorationEvaluationResult {
  tree: DecorationChild;
  serialized: SerializableDecorationChild;
  transform: WindowTransform;
  version: number;
}

export interface DecorationEvaluationCache {
  readonly window: ReactiveWaylandWindowHandle["window"];
  readonly version: number;
  readonly lastSerialized: SerializableDecorationChild;
  readonly lastTree: DecorationChild;
  readonly lastTransform: WindowTransform;
  update(snapshot: WaylandWindowSnapshot): DecorationEvaluationResult | null;
}

export function diffWindowSnapshot(
  previous: WaylandWindowSnapshot,
  next: WaylandWindowSnapshot,
): WindowSnapshotDiff {
  const title = previous.title !== next.title;
  const appId = previous.appId !== next.appId;
  const position = !shallowEqual(previous.position, next.position);
  const focus = previous.isFocused !== next.isFocused;
  const floating = previous.isFloating !== next.isFloating;
  const maximized = previous.isMaximized !== next.isMaximized;
  const fullscreen = previous.isFullscreen !== next.isFullscreen;
  const icon = !shallowEqual(previous.icon, next.icon);
  const interaction = !shallowEqual(previous.interaction, next.interaction);
  const xwayland = previous.isXwayland !== next.isXwayland;

  return {
    changed:
      title ||
      appId ||
      position ||
      focus ||
      floating ||
      maximized ||
      fullscreen ||
      icon ||
      interaction ||
      xwayland,
    title,
    appId,
    position,
    focus,
    floating,
    maximized,
    fullscreen,
    icon,
    interaction,
    xwayland,
  };
}

/**
 * Minimal policy for when a decoration needs reevaluation.
 *
 * This is intentionally coarse-grained:
 * if any user-visible property changed, reevaluate the whole decoration tree.
 * Later milestones can reduce this to subtree-level invalidation.
 */
export function shouldReevaluateDecoration(
  previous: WaylandWindowSnapshot,
  next: WaylandWindowSnapshot,
): boolean {
  return diffWindowSnapshot(previous, next).changed;
}

export function createDecorationEvaluationCache(
  snapshot: WaylandWindowSnapshot,
  actions: WaylandWindowActions,
  evaluate: DecorationFunction,
): DecorationEvaluationCache {
  const handle = createReactiveWindow(snapshot, actions);

  let currentSnapshot = snapshot;
  let version = 1;
  let tree = evaluate(handle.window);
  let serialized = serializeDecorationTree(tree);
  let transform = snapshotTransform(handle);

  return {
    get window() {
      return handle.window;
    },
    get version() {
      return version;
    },
    get lastSerialized() {
      return serialized;
    },
    get lastTree() {
      return tree;
    },
    get lastTransform() {
      return transform;
    },
    update(nextSnapshot) {
      if (!shouldReevaluateDecoration(currentSnapshot, nextSnapshot)) {
        handle.update(nextSnapshot);
        currentSnapshot = nextSnapshot;
        return null;
      }

      handle.update(nextSnapshot);
      currentSnapshot = nextSnapshot;
      tree = evaluate(handle.window);
      serialized = serializeDecorationTree(tree);
      transform = snapshotTransform(handle);
      version += 1;

      return {
        tree,
        serialized,
        transform,
        version,
      };
    },
  };
}

function snapshotTransform(
  handle: ReactiveWaylandWindowHandle,
): WindowTransform {
  return {
    origin: {
      x: handle.transform.origin.x,
      y: handle.transform.origin.y,
    },
    translateX: handle.transform.translateX,
    translateY: handle.transform.translateY,
    scaleX: handle.transform.scaleX,
    scaleY: handle.transform.scaleY,
    opacity: handle.transform.opacity,
  };
}

function shallowEqual(a: unknown, b: unknown): boolean {
  if (Object.is(a, b)) {
    return true;
  }

  if (!a || !b || typeof a !== "object" || typeof b !== "object") {
    return false;
  }

  if (Array.isArray(a) || Array.isArray(b)) {
    if (!Array.isArray(a) || !Array.isArray(b) || a.length !== b.length) {
      return false;
    }
    return a.every((value, index) => Object.is(value, b[index]));
  }

  const aEntries = Object.entries(a as Record<string, unknown>);
  const bEntries = Object.entries(b as Record<string, unknown>);
  if (aEntries.length !== bEntries.length) {
    return false;
  }

  return aEntries.every(([key, value]) =>
    shallowEqual(value, (b as Record<string, unknown>)[key])
  );
}
