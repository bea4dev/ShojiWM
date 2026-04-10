import { resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { Socket, createConnection } from "node:net";
import { createInterface } from "node:readline";
import { format } from "node:util";

import {
  advanceAnimationFrame,
  beginProcessConfigRegistration,
  commitProcessConfigRegistration,
  drainPendingProcessActions,
  hasActiveAnimations,
  type CompiledEffectHandle,
  createReactiveLayer,
  createWindowAnimationControllerWithStore,
  createDecorationEvaluationCache,
  createManagedPoll,
  dropLayerDependencies,
  dropWindowDependencies,
  enterLayerDependencyScope,
  isSignal,
  installProcessResolverBridge,
  installShaderResolverBridge,
  installRuntimeHooks,
  takePendingDisplayConfig,
  takePendingProcessConfig,
  leaveLayerDependencyScope,
  read,
  takeDirtyLayerNodeIds,
  takeDirtyWindowNodeIds,
  type WindowManagerEventController,
  installSchedulerBridge,
  type DecorationEvaluationCache,
  type DisplayConfigDraft,
  type DecorationFunction,
  type OutputStateSnapshot,
  type PollCallback,
  type PollDirtyMode,
  type PollHandle,
  updateOutputState,
  type WaylandLayerSnapshot,
  type WaylandLayer,
  type WaylandWindowActions,
  type WaylandWindowSnapshot,
  type WindowTransform,
} from "shoji_wm";

interface EvaluateRequest {
  requestId: number;
  kind: "evaluate";
  snapshot: WaylandWindowSnapshot;
  nowMs: number;
  displayState: Record<string, OutputStateSnapshot>;
}

interface SchedulerTickRequest {
  requestId: number;
  kind: "schedulerTick";
  nowMs: number;
  displayState: Record<string, OutputStateSnapshot>;
}

interface WindowClosedRequest {
  requestId: number;
  kind: "windowClosed";
  windowId: string;
  displayState: Record<string, OutputStateSnapshot>;
}

interface StartCloseRequest {
  requestId: number;
  kind: "startClose";
  windowId: string;
  nowMs: number;
  displayState: Record<string, OutputStateSnapshot>;
}

interface EvaluateCachedRequest {
  requestId: number;
  kind: "evaluateCached";
  windowId: string;
  nowMs: number;
  displayState: Record<string, OutputStateSnapshot>;
}

interface InvokeHandlerRequest {
  requestId: number;
  kind: "invokeHandler";
  windowId: string;
  handlerId: string;
  nowMs: number;
  displayState: Record<string, OutputStateSnapshot>;
}

interface GetEffectConfigRequest {
  requestId: number;
  kind: "getEffectConfig";
  displayState: Record<string, OutputStateSnapshot>;
}

interface EvaluateLayerEffectsRequest {
  requestId: number;
  kind: "evaluateLayerEffects";
  outputName: string;
  nowMs: number;
  layers: WaylandLayerSnapshot[];
  displayState: Record<string, OutputStateSnapshot>;
}

type RuntimeRequest =
  | EvaluateRequest
  | SchedulerTickRequest
  | WindowClosedRequest
  | StartCloseRequest
  | EvaluateCachedRequest
  | InvokeHandlerRequest
  | GetEffectConfigRequest
  | EvaluateLayerEffectsRequest;

interface EvaluateSuccess {
  requestId: number;
  ok: true;
  kind: "evaluate";
  serialized: unknown;
  transform: WindowTransform;
  dirtyNodeIds?: string[];
  nextPollInMs?: number;
  displayConfig?: { outputs: DisplayConfigDraft };
  processConfig?: { entries: RuntimeProcessConfigEntry[] };
  processActions?: RuntimeProcessSpawnAction[];
}

interface SchedulerTickSuccess {
  requestId: number;
  ok: true;
  kind: "schedulerTick";
  dirty: boolean;
  dirtyWindowIds: string[];
  dirtyWindowNodeIds?: Record<string, string[]>;
  dirtyLayerNodeIds?: Record<string, string[]>;
  actions: RuntimeWindowAction[];
  nextPollInMs?: number;
  displayConfig?: { outputs: DisplayConfigDraft };
  processConfig?: { entries: RuntimeProcessConfigEntry[] };
  processActions?: RuntimeProcessSpawnAction[];
}

interface WindowClosedSuccess {
  requestId: number;
  ok: true;
  kind: "windowClosed";
  displayConfig?: { outputs: DisplayConfigDraft };
  processConfig?: { entries: RuntimeProcessConfigEntry[] };
  processActions?: RuntimeProcessSpawnAction[];
}

interface RuntimeWindowAction {
  windowId: string;
  action: "close" | "finalizeClose" | "maximize" | "minimize";
}

interface InvokeHandlerSuccess {
  requestId: number;
  ok: true;
  kind: "invokeHandler";
  invoked: boolean;
  serialized?: unknown;
  transform?: WindowTransform;
  dirtyWindowIds: string[];
  dirtyWindowNodeIds?: Record<string, string[]>;
  actions: RuntimeWindowAction[];
  nextPollInMs?: number;
  displayConfig?: { outputs: DisplayConfigDraft };
  processConfig?: { entries: RuntimeProcessConfigEntry[] };
  processActions?: RuntimeProcessSpawnAction[];
}

interface StartCloseSuccess {
  requestId: number;
  ok: true;
  kind: "startClose";
  invoked: boolean;
  serialized?: unknown;
  transform?: WindowTransform;
  dirtyWindowIds: string[];
  dirtyWindowNodeIds?: Record<string, string[]>;
  actions: RuntimeWindowAction[];
  nextPollInMs?: number;
  displayConfig?: { outputs: DisplayConfigDraft };
  processConfig?: { entries: RuntimeProcessConfigEntry[] };
  processActions?: RuntimeProcessSpawnAction[];
}

interface GetEffectConfigSuccess {
  requestId: number;
  ok: true;
  kind: "getEffectConfig";
  backgroundEffect?: CompiledEffectHandle | null;
  displayConfig?: { outputs: DisplayConfigDraft };
  processConfig?: { entries: RuntimeProcessConfigEntry[] };
  processActions?: RuntimeProcessSpawnAction[];
}

interface EvaluateLayerEffectsSuccess {
  requestId: number;
  ok: true;
  kind: "evaluateLayerEffects";
  effects: RuntimeLayerEffectAssignment[];
  nextPollInMs?: number;
  displayConfig?: { outputs: DisplayConfigDraft };
  processConfig?: { entries: RuntimeProcessConfigEntry[] };
  processActions?: RuntimeProcessSpawnAction[];
}

interface RuntimeFailure {
  requestId: number;
  ok: false;
  error: string;
  displayConfig?: { outputs: DisplayConfigDraft };
}

interface RuntimeLayerEffectAssignment {
  layerId: string;
  effect: CompiledEffectHandle | null;
}

interface RuntimeProcessConfigEntry {
  id: string;
  kind: "once" | "service";
  cwd?: string;
  env?: Record<string, string>;
  command?: string[];
  shell?: string;
  runPolicy?: "once-per-session" | "once-per-config-version";
  restart?: "never" | "on-failure" | "on-exit";
  reload?: "keep-if-unchanged" | "always-restart";
}

interface RuntimeProcessSpawnAction {
  cwd?: string;
  env?: Record<string, string>;
  command?: string[];
  shell?: string;
}

function pendingDisplayConfigPayload():
  | { outputs: DisplayConfigDraft }
  | undefined {
  const outputs = takePendingDisplayConfig();
  return outputs ? { outputs } : undefined;
}

function pendingProcessConfigPayload():
  | { entries: RuntimeProcessConfigEntry[] }
  | undefined {
  const entries = takePendingProcessConfig() as RuntimeProcessConfigEntry[] | undefined;
  return entries ? { entries } : undefined;
}

function pendingProcessActionsPayload():
  | RuntimeProcessSpawnAction[]
  | undefined {
  const actions = drainPendingProcessActions() as RuntimeProcessSpawnAction[];
  return actions.length > 0 ? actions : undefined;
}

const cacheByWindowId = new Map<string, RuntimeCacheEntry>();
const openedWindowIds = new Set<string>();
const animationEntriesByWindowId = new Map<string, Map<symbol, unknown>>();
const cacheByLayerId = new Map<string, RuntimeLayerEntry>();
const openedLayerIds = new Set<string>();
const animationEntriesByLayerId = new Map<string, Map<symbol, unknown>>();
const polls = new Map<number, RuntimePoll>();
const dirtyWindowIds = new Set<string>();
const dirtyLayerIds = new Set<string>();
let runtimeDirty = false;
let nextPollId = 1;
let currentSchedulerTimeMs = 0;

interface RuntimeCacheEntry {
  latestSnapshot: WaylandWindowSnapshot;
  cache: DecorationEvaluationCache;
  pendingActions: RuntimeWindowAction[];
  closeAnimationDurationMs: number;
  closeStarted: boolean;
  closePoll?: PollHandle;
}

interface RuntimeLayerEntry {
  latestSnapshot: WaylandLayerSnapshot;
  layer: ReturnType<typeof createReactiveLayer>["layer"];
  update(snapshot: WaylandLayerSnapshot): void;
}

interface RuntimePoll {
  intervalMs: number;
  nextRunAtMs: number;
  callback: PollCallback;
  handle: PollHandle;
  nowMs: number;
  dirtyMode: PollDirtyMode;
}

function installRuntimeConsoleBridge() {
  const original = { ...console };
  const emit = (level: "debug" | "info" | "warn" | "error", args: unknown[]) => {
    const message = format(...args);
    process.stderr.write(
      `__SHOJI_RUNTIME_LOG__${JSON.stringify({ level, message })}\n`,
    );
  };

  console.debug = (...args: unknown[]) => emit("debug", args);
  console.log = (...args: unknown[]) => emit("info", args);
  console.info = (...args: unknown[]) => emit("info", args);
  console.warn = (...args: unknown[]) => emit("warn", args);
  console.error = (...args: unknown[]) => emit("error", args);

  return original;
}

async function main() {
  const configPath = process.argv[2];
  const socketPath = process.argv[3];
  if (!configPath || !socketPath) {
    throw new Error("usage: tsx tools/decoration-runtime.ts <config-path> <socket-path>");
  }
  installRuntimeConsoleBridge();

  installSchedulerBridge({
    registerPoll(intervalMs, callback, dirtyMode) {
      return registerPoll(intervalMs, callback, dirtyMode);
    },
  });
  installRuntimeHooks({
    markRuntimeDirty() {
      runtimeDirty = true;
    },
    markWindowDirty(windowId) {
      dirtyWindowIds.add(windowId);
    },
    markLayerDirty(layerId) {
      dirtyLayerIds.add(layerId);
    },
  });

  const moduleUrl = pathToFileURL(resolve(configPath)).href;
  installShaderResolverBridge(resolve(configPath));
  installProcessResolverBridge(resolve(configPath));
  beginProcessConfigRegistration();
  const loaded = await import(moduleUrl).finally(() => {
    commitProcessConfigRegistration();
  });
  const decoration = resolveDecoration(loaded);
  const events = resolveEvents(loaded);
  const effectConfig = resolveEffectConfig(loaded);

  const socket = await connectSocket(socketPath);
  const rl = createInterface({
    input: socket,
    crlfDelay: Infinity,
  });

  for await (const line of rl) {
    if (!line.trim()) {
      continue;
    }

    let request: RuntimeRequest;
    try {
      request = JSON.parse(line) as RuntimeRequest;
    } catch (error) {
        writeResponse(socket, {
          requestId: -1,
          ok: false,
          error: error instanceof Error ? error.message : String(error),
      });
      continue;
    }

    try {
      updateOutputState(request.displayState);
      if (request.kind === "evaluate") {
        currentSchedulerTimeMs = request.nowMs;
        advanceAnimationFrame(request.nowMs);
        const serialized = evaluateSnapshot(decoration, events, request.snapshot);
        const processConfig = pendingProcessConfigPayload();
        const processActions = pendingProcessActionsPayload();
        writeResponse(socket, {
          requestId: request.requestId,
          ok: true,
          kind: "evaluate",
          serialized,
          transform: cacheByWindowId.get(request.snapshot.id)?.cache.lastTransform ??
            identityTransform(),
          dirtyNodeIds: takeDirtyWindowNodeIds(request.snapshot.id),
          nextPollInMs: hasActiveAnimations() ? 0 : peekNextPollDelay(),
          displayConfig: pendingDisplayConfigPayload(),
          processConfig,
          processActions,
        });
      } else {
        if (request.kind === "schedulerTick") {
          const tick = processSchedulerTick(request.nowMs);
          const processConfig = pendingProcessConfigPayload();
          const processActions = pendingProcessActionsPayload();
          writeResponse(socket, {
            requestId: request.requestId,
            ok: true,
            kind: "schedulerTick",
            dirty: tick.dirty,
            dirtyWindowIds: tick.dirtyWindowIds,
            dirtyWindowNodeIds: tick.dirtyWindowNodeIds,
            dirtyLayerNodeIds: tick.dirtyLayerNodeIds,
            actions: tick.actions,
            nextPollInMs: tick.nextPollInMs,
            displayConfig: pendingDisplayConfigPayload(),
            processConfig,
            processActions,
          });
        } else if (request.kind === "windowClosed") {
          closeWindow(events, request.windowId);
          const processConfig = pendingProcessConfigPayload();
          const processActions = pendingProcessActionsPayload();
          writeResponse(socket, {
            requestId: request.requestId,
            ok: true,
            kind: "windowClosed",
            displayConfig: pendingDisplayConfigPayload(),
            processConfig,
            processActions,
          });
        } else if (request.kind === "startClose") {
          currentSchedulerTimeMs = request.nowMs;
          const result = startClose(events, request.windowId);
          const processConfig = pendingProcessConfigPayload();
          const processActions = pendingProcessActionsPayload();
          writeResponse(socket, {
            requestId: request.requestId,
            ok: true,
            kind: "startClose",
            ...result,
            displayConfig: pendingDisplayConfigPayload(),
            processConfig,
            processActions,
          });
        } else if (request.kind === "evaluateCached") {
          currentSchedulerTimeMs = request.nowMs;
          advanceAnimationFrame(request.nowMs);
          const result = evaluateCached(request.windowId);
          const processConfig = pendingProcessConfigPayload();
          const processActions = pendingProcessActionsPayload();
          writeResponse(socket, {
            requestId: request.requestId,
            ok: true,
            kind: "evaluateCached",
            serialized: result.serialized,
            transform: result.transform,
            dirtyNodeIds: result.dirtyNodeIds,
            nextPollInMs: hasActiveAnimations() ? 0 : result.nextPollInMs,
            displayConfig: pendingDisplayConfigPayload(),
            processConfig,
            processActions,
          });
        } else if (request.kind === "getEffectConfig") {
          writeResponse(socket, {
            requestId: request.requestId,
            ok: true,
            kind: "getEffectConfig",
            backgroundEffect: effectConfig.background_effect,
            displayConfig: pendingDisplayConfigPayload(),
          });
        } else if (request.kind === "evaluateLayerEffects") {
          currentSchedulerTimeMs = request.nowMs;
          advanceAnimationFrame(request.nowMs);
          const result = evaluateLayerEffects(events, request.outputName, request.layers);
          const processConfig = pendingProcessConfigPayload();
          const processActions = pendingProcessActionsPayload();
          writeResponse(socket, {
            requestId: request.requestId,
            ok: true,
            kind: "evaluateLayerEffects",
            effects: result.effects,
            nextPollInMs: hasActiveAnimations() ? 0 : result.nextPollInMs,
            displayConfig: pendingDisplayConfigPayload(),
            processConfig,
            processActions,
          });
        } else {
          currentSchedulerTimeMs = request.nowMs;
          const result = invokeHandler(request.windowId, request.handlerId);
          const processConfig = pendingProcessConfigPayload();
          const processActions = pendingProcessActionsPayload();
          writeResponse(socket, {
            requestId: request.requestId,
            ok: true,
            kind: "invokeHandler",
            ...result,
            displayConfig: pendingDisplayConfigPayload(),
            processConfig,
            processActions,
          });
        }
      }
    } catch (error) {
      writeResponse(socket, {
        requestId: request.requestId,
        ok: false,
        error: error instanceof Error ? error.stack ?? error.message : String(error),
        displayConfig: pendingDisplayConfigPayload(),
      });
    }
  }
}

function evaluateCached(windowId: string): {
  serialized: unknown;
  transform: WindowTransform;
  dirtyNodeIds?: string[];
  nextPollInMs?: number;
} {
  const entry = cacheByWindowId.get(windowId);
  if (!entry) {
    throw new Error(`missing cache entry for closing window ${windowId}`);
  }

  const dirtyNodeIds = takeDirtyWindowNodeIds(windowId);
  const reevaluated = entry.cache.reevaluate(dirtyNodeIds);
  return {
    serialized: reevaluated.serialized,
    transform: entry.cache.lastTransform,
    dirtyNodeIds,
    nextPollInMs: hasActiveAnimations() ? 0 : peekNextPollDelay(),
  };
}

function evaluateSnapshot(
  decoration: DecorationFunction,
  events: WindowManagerEventController,
  snapshot: WaylandWindowSnapshot,
): unknown {
  const existing = cacheByWindowId.get(snapshot.id);
  if (!existing) {
    const entry = createRuntimeCacheEntry(snapshot, decoration);
    cacheByWindowId.set(snapshot.id, entry);
    if (!openedWindowIds.has(snapshot.id)) {
      openedWindowIds.add(snapshot.id);
      events.emitOpen(entry.cache.window);
    }
    events.emitFocus(entry.cache.window, snapshot.isFocused);
    dirtyWindowIds.delete(snapshot.id);
    return entry.cache.reevaluate(takeDirtyWindowNodeIds(snapshot.id)).serialized;
  }

  const focusChanged = existing.latestSnapshot.isFocused !== snapshot.isFocused;
  existing.latestSnapshot = snapshot;
  const updated = existing.cache.update(snapshot);
  if (focusChanged) {
    events.emitFocus(existing.cache.window, snapshot.isFocused);
  }

  const wasDirty = dirtyWindowIds.delete(snapshot.id);
  if (wasDirty) {
    const dirtyNodeIds = takeDirtyWindowNodeIds(snapshot.id);
    return existing.cache.reevaluate(dirtyNodeIds).serialized;
  }

  return updated?.serialized ?? existing.cache.lastSerialized;
}

function createRuntimeCacheEntry(
  snapshot: WaylandWindowSnapshot,
  decoration: DecorationFunction,
): RuntimeCacheEntry {
  let latestSnapshot = snapshot;
  const actions: WaylandWindowActions = {
    close() {
      entry.pendingActions.push({ windowId: latestSnapshot.id, action: "close" });
    },
    maximize() {
      entry.pendingActions.push({ windowId: latestSnapshot.id, action: "maximize" });
    },
    minimize() {
      entry.pendingActions.push({ windowId: latestSnapshot.id, action: "minimize" });
    },
    setCloseAnimationDuration(durationMs) {
      entry.closeAnimationDurationMs = Math.max(0, Math.floor(durationMs));
    },
    isXWayland() {
      return latestSnapshot.isXwayland;
    },
  };

  const animationEntries =
    animationEntriesByWindowId.get(snapshot.id) ?? new Map();
  animationEntriesByWindowId.set(snapshot.id, animationEntries);
  const animation = createWindowAnimationControllerWithStore(
    snapshot.id,
    animationEntries as Map<symbol, never>,
  );
  const cache = createDecorationEvaluationCache(snapshot, actions, decoration, animation);
  const entry: RuntimeCacheEntry = {
    latestSnapshot,
    cache,
    pendingActions: [],
    closeAnimationDurationMs: 0,
    closeStarted: false,
  };
  return entry;
}

function createRuntimeLayerEntry(
  snapshot: WaylandLayerSnapshot,
): RuntimeLayerEntry {
  const animationEntries =
    animationEntriesByLayerId.get(snapshot.id) ?? new Map();
  animationEntriesByLayerId.set(snapshot.id, animationEntries);
  const handle = createReactiveLayer(
    snapshot,
    createWindowAnimationControllerWithStore(
      snapshot.id,
      animationEntries as Map<symbol, never>,
    ),
  );
  return {
    latestSnapshot: snapshot,
    layer: handle.layer,
    update(nextSnapshot) {
      this.latestSnapshot = nextSnapshot;
      handle.update(nextSnapshot);
    },
  };
}

function evaluateLayerEffects(
  events: WindowManagerEventController,
  outputName: string,
  snapshots: WaylandLayerSnapshot[],
): {
  effects: RuntimeLayerEffectAssignment[];
  nextPollInMs?: number;
} {
  syncLayerSnapshots(events, snapshots);

  const effects: RuntimeLayerEffectAssignment[] = [];
  for (const snapshot of snapshots) {
    if (snapshot.outputName !== outputName) {
      continue;
    }
    const entry = cacheByLayerId.get(snapshot.id);
    if (!entry) {
      continue;
    }
    effects.push({
      layerId: snapshot.id,
      effect: snapshotLayerEffect(entry.layer),
    });
  }

  return {
    effects,
    nextPollInMs: hasActiveAnimations() ? 0 : peekNextPollDelay(),
  };
}

function syncLayerSnapshots(
  events: WindowManagerEventController,
  snapshots: WaylandLayerSnapshot[],
): void {
  const nextIds = new Set(snapshots.map((snapshot) => snapshot.id));

  for (const snapshot of snapshots) {
    const existing = cacheByLayerId.get(snapshot.id);
    if (!existing) {
      const entry = createRuntimeLayerEntry(snapshot);
      cacheByLayerId.set(snapshot.id, entry);
      if (!openedLayerIds.has(snapshot.id)) {
        openedLayerIds.add(snapshot.id);
        events.emitCreateLayer(entry.layer);
      }
      continue;
    }
    existing.update(snapshot);
  }

  for (const [layerId, existing] of cacheByLayerId) {
    if (nextIds.has(layerId)) {
      continue;
    }
    events.emitDestroyLayer(existing.layer);
    cacheByLayerId.delete(layerId);
    openedLayerIds.delete(layerId);
    animationEntriesByLayerId.delete(layerId);
    dirtyLayerIds.delete(layerId);
    dropLayerDependencies(layerId);
  }
}

function snapshotLayerEffect(layer: WaylandLayer): CompiledEffectHandle | null {
  enterLayerDependencyScope(layer.id);
  try {
    if (layer.effect == null) {
      return null;
    }
    return resolveSignals(layer.effect) as CompiledEffectHandle;
  } finally {
    leaveLayerDependencyScope();
  }
}

function resolveSignals<T>(value: T): T {
  if (isSignal(value)) {
    return read(value) as T;
  }
  if (Array.isArray(value)) {
    return value.map((item) => resolveSignals(item)) as T;
  }
  if (value && typeof value === "object") {
    const resolved: Record<string, unknown> = {};
    for (const [key, entry] of Object.entries(value as Record<string, unknown>)) {
      resolved[key] = resolveSignals(entry);
    }
    return resolved as T;
  }
  return value;
}

function identityTransform(): WindowTransform {
  return {
    origin: { x: 0.5, y: 0.5 },
    translateX: 0,
    translateY: 0,
    scaleX: 1,
    scaleY: 1,
    opacity: 1,
  };
}

function registerPoll(
  intervalMs: number,
  callback: PollCallback,
  dirtyMode: PollDirtyMode,
): PollHandle {
  const pollId = nextPollId++;
  const normalizedIntervalMs = Math.max(1, Math.floor(intervalMs));
  let cancelled = false;

  const handle: PollHandle = {
    cancel() {
      cancelled = true;
      polls.delete(pollId);
    },
    get cancelled() {
      return cancelled;
    },
    get nowMs() {
      return currentSchedulerTimeMs;
    },
  };

  polls.set(pollId, {
    intervalMs: normalizedIntervalMs,
    nextRunAtMs: currentSchedulerTimeMs + normalizedIntervalMs,
    callback,
    handle,
    nowMs: currentSchedulerTimeMs,
    dirtyMode,
  });

  return handle;
}

function processSchedulerTick(nowMs: number): {
  dirty: boolean;
  dirtyWindowIds: string[];
  dirtyWindowNodeIds?: Record<string, string[]>;
  dirtyLayerNodeIds?: Record<string, string[]>;
  actions: RuntimeWindowAction[];
  nextPollInMs?: number;
} {
  currentSchedulerTimeMs = nowMs;
  const animationsActive = hasActiveAnimations();

  for (const [pollId, poll] of polls) {
    if (poll.handle.cancelled) {
      polls.delete(pollId);
      continue;
    }

    if (poll.nextRunAtMs > nowMs) {
      continue;
    }

    poll.nowMs = nowMs;
    poll.nextRunAtMs = nowMs + poll.intervalMs;
    poll.callback(poll.handle);
    if (poll.dirtyMode === "runtime") {
      runtimeDirty = true;
    }

    if (poll.handle.cancelled) {
      polls.delete(pollId);
    }
  }

  let nextPollInMs: number | undefined;
  for (const poll of polls.values()) {
    if (poll.handle.cancelled) {
      continue;
    }
    const delay = Math.max(1, poll.nextRunAtMs - nowMs);
    nextPollInMs =
      nextPollInMs === undefined ? delay : Math.min(nextPollInMs, delay);
  }

  const nextDirtyWindowIds = Array.from(dirtyWindowIds);
  dirtyWindowIds.clear();
  const nextDirtyLayerIds = Array.from(dirtyLayerIds);
  dirtyLayerIds.clear();
  const dirtyWindowNodeIds = Object.fromEntries(
    nextDirtyWindowIds
      .map((windowId) => [windowId, takeDirtyWindowNodeIds(windowId)] as const)
      .filter(([, nodeIds]) => nodeIds.length > 0),
  );
  const dirtyLayerNodeIds = Object.fromEntries(
    nextDirtyLayerIds
      .map((layerId) => [layerId, takeDirtyLayerNodeIds(layerId)] as const)
      .filter(([, nodeIds]) => nodeIds.length > 0),
  );
  const actions = drainPendingActions();
  const dirty = runtimeDirty || nextDirtyWindowIds.length > 0 || nextDirtyLayerIds.length > 0;
  runtimeDirty = false;

  return {
    dirty,
    dirtyWindowIds: nextDirtyWindowIds,
    dirtyWindowNodeIds:
      Object.keys(dirtyWindowNodeIds).length > 0 ? dirtyWindowNodeIds : undefined,
    dirtyLayerNodeIds:
      Object.keys(dirtyLayerNodeIds).length > 0 ? dirtyLayerNodeIds : undefined,
    actions,
    nextPollInMs,
  };
}

function closeWindow(events: WindowManagerEventController, windowId: string): void {
  const existing = cacheByWindowId.get(windowId);
  if (!existing) {
    return;
  }

  existing.closePoll?.cancel();
  events.emitClose(existing.cache.window);
  cacheByWindowId.delete(windowId);
  openedWindowIds.delete(windowId);
  animationEntriesByWindowId.delete(windowId);
  dirtyWindowIds.delete(windowId);
  dropWindowDependencies(windowId);
}

function startClose(
  events: WindowManagerEventController,
  windowId: string,
): Omit<StartCloseSuccess, "requestId" | "ok" | "kind"> {
  const entry = cacheByWindowId.get(windowId);
  if (!entry) {
    return {
      invoked: false,
      dirtyWindowIds: [],
      actions: [],
      nextPollInMs: hasActiveAnimations() ? 0 : peekNextPollDelay(),
    };
  }

  if (!entry.closeStarted) {
    entry.closeStarted = true;
    events.emitStartClose(entry.cache.window);

    const durationMs = entry.closeAnimationDurationMs;
    if (durationMs <= 0) {
      entry.pendingActions.push({ windowId, action: "finalizeClose" });
    } else {
      entry.closePoll?.cancel();
      entry.closePoll = createManagedPoll(
        durationMs,
        (handle) => {
          const current = cacheByWindowId.get(windowId);
          if (!current || !current.closeStarted) {
            handle.cancel();
            return;
          }
          current.pendingActions.push({ windowId, action: "finalizeClose" });
          dirtyWindowIds.add(windowId);
          handle.cancel();
          current.closePoll = undefined;
        },
        "none",
      );
    }
  }

  const dirtyNodeIds = takeDirtyWindowNodeIds(windowId);
  const reevaluated = entry.cache.reevaluate(dirtyNodeIds);
  const actions = entry.pendingActions.splice(0, entry.pendingActions.length);
  return {
    invoked: true,
    serialized: reevaluated.serialized,
    transform: entry.cache.lastTransform,
    dirtyWindowIds: [windowId],
    dirtyWindowNodeIds: { [windowId]: dirtyNodeIds },
    actions,
    nextPollInMs: hasActiveAnimations() ? 0 : peekNextPollDelay(),
  };
}

function invokeHandler(
  windowId: string,
  handlerId: string,
): Omit<InvokeHandlerSuccess, "requestId" | "ok" | "kind"> {
  const entry = cacheByWindowId.get(windowId);
  if (!entry) {
    return {
      invoked: false,
      dirtyWindowIds: [],
      actions: [],
      nextPollInMs: hasActiveAnimations() ? 0 : peekNextPollDelay(),
    };
  }

  const invoked = entry.cache.invokeHandler(handlerId);
  if (!invoked) {
    return {
      invoked: false,
      dirtyWindowIds: [],
      actions: [],
      nextPollInMs: hasActiveAnimations() ? 0 : peekNextPollDelay(),
    };
  }

  const dirtyNodeIds = takeDirtyWindowNodeIds(windowId);
  const reevaluated = entry.cache.reevaluate(dirtyNodeIds);
  const actions = entry.pendingActions.splice(0, entry.pendingActions.length);

  return {
    invoked: true,
    serialized: reevaluated.serialized,
    transform: entry.cache.lastTransform,
    dirtyWindowIds: [windowId],
    dirtyWindowNodeIds: { [windowId]: dirtyNodeIds },
    actions,
    nextPollInMs: hasActiveAnimations() ? 0 : peekNextPollDelay(),
  };
}

function peekNextPollDelay(): number | undefined {
  let nextPollInMs: number | undefined;
  for (const poll of polls.values()) {
    if (poll.handle.cancelled) {
      continue;
    }
    const delay = Math.max(1, poll.nextRunAtMs - currentSchedulerTimeMs);
    nextPollInMs =
      nextPollInMs === undefined ? delay : Math.min(nextPollInMs, delay);
  }
  return nextPollInMs;
}

function drainPendingActions(): RuntimeWindowAction[] {
  const actions: RuntimeWindowAction[] = [];
  for (const entry of cacheByWindowId.values()) {
    if (entry.pendingActions.length === 0) {
      continue;
    }
    actions.push(...entry.pendingActions.splice(0, entry.pendingActions.length));
  }
  return actions;
}

function resolveDecoration(
  loaded: Record<string, unknown>,
): DecorationFunction {
  const maybeDecoration =
    (loaded.WINDOW_MANAGER as { decoration?: DecorationFunction } | undefined)
      ?.decoration ??
    (loaded.default as { decoration?: DecorationFunction } | undefined)?.decoration ??
    (loaded.decoration as DecorationFunction | undefined);

  if (!maybeDecoration) {
    throw new Error(
      "config module did not export WINDOW_MANAGER.decoration",
    );
  }

  return maybeDecoration;
}

function resolveEvents(
  loaded: Record<string, unknown>,
): WindowManagerEventController {
  const maybeEvents =
    (loaded.WINDOW_MANAGER as { event?: WindowManagerEventController } | undefined)?.event ??
    (loaded.default as { event?: WindowManagerEventController } | undefined)?.event;

  if (!maybeEvents) {
    throw new Error(
      "config module did not export WINDOW_MANAGER.event",
    );
  }

  return maybeEvents;
}

function resolveEffectConfig(
  loaded: Record<string, unknown>,
) : { background_effect: CompiledEffectHandle | null } {
  const maybeEffect =
    (loaded.WINDOW_MANAGER as { effect?: { background_effect?: CompiledEffectHandle | null } } | undefined)
      ?.effect ??
    (loaded.default as { effect?: { background_effect?: CompiledEffectHandle | null } } | undefined)?.effect;

  return {
    background_effect: maybeEffect?.background_effect ?? null,
  };
}

async function connectSocket(socketPath: string): Promise<Socket> {
  return await new Promise((resolveSocket, reject) => {
    const socket = createConnection(socketPath);
    socket.once("connect", () => resolveSocket(socket));
    socket.once("error", reject);
  });
}

function writeResponse(
  socket: Socket,
  response:
    | EvaluateSuccess
    | SchedulerTickSuccess
    | WindowClosedSuccess
    | StartCloseSuccess
    | InvokeHandlerSuccess
    | GetEffectConfigSuccess
    | EvaluateLayerEffectsSuccess
    | RuntimeFailure,
) {
  socket.write(`${JSON.stringify(response)}\n`);
}

main().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
