export interface WaylandWindowSnapshot {
  readonly id: string;
  readonly title: string;
  readonly appId?: string;
  readonly isFocused: boolean;
  readonly isFloating: boolean;
  readonly isMaximized: boolean;
  readonly isFullscreen: boolean;
  readonly isXwayland: boolean;
  readonly icon?: WindowIcon;
  readonly interaction: DecorationInteractionSnapshot;
}

export interface WaylandWindow extends WaylandWindowSnapshot {

  close(): void;
  maximize(): void;
  minimize(): void;
  isXWayland(): boolean;
}

export type PrimitiveChild = string | number;
export type WindowIcon = string | { name?: string; bytes?: Uint8Array };

export interface DecorationInteractionSnapshot {
  hoveredIds: string[];
  activeIds: string[];
}

export interface InteractionState {
  hovered: boolean;
  active: boolean;
  focused: boolean;
}

export interface DecorationElementNode {
  kind: "element";
  type: DecorationNodeType;
  key: string | number | null;
  props: Record<string, unknown>;
  children: DecorationChild[];
}

export type DecorationChild = DecorationElementNode | PrimitiveChild;

export type DecorationNodeType =
  | "Box"
  | "Label"
  | "Button"
  | "AppIcon"
  | "Window"
  | "WindowBorder"
  | "Fragment";

export interface ComponentProps {
  children?: DecorationChild | DecorationChild[];
}

export type Component<TProps extends ComponentProps = ComponentProps> = (
  props: TProps,
) => DecorationElementNode;

export type Direction = "row" | "column" | "horizontal" | "vertical";
export type AlignItems = "start" | "center" | "end" | "stretch";
export type JustifyContent = "start" | "center" | "end" | "space-between";
export type FontWeight = "normal" | "medium" | "semibold" | "bold" | number;
export type FontFamily = string | string[];

export interface BorderValue {
  px: number;
  color: string;
}

export interface SSDStyle {
  width?: number | string;
  height?: number | string;
  minWidth?: number;
  minHeight?: number;
  maxWidth?: number;
  maxHeight?: number;
  flexGrow?: number;
  flexShrink?: number;
  gap?: number;
  padding?: number;
  paddingX?: number;
  paddingY?: number;
  paddingTop?: number;
  paddingRight?: number;
  paddingBottom?: number;
  paddingLeft?: number;
  margin?: number;
  marginX?: number;
  marginY?: number;
  marginTop?: number;
  marginRight?: number;
  marginBottom?: number;
  marginLeft?: number;
  alignItems?: AlignItems;
  justifyContent?: JustifyContent;
  background?: string;
  color?: string;
  opacity?: number;
  border?: BorderValue;
  borderTop?: BorderValue;
  borderRight?: BorderValue;
  borderBottom?: BorderValue;
  borderLeft?: BorderValue;
  borderRadius?: number;
  visible?: boolean;
  cursor?: string;
  fontSize?: number;
  fontWeight?: FontWeight;
  fontFamily?: FontFamily;
  textAlign?: "start" | "center" | "end";
  lineHeight?: number;
}

export interface InteractionStyleVariants {
  hovered?: SSDStyle;
  active?: SSDStyle;
  focused?: SSDStyle;
}

export interface BoxProps extends ComponentProps {
  direction?: Direction;
  split?: Direction;
  style?: SSDStyle;
  id?: string;
}

export interface LabelProps extends ComponentProps {
  text?: string;
  style?: SSDStyle;
  id?: string;
}

export interface ButtonProps extends ComponentProps {
  style?: SSDStyle;
  id?: string;
  onClick?: WindowActionDescriptor | (() => void);
}

export interface AppIconProps extends ComponentProps {
  icon?: WindowIcon;
  style?: SSDStyle;
  id?: string;
}

export interface WindowProps extends ComponentProps {
  style?: SSDStyle;
  id?: string;
  children?: never;
}

export interface WindowBorderProps extends ComponentProps {
  style?: SSDStyle;
  id?: string;
}

export type DecorationFunction = (window: WaylandWindow) => DecorationChild;

export interface WindowManagerDefinition {
  decoration: DecorationFunction | null;
  display?: DisplayConfig;
}

export type DisplayModePreference =
  | "auto"
  | {
      width: number;
      height: number;
      refreshMhz?: number;
    };

export interface DisplayConfig {
  defaultMode?: DisplayModePreference;
}

export interface ReactiveWaylandWindowSignals {
  id: import("./signals").ReadonlySignal<string>;
  title: import("./signals").ReadonlySignal<string>;
  appId: import("./signals").ReadonlySignal<string | undefined>;
  isFocused: import("./signals").ReadonlySignal<boolean>;
  isFloating: import("./signals").ReadonlySignal<boolean>;
  isMaximized: import("./signals").ReadonlySignal<boolean>;
  isFullscreen: import("./signals").ReadonlySignal<boolean>;
  icon: import("./signals").ReadonlySignal<WindowIcon | undefined>;
  interaction: import("./signals").ReadonlySignal<DecorationInteractionSnapshot>;
}

export interface ReactiveWaylandWindow extends WaylandWindow {
  readonly signals: ReactiveWaylandWindowSignals;
}

export interface WaylandWindowActions {
  close(): void;
  maximize(): void;
  minimize(): void;
  isXWayland(): boolean;
}

export interface ReactiveWaylandWindowHandle {
  readonly window: ReactiveWaylandWindow;
  update(snapshot: WaylandWindowSnapshot): void;
}

export type WindowActionType = "close" | "maximize" | "minimize";

export interface WindowActionDescriptor {
  kind: "window-action";
  action: WindowActionType;
}

export type SerializableDecorationChild =
  | SerializedDecorationNode
  | PrimitiveChild;

export interface SerializedDecorationNode {
  kind: DecorationNodeType;
  props: Record<string, unknown>;
  children: SerializableDecorationChild[];
}
