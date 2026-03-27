import type {
  ComponentProps,
  DecorationChild,
  DecorationRenderable,
  DecorationElementNode,
  DecorationNodeType,
} from "./types";
import { computed, signal, type ReadonlySignal, type SignalTuple } from "./signals";

interface ComponentStateStore {
  instances: Map<string, ComponentInstanceState>;
}

interface ComponentInstanceState {
  hooks: unknown[];
}

interface RenderRootContext {
  rootId: string;
  store: ComponentStateStore;
  seenInstances: Set<string>;
  pendingLayoutEffects: Array<() => void>;
  pendingEffects: Array<() => void>;
}

interface ComponentRenderFrame {
  instanceId: string;
  childCursor: number;
  hookCursor: number;
}

let activeRenderRoot: RenderRootContext | null = null;
const renderFrames: ComponentRenderFrame[] = [];

interface ComputedHookSlot<T> {
  kind: "computed";
  signal: ReadonlySignal<T>;
  computeRef: { current: () => T };
}

interface EffectHookSlot {
  kind: "effect";
  deps: readonly unknown[] | undefined;
  cleanup?: (() => void) | undefined;
}

interface MemoHookSlot<T> {
  kind: "memo";
  deps: readonly unknown[] | undefined;
  value: T;
}

interface RefHookSlot<T> {
  kind: "ref";
  ref: { current: T };
}

export function createElementNode(
  type: DecorationNodeType,
  props: ComponentProps = {},
  key?: string | number | null,
): DecorationElementNode {
  const { children, ...rest } = props;

  return {
    kind: "element",
    type,
    key: key ?? null,
    props: rest,
    children: normalizeChildren(children),
  };
}

export function normalizeChildren(children: unknown): DecorationChild[] {
  if (children == null || children === false || children === true) {
    return [];
  }

  if (Array.isArray(children)) {
    return children.flatMap(normalizeChildren);
  }

  return [children as DecorationChild];
}

export function createComponentStateStore(): ComponentStateStore {
  return {
    instances: new Map(),
  };
}

export function withComponentRenderRoot<T>(
  rootId: string,
  store: ComponentStateStore,
  render: () => T,
): T {
  const previousRoot = activeRenderRoot;
  const previousDepth = renderFrames.length;
  const rootInstanceId = `${rootId}/__root__`;
  activeRenderRoot = {
    rootId,
    store,
    seenInstances: new Set([rootInstanceId]),
    pendingLayoutEffects: [],
    pendingEffects: [],
  };
  renderFrames.push({
    instanceId: rootInstanceId,
    childCursor: 0,
    hookCursor: 0,
  });

  try {
    return render();
  } finally {
    const currentRoot = activeRenderRoot;
    if (currentRoot) {
      const prefix = `${rootId}/`;
      for (const [instanceId, instance] of Array.from(store.instances.entries())) {
        if (instanceId.startsWith(prefix) && !currentRoot.seenInstances.has(instanceId)) {
          cleanupInstance(instance);
          store.instances.delete(instanceId);
        }
      }
    }
    const pendingLayoutEffects = currentRoot?.pendingLayoutEffects ?? [];
    const pendingEffects = currentRoot?.pendingEffects ?? [];
    renderFrames.length = previousDepth;
    activeRenderRoot = previousRoot;
    for (const effect of pendingLayoutEffects) {
      effect();
    }
    for (const effect of pendingEffects) {
      effect();
    }
  }
}

export function renderComponent<TProps extends ComponentProps>(
  type: (props: TProps) => DecorationRenderable,
  props: TProps,
  key?: string | number | null,
): DecorationRenderable {
  const parentFrame = renderFrames[renderFrames.length - 1];
  const root = activeRenderRoot;
  if (!root) {
    return type(props);
  }

  const ordinal = parentFrame ? parentFrame.childCursor++ : 0;
  const typeName = type.name || "Anonymous";
  const instanceId = parentFrame
    ? buildInstanceId(parentFrame.instanceId, typeName, ordinal, key)
    : buildInstanceId(root.rootId, typeName, ordinal, key);

  root.seenInstances.add(instanceId);
  renderFrames.push({
    instanceId,
    childCursor: 0,
    hookCursor: 0,
  });
  try {
    return type(props);
  } finally {
    renderFrames.pop();
  }
}

export function createState<T>(initialValue: T | (() => T)): SignalTuple<T> {
  const { hooks, hookIndex } = currentHookSlotContext("createState");
  const existing = hooks[hookIndex];
  if (existing) {
    return existing as SignalTuple<T>;
  }

  const resolvedInitial =
    typeof initialValue === "function"
      ? (initialValue as () => T)()
      : initialValue;
  const state = signal(resolvedInitial);
  hooks[hookIndex] = state;
  return state;
}

export const useState = createState;

export function useComputed<T>(compute: () => T): ReadonlySignal<T> {
  const { hooks, hookIndex } = currentHookSlotContext("useComputed");
  const existing = hooks[hookIndex] as ComputedHookSlot<T> | undefined;
  if (existing?.kind === "computed") {
    existing.computeRef.current = compute;
    return existing.signal;
  }

  const computeRef = { current: compute };
  const signalValue = computed(() => computeRef.current());
  hooks[hookIndex] = {
    kind: "computed",
    signal: signalValue,
    computeRef,
  } satisfies ComputedHookSlot<T>;
  return signalValue;
}

export const createComputed = useComputed;

export function useEffect(
  run: () => void | (() => void),
  deps?: readonly unknown[],
): void {
  queueEffect("useEffect", run, deps, false);
}

export function useLayoutEffect(
  run: () => void | (() => void),
  deps?: readonly unknown[],
): void {
  queueEffect("useLayoutEffect", run, deps, true);
}

export function useMemo<T>(
  compute: () => T,
  deps?: readonly unknown[],
): T {
  const { hooks, hookIndex } = currentHookSlotContext("useMemo");
  const existing = hooks[hookIndex] as MemoHookSlot<T> | undefined;
  if (existing?.kind === "memo" && sameHookDeps(existing.deps, deps)) {
    return existing.value;
  }

  const value = compute();
  hooks[hookIndex] = {
    kind: "memo",
    deps: deps ? [...deps] : undefined,
    value,
  } satisfies MemoHookSlot<T>;
  return value;
}

export function useRef<T>(initialValue: T): { current: T } {
  const { hooks, hookIndex } = currentHookSlotContext("useRef");
  const existing = hooks[hookIndex] as RefHookSlot<T> | undefined;
  if (existing?.kind === "ref") {
    return existing.ref;
  }

  const ref = { current: initialValue };
  hooks[hookIndex] = {
    kind: "ref",
    ref,
  } satisfies RefHookSlot<T>;
  return ref;
}

export function onCleanup(cleanup: () => void): void {
  const { hooks, hookIndex, root } = currentHookSlotContext("onCleanup");
  const existing = hooks[hookIndex] as EffectHookSlot | undefined;
  const slot: EffectHookSlot =
    existing?.kind === "effect"
      ? existing
      : {
          kind: "effect",
          deps: undefined,
          cleanup: undefined,
        };
  hooks[hookIndex] = slot;
  root.pendingEffects.push(() => {
    slot.cleanup?.();
    slot.cleanup = cleanup;
  });
}

function queueEffect(
  apiName: "useEffect" | "useLayoutEffect",
  run: () => void | (() => void),
  deps: readonly unknown[] | undefined,
  layout: boolean,
): void {
  const { hooks, hookIndex, root } = currentHookSlotContext(apiName);
  const existing = hooks[hookIndex] as EffectHookSlot | undefined;
  const depsChanged = !existing || !sameHookDeps(existing.deps, deps);
  if (!depsChanged) {
    return;
  }

  const slot: EffectHookSlot =
    existing?.kind === "effect"
      ? existing
      : {
          kind: "effect",
          deps: undefined,
          cleanup: undefined,
        };
  hooks[hookIndex] = slot;
  (layout ? root.pendingLayoutEffects : root.pendingEffects).push(() => {
    slot.cleanup?.();
    const cleanup = run();
    slot.cleanup = typeof cleanup === "function" ? cleanup : undefined;
    slot.deps = deps ? [...deps] : undefined;
  });
}

function buildInstanceId(
  parentId: string,
  typeName: string,
  ordinal: number,
  key?: string | number | null,
): string {
  if (key != null) {
    return `${parentId}/${typeName}#${String(key)}`;
  }

  return `${parentId}/${typeName}[${ordinal}]`;
}

function currentHookSlotContext(apiName: string): {
  hooks: unknown[];
  hookIndex: number;
  root: RenderRootContext;
} {
  const frame = renderFrames[renderFrames.length - 1];
  const root = activeRenderRoot;
  if (!frame || !root) {
    throw new Error(`${apiName}() can only be used inside a function component render`);
  }

  let instance = root.store.instances.get(frame.instanceId);
  if (!instance) {
    instance = { hooks: [] };
    root.store.instances.set(frame.instanceId, instance);
  }

  const hookIndex = frame.hookCursor++;
  return {
    hooks: instance.hooks,
    hookIndex,
    root,
  };
}

function sameHookDeps(
  previous: readonly unknown[] | undefined,
  next: readonly unknown[] | undefined,
): boolean {
  if (previous === undefined || next === undefined) {
    return previous === next;
  }

  if (previous.length !== next.length) {
    return false;
  }

  return previous.every((value, index) => Object.is(value, next[index]));
}

function cleanupInstance(instance: ComponentInstanceState): void {
  for (const hook of instance.hooks) {
    if (
      hook &&
      typeof hook === "object" &&
      "kind" in hook &&
      (hook as { kind?: string }).kind === "effect"
    ) {
      (hook as EffectHookSlot).cleanup?.();
    }
  }
}
