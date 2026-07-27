interface EmbeddedRuntimeBridge {
  readRequest(): Promise<EmbeddedRuntimeRequest | null>;
  writeResponse(response: string): void;
  writeInteractionResponse(response: NativeInteractionSuccess): void;
  writeCompositionUpdate(
    requestId: number,
    update: NativeCompositionUpdate,
  ): void;
  writeEffectUpdate(requestId: number, update: NativeEffectUpdate): void;
  beginEffectShaderUniformSlotPatches(
    requestId: number,
    updateKind: number,
  ): void;
  addEffectShaderUniformPatchTarget(
    requestId: number,
    targetId: string,
  ): void;
  registerEffectShaderUniformSlot(
    slotId: number,
    targetKind: number,
    targetId: string,
    effectSlot: number,
    shaderPathJson: string,
    name: string,
  ): void;
  clearEffectShaderUniformSlots(
    targetKind: number,
    targetId: string,
  ): void;
  writeEffectShaderUniformSlotPatch(
    requestId: number,
    slotId: number,
    valueLength: number,
    x: number,
    y: number,
    z: number,
    w: number,
  ): void;
  writeEffectShaderUniformArraySlotPatch(
    requestId: number,
    slotId: number,
    elementWidth: number,
    values: Float32Array,
  ): void;
  beginCompositionPatches(requestId: number, windowId: string): void;
  writeCompositionShaderUniformPatch(
    requestId: number,
    nodeId: string,
    stageIndex: number,
    name: string,
    valueLength: number,
    x: number,
    y: number,
    z: number,
    w: number,
  ): void;
  writeCompositionShaderUniformArrayPatch(
    requestId: number,
    nodeId: string,
    stageIndex: number,
    name: string,
    elementWidth: number,
    values: Float32Array,
  ): void;
  registerCompositionShaderUniformSlot(
    slotId: number,
    windowId: string,
    nodeId: string,
    stageIndex: number,
    name: string,
  ): void;
  clearCompositionShaderUniformSlots(windowId: string): void;
  beginCompositionShaderUniformSlotPatches(
    requestId: number,
    slotId: number,
  ): void;
  writeCompositionShaderUniformSlotPatch(
    requestId: number,
    slotId: number,
    valueLength: number,
    x: number,
    y: number,
    z: number,
    w: number,
  ): void;
  writeCompositionShaderUniformArraySlotPatch(
    requestId: number,
    slotId: number,
    elementWidth: number,
    values: Float32Array,
  ): void;
  beginSchedulerResponse(
    requestId: number,
    dirty: boolean,
    runtimeDirty: boolean,
    nextPollInMs: number,
  ): void;
  addSchedulerDirtyWindow(windowId: string, managedOnly: boolean): void;
  addSchedulerDirtyWindowNode(windowId: string, nodeId: string): void;
  addSchedulerDirtyLayer(layerId: string): void;
  addSchedulerDirtyLayerNode(layerId: string, nodeId: string): void;
  beginCachedResponse(payload: Uint8Array): void;
  addCachedDirtyNode(nodeId: string): void;
  addCachedVisibleOutput(outputName: string): void;
  setCachedWorkspaceString(workspace: string): void;
  setCachedWorkspaceNumber(workspace: number): void;
  finishNativeResponse(): void;
  log(level: "debug" | "info" | "warn" | "error", message: string): void;
}

interface EmbeddedRuntimeRequest {
  json(): string | null;
  composition(): RuntimeRequest | null;
  effect(): RuntimeRequest | null;
  interaction(): RuntimeRequest | null;
  scheduler(): SchedulerTickRequest | null;
  fastKind(): number;
  fastRequestId(): number;
  fastWindowId(): string;
  fastForceFullReevaluation(): boolean;
  fastNowMs(): number;
  finishFast(): void;
}

type NativeCompositionUpdate =
  | {
      kind: "full";
      windowId: string;
      tree: unknown;
    }
  | {
      kind: "patches";
      windowId: string;
      patches: Array<{ nodeId: string; node: unknown }>;
    };

type NativeEffectUpdate =
  | {
      kind: "background";
      effect: CompiledEffectHandle | null;
    }
  | {
      kind: "window";
      windowId: string;
      effects: WindowEffectAssignment | null;
    }
  | {
      kind: "layers";
      assignments: RuntimeLayerEffectAssignment[];
    }
  | {
      kind: "popups";
      assignments: RuntimePopupEffectAssignment[];
    };

interface EmbeddedRuntimeBridgeConstructor {
  new (bridgeId: number): EmbeddedRuntimeBridge;
}

const runtimeGlobal = globalThis as typeof globalThis & {
  ShojiRuntimeBridge?: EmbeddedRuntimeBridgeConstructor;
  __SHOJI_EMBEDDED_RUNTIME__?: boolean;
  Deno?: {
    cwd(): string;
    env: {
      get(key: string): string | undefined;
      set(key: string, value: string): void;
      delete(key: string): void;
    };
    statSync(path: string): unknown;
    inspect?(value: unknown): string;
  };
  process?: {
    cwd?(): string;
    env?: Record<string, string | undefined>;
  };
  __SHOJI_PATH_EXISTS__?: (path: string) => boolean;
};

function runtimeCwd(): string {
  return runtimeGlobal.Deno?.cwd() ?? runtimeGlobal.process?.cwd?.() ?? "/";
}

function normalizePath(path: string): string {
  const absolute = path.startsWith("/") ? path : `${runtimeCwd()}/${path}`;
  const parts: string[] = [];
  for (const part of absolute.split("/")) {
    if (!part || part === ".") continue;
    if (part === "..") {
      parts.pop();
      continue;
    }
    parts.push(part);
  }
  return `/${parts.join("/")}`;
}

function resolvePath(...paths: string[]): string {
  return normalizePath(paths.filter(Boolean).join("/"));
}

function dirnamePath(path: string): string {
  const normalized = normalizePath(path);
  const slash = normalized.lastIndexOf("/");
  return slash <= 0 ? "/" : normalized.slice(0, slash);
}

function pathToFileUrl(path: string): string {
  const url = new URL("file:///");
  url.pathname = normalizePath(path);
  return url.href;
}

function pathExists(path: string): boolean {
  if (runtimeGlobal.__SHOJI_PATH_EXISTS__) {
    return runtimeGlobal.__SHOJI_PATH_EXISTS__(path);
  }
  try {
    if (!runtimeGlobal.Deno?.statSync) return false;
    runtimeGlobal.Deno.statSync(path);
    return true;
  } catch {
    return false;
  }
}

function runtimeEnv(key: string): string | undefined {
  return runtimeGlobal.Deno?.env?.get(key) ?? runtimeGlobal.process?.env?.[key];
}

function formatRuntimeLog(args: unknown[]): string {
  return args
    .map((value) => {
      if (typeof value === "string") return value;
      if (value instanceof Error) return value.stack ?? value.message;
      if (runtimeGlobal.Deno?.inspect) {
        return runtimeGlobal.Deno.inspect(value);
      }
      try {
        return JSON.stringify(value);
      } catch {
        return String(value);
      }
    })
    .join(" ");
}

function findConfigRoot(entryPath: string): string {
  let dir = dirnamePath(resolvePath(entryPath));
  while (dir !== dirnamePath(dir)) {
    if (pathExists(`${dir}/package.json`)) {
      return dir;
    }
    dir = dirnamePath(dir);
  }
  return dirnamePath(resolvePath(entryPath));
}

function findPreloadPath(configPath: string): string | null {
  const candidate = resolvePath(
    dirnamePath(resolvePath(configPath)),
    "preload.ts",
  );
  return pathExists(candidate) ? candidate : null;
}

import {
  advanceAnimationFrame,
  beginKeyBindingRegistration,
  beginInputConfigurationRegistration,
  beginOutputConfigurationRegistration,
  beginPointerConfigRegistration,
  beginProcessConfigRegistration,
  beginWorkspaceConfigurationRegistration,
  commitKeyBindingRegistration,
  commitInputConfigurationRegistration,
  commitOutputConfigurationRegistration,
  commitPointerConfigRegistration,
  commitProcessConfigRegistration,
  commitWorkspaceConfigurationRegistration,
  drainPendingProcessActions,
  drainPendingEnvUpdates,
  hasActiveAnimations,
  hasActiveAnimationsInStore,
  type CompiledEffectHandle,
  type LayerEffectAssignment,
  type PopupEffectAssignment,
  createReactiveLayer,
  createReactiveWindow,
  createWindowAnimationControllerWithStore,
  createCompositionEvaluationCache,
  type WindowCompositionContext,
  createManagedPoll,
  consumeManagedWindowOnlyFastPathInvalidated,
  dropLayerDependencies,
  dropWindowDependencies,
  dropWindowState,
  enterLayerDependencyScope,
  isSignal,
  installAssetResolverBridge,
  installProcessResolverBridge,
  installRuntimeHooks,
  enterWindowEffectDependencyScope,
  invokeKeyBinding,
  managedWindowOnlyDirtyIds,
  takePendingDebugConfig,
  takePendingCursorConfig,
  takePendingDisplayConfig,
  takePendingKeyBindingConfig,
  takePendingInputConfig,
  takePendingPointerConfig,
  takePendingProcessConfig,
  takePendingWorkspaceConfig,
  leaveWindowEffectDependencyScope,
  leaveLayerDependencyScope,
  read,
  takeDirtyLayerNodeIds,
  takeManagedWindowOnlyDirty,
  takeDirtyWindowNodeIds,
  takeDirtyWindowShaderUniformBindingKeys,
  type CompositorEventController,
  installSchedulerBridge,
  isManagedWindowOnlyDirty,
  type CompositionEvaluationCache,
  type DisplayConfigDraft,
  type InputConfigDraft,
  type InputDeviceInfo,
  type WindowCompositionFunction,
  type OutputStateSnapshot,
  type WorkspaceConfig,
  emitWorkspaceActivate,
  type PollCallback,
  type PollDirtyMode,
  type PollHandle,
  type RuntimeWindowResizeEvent,
  type RuntimeWindowMoveEvent,
  type RuntimeWindowMaximizeRequestEvent,
  type RuntimeWindowMinimizeRequestEvent,
  type RuntimeWindowFullscreenRequestEvent,
  type RuntimeWindowActivateRequestEvent,
  type PointerMoveEvent,
  type GestureSwipeEvent,
  type RuntimeEventConfig,
  type RuntimePersistedState,
  updateOutputState,
  updateInputState,
  updateLayerSnapshots,
  COMPOSITOR,
  type SurfacePolicy,
  type WaylandLayerSnapshot,
  type WaylandLayer,
  type WaylandPopup,
  type WaylandWindowActions,
  type WaylandWindowSnapshot,
  type WindowDecorationContext,
  type WindowDecorationDecision,
  resolveWindowDecorationDecision,
  type WindowEffectAssignment,
  type ManagedWindowAnimationEasing,
  type ManagedWindowPoint,
  type ManagedWindowRect,
  type ManagedWindowScheduleAnimationOptions,
  type ManagedWindowState,
  type WindowTransform,
} from "../packages/shoji_wm/src/index.ts";
import { peekDirtyWindowNodeIds } from "../packages/shoji_wm/src/runtime-hooks.ts";

function debugSSD(
  message: string,
  details: Record<string, unknown> = {},
): void {
  if (!runtimeEnv("SHOJI_SSD_SUPPRESSION_DEBUG")) {
    return;
  }
  console.info(`ssd-suppression ${message}`, JSON.stringify(details));
}

function debugLabel(
  message: string,
  details: Record<string, unknown> = {},
): void {
  if (!runtimeEnv("SHOJI_LABEL_DEBUG")) {
    return;
  }
  console.info(`label-debug ${message}`, JSON.stringify(details));
}

function debugHotReload(
  message: string,
  details: Record<string, unknown> = {},
): void {
  if (!runtimeEnv("SHOJI_HOT_RELOAD_DEBUG")) {
    return;
  }
  console.info(`hot-reload-runtime ${message}`, JSON.stringify(details));
}

function snapshotForDebug(
  snapshot: WaylandWindowSnapshot,
): Record<string, unknown> {
  return {
    windowId: snapshot.id,
    title: snapshot.title,
    appId: snapshot.appId,
    position: snapshot.position,
    rect: snapshot.rect,
    focused: snapshot.isFocused,
    resizable: snapshot.isResizable,
    transient: snapshot.isTransient,
  };
}

function summarizeWindowAction(
  action: RuntimeWindowAction,
): Record<string, unknown> {
  return {
    windowId: action.windowId,
    action: action.action,
    channel: action.channel,
    animationChannel: action.animation?.channel,
    rect: action.animation?.rect,
    offset: action.animation?.offset,
    opacity: action.animation?.opacity,
  };
}

function summarizeAnimationEntries(
  entries: Map<string, Map<symbol, unknown>>,
): Record<string, unknown>[] {
  return Array.from(entries.entries()).map(([windowId, perWindow]) => ({
    windowId,
    entryCount: perWindow.size,
    entries: Array.from(perWindow.entries()).map(([key, value]) => {
      const entry = value as {
        progress?: { peek?: () => number };
        timeline?: {
          startedAtMs: number;
          durationMs: number;
          from: number;
          to: number;
          repeat?: unknown;
        };
      };
      return {
        variable: key.description,
        progress: entry.progress?.peek?.(),
        running: entry.timeline !== undefined,
        timeline: entry.timeline
          ? {
              startedAtMs: entry.timeline.startedAtMs,
              durationMs: entry.timeline.durationMs,
              from: entry.timeline.from,
              to: entry.timeline.to,
              repeat: entry.timeline.repeat,
            }
          : undefined,
      };
    }),
  }));
}

interface EvaluateRequest {
  requestId: number;
  kind: "evaluate";
  snapshot: WaylandWindowSnapshot;
  nowMs: number;
  displayState: Record<string, OutputStateSnapshot>;
  inputState?: Record<string, InputDeviceInfo>;
}

interface DrainPreloadRequest {
  requestId: number;
  kind: "drainPreload";
}

interface EvaluatePreviewRequest {
  requestId: number;
  kind: "evaluatePreview";
  snapshot: WaylandWindowSnapshot;
  nowMs: number;
  displayState: Record<string, OutputStateSnapshot>;
  inputState?: Record<string, InputDeviceInfo>;
}

interface WindowDecorationPolicyRequest {
  requestId: number;
  kind: "windowDecorationPolicy";
  snapshot: WaylandWindowSnapshot;
  context: WindowDecorationContext;
  displayState: Record<string, OutputStateSnapshot>;
  inputState?: Record<string, InputDeviceInfo>;
}

interface SchedulerTickRequest {
  requestId: number;
  kind: "schedulerTick";
  nowMs: number;
  displayState?: Record<string, OutputStateSnapshot>;
  inputState?: Record<string, InputDeviceInfo>;
}

interface WindowClosedRequest {
  requestId: number;
  kind: "windowClosed";
  windowId: string;
  displayState: Record<string, OutputStateSnapshot>;
  inputState?: Record<string, InputDeviceInfo>;
}

interface StartCloseRequest {
  requestId: number;
  kind: "startClose";
  windowId: string;
  nowMs: number;
  displayState: Record<string, OutputStateSnapshot>;
  inputState?: Record<string, InputDeviceInfo>;
}

interface EvaluateCachedRequest {
  requestId: number;
  kind: "evaluateCached";
  windowId: string;
  snapshot?: WaylandWindowSnapshot;
  forceFullReevaluation?: boolean;
  nowMs: number;
  displayState?: Record<string, OutputStateSnapshot>;
  inputState?: Record<string, InputDeviceInfo>;
}

interface InvokeHandlerRequest {
  requestId: number;
  kind: "invokeHandler";
  windowId: string;
  handlerId: string;
  nowMs: number;
  displayState: Record<string, OutputStateSnapshot>;
  inputState?: Record<string, InputDeviceInfo>;
}

interface InvokeKeyBindingRequest {
  requestId: number;
  kind: "invokeKeyBinding";
  bindingId: string;
  nowMs: number;
  displayState: Record<string, OutputStateSnapshot>;
  inputState?: Record<string, InputDeviceInfo>;
}

interface WindowResizeRequest {
  requestId: number;
  kind: "windowResize";
  windowId: string;
  event: RuntimeWindowResizeEvent;
  nowMs: number;
  displayState: Record<string, OutputStateSnapshot>;
  inputState?: Record<string, InputDeviceInfo>;
}

interface WindowMoveRequest {
  requestId: number;
  kind: "windowMove";
  windowId: string;
  event: RuntimeWindowMoveEvent;
  nowMs: number;
  displayState: Record<string, OutputStateSnapshot>;
  inputState?: Record<string, InputDeviceInfo>;
}

interface WindowMaximizeRequest {
  requestId: number;
  kind: "windowMaximizeRequest";
  windowId: string;
  snapshot: WaylandWindowSnapshot;
  event: RuntimeWindowMaximizeRequestEvent;
  nowMs: number;
  displayState: Record<string, OutputStateSnapshot>;
  inputState?: Record<string, InputDeviceInfo>;
}

interface WindowMinimizeRequest {
  requestId: number;
  kind: "windowMinimizeRequest";
  windowId: string;
  snapshot: WaylandWindowSnapshot;
  event: RuntimeWindowMinimizeRequestEvent;
  nowMs: number;
  displayState: Record<string, OutputStateSnapshot>;
  inputState?: Record<string, InputDeviceInfo>;
}

interface WindowFullscreenRequest {
  requestId: number;
  kind: "windowFullscreenRequest";
  windowId: string;
  snapshot: WaylandWindowSnapshot;
  event: RuntimeWindowFullscreenRequestEvent;
  nowMs: number;
  displayState: Record<string, OutputStateSnapshot>;
  inputState?: Record<string, InputDeviceInfo>;
}

interface WindowActivateRequest {
  requestId: number;
  kind: "windowActivateRequest";
  windowId: string;
  snapshot: WaylandWindowSnapshot;
  event: RuntimeWindowActivateRequestEvent;
  nowMs: number;
  displayState: Record<string, OutputStateSnapshot>;
  inputState?: Record<string, InputDeviceInfo>;
}

interface PointerMoveRequest {
  requestId: number;
  kind: "pointerMove" | "pointerMoveAsync";
  event: PointerMoveEvent;
  nowMs: number;
  displayState: Record<string, OutputStateSnapshot>;
  inputState?: Record<string, InputDeviceInfo>;
}

interface GestureSwipeRequest {
  requestId: number;
  kind: "gestureSwipe" | "gestureSwipeAsync";
  event: GestureSwipeEvent;
  nowMs: number;
  displayState: Record<string, OutputStateSnapshot>;
  inputState?: Record<string, InputDeviceInfo>;
}

interface GetEffectConfigRequest {
  requestId: number;
  kind: "getEffectConfig";
  displayState: Record<string, OutputStateSnapshot>;
  inputState?: Record<string, InputDeviceInfo>;
}

interface EvaluateLayerEffectsRequest {
  requestId: number;
  kind: "evaluateLayerEffects";
  outputName: string;
  nowMs: number;
  layers: WaylandLayerSnapshot[];
  displayState: Record<string, OutputStateSnapshot>;
  inputState?: Record<string, InputDeviceInfo>;
}

interface EvaluatePopupEffectsRequest {
  requestId: number;
  kind: "evaluatePopupEffects";
  outputName: string;
  nowMs: number;
  popups: WaylandPopup[];
  displayState: Record<string, OutputStateSnapshot>;
  inputState?: Record<string, InputDeviceInfo>;
}

interface LifecycleEnableRequest {
  requestId: number;
  kind: "lifecycleEnable";
  reason: "initial" | "reload";
  state?: RuntimePersistedState;
  environment?: Record<string, string>;
  displayState: Record<string, OutputStateSnapshot>;
  inputState?: Record<string, InputDeviceInfo>;
}

interface LifecycleDisableRequest {
  requestId: number;
  kind: "lifecycleDisable";
  reason: "reload" | "shutdown";
  displayState: Record<string, OutputStateSnapshot>;
}

interface WorkspaceActivateRequest {
  requestId: number;
  kind: "workspaceActivate";
  workspaceId: string;
  groupId?: string;
  nowMs: number;
  displayState: Record<string, OutputStateSnapshot>;
  inputState?: Record<string, InputDeviceInfo>;
}

type RuntimeRequest =
  | DrainPreloadRequest
  | EvaluateRequest
  | EvaluatePreviewRequest
  | WindowDecorationPolicyRequest
  | SchedulerTickRequest
  | WindowClosedRequest
  | StartCloseRequest
  | EvaluateCachedRequest
  | InvokeHandlerRequest
  | InvokeKeyBindingRequest
  | WindowResizeRequest
  | WindowMoveRequest
  | WindowMaximizeRequest
  | WindowMinimizeRequest
  | WindowFullscreenRequest
  | WindowActivateRequest
  | PointerMoveRequest
  | GestureSwipeRequest
  | GetEffectConfigRequest
  | EvaluateLayerEffectsRequest
  | EvaluatePopupEffectsRequest
  | LifecycleEnableRequest
  | LifecycleDisableRequest
  | WorkspaceActivateRequest;

type RuntimeRequestWithTimestamp = Extract<RuntimeRequest, { nowMs: number }>;

interface EvaluateSuccess {
  requestId: number;
  ok: true;
  kind: "evaluate" | "evaluatePreview" | "evaluateCached";
  serialized?: unknown;
  transform: WindowTransform;
  managedWindow: ManagedWindowState;
  windowEffects?: WindowEffectAssignment | null;
  effectTargetId?: string;
  dirtyNodeIds?: string[];
  managedWindowOnly?: boolean;
  nextPollInMs?: number;
  // Window actions queued by user handlers during this evaluation (typically
  // scheduleAnimation from onOpen/onFirstCommit). Returned here — rather than
  // letting them sit in pendingActions until the next scheduler tick — so the
  // compositor can apply them *before* sampling animations for the same
  // refresh, eliminating the one-frame flash at the static target position
  // before the open animation kicks in.
  actions?: RuntimeWindowAction[];
  displayConfig?: { outputs: DisplayConfigDraft };
  workspaceConfig?: WorkspaceConfig;
  keyBindingConfig?: { entries: RuntimeKeyBindingConfigEntry[] };
  pointerConfig?: RuntimePointerConfig;
  inputConfig?: { config: InputConfigDraft };
  eventConfig?: RuntimeEventConfig;
  processConfig?: { entries: RuntimeProcessConfigEntry[] };
  processActions?: RuntimeProcessSpawnAction[];
}

interface DrainPreloadSuccess {
  requestId: number;
  ok: true;
  kind: "drainPreload";
}

interface WindowDecorationPolicySuccess {
  requestId: number;
  ok: true;
  kind: "windowDecorationPolicy";
  decision: WindowDecorationDecision;
}

interface SchedulerTickSuccess {
  requestId: number;
  ok: true;
  kind: "schedulerTick";
  dirty: boolean;
  runtimeDirty?: boolean;
  dirtyWindowIds: string[];
  dirtyManagedWindowIds?: string[];
  dirtyWindowNodeIds?: Record<string, string[]>;
  dirtyLayerIds?: string[];
  dirtyLayerNodeIds?: Record<string, string[]>;
  actions: RuntimeWindowAction[];
  nextPollInMs?: number;
  displayConfig?: { outputs: DisplayConfigDraft };
  workspaceConfig?: WorkspaceConfig;
  keyBindingConfig?: { entries: RuntimeKeyBindingConfigEntry[] };
  pointerConfig?: RuntimePointerConfig;
  inputConfig?: { config: InputConfigDraft };
  eventConfig?: RuntimeEventConfig;
  processConfig?: { entries: RuntimeProcessConfigEntry[] };
  processActions?: RuntimeProcessSpawnAction[];
  debugConfig?: { fpsCounter: boolean; profile: boolean };
}

interface WindowClosedSuccess {
  requestId: number;
  ok: true;
  kind: "windowClosed";
  displayConfig?: { outputs: DisplayConfigDraft };
  workspaceConfig?: WorkspaceConfig;
  keyBindingConfig?: { entries: RuntimeKeyBindingConfigEntry[] };
  pointerConfig?: RuntimePointerConfig;
  inputConfig?: { config: InputConfigDraft };
  eventConfig?: RuntimeEventConfig;
  processConfig?: { entries: RuntimeProcessConfigEntry[] };
  processActions?: RuntimeProcessSpawnAction[];
}

interface RuntimeWindowAction {
  windowId: string;
  action:
    | "close"
    | "finalizeClose"
    | "maximize"
    | "unmaximize"
    | "minimize"
    | "fullscreen"
    | "unfullscreen"
    | "focus"
    | "scheduleAnimation"
    | "cancelAnimation";
  animation?: RuntimeManagedWindowAnimation;
  channel?: string;
}

interface RuntimeManagedWindowAnimation {
  channel: string;
  rect?: {
    from?: RuntimeManagedWindowRect;
    to: RuntimeManagedWindowRect;
    duration: number;
    easing: RuntimeManagedWindowAnimationEasing;
    mode: "override" | "add" | "sub";
  };
  offset?: {
    from?: RuntimeManagedWindowPoint;
    to: RuntimeManagedWindowPoint;
    duration: number;
    easing: RuntimeManagedWindowAnimationEasing;
    mode: "override" | "add" | "sub";
  };
  opacity?: {
    from?: number;
    to: number;
    duration: number;
    easing: RuntimeManagedWindowAnimationEasing;
    mode: "override" | "add" | "sub" | "multiply";
  };
}

interface RuntimeManagedWindowRect {
  x: number;
  y: number;
  width: number;
  height: number;
}

interface RuntimeManagedWindowPoint {
  x: number;
  y: number;
}

type RuntimeManagedWindowAnimationEasing =
  | { kind: "linear" }
  | { kind: "cubicBezier"; x1: number; y1: number; x2: number; y2: number };

interface InvokeHandlerSuccess {
  requestId: number;
  ok: true;
  kind: "invokeHandler";
  invoked: boolean;
  serialized?: unknown;
  transform?: WindowTransform;
  managedWindow?: ManagedWindowState;
  windowEffects?: WindowEffectAssignment | null;
  effectTargetId?: string;
  dirtyWindowIds: string[];
  dirtyManagedWindowIds?: string[];
  dirtyWindowNodeIds?: Record<string, string[]>;
  actions: RuntimeWindowAction[];
  nextPollInMs?: number;
  displayConfig?: { outputs: DisplayConfigDraft };
  workspaceConfig?: WorkspaceConfig;
  keyBindingConfig?: { entries: RuntimeKeyBindingConfigEntry[] };
  pointerConfig?: RuntimePointerConfig;
  inputConfig?: { config: InputConfigDraft };
  eventConfig?: RuntimeEventConfig;
  processConfig?: { entries: RuntimeProcessConfigEntry[] };
  processActions?: RuntimeProcessSpawnAction[];
}

interface InvokeKeyBindingSuccess {
  requestId: number;
  ok: true;
  kind: "invokeKeyBinding";
  invoked: boolean;
  dirty: boolean;
  dirtyWindowIds: string[];
  dirtyManagedWindowIds?: string[];
  dirtyWindowNodeIds?: Record<string, string[]>;
  dirtyLayerNodeIds?: Record<string, string[]>;
  actions: RuntimeWindowAction[];
  nextPollInMs?: number;
  displayConfig?: { outputs: DisplayConfigDraft };
  workspaceConfig?: WorkspaceConfig;
  keyBindingConfig?: { entries: RuntimeKeyBindingConfigEntry[] };
  pointerConfig?: RuntimePointerConfig;
  inputConfig?: { config: InputConfigDraft };
  eventConfig?: RuntimeEventConfig;
  processConfig?: { entries: RuntimeProcessConfigEntry[] };
  processActions?: RuntimeProcessSpawnAction[];
  debugConfig?: { fpsCounter: boolean; profile: boolean };
}

interface WindowResizeSuccess {
  requestId: number;
  ok: true;
  kind: "windowResize";
  invoked: boolean;
  dirty: boolean;
  dirtyWindowIds: string[];
  dirtyManagedWindowIds?: string[];
  dirtyWindowNodeIds?: Record<string, string[]>;
  dirtyLayerNodeIds?: Record<string, string[]>;
  actions: RuntimeWindowAction[];
  nextPollInMs?: number;
  displayConfig?: { outputs: DisplayConfigDraft };
  workspaceConfig?: WorkspaceConfig;
  keyBindingConfig?: { entries: RuntimeKeyBindingConfigEntry[] };
  pointerConfig?: RuntimePointerConfig;
  eventConfig?: RuntimeEventConfig;
  processConfig?: { entries: RuntimeProcessConfigEntry[] };
  processActions?: RuntimeProcessSpawnAction[];
}

interface WindowMoveSuccess {
  requestId: number;
  ok: true;
  kind: "windowMove";
  invoked: boolean;
  dirty: boolean;
  dirtyWindowIds: string[];
  dirtyManagedWindowIds?: string[];
  dirtyWindowNodeIds?: Record<string, string[]>;
  dirtyLayerNodeIds?: Record<string, string[]>;
  actions: RuntimeWindowAction[];
  nextPollInMs?: number;
  displayConfig?: { outputs: DisplayConfigDraft };
  workspaceConfig?: WorkspaceConfig;
  keyBindingConfig?: { entries: RuntimeKeyBindingConfigEntry[] };
  pointerConfig?: RuntimePointerConfig;
  eventConfig?: RuntimeEventConfig;
  processConfig?: { entries: RuntimeProcessConfigEntry[] };
  processActions?: RuntimeProcessSpawnAction[];
}

interface WindowStateRequestSuccess {
  requestId: number;
  ok: true;
  kind:
    | "windowMaximizeRequest"
    | "windowMinimizeRequest"
    | "windowFullscreenRequest"
    | "windowActivateRequest";
  invoked: boolean;
  dirty: boolean;
  dirtyWindowIds: string[];
  dirtyManagedWindowIds?: string[];
  dirtyWindowNodeIds?: Record<string, string[]>;
  dirtyLayerNodeIds?: Record<string, string[]>;
  actions: RuntimeWindowAction[];
  nextPollInMs?: number;
  displayConfig?: { outputs: DisplayConfigDraft };
  workspaceConfig?: WorkspaceConfig;
  keyBindingConfig?: { entries: RuntimeKeyBindingConfigEntry[] };
  pointerConfig?: RuntimePointerConfig;
  eventConfig?: RuntimeEventConfig;
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
  managedWindow?: ManagedWindowState;
  windowEffects?: WindowEffectAssignment | null;
  effectTargetId?: string;
  dirtyWindowIds: string[];
  dirtyManagedWindowIds?: string[];
  dirtyWindowNodeIds?: Record<string, string[]>;
  actions: RuntimeWindowAction[];
  nextPollInMs?: number;
  displayConfig?: { outputs: DisplayConfigDraft };
  workspaceConfig?: WorkspaceConfig;
  keyBindingConfig?: { entries: RuntimeKeyBindingConfigEntry[] };
  pointerConfig?: RuntimePointerConfig;
  eventConfig?: RuntimeEventConfig;
  processConfig?: { entries: RuntimeProcessConfigEntry[] };
  processActions?: RuntimeProcessSpawnAction[];
}

interface NativeInteractionSuccess {
  requestId: number;
  ok: true;
  kind:
    | "pointerMove"
    | "pointerMoveAsync"
    | "gestureSwipe"
    | "gestureSwipeAsync"
    | "windowMove"
    | "windowResize";
  invoked: boolean;
  dirty: boolean;
  dirtyWindowIds: string[];
  dirtyManagedWindowIds?: string[];
  dirtyWindowNodeIds?: Record<string, string[]>;
  dirtyLayerNodeIds?: Record<string, string[]>;
  actions: RuntimeWindowAction[];
  nextPollInMs?: number;
  displayConfig?: { outputs: DisplayConfigDraft };
  workspaceConfig?: WorkspaceConfig;
  keyBindingConfig?: { entries: RuntimeKeyBindingConfigEntry[] };
  pointerConfig?: RuntimePointerConfig;
  inputConfig?: { config: InputConfigDraft };
  eventConfig?: RuntimeEventConfig;
  processConfig?: { entries: RuntimeProcessConfigEntry[] };
  processActions?: RuntimeProcessSpawnAction[];
}

interface GetEffectConfigSuccess {
  requestId: number;
  ok: true;
  kind: "getEffectConfig";
  backgroundEffect?: CompiledEffectHandle | null;
  displayConfig?: { outputs: DisplayConfigDraft };
  workspaceConfig?: WorkspaceConfig;
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
  workspaceConfig?: WorkspaceConfig;
  keyBindingConfig?: { entries: RuntimeKeyBindingConfigEntry[] };
  pointerConfig?: RuntimePointerConfig;
  eventConfig?: RuntimeEventConfig;
  processConfig?: { entries: RuntimeProcessConfigEntry[] };
  processActions?: RuntimeProcessSpawnAction[];
}

interface EvaluatePopupEffectsSuccess {
  requestId: number;
  ok: true;
  kind: "evaluatePopupEffects";
  effects: RuntimePopupEffectAssignment[];
  nextPollInMs?: number;
  displayConfig?: { outputs: DisplayConfigDraft };
  workspaceConfig?: WorkspaceConfig;
  keyBindingConfig?: { entries: RuntimeKeyBindingConfigEntry[] };
  pointerConfig?: RuntimePointerConfig;
  inputConfig?: { config: InputConfigDraft };
  eventConfig?: RuntimeEventConfig;
  processConfig?: { entries: RuntimeProcessConfigEntry[] };
  processActions?: RuntimeProcessSpawnAction[];
}

interface LifecycleEnableSuccess {
  requestId: number;
  ok: true;
  kind: "lifecycleEnable";
  displayConfig?: { outputs: DisplayConfigDraft };
  workspaceConfig?: WorkspaceConfig;
  keyBindingConfig?: { entries: RuntimeKeyBindingConfigEntry[] };
  pointerConfig?: RuntimePointerConfig;
  eventConfig?: RuntimeEventConfig;
  processConfig?: { entries: RuntimeProcessConfigEntry[] };
  processActions?: RuntimeProcessSpawnAction[];
}

interface LifecycleDisableSuccess {
  requestId: number;
  ok: true;
  kind: "lifecycleDisable";
  state: RuntimePersistedState;
}

interface RuntimeFailure {
  requestId: number;
  ok: false;
  kind?: RuntimeRequest["kind"];
  error: string;
  displayConfig?: { outputs: DisplayConfigDraft };
  workspaceConfig?: WorkspaceConfig;
}

interface RuntimeLayerEffectAssignment {
  layerId: string;
  effects: LayerEffectAssignment | null;
}

interface RuntimePopupEffectAssignment {
  popupId: string;
  effects: PopupEffectAssignment | null;
  /** COMPOSITOR.rendering.surfacePolicy result for this popup's surface. */
  surfacePolicy?: SurfacePolicy | null;
}

interface RuntimeEffectConfig {
  background_effect: CompiledEffectHandle | null;
  window?: (
    window: ReturnType<typeof createCompositionEvaluationCache>["window"],
  ) => WindowEffectAssignment | null;
  layer?: (layer: WaylandLayer) => LayerEffectAssignment | null;
  popup?: (popup: WaylandPopup) => PopupEffectAssignment | null;
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

interface RuntimeKeyBindingConfigEntry {
  id: string;
  shortcut: string;
  on: "press" | "release";
}

interface RuntimePointerConfig {
  windowMoveModifier?: string;
}

function pendingDisplayConfigPayload():
  { outputs: DisplayConfigDraft } | undefined {
  const outputs = takePendingDisplayConfig();
  return outputs ? { outputs } : undefined;
}

function pendingWorkspaceConfigPayload(): WorkspaceConfig | undefined {
  return takePendingWorkspaceConfig();
}

function pendingProcessConfigPayload():
  { entries: RuntimeProcessConfigEntry[] } | undefined {
  const entries = takePendingProcessConfig() as
    RuntimeProcessConfigEntry[] | undefined;
  return entries ? { entries } : undefined;
}

function pendingProcessActionsPayload():
  RuntimeProcessSpawnAction[] | undefined {
  const actions = drainPendingProcessActions() as RuntimeProcessSpawnAction[];
  return actions.length > 0 ? actions : undefined;
}

function pendingKeyBindingConfigPayload():
  { entries: RuntimeKeyBindingConfigEntry[] } | undefined {
  const entries = takePendingKeyBindingConfig() as
    RuntimeKeyBindingConfigEntry[] | undefined;
  return entries ? { entries } : undefined;
}

function pendingPointerConfigPayload(): RuntimePointerConfig | undefined {
  return takePendingPointerConfig() as RuntimePointerConfig | undefined;
}

function pendingInputConfigPayload(): { config: InputConfigDraft } | undefined {
  const config = takePendingInputConfig();
  return config ? { config } : undefined;
}

function pendingEventConfigPayload(
  events: CompositorEventController,
): RuntimeEventConfig | undefined {
  return events.takePendingEventConfig();
}

function applyRuntimeEnvironment(
  environment: Record<string, string> | undefined,
) {
  if (!environment) {
    return;
  }
  for (const [key, value] of Object.entries(environment)) {
    runtimeGlobal.Deno?.env?.set(key, value);
    if (runtimeGlobal.process?.env) {
      runtimeGlobal.process.env[key] = value;
    }
  }
}

const cacheByWindowId = new Map<string, RuntimeCacheEntry>();
// Decoration negotiation can run before the first composition evaluation and
// during hot reload. Keep it out of cacheByWindowId so it cannot consume the
// onOpen/onFirstCommit lifecycle that restores workspace membership.
const decorationPolicyWindowById = new Map<
  string,
  DecorationPolicyWindowEntry
>();
const openedWindowIds = new Set<string>();
const initialConfiguredWindowIds = new Set<string>();
const firstCommittedWindowIds = new Set<string>();
const animationEntriesByWindowId = new Map<string, Map<symbol, unknown>>();
const cacheByLayerId = new Map<string, RuntimeLayerEntry>();
const openedLayerIds = new Set<string>();
const animationEntriesByLayerId = new Map<string, Map<symbol, unknown>>();
const polls = new Map<number, RuntimePoll>();
const dirtyWindowIds = new Set<string>();
const dirtyLayerIds = new Set<string>();
const lastNativeCompositionByWindowId = new Map<string, unknown>();
const shaderUniformSlotIdsByWindow = new Map<string, Map<string, number>>();
let nextShaderUniformSlotId = 1;
const lastNativeEffectByTarget = new Map<string, unknown>();
const lastNativeEffectStructureByTarget = new Map<string, unknown>();
const effectUniformSlotIdsByTarget = new Map<string, Map<string, number>>();
let nextEffectUniformSlotId = 1;
let runtimeDirty = false;
let immediateDirtyPoll: PollHandle | null = null;
let nextPollId = 1;
let currentSchedulerTimeMs = 0;
let lastAnimationAdvanceMs: number | undefined;

const RENDER_COMPOSITION_CONTEXT: WindowCompositionContext = {
  phase: "render",
  isPreview: false,
};

const PRECONFIGURE_COMPOSITION_CONTEXT: WindowCompositionContext = {
  phase: "preconfigure",
  isPreview: true,
};

interface RuntimeCacheEntry {
  latestSnapshot: WaylandWindowSnapshot;
  cache: CompositionEvaluationCache;
  animationEntries: Map<symbol, unknown>;
  pendingActions: RuntimeWindowAction[];
  closeAnimationDurationMs: number;
  closeStarted: boolean;
  preconfigured: boolean;
  closePoll?: PollHandle;
}

interface DecorationPolicyWindowEntry {
  snapshotRef: { current: WaylandWindowSnapshot };
  handle: ReturnType<typeof createReactiveWindow>;
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

function installRuntimeConsoleBridge(embeddedBridge: EmbeddedRuntimeBridge) {
  const emit = (
    level: "debug" | "info" | "warn" | "error",
    args: unknown[],
  ) => {
    embeddedBridge.log(level, formatRuntimeLog(args));
  };

  console.debug = (...args: unknown[]) => emit("debug", args);
  console.log = (...args: unknown[]) => emit("info", args);
  console.info = (...args: unknown[]) => emit("info", args);
  console.warn = (...args: unknown[]) => emit("warn", args);
  console.error = (...args: unknown[]) => emit("error", args);
}

function hasRuntimeTimestamp(
  request: RuntimeRequest,
): request is RuntimeRequestWithTimestamp {
  return "nowMs" in request;
}

function beginRuntimeTurn(nowMs: number): void {
  currentSchedulerTimeMs = nowMs;
  if (lastAnimationAdvanceMs === nowMs) {
    return;
  }
  lastAnimationAdvanceMs = nowMs;
  // A runtime turn may evaluate declarations or run user handlers, both of
  // which can start animations. Synchronizing once at the turn boundary keeps
  // every newly-created timeline anchored to the compositor timestamp for this
  // request instead of the previous composition evaluation.
  advanceAnimationFrame(nowMs);
}

// --- Diagnostic counters (SHOJI_RUNTIME_STATS=1) -----------------------------
const statsEnabled = runtimeEnv("SHOJI_RUNTIME_STATS") === "1";
const stats = {
  evaluate: 0,
  schedulerTick: 0,
  schedulerTickDirty: 0,
  invokeHandler: 0,
  invokeKeyBinding: 0,
  windowResize: 0,
  windowMove: 0,
  windowMaximizeRequest: 0,
  windowMinimizeRequest: 0,
  windowFullscreenRequest: 0,
  windowActivateRequest: 0,
  pointerMoveAsync: 0,
  gestureSwipeAsync: 0,
  getEffectConfig: 0,
  evaluateLayerEffects: 0,
  evaluateLayerEffectsAnim: 0,
  evaluatePopupEffects: 0,
  evaluatePopupEffectsAnim: 0,
  markWindowDirty: 0,
  markRuntimeDirty: 0,
  markLayerDirty: 0,
};
function startStatsLogger(): void {
  if (!statsEnabled) return;
  setInterval(() => {
    const total = Object.values(stats).reduce((a, b) => a + b, 0);
    if (total === 0) return;
    const snapshot = { ...stats };
    for (const key of Object.keys(stats) as (keyof typeof stats)[]) {
      stats[key] = 0;
    }
    console.error("[stats/1s]", JSON.stringify(snapshot));
  }, 1000);
}

async function main(configPath: string, embeddedBridge: EmbeddedRuntimeBridge) {
  installRuntimeConsoleBridge(embeddedBridge);
  startStatsLogger();

  installSchedulerBridge({
    registerPoll(intervalMs, callback, dirtyMode) {
      return registerPoll(intervalMs, callback, dirtyMode);
    },
  });
  installRuntimeHooks({
    markRuntimeDirty() {
      if (statsEnabled) stats.markRuntimeDirty++;
      runtimeDirty = true;
      ensureImmediateDirtyPoll();
    },
    markWindowDirty(windowId) {
      if (statsEnabled) stats.markWindowDirty++;
      dirtyWindowIds.add(windowId);
      ensureImmediateDirtyPoll();
    },
    markLayerDirty(layerId) {
      if (statsEnabled) stats.markLayerDirty++;
      dirtyLayerIds.add(layerId);
      ensureImmediateDirtyPoll();
    },
    wakeRuntime() {
      ensureImmediateDirtyPoll();
    },
  });

  const resolvedConfigPath = resolvePath(configPath);
  const moduleUrl = pathToFileUrl(resolvedConfigPath);
  installAssetResolverBridge(findConfigRoot(configPath));
  installProcessResolverBridge(resolvedConfigPath);

  const preloadPath = findPreloadPath(resolvedConfigPath);
  if (preloadPath) {
    await import(pathToFileUrl(preloadPath));
  }

  let loadedConfig: Record<string, unknown> | null = null;
  let composition: WindowCompositionFunction | null = null;
  let events: CompositorEventController | null = null;
  let effectConfig: RuntimeEffectConfig | null = null;

  async function loadRuntimeConfig(): Promise<{
    composition: WindowCompositionFunction;
    events: CompositorEventController;
    effectConfig: RuntimeEffectConfig;
  }> {
    if (!loadedConfig) {
      beginKeyBindingRegistration();
      beginOutputConfigurationRegistration();
      beginWorkspaceConfigurationRegistration();
      beginInputConfigurationRegistration();
      beginPointerConfigRegistration();
      beginProcessConfigRegistration();
      loadedConfig = (await import(moduleUrl).finally(() => {
        commitKeyBindingRegistration();
        commitOutputConfigurationRegistration();
        commitWorkspaceConfigurationRegistration();
        commitInputConfigurationRegistration();
        commitPointerConfigRegistration();
        commitProcessConfigRegistration();
      })) as Record<string, unknown>;
      composition = resolveComposition(loadedConfig);
      events = resolveEvents(loadedConfig);
      effectConfig = resolveEffectConfig(loadedConfig);
    }
    return {
      composition: composition!,
      events: events!,
      effectConfig: effectConfig!,
    };
  }

  for await (const request of readEmbeddedMessages(embeddedBridge)) {
    try {
      if ("displayState" in request) {
        updateOutputState(request.displayState);
      }
      if ("inputState" in request) {
        updateInputState(request.inputState);
      }
      if (hasRuntimeTimestamp(request)) {
        beginRuntimeTurn(request.nowMs);
      }
      if (statsEnabled) {
        switch (request.kind) {
          case "drainPreload":
            break;
          case "evaluate":
          case "evaluatePreview":
            stats.evaluate++;
            break;
          case "schedulerTick":
            stats.schedulerTick++;
            break;
          case "invokeHandler":
            stats.invokeHandler++;
            break;
          case "invokeKeyBinding":
            stats.invokeKeyBinding++;
            break;
          case "windowResize":
            stats.windowResize++;
            break;
          case "windowMove":
            stats.windowMove++;
            break;
          case "windowMaximizeRequest":
            stats.windowMaximizeRequest++;
            break;
          case "windowMinimizeRequest":
            stats.windowMinimizeRequest++;
            break;
          case "windowFullscreenRequest":
            stats.windowFullscreenRequest++;
            break;
          case "windowActivateRequest":
            stats.windowActivateRequest++;
            break;
          case "pointerMove":
          case "pointerMoveAsync":
            stats.pointerMoveAsync++;
            break;
          case "gestureSwipe":
          case "gestureSwipeAsync":
            stats.gestureSwipeAsync++;
            break;
          case "getEffectConfig":
            stats.getEffectConfig++;
            break;
          case "evaluateLayerEffects":
            stats.evaluateLayerEffects++;
            if (hasActiveAnimations()) stats.evaluateLayerEffectsAnim++;
            break;
          case "evaluatePopupEffects":
            stats.evaluatePopupEffects++;
            if (hasActiveAnimations()) stats.evaluatePopupEffectsAnim++;
            break;
          case "lifecycleEnable":
          case "lifecycleDisable":
          case "windowDecorationPolicy":
          case "workspaceActivate":
            break;
        }
      }
      if (request.kind === "drainPreload") {
        await writeResponse(embeddedBridge, {
          requestId: request.requestId,
          ok: true,
          kind: "drainPreload",
        });
      } else if (request.kind === "lifecycleEnable") {
        applyRuntimeEnvironment(request.environment);
        const runtimeConfig = await loadRuntimeConfig();
        debugHotReload("lifecycle-enable-before-emit", {
          reason: request.reason,
          persistedStateKeys: Object.keys(request.state ?? {}),
          cacheWindowIds: Array.from(cacheByWindowId.keys()),
          openedWindowIds: Array.from(openedWindowIds),
          firstCommittedWindowIds: Array.from(firstCommittedWindowIds),
          animationEntries: summarizeAnimationEntries(
            animationEntriesByWindowId,
          ),
        });
        runtimeConfig.events.emitEnable(request.reason, request.state);
        const keyBindingConfig = pendingKeyBindingConfigPayload();
        const pointerConfig = pendingPointerConfigPayload();
        const inputConfig = pendingInputConfigPayload();
        const eventConfig = pendingEventConfigPayload(runtimeConfig.events);
        const processConfig = pendingProcessConfigPayload();
        const processActions = pendingProcessActionsPayload();
        debugHotReload("lifecycle-enable-after-emit", {
          reason: request.reason,
          cacheWindowIds: Array.from(cacheByWindowId.keys()),
          openedWindowIds: Array.from(openedWindowIds),
          firstCommittedWindowIds: Array.from(firstCommittedWindowIds),
          animationEntries: summarizeAnimationEntries(
            animationEntriesByWindowId,
          ),
          processActions,
        });
        await writeResponse(embeddedBridge, {
          requestId: request.requestId,
          ok: true,
          kind: "lifecycleEnable",
          displayConfig: pendingDisplayConfigPayload(),
          workspaceConfig: pendingWorkspaceConfigPayload(),
          keyBindingConfig,
          pointerConfig,
          inputConfig,
          eventConfig,
          processConfig,
          processActions,
        });
      } else if (request.kind === "workspaceActivate") {
        const runtimeConfig = await loadRuntimeConfig();
        const invoked = emitWorkspaceActivate({
          workspaceId: request.workspaceId,
          groupId: request.groupId,
        });
        const mutation = invoked
          ? collectRuntimeMutationState()
          : {
              dirtyWindowIds: [],
              dirtyManagedWindowIds: undefined,
              dirtyWindowNodeIds: undefined,
              actions: [],
              nextPollInMs: peekNextPollDelay(),
            };
        const keyBindingConfig = pendingKeyBindingConfigPayload();
        const pointerConfig = pendingPointerConfigPayload();
        const inputConfig = pendingInputConfigPayload();
        const eventConfig = pendingEventConfigPayload(runtimeConfig.events);
        const processConfig = pendingProcessConfigPayload();
        const processActions = pendingProcessActionsPayload();
        await writeResponse(embeddedBridge, {
          requestId: request.requestId,
          ok: true,
          kind: "invokeHandler",
          invoked,
          dirtyWindowIds: mutation.dirtyWindowIds,
          dirtyManagedWindowIds: mutation.dirtyManagedWindowIds,
          dirtyWindowNodeIds: mutation.dirtyWindowNodeIds,
          actions: mutation.actions,
          nextPollInMs: hasActiveAnimations() ? 0 : mutation.nextPollInMs,
          displayConfig: pendingDisplayConfigPayload(),
          workspaceConfig: pendingWorkspaceConfigPayload(),
          keyBindingConfig,
          pointerConfig,
          inputConfig,
          eventConfig,
          processConfig,
          processActions,
        });
      } else if (request.kind === "lifecycleDisable") {
        const runtimeConfig = await loadRuntimeConfig();
        debugHotReload("lifecycle-disable-before-emit", {
          reason: request.reason,
          cacheWindowIds: Array.from(cacheByWindowId.keys()),
          openedWindowIds: Array.from(openedWindowIds),
          firstCommittedWindowIds: Array.from(firstCommittedWindowIds),
          animationEntries: summarizeAnimationEntries(
            animationEntriesByWindowId,
          ),
        });
        const state = runtimeConfig.events.emitDisable(request.reason);
        debugHotReload("lifecycle-disable-after-emit", {
          reason: request.reason,
          stateKeys: Object.keys(state),
          cacheWindowIds: Array.from(cacheByWindowId.keys()),
          firstCommittedWindowIds: Array.from(firstCommittedWindowIds),
          animationEntries: summarizeAnimationEntries(
            animationEntriesByWindowId,
          ),
        });
        await writeResponse(embeddedBridge, {
          requestId: request.requestId,
          ok: true,
          kind: "lifecycleDisable",
          state,
        });
      } else {
        const runtimeConfig = await loadRuntimeConfig();
        const composition = runtimeConfig.composition;
        const events = runtimeConfig.events;
        const effectConfig = runtimeConfig.effectConfig;
        if (request.kind === "evaluate" || request.kind === "evaluatePreview") {
          const result =
            request.kind === "evaluate"
              ? evaluateSnapshot(
                  composition,
                  events,
                  effectConfig,
                  request.snapshot,
                  request.nowMs,
                )
              : evaluatePreconfigure(
                  composition,
                  events,
                  effectConfig,
                  request.snapshot,
                );
          const keyBindingConfig = pendingKeyBindingConfigPayload();
          const pointerConfig = pendingPointerConfigPayload();
          const inputConfig = pendingInputConfigPayload();
          const eventConfig = pendingEventConfigPayload(events);
          const processConfig = pendingProcessConfigPayload();
          const processActions = pendingProcessActionsPayload();
          const cached =
            request.kind === "evaluate"
              ? cacheByWindowId.get(request.snapshot.id)?.cache
              : undefined;
          // Drain queued actions so they ride along with the evaluation response.
          // Layout events such as onFirstCommit can schedule animations for
          // multiple windows while only one window is being evaluated; draining
          // all render actions here keeps those animations frame-aligned instead
          // of leaking them to later dirty evaluations in window order.
          const evaluationActions =
            request.kind === "evaluate"
              ? drainPendingActions()
              : drainPendingActionsForWindow(request.snapshot.id);
          if (evaluationActions.length > 0) {
            debugHotReload("evaluate-actions", {
              kind: request.kind,
              windowId: request.snapshot.id,
              title: request.snapshot.title,
              actions: evaluationActions.map(summarizeWindowAction),
            });
          }
          embeddedBridge.writeCompositionUpdate(request.requestId, {
            kind: "full",
            windowId: request.snapshot.id,
            tree: result.serialized,
          });
          lastNativeCompositionByWindowId.set(
            request.snapshot.id,
            result.serialized,
          );
          if (cached) {
            syncCompositionShaderUniformSlots(
              embeddedBridge,
              request.snapshot.id,
              cached,
            );
          }
          await writeResponse(embeddedBridge, {
            requestId: request.requestId,
            ok: true,
            kind: request.kind,
            transform:
              cached?.lastTransform ?? result.transform ?? identityTransform(),
            managedWindow:
              cached?.lastManagedWindow ??
              result.managedWindow ??
              identityManagedWindow(),
            windowEffects: result.windowEffects,
            effectTargetId: request.snapshot.id,
            dirtyNodeIds:
              request.kind === "evaluate"
                ? cached
                  ? takeDirtyCompositionNodeIds(request.snapshot.id, cached)
                  : []
                : [],
            nextPollInMs:
              request.kind === "evaluate"
                ? hasActiveAnimations()
                  ? 0
                  : peekNextPollDelay()
                : undefined,
            actions:
              evaluationActions.length > 0 ? evaluationActions : undefined,
            displayConfig: pendingDisplayConfigPayload(),
            workspaceConfig: pendingWorkspaceConfigPayload(),
            keyBindingConfig,
            pointerConfig,
            inputConfig,
            eventConfig,
            processConfig,
            processActions,
          });
        } else if (request.kind === "windowDecorationPolicy") {
          const decision = evaluateWindowDecorationPolicy(
            composition,
            events,
            request.snapshot,
            request.context,
          );
          await writeResponse(embeddedBridge, {
            requestId: request.requestId,
            ok: true,
            kind: "windowDecorationPolicy",
            decision,
          });
        } else {
          if (request.kind === "schedulerTick") {
            const tick = processSchedulerTick(request.nowMs);
            if (statsEnabled && tick.dirty) stats.schedulerTickDirty++;
            const keyBindingConfig = pendingKeyBindingConfigPayload();
            const pointerConfig = pendingPointerConfigPayload();
            const inputConfig = pendingInputConfigPayload();
            const eventConfig = pendingEventConfigPayload(events);
            const processConfig = pendingProcessConfigPayload();
            const processActions = pendingProcessActionsPayload();
            const debugConfig = takePendingDebugConfig();
            const response: SchedulerTickSuccess = {
              requestId: request.requestId,
              ok: true,
              kind: "schedulerTick",
              dirty: tick.dirty,
              runtimeDirty: tick.runtimeDirty,
              dirtyWindowIds: tick.dirtyWindowIds,
              dirtyManagedWindowIds: tick.dirtyManagedWindowIds,
              dirtyWindowNodeIds: tick.dirtyWindowNodeIds,
              dirtyLayerIds: tick.dirtyLayerIds,
              dirtyLayerNodeIds: tick.dirtyLayerNodeIds,
              actions: tick.actions,
              nextPollInMs: hasActiveAnimations() ? 0 : tick.nextPollInMs,
              displayConfig: pendingDisplayConfigPayload(),
              workspaceConfig: pendingWorkspaceConfigPayload(),
              keyBindingConfig,
              pointerConfig,
              inputConfig,
              eventConfig,
              processConfig,
              processActions,
              debugConfig,
            };
            const runtimeUpdates = takeRuntimeResponseUpdates();
            if (
              !tryWriteNativeSchedulerResponse(
                embeddedBridge,
                response,
                runtimeUpdates,
              )
            ) {
              await writeResponseWithRuntimeUpdates(
                embeddedBridge,
                response,
                runtimeUpdates,
              );
            }
          } else if (request.kind === "windowClosed") {
            embeddedBridge.clearCompositionShaderUniformSlots(
              request.windowId,
            );
            shaderUniformSlotIdsByWindow.delete(request.windowId);
            clearNativeEffectTarget(
              embeddedBridge,
              NATIVE_EFFECT_TARGET_WINDOW,
              request.windowId,
            );
            closeWindow(events, request.windowId);
            const keyBindingConfig = pendingKeyBindingConfigPayload();
            const pointerConfig = pendingPointerConfigPayload();
            const inputConfig = pendingInputConfigPayload();
            const processConfig = pendingProcessConfigPayload();
            const processActions = pendingProcessActionsPayload();
            await writeResponse(embeddedBridge, {
              requestId: request.requestId,
              ok: true,
              kind: "windowClosed",
              displayConfig: pendingDisplayConfigPayload(),
              workspaceConfig: pendingWorkspaceConfigPayload(),
              keyBindingConfig,
              pointerConfig,
              inputConfig,
              processConfig,
              processActions,
            });
          } else if (request.kind === "startClose") {
            const result = startClose(events, effectConfig, request.windowId);
            const keyBindingConfig = pendingKeyBindingConfigPayload();
            const pointerConfig = pendingPointerConfigPayload();
            const inputConfig = pendingInputConfigPayload();
            const processConfig = pendingProcessConfigPayload();
            const processActions = pendingProcessActionsPayload();
            const response: EvaluateSuccess = {
              requestId: request.requestId,
              ok: true,
              kind: "startClose",
              ...result,
              effectTargetId: request.windowId,
              displayConfig: pendingDisplayConfigPayload(),
              workspaceConfig: pendingWorkspaceConfigPayload(),
              keyBindingConfig,
              pointerConfig,
              inputConfig,
              processConfig,
              processActions,
            };
            const runtimeUpdates = takeRuntimeResponseUpdates();
            if (
              !tryWriteNativeCachedResponse(
                embeddedBridge,
                response,
                runtimeUpdates,
                request.windowId,
              )
            ) {
              await writeResponseWithRuntimeUpdates(
                embeddedBridge,
                response,
                runtimeUpdates,
              );
            }
          } else if (request.kind === "evaluateCached") {
            const result = evaluateCached(
              composition,
              events,
              effectConfig,
              request.windowId,
              request.snapshot,
              request.forceFullReevaluation ?? false,
            );
            const keyBindingConfig = pendingKeyBindingConfigPayload();
            const pointerConfig = pendingPointerConfigPayload();
            const inputConfig = pendingInputConfigPayload();
            const processConfig = pendingProcessConfigPayload();
            const processActions = pendingProcessActionsPayload();
            // Same as evaluate: drain all queued window actions so layout-wide
            // animation requests produced by one cached event are delivered in
            // a single response.
            const cachedActions = drainPendingActions();
            if (cachedActions.length > 0) {
              debugHotReload("evaluate-cached-actions", {
                windowId: request.windowId,
                actions: cachedActions.map(summarizeWindowAction),
              });
            }
            writeCachedCompositionUpdate(
              embeddedBridge,
              request.requestId,
              request.windowId,
              result,
              cacheByWindowId.get(request.windowId)?.cache,
            );
            const response: EvaluateSuccess = {
              requestId: request.requestId,
              ok: true,
              kind: "evaluateCached",
              transform: result.transform,
              managedWindow: result.managedWindow,
              windowEffects: result.windowEffects,
              effectTargetId: request.windowId,
              dirtyNodeIds: result.dirtyNodeIds,
              managedWindowOnly: result.managedWindowOnly,
              nextPollInMs: hasWindowAnimations(request.windowId)
                ? 0
                : result.nextPollInMs,
              actions: cachedActions.length > 0 ? cachedActions : undefined,
              displayConfig: pendingDisplayConfigPayload(),
              workspaceConfig: pendingWorkspaceConfigPayload(),
              keyBindingConfig,
              pointerConfig,
              inputConfig,
              processConfig,
              processActions,
            };
            const runtimeUpdates = takeRuntimeResponseUpdates();
            if (
              !tryWriteNativeCachedResponse(
                embeddedBridge,
                response,
                runtimeUpdates,
                request.windowId,
              )
            ) {
              await writeResponseWithRuntimeUpdates(
                embeddedBridge,
                response,
                runtimeUpdates,
              );
            }
          } else if (request.kind === "getEffectConfig") {
            await writeResponse(embeddedBridge, {
              requestId: request.requestId,
              ok: true,
              kind: "getEffectConfig",
              backgroundEffect: effectConfig.background_effect,
              displayConfig: pendingDisplayConfigPayload(),
              workspaceConfig: pendingWorkspaceConfigPayload(),
            });
          } else if (request.kind === "evaluateLayerEffects") {
            const result = evaluateLayerEffects(
              events,
              effectConfig,
              request.outputName,
              request.layers,
            );
            const keyBindingConfig = pendingKeyBindingConfigPayload();
            const pointerConfig = pendingPointerConfigPayload();
            const inputConfig = pendingInputConfigPayload();
            const processConfig = pendingProcessConfigPayload();
            const processActions = pendingProcessActionsPayload();
            await writeResponse(embeddedBridge, {
              requestId: request.requestId,
              ok: true,
              kind: "evaluateLayerEffects",
              effects: result.effects,
              nextPollInMs: result.nextPollInMs,
              displayConfig: pendingDisplayConfigPayload(),
              workspaceConfig: pendingWorkspaceConfigPayload(),
              keyBindingConfig,
              pointerConfig,
              inputConfig,
              processConfig,
              processActions,
            });
          } else if (request.kind === "evaluatePopupEffects") {
            const result = evaluatePopupEffects(
              effectConfig,
              request.outputName,
              request.popups,
            );
            const keyBindingConfig = pendingKeyBindingConfigPayload();
            const pointerConfig = pendingPointerConfigPayload();
            const inputConfig = pendingInputConfigPayload();
            const eventConfig = pendingEventConfigPayload(events);
            const processConfig = pendingProcessConfigPayload();
            const processActions = pendingProcessActionsPayload();
            await writeResponse(embeddedBridge, {
              requestId: request.requestId,
              ok: true,
              kind: "evaluatePopupEffects",
              effects: result.effects,
              nextPollInMs: result.nextPollInMs,
              displayConfig: pendingDisplayConfigPayload(),
              workspaceConfig: pendingWorkspaceConfigPayload(),
              keyBindingConfig,
              pointerConfig,
              inputConfig,
              eventConfig,
              processConfig,
              processActions,
            });
          } else if (request.kind === "invokeKeyBinding") {
            const result = invokeGlobalKeyBinding(request.bindingId);
            const keyBindingConfig = pendingKeyBindingConfigPayload();
            const pointerConfig = pendingPointerConfigPayload();
            const inputConfig = pendingInputConfigPayload();
            const processConfig = pendingProcessConfigPayload();
            const processActions = pendingProcessActionsPayload();
            const debugConfig = takePendingDebugConfig();
            await writeResponse(embeddedBridge, {
              requestId: request.requestId,
              ok: true,
              kind: "invokeKeyBinding",
              ...result,
              displayConfig: pendingDisplayConfigPayload(),
              workspaceConfig: pendingWorkspaceConfigPayload(),
              keyBindingConfig,
              pointerConfig,
              inputConfig,
              processConfig,
              processActions,
              debugConfig,
            });
          } else if (request.kind === "windowResize") {
            const result = invokeWindowResize(
              events,
              request.windowId,
              request.event,
            );
            const keyBindingConfig = pendingKeyBindingConfigPayload();
            const pointerConfig = pendingPointerConfigPayload();
            const inputConfig = pendingInputConfigPayload();
            const processConfig = pendingProcessConfigPayload();
            const processActions = pendingProcessActionsPayload();
            await writeInteractionEventResponse(embeddedBridge, {
              requestId: request.requestId,
              ok: true,
              kind: "windowResize",
              ...result,
              displayConfig: pendingDisplayConfigPayload(),
              workspaceConfig: pendingWorkspaceConfigPayload(),
              keyBindingConfig,
              pointerConfig,
              inputConfig,
              processConfig,
              processActions,
            });
          } else if (request.kind === "windowMove") {
            const result = invokeWindowMove(
              events,
              request.windowId,
              request.event,
            );
            const keyBindingConfig = pendingKeyBindingConfigPayload();
            const pointerConfig = pendingPointerConfigPayload();
            const inputConfig = pendingInputConfigPayload();
            const eventConfig = pendingEventConfigPayload(events);
            const processConfig = pendingProcessConfigPayload();
            const processActions = pendingProcessActionsPayload();
            await writeInteractionEventResponse(embeddedBridge, {
              requestId: request.requestId,
              ok: true,
              kind: "windowMove",
              ...result,
              displayConfig: pendingDisplayConfigPayload(),
              workspaceConfig: pendingWorkspaceConfigPayload(),
              keyBindingConfig,
              pointerConfig,
              inputConfig,
              eventConfig,
              processConfig,
              processActions,
            });
          } else if (request.kind === "windowMaximizeRequest") {
            const result = invokeWindowMaximizeRequest(
              composition,
              events,
              request.windowId,
              request.snapshot,
              request.event,
            );
            const keyBindingConfig = pendingKeyBindingConfigPayload();
            const pointerConfig = pendingPointerConfigPayload();
            const inputConfig = pendingInputConfigPayload();
            const eventConfig = pendingEventConfigPayload(events);
            const processConfig = pendingProcessConfigPayload();
            const processActions = pendingProcessActionsPayload();
            await writeResponse(embeddedBridge, {
              requestId: request.requestId,
              ok: true,
              kind: "windowMaximizeRequest",
              ...result,
              displayConfig: pendingDisplayConfigPayload(),
              workspaceConfig: pendingWorkspaceConfigPayload(),
              keyBindingConfig,
              pointerConfig,
              inputConfig,
              eventConfig,
              processConfig,
              processActions,
            });
          } else if (request.kind === "windowMinimizeRequest") {
            const result = invokeWindowMinimizeRequest(
              composition,
              events,
              request.windowId,
              request.snapshot,
              request.event,
            );
            const keyBindingConfig = pendingKeyBindingConfigPayload();
            const pointerConfig = pendingPointerConfigPayload();
            const inputConfig = pendingInputConfigPayload();
            const eventConfig = pendingEventConfigPayload(events);
            const processConfig = pendingProcessConfigPayload();
            const processActions = pendingProcessActionsPayload();
            await writeResponse(embeddedBridge, {
              requestId: request.requestId,
              ok: true,
              kind: "windowMinimizeRequest",
              ...result,
              displayConfig: pendingDisplayConfigPayload(),
              workspaceConfig: pendingWorkspaceConfigPayload(),
              keyBindingConfig,
              pointerConfig,
              inputConfig,
              eventConfig,
              processConfig,
              processActions,
            });
          } else if (request.kind === "windowFullscreenRequest") {
            const result = invokeWindowFullscreenRequest(
              composition,
              events,
              request.windowId,
              request.snapshot,
              request.event,
            );
            const keyBindingConfig = pendingKeyBindingConfigPayload();
            const pointerConfig = pendingPointerConfigPayload();
            const inputConfig = pendingInputConfigPayload();
            const eventConfig = pendingEventConfigPayload(events);
            const processConfig = pendingProcessConfigPayload();
            const processActions = pendingProcessActionsPayload();
            await writeResponse(embeddedBridge, {
              requestId: request.requestId,
              ok: true,
              kind: "windowFullscreenRequest",
              ...result,
              displayConfig: pendingDisplayConfigPayload(),
              workspaceConfig: pendingWorkspaceConfigPayload(),
              keyBindingConfig,
              pointerConfig,
              inputConfig,
              eventConfig,
              processConfig,
              processActions,
            });
          } else if (request.kind === "windowActivateRequest") {
            const result = invokeWindowActivateRequest(
              composition,
              events,
              request.windowId,
              request.snapshot,
              request.event,
            );
            const keyBindingConfig = pendingKeyBindingConfigPayload();
            const pointerConfig = pendingPointerConfigPayload();
            const inputConfig = pendingInputConfigPayload();
            const eventConfig = pendingEventConfigPayload(events);
            const processConfig = pendingProcessConfigPayload();
            const processActions = pendingProcessActionsPayload();
            await writeResponse(embeddedBridge, {
              requestId: request.requestId,
              ok: true,
              kind: "windowActivateRequest",
              ...result,
              displayConfig: pendingDisplayConfigPayload(),
              workspaceConfig: pendingWorkspaceConfigPayload(),
              keyBindingConfig,
              pointerConfig,
              inputConfig,
              eventConfig,
              processConfig,
              processActions,
            });
          } else if (
            request.kind === "pointerMove" ||
            request.kind === "pointerMoveAsync"
          ) {
            const result =
              request.kind === "pointerMove"
                ? invokePointerMove(events, request.event)
                : await invokePointerMoveAsync(events, request.event);
            const keyBindingConfig = pendingKeyBindingConfigPayload();
            const pointerConfig = pendingPointerConfigPayload();
            const inputConfig = pendingInputConfigPayload();
            const eventConfig = pendingEventConfigPayload(events);
            const processConfig = pendingProcessConfigPayload();
            const processActions = pendingProcessActionsPayload();
            await writeInteractionEventResponse(embeddedBridge, {
              requestId: request.requestId,
              ok: true,
              kind: request.kind,
              ...result,
              displayConfig: pendingDisplayConfigPayload(),
              workspaceConfig: pendingWorkspaceConfigPayload(),
              keyBindingConfig,
              pointerConfig,
              inputConfig,
              eventConfig,
              processConfig,
              processActions,
            });
          } else if (
            request.kind === "gestureSwipe" ||
            request.kind === "gestureSwipeAsync"
          ) {
            const result =
              request.kind === "gestureSwipe"
                ? invokeGestureSwipe(events, request.event)
                : await invokeGestureSwipeAsync(events, request.event);
            const keyBindingConfig = pendingKeyBindingConfigPayload();
            const pointerConfig = pendingPointerConfigPayload();
            const inputConfig = pendingInputConfigPayload();
            const eventConfig = pendingEventConfigPayload(events);
            const processConfig = pendingProcessConfigPayload();
            const processActions = pendingProcessActionsPayload();
            await writeInteractionEventResponse(embeddedBridge, {
              requestId: request.requestId,
              ok: true,
              kind: request.kind,
              ...result,
              displayConfig: pendingDisplayConfigPayload(),
              workspaceConfig: pendingWorkspaceConfigPayload(),
              keyBindingConfig,
              pointerConfig,
              inputConfig,
              eventConfig,
              processConfig,
              processActions,
            });
          } else {
            const result = invokeHandler(
              effectConfig,
              request.windowId,
              request.handlerId,
            );
            const keyBindingConfig = pendingKeyBindingConfigPayload();
            const pointerConfig = pendingPointerConfigPayload();
            const inputConfig = pendingInputConfigPayload();
            const processConfig = pendingProcessConfigPayload();
            const processActions = pendingProcessActionsPayload();
            await writeResponse(embeddedBridge, {
              requestId: request.requestId,
              ok: true,
              kind: "invokeHandler",
              ...result,
              effectTargetId: request.windowId,
              displayConfig: pendingDisplayConfigPayload(),
              workspaceConfig: pendingWorkspaceConfigPayload(),
              keyBindingConfig,
              pointerConfig,
              inputConfig,
              processConfig,
              processActions,
            });
          }
        }
      }
    } catch (error) {
      await writeResponse(embeddedBridge, {
        requestId: request.requestId,
        ok: false,
        kind: request.kind,
        error:
          error instanceof Error
            ? (error.stack ?? error.message)
            : String(error),
        displayConfig: pendingDisplayConfigPayload(),
        workspaceConfig: pendingWorkspaceConfigPayload(),
      });
    }
  }
}

function evaluateCached(
  composition: WindowCompositionFunction,
  events: CompositorEventController,
  effectConfig: RuntimeEffectConfig,
  windowId: string,
  snapshot?: WaylandWindowSnapshot,
  forceFullReevaluation = false,
): {
  serialized?: unknown;
  treeUpdate: "full" | "patches" | "uniforms" | "none";
  uniformPatches?: NativeShaderUniformPatch[];
  transform: WindowTransform;
  managedWindow: ManagedWindowState;
  windowEffects: WindowEffectAssignment | null;
  dirtyNodeIds?: string[];
  managedWindowOnly?: boolean;
  nextPollInMs?: number;
} {
  let entry = cacheByWindowId.get(windowId);
  if (!entry) {
    if (!snapshot) {
      throw new Error(`missing cache entry for closing window ${windowId}`);
    }
    if (snapshot.id !== windowId) {
      throw new Error(
        `cached window snapshot id mismatch: ${windowId} != ${snapshot.id}`,
      );
    }
    debugHotReload("evaluate-cached-recreate-cache", {
      windowId,
      snapshot: snapshotForDebug(snapshot),
    });
    entry = createRuntimeCacheEntry(
      snapshot,
      composition,
      RENDER_COMPOSITION_CONTEXT,
    );
    cacheByWindowId.set(windowId, entry);
    openedWindowIds.add(windowId);
    dirtyWindowIds.delete(windowId);
    takeDirtyWindowNodeIds(windowId);
    takeDirtyWindowShaderUniformBindingKeys(windowId);
    takeManagedWindowOnlyDirty(windowId);
    events.emitFocus(entry.cache.window, snapshot.isFocused);
    if (!firstCommittedWindowIds.has(windowId)) {
      firstCommittedWindowIds.add(windowId);
      debugHotReload("evaluate-cached-recreate-first-commit", {
        windowId,
        snapshot: snapshotForDebug(snapshot),
      });
      events.emitFirstCommit(entry.cache.window);
    }
  }

  let updated: ReturnType<CompositionEvaluationCache["update"]> = null;
  if (snapshot !== undefined) {
    if (snapshot.id !== windowId) {
      throw new Error(
        `cached window snapshot id mismatch: ${windowId} != ${snapshot.id}`,
      );
    }
    debugSSD("runtime-evaluate-cached-update-snapshot", {
      windowId,
      snapshot: snapshotForDebug(snapshot),
    });
    entry.latestSnapshot = snapshot;
    updated = entry.cache.update(snapshot);
    debugLabel("evaluate-cached-update-snapshot", {
      windowId,
      snapshotTitle: snapshot.title,
      windowTitle: entry.cache.window.title.peek(),
      updated: updated !== null,
      labels: updated
        ? summarizeSerializedLabels(updated.serialized)
        : undefined,
    });
    // Updating reactive snapshot signals can mark this same window dirty.
    // This cached evaluation is already consuming that snapshot update, so
    // clear the outer dirty mark to avoid a duplicate follow-up tick.
    dirtyWindowIds.delete(windowId);
  }

  const managedWindowOnlyDirty = takeManagedWindowOnlyDirty(windowId);
  if (managedWindowOnlyDirty && !forceFullReevaluation) {
    const dirtyNodeIds = takeDirtyWindowNodeIds(windowId);
    takeDirtyWindowShaderUniformBindingKeys(windowId);
    if (updated) {
      debugLabel("evaluate-cached-managed-dirty-with-updated-tree", {
        windowId,
        dirtyNodeIds,
        labels: summarizeSerializedLabels(updated.serialized),
      });
      return {
        serialized: updated.serialized,
        treeUpdate: "full",
        transform: updated.transform,
        managedWindow: updated.managedWindow,
        windowEffects: evaluateWindowEffects(effectConfig, windowId, entry),
        dirtyNodeIds,
        nextPollInMs: hasWindowAnimations(windowId) ? 0 : peekNextPollDelay(),
      };
    }
    const reevaluated = entry.cache.reevaluateManagedWindow();
    return {
      treeUpdate: "none",
      transform: reevaluated.transform,
      managedWindow: reevaluated.managedWindow,
      windowEffects: evaluateWindowEffects(effectConfig, windowId, entry),
      dirtyNodeIds: [],
      managedWindowOnly: true,
      nextPollInMs: hasWindowAnimations(windowId) ? 0 : peekNextPollDelay(),
    };
  }

  const dirtyNodeIds = takeDirtyWindowNodeIds(windowId);
  const dirtyUniformBindingKeys =
    takeDirtyWindowShaderUniformBindingKeys(windowId);
  if (updated && !forceFullReevaluation) {
    debugLabel("evaluate-cached-updated-tree", {
      windowId,
      dirtyNodeIds,
      labels: summarizeSerializedLabels(updated.serialized),
    });
    return {
      serialized: updated.serialized,
      treeUpdate: "full",
      transform: updated.transform,
      managedWindow: updated.managedWindow,
      windowEffects: evaluateWindowEffects(effectConfig, windowId, entry),
      dirtyNodeIds,
      nextPollInMs: hasWindowAnimations(windowId) ? 0 : peekNextPollDelay(),
    };
  }
  if (
    !forceFullReevaluation &&
    dirtyNodeIds.length === 0 &&
    dirtyUniformBindingKeys.length > 0
  ) {
    const uniformPatches = entry.cache.readShaderUniformPatches(
      dirtyUniformBindingKeys,
    );
    if (uniformPatches !== null && uniformPatches.length > 0) {
      return {
        treeUpdate: "uniforms",
        uniformPatches,
        transform: entry.cache.lastTransform,
        managedWindow: entry.cache.lastManagedWindow,
        windowEffects: evaluateWindowEffects(effectConfig, windowId, entry),
        dirtyNodeIds: Array.from(
          new Set(uniformPatches.map((patch) => patch.nodeId)),
        ),
        nextPollInMs: hasWindowAnimations(windowId) ? 0 : peekNextPollDelay(),
      };
    }
  }
  const reevaluateNodeIds =
    dirtyUniformBindingKeys.length > 0
      ? Array.from(
          new Set([
            ...dirtyNodeIds,
            ...entry.cache.shaderUniformBindings
              .filter((binding) =>
                dirtyUniformBindingKeys.includes(binding.key)
              )
              .map((binding) => binding.nodeId),
          ]),
        )
      : dirtyNodeIds;
  // A full window dirty can coincide with node-scoped dirty marks from
  // derived signals. Passing those node ids to reevaluate() selects the
  // serialized-tree patch path, which deliberately does not recreate the
  // composition root or its ManagedWindow props. State transitions such as
  // unminimize would then leave the old idle/opacity static state behind and
  // disappear again once the visual animation completed.
  const reevaluated = forceFullReevaluation
    ? entry.cache.reevaluate()
    : entry.cache.reevaluate(reevaluateNodeIds);
  debugLabel("evaluate-cached-reevaluate", {
    windowId,
    dirtyNodeIds: reevaluateNodeIds,
    forceFullReevaluation,
    labels: summarizeSerializedLabels(reevaluated.serialized),
  });
  return {
    serialized: reevaluated.serialized,
    treeUpdate:
      !forceFullReevaluation && reevaluateNodeIds.length > 0
        ? "patches"
        : "full",
    transform: entry.cache.lastTransform,
    managedWindow: entry.cache.lastManagedWindow,
    windowEffects: evaluateWindowEffects(effectConfig, windowId, entry),
    dirtyNodeIds: forceFullReevaluation ? [] : reevaluateNodeIds,
    nextPollInMs: hasWindowAnimations(windowId) ? 0 : peekNextPollDelay(),
  };
}

function writeCachedCompositionUpdate(
  bridge: EmbeddedRuntimeBridge,
  requestId: number,
  windowId: string,
  result: ReturnType<typeof evaluateCached>,
  cache: CompositionEvaluationCache | undefined,
): void {
  if (result.treeUpdate === "none") {
    return;
  }
  if (result.treeUpdate === "uniforms") {
    const uniformPatches = result.uniformPatches ?? [];
    if (uniformPatches.length === 0) {
      throw new Error(
        `cached uniform composition update for ${windowId} has no patches`,
      );
    }
    const registeredSlotIds = uniformPatches.map((patch) =>
      patch.bindingKey === undefined
        ? undefined
        : shaderUniformSlotIdsByWindow
            .get(windowId)
            ?.get(patch.bindingKey)
    );
    const firstSlotId = registeredSlotIds[0];
    const usesOnlyRegisteredSlots =
      firstSlotId !== undefined &&
      registeredSlotIds.every((slotId) => slotId !== undefined);
    if (usesOnlyRegisteredSlots) {
      bridge.beginCompositionShaderUniformSlotPatches(
        requestId,
        firstSlotId,
      );
    } else {
      bridge.beginCompositionPatches(requestId, windowId);
    }
    for (let index = 0; index < uniformPatches.length; index++) {
      const patch = uniformPatches[index];
      const slotId = registeredSlotIds[index];
      if (patch.arrayElementWidth !== undefined) {
        const values = new Float32Array(patch.values);
        if (slotId !== undefined) {
          bridge.writeCompositionShaderUniformArraySlotPatch(
            requestId,
            slotId,
            patch.arrayElementWidth,
            values,
          );
        } else {
          bridge.writeCompositionShaderUniformArrayPatch(
            requestId,
            patch.nodeId,
            patch.stageIndex,
            patch.name,
            patch.arrayElementWidth,
            values,
          );
        }
        continue;
      }
      const [x = 0, y = 0, z = 0, w = 0] = patch.values;
      if (slotId !== undefined) {
        bridge.writeCompositionShaderUniformSlotPatch(
          requestId,
          slotId,
          patch.values.length,
          x,
          y,
          z,
          w,
        );
      } else {
        bridge.writeCompositionShaderUniformPatch(
          requestId,
          patch.nodeId,
          patch.stageIndex,
          patch.name,
          patch.values.length,
          x,
          y,
          z,
          w,
        );
      }
    }
    return;
  }
  if (result.serialized === undefined) {
    throw new Error(
      `cached composition update for ${windowId} is missing its serialized tree`,
    );
  }
  if (result.treeUpdate === "full") {
    bridge.writeCompositionUpdate(requestId, {
      kind: "full",
      windowId,
      tree: result.serialized,
    });
    lastNativeCompositionByWindowId.set(windowId, result.serialized);
    if (cache) {
      syncCompositionShaderUniformSlots(bridge, windowId, cache);
    }
    return;
  }

  const dirtyNodeIds = topLevelDirtyNodeIds(result.dirtyNodeIds ?? []);
  const previous = lastNativeCompositionByWindowId.get(windowId);
  const uniformPatches =
    previous === undefined
      ? null
      : collectShaderUniformPatches(
          previous,
          result.serialized,
          dirtyNodeIds,
          result.dirtyNodeIds ?? [],
        );
  if (uniformPatches && uniformPatches.length > 0) {
    bridge.beginCompositionPatches(requestId, windowId);
    for (const patch of uniformPatches) {
      if (patch.arrayElementWidth !== undefined) {
        bridge.writeCompositionShaderUniformArrayPatch(
          requestId,
          patch.nodeId,
          patch.stageIndex,
          patch.name,
          patch.arrayElementWidth,
          new Float32Array(patch.values),
        );
        continue;
      }
      const [x = 0, y = 0, z = 0, w = 0] = patch.values;
      bridge.writeCompositionShaderUniformPatch(
        requestId,
        patch.nodeId,
        patch.stageIndex,
        patch.name,
        patch.values.length,
        x,
        y,
        z,
        w,
      );
    }
    lastNativeCompositionByWindowId.set(windowId, result.serialized);
    return;
  }
  const patches = dirtyNodeIds.map((nodeId) => {
    const node = findSerializedNode(result.serialized, nodeId);
    if (node === undefined) {
      throw new Error(
        `dirty composition node ${nodeId} is missing from ${windowId}`,
      );
    }
    return { nodeId, node };
  });
  bridge.writeCompositionUpdate(requestId, {
    kind: "patches",
    windowId,
    patches,
  });
  lastNativeCompositionByWindowId.set(windowId, result.serialized);
  if (cache) {
    syncCompositionShaderUniformSlots(bridge, windowId, cache);
  }
}

interface NativeShaderUniformPatch {
  bindingKey?: string;
  nodeId: string;
  stageIndex: number;
  name: string;
  values: number[];
  arrayElementWidth?: number;
}

function syncCompositionShaderUniformSlots(
  bridge: EmbeddedRuntimeBridge,
  windowId: string,
  cache: CompositionEvaluationCache,
): void {
  bridge.clearCompositionShaderUniformSlots(windowId);
  const slotIds = new Map<string, number>();
  for (const binding of cache.shaderUniformBindings) {
    const slotId = nextShaderUniformSlotId++;
    if (nextShaderUniformSlotId > 0xffff_ffff) {
      nextShaderUniformSlotId = 1;
    }
    slotIds.set(binding.key, slotId);
    bridge.registerCompositionShaderUniformSlot(
      slotId,
      windowId,
      binding.nodeId,
      binding.stageIndex,
      binding.name,
    );
  }
  shaderUniformSlotIdsByWindow.set(windowId, slotIds);
}

function takeDirtyCompositionNodeIds(
  windowId: string,
  cache: CompositionEvaluationCache,
): string[] {
  const nodeIds = new Set(takeDirtyWindowNodeIds(windowId));
  const bindingKeys = new Set(
    takeDirtyWindowShaderUniformBindingKeys(windowId),
  );
  if (bindingKeys.size > 0) {
    for (const binding of cache.shaderUniformBindings) {
      if (bindingKeys.has(binding.key)) {
        nodeIds.add(binding.nodeId);
      }
    }
  }
  return Array.from(nodeIds);
}

function collectShaderUniformPatches(
  previousTree: unknown,
  nextTree: unknown,
  dirtyNodeIds: readonly string[],
  allDirtyNodeIds: readonly string[],
): NativeShaderUniformPatch[] | null {
  const patches: NativeShaderUniformPatch[] = [];
  for (const nodeId of dirtyNodeIds) {
    if (
      allDirtyNodeIds.some(
        (candidate) =>
          candidate !== nodeId &&
          (candidate.startsWith(`${nodeId}.`) ||
            candidate.startsWith(`${nodeId}[`)),
      )
    ) {
      return null;
    }
    const previous = findSerializedNode(previousTree, nodeId);
    const next = findSerializedNode(nextTree, nodeId);
    const nodePatches = collectNodeShaderUniformPatches(previous, next);
    if (nodePatches === null) {
      return null;
    }
    patches.push(...nodePatches);
  }
  return patches;
}

function collectNodeShaderUniformPatches(
  previousValue: unknown,
  nextValue: unknown,
): NativeShaderUniformPatch[] | null {
  const previous = asRecord(previousValue);
  const next = asRecord(nextValue);
  if (
    !previous ||
    !next ||
    previous.kind !== "ShaderEffect" ||
    next.kind !== "ShaderEffect" ||
    typeof previous.nodeId !== "string" ||
    previous.nodeId !== next.nodeId
  ) {
    return null;
  }
  const previousProps = asRecord(previous.props);
  const nextProps = asRecord(next.props);
  if (
    !previousProps ||
    !nextProps ||
    !recordsEqualExcept(previousProps, nextProps, "shader")
  ) {
    return null;
  }
  const previousShader = asRecord(previousProps.shader);
  const nextShader = asRecord(nextProps.shader);
  if (
    !previousShader ||
    !nextShader ||
    !recordsEqualExcept(previousShader, nextShader, "pipeline") ||
    !Array.isArray(previousShader.pipeline) ||
    !Array.isArray(nextShader.pipeline) ||
    previousShader.pipeline.length !== nextShader.pipeline.length
  ) {
    return null;
  }

  const patches: NativeShaderUniformPatch[] = [];
  for (let stageIndex = 0; stageIndex < previousShader.pipeline.length; stageIndex++) {
    const previousStage = asRecord(previousShader.pipeline[stageIndex]);
    const nextStage = asRecord(nextShader.pipeline[stageIndex]);
    if (!previousStage || !nextStage) {
      return null;
    }
    if (previousStage.kind !== "shader-stage") {
      if (!deepEqual(previousStage, nextStage)) {
        return null;
      }
      continue;
    }
    if (
      nextStage.kind !== "shader-stage" ||
      !recordsEqualExcept(previousStage, nextStage, "uniforms")
    ) {
      return null;
    }
    const previousUniforms = asRecord(previousStage.uniforms) ?? {};
    const nextUniforms = asRecord(nextStage.uniforms) ?? {};
    const previousNames = Object.keys(previousUniforms).sort();
    const nextNames = Object.keys(nextUniforms).sort();
    if (!deepEqual(previousNames, nextNames)) {
      return null;
    }
    for (const name of nextNames) {
      if (deepEqual(previousUniforms[name], nextUniforms[name])) {
        continue;
      }
      const previousSnapshot = numericUniformSnapshot(previousUniforms[name]);
      const snapshot = numericUniformSnapshot(nextUniforms[name]);
      if (!sameShaderUniformShape(previousSnapshot, snapshot) || snapshot === null) {
        return null;
      }
      patches.push({
        nodeId: previous.nodeId,
        stageIndex,
        name,
        values: snapshot.values,
        arrayElementWidth: snapshot.arrayElementWidth,
      });
    }
  }
  return patches;
}

interface NativeShaderUniformSnapshot {
  values: number[];
  arrayElementWidth?: number;
}

function numericUniformSnapshot(
  value: unknown,
): NativeShaderUniformSnapshot | null {
  if (isRecord(value) && value.kind === "uniform-array") {
    const width =
      value.element === "float"
        ? 1
        : value.element === "vec2"
          ? 2
          : value.element === "vec3"
            ? 3
            : value.element === "vec4"
              ? 4
              : 0;
    if (width === 0 || !Array.isArray(value.values) || value.values.length === 0) {
      return null;
    }
    const flattened: number[] = [];
    for (const element of value.values) {
      const entries = width === 1 ? [element] : element;
      if (
        !Array.isArray(entries) ||
        entries.length !== width ||
        entries.some(
          (entry) => typeof entry !== "number" || !Number.isFinite(entry),
        )
      ) {
        return null;
      }
      flattened.push(...(entries as number[]));
    }
    return { values: flattened, arrayElementWidth: width };
  }
  const values = typeof value === "number" ? [value] : value;
  if (
    !Array.isArray(values) ||
    values.length < 1 ||
    values.length > 4 ||
    values.some((entry) => typeof entry !== "number" || !Number.isFinite(entry))
  ) {
    return null;
  }
  return { values: values as number[] };
}

function asRecord(value: unknown): Record<string, unknown> | null {
  return typeof value === "object" && value !== null && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : null;
}

function recordsEqualExcept(
  left: Record<string, unknown>,
  right: Record<string, unknown>,
  excludedKey: string,
): boolean {
  const keys = Array.from(
    new Set([...Object.keys(left), ...Object.keys(right)]),
  ).filter((key) => key !== excludedKey);
  return keys.every((key) => deepEqual(left[key], right[key]));
}

function deepEqual(left: unknown, right: unknown): boolean {
  if (Object.is(left, right)) {
    return true;
  }
  if (Array.isArray(left) && Array.isArray(right)) {
    return (
      left.length === right.length &&
      left.every((value, index) => deepEqual(value, right[index]))
    );
  }
  const leftRecord = asRecord(left);
  const rightRecord = asRecord(right);
  if (!leftRecord || !rightRecord) {
    return false;
  }
  const leftKeys = Object.keys(leftRecord);
  const rightKeys = Object.keys(rightRecord);
  return (
    leftKeys.length === rightKeys.length &&
    leftKeys.every(
      (key) =>
        Object.prototype.hasOwnProperty.call(rightRecord, key) &&
        deepEqual(leftRecord[key], rightRecord[key]),
    )
  );
}

function topLevelDirtyNodeIds(nodeIds: readonly string[]): string[] {
  const sorted = Array.from(new Set(nodeIds)).sort(
    (left, right) => left.length - right.length,
  );
  const selected: string[] = [];
  for (const nodeId of sorted) {
    if (
      selected.some(
        (ancestor) =>
          nodeId === ancestor || nodeId.startsWith(`${ancestor}.`),
      )
    ) {
      continue;
    }
    selected.push(nodeId);
  }
  return selected;
}

function findSerializedNode(tree: unknown, nodeId: string): unknown {
  if (!tree || typeof tree !== "object") {
    return undefined;
  }
  const node = tree as { nodeId?: unknown; children?: unknown[] };
  if (node.nodeId === nodeId) {
    return tree;
  }
  for (const child of node.children ?? []) {
    const found = findSerializedNode(child, nodeId);
    if (found !== undefined) {
      return found;
    }
  }
  return undefined;
}

function summarizeSerializedLabels(node: unknown): unknown[] {
  const labels: unknown[] = [];
  collectSerializedLabels(node, labels);
  return labels;
}

function collectSerializedLabels(node: unknown, labels: unknown[]): void {
  if (!node || typeof node !== "object") {
    return;
  }
  const record = node as {
    kind?: unknown;
    nodeId?: unknown;
    props?: Record<string, unknown>;
    children?: unknown[];
  };
  if (record.kind === "Label") {
    labels.push({
      nodeId: record.nodeId,
      text: record.props?.text,
      style: record.props?.style,
    });
  }
  for (const child of record.children ?? []) {
    collectSerializedLabels(child, labels);
  }
}

function evaluateSnapshot(
  composition: WindowCompositionFunction,
  events: CompositorEventController,
  effectConfig: RuntimeEffectConfig,
  snapshot: WaylandWindowSnapshot,
  nowMs: number,
): {
  serialized: unknown;
  transform?: WindowTransform;
  managedWindow?: ManagedWindowState;
  windowEffects: WindowEffectAssignment | null;
} {
  const existing = cacheByWindowId.get(snapshot.id);
  if (!existing) {
    debugSSD("runtime-evaluate-new-cache", {
      nowMs,
      snapshot: snapshotForDebug(snapshot),
    });
    const entry = createRuntimeCacheEntry(
      snapshot,
      composition,
      RENDER_COMPOSITION_CONTEXT,
    );
    cacheByWindowId.set(snapshot.id, entry);
    if (!openedWindowIds.has(snapshot.id)) {
      openedWindowIds.add(snapshot.id);
      debugSSD("runtime-emit-open", {
        windowId: snapshot.id,
        phase: "evaluate-new-cache",
      });
      events.emitOpen(entry.cache.window);
    }
    events.emitFocus(entry.cache.window, snapshot.isFocused);
    if (!firstCommittedWindowIds.has(snapshot.id)) {
      firstCommittedWindowIds.add(snapshot.id);
      debugSSD("runtime-emit-first-commit", {
        windowId: snapshot.id,
        phase: "evaluate-new-cache",
      });
      events.emitFirstCommit(entry.cache.window);
    }
    dirtyWindowIds.delete(snapshot.id);
    const dirtyNodeIds = takeDirtyCompositionNodeIds(
      snapshot.id,
      entry.cache,
    );
    debugSSD("runtime-evaluate-new-cache-reevaluate", {
      windowId: snapshot.id,
      dirtyNodeIds,
    });
    return {
      serialized: entry.cache.reevaluate(dirtyNodeIds).serialized,
      windowEffects: evaluateWindowEffects(effectConfig, snapshot.id, entry),
    };
  }

  const wasPreconfigured = existing.preconfigured;
  if (wasPreconfigured) {
    debugSSD("runtime-evaluate-preconfigured-to-render", {
      nowMs,
      snapshot: snapshotForDebug(snapshot),
    });
    existing.preconfigured = false;
    reanchorAnimationEntries(existing.animationEntries, nowMs);
    dirtyWindowIds.add(snapshot.id);
  }
  existing.cache.setContext(RENDER_COMPOSITION_CONTEXT);

  const focusChanged = existing.latestSnapshot.isFocused !== snapshot.isFocused;
  existing.latestSnapshot = snapshot;
  const updated = existing.cache.update(snapshot);
  if (focusChanged) {
    events.emitFocus(existing.cache.window, snapshot.isFocused);
  }
  if (!firstCommittedWindowIds.has(snapshot.id)) {
    firstCommittedWindowIds.add(snapshot.id);
    debugSSD("runtime-emit-first-commit", {
      windowId: snapshot.id,
      phase: "evaluate-existing",
      wasPreconfigured,
    });
    events.emitFirstCommit(existing.cache.window);
    dirtyWindowIds.add(snapshot.id);
  }

  const wasDirty = dirtyWindowIds.delete(snapshot.id);
  if (wasDirty) {
    const dirtyNodeIds = takeDirtyCompositionNodeIds(
      snapshot.id,
      existing.cache,
    );
    debugSSD("runtime-evaluate-existing-dirty", {
      windowId: snapshot.id,
      wasPreconfigured,
      dirtyNodeIds,
    });
    return {
      serialized: existing.cache.reevaluate(dirtyNodeIds).serialized,
      windowEffects: evaluateWindowEffects(effectConfig, snapshot.id, existing),
    };
  }

  debugSSD("runtime-evaluate-existing-clean", {
    windowId: snapshot.id,
    wasPreconfigured,
    updated: updated !== undefined,
  });
  return {
    serialized: updated?.serialized ?? existing.cache.lastSerialized,
    windowEffects: evaluateWindowEffects(effectConfig, snapshot.id, existing),
  };
}

function evaluatePreconfigure(
  composition: WindowCompositionFunction,
  events: CompositorEventController,
  effectConfig: RuntimeEffectConfig,
  snapshot: WaylandWindowSnapshot,
): {
  serialized: unknown;
  transform: WindowTransform;
  managedWindow: ManagedWindowState;
  windowEffects: WindowEffectAssignment | null;
} {
  // Preconfigure evaluation is used before the client has committed its first real frame so
  // Rust can send an initial configure matching <ManagedWindow rect>. It intentionally goes
  // through the normal cache/onOpen path so user window state initialized in onOpen is visible
  // to the layout. The first real evaluate reanchors any animations started here to its own
  // compositor timestamp, preventing open animations from appearing halfway through.
  let entry = cacheByWindowId.get(snapshot.id);
  if (!entry) {
    debugSSD("runtime-preconfigure-new-cache", {
      snapshot: snapshotForDebug(snapshot),
    });
    entry = createRuntimeCacheEntry(
      snapshot,
      composition,
      PRECONFIGURE_COMPOSITION_CONTEXT,
    );
    cacheByWindowId.set(snapshot.id, entry);
    if (!openedWindowIds.has(snapshot.id)) {
      openedWindowIds.add(snapshot.id);
      debugSSD("runtime-emit-open", {
        windowId: snapshot.id,
        phase: "preconfigure-new-cache",
      });
      events.emitOpen(entry.cache.window);
    }
    if (!initialConfiguredWindowIds.has(snapshot.id)) {
      initialConfiguredWindowIds.add(snapshot.id);
      debugSSD("runtime-emit-initial-configure", {
        windowId: snapshot.id,
        phase: "preconfigure-new-cache",
      });
      events.emitInitialConfigure(entry.cache.window);
    }
    events.emitFocus(entry.cache.window, snapshot.isFocused);
    const dirtyNodeIds = takeDirtyCompositionNodeIds(
      snapshot.id,
      entry.cache,
    );
    debugSSD("runtime-preconfigure-reevaluate", {
      windowId: snapshot.id,
      dirtyNodeIds,
      phase: "new-cache",
    });
    entry.cache.reevaluate(dirtyNodeIds);
  } else {
    debugSSD("runtime-preconfigure-existing-cache", {
      snapshot: snapshotForDebug(snapshot),
    });
    entry.cache.setContext(PRECONFIGURE_COMPOSITION_CONTEXT);
    const focusChanged = entry.latestSnapshot.isFocused !== snapshot.isFocused;
    entry.latestSnapshot = snapshot;
    entry.cache.update(snapshot);
    if (focusChanged) {
      events.emitFocus(entry.cache.window, snapshot.isFocused);
    }
    if (!initialConfiguredWindowIds.has(snapshot.id)) {
      initialConfiguredWindowIds.add(snapshot.id);
      debugSSD("runtime-emit-initial-configure", {
        windowId: snapshot.id,
        phase: "preconfigure-existing-cache",
      });
      events.emitInitialConfigure(entry.cache.window);
    }
    const dirtyNodeIds = takeDirtyCompositionNodeIds(
      snapshot.id,
      entry.cache,
    );
    debugSSD("runtime-preconfigure-reevaluate", {
      windowId: snapshot.id,
      dirtyNodeIds,
      phase: "existing-cache",
    });
    entry.cache.reevaluate(dirtyNodeIds);
  }

  entry.preconfigured = true;
  debugSSD("runtime-preconfigure-result", {
    windowId: snapshot.id,
    managedWindow: entry.cache.lastManagedWindow,
  });
  return {
    serialized: entry.cache.lastSerialized,
    transform: entry.cache.lastTransform,
    managedWindow: entry.cache.lastManagedWindow,
    windowEffects: evaluateWindowEffects(effectConfig, snapshot.id, entry),
  };
}

function evaluateWindowDecorationPolicy(
  _composition: WindowCompositionFunction,
  _events: CompositorEventController,
  snapshot: WaylandWindowSnapshot,
  context: WindowDecorationContext,
): WindowDecorationDecision {
  let policyEntry = decorationPolicyWindowById.get(snapshot.id);
  if (!policyEntry) {
    const snapshotRef = { current: snapshot };
    const unsupportedAction = () => {
      throw new Error(
        "window actions cannot be called from COMPOSITOR.window.decoration.configure",
      );
    };
    const handle = createReactiveWindow(snapshot, {
      close: unsupportedAction,
      maximize: unsupportedAction,
      unmaximize: unsupportedAction,
      minimize: unsupportedAction,
      fullscreen: unsupportedAction,
      unfullscreen: unsupportedAction,
      focus: unsupportedAction,
      scheduleAnimation: unsupportedAction,
      cancelAnimation: unsupportedAction,
      setCloseAnimationDuration: unsupportedAction,
      isXWayland: () => snapshotRef.current.isXwayland,
    });
    policyEntry = { snapshotRef, handle };
    decorationPolicyWindowById.set(snapshot.id, policyEntry);
  } else {
    policyEntry.snapshotRef.current = snapshot;
    policyEntry.handle.update(snapshot);
  }

  return resolveWindowDecorationDecision(policyEntry.handle.window, context);
}

function evaluateWindowEffects(
  effectConfig: RuntimeEffectConfig,
  windowId: string,
  entry: RuntimeCacheEntry,
): WindowEffectAssignment | null {
  const evaluate = effectConfig.window;
  if (!evaluate) {
    return null;
  }

  enterWindowEffectDependencyScope(windowId);
  try {
    return resolveSignals(
      evaluate(entry.cache.window),
    ) as WindowEffectAssignment | null;
  } finally {
    leaveWindowEffectDependencyScope();
  }
}

function reanchorAnimationEntries(
  entries: Map<symbol, unknown>,
  nowMs: number,
): void {
  for (const rawEntry of entries.values()) {
    const entry = rawEntry as {
      progress?: { value: number };
      timeline?: { startedAtMs: number; from: number };
    };
    if (!entry.timeline || !entry.progress) {
      continue;
    }
    entry.timeline.startedAtMs = nowMs;
    entry.progress.value = entry.timeline.from;
  }
}

function serializeManagedWindowAnimation(
  options: ManagedWindowScheduleAnimationOptions,
): RuntimeManagedWindowAnimation {
  return {
    channel: options.channel ?? "default",
    rect: options.rect
      ? {
          from: options.rect.from
            ? snapshotManagedWindowRectOption(options.rect.from)
            : undefined,
          to: snapshotManagedWindowRectOption(options.rect.to),
          duration: Math.max(1, Math.floor(options.rect.duration)),
          easing: serializeManagedWindowEasing(options.rect.easing),
          mode: options.rect.mode ?? "override",
        }
      : undefined,
    offset: options.offset
      ? {
          from: options.offset.from
            ? snapshotManagedWindowPointOption(options.offset.from)
            : undefined,
          to: snapshotManagedWindowPointOption(options.offset.to),
          duration: Math.max(1, Math.floor(options.offset.duration)),
          easing: serializeManagedWindowEasing(options.offset.easing),
          mode: options.offset.mode ?? "add",
        }
      : undefined,
    opacity: options.opacity
      ? {
          from:
            options.opacity.from === undefined
              ? undefined
              : read(options.opacity.from),
          to: read(options.opacity.to),
          duration: Math.max(1, Math.floor(options.opacity.duration)),
          easing: serializeManagedWindowEasing(options.opacity.easing),
          mode: options.opacity.mode ?? "multiply",
        }
      : undefined,
  };
}

function snapshotManagedWindowRectOption(
  rect: ManagedWindowRect,
): RuntimeManagedWindowRect {
  return {
    x: read(rect.x),
    y: read(rect.y),
    width: read(rect.width),
    height: read(rect.height),
  };
}

function snapshotManagedWindowPointOption(
  point: ManagedWindowPoint,
): RuntimeManagedWindowPoint {
  return {
    x: read(point.x),
    y: read(point.y),
  };
}

function serializeManagedWindowEasing(
  easing: ManagedWindowAnimationEasing | undefined,
): RuntimeManagedWindowAnimationEasing {
  if (!easing || easing === "linear") {
    return { kind: "linear" };
  }
  if (typeof easing === "function") {
    const bezier = (
      easing as {
        __shojiCubicBezier?: readonly [number, number, number, number];
      }
    ).__shojiCubicBezier;
    if (bezier) {
      const [x1, y1, x2, y2] = bezier;
      return { kind: "cubicBezier", x1, y1, x2, y2 };
    }
    console.warn(
      "window.scheduleAnimation received a non-serializable easing; using linear",
    );
    return { kind: "linear" };
  }
  if (easing.kind === "cubicBezier") {
    return easing;
  }
  return { kind: "linear" };
}

function createRuntimeCacheEntry(
  snapshot: WaylandWindowSnapshot,
  composition: WindowCompositionFunction,
  context: WindowCompositionContext = RENDER_COMPOSITION_CONTEXT,
): RuntimeCacheEntry {
  let latestSnapshot = snapshot;
  const actions: WaylandWindowActions = {
    close() {
      entry.pendingActions.push({
        windowId: latestSnapshot.id,
        action: "close",
      });
    },
    maximize() {
      entry.pendingActions.push({
        windowId: latestSnapshot.id,
        action: "maximize",
      });
    },
    unmaximize() {
      entry.pendingActions.push({
        windowId: latestSnapshot.id,
        action: "unmaximize",
      });
    },
    minimize() {
      entry.pendingActions.push({
        windowId: latestSnapshot.id,
        action: "minimize",
      });
    },
    fullscreen() {
      entry.pendingActions.push({
        windowId: latestSnapshot.id,
        action: "fullscreen",
      });
    },
    unfullscreen() {
      entry.pendingActions.push({
        windowId: latestSnapshot.id,
        action: "unfullscreen",
      });
    },
    focus() {
      entry.pendingActions.push({
        windowId: latestSnapshot.id,
        action: "focus",
      });
    },
    scheduleAnimation(options) {
      const action = {
        windowId: latestSnapshot.id,
        action: "scheduleAnimation",
        animation: serializeManagedWindowAnimation(options),
      } satisfies RuntimeWindowAction;
      debugHotReload("queue-schedule-animation", {
        windowId: latestSnapshot.id,
        title: latestSnapshot.title,
        action: summarizeWindowAction(action),
      });
      entry.pendingActions.push(action);
    },
    cancelAnimation(channel) {
      const action = {
        windowId: latestSnapshot.id,
        action: "cancelAnimation",
        channel,
      } satisfies RuntimeWindowAction;
      debugHotReload("queue-cancel-animation", {
        windowId: latestSnapshot.id,
        title: latestSnapshot.title,
        action: summarizeWindowAction(action),
      });
      entry.pendingActions.push(action);
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
  const cache = createCompositionEvaluationCache(
    snapshot,
    actions,
    composition,
    animation,
    context,
  );
  const entry: RuntimeCacheEntry = {
    latestSnapshot,
    cache,
    animationEntries,
    pendingActions: [],
    closeAnimationDurationMs: 0,
    closeStarted: false,
    preconfigured: false,
  };
  return entry;
}

function ensureRuntimeCacheEntry(
  composition: WindowCompositionFunction,
  events: CompositorEventController,
  snapshot: WaylandWindowSnapshot,
): RuntimeCacheEntry {
  let entry = cacheByWindowId.get(snapshot.id);
  if (!entry) {
    entry = createRuntimeCacheEntry(
      snapshot,
      composition,
      RENDER_COMPOSITION_CONTEXT,
    );
    cacheByWindowId.set(snapshot.id, entry);
    if (!openedWindowIds.has(snapshot.id)) {
      openedWindowIds.add(snapshot.id);
      events.emitOpen(entry.cache.window);
    }
    events.emitFocus(entry.cache.window, snapshot.isFocused);
    dirtyWindowIds.delete(snapshot.id);
    return entry;
  }

  // Window state requests can arrive before the first real client commit. For example Discord
  // sends unmaximize / activation requests while restoring from the tray. These requests must not
  // consume the first-commit lifecycle or switch a preconfigure cache into render mode; otherwise
  // config code that initializes rects in onFirstCommit observes the tiny pre-commit geometry and
  // never gets a chance to replace it with the natural first-buffer size.
  const focusChanged = entry.latestSnapshot.isFocused !== snapshot.isFocused;
  entry.latestSnapshot = snapshot;
  entry.cache.update(snapshot);
  if (focusChanged) {
    events.emitFocus(entry.cache.window, snapshot.isFocused);
  }
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
  events: CompositorEventController,
  effectConfig: RuntimeEffectConfig,
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
      effects: evaluateLayerEffect(effectConfig, entry.layer),
    });
  }

  return {
    effects,
    nextPollInMs: snapshots.some(
      (snapshot) =>
        snapshot.outputName === outputName &&
        hasActiveAnimationsInStore(
          animationEntriesByLayerId.get(snapshot.id) ?? new Map(),
        ),
    )
      ? 0
      : peekNextPollDelay(),
  };
}

function syncLayerSnapshots(
  events: CompositorEventController,
  snapshots: WaylandLayerSnapshot[],
): void {
  updateLayerSnapshots(snapshots);
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
    const usableAreaChanged = layerUsableAreaChanged(existing.layer, snapshot);
    existing.update(snapshot);
    if (usableAreaChanged) {
      events.emitUpdateLayer(existing.layer);
    }
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

function layerUsableAreaChanged(
  layer: WaylandLayer,
  snapshot: WaylandLayerSnapshot,
): boolean {
  const currentExclusiveZone = read(layer.exclusiveZone);
  const currentAnchor = read(layer.anchor);
  const nextExclusiveZone = snapshot.exclusiveZone;
  const nextAnchor = snapshot.anchor;

  return (
    read(layer.outputName) !== snapshot.outputName ||
    read(layer.exclusiveEdge) !== snapshot.exclusiveEdge ||
    currentExclusiveZone.mode !== nextExclusiveZone.mode ||
    currentExclusiveZone.size !== nextExclusiveZone.size ||
    currentAnchor.top !== nextAnchor.top ||
    currentAnchor.right !== nextAnchor.right ||
    currentAnchor.bottom !== nextAnchor.bottom ||
    currentAnchor.left !== nextAnchor.left
  );
}

function evaluateLayerEffect(
  effectConfig: RuntimeEffectConfig,
  layer: WaylandLayer,
): LayerEffectAssignment | null {
  const evaluate = effectConfig.layer;
  if (!evaluate) {
    return null;
  }

  enterLayerDependencyScope(layer.id);
  try {
    return resolveSignals(evaluate(layer)) as LayerEffectAssignment | null;
  } finally {
    leaveLayerDependencyScope();
  }
}

function evaluatePopupEffects(
  effectConfig: RuntimeEffectConfig,
  outputName: string,
  popups: WaylandPopup[],
): {
  effects: RuntimePopupEffectAssignment[];
  nextPollInMs?: number;
} {
  const evaluate = effectConfig.popup;
  // Surface policies ride along with the popup-effect evaluation: same
  // trigger (surface set changed / hot reload), same cache lifetime. Signals
  // are resolved once here, not subscribed.
  const evaluatePolicy = COMPOSITOR.rendering?.surfacePolicy;
  return {
    effects: popups
      .filter((popup) => popup.outputName === outputName)
      .map((popup) => ({
        popupId: popup.id,
        effects: evaluate
          ? (resolveSignals(evaluate(popup)) as PopupEffectAssignment | null)
          : null,
        surfacePolicy: evaluatePolicy
          ? (resolveSignals(
              evaluatePolicy({ kind: "popup", ...popup }),
            ) as SurfacePolicy | null)
          : null,
      })),
    // Popup effects do not currently expose an animation controller. A
    // window/layer animation therefore cannot make this output's popup cache
    // continuously animating.
    nextPollInMs: peekNextPollDelay(),
  };
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
    for (const [key, entry] of Object.entries(
      value as Record<string, unknown>,
    )) {
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

function identityManagedWindow(): ManagedWindowState {
  return {
    managed: false,
    visible: true,
    idle: false,
    interactive: true,
    forceRectSize: false,
    zIndex: 0,
    transform: identityTransform(),
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

function ensureImmediateDirtyPoll(): void {
  if (hasActiveAnimations()) {
    return;
  }
  if (immediateDirtyPoll && !immediateDirtyPoll.cancelled) {
    return;
  }
  immediateDirtyPoll = registerPoll(
    1,
    (handle) => {
      handle.cancel();
      immediateDirtyPoll = null;
    },
    "none",
  );
  debugSSD("runtime-immediate-dirty-poll-scheduled");
}

function processSchedulerTick(nowMs: number): {
  dirty: boolean;
  runtimeDirty?: boolean;
  dirtyWindowIds: string[];
  dirtyManagedWindowIds?: string[];
  dirtyWindowNodeIds?: Record<string, string[]>;
  dirtyLayerNodeIds?: Record<string, string[]>;
  actions: RuntimeWindowAction[];
  nextPollInMs?: number;
} {
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

  return collectRuntimeMutationState();
}

function collectRuntimeMutationState(): {
  dirty: boolean;
  runtimeDirty?: boolean;
  dirtyWindowIds: string[];
  dirtyManagedWindowIds?: string[];
  dirtyWindowNodeIds?: Record<string, string[]>;
  dirtyLayerIds?: string[];
  dirtyLayerNodeIds?: Record<string, string[]>;
  actions: RuntimeWindowAction[];
  nextPollInMs?: number;
} {
  let nextPollInMs: number | undefined;
  for (const poll of polls.values()) {
    if (poll.handle.cancelled) {
      continue;
    }
    const delay = Math.max(1, poll.nextRunAtMs - currentSchedulerTimeMs);
    nextPollInMs =
      nextPollInMs === undefined ? delay : Math.min(nextPollInMs, delay);
  }

  const fullDirtyWindowIds = Array.from(dirtyWindowIds);
  const fullDirtyWindowIdSet = new Set(fullDirtyWindowIds);
  const managedOnlyWindowIds = managedWindowOnlyDirtyIds().filter(
    (windowId) => !fullDirtyWindowIdSet.has(windowId),
  );
  const nextDirtyWindowIds = Array.from(
    new Set([...fullDirtyWindowIds, ...managedOnlyWindowIds]),
  );
  dirtyWindowIds.clear();
  const managedOnlyFastPathInvalidated =
    consumeManagedWindowOnlyFastPathInvalidated();
  if (managedOnlyFastPathInvalidated) {
    for (const windowId of nextDirtyWindowIds) {
      takeManagedWindowOnlyDirty(windowId);
    }
  }
  const nextDirtyLayerIds = Array.from(dirtyLayerIds);
  dirtyLayerIds.clear();
  for (const windowId of fullDirtyWindowIds) {
    takeManagedWindowOnlyDirty(windowId);
  }
  const dirtyManagedWindowIds = managedOnlyFastPathInvalidated
    ? []
    : managedOnlyWindowIds.filter((windowId) =>
        isManagedWindowOnlyDirty(windowId),
      );
  const dirtyWindowNodeIds = Object.fromEntries(
    nextDirtyWindowIds
      .map((windowId) => [windowId, peekDirtyWindowNodeIds(windowId)] as const)
      .filter(([, nodeIds]) => nodeIds.length > 0),
  );
  const dirtyLayerNodeIds = Object.fromEntries(
    nextDirtyLayerIds
      .map((layerId) => [layerId, takeDirtyLayerNodeIds(layerId)] as const)
      .filter(([, nodeIds]) => nodeIds.length > 0),
  );
  const actions = drainPendingActions();
  if (actions.length > 0) {
    debugHotReload("collect-runtime-actions", {
      actions: actions.map(summarizeWindowAction),
    });
  }
  const nextRuntimeDirty = runtimeDirty;
  const dirty =
    nextRuntimeDirty ||
    nextDirtyWindowIds.length > 0 ||
    nextDirtyLayerIds.length > 0;
  runtimeDirty = false;
  if (dirty) {
    debugSSD("runtime-collect-dirty", {
      dirtyWindowIds: nextDirtyWindowIds,
      dirtyLayerIds: nextDirtyLayerIds,
      dirtyManagedWindowIds,
      dirtyWindowNodeIds,
      dirtyLayerNodeIds,
      actions: actions.map((action) => ({
        windowId: action.windowId,
        action: action.action,
      })),
      nextPollInMs,
    });
  }

  return {
    dirty,
    runtimeDirty: nextRuntimeDirty || undefined,
    dirtyWindowIds: nextDirtyWindowIds,
    dirtyManagedWindowIds:
      dirtyManagedWindowIds.length > 0 ? dirtyManagedWindowIds : undefined,
    dirtyWindowNodeIds:
      Object.keys(dirtyWindowNodeIds).length > 0
        ? dirtyWindowNodeIds
        : undefined,
    dirtyLayerIds:
      nextDirtyLayerIds.length > 0 ? nextDirtyLayerIds : undefined,
    dirtyLayerNodeIds:
      Object.keys(dirtyLayerNodeIds).length > 0 ? dirtyLayerNodeIds : undefined,
    actions,
    nextPollInMs,
  };
}

function hasWindowAnimations(windowId: string): boolean {
  return hasActiveAnimationsInStore(
    animationEntriesByWindowId.get(windowId) ?? new Map(),
  );
}

function invokeGlobalKeyBinding(
  bindingId: string,
): Omit<InvokeKeyBindingSuccess, "requestId" | "ok" | "kind"> {
  const invoked = invokeKeyBinding(bindingId);
  if (!invoked) {
    return {
      invoked: false,
      dirty: false,
      dirtyWindowIds: [],
      actions: [],
      nextPollInMs: hasActiveAnimations() ? 0 : peekNextPollDelay(),
    };
  }

  const result = collectRuntimeMutationState();
  return {
    invoked: true,
    dirty: result.dirty,
    dirtyWindowIds: result.dirtyWindowIds,
    dirtyManagedWindowIds: result.dirtyManagedWindowIds,
    dirtyWindowNodeIds: result.dirtyWindowNodeIds,
    dirtyLayerNodeIds: result.dirtyLayerNodeIds,
    actions: result.actions,
    nextPollInMs: hasActiveAnimations() ? 0 : result.nextPollInMs,
  };
}

function invokeWindowResize(
  events: CompositorEventController,
  windowId: string,
  event: RuntimeWindowResizeEvent,
): Omit<WindowResizeSuccess, "requestId" | "ok" | "kind"> {
  const entry = cacheByWindowId.get(windowId);
  if (!entry) {
    return {
      invoked: false,
      dirty: false,
      dirtyWindowIds: [],
      actions: [],
      nextPollInMs: hasActiveAnimations() ? 0 : peekNextPollDelay(),
    };
  }

  const invoked = events.emitWindowResize(entry.cache.window, event);
  if (!invoked) {
    return {
      invoked: false,
      dirty: false,
      dirtyWindowIds: [],
      actions: [],
      nextPollInMs: hasActiveAnimations() ? 0 : peekNextPollDelay(),
    };
  }

  const result = collectRuntimeMutationState();
  return {
    invoked: true,
    dirty: result.dirty,
    dirtyWindowIds: result.dirtyWindowIds,
    dirtyManagedWindowIds: result.dirtyManagedWindowIds,
    dirtyWindowNodeIds: result.dirtyWindowNodeIds,
    dirtyLayerNodeIds: result.dirtyLayerNodeIds,
    actions: result.actions,
    nextPollInMs: hasActiveAnimations() ? 0 : result.nextPollInMs,
  };
}

function invokeWindowMove(
  events: CompositorEventController,
  windowId: string,
  event: RuntimeWindowMoveEvent,
): Omit<WindowMoveSuccess, "requestId" | "ok" | "kind"> {
  const entry = cacheByWindowId.get(windowId);
  if (!entry) {
    return {
      invoked: false,
      dirty: false,
      dirtyWindowIds: [],
      actions: [],
      nextPollInMs: hasActiveAnimations() ? 0 : peekNextPollDelay(),
    };
  }

  const invoked = events.emitWindowMove(entry.cache.window, event);
  if (!invoked) {
    return {
      invoked: false,
      dirty: false,
      dirtyWindowIds: [],
      actions: [],
      nextPollInMs: hasActiveAnimations() ? 0 : peekNextPollDelay(),
    };
  }

  const result = collectRuntimeMutationState();
  return {
    invoked: true,
    dirty: result.dirty,
    dirtyWindowIds: result.dirtyWindowIds,
    dirtyManagedWindowIds: result.dirtyManagedWindowIds,
    dirtyWindowNodeIds: result.dirtyWindowNodeIds,
    dirtyLayerNodeIds: result.dirtyLayerNodeIds,
    actions: result.actions,
    nextPollInMs: hasActiveAnimations() ? 0 : result.nextPollInMs,
  };
}

function invokeWindowMaximizeRequest(
  composition: WindowCompositionFunction,
  events: CompositorEventController,
  windowId: string,
  snapshot: WaylandWindowSnapshot,
  event: RuntimeWindowMaximizeRequestEvent,
): Omit<WindowStateRequestSuccess, "requestId" | "ok" | "kind"> {
  if (snapshot.id !== windowId) {
    return emptyWindowStateRequestResult();
  }
  const entry = ensureRuntimeCacheEntry(composition, events, snapshot);

  const invoked = events.emitWindowMaximizeRequest(entry.cache.window, event);
  if (!invoked) {
    return emptyWindowStateRequestResult();
  }

  const result = collectRuntimeMutationState();
  return {
    invoked: true,
    dirty: result.dirty,
    dirtyWindowIds: result.dirtyWindowIds,
    dirtyManagedWindowIds: result.dirtyManagedWindowIds,
    dirtyWindowNodeIds: result.dirtyWindowNodeIds,
    dirtyLayerNodeIds: result.dirtyLayerNodeIds,
    actions: result.actions,
    nextPollInMs: hasActiveAnimations() ? 0 : result.nextPollInMs,
  };
}

function invokeWindowMinimizeRequest(
  composition: WindowCompositionFunction,
  events: CompositorEventController,
  windowId: string,
  snapshot: WaylandWindowSnapshot,
  event: RuntimeWindowMinimizeRequestEvent,
): Omit<WindowStateRequestSuccess, "requestId" | "ok" | "kind"> {
  if (snapshot.id !== windowId) {
    return emptyWindowStateRequestResult();
  }
  const entry = ensureRuntimeCacheEntry(composition, events, snapshot);

  const invoked = events.emitWindowMinimizeRequest(entry.cache.window, event);
  if (!invoked) {
    return emptyWindowStateRequestResult();
  }

  const result = collectRuntimeMutationState();
  return {
    invoked: true,
    dirty: result.dirty,
    dirtyWindowIds: result.dirtyWindowIds,
    dirtyManagedWindowIds: result.dirtyManagedWindowIds,
    dirtyWindowNodeIds: result.dirtyWindowNodeIds,
    dirtyLayerNodeIds: result.dirtyLayerNodeIds,
    actions: result.actions,
    nextPollInMs: hasActiveAnimations() ? 0 : result.nextPollInMs,
  };
}

function invokeWindowFullscreenRequest(
  composition: WindowCompositionFunction,
  events: CompositorEventController,
  windowId: string,
  snapshot: WaylandWindowSnapshot,
  event: RuntimeWindowFullscreenRequestEvent,
): Omit<WindowStateRequestSuccess, "requestId" | "ok" | "kind"> {
  if (snapshot.id !== windowId) {
    return emptyWindowStateRequestResult();
  }
  const entry = ensureRuntimeCacheEntry(composition, events, snapshot);

  const invoked = events.emitWindowFullscreenRequest(entry.cache.window, event);
  if (!invoked) {
    return emptyWindowStateRequestResult();
  }

  const result = collectRuntimeMutationState();
  return {
    invoked: true,
    dirty: result.dirty,
    dirtyWindowIds: result.dirtyWindowIds,
    dirtyManagedWindowIds: result.dirtyManagedWindowIds,
    dirtyWindowNodeIds: result.dirtyWindowNodeIds,
    dirtyLayerNodeIds: result.dirtyLayerNodeIds,
    actions: result.actions,
    nextPollInMs: hasActiveAnimations() ? 0 : result.nextPollInMs,
  };
}

function invokeWindowActivateRequest(
  composition: WindowCompositionFunction,
  events: CompositorEventController,
  windowId: string,
  snapshot: WaylandWindowSnapshot,
  event: RuntimeWindowActivateRequestEvent,
): Omit<WindowStateRequestSuccess, "requestId" | "ok" | "kind"> {
  if (snapshot.id !== windowId) {
    return emptyWindowStateRequestResult();
  }
  const entry = ensureRuntimeCacheEntry(composition, events, snapshot);

  const invoked = events.emitWindowActivateRequest(entry.cache.window, event);
  if (!invoked) {
    return emptyWindowStateRequestResult();
  }

  const result = collectRuntimeMutationState();
  return {
    invoked: true,
    dirty: result.dirty,
    dirtyWindowIds: result.dirtyWindowIds,
    dirtyManagedWindowIds: result.dirtyManagedWindowIds,
    dirtyWindowNodeIds: result.dirtyWindowNodeIds,
    dirtyLayerNodeIds: result.dirtyLayerNodeIds,
    actions: result.actions,
    nextPollInMs: hasActiveAnimations() ? 0 : result.nextPollInMs,
  };
}

function emptyWindowStateRequestResult(): Omit<
  WindowStateRequestSuccess,
  "requestId" | "ok" | "kind"
> {
  return {
    invoked: false,
    dirty: false,
    dirtyWindowIds: [],
    actions: [],
    nextPollInMs: hasActiveAnimations() ? 0 : peekNextPollDelay(),
  };
}

function interactionMutationResult(
  invoked: boolean,
): Omit<NativeInteractionSuccess, "requestId" | "ok" | "kind"> {
  if (!invoked) {
    return {
      invoked: false,
      dirty: false,
      dirtyWindowIds: [],
      actions: [],
      nextPollInMs: hasActiveAnimations() ? 0 : peekNextPollDelay(),
    };
  }

  const result = collectRuntimeMutationState();
  return {
    invoked: true,
    dirty: result.dirty,
    dirtyWindowIds: result.dirtyWindowIds,
    dirtyManagedWindowIds: result.dirtyManagedWindowIds,
    dirtyWindowNodeIds: result.dirtyWindowNodeIds,
    dirtyLayerNodeIds: result.dirtyLayerNodeIds,
    actions: result.actions,
    nextPollInMs: hasActiveAnimations() ? 0 : result.nextPollInMs,
  };
}

function invokePointerMove(
  events: CompositorEventController,
  event: PointerMoveEvent,
): Omit<NativeInteractionSuccess, "requestId" | "ok" | "kind"> {
  return interactionMutationResult(events.emitPointerMove(event));
}

function invokeGestureSwipe(
  events: CompositorEventController,
  event: GestureSwipeEvent,
): Omit<NativeInteractionSuccess, "requestId" | "ok" | "kind"> {
  return interactionMutationResult(events.emitGestureSwipe(event));
}

async function invokePointerMoveAsync(
  events: CompositorEventController,
  event: PointerMoveEvent,
): Promise<Omit<NativeInteractionSuccess, "requestId" | "ok" | "kind">> {
  return interactionMutationResult(await events.emitPointerMoveAsync(event));
}

async function invokeGestureSwipeAsync(
  events: CompositorEventController,
  event: GestureSwipeEvent,
): Promise<Omit<NativeInteractionSuccess, "requestId" | "ok" | "kind">> {
  return interactionMutationResult(await events.emitGestureSwipeAsync(event));
}

function closeWindow(
  events: CompositorEventController,
  windowId: string,
): void {
  decorationPolicyWindowById.delete(windowId);
  const existing = cacheByWindowId.get(windowId);
  if (!existing) {
    return;
  }

  existing.closePoll?.cancel();
  events.emitClose(existing.cache.window);
  cacheByWindowId.delete(windowId);
  lastNativeCompositionByWindowId.delete(windowId);
  openedWindowIds.delete(windowId);
  initialConfiguredWindowIds.delete(windowId);
  firstCommittedWindowIds.delete(windowId);
  animationEntriesByWindowId.delete(windowId);
  dirtyWindowIds.delete(windowId);
  dropWindowDependencies(windowId);
  dropWindowState(windowId);
}

function startClose(
  events: CompositorEventController,
  effectConfig: RuntimeEffectConfig,
  windowId: string,
): Omit<StartCloseSuccess, "requestId" | "ok" | "kind"> {
  const entry = cacheByWindowId.get(windowId);
  if (!entry) {
    return {
      invoked: false,
      dirtyWindowIds: [],
      actions: [],
      windowEffects: null,
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

  const dirtyNodeIds = takeDirtyCompositionNodeIds(windowId, entry.cache);
  const reevaluated = entry.cache.reevaluate(dirtyNodeIds);
  const actions = drainPendingActions();
  return {
    invoked: true,
    serialized: reevaluated?.serialized,
    transform: entry.cache.lastTransform,
    managedWindow: entry.cache.lastManagedWindow,
    windowEffects: evaluateWindowEffects(effectConfig, windowId, entry),
    dirtyWindowIds: [windowId],
    dirtyWindowNodeIds: { [windowId]: dirtyNodeIds },
    actions,
    nextPollInMs: hasActiveAnimations() ? 0 : peekNextPollDelay(),
  };
}

function invokeHandler(
  effectConfig: RuntimeEffectConfig,
  windowId: string,
  handlerId: string,
): Omit<InvokeHandlerSuccess, "requestId" | "ok" | "kind"> {
  const entry = cacheByWindowId.get(windowId);
  if (!entry) {
    return {
      invoked: false,
      dirtyWindowIds: [],
      actions: [],
      windowEffects: null,
      nextPollInMs: hasActiveAnimations() ? 0 : peekNextPollDelay(),
    };
  }

  const invoked = entry.cache.invokeHandler(handlerId);
  if (!invoked) {
    return {
      invoked: false,
      dirtyWindowIds: [],
      actions: [],
      windowEffects: evaluateWindowEffects(effectConfig, windowId, entry),
      nextPollInMs: hasActiveAnimations() ? 0 : peekNextPollDelay(),
    };
  }

  const managedWindowOnly = isManagedWindowOnlyDirty(windowId);
  const dirtyNodeIds = managedWindowOnly
    ? []
    : takeDirtyCompositionNodeIds(windowId, entry.cache);
  if (managedWindowOnly) {
    takeDirtyWindowShaderUniformBindingKeys(windowId);
  }
  const reevaluated = managedWindowOnly
    ? undefined
    : entry.cache.reevaluate(dirtyNodeIds);
  if (managedWindowOnly) {
    entry.cache.reevaluateManagedWindow();
  }
  const actions = entry.pendingActions.splice(0, entry.pendingActions.length);
  if (runtimeEnv("SHOJI_SSD_HANDLER_DEBUG")) {
    console.debug(
      "runtime handler composition result",
      JSON.stringify({
        windowId,
        handlerId,
        dirtyNodeIds,
        managedWindowOnly,
        nodeCount: reevaluated
          ? countSerializedNodes(reevaluated.serialized)
          : 0,
        topLevel: reevaluated
          ? summarizeSerializedChildren(reevaluated.serialized)
          : null,
      }),
    );
  }

  return {
    invoked: true,
    serialized: reevaluated?.serialized,
    transform: entry.cache.lastTransform,
    managedWindow: entry.cache.lastManagedWindow,
    windowEffects: evaluateWindowEffects(effectConfig, windowId, entry),
    dirtyWindowIds: [windowId],
    dirtyManagedWindowIds: managedWindowOnly ? [windowId] : undefined,
    dirtyWindowNodeIds: { [windowId]: dirtyNodeIds },
    actions,
    nextPollInMs: hasActiveAnimations() ? 0 : peekNextPollDelay(),
  };
}

function countSerializedNodes(node: unknown): number {
  if (!node || typeof node !== "object") {
    return 0;
  }
  const record = node as { children?: unknown[] };
  return (
    1 +
    (record.children ?? []).reduce(
      (sum, child) => sum + countSerializedNodes(child),
      0,
    )
  );
}

function summarizeSerializedChildren(node: unknown): unknown {
  if (!node || typeof node !== "object") {
    return null;
  }
  const record = node as {
    kind?: string;
    nodeId?: string;
    children?: unknown[];
  };
  return {
    kind: record.kind,
    nodeId: record.nodeId,
    childCount: record.children?.length ?? 0,
    children: (record.children ?? []).map((child) => {
      if (!child || typeof child !== "object") {
        return { primitive: typeof child };
      }
      const childRecord = child as {
        kind?: string;
        nodeId?: string;
        children?: unknown[];
      };
      return {
        kind: childRecord.kind,
        nodeId: childRecord.nodeId,
        childCount: childRecord.children?.length ?? 0,
      };
    }),
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
    actions.push(
      ...entry.pendingActions.splice(0, entry.pendingActions.length),
    );
  }
  return actions;
}

function drainPendingActionsForWindow(windowId: string): RuntimeWindowAction[] {
  const entry = cacheByWindowId.get(windowId);
  if (!entry || entry.pendingActions.length === 0) {
    return [];
  }
  return entry.pendingActions.splice(0, entry.pendingActions.length);
}

function resolveComposition(
  loaded: Record<string, unknown>,
): WindowCompositionFunction {
  type WindowSlot = { composition?: WindowCompositionFunction | null };
  type WmSlot = { window?: WindowSlot };
  const maybeComposition =
    COMPOSITOR.window.composition ??
    (loaded.default as WmSlot | undefined)?.window?.composition ??
    (loaded.composition as WindowCompositionFunction | undefined);

  if (!maybeComposition) {
    throw new Error("config did not assign COMPOSITOR.window.composition");
  }

  return maybeComposition;
}

function resolveEvents(
  loaded: Record<string, unknown>,
): CompositorEventController {
  const maybeEvents =
    COMPOSITOR.event ??
    (loaded.default as { event?: CompositorEventController } | undefined)
      ?.event;

  if (!maybeEvents) {
    throw new Error("config did not initialize COMPOSITOR.event");
  }

  return maybeEvents;
}

function resolveEffectConfig(
  loaded: Record<string, unknown>,
): RuntimeEffectConfig {
  const maybeEffect =
    COMPOSITOR.effect ??
    (
      loaded.default as
        | {
            effect?: {
              background_effect?: CompiledEffectHandle | null;
              window?: RuntimeEffectConfig["window"];
              layer?: RuntimeEffectConfig["layer"];
              popup?: RuntimeEffectConfig["popup"];
            };
          }
        | undefined
    )?.effect;

  return {
    background_effect: maybeEffect?.background_effect ?? null,
    window: maybeEffect?.window,
    layer: maybeEffect?.layer,
    popup: maybeEffect?.popup,
  };
}

interface PendingRuntimeResponseUpdates {
  envUpdates: ReturnType<typeof drainPendingEnvUpdates>;
  cursorConfig: ReturnType<typeof takePendingCursorConfig>;
}

function takeRuntimeResponseUpdates(): PendingRuntimeResponseUpdates {
  return {
    envUpdates: drainPendingEnvUpdates(),
    cursorConfig: takePendingCursorConfig(),
  };
}

function hasRuntimeResponseUpdates(
  updates: PendingRuntimeResponseUpdates,
): boolean {
  return updates.envUpdates !== undefined || updates.cursorConfig !== undefined;
}

function tryWriteNativeSchedulerResponse(
  output: EmbeddedRuntimeBridge,
  response: SchedulerTickSuccess,
  updates: PendingRuntimeResponseUpdates,
): boolean {
  if (
    hasRuntimeResponseUpdates(updates) ||
    response.actions.length > 0 ||
    response.displayConfig !== undefined ||
    response.workspaceConfig !== undefined ||
    response.keyBindingConfig !== undefined ||
    response.pointerConfig !== undefined ||
    response.inputConfig !== undefined ||
    response.eventConfig !== undefined ||
    response.processConfig !== undefined ||
    (response.processActions?.length ?? 0) > 0 ||
    response.debugConfig !== undefined
  ) {
    return false;
  }

  output.beginSchedulerResponse(
    response.requestId,
    response.dirty,
    response.runtimeDirty ?? false,
    response.nextPollInMs ?? -1,
  );
  const managedOnly = new Set(response.dirtyManagedWindowIds ?? []);
  for (const windowId of response.dirtyWindowIds) {
    output.addSchedulerDirtyWindow(windowId, managedOnly.has(windowId));
  }
  for (const [windowId, nodeIds] of Object.entries(
    response.dirtyWindowNodeIds ?? {},
  )) {
    for (const nodeId of nodeIds) {
      output.addSchedulerDirtyWindowNode(windowId, nodeId);
    }
  }
  for (const layerId of response.dirtyLayerIds ?? []) {
    output.addSchedulerDirtyLayer(layerId);
  }
  for (const [layerId, nodeIds] of Object.entries(
    response.dirtyLayerNodeIds ?? {},
  )) {
    for (const nodeId of nodeIds) {
      output.addSchedulerDirtyLayerNode(layerId, nodeId);
    }
  }
  output.finishNativeResponse();
  return true;
}

const NATIVE_CACHED_RESPONSE_FIELD_COUNT = 15;
const nativeCachedResponsePayload = new Uint8Array(
  NATIVE_CACHED_RESPONSE_FIELD_COUNT * Float64Array.BYTES_PER_ELEMENT,
);
const nativeCachedResponseView = new DataView(
  nativeCachedResponsePayload.buffer,
);

function tryWriteNativeCachedResponse(
  output: EmbeddedRuntimeBridge,
  response: EvaluateSuccess,
  updates: PendingRuntimeResponseUpdates,
  windowId: string,
): boolean {
  if (
    response.kind !== "evaluateCached" ||
    hasRuntimeResponseUpdates(updates) ||
    (response.actions?.length ?? 0) > 0 ||
    response.displayConfig !== undefined ||
    response.workspaceConfig !== undefined ||
    response.keyBindingConfig !== undefined ||
    response.pointerConfig !== undefined ||
    response.inputConfig !== undefined ||
    response.eventConfig !== undefined ||
    response.processConfig !== undefined ||
    (response.processActions?.length ?? 0) > 0
  ) {
    return false;
  }

  const transform = response.transform ?? identityTransform();
  const managedWindow = response.managedWindow ?? identityManagedWindow();
  const workspace = managedWindow.workspace;
  if (
    workspace !== undefined &&
    typeof workspace !== "string" &&
    (typeof workspace !== "number" || !Number.isFinite(workspace))
  ) {
    return false;
  }
  const visibleOutputs = managedWindow.visibleOutputs;
  if (
    visibleOutputs !== undefined &&
    visibleOutputs !== null &&
    !Array.isArray(visibleOutputs)
  ) {
    return false;
  }

  let flags = 0;
  if (response.managedWindowOnly) flags |= 1 << 0;
  if (managedWindow.managed) flags |= 1 << 1;
  if (managedWindow.visible) flags |= 1 << 2;
  if (managedWindow.idle) flags |= 1 << 3;
  if (managedWindow.interactive) flags |= 1 << 4;
  if (managedWindow.forceRectSize) flags |= 1 << 5;
  if (managedWindow.tiled) flags |= 1 << 6;
  if (managedWindow.rect !== undefined) flags |= 1 << 7;
  if (managedWindow.allowTearing === false) flags |= 1 << 8;
  if (managedWindow.allowTearing === true) flags |= 2 << 8;
  if (Array.isArray(visibleOutputs)) flags |= 1 << 10;
  if (managedWindow.zIndex !== undefined) flags |= 1 << 11;

  const rect = managedWindow.rect;
  const fields = [
    response.requestId,
    response.nextPollInMs ?? -1,
    flags,
    transform.origin.x,
    transform.origin.y,
    transform.translateX,
    transform.translateY,
    transform.scaleX,
    transform.scaleY,
    transform.opacity,
    rect?.x ?? 0,
    rect?.y ?? 0,
    rect?.width ?? 0,
    rect?.height ?? 0,
    managedWindow.zIndex ?? 0,
  ];
  if (fields.some((value) => !Number.isFinite(value))) {
    return false;
  }
  for (let index = 0; index < fields.length; index++) {
    nativeCachedResponseView.setFloat64(index * 8, fields[index], true);
  }

  writeNativeWindowEffectUpdate(
    output,
    response.requestId,
    windowId,
    response.windowEffects ?? null,
  );
  output.beginCachedResponse(nativeCachedResponsePayload);
  for (const nodeId of response.dirtyNodeIds ?? []) {
    output.addCachedDirtyNode(nodeId);
  }
  for (const outputName of visibleOutputs ?? []) {
    output.addCachedVisibleOutput(outputName);
  }
  if (typeof workspace === "string") {
    output.setCachedWorkspaceString(workspace);
  } else if (typeof workspace === "number") {
    output.setCachedWorkspaceNumber(workspace);
  }
  output.finishNativeResponse();
  return true;
}

function writeResponse(
  output: EmbeddedRuntimeBridge,
  response:
    | EvaluateSuccess
    | SchedulerTickSuccess
    | WindowClosedSuccess
    | StartCloseSuccess
    | InvokeHandlerSuccess
    | InvokeKeyBindingSuccess
    | WindowResizeSuccess
    | WindowMoveSuccess
    | WindowStateRequestSuccess
    | NativeInteractionSuccess
    | GetEffectConfigSuccess
    | EvaluateLayerEffectsSuccess
    | EvaluatePopupEffectsSuccess
    | LifecycleEnableSuccess
    | LifecycleDisableSuccess
    | DrainPreloadSuccess
    | WindowDecorationPolicySuccess
    | RuntimeFailure,
): Promise<void> {
  return writeResponseWithRuntimeUpdates(
    output,
    response,
    takeRuntimeResponseUpdates(),
  );
}

function writeInteractionEventResponse(
  output: EmbeddedRuntimeBridge,
  response: NativeInteractionSuccess,
): Promise<void> {
  const updates = takeRuntimeResponseUpdates();
  if (
    !hasRuntimeResponseUpdates(updates) &&
    response.displayConfig === undefined &&
    response.workspaceConfig === undefined &&
    response.keyBindingConfig === undefined &&
    response.pointerConfig === undefined &&
    response.inputConfig === undefined &&
    response.eventConfig === undefined &&
    response.processConfig === undefined &&
    (response.processActions?.length ?? 0) === 0
  ) {
    output.writeInteractionResponse(response);
    return Promise.resolve();
  }
  return writeResponseWithRuntimeUpdates(output, response, updates);
}

function writeResponseWithRuntimeUpdates(
  output: EmbeddedRuntimeBridge,
  response: Parameters<typeof writeResponse>[1],
  updates: PendingRuntimeResponseUpdates,
): Promise<void> {
  const responseWithoutEffects = writeNativeEffectUpdate(output, response);
  const responseWithRuntimeUpdates = {
    ...responseWithoutEffects,
    ...(updates.envUpdates ? { envUpdates: updates.envUpdates } : {}),
    ...(updates.cursorConfig ? { cursorConfig: updates.cursorConfig } : {}),
  };
  output.writeResponse(JSON.stringify(responseWithRuntimeUpdates));
  return Promise.resolve();
}

function writeNativeEffectUpdate(
  output: EmbeddedRuntimeBridge,
  response: Parameters<typeof writeResponse>[1],
): Record<string, unknown> {
  const responseWithoutEffects = {
    ...response,
  } as Record<string, unknown>;
  if (!response.ok) {
    return responseWithoutEffects;
  }
  if (Object.prototype.hasOwnProperty.call(response, "windowEffects")) {
    const windowResponse = response as
      | EvaluateSuccess
      | StartCloseSuccess
      | InvokeHandlerSuccess;
    const windowId = windowResponse.effectTargetId;
    if (windowId === undefined) {
      throw new Error(
        `native window effect response ${response.kind} has no target id`,
      );
    }
    writeNativeWindowEffectUpdate(
      output,
      response.requestId,
      windowId,
      windowResponse.windowEffects ?? null,
    );
    delete responseWithoutEffects.windowEffects;
    delete responseWithoutEffects.effectTargetId;
  } else if (response.kind === "getEffectConfig") {
    writeNativeEffectTargets(
      output,
      response.requestId,
      NATIVE_EFFECT_UPDATE_BACKGROUND,
      [
        {
          targetKind: NATIVE_EFFECT_TARGET_BACKGROUND,
          targetId: "",
          value: response.backgroundEffect ?? null,
        },
      ],
      () =>
        output.writeEffectUpdate(response.requestId, {
          kind: "background",
          effect: response.backgroundEffect ?? null,
        }),
    );
    delete responseWithoutEffects.backgroundEffect;
  } else if (response.kind === "evaluateLayerEffects") {
    writeNativeEffectTargets(
      output,
      response.requestId,
      NATIVE_EFFECT_UPDATE_LAYERS,
      response.effects.map((assignment) => ({
        targetKind: NATIVE_EFFECT_TARGET_LAYER,
        targetId: assignment.layerId,
        value: assignment.effects,
      })),
      () =>
        output.writeEffectUpdate(response.requestId, {
          kind: "layers",
          assignments: response.effects,
        }),
    );
    delete responseWithoutEffects.effects;
  } else if (response.kind === "evaluatePopupEffects") {
    writeNativeEffectTargets(
      output,
      response.requestId,
      NATIVE_EFFECT_UPDATE_POPUPS,
      response.effects.map((assignment) => ({
        targetKind: NATIVE_EFFECT_TARGET_POPUP,
        targetId: assignment.popupId,
        value: assignment.effects,
        structure: {
          effects: assignment.effects,
          surfacePolicy: assignment.surfacePolicy ?? null,
        },
      })),
      () =>
        output.writeEffectUpdate(response.requestId, {
          kind: "popups",
          assignments: response.effects,
        }),
    );
    delete responseWithoutEffects.effects;
  }
  return responseWithoutEffects;
}

const NATIVE_EFFECT_UPDATE_BACKGROUND = 0;
const NATIVE_EFFECT_UPDATE_WINDOW = 1;
const NATIVE_EFFECT_UPDATE_LAYERS = 2;
const NATIVE_EFFECT_UPDATE_POPUPS = 3;
const NATIVE_EFFECT_TARGET_BACKGROUND = 0;
const NATIVE_EFFECT_TARGET_WINDOW = 1;
const NATIVE_EFFECT_TARGET_LAYER = 2;
const NATIVE_EFFECT_TARGET_POPUP = 3;
const NATIVE_EFFECT_SLOT_BACKGROUND = 0;
const NATIVE_EFFECT_SLOT_BEHIND = 1;
const NATIVE_EFFECT_SLOT_BEHIND_ROOT_SURFACE = 2;
const NATIVE_EFFECT_SLOT_IN_FRONT = 3;
const NATIVE_EFFECT_SLOT_REPLACE = 4;

interface NativeEffectTarget {
  targetKind: number;
  targetId: string;
  value: unknown;
  structure?: unknown;
}

type NativeEffectShaderPathSegment =
  | { kind: "input" }
  | { kind: "pipeline"; index: number }
  | { kind: "texture"; name: string };

interface NativeEffectUniformBinding {
  key: string;
  effectSlot: number;
  shaderPath: NativeEffectShaderPathSegment[];
  name: string;
  values: number[];
  arrayElementWidth?: number;
}

function writeNativeWindowEffectUpdate(
  output: EmbeddedRuntimeBridge,
  requestId: number,
  windowId: string,
  effects: WindowEffectAssignment | null,
): void {
  writeNativeEffectTargets(
    output,
    requestId,
    NATIVE_EFFECT_UPDATE_WINDOW,
    [
      {
        targetKind: NATIVE_EFFECT_TARGET_WINDOW,
        targetId: windowId,
        value: effects,
      },
    ],
    () =>
      output.writeEffectUpdate(requestId, {
        kind: "window",
        windowId,
        effects,
      }),
  );
}

function writeNativeEffectTargets(
  output: EmbeddedRuntimeBridge,
  requestId: number,
  updateKind: number,
  targets: NativeEffectTarget[],
  writeFullUpdate: () => void,
): void {
  const changes: Array<{
    slotId: number;
    values: number[];
    arrayElementWidth?: number;
  }> = [];
  let canPatch = targets.length > 0;
  for (const target of targets) {
    const targetKey = nativeEffectTargetKey(
      target.targetKind,
      target.targetId,
    );
    const previous = lastNativeEffectByTarget.get(targetKey);
    const previousStructure =
      lastNativeEffectStructureByTarget.get(targetKey);
    const slots = effectUniformSlotIdsByTarget.get(targetKey);
    if (
      previous === undefined ||
      previousStructure === undefined ||
      slots === undefined ||
      !sameEffectStructure(
        previousStructure,
        target.structure ?? target.value,
      )
    ) {
      canPatch = false;
      break;
    }
    const previousBindings = collectNativeEffectUniformBindings(previous);
    const nextBindings = collectNativeEffectUniformBindings(target.value);
    if (
      previousBindings.length !== nextBindings.length ||
      previousBindings.some(
        (binding, index) => binding.key !== nextBindings[index]?.key,
      )
    ) {
      canPatch = false;
      break;
    }
    for (let index = 0; index < nextBindings.length; index++) {
      const previousBinding = previousBindings[index];
      const nextBinding = nextBindings[index];
      if (sameUniformValues(previousBinding.values, nextBinding.values)) {
        continue;
      }
      const slotId = slots.get(nextBinding.key);
      if (slotId === undefined) {
        canPatch = false;
        break;
      }
      changes.push({
        slotId,
        values: nextBinding.values,
        arrayElementWidth: nextBinding.arrayElementWidth,
      });
    }
    if (!canPatch) {
      break;
    }
  }

  if (canPatch) {
    output.beginEffectShaderUniformSlotPatches(requestId, updateKind);
    for (const target of targets) {
      output.addEffectShaderUniformPatchTarget(requestId, target.targetId);
    }
    for (const change of changes) {
      if (change.arrayElementWidth !== undefined) {
        output.writeEffectShaderUniformArraySlotPatch(
          requestId,
          change.slotId,
          change.arrayElementWidth,
          new Float32Array(change.values),
        );
        continue;
      }
      const [x = 0, y = 0, z = 0, w = 0] = change.values;
      output.writeEffectShaderUniformSlotPatch(
        requestId,
        change.slotId,
        change.values.length,
        x,
        y,
        z,
        w,
      );
    }
  } else {
    writeFullUpdate();
    for (const target of targets) {
      syncNativeEffectUniformSlots(output, target);
    }
  }

  for (const target of targets) {
    lastNativeEffectByTarget.set(
      nativeEffectTargetKey(target.targetKind, target.targetId),
      target.value,
    );
    lastNativeEffectStructureByTarget.set(
      nativeEffectTargetKey(target.targetKind, target.targetId),
      target.structure ?? target.value,
    );
  }
}

function syncNativeEffectUniformSlots(
  output: EmbeddedRuntimeBridge,
  target: NativeEffectTarget,
): void {
  const targetKey = nativeEffectTargetKey(
    target.targetKind,
    target.targetId,
  );
  output.clearEffectShaderUniformSlots(target.targetKind, target.targetId);
  const slotIds = new Map<string, number>();
  for (const binding of collectNativeEffectUniformBindings(target.value)) {
    const slotId = nextEffectUniformSlotId++;
    if (nextEffectUniformSlotId > 0xffff_ffff) {
      nextEffectUniformSlotId = 1;
    }
    slotIds.set(binding.key, slotId);
    output.registerEffectShaderUniformSlot(
      slotId,
      target.targetKind,
      target.targetId,
      binding.effectSlot,
      JSON.stringify(binding.shaderPath),
      binding.name,
    );
  }
  effectUniformSlotIdsByTarget.set(targetKey, slotIds);
}

function clearNativeEffectTarget(
  output: EmbeddedRuntimeBridge,
  targetKind: number,
  targetId: string,
): void {
  output.clearEffectShaderUniformSlots(targetKind, targetId);
  const targetKey = nativeEffectTargetKey(targetKind, targetId);
  effectUniformSlotIdsByTarget.delete(targetKey);
  lastNativeEffectByTarget.delete(targetKey);
  lastNativeEffectStructureByTarget.delete(targetKey);
}

function nativeEffectTargetKey(targetKind: number, targetId: string): string {
  return `${targetKind}\u0000${targetId}`;
}

function collectNativeEffectUniformBindings(
  value: unknown,
): NativeEffectUniformBinding[] {
  const bindings: NativeEffectUniformBinding[] = [];
  if (isCompiledEffect(value)) {
    collectCompiledEffectUniformBindings(
      value,
      NATIVE_EFFECT_SLOT_BACKGROUND,
      [],
      bindings,
    );
    return bindings;
  }
  if (!isRecord(value)) {
    return bindings;
  }
  const slots: Array<[string, number]> = [
    ["behind", NATIVE_EFFECT_SLOT_BEHIND],
    ["behindRootSurface", NATIVE_EFFECT_SLOT_BEHIND_ROOT_SURFACE],
    ["inFront", NATIVE_EFFECT_SLOT_IN_FRONT],
    ["replace", NATIVE_EFFECT_SLOT_REPLACE],
  ];
  for (const [name, effectSlot] of slots) {
    const handle = value[name];
    if (!isRecord(handle) || !isCompiledEffect(handle.effect)) {
      continue;
    }
    collectCompiledEffectUniformBindings(
      handle.effect,
      effectSlot,
      [],
      bindings,
    );
  }
  return bindings;
}

function collectCompiledEffectUniformBindings(
  effect: Record<string, unknown>,
  effectSlot: number,
  prefix: NativeEffectShaderPathSegment[],
  bindings: NativeEffectUniformBinding[],
): void {
  collectEffectInputUniformBindings(
    effect.input,
    effectSlot,
    [...prefix, { kind: "input" }],
    bindings,
  );
  const pipeline = effect.pipeline;
  if (!Array.isArray(pipeline)) {
    return;
  }
  for (let index = 0; index < pipeline.length; index++) {
    const stage = pipeline[index];
    if (!isRecord(stage)) {
      continue;
    }
    const path = [...prefix, { kind: "pipeline" as const, index }];
    if (stage.kind === "shader-stage") {
      collectShaderStageUniformBindings(stage, effectSlot, path, bindings);
    } else if (stage.kind === "blend") {
      collectEffectInputUniformBindings(
        stage.input,
        effectSlot,
        path,
        bindings,
      );
    } else if (stage.kind === "unit" && isCompiledEffect(stage.effect)) {
      collectCompiledEffectUniformBindings(
        stage.effect,
        effectSlot,
        path,
        bindings,
      );
    }
  }
}

function collectEffectInputUniformBindings(
  input: unknown,
  effectSlot: number,
  path: NativeEffectShaderPathSegment[],
  bindings: NativeEffectUniformBinding[],
): void {
  if (!isRecord(input) || input.kind !== "shader-input") {
    return;
  }
  collectShaderStageUniformBindings(input, effectSlot, path, bindings);
}

function collectShaderStageUniformBindings(
  shader: Record<string, unknown>,
  effectSlot: number,
  path: NativeEffectShaderPathSegment[],
  bindings: NativeEffectUniformBinding[],
): void {
  const uniforms = shader.uniforms;
  if (isRecord(uniforms)) {
    for (const name of Object.keys(uniforms).sort()) {
      const snapshot = numericUniformSnapshot(uniforms[name]);
      if (snapshot === null) {
        continue;
      }
      bindings.push({
        key: nativeEffectUniformBindingKey(effectSlot, path, name),
        effectSlot,
        shaderPath: path,
        name,
        values: snapshot.values,
        arrayElementWidth: snapshot.arrayElementWidth,
      });
    }
  }
  const textures = shader.textures;
  if (!isRecord(textures)) {
    return;
  }
  for (const name of Object.keys(textures).sort()) {
    collectEffectInputUniformBindings(
      textures[name],
      effectSlot,
      [...path, { kind: "texture", name }],
      bindings,
    );
  }
}

function nativeEffectUniformBindingKey(
  effectSlot: number,
  path: NativeEffectShaderPathSegment[],
  name: string,
): string {
  const encodedPath = path
    .map((segment) => {
      if (segment.kind === "input") {
        return "i";
      }
      if (segment.kind === "pipeline") {
        return `p${segment.index}`;
      }
      return `t${segment.name.length}:${segment.name}`;
    })
    .join("/");
  return `${effectSlot}|${encodedPath}|u${name.length}:${name}`;
}

function sameEffectStructure(previous: unknown, next: unknown): boolean {
  if (Object.is(previous, next)) {
    return true;
  }
  if (Array.isArray(previous) || Array.isArray(next)) {
    return (
      Array.isArray(previous) &&
      Array.isArray(next) &&
      previous.length === next.length &&
      previous.every((entry, index) =>
        sameEffectStructure(entry, next[index]),
      )
    );
  }
  if (!isRecord(previous) || !isRecord(next)) {
    return false;
  }
  const previousKeys = Object.keys(previous).sort();
  const nextKeys = Object.keys(next).sort();
  if (
    previousKeys.length !== nextKeys.length ||
    previousKeys.some((key, index) => key !== nextKeys[index])
  ) {
    return false;
  }
  const isShader =
    (previous.kind === "shader-stage" ||
      previous.kind === "shader-input") &&
    previous.kind === next.kind;
  for (const key of previousKeys) {
    if (isShader && key === "uniforms") {
      if (!sameShaderUniformStructure(previous[key], next[key])) {
        return false;
      }
      continue;
    }
    if (!sameEffectStructure(previous[key], next[key])) {
      return false;
    }
  }
  return true;
}

function sameShaderUniformStructure(
  previous: unknown,
  next: unknown,
): boolean {
  if (!isRecord(previous) || !isRecord(next)) {
    return previous === next;
  }
  const previousNames = Object.keys(previous).sort();
  const nextNames = Object.keys(next).sort();
  return (
    previousNames.length === nextNames.length &&
    previousNames.every(
      (name, index) =>
        name === nextNames[index] &&
        sameShaderUniformShape(
          numericUniformSnapshot(previous[name]),
          numericUniformSnapshot(next[name]),
        ),
    )
  );
}

function sameShaderUniformShape(
  previous: NativeShaderUniformSnapshot | null,
  next: NativeShaderUniformSnapshot | null,
): boolean {
  return (
    previous !== null &&
    next !== null &&
    previous.values.length === next.values.length &&
    previous.arrayElementWidth === next.arrayElementWidth
  );
}

function sameUniformValues(previous: number[], next: number[]): boolean {
  return (
    previous.length === next.length &&
    previous.every((value, index) => Object.is(value, next[index]))
  );
}

function isCompiledEffect(value: unknown): value is Record<string, unknown> {
  return isRecord(value) && value.kind === "compiled-effect";
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

async function* readEmbeddedMessages(
  bridge: EmbeddedRuntimeBridge,
): AsyncGenerator<RuntimeRequest> {
  while (true) {
    const envelope = await bridge.readRequest();
    if (envelope === null) {
      return;
    }
    const composition = envelope.composition();
    if (composition !== null) {
      yield composition;
      continue;
    }
    const effect = envelope.effect();
    if (effect !== null) {
      yield effect;
      continue;
    }
    const interaction = envelope.interaction();
    if (interaction !== null) {
      yield interaction;
      continue;
    }
    const scheduler = envelope.scheduler();
    if (scheduler !== null) {
      yield scheduler;
      continue;
    }
    const fastKind = envelope.fastKind();
    if (fastKind === 1) {
      const request: EvaluateCachedRequest = {
        requestId: envelope.fastRequestId(),
        kind: "evaluateCached",
        windowId: envelope.fastWindowId(),
        forceFullReevaluation: envelope.fastForceFullReevaluation(),
        nowMs: envelope.fastNowMs(),
      };
      envelope.finishFast();
      yield request;
      continue;
    }
    if (fastKind === 2) {
      const request: SchedulerTickRequest = {
        requestId: envelope.fastRequestId(),
        kind: "schedulerTick",
        nowMs: envelope.fastNowMs(),
      };
      envelope.finishFast();
      yield request;
      continue;
    }
    const json = envelope.json();
    if (json === null) {
      throw new Error("runtime request envelope contains no request");
    }
    yield JSON.parse(json) as RuntimeRequest;
  }
}

export async function runEmbeddedRuntime(
  configPath: string,
  bridgeId: number,
): Promise<void> {
  const Bridge = runtimeGlobal.ShojiRuntimeBridge;
  if (!Bridge) {
    throw new Error("embedded ShojiWM runtime bridge is unavailable");
  }
  await main(configPath, new Bridge(bridgeId));
}
