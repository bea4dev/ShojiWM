import type {
  AppIconProps,
  ImageProps,
  Component,
  ComponentProps,
  DecorationInteractionSnapshot,
  DecorationFunction,
  DecorationChild,
  DecorationElementNode,
  ReactiveWaylandWindow,
  ReactiveWaylandWindowHandle,
  ReactiveWaylandWindowSignals,
  DecorationNodeType,
  DisplayConfig,
  DisplayConfigDraft,
  DisplayModePreference,
  EffectInvalidationPolicyHandle,
  AutomaticEffectInvalidationPolicyHandle,
  BoxProps,
  ButtonProps,
  LabelProps,
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
  NoiseKind,
  NoiseStageHandle,
  OutputConfigEntry,
  OutputController,
  OutputMode,
  OutputPositionPreference,
  OutputResolutionPreference,
  OutputStateSnapshot,
  ProcessController,
  ProcessEnv,
  ProcessLaunchSpec,
  ProcessSpawnSpec,
  SaveStageHandle,
  ShaderUniformMap,
  ShaderUniformValue,
  ShaderModuleHandle,
  UnitStageHandle,
  SerializableDecorationChild,
  SerializedDecorationNode,
  WindowActionDescriptor,
  WindowActionType,
  WindowBorderProps,
  WindowManagerDefinition,
  WindowManagerEffectConfig,
  WindowPosition,
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
  WaylandLayerKind,
  WaylandLayerSnapshot,
  StartupOnceSpec,
  StartupProcessRunPolicy,
  StartupServiceSpec,
  ManagedProcessRestartPolicy,
  ManagedProcessReloadPolicy,
  KeyBindingController,
  KeyBindingOptions,
  KeyBindingEventPhase,
} from "./types";
import { createWindowManagerEventController } from "./events";
import {
  KEY_BINDING_CONTROLLER,
  beginKeyBindingRegistration,
  commitKeyBindingRegistration,
  invokeKeyBinding,
  takePendingKeyBindingConfig,
} from "./key";
import { OUTPUT_CONTROLLER } from "./output";
import {
  PROCESS_CONTROLLER,
  beginProcessConfigRegistration,
  commitProcessConfigRegistration,
  drainPendingProcessActions,
  installProcessResolverBridge,
  takePendingProcessConfig,
} from "./process";
import { createElementNode } from "./runtime";
import { computed as createComputedSignal, isSignal as isReadonlySignal } from "./signals";
import { resolveAssetPath } from "./shader";
import { serializeDecorationTree } from "./serialize";
export {
  advanceAnimationFrame,
  hasActiveAnimations,
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
  dualKawaseBlur,
  get,
  imageSource,
  installAssetResolverBridge,
  installShaderResolverBridge,
  loadShader,
  resolveAssetPath,
  noise,
  save,
  shaderInput,
  shaderStage,
  unit,
  xrayBackdropSource,
  type CompileEffectOptions,
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
  type EasingFunction,
} from "./easing";
export {
  createWindowManagerEventController,
  type LayerCreateListener,
  type LayerDestroyListener,
  type WindowCloseListener,
  type WindowFocusListener,
  type WindowManagerEventController,
  type WindowOpenListener,
  type WindowStartCloseListener,
} from "./events";
export { createReactiveWindow } from "./reactive-window";
export { createReactiveLayer } from "./reactive-layer";
export {
  OUTPUT_CONTROLLER,
  takePendingDisplayConfig,
  updateOutputState,
} from "./output";
export {
  KEY_BINDING_CONTROLLER,
  beginKeyBindingRegistration,
  commitKeyBindingRegistration,
  invokeKeyBinding,
  takePendingKeyBindingConfig,
} from "./key";
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
  createDecorationEvaluationCache,
  diffWindowSnapshot,
  shouldReevaluateDecoration,
  type DecorationEvaluationCache,
  type DecorationEvaluationResult,
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
  createPoll,
  createManagedPoll,
  installSchedulerBridge,
  type PollCallback,
  type PollDirtyMode,
  type PollHandle,
} from "./scheduler";
export {
  dropLayerDependencies,
  dropWindowDependencies,
  enterLayerNodeDependencyScope,
  enterLayerDependencyScope,
  enterWindowNodeDependencyScope,
  enterWindowDependencyScope,
  installRuntimeHooks,
  leaveLayerNodeDependencyScope,
  leaveLayerDependencyScope,
  leaveWindowNodeDependencyScope,
  leaveWindowDependencyScope,
  markLayerDirty,
  markRuntimeDirty,
  markWindowDirty,
  takeDirtyLayerNodeIds,
  takeDirtyWindowNodeIds,
  trackSignalRead,
  trackSignalWrite,
} from "./runtime-hooks";

export type {
  AppIconProps,
  BoxProps,
  ButtonProps,
  ImageFit,
  ImageProps,
  Component,
  DecorationInteractionSnapshot,
  DecorationFunction,
  DecorationChild,
  DecorationElementNode,
  ReactiveWaylandWindow,
  ReactiveWaylandWindowHandle,
  ReactiveWaylandWindowSignals,
  DecorationNodeType,
  DisplayConfig,
  DisplayModePreference,
  EffectInvalidationPolicyHandle,
  AutomaticEffectInvalidationPolicyHandle,
  LabelProps,
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
  NoiseKind,
  NoiseStageHandle,
  OutputConfigEntry,
  OutputController,
  OutputMode,
  OutputPositionPreference,
  OutputResolutionPreference,
  OutputStateSnapshot,
  ProcessController,
  ProcessEnv,
  ProcessLaunchSpec,
  ProcessSpawnSpec,
  SaveStageHandle,
  ShaderUniformMap,
  ShaderUniformValue,
  ShaderModuleHandle,
  UnitStageHandle,
  SerializableDecorationChild,
  SerializedDecorationNode,
  WindowActionDescriptor,
  WindowActionType,
  WindowBorderProps,
  WindowManagerDefinition,
  WindowManagerEffectConfig,
  WindowPosition,
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
  WaylandLayerKind,
  WaylandLayerSnapshot,
  StartupOnceSpec,
  StartupProcessRunPolicy,
  StartupServiceSpec,
  ManagedProcessRestartPolicy,
  ManagedProcessReloadPolicy,
  KeyBindingController,
  KeyBindingOptions,
  KeyBindingEventPhase,
} from "./types";
export { DecorationSerializationError, serializeDecorationTree } from "./serialize";

export type DecorationNode = DecorationChild;

/**
 * M2-T2 note:
 * These component placeholders already use the custom JSX runtime contract so
 * TSX snippets can be authored before concrete layout semantics land.
 */
export const Box = defineIntrinsicComponent<BoxProps>("Box");
export const Label = defineIntrinsicComponent<LabelProps>("Label");
export const Button = defineIntrinsicComponent<ButtonProps>("Button");
export const AppIcon = defineIntrinsicComponent<AppIconProps>("AppIcon");

const ImageIntrinsic = defineIntrinsicComponent<ImageProps>("Image");
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
export const ShaderEffect = defineIntrinsicComponent<ShaderEffectProps>("ShaderEffect");
export const ClientWindow = defineIntrinsicComponent<ClientWindowProps>("Window");
export const Window = ClientWindow;
export const WindowBorder = defineIntrinsicComponent<WindowBorderProps>("WindowBorder");

/**
 * Placeholder namespace for future WM-level entrypoints.
 */
export const WINDOW_MANAGER: WindowManagerDefinition = {
  decoration: null,
  event: createWindowManagerEventController(),
  effect: {
    background_effect: null,
  },
  output: OUTPUT_CONTROLLER,
  process: PROCESS_CONTROLLER,
  key: KEY_BINDING_CONTROLLER,
};

export function windowAction(
  action: WindowActionType,
): WindowActionDescriptor {
  return {
    kind: "window-action",
    action,
  };
}

function defineIntrinsicComponent<TProps extends ComponentProps>(
  type: DecorationNodeType,
): Component<TProps> {
  return function IntrinsicComponent(props: TProps): DecorationElementNode {
    return createElementNode(type, props as Record<string, unknown>);
  };
}
