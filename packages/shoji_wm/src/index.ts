import type {
  AppIconProps,
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
  BoxProps,
  ButtonProps,
  InteractionState,
  InteractionStyleVariants,
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
} from "./types";
import { createWindowManagerEventController } from "./events";
import { createElementNode } from "./runtime";
import { serializeDecorationTree } from "./serialize";
import { computed, read, isSignal } from "./signals";
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
  installShaderResolverBridge,
  loadShader,
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
  InteractionState,
  InteractionStyleVariants,
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
};

export function windowAction(
  action: WindowActionType,
): WindowActionDescriptor {
  return {
    kind: "window-action",
    action,
  };
}

export function getInteractionState(
  window: WaylandWindow,
  id: string,
): InteractionState {
  return {
    hovered: window.interaction((interaction) => interaction.hoveredIds.includes(id)),
    active: window.interaction((interaction) => interaction.activeIds.includes(id)),
    focused: window.isFocused,
  };
}

export function applyInteractionStyle(
  base: SSDStyle | undefined,
  variants: InteractionStyleVariants | undefined,
  state: InteractionState,
): SSDStyle | undefined {
  if (!base && !variants) {
    return undefined;
  }

  const hasReactiveState =
    isSignal(state.focused) || isSignal(state.hovered) || isSignal(state.active);

  if (!hasReactiveState) {
    let style: SSDStyle = { ...(base ?? {}) };

    if (read(state.focused) && variants?.focused) {
      style = { ...style, ...variants.focused };
    }
    if (read(state.hovered) && variants?.hovered) {
      style = { ...style, ...variants.hovered };
    }
    if (read(state.active) && variants?.active) {
      style = { ...style, ...variants.active };
    }

    return style;
  }

  const keys = new Set<string>([
    ...Object.keys(base ?? {}),
    ...Object.keys(variants?.focused ?? {}),
    ...Object.keys(variants?.hovered ?? {}),
    ...Object.keys(variants?.active ?? {}),
  ]);
  const style: SSDStyle = {};

  for (const key of keys) {
    (style as Record<string, unknown>)[key] = computed(() => {
      let merged: SSDStyle = { ...(base ?? {}) };

      if (read(state.focused) && variants?.focused) {
        merged = { ...merged, ...variants.focused };
      }
      if (read(state.hovered) && variants?.hovered) {
        merged = { ...merged, ...variants.hovered };
      }
      if (read(state.active) && variants?.active) {
        merged = { ...merged, ...variants.active };
      }

      return (merged as Record<string, unknown>)[key];
    });
  }

  return style;
}

function defineIntrinsicComponent<TProps extends { children?: DecorationChild | DecorationChild[] }>(
  type: DecorationNodeType,
): Component<TProps> {
  return function IntrinsicComponent(props: TProps): DecorationElementNode {
    return createElementNode(type, props as Record<string, unknown>);
  };
}
