import { resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { Socket, createConnection } from "node:net";
import { createInterface } from "node:readline";

import {
  createWindowAnimationControllerWithStore,
  createDecorationEvaluationCache,
  createManagedPoll,
  dropWindowDependencies,
  installRuntimeHooks,
  type WindowManagerEventController,
  installSchedulerBridge,
  type DecorationEvaluationCache,
  type DecorationFunction,
  type PollCallback,
  type PollDirtyMode,
  type PollHandle,
  type WaylandWindowActions,
  type WaylandWindowSnapshot,
  type WindowTransform,
} from "shoji_wm";

interface EvaluateRequest {
  requestId: number;
  kind: "evaluate";
  snapshot: WaylandWindowSnapshot;
}

interface SchedulerTickRequest {
  requestId: number;
  kind: "schedulerTick";
  nowMs: number;
}

interface WindowClosedRequest {
  requestId: number;
  kind: "windowClosed";
  windowId: string;
}

interface StartCloseRequest {
  requestId: number;
  kind: "startClose";
  windowId: string;
  nowMs: number;
}

interface EvaluateCachedRequest {
  requestId: number;
  kind: "evaluateCached";
  windowId: string;
}

interface InvokeHandlerRequest {
  requestId: number;
  kind: "invokeHandler";
  windowId: string;
  handlerId: string;
  nowMs: number;
}

type RuntimeRequest =
  | EvaluateRequest
  | SchedulerTickRequest
  | WindowClosedRequest
  | StartCloseRequest
  | EvaluateCachedRequest
  | InvokeHandlerRequest;

interface EvaluateSuccess {
  requestId: number;
  ok: true;
  kind: "evaluate";
  serialized: unknown;
  transform: WindowTransform;
  nextPollInMs?: number;
}

interface SchedulerTickSuccess {
  requestId: number;
  ok: true;
  kind: "schedulerTick";
  dirty: boolean;
  dirtyWindowIds: string[];
  actions: RuntimeWindowAction[];
  nextPollInMs?: number;
}

interface WindowClosedSuccess {
  requestId: number;
  ok: true;
  kind: "windowClosed";
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
  actions: RuntimeWindowAction[];
  nextPollInMs?: number;
}

interface StartCloseSuccess {
  requestId: number;
  ok: true;
  kind: "startClose";
  invoked: boolean;
  serialized?: unknown;
  transform?: WindowTransform;
  dirtyWindowIds: string[];
  actions: RuntimeWindowAction[];
  nextPollInMs?: number;
}

interface RuntimeFailure {
  requestId: number;
  ok: false;
  error: string;
}

const cacheByWindowId = new Map<string, RuntimeCacheEntry>();
const openedWindowIds = new Set<string>();
const animationEntriesByWindowId = new Map<string, Map<symbol, unknown>>();
const polls = new Map<number, RuntimePoll>();
const dirtyWindowIds = new Set<string>();
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

interface RuntimePoll {
  intervalMs: number;
  nextRunAtMs: number;
  callback: PollCallback;
  handle: PollHandle;
  nowMs: number;
  dirtyMode: PollDirtyMode;
}

async function main() {
  const configPath = process.argv[2];
  const socketPath = process.argv[3];
  if (!configPath || !socketPath) {
    throw new Error("usage: tsx tools/decoration-runtime.ts <config-path> <socket-path>");
  }

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
  });

  const moduleUrl = pathToFileURL(resolve(configPath)).href;
  const loaded = await import(moduleUrl);
  const decoration = resolveDecoration(loaded);
  const events = resolveEvents(loaded);

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
      if (request.kind === "evaluate") {
        const serialized = evaluateSnapshot(decoration, events, request.snapshot);
        writeResponse(socket, {
          requestId: request.requestId,
          ok: true,
          kind: "evaluate",
          serialized,
          transform: cacheByWindowId.get(request.snapshot.id)?.cache.lastTransform ??
            identityTransform(),
          nextPollInMs: peekNextPollDelay(),
        });
      } else {
        if (request.kind === "schedulerTick") {
          const tick = processSchedulerTick(request.nowMs);
          writeResponse(socket, {
            requestId: request.requestId,
            ok: true,
            kind: "schedulerTick",
            dirty: tick.dirty,
            dirtyWindowIds: tick.dirtyWindowIds,
            actions: tick.actions,
            nextPollInMs: tick.nextPollInMs,
          });
        } else if (request.kind === "windowClosed") {
          closeWindow(events, request.windowId);
          writeResponse(socket, {
            requestId: request.requestId,
            ok: true,
            kind: "windowClosed",
          });
        } else if (request.kind === "startClose") {
          currentSchedulerTimeMs = request.nowMs;
          const result = startClose(events, request.windowId);
          writeResponse(socket, {
            requestId: request.requestId,
            ok: true,
            kind: "startClose",
            ...result,
          });
        } else if (request.kind === "evaluateCached") {
          const result = evaluateCached(request.windowId);
          writeResponse(socket, {
            requestId: request.requestId,
            ok: true,
            kind: "evaluateCached",
            serialized: result.serialized,
            transform: result.transform,
            nextPollInMs: result.nextPollInMs,
          });
        } else {
          currentSchedulerTimeMs = request.nowMs;
          const result = invokeHandler(request.windowId, request.handlerId);
          writeResponse(socket, {
            requestId: request.requestId,
            ok: true,
            kind: "invokeHandler",
            ...result,
          });
        }
      }
    } catch (error) {
      writeResponse(socket, {
        requestId: request.requestId,
        ok: false,
        error: error instanceof Error ? error.stack ?? error.message : String(error),
      });
    }
  }
}

function evaluateCached(windowId: string): {
  serialized: unknown;
  transform: WindowTransform;
  nextPollInMs?: number;
} {
  const entry = cacheByWindowId.get(windowId);
  if (!entry) {
    throw new Error(`missing cache entry for closing window ${windowId}`);
  }

  const reevaluated = entry.cache.reevaluate();
  return {
    serialized: reevaluated.serialized,
    transform: entry.cache.lastTransform,
    nextPollInMs: peekNextPollDelay(),
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
    return entry.cache.reevaluate().serialized;
  }

  const focusChanged = existing.latestSnapshot.isFocused !== snapshot.isFocused;
  const wasDirty = dirtyWindowIds.delete(snapshot.id);
  existing.latestSnapshot = snapshot;
  const updated = existing.cache.update(snapshot);
  if (focusChanged) {
    events.emitFocus(existing.cache.window, snapshot.isFocused);
  }

  if (focusChanged || wasDirty) {
    return existing.cache.reevaluate().serialized;
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
  actions: RuntimeWindowAction[];
  nextPollInMs?: number;
} {
  currentSchedulerTimeMs = nowMs;

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
  const actions = drainPendingActions();
  const dirty = runtimeDirty || nextDirtyWindowIds.length > 0;
  runtimeDirty = false;

  return {
    dirty,
    dirtyWindowIds: nextDirtyWindowIds,
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
      nextPollInMs: peekNextPollDelay(),
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

  const reevaluated = entry.cache.reevaluate();
  const actions = entry.pendingActions.splice(0, entry.pendingActions.length);
  return {
    invoked: true,
    serialized: reevaluated.serialized,
    transform: entry.cache.lastTransform,
    dirtyWindowIds: [windowId],
    actions,
    nextPollInMs: peekNextPollDelay(),
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
      nextPollInMs: peekNextPollDelay(),
    };
  }

  const invoked = entry.cache.invokeHandler(handlerId);
  if (!invoked) {
    return {
      invoked: false,
      dirtyWindowIds: [],
      actions: [],
      nextPollInMs: peekNextPollDelay(),
    };
  }

  const reevaluated = entry.cache.reevaluate();
  const actions = entry.pendingActions.splice(0, entry.pendingActions.length);

  return {
    invoked: true,
    serialized: reevaluated.serialized,
    transform: entry.cache.lastTransform,
    dirtyWindowIds: [windowId],
    actions,
    nextPollInMs: peekNextPollDelay(),
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
    | RuntimeFailure,
) {
  socket.write(`${JSON.stringify(response)}\n`);
}

main().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
