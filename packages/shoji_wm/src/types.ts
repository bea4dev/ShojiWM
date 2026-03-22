export interface WaylandWindowSnapshot {
  readonly id: string;
  readonly title: string;
  readonly appId?: string;
  readonly position: WindowPosition;
  readonly isFocused: boolean;
  readonly isFloating: boolean;
  readonly isMaximized: boolean;
  readonly isFullscreen: boolean;
  readonly isXwayland: boolean;
  readonly icon?: WindowIcon;
  readonly interaction: DecorationInteractionSnapshot;
}

export type MaybeSignal<T> = T | import("./signals").ReadonlySignal<T>;

export interface WaylandWindow {
  readonly id: string;
  readonly title: import("./signals").ReadonlySignal<string>;
  readonly appId: import("./signals").ReadonlySignal<string | undefined>;
  readonly position: WindowPosition;
  readonly transform: WindowTransform;
  readonly animation: import("./animation").WindowAnimationController;
  readonly isFocused: import("./signals").ReadonlySignal<boolean>;
  readonly isFloating: import("./signals").ReadonlySignal<boolean>;
  readonly isMaximized: import("./signals").ReadonlySignal<boolean>;
  readonly isFullscreen: import("./signals").ReadonlySignal<boolean>;
  readonly icon: import("./signals").ReadonlySignal<WindowIcon | undefined>;
  readonly interaction: import("./signals").ReadonlySignal<DecorationInteractionSnapshot>;
  close(): void;
  maximize(): void;
  minimize(): void;
  setCloseAnimationDuration(durationMs: number): void;
  isXWayland(): boolean;
}

export interface WindowPosition {
  x: number;
  y: number;
  width: number;
  height: number;
}

export interface WindowTransform {
  origin: MaybeSignal<TransformOrigin>;
  translateX: MaybeSignal<number>;
  translateY: MaybeSignal<number>;
  scaleX: MaybeSignal<number>;
  scaleY: MaybeSignal<number>;
  opacity: MaybeSignal<number>;
}

export interface TransformOrigin {
  x: MaybeSignal<number>;
  y: MaybeSignal<number>;
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
  | "ShaderEffect"
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
export type NoiseKind = "salt";
export type BlendMode = "normal" | "add" | "screen" | "multiply";
export type ShaderUniformScalar = MaybeSignal<number>;
export type ShaderUniformValue =
  | ShaderUniformScalar
  | readonly [ShaderUniformScalar, ShaderUniformScalar]
  | readonly [ShaderUniformScalar, ShaderUniformScalar, ShaderUniformScalar]
  | readonly [
      ShaderUniformScalar,
      ShaderUniformScalar,
      ShaderUniformScalar,
      ShaderUniformScalar,
    ];
export type ShaderUniformMap = Record<string, ShaderUniformValue>;

export interface BackdropBlurOptions {
  radius?: number;
  passes?: number;
}

export interface ShaderModuleHandle {
  kind: "shader-module";
  path: string;
}

export interface ShaderStageHandle {
  kind: "shader-stage";
  shader: ShaderModuleHandle;
  uniforms?: ShaderUniformMap;
}

export interface BackdropSourceHandle {
  kind: "backdrop-source";
}

export interface ImageSourceHandle {
  kind: "image-source";
  path: string;
}

export interface NamedTextureHandle {
  kind: "named-texture";
  name: string;
}

export interface NoiseStageHandle {
  kind: "noise";
  noiseKind: NoiseKind;
  amount?: number;
}

export interface DualKawaseBlurStageHandle {
  kind: "dual-kawase-blur";
  radius?: number;
  passes?: number;
}

export interface SaveStageHandle {
  kind: "save";
  name: string;
}

export interface BlendStageHandle {
  kind: "blend";
  input: EffectInputHandle;
  mode?: BlendMode;
  alpha?: number;
}

export interface UnitStageHandle {
  kind: "unit";
  effect: CompiledEffectHandle;
}

export type EffectInputHandle =
  | BackdropSourceHandle
  | ImageSourceHandle
  | NamedTextureHandle;

export type EffectStageHandle =
  | ShaderStageHandle
  | NoiseStageHandle
  | DualKawaseBlurStageHandle
  | SaveStageHandle
  | BlendStageHandle
  | UnitStageHandle;

export interface CompiledEffectHandle {
  kind: "compiled-effect";
  input: EffectInputHandle;
  pipeline: EffectStageHandle[];
}

export interface BorderValue {
  px: MaybeSignal<number>;
  color: MaybeSignal<string>;
}

export interface SSDStyle {
  width?: MaybeSignal<number | string>;
  height?: MaybeSignal<number | string>;
  minWidth?: MaybeSignal<number>;
  minHeight?: MaybeSignal<number>;
  maxWidth?: MaybeSignal<number>;
  maxHeight?: MaybeSignal<number>;
  flexGrow?: MaybeSignal<number>;
  flexShrink?: MaybeSignal<number>;
  gap?: MaybeSignal<number>;
  padding?: MaybeSignal<number>;
  paddingX?: MaybeSignal<number>;
  paddingY?: MaybeSignal<number>;
  paddingTop?: MaybeSignal<number>;
  paddingRight?: MaybeSignal<number>;
  paddingBottom?: MaybeSignal<number>;
  paddingLeft?: MaybeSignal<number>;
  margin?: MaybeSignal<number>;
  marginX?: MaybeSignal<number>;
  marginY?: MaybeSignal<number>;
  marginTop?: MaybeSignal<number>;
  marginRight?: MaybeSignal<number>;
  marginBottom?: MaybeSignal<number>;
  marginLeft?: MaybeSignal<number>;
  alignItems?: MaybeSignal<AlignItems>;
  justifyContent?: MaybeSignal<JustifyContent>;
  background?: MaybeSignal<string>;
  color?: MaybeSignal<string>;
  opacity?: MaybeSignal<number>;
  border?: MaybeSignal<BorderValue>;
  borderTop?: MaybeSignal<BorderValue>;
  borderRight?: MaybeSignal<BorderValue>;
  borderBottom?: MaybeSignal<BorderValue>;
  borderLeft?: MaybeSignal<BorderValue>;
  borderRadius?: MaybeSignal<number>;
  visible?: MaybeSignal<boolean>;
  cursor?: MaybeSignal<string>;
  fontSize?: MaybeSignal<number>;
  fontWeight?: MaybeSignal<FontWeight>;
  fontFamily?: MaybeSignal<FontFamily>;
  textAlign?: MaybeSignal<"start" | "center" | "end">;
  lineHeight?: MaybeSignal<number>;
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

export interface ShaderEffectProps extends ComponentProps {
  shader: CompiledEffectHandle;
  direction?: Direction;
  split?: Direction;
  style?: SSDStyle;
  id?: string;
}

export interface ClientWindowProps extends ComponentProps {
  style?: SSDStyle;
  id?: string;
  children?: never;
}

export type WindowProps = ClientWindowProps;

export interface WindowBorderProps extends ComponentProps {
  style?: SSDStyle;
  id?: string;
}

export type DecorationFunction = (window: WaylandWindow) => DecorationChild;

export interface WindowManagerDefinition {
  decoration: DecorationFunction | null;
  event: import("./events").WindowManagerEventController;
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
  positionX: import("./signals").ReadonlySignal<number>;
  positionY: import("./signals").ReadonlySignal<number>;
  positionWidth: import("./signals").ReadonlySignal<number>;
  positionHeight: import("./signals").ReadonlySignal<number>;
  isFocused: import("./signals").ReadonlySignal<boolean>;
  isFloating: import("./signals").ReadonlySignal<boolean>;
  isMaximized: import("./signals").ReadonlySignal<boolean>;
  isFullscreen: import("./signals").ReadonlySignal<boolean>;
  icon: import("./signals").ReadonlySignal<WindowIcon | undefined>;
  interaction: import("./signals").ReadonlySignal<DecorationInteractionSnapshot>;
  transformOriginX: import("./signals").Signal<number>;
  transformOriginY: import("./signals").Signal<number>;
  transformTranslateX: import("./signals").Signal<number>;
  transformTranslateY: import("./signals").Signal<number>;
  transformScaleX: import("./signals").Signal<number>;
  transformScaleY: import("./signals").Signal<number>;
  transformOpacity: import("./signals").Signal<number>;
}

export interface ReactiveWaylandWindow extends WaylandWindow {
  readonly signals: ReactiveWaylandWindowSignals;
}

export interface WaylandWindowActions {
  close(): void;
  maximize(): void;
  minimize(): void;
  setCloseAnimationDuration(durationMs: number): void;
  isXWayland(): boolean;
}

export interface ReactiveWaylandWindowHandle {
  readonly window: ReactiveWaylandWindow;
  readonly transform: WindowTransform;
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
