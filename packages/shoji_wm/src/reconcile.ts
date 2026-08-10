import { createReactiveWindow } from "./reactive-window";
import type { WindowAnimationController } from "./animation";
import { read } from "./signals";
import { createElementNode } from "./runtime";
import { createComponentStateStore, withComponentRenderRoot } from "./runtime";
import {
  enterWindowManagedDependencyScope,
  enterWindowDependencyScope,
  enterWindowPatchDependencyScope,
  leaveWindowManagedDependencyScope,
  leaveWindowDependencyScope,
} from "./runtime-hooks";
import {
  patchSerializedCompositionTree,
  serializeCompositionTree,
  type CompositionSerializationContext,
  type ShaderUniformBinding,
} from "./serialize";
import type {
  CompositionChild,
  WindowCompositionContext,
  CompositionElementNode,
  CompositionRenderable,
  CompositorDefinition,
  WindowCompositionFunction,
  ManagedWindowProps,
  ManagedWindowRect,
  ManagedWindowState,
  ReactiveWaylandWindowHandle,
  SerializableCompositionChild,
  SurfacePolicy,
  WaylandWindowActions,
  WaylandWindowSnapshot,
  WindowPosition,
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
  decoration: boolean;
  xwayland: boolean;
}

export interface CompositionEvaluationResult {
  tree: CompositionChild;
  serialized: SerializableCompositionChild;
  transform: WindowTransform;
  managedWindow: ManagedWindowState;
  version: number;
}

export interface CompositionShaderUniformPatch {
  bindingKey: string;
  nodeId: string;
  stageIndex: number;
  name: string;
  values: number[];
  arrayElementWidth?: number;
}

export interface CompositionEvaluationCache {
  readonly window: ReactiveWaylandWindowHandle["window"];
  readonly version: number;
  readonly lastSerialized: SerializableCompositionChild;
  readonly lastTree: CompositionChild;
  readonly lastTransform: WindowTransform;
  readonly lastManagedWindow: ManagedWindowState;
  readonly shaderUniformBindings: readonly ShaderUniformBinding[];
  update(snapshot: WaylandWindowSnapshot): CompositionEvaluationResult | null;
  reevaluate(dirtyNodeIds?: readonly string[]): CompositionEvaluationResult;
  reevaluateManagedWindow(): Pick<
    CompositionEvaluationResult,
    "transform" | "managedWindow" | "version"
  >;
  readShaderUniformPatches(
    bindingKeys: readonly string[],
  ): CompositionShaderUniformPatch[] | null;
  invokeHandler(handlerId: string): boolean;
  setContext(context: WindowCompositionContext): void;
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
  const decoration = !shallowEqual(previous.decoration, next.decoration);
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
      decoration ||
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
    decoration,
    xwayland,
  };
}

/**
 * Minimal policy for when a composition tree needs reevaluation.
 *
 * This is intentionally structural:
 * runtime-only state such as focus and interaction is expected to flow through
 * signals and runtime dirty tracking rather than forcing a snapshot rebuild.
 */
export function shouldReevaluateComposition(
  previous: WaylandWindowSnapshot,
  next: WaylandWindowSnapshot,
): boolean {
  return diffWindowSnapshot(previous, next).changed;
}

export function createCompositionEvaluationCache(
  snapshot: WaylandWindowSnapshot,
  actions: WaylandWindowActions,
  evaluate: WindowCompositionFunction,
  animation?: WindowAnimationController,
  initialContext: WindowCompositionContext = { phase: "render", isPreview: false },
): CompositionEvaluationCache {
  const handle = createReactiveWindow(snapshot, actions, animation);
  const componentStateStore = createComponentStateStore();

  let currentSnapshot = snapshot;
  let version = 1;
  let tree: CompositionChild;
  let serialized: SerializableCompositionChild;
  let transform: WindowTransform;
  let managedWindow: ManagedWindowState;
  let managedWindowProps: ManagedWindowProps | undefined;
  let context = initialContext;
  let nextHandlerId = 1;
  let runtimeHandlers = new Map<string, () => void>();
  const handlerIdsByKey = new Map<string, string>();
  const shaderUniformBindings = new Map<string, ShaderUniformBinding>();
  const shaderUniformSnapshots = new Map<
    string,
    ReturnType<ShaderUniformBinding["read"]>
  >();

  const serializationContext: CompositionSerializationContext = {
    registerClickHandler(key, handler) {
      const handlerId = handlerIdsByKey.get(key) ?? `click-${nextHandlerId++}`;
      handlerIdsByKey.set(key, handlerId);
      runtimeHandlers.set(handlerId, handler);
      return handlerId;
    },
    registerInteractionHandler(key, handler) {
      const handlerId = handlerIdsByKey.get(key) ?? `interaction-${nextHandlerId++}`;
      handlerIdsByKey.set(key, handlerId);
      runtimeHandlers.set(handlerId, handler);
      return handlerId;
    },
    registerShaderUniformBinding(binding) {
      shaderUniformBindings.set(binding.key, binding);
      shaderUniformSnapshots.set(binding.key, binding.read());
    },
  };

  const evaluateCurrentTree = (): CompositionEvaluationResult => {
    runtimeHandlers = new Map();
    enterWindowDependencyScope(currentSnapshot.id);
    try {
      const rendered = withComponentRenderRoot(currentSnapshot.id, componentStateStore, () =>
        evaluate(handle.window, context)
      );
      const extracted = extractManagedWindowRoot(rendered, handle, currentSnapshot.id);
      tree = extracted.tree;
      managedWindow = extracted.managedWindow;
      managedWindowProps = extracted.props;
      handle.updateManagedWindow(managedWindow);
      shaderUniformBindings.clear();
      serialized = serializeCompositionTree(tree, serializationContext);
      transform = managedWindow.transform;
    } finally {
      leaveWindowDependencyScope();
    }
    version += 1;

    return {
      tree,
      serialized,
      transform,
      managedWindow,
      version,
    };
  };

  const patchCurrentTree = (dirtyNodeIds: readonly string[]): CompositionEvaluationResult => {
    if (dirtyNodeIds.length === 0) {
      return evaluateCurrentTree();
    }

    enterWindowPatchDependencyScope(currentSnapshot.id, dirtyNodeIds);
    try {
      const dirtyNodeIdSet = new Set(dirtyNodeIds);
      for (const [key, binding] of shaderUniformBindings) {
        if (
          dirtyNodeIds.some(
            (nodeId) =>
              binding.nodeId === nodeId ||
              binding.nodeId.startsWith(`${nodeId}.`) ||
              binding.nodeId.startsWith(`${nodeId}[`),
          )
        ) {
          shaderUniformBindings.delete(key);
        }
      }
      serialized = patchSerializedCompositionTree(
        tree,
        serialized,
        dirtyNodeIdSet,
        serializationContext,
      );
      managedWindow = snapshotManagedWindow(currentSnapshot.id, managedWindowProps, handle);
      handle.updateManagedWindow(managedWindow);
      transform = managedWindow.transform;
    } finally {
      leaveWindowDependencyScope();
    }
    version += 1;

    return {
      tree,
      serialized,
      transform,
      managedWindow,
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
    get lastManagedWindow() {
      return managedWindow;
    },
    get shaderUniformBindings() {
      return Array.from(shaderUniformBindings.values());
    },
    update(nextSnapshot) {
      if (!shouldReevaluateComposition(currentSnapshot, nextSnapshot)) {
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
    reevaluateManagedWindow() {
      managedWindow = snapshotManagedWindow(currentSnapshot.id, managedWindowProps, handle);
      handle.updateManagedWindow(managedWindow);
      transform = managedWindow.transform;
      version += 1;
      return {
        transform,
        managedWindow,
        version,
      };
    },
    readShaderUniformPatches(bindingKeys) {
      const patches: CompositionShaderUniformPatch[] = [];
      const nextSnapshots = new Map<
        string,
        NonNullable<ReturnType<ShaderUniformBinding["read"]>>
      >();
      for (const bindingKey of bindingKeys) {
        const binding = shaderUniformBindings.get(bindingKey);
        if (!binding) {
          return null;
        }
        const snapshot = binding.read();
        if (snapshot === null) {
          return null;
        }
        const previous = shaderUniformSnapshots.get(bindingKey);
        if (
          previous === undefined ||
          previous === null ||
          previous.values.length !== snapshot.values.length ||
          previous.arrayElementWidth !== snapshot.arrayElementWidth
        ) {
          return null;
        }
        nextSnapshots.set(bindingKey, snapshot);
        patches.push({
          bindingKey,
          nodeId: binding.nodeId,
          stageIndex: binding.stageIndex,
          name: binding.name,
          values: snapshot.values,
          arrayElementWidth: snapshot.arrayElementWidth,
        });
      }
      for (const [bindingKey, snapshot] of nextSnapshots) {
        shaderUniformSnapshots.set(bindingKey, snapshot);
      }
      return patches;
    },
    invokeHandler(handlerId) {
      const handler = runtimeHandlers.get(handlerId);
      if (!handler) {
        return false;
      }

      handler();
      return true;
    },
    setContext(nextContext) {
      context = nextContext;
    },
  };
}

function normalizeRootComposition(rendered: CompositionRenderable): CompositionChild {
  if (rendered == null || rendered === false || rendered === true) {
    return createElementNode("Fragment", {});
  }

  return rendered;
}

function extractManagedWindowRoot(
  rendered: CompositionRenderable,
  handle: ReactiveWaylandWindowHandle,
  windowId: string,
): {
  tree: CompositionChild;
  managedWindow: ManagedWindowState;
  props?: ManagedWindowProps;
} {
  const normalized = normalizeRootComposition(rendered);
  if (!isManagedWindowNode(normalized)) {
    return {
      tree: normalized,
      managedWindow: snapshotManagedWindow(windowId, undefined, handle),
    };
  }

  return {
    tree: managedWindowChildrenAsRoot(normalized),
    managedWindow: snapshotManagedWindow(windowId, normalized.props as ManagedWindowProps, handle),
    props: normalized.props as ManagedWindowProps,
  };
}

function managedWindowChildrenAsRoot(node: CompositionElementNode): CompositionChild {
  if (node.children.length === 1) {
    return node.children[0] ?? createElementNode("Fragment", {});
  }

  return createElementNode("Fragment", {
    children: node.children,
  });
}

function isManagedWindowNode(node: CompositionChild): node is CompositionElementNode {
  return typeof node !== "string" && typeof node !== "number" && node.type === "ManagedWindow";
}

// The COMPOSITOR singleton lives on globalThis (see index.ts) so every bundle
// shares one instance. Read it through the same key instead of importing
// index.ts, which would create a module cycle (index.ts imports this file).
function compositorSingleton(): CompositorDefinition | undefined {
  return (globalThis as Record<string, unknown>)["__shoji_wm_COMPOSITOR__"] as
    | CompositorDefinition
    | undefined;
}

function resolveWindowSurfacePolicy(
  handle: ReactiveWaylandWindowHandle,
): SurfacePolicy | undefined {
  const evaluatePolicy = compositorSingleton()?.rendering?.surfacePolicy;
  if (!evaluatePolicy) {
    return undefined;
  }
  // Runs inside the managed-window dependency scope of the caller: window
  // state signals the callback reads subscribe this window for re-evaluation.
  const resolved = evaluatePolicy({ kind: "toplevel", window: handle.window });
  if (!resolved) {
    return undefined;
  }
  const opaqueRegion = read(resolved.opaqueRegion);
  return opaqueRegion === undefined ? undefined : { opaqueRegion };
}

function snapshotManagedWindow(
  windowId: string,
  props: ManagedWindowProps | undefined,
  handle: ReactiveWaylandWindowHandle,
): ManagedWindowState {
  enterWindowManagedDependencyScope(windowId);
  try {
    const legacyTransform = snapshotTransform(handle);
    const transform = snapshotManagedWindowTransform(props?.transform, legacyTransform);
    const opacity = props?.opacity === undefined ? transform.opacity : read(props.opacity);
    const visible = props?.visible === undefined ? true : read(props.visible);
    const idle = props?.idle === undefined ? false : read(props.idle);

    return {
      managed: props !== undefined,
      rect:
        props?.rect === undefined
          ? undefined
          : snapshotManagedWindowRect(read(props.rect)),
      workspace: props?.workspace === undefined ? undefined : read(props.workspace),
      visibleOutputs:
        props?.visibleOutputs === undefined
          ? undefined
          : snapshotManagedWindowVisibleOutputs(read(props.visibleOutputs)),
      visible,
      idle,
      interactive: props?.interactive === undefined ? true : read(props.interactive),
      forceRectSize: props?.forceRectSize === undefined ? false : read(props.forceRectSize),
      tiled: props?.tiled === undefined ? false : read(props.tiled),
      // Left undefined when unset so the compositor can fall back to the client's
      // `wp_tearing_control` hint; a concrete value overrides that hint (see ManagedWindowProps).
      allowTearing:
        props?.allowTearing === undefined ? undefined : read(props.allowTearing),
      zIndex: props?.zIndex === undefined ? undefined : read(props.zIndex),
      transform: {
        ...transform,
        opacity: visible && !idle ? opacity : 0,
      },
      surfacePolicy: resolveWindowSurfacePolicy(handle),
    };
  } finally {
    leaveWindowManagedDependencyScope();
  }
}

function snapshotManagedWindowVisibleOutputs(outputs: readonly string[] | null): string[] | null {
  if (outputs === null) {
    return null;
  }

  return Array.from(new Set(outputs));
}

function snapshotManagedWindowRect(rect: ManagedWindowRect): WindowPosition {
  return {
    x: read(rect.x),
    y: read(rect.y),
    width: read(rect.width),
    height: read(rect.height),
  };
}

function snapshotManagedWindowTransform(
  transform: ManagedWindowProps["transform"] | undefined,
  fallback: WindowTransform,
): WindowTransform {
  const resolved = transform === undefined ? undefined : read(transform);
  if (!resolved) {
    return fallback;
  }

  const origin = resolved.origin === undefined ? read(fallback.origin) : read(resolved.origin);
  const scale = resolved.scale === undefined ? undefined : read(resolved.scale);

  return {
    origin: {
      x: read(origin.x),
      y: read(origin.y),
    },
    translateX:
      resolved.translateX === undefined ? read(fallback.translateX) : read(resolved.translateX),
    translateY:
      resolved.translateY === undefined ? read(fallback.translateY) : read(resolved.translateY),
    scaleX:
      resolved.scaleX === undefined
        ? scale ?? read(fallback.scaleX)
        : read(resolved.scaleX),
    scaleY:
      resolved.scaleY === undefined
        ? scale ?? read(fallback.scaleY)
        : read(resolved.scaleY),
    opacity: read(fallback.opacity),
  };
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

export function shallowEqual(a: unknown, b: unknown): boolean {
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
