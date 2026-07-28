/// <reference path="./runtime-globals.d.ts" />
import type {
  AppIconProps,
  ImageProps,
  Component,
  ComponentProps,
  WindowCompositionInteractionSnapshot,
  WindowCompositionContext,
  WindowCompositionFunction,
  WindowCompositionPhase,
  CompositionChild,
  CompositionElementNode,
  ReactiveWaylandWindow,
  ReactiveWaylandWindowHandle,
  ReactiveWaylandWindowSignals,
  CompositionNodeType,
  DisplayConfig,
  DisplayConfigDraft,
  DisplayModePreference,
  EffectAlphaMode,
  EffectInvalidationPolicyHandle,
  AutomaticEffectInvalidationPolicyHandle,
  BoxProps,
  ButtonProps,
  LabelProps,
  ManagedWindowProps,
  ManagedWindowAnimationEasing,
  ManagedWindowAnimationMode,
  ManagedWindowPoint,
  ManagedWindowPointAnimationOptions,
  ManagedWindowRect,
  ManagedWindowRectAnimationOptions,
  ManagedWindowScalarAnimationOptions,
  ManagedWindowScheduleAnimationOptions,
  ManagedWindowState,
  ManagedWindowTransform,
  MaybeSignal,
  SSDStyle,
  BackdropSourceHandle,
  XrayBackdropSourceHandle,
  ShaderInputHandle,
  BlendMode,
  BlendStageHandle,
  ShaderEffectProps,
  CompiledEffectHandle,
  DualKawaseBlurStageHandle,
  EffectInputHandle,
  EffectStageHandle,
  ImageSourceHandle,
  NamedTextureHandle,
  StateTextureHandle,
  StateTextureSourceHandle,
  EffectStateTextureFormat,
  EffectStateResizePolicy,
  RenderToStageHandle,
  NoiseKind,
  NoiseStageHandle,
  EffectOutsets,
  OutputConfigEntry,
  OutputController,
  OutputConfigureContext,
  OutputConfigureFactory,
  OutputInfo,
  OutputMode,
  OutputPositionPreference,
  OutputResolutionPreference,
  OutputStateSnapshot,
  WorkspaceActivateEvent,
  WorkspaceConfig,
  WorkspaceConfigEntry,
  WorkspaceConfigureFactory,
  WorkspaceController,
  WorkspaceGroupConfig,
  EnvController,
  EnvUpdateOperation,
  EnvUpdatePayload,
  EnvValue,
  InputAccelProfile,
  InputClickMethod,
  InputConfigDraft,
  InputConfigureContext,
  InputConfigureFactory,
  InputController,
  InputDeviceConfig,
  InputDeviceInfo,
  InputDeviceKindFlags,
  InputScrollMethod,
  InputTapButtonMap,
  KeyboardInputConfig,
  PointerInputConfig,
  TouchpadInputConfig,
  ProcessController,
  ProcessEnv,
  ProcessLaunchSpec,
  ProcessSpawnSpec,
  SaveStageHandle,
  ShaderUniformArrayElement,
  ShaderUniformArrayHandle,
  ShaderUniformArrayValues,
  ShaderUniformMap,
  ShaderUniformScalar,
  ShaderUniformValue,
  ShaderUniformVec2,
  ShaderUniformVec3,
  ShaderUniformVec4,
  ShaderModuleHandle,
  UnitStageHandle,
  WindowEffectAssignment,
  WindowEffectHandle,
  WindowSourceHandle,
  LayerEffectAssignment,
  LayerEffectHandle,
  LayerEffectInputHandle,
  LayerSourceHandle,
  PopupEffectAssignment,
  PopupEffectHandle,
  PopupEffectInputHandle,
  PopupSourceHandle,
  WaylandPopup,
  SerializableCompositionChild,
  SerializedCompositionNode,
  WindowActionDescriptor,
  WindowActionType,
  WindowBorderProps,
  WindowBorderInteraction,
  WindowResizeHitArea,
  CompositorDefinition,
  CompositorEffectConfig,
  CompositorWindowController,
  CompositorWindowDecorationController,
  WindowDecorationContext,
  WindowDecorationDecision,
  WindowDecorationMode,
  WindowDecorationPolicyReason,
  WindowDecorationProtocol,
  WindowDecorationResolver,
  WindowDecorationState,
  WindowPosition,
  WindowSize,
  WindowSizeConstraints,
  ClientWindowProps,
  WindowProps,
  WindowTransform,
  TransformOrigin,
  WaylandWindowActions,
  WaylandWindowSnapshot,
  WaylandWindow,
  LayerPosition,
  ReactiveWaylandLayer,
  ReactiveWaylandLayerHandle,
  ReactiveWaylandLayerSignals,
  WaylandLayer,
  WaylandLayerAnchor,
  WaylandLayerDesiredSize,
  WaylandLayerEdge,
  WaylandLayerExclusiveZone,
  WaylandLayerKeyboardInteractivity,
  WaylandLayerKind,
  WaylandLayerMargin,
  WaylandLayerSnapshot,
  LayerController,
  LayerInsets,
  UsableAreaOptions,
  StartupOnceSpec,
  StartupProcessRunPolicy,
  StartupServiceSpec,
  ManagedProcessRestartPolicy,
  ManagedProcessReloadPolicy,
  KeyBindingController,
  KeyBindingOptions,
  KeyBindingEventPhase,
  PointerController,
  PreloadController,
  RuntimeController,
  DebugController,
  SSDRebuildSuppressionHandle,
  SSDRebuildSuppressionOptions,
  SSDRebuildSuppressionViolationPolicy,
} from "./types";
import { createCompositorEventController } from "./events";
import { suppressSSDRebuild, withSSDRebuildSuppressed } from "./runtime-hooks";
import {
  KEY_BINDING_CONTROLLER,
  beginKeyBindingRegistration,
  commitKeyBindingRegistration,
  invokeKeyBinding,
  takePendingKeyBindingConfig,
} from "./key";
import {
  POINTER_CONTROLLER,
  beginPointerConfigRegistration,
  commitPointerConfigRegistration,
  takePendingPointerConfig,
} from "./pointer";
import { INPUT_CONTROLLER, installInputDeviceChangeEmitter } from "./input";
import { OUTPUT_CONTROLLER, installOutputChangeEmitter } from "./output";
import { WORKSPACE_CONTROLLER } from "./workspace";
import { DEBUG_CONTROLLER, takePendingDebugConfig } from "./debug";
import { ENV_CONTROLLER, drainPendingEnvUpdates } from "./env";
import { CURSOR_CONTROLLER, takePendingCursorConfig } from "./cursor";
import {
  WINDOW_DECORATION_CONTROLLER,
  resolveWindowDecorationDecision,
} from "./window-decoration";
import { LAYER_CONTROLLER, updateLayerSnapshots } from "./layer";
import {
  PROCESS_CONTROLLER,
  beginProcessConfigRegistration,
  commitProcessConfigRegistration,
  drainPendingProcessActions,
  installProcessResolverBridge,
  takePendingProcessConfig,
} from "./process";
import { createElementNode } from "./runtime";
import {
  computed as createComputedSignal,
  isSignal as isReadonlySignal,
} from "./signals";
import { resolveAssetPath } from "./shader";
import { serializeCompositionTree } from "./serialize";
export {
  advanceAnimationFrame,
  hasActiveAnimations,
  hasActiveAnimationsInStore,
  createAnimationControllerWithStore,
  createAnimationController,
  animationVariable,
  createWindowAnimationControllerWithStore,
  createWindowAnimationController,
  milliseconds,
  seconds,
  type AnimationRepeatMode,
  type AnimationStartOptions,
  type AnimationController,
  type AnimationVariable,
  type WindowAnimationController,
} from "./animation";
export {
  backdropSource,
  blend,
  compileEffect,
  compileLayerEffect,
  compilePopupEffect,
  compileWindowEffect,
  dualKawaseBlur,
  get,
  imageSource,
  installAssetResolverBridge,
  installShaderResolverBridge,
  loadShader,
  resolveAssetPath,
  noise,
  renderTo,
  save,
  shaderInput,
  shaderStage,
  stateSource,
  stateTexture,
  uniformArray,
  unit,
  layerSource,
  popupSource,
  windowSource,
  xrayBackdropSource,
  type CompileEffectOptions,
  type CompileLayerEffectOptions,
  type CompilePopupEffectOptions,
  type CompileWindowEffectOptions,
} from "./shader";
export {
  cubicBezier,
  ease,
  easeIn,
  easeInOut,
  easeInOutCubic,
  easeOut,
  easeOutCubic,
  easeOutExpo,
  linear,
  type CubicBezierEasingFunction,
  type EasingFunction,
} from "./easing";
export {
  createCompositorEventController,
  type LayerCreateListener,
  type LayerDestroyListener,
  type LayerUpdateListener,
  type RuntimeDisableEvent,
  type RuntimeDisableListener,
  type RuntimeEnableEvent,
  type RuntimeEnableListener,
  type RuntimeLifecycleReason,
  type RuntimePersistedState,
  type WindowCloseListener,
  type WindowFirstCommitListener,
  type WindowFocusListener,
  type WindowInitialConfigureListener,
  type CompositorEventController,
  type WindowOpenListener,
  type WindowResizeEdges,
  type WindowResizeEvent,
  type WindowResizeListener,
  type WindowResizePhase,
  type WindowResizePoint,
  type WindowResizeRect,
  type WindowResizeSource,
  type RuntimeWindowResizeEvent,
  type RuntimeWindowMoveEvent,
  type RuntimeWindowMaximizeRequestEvent,
  type RuntimeWindowMinimizeRequestEvent,
  type RuntimeWindowFullscreenRequestEvent,
  type RuntimeWindowActivateRequestEvent,
  type PointerModifierState,
  type PointerHitTarget,
  type PointerMoveListener,
  type PointerMoveAsyncListener,
  type GestureSwipeListener,
  type GestureSwipeAsyncListener,
  type GestureSwipeEvent,
  type GestureSwipePhase,
  type OutputChangeEvent,
  type OutputChangeListener,
  type InputDeviceChangeEvent,
  type InputDeviceChangeListener,
  type PointerMoveEvent,
  type PointerMovePoint,
  type RuntimeEventConfig,
  type WindowMoveEvent,
  type WindowMoveListener,
  type WindowMovePhase,
  type WindowMovePoint,
  type WindowMoveRect,
  type WindowMoveSource,
  type WindowMaximizeRequestEvent,
  type WindowMaximizeRequestListener,
  type WindowMinimizeRequestEvent,
  type WindowMinimizeRequestListener,
  type WindowFullscreenRequestEvent,
  type WindowFullscreenRequestListener,
  type WindowActivateRequestEvent,
  type WindowActivateRequestListener,
  type WindowActivateRequestSource,
  type WindowStateRequestSource,
  type WindowStartCloseListener,
} from "./events";
export { createReactiveWindow } from "./reactive-window";
export { createReactiveLayer } from "./reactive-layer";
export {
  OUTPUT_CONTROLLER,
  beginOutputConfigurationRegistration,
  commitOutputConfigurationRegistration,
  installOutputChangeEmitter,
  takePendingDisplayConfig,
  updateOutputState,
} from "./output";
export {
  WORKSPACE_CONTROLLER,
  beginWorkspaceConfigurationRegistration,
  commitWorkspaceConfigurationRegistration,
  emitWorkspaceActivate,
  takePendingWorkspaceConfig,
} from "./workspace";
export { LAYER_CONTROLLER, updateLayerSnapshots } from "./layer";
export {
  WINDOW_DECORATION_CONTROLLER,
  resolveWindowDecorationDecision,
} from "./window-decoration";
export { DEBUG_CONTROLLER, takePendingDebugConfig } from "./debug";
export { ENV_CONTROLLER, drainPendingEnvUpdates } from "./env";
export {
  CURSOR_CONTROLLER,
  currentCursorConfig,
  takePendingCursorConfig,
} from "./cursor";
export {
  KEY_BINDING_CONTROLLER,
  beginKeyBindingRegistration,
  commitKeyBindingRegistration,
  invokeKeyBinding,
  takePendingKeyBindingConfig,
} from "./key";
export {
  POINTER_CONTROLLER,
  beginPointerConfigRegistration,
  commitPointerConfigRegistration,
  takePendingPointerConfig,
} from "./pointer";
export {
  INPUT_CONTROLLER,
  beginInputConfigurationRegistration,
  commitInputConfigurationRegistration,
  installInputDeviceChangeEmitter,
  takePendingInputConfig,
  updateInputState,
} from "./input";
export {
  PROCESS_CONTROLLER,
  beginProcessConfigRegistration,
  commitProcessConfigRegistration,
  drainPendingProcessActions,
  installProcessResolverBridge,
  takePendingProcessConfig,
} from "./process";
export {
  createComponentStateStore,
  createComputed,
  createState,
  onCleanup,
  useLayoutEffect,
  useMemo,
  useRef,
  useComputed,
  useEffect,
  useState,
  withComponentRenderRoot,
} from "./runtime";
export {
  createCompositionEvaluationCache,
  diffWindowSnapshot,
  shouldReevaluateComposition,
  type CompositionEvaluationCache,
  type CompositionEvaluationResult,
  type WindowSnapshotDiff,
} from "./reconcile";
export {
  computed,
  effect,
  isSignal,
  read,
  signal,
  type ReadonlySignal,
  type Signal,
  type SignalSetter,
} from "./signals";
export {
  createWindowState,
  dropWindowState,
  type WindowStateDefault,
  type WindowStateKey,
  type WindowStateStore,
} from "./window-state";
export {
  createWindowStack,
  type WindowStack,
  type WindowStackAddOptions,
  type WindowStackOptions,
  type WindowStackPlacement,
} from "./window-stack";
export {
  createPoll,
  createManagedPoll,
  installSchedulerBridge,
  type PollCallback,
  type PollDirtyMode,
  type PollHandle,
} from "./scheduler";
export {
  createIpcServer,
  defaultSocketPath,
  type IpcClient,
  type IpcHandler,
  type IpcRequestMessage,
  type IpcServer,
} from "./ipc";
export {
  dropLayerDependencies,
  dropWindowDependencies,
  enterLayerNodeDependencyScope,
  enterLayerDependencyScope,
  consumeManagedWindowOnlyFastPathInvalidated,
  enterWindowEffectDependencyScope,
  enterWindowManagedDependencyScope,
  enterWindowNodeDependencyScope,
  enterWindowPatchDependencyScope,
  enterWindowShaderUniformDependencyScope,
  enterWindowDependencyScope,
  installRuntimeHooks,
  isManagedWindowOnlyDirty,
  leaveLayerNodeDependencyScope,
  leaveLayerDependencyScope,
  leaveWindowEffectDependencyScope,
  leaveWindowManagedDependencyScope,
  leaveWindowNodeDependencyScope,
  leaveWindowShaderUniformDependencyScope,
  leaveWindowDependencyScope,
  managedWindowOnlyDirtyIds,
  markManagedWindowDirty,
  markLayerDirty,
  markRuntimeDirty,
  markWindowDirty,
  suppressSSDRebuild,
  takeDirtyLayerNodeIds,
  takeManagedWindowOnlyDirty,
  takeDirtyWindowNodeIds,
  takeDirtyWindowShaderUniformBindingKeys,
  trackSignalRead,
  trackSignalWrite,
  withSSDRebuildSuppressed,
} from "./runtime-hooks";

export type {
  AppIconProps,
  BoxProps,
  ButtonProps,
  ImageFit,
  ImageProps,
  Component,
  WindowCompositionInteractionSnapshot,
  WindowCompositionContext,
  WindowCompositionFunction,
  WindowCompositionPhase,
  CompositionChild,
  CompositionElementNode,
  ReactiveWaylandWindow,
  ReactiveWaylandWindowHandle,
  ReactiveWaylandWindowSignals,
  CompositionNodeType,
  DisplayConfig,
  DisplayConfigDraft,
  DisplayModePreference,
  EffectAlphaMode,
  EffectInvalidationPolicyHandle,
  AutomaticEffectInvalidationPolicyHandle,
  LabelProps,
  ManagedWindowProps,
  ManagedWindowState,
  ManagedWindowTransform,
  MaybeSignal,
  SSDStyle,
  BackdropSourceHandle,
  XrayBackdropSourceHandle,
  ShaderInputHandle,
  BlendMode,
  BlendStageHandle,
  ShaderEffectProps,
  CompiledEffectHandle,
  DualKawaseBlurStageHandle,
  EffectInputHandle,
  EffectStageHandle,
  ImageSourceHandle,
  NamedTextureHandle,
  StateTextureHandle,
  StateTextureSourceHandle,
  EffectStateTextureFormat,
  EffectStateResizePolicy,
  RenderToStageHandle,
  NoiseKind,
  NoiseStageHandle,
  EffectOutsets,
  OutputConfigEntry,
  OutputConfigureContext,
  OutputConfigureFactory,
  OutputController,
  OutputInfo,
  OutputMode,
  OutputPositionPreference,
  OutputResolutionPreference,
  OutputStateSnapshot,
  EnvController,
  EnvUpdateOperation,
  EnvUpdatePayload,
  EnvValue,
  ProcessController,
  ProcessEnv,
  ProcessLaunchSpec,
  ProcessSpawnSpec,
  SaveStageHandle,
  ShaderUniformArrayElement,
  ShaderUniformArrayHandle,
  ShaderUniformArrayValues,
  ShaderUniformMap,
  ShaderUniformScalar,
  ShaderUniformValue,
  ShaderUniformVec2,
  ShaderUniformVec3,
  ShaderUniformVec4,
  ShaderModuleHandle,
  UnitStageHandle,
  WindowEffectAssignment,
  WindowEffectHandle,
  WindowSourceHandle,
  LayerEffectAssignment,
  LayerEffectHandle,
  LayerEffectInputHandle,
  LayerSourceHandle,
  PopupEffectAssignment,
  PopupEffectHandle,
  PopupEffectInputHandle,
  PopupSourceHandle,
  WaylandPopup,
  SerializableCompositionChild,
  SerializedCompositionNode,
  WindowActionDescriptor,
  WindowActionType,
  WindowBorderProps,
  WindowBorderInteraction,
  WindowResizeHitArea,
  CompositorDefinition,
  CompositorEffectConfig,
  CompositorRenderingConfig,
  SurfacePolicy,
  SurfacePolicyTarget,
  CompositorWindowController,
  CompositorWindowDecorationController,
  WindowDecorationContext,
  WindowDecorationDecision,
  WindowDecorationMode,
  WindowDecorationPolicyReason,
  WindowDecorationProtocol,
  WindowDecorationResolver,
  WindowDecorationState,
  WindowPosition,
  WindowSize,
  WindowSizeConstraints,
  ClientWindowProps,
  WindowProps,
  WindowTransform,
  TransformOrigin,
  WaylandWindowActions,
  WaylandWindowSnapshot,
  WaylandWindow,
  LayerPosition,
  ReactiveWaylandLayer,
  ReactiveWaylandLayerHandle,
  ReactiveWaylandLayerSignals,
  WaylandLayer,
  WaylandLayerAnchor,
  WaylandLayerDesiredSize,
  WaylandLayerEdge,
  WaylandLayerExclusiveZone,
  WaylandLayerKeyboardInteractivity,
  WaylandLayerKind,
  WaylandLayerMargin,
  WaylandLayerSnapshot,
  LayerController,
  LayerInsets,
  UsableAreaOptions,
  StartupOnceSpec,
  StartupProcessRunPolicy,
  StartupServiceSpec,
  ManagedProcessRestartPolicy,
  ManagedProcessReloadPolicy,
  KeyBindingController,
  KeyBindingOptions,
  KeyBindingEventPhase,
  PointerController,
  InputAccelProfile,
  InputClickMethod,
  InputConfigDraft,
  InputConfigureContext,
  InputConfigureFactory,
  InputController,
  InputDeviceConfig,
  InputDeviceInfo,
  InputDeviceKindFlags,
  InputScrollMethod,
  InputTapButtonMap,
  KeyboardInputConfig,
  PointerInputConfig,
  TouchpadInputConfig,
  CursorConfig,
  CursorController,
  RuntimeCursorConfigUpdate,
  PreloadController,
  RuntimeController,
  DebugController,
  SSDRebuildSuppressionHandle,
  SSDRebuildSuppressionOptions,
  SSDRebuildSuppressionViolationPolicy,
} from "./types";
export {
  CompositionSerializationError,
  serializeCompositionTree,
} from "./serialize";

export type CompositionNode = CompositionChild;

/**
 * A flexbox-like container that arranges children horizontally or vertically.
 * Accepts `direction`, `split`, and `style` props.
 * 子要素を水平または垂直に整列するフレックスボックス風コンテナ。
 * `direction`・`split`・`style` を受け付けます。
 *
 * @example
 * ```tsx
 * <Box direction="row" style={{ gap: 8, alignItems: "center" }}>
 *   <AppIcon icon={window.icon} style={{ width: 16, height: 16 }} />
 *   <Label text={window.title} style={{ flexGrow: 1 }} />
 * </Box>
 * ```
 */
export const Box = defineIntrinsicComponent<BoxProps>("Box");

/**
 * Renders a text string, optionally reactive via a `ReadonlySignal<string>`.
 * `ReadonlySignal<string>` を渡すとリアクティブに更新されるテキストを描画します。
 *
 * @example
 * ```tsx
 * <Label text={window.title} style={{ fontSize: 13, color: "#ffffffcc" }} />
 * ```
 */
export const Label = defineIntrinsicComponent<LabelProps>("Label");

/**
 * A pressable region that triggers an action on click. Pass a callback or a
 * `WindowActionDescriptor` from `windowAction(...)` to `onClick`.
 * クリックでアクションをトリガーするプレス可能な領域。
 * `onClick` にコールバックまたは `windowAction(...)` を渡します。
 *
 * @example
 * ```tsx
 * <Button onClick={windowAction("close")} style={{ width: 12, height: 12 }} />
 * ```
 */
export const Button = defineIntrinsicComponent<ButtonProps>("Button");

/**
 * Renders a window's application icon. Pass `window.icon` for reactive updates.
 * ウィンドウのアプリケーションアイコンを描画します。
 * `window.icon` を渡すとリアクティブに更新されます。
 *
 * @example
 * ```tsx
 * <AppIcon icon={window.icon} style={{ width: 16, height: 16 }} />
 * ```
 */
export const AppIcon = defineIntrinsicComponent<AppIconProps>("AppIcon");

const ImageIntrinsic = defineIntrinsicComponent<ImageProps>("Image");

/**
 * Displays an image from a file path (resolved relative to the config package
 * root) or a reactive `ReadonlySignal<string>` source.
 * 設定パッケージルートからの相対パス、またはリアクティブな
 * `ReadonlySignal<string>` ソースから画像を表示します。
 *
 * @example
 * ```tsx
 * <Image src="assets/wallpaper.jpg" fit="cover"
 *   style={{ width: "100%", height: "100%" }} />
 * ```
 */
export function Image(props: ImageProps) {
  const src = props.src;
  const resolved =
    typeof src === "string"
      ? resolveAssetPath(src)
      : isReadonlySignal(src)
        ? createComputedSignal(() => resolveAssetPath(src()))
        : src;
  return ImageIntrinsic({ ...props, src: resolved });
}

/**
 * Applies a compiled GPU shader effect to the region occupied by its children.
 * コンパイル済み GPU シェーダーエフェクトを子要素が占める領域に適用します。
 *
 * @example
 * ```tsx
 * const blur = compileEffect({ input: backdropSource(),
 *   pipeline: [dualKawaseBlur({ passes: 3 })] });
 *
 * <ShaderEffect shader={blur} style={{ height: 32, borderRadius: 8 }}>
 *   <Label text={window.title} />
 * </ShaderEffect>
 * ```
 */
export const ShaderEffect =
  defineIntrinsicComponent<ShaderEffectProps>("ShaderEffect");

/**
 * Binds a Wayland window into the compositor's layout system. Controls
 * placement (`rect`), workspace, visibility, z-order, opacity, transform,
 * and fullscreen tearing. Wrap `<ClientWindow/>` and `<WindowBorder/>` inside
 * this component.
 * Wayland ウィンドウをコンポジターのレイアウトシステムに結びつけます。
 * 配置（`rect`）・ワークスペース・表示状態・z オーダー・不透明度・トランスフォーム・
 * フルスクリーンテアリングを制御します。
 *
 * @example
 * ```tsx
 * COMPOSITOR.window.composition = (window) => (
 *   <ManagedWindow rect={window.position} zIndex={getZIndex(window)}>
 *     <WindowBorder
 *       style={{ border: { px: 1, color: "#ffffff20" }, borderRadius: 8 }}
 *     >
 *       <ClientWindow />
 *     </WindowBorder>
 *   </ManagedWindow>
 * );
 * ```
 */
export const ManagedWindow =
  defineIntrinsicComponent<ManagedWindowProps>("ManagedWindow");

/**
 * Renders the Wayland client's complete surface tree. Must be placed inside
 * `<ManagedWindow/>`. Leaf node — does not accept children. A bare
 * `<ClientWindow/>` preserves client-owned CSD shadows and resize margins;
 * wrapping it in an SSD container with a border applies that container's clip.
 * Wayland クライアントの実際のサーフェスバッファを描画します。
 * `<ManagedWindow/>` の内側に配置する必要があります。子要素は受け付けません。素の
 * `<ClientWindow/>` は CSD の影やリサイズマージンを保持し、border を持つ SSD
 * コンテナで囲んだ場合だけ、そのコンテナのクリップが適用されます。
 *
 * @example
 * ```tsx
 * <ManagedWindow rect={...}>
 *   <ClientWindow />
 * </ManagedWindow>
 * ```
 */
export const ClientWindow =
  defineIntrinsicComponent<ClientWindowProps>("Window");

/** Alias for `ClientWindow`. / `ClientWindow` の別名。 */
export const Window = ClientWindow;

/**
 * A chrome container placed around `<ClientWindow/>` that handles border
 * rendering and interactive resize hit areas.
 * `<ClientWindow/>` の周囲に配置し、ボーダー描画とインタラクティブな
 * リサイズヒット領域を処理するクロムコンテナ。
 *
 * @example
 * ```tsx
 * <WindowBorder
 *   style={{ borderRadius: 8, border: { px: 1, color: "#ffffff20" } }}
 *   interaction={{ resizeHitArea: { edgePx: 4, cornerPx: 8 } }}
 * >
 *   <ClientWindow />
 * </WindowBorder>
 * ```
 */
export const WindowBorder =
  defineIntrinsicComponent<WindowBorderProps>("WindowBorder");

const WINDOW_CONTROLLER: CompositorWindowController = {
  composition: null,
  decoration: WINDOW_DECORATION_CONTROLLER,
  focus(window) {
    window.focus();
  },
};

const RUNTIME_CONTROLLER: RuntimeController = {
  suppressSSDRebuild,
  withSSDRebuildSuppressed,
};

const PRELOAD_CONTROLLER: PreloadController = {};

/**
 * The ShojiWM compositor API root — the single entry-point for all config-layer
 * interactions with the compositor.
 * ShojiWM コンポジター API のルートオブジェクト。設定スクリプトからコンポジターへの
 * すべての操作はここから始まります。
 *
 * @example Startup processes + key bindings / 起動プロセス + キーバインド
 * ```ts
 * COMPOSITOR.process.once("shell", { command: ["foot", "--server"] });
 * COMPOSITOR.env.set("QT_QPA_PLATFORM", "wayland;xcb");
 *
 * COMPOSITOR.key.bind("terminal", "Super+T", () => {
 *   COMPOSITOR.process.spawn({ command: ["kitty"] });
 * });
 * ```
 *
 * @example Window composition / ウィンドウ合成
 * ```tsx
 * COMPOSITOR.window.composition = (window) => (
 *   <ManagedWindow rect={layoutRect(window)} zIndex={getZIndex(window)}>
 *     <WindowBorder style={{ borderRadius: 8 }}>
 *       <ClientWindow />
 *     </WindowBorder>
 *   </ManagedWindow>
 * );
 * ```
 *
 * @example Output configuration / 出力設定
 * ```ts
 * COMPOSITOR.output.configure((ctx) => ({
 *   "DP-1":  { resolution: { width: 2560, height: 1440, refreshRate: 144 } },
 *   "eDP-1": { resolution: "best", scale: 2 },
 * }));
 * ```
 *
 * @example Hot-reload lifecycle / ホットリロードライフサイクル
 * ```ts
 * COMPOSITOR.onDisable((event) => { event.persist("state", snapshot()); });
 * COMPOSITOR.onEnable((event) => {
 *   const saved = event.restore<MyState>("state");
 *   if (saved) restore(saved);
 * });
 * ```
 */
function createCompositorDefinition(): CompositorDefinition {
  return {
    event: createCompositorEventController(),
    onEnable(listener) {
      return this.event.onEnable(listener);
    },
    onDisable(listener) {
      return this.event.onDisable(listener);
    },
    preload: PRELOAD_CONTROLLER,
    effect: {
      background_effect: null,
    },
    rendering: {},
    output: OUTPUT_CONTROLLER,
    workspace: WORKSPACE_CONTROLLER,
    env: ENV_CONTROLLER,
    process: PROCESS_CONTROLLER,
    key: KEY_BINDING_CONTROLLER,
    pointer: POINTER_CONTROLLER,
    input: INPUT_CONTROLLER,
    cursor: CURSOR_CONTROLLER,
    runtime: RUNTIME_CONTROLLER,
    window: WINDOW_CONTROLLER,
    layer: LAYER_CONTROLLER,
    debug: DEBUG_CONTROLLER,
  };
}

const COMPOSITOR_GLOBAL_KEY = "__shoji_wm_COMPOSITOR__";
type ShojiCompositorGlobal = typeof globalThis & {
  [COMPOSITOR_GLOBAL_KEY]?: CompositorDefinition;
};

const shojiCompositorGlobal = globalThis as ShojiCompositorGlobal;

export const COMPOSITOR: CompositorDefinition =
  shojiCompositorGlobal[COMPOSITOR_GLOBAL_KEY] ??
  (shojiCompositorGlobal[COMPOSITOR_GLOBAL_KEY] = createCompositorDefinition());

installOutputChangeEmitter((event) => {
  COMPOSITOR.event.emitOutputChange(event);
});

installInputDeviceChangeEmitter((event) => {
  COMPOSITOR.event.emitInputDeviceChange(event);
});

export function windowAction(action: WindowActionType): WindowActionDescriptor {
  return {
    kind: "window-action",
    action,
  };
}

function defineIntrinsicComponent<TProps extends ComponentProps>(
  type: CompositionNodeType,
): Component<TProps> {
  return function IntrinsicComponent(props: TProps): CompositionElementNode {
    return createElementNode(type, props as Record<string, unknown>);
  };
}
