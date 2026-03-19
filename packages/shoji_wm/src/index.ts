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
  BoxProps,
  ButtonProps,
  InteractionState,
  InteractionStyleVariants,
  LabelProps,
  SSDStyle,
  SerializableDecorationChild,
  SerializedDecorationNode,
  WindowActionDescriptor,
  WindowActionType,
  WindowBorderProps,
  WindowManagerDefinition,
  WindowProps,
  WaylandWindowActions,
  WaylandWindowSnapshot,
  WaylandWindow,
} from "./types";
import { createElementNode } from "./runtime";
import { serializeDecorationTree } from "./serialize";
export { createReactiveWindow } from "./reactive-window";
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
} from "./signals";

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
  LabelProps,
  InteractionState,
  InteractionStyleVariants,
  SSDStyle,
  SerializableDecorationChild,
  SerializedDecorationNode,
  WindowActionDescriptor,
  WindowActionType,
  WindowBorderProps,
  WindowManagerDefinition,
  WindowProps,
  WaylandWindowActions,
  WaylandWindowSnapshot,
  WaylandWindow,
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
export const Window = defineIntrinsicComponent<WindowProps>("Window");
export const WindowBorder = defineIntrinsicComponent<WindowBorderProps>("WindowBorder");

/**
 * Placeholder namespace for future WM-level entrypoints.
 */
export const WINDOW_MANAGER: WindowManagerDefinition = {
  decoration: null,
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
    hovered: window.interaction.hoveredIds.includes(id),
    active: window.interaction.activeIds.includes(id),
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

  let style: SSDStyle = { ...(base ?? {}) };

  if (state.focused && variants?.focused) {
    style = { ...style, ...variants.focused };
  }
  if (state.hovered && variants?.hovered) {
    style = { ...style, ...variants.hovered };
  }
  if (state.active && variants?.active) {
    style = { ...style, ...variants.active };
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
