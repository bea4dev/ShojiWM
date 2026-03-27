import { createReactiveWindow } from "./reactive-window";
import type { WindowAnimationController } from "./animation";
import { read } from "./signals";
import { createElementNode } from "./runtime";
import { createComponentStateStore, withComponentRenderRoot } from "./runtime";
import {
  enterWindowDependencyScope,
  leaveWindowDependencyScope,
} from "./runtime-hooks";
import {
  patchSerializedDecorationTree,
  serializeDecorationTree,
  type DecorationSerializationContext,
} from "./serialize";
import type {
  DecorationChild,
  DecorationRenderable,
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
  reevaluate(dirtyNodeIds?: readonly string[]): DecorationEvaluationResult;
  invokeHandler(handlerId: string): boolean;
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
      floating ||
      maximized ||
      fullscreen ||
      icon ||
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
 * This is intentionally structural:
 * runtime-only state such as focus and interaction is expected to flow through
 * signals and runtime dirty tracking rather than forcing a snapshot rebuild.
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
  animation?: WindowAnimationController,
): DecorationEvaluationCache {
  const handle = createReactiveWindow(snapshot, actions, animation);
  const componentStateStore = createComponentStateStore();

  let currentSnapshot = snapshot;
  let version = 1;
  let tree: DecorationChild;
  let serialized: SerializableDecorationChild;
  let transform: WindowTransform;
  let nextHandlerId = 1;
  let clickHandlers = new Map<string, () => void>();
  const handlerIdsByKey = new Map<string, string>();

  const serializationContext: DecorationSerializationContext = {
    registerClickHandler(key, handler) {
      const handlerId = handlerIdsByKey.get(key) ?? `click-${nextHandlerId++}`;
      handlerIdsByKey.set(key, handlerId);
      clickHandlers.set(handlerId, handler);
      return handlerId;
    },
  };

  const evaluateCurrentTree = (): DecorationEvaluationResult => {
    clickHandlers = new Map();
    enterWindowDependencyScope(currentSnapshot.id);
    try {
      tree = normalizeRootDecoration(
        withComponentRenderRoot(currentSnapshot.id, componentStateStore, () =>
          evaluate(handle.window)
        )
      );
      serialized = serializeDecorationTree(tree, serializationContext);
      transform = snapshotTransform(handle);
    } finally {
      leaveWindowDependencyScope();
    }
    version += 1;

    return {
      tree,
      serialized,
      transform,
      version,
    };
  };

  const patchCurrentTree = (dirtyNodeIds: readonly string[]): DecorationEvaluationResult => {
    if (dirtyNodeIds.length === 0) {
      return evaluateCurrentTree();
    }

    enterWindowDependencyScope(currentSnapshot.id);
    try {
      const dirtyNodeIdSet = new Set(dirtyNodeIds);
      serialized = patchSerializedDecorationTree(
        tree,
        serialized,
        dirtyNodeIdSet,
        serializationContext,
      );
      transform = snapshotTransform(handle);
    } finally {
      leaveWindowDependencyScope();
    }
    version += 1;

    return {
      tree,
      serialized,
      transform,
      version,
    };
  };

  const initial = evaluateCurrentTree();
  version = initial.version;

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
      return evaluateCurrentTree();
    },
    reevaluate(dirtyNodeIds) {
      if (dirtyNodeIds && dirtyNodeIds.length > 0) {
        return patchCurrentTree(dirtyNodeIds);
      }
      return evaluateCurrentTree();
    },
    invokeHandler(handlerId) {
      const handler = clickHandlers.get(handlerId);
      if (!handler) {
        return false;
      }

      handler();
      return true;
    },
  };
}

function normalizeRootDecoration(rendered: DecorationRenderable): DecorationChild {
  if (rendered == null || rendered === false || rendered === true) {
    return createElementNode("Fragment", {});
  }

  return rendered;
}

function snapshotTransform(
  handle: ReactiveWaylandWindowHandle,
): WindowTransform {
  const origin = read(handle.transform.origin);

  return {
    origin: {
      x: read(origin.x),
      y: read(origin.y),
    },
    translateX: read(handle.transform.translateX),
    translateY: read(handle.transform.translateY),
    scaleX: read(handle.transform.scaleX),
    scaleY: read(handle.transform.scaleY),
    opacity: read(handle.transform.opacity),
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
