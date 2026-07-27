export interface WaylandWindowSnapshot {
  readonly id: string;
  readonly title: string;
  readonly appId?: string;
  readonly position: WindowPosition;
  readonly rect: WindowPosition;
  readonly isFocused: boolean;
  readonly isFloating: boolean;
  readonly isMaximized: boolean;
  readonly isFullscreen: boolean;
  readonly isXwayland: boolean;
  readonly decoration: WindowDecorationState;
  readonly sizeConstraints: WindowSizeConstraints;
  readonly isResizable: boolean;
  readonly isTransient: boolean;
  readonly parentId?: string;
  readonly icon?: WindowIcon;
  readonly interaction: WindowCompositionInteractionSnapshot;
}

export type WindowDecorationMode = "client" | "server";

export type WindowDecorationProtocol =
  "xdg-decoration-v1" | "kde-server-decoration" | "xwayland" | "none";

export type WindowDecorationPolicyReason =
  "initial" | "clientRequest" | "clientUnset" | "metadataChanged" | "reload";

/** Current client/compositor decoration negotiation state. */
export interface WindowDecorationState {
  readonly protocol: WindowDecorationProtocol;
  readonly clientPreference: WindowDecorationMode | null;
  /** Last mode sent to the client, or the compositor fallback without a negotiation protocol. */
  readonly configuredMode: WindowDecorationMode;
  /** Mode acknowledged and committed by the client. */
  readonly mode: WindowDecorationMode;
}

/** Input passed to the synchronous decoration policy resolver. */
export interface WindowDecorationContext {
  readonly protocol: WindowDecorationProtocol;
  readonly clientPreference: WindowDecorationMode | null;
  readonly canNegotiate: boolean;
  readonly reason: WindowDecorationPolicyReason;
}

export interface WindowDecorationDecision {
  readonly mode: WindowDecorationMode;
}

export type WindowDecorationResolver = (
  window: WaylandWindow,
  context: WindowDecorationContext,
) => WindowDecorationDecision;

export interface CompositorWindowDecorationController {
  /**
   * Register the policy that selects CSD (`client`) or SSD (`server`). The
   * resolver runs when the client preference or relevant window metadata
   * changes and during config reload.
   */
  configure(resolver: WindowDecorationResolver): void;
}

export type WaylandLayerKind = "background" | "bottom" | "top" | "overlay";

export type WaylandLayerEdge = "top" | "bottom" | "left" | "right";

export type WaylandLayerKeyboardInteractivity =
  "none" | "onDemand" | "exclusive";

/**
 * Anchored edges. `true` for each edge the client requested. A layer with all
 * four `true` means it stretches across the entire output; `top + bottom +
 * left` for example pins to three edges and stretches vertically.
 */
export interface WaylandLayerAnchor {
  readonly top: boolean;
  readonly bottom: boolean;
  readonly left: boolean;
  readonly right: boolean;
}

/**
 * Exclusive-zone request from the client. See `zwlr_layer_surface_v1`.
 *
 * - `exclusive` — surface reserves `size` logical pixels along its anchored
 *   edge; other surfaces avoid this strip.
 * - `neutral` — surface participates in avoidance but reserves nothing.
 * - `dontCare` — surface opts out; compositor may extend it under reserved
 *   zones.
 */
export type WaylandLayerExclusiveZone =
  | { readonly mode: "exclusive"; readonly size: number }
  | { readonly mode: "neutral" }
  | { readonly mode: "dontCare" };

export interface WaylandLayerMargin {
  readonly top: number;
  readonly right: number;
  readonly bottom: number;
  readonly left: number;
}

export interface WaylandLayerDesiredSize {
  readonly width: number;
  readonly height: number;
}

export interface WaylandLayerSnapshot {
  readonly id: string;
  readonly namespace?: string;
  readonly layer: WaylandLayerKind;
  readonly outputName: string;
  readonly position: LayerPosition;
  readonly anchor: WaylandLayerAnchor;
  readonly exclusiveZone: WaylandLayerExclusiveZone;
  /**
   * The single edge the client wants its exclusive zone applied to (since
   * layer-shell v5). `null` when the client did not select an unambiguous
   * edge — fall back to the implicit edge derived from `anchor`.
   */
  readonly exclusiveEdge: WaylandLayerEdge | null;
  readonly margin: WaylandLayerMargin;
  readonly keyboardInteractivity: WaylandLayerKeyboardInteractivity;
  /** Size the client asked for in its layer-shell `set_size` request. */
  readonly desiredSize: WaylandLayerDesiredSize;
}

/**
 * A plain value or a `ReadonlySignal<T>`. Composition functions accept both so
 * callers can pass either a static constant or a reactive signal.
 * 固定値または `ReadonlySignal<T>` のいずれかです。合成関数は両方を受け付けるので、
 * 静的な定数またはリアクティブなシグナルのどちらでも渡せます。
 */
export type MaybeSignal<T> = T | import("./signals").ReadonlySignal<T>;

/**
 * A mapped (visible) Wayland toplevel window. Reactive properties are
 * `ReadonlySignal<T>` — the compositor re-evaluates any composition code that
 * reads them whenever they change.
 * マップされた（表示中の）Wayland トップレベルウィンドウ。リアクティブなプロパティは
 * `ReadonlySignal<T>` です。読み取った合成コードは変更時にコンポジターが自動的に
 * 再評価します。
 *
 * @example Read reactive state / リアクティブな状態を読む
 * ```ts
 * COMPOSITOR.window.composition = (window) => {
 *   const focused = window.isFocused.value;
 *   const title = window.title.value;
 *   // ...
 * };
 * ```
 */
export interface WaylandWindow {
  /** Stable string identifier unique to this window for its lifetime. / ウィンドウのライフタイムを通じてユニークな安定した識別子。 */
  readonly id: string;
  /** Reactive window title as set by the client. / クライアントが設定するリアクティブなウィンドウタイトル。 */
  readonly title: import("./signals").ReadonlySignal<string>;
  /** Reactive app-id (e.g. `"org.gnome.Nautilus"`). `undefined` if the client did not set one. / リアクティブな app-id。クライアントが設定していない場合は `undefined`。 */
  readonly appId: import("./signals").ReadonlySignal<string | undefined>;
  /** Current logical position and size in global compositor coordinates. / グローバル座標でのウィンドウの現在の論理的な位置とサイズ。 */
  readonly position: WindowPosition;
  /** Alias for `position`. / `position` の別名。 */
  readonly rect: WindowPosition;
  /**
   * Per-window key/value state store. Keys are created with `createWindowState`;
   * reading a key returns a `Signal<T>` scoped to this window.
   * `createWindowState` で作成したキーで読み取ると、このウィンドウにスコープされた
   * `Signal<T>` を返すウィンドウごとの状態ストア。
   *
   * @example
   * ```ts
   * const isMinimized = createWindowState("minimized", { default: false });
   * // inside composition:
   * window.state[isMinimized].value; // Signal<boolean>
   * ```
   */
  readonly state: import("./window-state").WindowStateStore;
  /**
   * GPU transform applied to this window during composition. Write signals or
   * values to drive reactive transforms (scale, translate, opacity, origin).
   * 合成時にこのウィンドウに適用される GPU トランスフォーム。シグナルまたは値を
   * 書き込んでリアクティブなトランスフォーム（スケール・移動・不透明度・原点）を制御します。
   */
  readonly transform: WindowTransform;
  /**
   * Animation controller for this window. Start/stop/set named animation
   * variables and read their progress as signals.
   * このウィンドウ用のアニメーションコントローラー。名前付きアニメーション変数の
   * 開始・停止・設定とシグナルとしての進捗読み取りを行います。
   */
  readonly animation: import("./animation").AnimationController;
  /** `true` while this window holds keyboard focus. / キーボードフォーカスを持っている間 `true`。 */
  readonly isFocused: import("./signals").ReadonlySignal<boolean>;
  /** `true` when the window is in floating (non-tiled) mode. / ウィンドウがフローティング（非タイル）モードのとき `true`。 */
  readonly isFloating: import("./signals").ReadonlySignal<boolean>;
  /** `true` when the window is maximized. / ウィンドウが最大化されているとき `true`。 */
  readonly isMaximized: import("./signals").ReadonlySignal<boolean>;
  /** `true` when the window is fullscreen. / ウィンドウがフルスクリーンのとき `true`。 */
  readonly isFullscreen: import("./signals").ReadonlySignal<boolean>;
  /** Reactive effective CSD/SSD negotiation state. */
  readonly decoration: import("./signals").ReadonlySignal<WindowDecorationState>;
  /** Min/max size constraints as reported by the client. / クライアントが報告する最小・最大サイズ制約。 */
  readonly sizeConstraints: import("./signals").ReadonlySignal<WindowSizeConstraints>;
  /** `false` when the client has disabled interactive resize. / クライアントがインタラクティブリサイズを無効にしているとき `false`。 */
  readonly isResizable: import("./signals").ReadonlySignal<boolean>;
  /** `true` for child windows (dialogs, etc.) that are transient for another toplevel. / 別のトップレベルに対してトランジェントな子ウィンドウ（ダイアログ等）のとき `true`。 */
  readonly isTransient: import("./signals").ReadonlySignal<boolean>;
  /** `id` of the parent window if this is a transient, otherwise `undefined`. / トランジェントの場合は親ウィンドウの `id`、そうでない場合は `undefined`。 */
  readonly parentId: import("./signals").ReadonlySignal<string | undefined>;
  /** Window icon provided by the client or resolved from the desktop entry. / クライアントまたはデスクトップエントリから提供されるウィンドウアイコン。 */
  readonly icon: import("./signals").ReadonlySignal<WindowIcon | undefined>;
  /** Current pointer/drag interaction state for use in composition code. / 合成コードで使用するポインター・ドラッグの現在のインタラクション状態。 */
  readonly interaction: import("./signals").ReadonlySignal<WindowCompositionInteractionSnapshot>;
  /** Ask the client to close the window. / クライアントにウィンドウを閉じるよう要求します。 */
  close(): void;
  /** Ask the client to maximize. / クライアントに最大化を要求します。 */
  maximize(): void;
  /** Ask the client to unmaximize. / クライアントに最大化解除を要求します。 */
  unmaximize(): void;
  /** Ask the client to minimize. / クライアントに最小化を要求します。 */
  minimize(): void;
  /** Ask the client to enter fullscreen. / クライアントにフルスクリーン移行を要求します。 */
  fullscreen(): void;
  /** Ask the client to leave fullscreen. / クライアントにフルスクリーン解除を要求します。 */
  unfullscreen(): void;
  /**
   * Give keyboard focus to this window. Raises the window and notifies
   * `COMPOSITOR.event.onFocus` listeners.
   * このウィンドウにキーボードフォーカスを与えます。ウィンドウを前面に出し、
   * `COMPOSITOR.event.onFocus` リスナーに通知します。
   */
  focus(): void;
  /** Schedule a managed-window animation (position, size, etc.) via the SSD pipeline. / SSD パイプライン経由でマネージドウィンドウのアニメーション（位置・サイズ等）をスケジュールします。 */
  scheduleAnimation(options: ManagedWindowScheduleAnimationOptions): void;
  /** Cancel a running animation on this window. Pass a channel name to cancel only that channel. / 実行中のアニメーションをキャンセルします。チャンネル名を渡すとそのチャンネルのみキャンセルします。 */
  cancelAnimation(channel?: string): void;
  /**
   * Set how long the compositor waits before destroying the window surface after
   * the close sequence begins. Use this to match the close animation duration.
   * 閉じるシーケンス開始後、コンポジターがウィンドウサーフェスを破棄するまでの
   * 待機時間を設定します。閉じるアニメーションの長さに合わせるために使います。
   */
  setCloseAnimationDuration(durationMs: number): void;
  /** `true` if this window is running under XWayland (X11 compatibility layer). / XWayland（X11 互換レイヤー）上で動作するウィンドウのとき `true`。 */
  isXWayland(): boolean;
}

/**
 * A mapped layer-shell surface (bars, docks, overlays, wallpapers, etc.).
 * Reactive properties are `ReadonlySignal<T>` and automatically invalidate any
 * composition code that reads them.
 * マップされたレイヤーシェルサーフェス（バー・ドック・オーバーレイ・壁紙等）。
 * リアクティブなプロパティは `ReadonlySignal<T>` で、読み取った合成コードを
 * 自動的に無効化します。
 *
 * @example In an effect factory / エフェクトファクトリー内で
 * ```ts
 * // panelBlur = compileLayerEffect({ input: backdropSource(), pipeline: [...] })
 * COMPOSITOR.effect.layer = (layer) =>
 *   layer.namespace.value === "bar" ? { behind: panelBlur } : {};
 * ```
 */
export interface WaylandLayer {
  /** Stable string identifier unique to this layer for its lifetime. / レイヤーのライフタイムを通じてユニークな安定した識別子。 */
  readonly id: string;
  /** Reactive namespace string set by the client (e.g. `"bar"`, `"dock"`). / クライアントが設定するリアクティブな名前空間文字列（例: `"bar"`、`"dock"`）。 */
  readonly namespace: import("./signals").ReadonlySignal<string | undefined>;
  /** Reactive z-layer kind: `"background" | "bottom" | "top" | "overlay"`. / リアクティブな z レイヤー種別。 */
  readonly layer: import("./signals").ReadonlySignal<WaylandLayerKind>;
  /** Reactive name of the output this surface is placed on. / このサーフェスが配置される出力名（リアクティブ）。 */
  readonly outputName: import("./signals").ReadonlySignal<string>;
  /** Current logical position and size in global compositor coordinates. / グローバル座標での現在の論理的な位置とサイズ。 */
  readonly position: LayerPosition;
  /** Reactive anchor edges. / リアクティブなアンカーエッジ。 */
  readonly anchor: import("./signals").ReadonlySignal<WaylandLayerAnchor>;
  /** Reactive exclusive-zone request. / リアクティブな排他ゾーン要求。 */
  readonly exclusiveZone: import("./signals").ReadonlySignal<WaylandLayerExclusiveZone>;
  /** Reactive explicit exclusive edge (layer-shell v5+). `null` when not set. / リアクティブな明示的排他エッジ（レイヤーシェル v5+）。未設定時は `null`。 */
  readonly exclusiveEdge: import("./signals").ReadonlySignal<WaylandLayerEdge | null>;
  /** Reactive margin insets in logical pixels. / 論理ピクセル単位のリアクティブなマージンインセット。 */
  readonly margin: import("./signals").ReadonlySignal<WaylandLayerMargin>;
  /** Reactive keyboard interactivity mode. / リアクティブなキーボードインタラクティビティモード。 */
  readonly keyboardInteractivity: import("./signals").ReadonlySignal<WaylandLayerKeyboardInteractivity>;
  /** Reactive size the client requested. / クライアントが要求したリアクティブなサイズ。 */
  readonly desiredSize: import("./signals").ReadonlySignal<WaylandLayerDesiredSize>;
  /** Animation controller for this layer surface. / このレイヤーサーフェス用のアニメーションコントローラー。 */
  readonly animation: import("./animation").AnimationController;
}

export interface WindowPosition {
  x: number;
  y: number;
  width: number;
  height: number;
}

export interface WindowSize {
  width: number;
  height: number;
}

export interface WindowSizeConstraints {
  min?: WindowSize;
  max?: WindowSize;
}

export interface LayerPosition {
  x: number;
  y: number;
  width: number;
  height: number;
}

/**
 * GPU transform applied to a window during composition. Each field accepts a
 * plain value or a `ReadonlySignal<number>` for reactive animation.
 * 合成時にウィンドウに適用される GPU トランスフォーム。各フィールドは固定値または
 * `ReadonlySignal<number>` によるリアクティブなアニメーションを受け付けます。
 *
 * @example Animate scale + opacity on open / オープン時にスケールと不透明度をアニメーション
 * ```ts
 * COMPOSITOR.event.onOpen((window) => {
 *   window.animation.start(openVar, { from: 0, to: 1, duration: ms(180) });
 * });
 * COMPOSITOR.window.composition = (window) => {
 *   const t = window.animation.variable(openVar);
 *   window.transform.scaleX = t((x) => 0.85 + x * 0.15);
 *   window.transform.scaleY = t((x) => 0.85 + x * 0.15);
 *   window.transform.opacity = t;
 *   // ...
 * };
 * ```
 */
export interface WindowTransform {
  /** Transform origin for scale operations. Defaults to the window center. / スケール操作のトランスフォーム原点。デフォルトはウィンドウ中央。 */
  origin: MaybeSignal<TransformOrigin>;
  /** Horizontal translation in logical pixels. / 論理ピクセル単位の水平移動量。 */
  translateX: MaybeSignal<number>;
  /** Vertical translation in logical pixels. / 論理ピクセル単位の垂直移動量。 */
  translateY: MaybeSignal<number>;
  /** Horizontal scale factor (`1.0` = no scale). / 水平スケール係数（`1.0` = スケールなし）。 */
  scaleX: MaybeSignal<number>;
  /** Vertical scale factor (`1.0` = no scale). / 垂直スケール係数（`1.0` = スケールなし）。 */
  scaleY: MaybeSignal<number>;
  /** Opacity from `0.0` (transparent) to `1.0` (opaque). / 不透明度（`0.0` = 透明、`1.0` = 不透明）。 */
  opacity: MaybeSignal<number>;
}

export interface TransformOrigin {
  x: MaybeSignal<number>;
  y: MaybeSignal<number>;
}

export interface ManagedWindowRect {
  x: MaybeSignal<number>;
  y: MaybeSignal<number>;
  width: MaybeSignal<number>;
  height: MaybeSignal<number>;
}

export interface ManagedWindowPoint {
  x: MaybeSignal<number>;
  y: MaybeSignal<number>;
}

export type ManagedWindowAnimationMode =
  "override" | "add" | "sub" | "multiply";

export type ManagedWindowAnimationEasing =
  | "linear"
  | import("./easing").EasingFunction
  | { kind: "linear" }
  | {
      kind: "cubicBezier";
      x1: number;
      y1: number;
      x2: number;
      y2: number;
    };

export interface ManagedWindowRectAnimationOptions {
  from?: ManagedWindowRect;
  to: ManagedWindowRect;
  duration: number;
  easing?: ManagedWindowAnimationEasing;
  mode?: Exclude<ManagedWindowAnimationMode, "multiply">;
}

export interface ManagedWindowPointAnimationOptions {
  from?: ManagedWindowPoint;
  to: ManagedWindowPoint;
  duration: number;
  easing?: ManagedWindowAnimationEasing;
  mode?: Exclude<ManagedWindowAnimationMode, "multiply">;
}

export interface ManagedWindowScalarAnimationOptions {
  from?: MaybeSignal<number>;
  to: MaybeSignal<number>;
  duration: number;
  easing?: ManagedWindowAnimationEasing;
  mode?: ManagedWindowAnimationMode;
}

export interface ManagedWindowScheduleAnimationOptions {
  channel?: string;
  rect?: ManagedWindowRectAnimationOptions;
  offset?: ManagedWindowPointAnimationOptions;
  opacity?: ManagedWindowScalarAnimationOptions;
}

export interface ManagedWindowTransform {
  origin?: MaybeSignal<TransformOrigin>;
  translateX?: MaybeSignal<number>;
  translateY?: MaybeSignal<number>;
  scale?: MaybeSignal<number>;
  scaleX?: MaybeSignal<number>;
  scaleY?: MaybeSignal<number>;
}

export interface ManagedWindowState {
  managed: boolean;
  rect?: WindowPosition;
  workspace?: string | number;
  visibleOutputs?: string[] | null;
  visible: boolean;
  idle: boolean;
  interactive: boolean;
  forceRectSize: boolean;
  tiled: boolean;
  /**
   * Per-window tearing permission. `undefined` means "unspecified" — the compositor falls back
   * to the client's `wp_tearing_control` hint. A concrete value overrides that hint.
   */
  allowTearing?: boolean;
  zIndex?: number;
  transform: WindowTransform;
}

export type PrimitiveChild = string | number;
export type WindowIcon = string | { name?: string; bytes?: Uint8Array };

export interface WindowCompositionInteractionSnapshot {
  hoveredIds: string[];
  activeIds: string[];
}

export type InteractionChangeHandler = (state: boolean) => void;

export interface CompositionElementNode {
  kind: "element";
  type: CompositionNodeType;
  key: string | number | null;
  props: Record<string, unknown>;
  children: CompositionChild[];
}

export type CompositionChild = CompositionElementNode | PrimitiveChild;
export type CompositionRenderable =
  CompositionChild | null | undefined | false | true;

export type CompositionNodeType =
  | "Box"
  | "Label"
  | "Button"
  | "AppIcon"
  | "Image"
  | "ShaderEffect"
  | "ManagedWindow"
  | "Window"
  | "WindowBorder"
  | "Fragment";

/**
 * Base props inherited by all composition components.
 * すべての合成コンポーネントが継承するベース props。
 */
export interface ComponentProps {
  children?: CompositionRenderable | CompositionRenderable[];
  /** Called with `true` when the pointer enters, `false` when it leaves. / ポインターが入ったとき `true`、出たとき `false` で呼ばれます。 */
  onHoverChange?: InteractionChangeHandler;
  /** Called with `true` when pressed, `false` when released. / 押下で `true`、離したとき `false` で呼ばれます。 */
  onActiveChange?: InteractionChangeHandler;
}

export type Component<TProps extends ComponentProps = ComponentProps> = (
  props: TProps,
) => CompositionRenderable;

export type Direction = "row" | "column" | "horizontal" | "vertical";
export type AlignItems = "start" | "center" | "end" | "stretch";
export type JustifyContent = "start" | "center" | "end" | "space-between";
export type FontWeight = "normal" | "medium" | "semibold" | "bold" | number;
export type FontFamily = string | string[];
export type NoiseKind = "salt";
export type BlendMode = "normal" | "add" | "screen" | "multiply";
export interface OnSourceDamageBoxInvalidationHandle {
  kind: "on-source-damage-box";
  /** Logical padding around the visible rect used for source-damage intersection. */
  damagePadding: MaybeSignal<number>;
}

export interface AlwaysInvalidationHandle {
  kind: "always";
}

export type AutomaticEffectInvalidationPolicyHandle =
  OnSourceDamageBoxInvalidationHandle | AlwaysInvalidationHandle;

export interface ManualInvalidationHandle {
  kind: "manual";
  dirtyWhen: MaybeSignal<boolean>;
  base?: AutomaticEffectInvalidationPolicyHandle;
}

export type EffectInvalidationPolicyHandle =
  AutomaticEffectInvalidationPolicyHandle | ManualInvalidationHandle;
export type ShaderUniformScalar = MaybeSignal<number>;
export type ShaderUniformVec2 = readonly [
  ShaderUniformScalar,
  ShaderUniformScalar,
];
export type ShaderUniformVec3 = readonly [
  ShaderUniformScalar,
  ShaderUniformScalar,
  ShaderUniformScalar,
];
export type ShaderUniformVec4 = readonly [
  ShaderUniformScalar,
  ShaderUniformScalar,
  ShaderUniformScalar,
  ShaderUniformScalar,
];
export type ShaderUniformArrayElement = "float" | "vec2" | "vec3" | "vec4";
export type ShaderUniformArrayValues<Element extends ShaderUniformArrayElement> =
  Element extends "float"
    ? readonly ShaderUniformScalar[]
    : Element extends "vec2"
      ? readonly ShaderUniformVec2[]
      : Element extends "vec3"
        ? readonly ShaderUniformVec3[]
        : readonly ShaderUniformVec4[];

export interface ShaderUniformArrayHandle<
  Element extends ShaderUniformArrayElement = ShaderUniformArrayElement,
> {
  kind: "uniform-array";
  element: Element;
  /** The array length is structural; changing it rebuilds the shader binding. */
  values: MaybeSignal<ShaderUniformArrayValues<Element>>;
}

export type ShaderUniformValue =
  | ShaderUniformScalar
  | ShaderUniformVec2
  | ShaderUniformVec3
  | ShaderUniformVec4
  | ShaderUniformArrayHandle;
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
  /** Additional sampler2D uniforms, keyed by their GLSL uniform names. */
  textures?: Record<string, EffectInputHandle>;
}

export interface ShaderInputHandle {
  kind: "shader-input";
  shader: ShaderModuleHandle;
  uniforms?: ShaderUniformMap;
  /** Additional sampler2D uniforms, keyed by their GLSL uniform names. */
  textures?: Record<string, EffectInputHandle>;
}

export interface BackdropSourceHandle {
  kind: "backdrop-source";
}

export interface XrayBackdropSourceHandle {
  kind: "xray-backdrop-source";
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
  | XrayBackdropSourceHandle
  | ShaderInputHandle
  | ImageSourceHandle
  | NamedTextureHandle
  | WindowSourceHandle
  | LayerSourceHandle
  | PopupSourceHandle;

export type EffectStageHandle =
  | ShaderStageHandle
  | NoiseStageHandle
  | DualKawaseBlurStageHandle
  | SaveStageHandle
  | BlendStageHandle
  | UnitStageHandle;

/**
 * How the alpha channel of the effect's output is treated when the result is
 * composited onto the screen.
 *
 * - `"opaque"` (default): force alpha to 1.0 at the end of the pipeline.
 *   Backdrop captures are cleared to transparent black and the blur chain
 *   smears uncovered border texels inward, so the alpha of a plain backdrop
 *   pipeline is noise rather than signal; forcing it opaque hides dark halos
 *   and see-through fringes at the blur edges. Correct for the common
 *   "frosted glass" case where the backdrop is already-composited screen
 *   content.
 * - `"preserve"`: keep the pipeline's alpha output intact. For pipelines
 *   that intentionally produce transparency (e.g. masking the blur against a
 *   layer's own alpha). Opting in makes the pipeline responsible for
 *   producing meaningful alpha everywhere, including the blur edge regions.
 *
 * This is an explicit declaration; the compositor never infers it from the
 * pipeline contents (such as whether a layer source is referenced), so adding
 * a texture input never silently changes edge-artifact handling.
 */
export type EffectAlphaMode = "opaque" | "preserve";

export interface CompiledEffectHandle {
  kind: "compiled-effect";
  input: EffectInputHandle;
  /**
   * Logical padding added around the visible content while capturing and
   * processing backdrop textures. Every pipeline stage sees the padded
   * texture; only the final result is cropped back to the content rect.
   */
  capturePadding: MaybeSignal<number>;
  invalidate: EffectInvalidationPolicyHandle;
  pipeline: EffectStageHandle[];
  /** Output alpha handling. Defaults to `"opaque"`. See {@link EffectAlphaMode}. */
  alpha?: EffectAlphaMode;
}

export interface WindowSourceHandle {
  kind: "window-source";
  include: "full" | "root-surface";
}

export interface LayerSourceHandle {
  kind: "layer-source";
  include: "full" | "root-surface";
}

export type LayerEffectInputHandle =
  LayerSourceHandle | BackdropSourceHandle | XrayBackdropSourceHandle;

/** The popup's own rendered content as an effect input. */
export interface PopupSourceHandle {
  kind: "popup-source";
  include: "full" | "root-surface";
}

/**
 * Inputs allowed for popup effects. Note that a backdrop input on the
 * `behind` slot must be resolvable from the framebuffer at draw time. It may
 * additionally use popupSource() as a named shader texture, but cannot use
 * xray/window/layer sources: popups render inline with their parent's element
 * stream, so there is no offline "scene below the popup" capture path.
 */
export type PopupEffectInputHandle = PopupSourceHandle | BackdropSourceHandle;

export type EffectOutsets =
  | number
  | {
      left?: number;
      right?: number;
      top?: number;
      bottom?: number;
    };

export interface WindowEffectHandle {
  kind: "window-effect";
  effect: CompiledEffectHandle;
  outsets?: EffectOutsets;
}

export interface WindowEffectAssignment {
  behind?: WindowEffectHandle | null;
  behindRootSurface?: WindowEffectHandle | null;
  inFront?: WindowEffectHandle | null;
  replace?: WindowEffectHandle | null;
}

export interface LayerEffectHandle {
  kind: "layer-effect";
  effect: CompiledEffectHandle;
  outsets?: EffectOutsets;
}

export interface LayerEffectAssignment {
  /** Backdrop inputs are supported in this slot for per-layer background effects. */
  behind?: LayerEffectHandle | null;
  behindRootSurface?: LayerEffectHandle | null;
  inFront?: LayerEffectHandle | null;
  replace?: LayerEffectHandle | null;
}

export interface PopupEffectHandle {
  kind: "popup-effect";
  effect: CompiledEffectHandle;
  outsets?: EffectOutsets;
}

export interface PopupEffectAssignment {
  /**
   * Drawn between the popup and the content below it. Accepts popupSource()
   * inputs (e.g. a drop shadow derived from the popup's own alpha) or a
   * framebuffer-resolvable backdrop effect, optionally sampling popupSource()
   * as a named shader texture for masking.
   */
  behind?: PopupEffectHandle | null;
  behindRootSurface?: PopupEffectHandle | null;
  /** Drawn on top of the popup. popupSource() inputs only. */
  inFront?: PopupEffectHandle | null;
  /** Replaces the popup's own rendering. popupSource() inputs only. */
  replace?: PopupEffectHandle | null;
}

/**
 * A mapped xdg_popup, delivered to `COMPOSITOR.effect.popup` so effects
 * can be assigned per popup. Covers popups attached to layer-shell surfaces
 * (`parentKind: "layer"`) and to toplevel windows (`parentKind: "window"`);
 * use `parentKind` to discriminate if an effect should only apply to one
 * of them. For window popups, effects are composited while the parent
 * window's visual transform is identity — during window animations the popup
 * temporarily falls back to its raw rendering.
 */
export interface WaylandPopup {
  readonly id: string;
  /** Runtime id of the root surface this popup belongs to. */
  readonly parentId: string;
  readonly parentKind: "layer" | "window";
  readonly outputName: string;
  /** Output-local logical rect of the popup's geometry box. */
  readonly position: { x: number; y: number; width: number; height: number };
}

/**
 * Assigns GPU-composited visual effects to the four layers of the scene graph.
 * シーングラフの 4 つのレイヤーに GPU 合成エフェクトを割り当てます。
 *
 * All slots are independent — set only the ones you need.
 * 各スロットは独立しています。必要なものだけ設定してください。
 */
export interface CompositorEffectConfig {
  /**
   * Effect rendered behind the regions that clients request through the
   * `ext-background-effect-v1` Wayland protocol (a `blur_region` on their
   * surface). This is NOT a global full-screen backdrop: a window or layer-shell
   * surface opts in by declaring a background-effect region, and the compositor
   * renders this effect behind that region only. Set to `null` to disable.
   * クライアントが `ext-background-effect-v1` Wayland プロトコルで要求した領域
   * （サーフェスの `blur_region`）の背後に描画されるエフェクト。画面全体の背景では
   * ありません。ウィンドウやレイヤーシェルサーフェスが背景エフェクト領域を宣言して
   * オプトインし、コンポジターがその領域の背後にのみこのエフェクトを描画します。
   * `null` で無効化。
   *
   * @example
   * ```ts
   * COMPOSITOR.effect.background_effect = compileEffect({
   *   input: backdropSource(),
   *   pipeline: [dualKawaseBlur({ radius: 4, passes: 2 })],
   * });
   * ```
   */
  background_effect: CompiledEffectHandle | null;
  /**
   * Per-window effect factory. Called once per mapped toplevel; return `null`
   * to apply no effect to that window.
   * マップされたトップレベルウィンドウごとに1回呼ばれるエフェクトファクトリー。
   * そのウィンドウにエフェクトを適用しない場合は `null` を返します。
   *
   * An assignment places an effect in a slot relative to the surface:
   * `behind`, `behindRootSurface`, `inFront`, or `replace`. Each takes a handle
   * from `compileWindowEffect(...)`.
   * 割り当ては、サーフェスに対する位置（`behind`・`behindRootSurface`・`inFront`・
   * `replace`）にエフェクトを置きます。各スロットには `compileWindowEffect(...)` の
   * ハンドルを渡します。
   *
   * @example
   * ```ts
   * // frostedGlass = compileWindowEffect({ input: windowSource(), pipeline: [...] })
   * COMPOSITOR.effect.window = (window) =>
   *   window.isFullscreen() ? null : { behind: frostedGlass };
   * ```
   */
  window?: (window: WaylandWindow) => WindowEffectAssignment | null;
  /**
   * Per-layer-shell-surface effect factory.
   * レイヤーシェルサーフェスごとのエフェクトファクトリー。
   *
   * @example
   * ```ts
   * // panelBlur = compileLayerEffect({ input: backdropSource(), pipeline: [...] })
   * COMPOSITOR.effect.layer = (layer) =>
   *   layer.namespace() === "no_blur" ? {} : { behind: panelBlur };
   * ```
   */
  layer?: (layer: WaylandLayer) => LayerEffectAssignment | null;
  /**
   * Per-popup effect factory. Covers both window-attached and layer-attached
   * popups; use `parentKind` to discriminate if needed.
   * ウィンドウ・レイヤー両方に付いたポップアップごとのエフェクトファクトリー。
   * 必要に応じて `parentKind` で区別できます。
   *
   * @example
   * ```ts
   * // tooltipBlur = compilePopupEffect({ input: backdropSource(), pipeline: [...] })
   * COMPOSITOR.effect.popup = (popup) =>
   *   popup.parentKind === "window" ? {} : { behind: tooltipBlur };
   * ```
   */
  popup?: (popup: WaylandPopup) => PopupEffectAssignment | null;
}

/**
 * The surface a `COMPOSITOR.rendering.surfacePolicy` callback is deciding
 * about. Currently only popups are delivered; the union will grow to layer
 * and window surfaces without breaking existing callbacks.
 * `COMPOSITOR.rendering.surfacePolicy` コールバックが判断対象とするサーフェス。
 * 現在はポップアップのみ配送されます。将来レイヤー・ウィンドウが union に
 * 追加されても既存コールバックは壊れません。
 */
export type SurfacePolicyTarget = { kind: "popup" } & WaylandPopup;

/**
 * Per-surface rendering policy. All fields optional; omitted fields keep the
 * compositor default.
 * サーフェス単位のレンダリングポリシー。省略したフィールドはコンポジターの
 * デフォルトのまま。
 */
export interface SurfacePolicy {
  /**
   * How to treat the opaque region the client declared with
   * `wl_surface.set_opaque_region`.
   *
   * - `"trust"` (default): honor the declaration. Opaque areas may occlude
   *   content composited behind the surface and are drawn without blending.
   * - `"ignore"`: discard the declaration and treat the whole surface as
   *   potentially transparent. Use this for clients that over-declare — e.g.
   *   GTK3 tooltips claim their full rect opaque despite transparent rounded
   *   corners, which paints the corners as a solid fill and culls any
   *   `behind` effect.
   *
   * クライアントが `wl_surface.set_opaque_region` で申告した不透明領域の扱い。
   * `"trust"`（デフォルト）は申告を尊重。`"ignore"` は申告を破棄して
   * サーフェス全体を透明の可能性ありとして扱います（GTK3 ツールチップのような
   * 過剰申告クライアント向け）。
   */
  opaqueRegion?: "trust" | "ignore";
}

/**
 * Rendering policy configuration under `COMPOSITOR.rendering`.
 * `COMPOSITOR.rendering` 配下のレンダリングポリシー設定。
 */
export interface CompositorRenderingConfig {
  /**
   * Per-surface policy factory. Evaluated when the surface set changes (and
   * on config hot-reload) and cached — signals are resolved once, not
   * subscribed. Return `null` (or omit fields) for compositor defaults.
   * サーフェスごとのポリシーファクトリー。サーフェス集合の変化時と設定の
   * ホットリロード時に評価されてキャッシュされます（signal は購読されず
   * 一度だけ解決）。デフォルトのままにするなら `null` を返します。
   */
  surfacePolicy?: (surface: SurfacePolicyTarget) => SurfacePolicy | null;
}

export interface OutputMode {
  width: number;
  height: number;
  refreshRate: number;
}

export type OutputResolutionPreference =
  | "best"
  | {
      width: number;
      height: number;
      refreshRate?: number;
    };

export type OutputPositionPreference =
  | "auto"
  | {
      x: number;
      y: number;
    };

export interface OutputExtendConfigEntry {
  mode?: "extend";
  resolution?: OutputResolutionPreference;
  position?: OutputPositionPreference;
  scale?: number;
}

export interface OutputDisabledConfigEntry {
  mode: "disabled";
}

export interface OutputMirrorConfigEntry {
  mode: "mirror";
  source: string;
}

export type OutputConfigEntry =
  OutputExtendConfigEntry | OutputDisabledConfigEntry | OutputMirrorConfigEntry;

export type DisplayConfigDraft = Record<string, OutputConfigEntry | null>;

export interface OutputStateSnapshot {
  name?: string;
  description?: string;
  make?: string;
  model?: string;
  serial?: string;
  connector?: string;
  enabled?: boolean;
  resolution?: OutputMode;
  position: {
    x: number;
    y: number;
  };
  scale: number;
  availableModes: OutputMode[];
}

export interface OutputInfo extends OutputStateSnapshot {
  name: string;
  enabled: boolean;
}

export interface OutputConfigureContext {
  connected: OutputInfo[];
  outputs: OutputInfo[];
  current: Record<string, OutputInfo>;
}

export type OutputConfigureFactory = (
  context: OutputConfigureContext,
) => DisplayConfigDraft;

/**
 * Read-only view onto connected outputs and the current display configuration.
 * Lets config code query output geometry and register a factory that produces a
 * `DisplayConfigDraft` whenever the connected-output set changes.
 * 接続中の出力と現在のディスプレイ設定への読み取り専用ビュー。出力のジオメトリを
 * 取得したり、接続中の出力セットが変わるたびに `DisplayConfigDraft` を返す
 * ファクトリーを登録したりできます。
 *
 * @example Configure multi-monitor layout / マルチモニターレイアウトを設定
 * ```ts
 * COMPOSITOR.output.configure((ctx) => ({
 *   "DP-1":  { resolution: { width: 2560, height: 1440, refreshRate: 144 } },
 *   "eDP-1": { resolution: "best", scale: 2 },
 * }));
 * ```
 *
 * @example Read output info / 出力情報を読む
 * ```ts
 * const hz = COMPOSITOR.output.get("DP-1")?.resolution?.refreshRate;
 * const [primary] = COMPOSITOR.output.outputs;
 * ```
 */
export interface OutputController {
  /** Names of all currently connected and enabled outputs. / 接続・有効な出力名の一覧。 */
  readonly list: string[];
  /** Snapshot of every connected output. / 接続中の全出力のスナップショット。 */
  readonly outputs: OutputInfo[];
  /** Snapshots keyed by output name. / 出力名をキーとしたスナップショット。 */
  readonly current: Record<string, OutputInfo>;
  /** Returns the snapshot for `outputName`, or `undefined` if not found. / 指定名の出力スナップショット。見つからない場合は `undefined`。 */
  get(outputName: string): OutputInfo | undefined;
  /** Returns the first output matching `predicate`. / `predicate` に一致する最初の出力を返します。 */
  find(predicate: (output: OutputInfo) => boolean): OutputInfo | undefined;
  /** All DRM modes reported by the driver for `outputName`. / ドライバーが報告する全 DRM モード。 */
  availableModes(outputName: string): OutputMode[];
  /**
   * Registers a factory that the compositor calls whenever the set of connected
   * outputs changes. The factory returns a `DisplayConfigDraft` (a map of output
   * name → config) describing the desired layout.
   * 接続中の出力セットが変わるたびにコンポジターが呼ぶファクトリーを登録します。
   * ファクトリーは希望のレイアウトを表す `DisplayConfigDraft` を返します。
   */
  configure(factory: OutputConfigureFactory): void;
  /**
   * Re-runs all registered `configure` factories immediately.
   * Useful after programmatically changing display state.
   * 登録済みの `configure` ファクトリーを即時再実行します。
   */
  reconfigure(): void;
}

export interface WorkspaceConfigEntry {
  /** Stable protocol id. / protocol 上の安定 ID。 */
  id: string;
  /** Human-readable workspace label. / 表示用のワークスペース名。 */
  name: string;
  /** Optional N-dimensional coordinates. / 任意の N 次元座標。 */
  coordinates?: number[];
  /** Whether this workspace is active in its group. / グループ内でアクティブか。 */
  active?: boolean;
  /** Whether this workspace requests attention. / 注意要求状態か。 */
  urgent?: boolean;
  /** Whether bars should hide this workspace. / バー側で非表示扱いにするか。 */
  hidden?: boolean;
}

export interface WorkspaceGroupConfig {
  /** Stable group id, usually an output name. / 安定したグループ ID。通常は出力名。 */
  id: string;
  /** Output names assigned to this workspace group. / このグループに属する出力名。 */
  outputs: string[];
  /** Workspaces in this group. / このグループ内のワークスペース。 */
  workspaces: WorkspaceConfigEntry[];
}

export interface WorkspaceConfig {
  groups: WorkspaceGroupConfig[];
}

export type WorkspaceConfigureFactory = () => WorkspaceConfig;

export interface WorkspaceActivateEvent {
  groupId?: string;
  workspaceId: string;
}

export interface WorkspaceController {
  /**
   * Registers a factory that describes the compositor workspace model exposed
   * through `ext_workspace_manager_v1`.
   * `ext_workspace_manager_v1` で公開するワークスペースモデルを返す
   * ファクトリーを登録します。
   */
  configure(factory: WorkspaceConfigureFactory): void;
  /**
   * Re-runs the registered workspace factory immediately.
   * 登録済みのワークスペースファクトリーを即時再実行します。
   */
  reconfigure(): void;
  event: {
    /**
     * Called when an external client such as Waybar asks to activate a
     * workspace.
     * Waybar などの外部クライアントがワークスペース activate を要求したときに
     * 呼ばれます。
     */
    onActivate(listener: (event: WorkspaceActivateEvent) => void): () => void;
  };
}

/** Logical-pixel insets reserved by exclusive-zone layers on each edge. */
export interface LayerInsets {
  top: number;
  right: number;
  bottom: number;
  left: number;
}

/** Optional filter for usableArea/reservedInsets computations. */
export interface UsableAreaOptions {
  /**
   * If supplied, only layers for which `filter(layer)` returns `true` are
   * considered when summing exclusive zones. Use this to ignore overlays,
   * scope to a namespace, etc.
   */
  filter?: (layer: WaylandLayerSnapshot) => boolean;
}

/**
 * Read-only view onto the layer-shell surfaces the compositor currently has
 * mapped. Snapshots reflect committed protocol state — anchor, exclusive
 * zone, margins, keyboard-interactivity — so config code can answer
 * questions like "how much vertical space is reserved on DP-1?" without
 * tracking lifecycle events itself.
 *
 * The controller is intentionally read-only for now. Per-layer actions
 * (focus, dismiss, …) and compositor-side placement will land on this same
 * surface later — adding them is non-breaking.
 */
export interface LayerController {
  /** Ids of every currently-mapped layer surface. */
  readonly list: string[];
  /** All current layer snapshots, keyed by id. */
  readonly current: Record<string, WaylandLayerSnapshot>;
  /** Snapshots filtered to a single output (matched by `Output.name()`). */
  forOutput(outputName: string): WaylandLayerSnapshot[];
  /**
   * Output rect minus the area reserved by exclusive-zone layers. Returns
   * the usable rectangle in global logical coordinates, suitable for
   * placing windows without occluding bars / docks / panels.
   *
   * Returns `null` when the output isn't registered or has no current
   * resolution (so its logical size can't be derived).
   */
  usableArea(
    outputName: string,
    options?: UsableAreaOptions,
  ): WindowPosition | null;
  /**
   * Per-edge reserved pixels for `outputName`. Useful when you want to do
   * your own arithmetic on the bare numbers (e.g., snap a tile `+8px` below
   * the top bar). Always returns an object; missing outputs or empty layer
   * sets yield zero insets.
   */
  reservedInsets(outputName: string, options?: UsableAreaOptions): LayerInsets;
}

export type EnvValue = string | number | boolean;
export type ProcessEnv = Record<string, EnvValue>;

export interface EnvUpdateOperation {
  key: string;
  value?: string;
}

export interface EnvUpdatePayload {
  operations: EnvUpdateOperation[];
  publish: string[];
}

/**
 * Manages the environment variables inherited by child processes spawned via
 * `COMPOSITOR.process`. Changes take effect for processes started after the
 * call; running processes are unaffected unless you call `publish`.
 * `COMPOSITOR.process` で起動される子プロセスが継承する環境変数を管理します。
 * 変更は呼び出し後に起動されるプロセスに反映されます。実行中のプロセスには
 * `publish` を呼ばない限り影響しません。
 *
 * @example
 * ```ts
 * COMPOSITOR.env.set("QT_QPA_PLATFORM", "wayland;xcb");
 * COMPOSITOR.env.apply({
 *   MOZ_ENABLE_WAYLAND: 1,
 *   GDK_BACKEND: "wayland",
 *   XCURSOR_SIZE: 24,
 * });
 * ```
 */
export interface EnvController {
  /** Set a single environment variable. / 環境変数を 1 つ設定します。 */
  set(key: string, value: EnvValue): void;
  /** Remove an environment variable. / 環境変数を削除します。 */
  unset(key: string): void;
  /** Read the current value of a variable. / 変数の現在値を取得します。 */
  get(key: string): string | undefined;
  /**
   * Bulk set/unset. Pass `null` or `undefined` as a value to unset that key.
   * 一括設定。値に `null` または `undefined` を渡すとその変数を削除します。
   */
  apply(values: Record<string, EnvValue | null | undefined>): void;
  /**
   * Broadcast the current environment to running compositor services. If `keys`
   * is omitted, all currently-set variables are published.
   * 実行中のコンポジターサービスに現在の環境変数をブロードキャストします。
   * `keys` を省略するとすべての設定済み変数を公開します。
   */
  publish(keys?: Iterable<string>): void;
}

/**
 * How a process is launched.
 *
 * - When `command` is a single string, it's run via `/bin/sh -lc <command>`,
 *   so shell features like pipes, redirection and environment expansion work.
 * - When `command` is a string array, it's exec'd directly with no shell
 *   involvement (each element is one argv entry, taken literally).
 */
export interface ProcessCommandSpec {
  command: string | string[];
}

export type ProcessLaunchSpec = ProcessCommandSpec;

export interface ProcessBaseSpec {
  cwd?: string;
  env?: ProcessEnv;
}

export type StartupProcessRunPolicy =
  "once-per-session" | "once-per-config-version";

export type ManagedProcessRestartPolicy = "never" | "on-failure" | "on-exit";

export type ManagedProcessReloadPolicy = "keep-if-unchanged" | "always-restart";

export type StartupOnceSpec = ProcessBaseSpec &
  ProcessLaunchSpec & {
    runPolicy?: StartupProcessRunPolicy;
  };

export type StartupServiceSpec = ProcessBaseSpec &
  ProcessLaunchSpec & {
    restart?: ManagedProcessRestartPolicy;
    reload?: ManagedProcessReloadPolicy;
  };

export type ProcessSpawnSpec = ProcessBaseSpec & ProcessLaunchSpec;

/**
 * Launches and manages processes that are part of the desktop session.
 * デスクトップセッションに属するプロセスの起動と管理を行います。
 *
 * Three launch modes / 3 種類の起動モード:
 * - `once`    — run once at startup (optionally once per config version) / 起動時1回（設定バージョンごとも可）
 * - `service` — keep running with an optional restart policy / オプションの再起動ポリシーで常駐
 * - `spawn`   — fire-and-forget; compositor does not track the process / 追跡なし即時起動
 *
 * @example
 * ```ts
 * // One-shot: start foot server on first session launch
 * COMPOSITOR.process.once("shell", {
 *   command: ["foot", "--server"],
 *   runPolicy: "once-per-session",
 * });
 *
 * // Service: cliphist stays alive and restarts on failure
 * COMPOSITOR.process.service("cliphist", {
 *   command: "wl-paste --watch cliphist store",
 *   restart: "on-failure",
 * });
 *
 * // Spawn: open a terminal on demand
 * COMPOSITOR.process.spawn({ command: ["kitty"] });
 * ```
 */
export interface ProcessController {
  /**
   * Run a command once at session startup. By default runs once per session;
   * set `runPolicy: "once-per-config-version"` to re-run when the config changes.
   * セッション起動時に1回だけ実行します。デフォルトはセッションごとに1回。
   * 設定変更時に再実行したい場合は `runPolicy: "once-per-config-version"` を指定します。
   */
  once(id: string, spec: StartupOnceSpec): void;
  /**
   * Start a long-running service. The compositor monitors the process and
   * restarts it according to `restart` policy.
   * 常駐サービスを起動します。コンポジターがプロセスを監視し、`restart`
   * ポリシーに従って再起動します。
   */
  service(id: string, spec: StartupServiceSpec): void;
  /**
   * Launch a process and forget it. The compositor does not track or restart it.
   * プロセスを起動して追跡しません。コンポジターは再起動も監視もしません。
   */
  spawn(spec: ProcessSpawnSpec): void;
}

export type KeyBindingEventPhase = "press" | "release";

export interface KeyBindingOptions {
  on?: KeyBindingEventPhase;
}

/**
 * Registers compositor-level keyboard shortcuts.
 * コンポジターレベルのキーボードショートカットを登録します。
 *
 * Shortcuts are identified by a string `id` (shown in help UIs). The `shortcut`
 * string uses modifier+key notation such as `"Super+T"` or `"Super+Shift+Left"`.
 * ショートカットは文字列 `id` で識別されます（ヘルプ UI などに表示されます）。
 * `shortcut` は `"Super+T"` や `"Super+Shift+Left"` のような記法を使います。
 *
 * @example
 * ```ts
 * COMPOSITOR.key.bind("launch-terminal", "Super+T", () => {
 *   COMPOSITOR.process.spawn({ command: ["kitty"] });
 * });
 *
 * // Tap binding: fires on Super key release / タップバインド: Superキーリリースで発火
 * COMPOSITOR.key.bind("launcher", "Super", openLauncher, { on: "release" });
 * ```
 */
export interface KeyBindingController {
  /**
   * Register a shortcut. `id` must be unique; registering the same id twice
   * replaces the previous binding.
   * ショートカットを登録します。`id` は一意である必要があります。
   * 同じ `id` を2回登録すると前のバインドが上書きされます。
   */
  bind(
    id: string,
    shortcut: string,
    handler: () => void,
    options?: KeyBindingOptions,
  ): void;
}

/**
 * Compositor-level pointer (mouse/touchpad cursor) configuration.
 * コンポジターレベルのポインター（マウス・タッチパッドカーソル）設定。
 *
 * For per-device settings (acceleration, scroll method, …) use
 * `COMPOSITOR.input.configure` instead.
 * デバイスごとの設定（加速度・スクロール方式など）は `COMPOSITOR.input.configure`
 * を使ってください。
 *
 * @example
 * ```ts
 * // Hold Super and drag any window to move it without grabbing its title bar
 * // Super を押しながら任意のウィンドウをドラッグして移動（タイトルバー不要）
 * COMPOSITOR.pointer.bindWindowMoveModifier("Super");
 * ```
 */
export interface PointerController {
  /**
   * Set the modifier key that lets the user drag any window by holding the
   * modifier and clicking anywhere on the window surface.
   * 指定したモディファイアキーを押しながらウィンドウ上の任意の場所をクリック
   * してドラッグできるようにします。
   */
  bindWindowMoveModifier(modifier: string): void;

  /**
   * Set the modifier key that lets the user resize any window by holding the
   * modifier and right clicking anywhere on the window surface, binding to
   * nearest surface corner.
   * えーと、マウスの右クリック、ウィンドウのサイズ変更
   */
  bindWindowResizeModifier(modifier: string): void;
}

export interface InputDeviceKindFlags {
  keyboard: boolean;
  pointer: boolean;
  touchpad: boolean;
  touch: boolean;
  tabletTool: boolean;
  tabletPad: boolean;
  gesture: boolean;
  switch: boolean;
}

export interface InputDeviceInfo {
  key: string;
  name: string;
  sysname?: string;
  vendor?: number;
  product?: number;
  kind: InputDeviceKindFlags;
}

export type InputAccelProfile = "adaptive" | "flat";
export type InputClickMethod = "buttonAreas" | "clickfinger";
export type InputScrollMethod = "none" | "twoFinger" | "edge" | "onButtonDown";
export type InputTapButtonMap = "leftRightMiddle" | "leftMiddleRight";

export interface KeyboardInputConfig {
  repeatRate?: number;
  repeatDelay?: number;
  rules?: string;
  model?: string;
  layout?: string;
  variant?: string;
  options?: string;
}

export interface PointerInputConfig {
  pointerAccel?: number;
  accelProfile?: InputAccelProfile;
  leftHanded?: boolean;
  naturalScroll?: boolean;
  middleEmulation?: boolean;
}

export interface TouchpadInputConfig extends PointerInputConfig {
  tapToClick?: boolean;
  tapButtonMap?: InputTapButtonMap;
  clickMethod?: InputClickMethod;
  scrollMethod?: InputScrollMethod;
  scrollFactor?: number;
  disableWhileTyping?: boolean;
}

export interface InputDeviceConfig {
  keyboard?: KeyboardInputConfig;
  pointer?: PointerInputConfig;
  touchpad?: TouchpadInputConfig;
}

export interface InputConfigDraft {
  global?: InputDeviceConfig;
  device: Record<string, InputDeviceConfig | null>;
}

export interface InputConfigureContext {
  devices: InputDeviceInfo[];
  current: Record<string, InputDeviceInfo>;
}

export type InputConfigureFactory = (
  input: InputConfigDraft,
  context: InputConfigureContext,
) => void;

/**
 * Provides access to the currently connected input devices and lets config
 * code adjust per-device settings (keyboard layout, touchpad behaviour, pointer
 * acceleration, and so on).
 * 接続中の入力デバイスへのアクセスと、デバイスごとの設定（キーボードレイアウト・
 * タッチパッド挙動・ポインター加速度など）の調整を提供します。
 *
 * `configure` is the main entry-point: mutate the `InputConfigDraft` passed to
 * your factory, then the compositor applies the diff. Call `reconfigure` to
 * re-run all registered factories (e.g. after a device hotplug event).
 * `configure` がメインのエントリーポイントです。ファクトリーに渡された
 * `InputConfigDraft` を変更すると、コンポジターが差分を適用します。
 * デバイスのホットプラグ後など再実行したい場合は `reconfigure` を呼びます。
 *
 * @example Global touchpad + keyboard defaults / グローバルなタッチパッド・キーボードのデフォルト
 * ```ts
 * COMPOSITOR.input.configure((input) => {
 *   input.global = {
 *     touchpad: { tapToClick: true, naturalScroll: true, disableWhileTyping: true },
 *     keyboard: { layout: "us", options: "ctrl:nocaps" },
 *   };
 * });
 * ```
 *
 * @example Per-device override / デバイスごとの上書き
 * ```ts
 * COMPOSITOR.input.configure((input, ctx) => {
 *   for (const device of ctx.devices) {
 *     if (device.kind.touchpad) {
 *       input.device[device.key] = { touchpad: { scrollMethod: "twoFinger" } };
 *     }
 *   }
 * });
 * ```
 */
export interface InputController {
  /** All currently connected input devices. / 接続中の入力デバイス一覧。 */
  readonly devices: InputDeviceInfo[];
  /** Devices keyed by their unique key string. / ユニークなキー文字列をキーとしたデバイス一覧。 */
  readonly current: Record<string, InputDeviceInfo>;
  /** Returns the device for `deviceKey`, or `undefined` if not found. / 指定キーのデバイス。見つからない場合は `undefined`。 */
  get(deviceKey: string): InputDeviceInfo | undefined;
  /** Returns the first device matching `predicate`. / `predicate` に一致する最初のデバイスを返します。 */
  find(
    predicate: (device: InputDeviceInfo) => boolean,
  ): InputDeviceInfo | undefined;
  /**
   * Registers a factory that the compositor calls whenever the set of connected
   * input devices changes. Mutate `input` (an `InputConfigDraft`) to apply
   * global or per-device settings.
   * 入力デバイスのセットが変わるたびにコンポジターが呼ぶファクトリーを登録します。
   * `input`（`InputConfigDraft`）を変更してグローバルまたはデバイスごとの設定を適用します。
   */
  configure(factory: InputConfigureFactory): void;
  /**
   * Re-runs all registered `configure` factories immediately.
   * 登録済みの `configure` ファクトリーを即時再実行します。
   */
  reconfigure(): void;
}

/** Desktop cursor theme and logical size. / デスクトップカーソルのテーマと論理サイズ。 */
export interface CursorConfig {
  /** XCursor theme name. / XCursor テーマ名。 */
  theme: string;
  /** Cursor size in logical pixels. / 論理ピクセル単位のカーソルサイズ。 */
  size: number;
}

/** @internal Wire payload sent to the compositor runtime. */
export interface RuntimeCursorConfigUpdate extends CursorConfig {
  reload: boolean;
}

/**
 * Configures compositor-owned named cursors and exports matching XCursor
 * environment variables for processes started afterwards.
 * コンポジター所有の named cursor を設定し、後から起動するプロセス向けに
 * 対応する XCursor 環境変数も公開します。
 */
export interface CursorController {
  /** Apply a theme and logical size immediately. / テーマと論理サイズを即時適用します。 */
  configure(config: CursorConfig): void;
  /** Reload the current theme and discard decoded cursor caches. / 現在のテーマをキャッシュ破棄して再読込します。 */
  reload(): void;
}

export interface BorderValue {
  px: MaybeSignal<number>;
  color: MaybeSignal<string>;
}

export interface WindowResizeHitArea {
  edgePx?: MaybeSignal<number>;
  cornerPx?: MaybeSignal<number>;
}

export interface WindowBorderInteraction {
  resizeHitArea?: MaybeSignal<number> | WindowResizeHitArea;
}

export type SSDPosition = "relative" | "absolute";
export type SSDOverflow = "visible" | "hidden";
export type SSDPointerEvents = "auto" | "none";

export interface SSDTransform {
  translateX?: MaybeSignal<number>;
  translateY?: MaybeSignal<number>;
  scale?: MaybeSignal<number>;
  scaleX?: MaybeSignal<number>;
  scaleY?: MaybeSignal<number>;
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
  position?: MaybeSignal<SSDPosition>;
  zIndex?: MaybeSignal<number>;
  inset?: MaybeSignal<number>;
  top?: MaybeSignal<number>;
  right?: MaybeSignal<number>;
  bottom?: MaybeSignal<number>;
  left?: MaybeSignal<number>;
  overflow?: MaybeSignal<SSDOverflow>;
  pointerEvents?: MaybeSignal<SSDPointerEvents>;
  transform?: MaybeSignal<SSDTransform>;
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
  borderFit?: MaybeSignal<"normal" | "fit-children">;
  borderRadius?: MaybeSignal<number>;
  visible?: MaybeSignal<boolean>;
  cursor?: MaybeSignal<string>;
  fontSize?: MaybeSignal<number>;
  fontWeight?: MaybeSignal<FontWeight>;
  fontFamily?: MaybeSignal<FontFamily>;
  textAlign?: MaybeSignal<"start" | "center" | "end">;
  lineHeight?: MaybeSignal<number>;
}

/**
 * Props for `<Box/>` — a flexbox-like container that arranges children.
 * 子要素を整列するフレックスボックス風コンテナ `<Box/>` の props。
 *
 * @example Horizontal title bar / 水平タイトルバー
 * ```tsx
 * <Box direction="row" style={{ gap: 8, padding: 4, alignItems: "center" }}>
 *   <AppIcon icon={window.icon} style={{ width: 16, height: 16 }} />
 *   <Label text={window.title} style={{ flexGrow: 1, fontSize: 13 }} />
 *   <Button onClick={windowAction("close")} style={{ width: 12, height: 12 }} />
 * </Box>
 * ```
 */
export interface BoxProps extends ComponentProps {
  /**
   * Layout direction for children. `"row"` / `"horizontal"` → left-to-right;
   * `"column"` / `"vertical"` → top-to-bottom. Defaults to `"row"`.
   * 子要素のレイアウト方向。`"row"` / `"horizontal"` は左→右、`"column"` / `"vertical"` は上→下。
   * デフォルトは `"row"`。
   */
  direction?: Direction;
  /** Split direction for two-panel layouts. / 2 パネルレイアウトの分割方向。 */
  split?: Direction;
  /** Visual styling (size, color, padding, border-radius, etc.). / 視覚スタイル（サイズ・色・パディング・角丸等）。 */
  style?: SSDStyle;
  /** Stable node id for targeted invalidation. / ターゲット無効化のための安定したノード ID。 */
  id?: string;
}

/**
 * Props for `<Label/>` — renders a text string.
 * テキスト文字列を描画する `<Label/>` の props。
 *
 * @example Reactive window title / リアクティブなウィンドウタイトル
 * ```tsx
 * <Label text={window.title} style={{ fontSize: 13, color: "#ffffffcc" }} />
 * ```
 */
export interface LabelProps extends ComponentProps {
  /**
   * The text to display. Accepts a plain string or a `ReadonlySignal<string>`
   * for reactive text that updates automatically.
   * 表示するテキスト。`ReadonlySignal<string>` を渡すとリアクティブに自動更新されます。
   */
  text?: MaybeSignal<string>;
  style?: SSDStyle;
  id?: string;
}

/**
 * Props for `<Button/>` — a pressable region that triggers an action on click.
 * クリックでアクションをトリガーするプレス可能な領域 `<Button/>` の props。
 *
 * @example Close button using a built-in window action / ウィンドウアクションを使った閉じるボタン
 * ```tsx
 * <Button onClick={windowAction("close")} style={{ width: 12, height: 12 }} />
 * ```
 *
 * @example Custom click handler / カスタムクリックハンドラー
 * ```tsx
 * <Button onClick={() => window.maximize()} style={{ width: 12, height: 12 }} />
 * ```
 */
export interface ButtonProps extends ComponentProps {
  style?: SSDStyle;
  id?: string;
  /**
   * Action on click. Pass a callback for custom logic, or a
   * `WindowActionDescriptor` created with `windowAction(...)` for built-in
   * window operations (close, maximize, minimize, fullscreen).
   * クリック時のアクション。カスタムロジックにはコールバック、ウィンドウ操作には
   * `windowAction(...)` で作成した `WindowActionDescriptor` を渡します。
   */
  onClick?: WindowActionDescriptor | (() => void);
}

/**
 * Props for `<AppIcon/>` — renders a window's application icon.
 * ウィンドウのアプリケーションアイコンを描画する `<AppIcon/>` の props。
 *
 * @example
 * ```tsx
 * <AppIcon icon={window.icon} style={{ width: 16, height: 16 }} />
 * ```
 */
export interface AppIconProps extends ComponentProps {
  /**
   * The icon to display. Pass `window.icon` for reactive updates.
   * Accepts a URL string or `{ name?, bytes? }` for named/embedded icons.
   * 表示するアイコン。`window.icon` を渡すとリアクティブに更新されます。
   * URL 文字列または `{ name?, bytes? }` を受け付けます。
   */
  icon?: MaybeSignal<WindowIcon | undefined>;
  style?: SSDStyle;
  id?: string;
}

/** How the image fills its container. `"contain"` fits inside, `"cover"` crops to fill, `"fill"` stretches. / 画像がコンテナを満たす方法。 */
export type ImageFit = "contain" | "cover" | "fill";

/**
 * Props for `<Image/>` — displays an image from a file path or reactive source.
 * ファイルパスまたはリアクティブなソースから画像を表示する `<Image/>` の props。
 *
 * @example Static background image / 静的な背景画像
 * ```tsx
 * <Image src="assets/wallpaper.jpg" fit="cover"
 *   style={{ width: "100%", height: "100%" }} />
 * ```
 */
export interface ImageProps extends ComponentProps {
  /**
   * Path to the image, relative to the config package root. Also accepts a
   * `ReadonlySignal<string>` for reactive image switching.
   * 設定パッケージルートからの相対パス。`ReadonlySignal<string>` を渡すと
   * リアクティブな画像切り替えが可能です。
   */
  src: MaybeSignal<string>;
  style?: SSDStyle;
  /**
   * How the image fills its container:
   * - `"contain"` — scaled to fit entirely inside without cropping
   * - `"cover"` — scaled to fill and cropped to the container shape
   * - `"fill"` — stretched to the exact container size
   */
  fit?: MaybeSignal<ImageFit>;
  id?: string;
}

/**
 * Props for `<ShaderEffect/>` — a container that applies a compiled GPU effect
 * to the region occupied by its children.
 * 子要素が占める領域にコンパイル済み GPU エフェクトを適用するコンテナ
 * `<ShaderEffect/>` の props。
 *
 * @example Frosted-glass title bar / すりガラスタイトルバー
 * ```tsx
 * const frostedGlass = compileEffect({
 *   input: backdropSource(),
 *   pipeline: [dualKawaseBlur({ passes: 3 })],
 * });
 *
 * <ShaderEffect shader={frostedGlass} style={{ height: 32, borderRadius: 8 }}>
 *   <Label text={window.title} style={{ padding: 8 }} />
 * </ShaderEffect>
 * ```
 */
export interface ShaderEffectProps extends ComponentProps {
  /** The compiled effect to render over this node's area. / このノードの領域に描画するコンパイル済みエフェクト。 */
  shader: CompiledEffectHandle;
  /** Layout direction for children (same as `<Box/>`). / 子要素のレイアウト方向（`<Box/>` と同じ）。 */
  direction?: Direction;
  split?: Direction;
  style?: SSDStyle;
  id?: string;
}

/**
 * Props for `<ManagedWindow/>` — the anchor that binds a Wayland window into
 * the compositor's layout system. Controls placement, workspace assignment,
 * visibility, z-order, opacity, transform, and fullscreen tearing.
 *
 * Place exactly one `<ManagedWindow/>` per window in the tree returned by
 * `COMPOSITOR.window.composition`.
 * Wayland ウィンドウをコンポジターのレイアウトシステムに結びつけるアンカー
 * `<ManagedWindow/>` の props。配置・ワークスペース割り当て・表示状態・
 * z オーダー・不透明度・トランスフォーム・フルスクリーンテアリングを制御します。
 *
 * `COMPOSITOR.window.composition` が返すツリー内にウィンドウごとに 1 つ配置します。
 *
 * @example Floating window with border / ボーダー付きフローティングウィンドウ
 * ```tsx
 * COMPOSITOR.window.composition = (window) => (
 *   <ManagedWindow
 *     rect={{ x: window.position.x, y: window.position.y,
 *             width: window.position.width, height: window.position.height }}
 *     zIndex={getZIndex(window)}
 *   >
 *     <WindowBorder style={{ borderRadius: 8 }}>
 *       <ClientWindow />
 *     </WindowBorder>
 *   </ManagedWindow>
 * );
 * ```
 *
 * @example Game window with tearing enabled / テアリング有効のゲームウィンドウ
 * ```tsx
 * <ManagedWindow
 *   rect={...}
 *   allowTearing={window.isFullscreen}
 * >
 *   <ClientWindow />
 * </ManagedWindow>
 * ```
 */
export interface ManagedWindowProps extends ComponentProps {
  /**
   * Logical position and size of the window in global compositor coordinates.
   * Each field is a `MaybeSignal<number>` for reactive layout.
   * グローバル座標でのウィンドウの論理的な位置とサイズ。各フィールドは
   * `MaybeSignal<number>` でリアクティブなレイアウトに対応します。
   */
  rect?: MaybeSignal<ManagedWindowRect>;
  /**
   * Workspace this window belongs to. Accepts a string name or numeric index.
   * このウィンドウが属するワークスペース。文字列名または数値インデックスを受け付けます。
   */
  workspace?: MaybeSignal<string | number>;
  /**
   * Restrict visibility to specific outputs by name. `null` means visible on all
   * outputs (the default).
   * 表示する出力を名前で制限します。`null` はすべての出力で表示（デフォルト）。
   */
  visibleOutputs?: MaybeSignal<string[] | null>;
  /**
   * Whether the window is visible. `false` hides it without unmapping the surface.
   * ウィンドウが表示されているか。`false` にするとサーフェスをアンマップせずに非表示にします。
   */
  visible?: MaybeSignal<boolean>;
  /**
   * When `true`, the window is excluded from focus cycling and treated as
   * background / idle content.
   * `true` にするとフォーカスサイクルから除外され、バックグラウンド・アイドルコンテンツ
   * として扱われます。
   */
  idle?: MaybeSignal<boolean>;
  /**
   * When `false`, the window surface ignores pointer input.
   * `false` にするとウィンドウサーフェスがポインター入力を無視します。
   */
  interactive?: MaybeSignal<boolean>;
  /**
   * When `true`, the compositor enforces `rect.width`/`rect.height` on the
   * client, overriding its own size preferences.
   * `true` にするとコンポジターがクライアントの希望サイズを上書きし、
   * `rect.width`/`rect.height` を強制します。
   */
  forceRectSize?: MaybeSignal<boolean>;
  /**
   * When `true`, the window is in tiled mode (no shadow, no rounded corners by
   * convention). The compositor sends the `tiled` xdg_toplevel state to the client.
   * `true` にするとウィンドウがタイルモードになります（慣例としてシャドウなし・角丸なし）。
   * コンポジターはクライアントに xdg_toplevel の `tiled` 状態を送ります。
   */
  tiled?: MaybeSignal<boolean>;
  /**
   * Whether this window may tear (immediate/async page flips) while it is fullscreen and on the
   * direct-scanout fast path.
   *
   * This is the compositor's source of truth: when set it overrides the client's
   * `wp_tearing_control` hint; when left undefined the compositor falls back to that hint.
   * Tearing still only actually happens while the window is fullscreen, directly scanned out, and
   * the pointer is hidden — so `allowTearing={() => window.isFullscreen.value}` is a typical
   * pattern for games (works for native Wayland and X11/Xwayland clients alike, since it does not
   * require the client to send `wp_tearing_control`).
   */
  allowTearing?: MaybeSignal<boolean>;
  /** Z-order within the scene graph. Higher values render on top. / シーングラフ内の z オーダー。値が大きいほど上に描画されます。 */
  zIndex?: MaybeSignal<number>;
  /** Overall opacity from `0.0` (transparent) to `1.0` (opaque). / 全体の不透明度（`0.0` = 透明、`1.0` = 不透明）。 */
  opacity?: MaybeSignal<number>;
  /**
   * Additional GPU transform (scale, translate, origin) applied to the managed
   * window node. Use `window.transform` for per-window animation-driven transforms instead.
   * このマネージドウィンドウノードに適用する追加の GPU トランスフォーム（スケール・
   * 移動・原点）。アニメーション駆動のトランスフォームには `window.transform` を使います。
   */
  transform?: MaybeSignal<ManagedWindowTransform>;
  id?: string;
}

/**
 * Props for `<ClientWindow/>` (alias: `<Window/>`). Renders the Wayland
 * client's actual surface content. Must be placed inside `<ManagedWindow/>`
 * in the tree returned by `COMPOSITOR.window.composition`.
 *
 * Leaf node — renders the client buffer and does not accept children.
 * Wayland クライアントの実際のサーフェスコンテンツを描画する `<ClientWindow/>`
 * （別名: `<Window/>`）の props。`COMPOSITOR.window.composition` が返すツリー内の
 * `<ManagedWindow/>` の内側に配置します。
 *
 * リーフノードです。クライアントバッファを描画し、子要素は受け付けません。
 *
 * @example Inside a WindowBorder / WindowBorder の内側
 * ```tsx
 * <ManagedWindow rect={...} zIndex={...}>
 *   <WindowBorder
 *     style={{ border: { px: 1, color: "#ffffff20" }, borderRadius: 8 }}
 *   >
 *     <ClientWindow />
 *   </WindowBorder>
 * </ManagedWindow>
 * ```
 *
 * A bare ClientWindow preserves the complete client-owned surface, including
 * CSD shadows and transparent resize margins. To crop it, wrap it in an SSD
 * container with a `border` (or explicit `overflow: "hidden"`).
 * 素の ClientWindow は CSD の影や透明なリサイズマージンを含む、クライアント所有の
 * サーフェス全体を保持します。切り取る場合は `border`（または明示的な
 * `overflow: "hidden"`）を持つ SSD コンテナで囲みます。
 */
export interface ClientWindowProps extends ComponentProps {
  id?: string;
  children?: never;
}

/** Alias for `ClientWindowProps`. / `ClientWindowProps` の別名。 */
export type WindowProps = ClientWindowProps;

/**
 * Props for `<WindowBorder/>` — a chrome container placed around
 * `<ClientWindow/>` that handles border rendering and interactive resize areas.
 * `<ClientWindow/>` の周囲に配置し、ボーダー描画とインタラクティブなリサイズ領域を
 * 処理するクロムコンテナ `<WindowBorder/>` の props。
 *
 * @example Rounded border with resize hit areas / リサイズ領域付き角丸ボーダー
 * ```tsx
 * <WindowBorder
 *   style={{ borderRadius: 8, border: { px: 1, color: "#ffffff20" } }}
 *   interaction={{ resizeHitArea: { edgePx: 4, cornerPx: 8 } }}
 * >
 *   <ClientWindow />
 * </WindowBorder>
 * ```
 */
export interface WindowBorderProps extends ComponentProps {
  /** Visual border styling (radius, color, shadow, etc.). / ボーダーの視覚スタイル（角丸・色・シャドウ等）。 */
  style?: SSDStyle;
  /** Interactive resize hit areas. / インタラクティブなリサイズヒット領域。 */
  interaction?: WindowBorderInteraction;
  id?: string;
}

export type WindowCompositionPhase = "preconfigure" | "render";

export interface WindowCompositionContext {
  readonly phase: WindowCompositionPhase;
  readonly isPreview: boolean;
}

export type WindowCompositionFunction = (
  window: WaylandWindow,
  context: WindowCompositionContext,
) => CompositionRenderable;

/**
 * The root API object exposed to ShojiWM config scripts.
 * ShojiWM の設定スクリプトに公開されるルート API オブジェクト。
 *
 * Every field is a scoped controller covering one functional area: events,
 * effects, outputs, input devices, environment, processes, key bindings, and
 * so on. Typically you call methods at module top-level; the compositor then
 * invokes your listeners and effect functions reactively at runtime.
 * 各フィールドはイベント・エフェクト・出力・入力デバイス・環境変数・プロセス・
 * キーバインドなど、1 つの機能領域をカバーするコントローラーです。
 *
 * @example Hot-reload lifecycle / ホットリロードライフサイクル
 * ```ts
 * COMPOSITOR.onEnable((event) => {
 *   const saved = event.restore<{ count: number }>("my-state");
 *   if (saved) count = saved.count;
 * });
 * COMPOSITOR.onDisable((event) => {
 *   event.persist("my-state", { count });
 * });
 * ```
 */
export interface CompositorDefinition {
  /**
   * Lifecycle and window/layer/output/input event bus.
   * ウィンドウ・レイヤー・出力・入力のライフサイクルイベントバス。
   *
   * @example
   * ```ts
   * COMPOSITOR.event.onOpen((window) => console.log("opened", window.id));
   * COMPOSITOR.event.onFocus((window, focused) => {
   *   window.animation.start(focusVar, { to: focused ? 1 : 0, duration: ms(120) });
   * });
   * ```
   */
  event: import("./events").CompositorEventController;
  /**
   * Shorthand for `COMPOSITOR.event.onEnable`. Returns an unsubscribe function.
   * `COMPOSITOR.event.onEnable` のショートハンド。解除関数を返します。
   *
   * @example
   * ```ts
   * COMPOSITOR.onEnable((event) => {
   *   const saved = event.restore<MyState>("key");
   *   if (saved) restore(saved);
   * });
   * ```
   */
  onEnable(listener: import("./events").RuntimeEnableListener): () => void;
  /**
   * Shorthand for `COMPOSITOR.event.onDisable`. Use `event.persist` inside the
   * listener to save state that survives a config hot-reload.
   * `COMPOSITOR.event.onDisable` のショートハンド。リスナー内で `event.persist` を
   * 呼ぶとホットリロードをまたいで状態を保持できます。
   *
   * @example
   * ```ts
   * COMPOSITOR.onDisable((event) => {
   *   event.persist("my-state", snapshot());
   * });
   * ```
   */
  onDisable(listener: import("./events").RuntimeDisableListener): () => void;
  preload: PreloadController;
  /**
   * Scene-graph effect assignments: background, per-window, per-layer, per-popup.
   * シーングラフのエフェクト設定。背景・ウィンドウごと・レイヤーごと・ポップアップごと。
   *
   * @example Background blur / 背景ブラー
   * ```ts
   * COMPOSITOR.effect.background_effect = compileEffect({
   *   input: backdropSource(),
   *   pipeline: [dualKawaseBlur({ radius: 4, passes: 2 })],
   * });
   * ```
   *
   * @example Per-window effect / ウィンドウごとのエフェクト
   * ```ts
   * // frostedGlass = compileWindowEffect({ input: windowSource(), pipeline: [...] })
   * COMPOSITOR.effect.window = (window) =>
   *   window.isFullscreen() ? null : { behind: frostedGlass };
   * ```
   */
  effect: CompositorEffectConfig;
  /**
   * Rendering policies that override how the compositor treats individual
   * surfaces, independent of effects and decorations.
   * エフェクトや装飾から独立して、コンポジターの個別サーフェスの扱いを
   * 上書きするレンダリングポリシー。
   *
   * @example Ignore a lying opaque region / 嘘の opaque region を無視
   * ```ts
   * COMPOSITOR.rendering.surfacePolicy = (surface) =>
   *   surface.kind === "popup" && surface.parentKind === "layer"
   *     ? { opaqueRegion: "ignore" }
   *     : null;
   * ```
   */
  rendering: CompositorRenderingConfig;
  /**
   * Output (monitor) access and display layout configuration.
   * 出力（モニター）へのアクセスとディスプレイレイアウト設定。
   *
   * @example Configure outputs / 出力を設定
   * ```ts
   * COMPOSITOR.output.configure((ctx) => ({
   *   "DP-1":  { resolution: { width: 2560, height: 1440, refreshRate: 144 } },
   *   "eDP-1": { resolution: "best", scale: 2 },
   * }));
   * ```
   *
   * @example Read current output info / 現在の出力情報を取得
   * ```ts
   * const info = COMPOSITOR.output.get("DP-1");
   * console.log(info?.resolution?.refreshRate);
   * ```
   */
  output: OutputController;
  /**
   * Workspace model exposed to external bars through `ext_workspace_manager_v1`.
   * `ext_workspace_manager_v1` 経由で外部バーへ公開するワークスペースモデル。
   */
  workspace: WorkspaceController;
  /**
   * Input device enumeration and per-device configuration.
   * 入力デバイスの列挙とデバイスごとの設定。
   *
   * @example Configure touchpad and keyboard / タッチパッドとキーボードを設定
   * ```ts
   * COMPOSITOR.input.configure((input) => {
   *   input.global = {
   *     touchpad: { tapToClick: true, naturalScroll: true, disableWhileTyping: true },
   *     keyboard: { layout: "us", options: "ctrl:nocaps" },
   *   };
   * });
   * ```
   */
  input: InputController;
  /**
   * Desktop cursor appearance. The compositor applies this to named cursors
   * and synchronizes `XCURSOR_THEME` / `XCURSOR_SIZE` for future processes.
   * デスクトップカーソルの外観。named cursor に適用し、今後起動する
   * プロセス向けの `XCURSOR_THEME` / `XCURSOR_SIZE` も同期します。
   *
   * @example
   * ```ts
   * COMPOSITOR.cursor.configure({
   *   theme: "Bibata-Modern-Classic",
   *   size: 24,
   * });
   * ```
   */
  cursor: CursorController;
  /**
   * Environment variable controller for child processes spawned by the compositor.
   * コンポジターが起動する子プロセスが継承する環境変数を管理します。
   *
   * @example
   * ```ts
   * COMPOSITOR.env.set("QT_QPA_PLATFORM", "wayland;xcb");
   * COMPOSITOR.env.apply({ MOZ_ENABLE_WAYLAND: 1, GDK_BACKEND: "wayland" });
   * ```
   */
  env: EnvController;
  /**
   * Process lifecycle management: one-shot startup, managed services, and ad-hoc spawns.
   * プロセスライフサイクル管理。起動時ワンショット・マネージドサービス・即時起動の 3 種類。
   *
   * @example One-shot on first session / セッション初回のみ起動
   * ```ts
   * COMPOSITOR.process.once("shell", { command: ["foot", "--server"] });
   * ```
   *
   * @example Managed service with auto-restart / 自動再起動つきマネージドサービス
   * ```ts
   * COMPOSITOR.process.service("cliphist", {
   *   command: "wl-paste --watch cliphist store",
   *   restart: "on-failure",
   * });
   * ```
   *
   * @example Spawn on demand / オンデマンド起動
   * ```ts
   * COMPOSITOR.key.bind("terminal", "Super+T", () => {
   *   COMPOSITOR.process.spawn({ command: ["kitty"] });
   * });
   * ```
   */
  process: ProcessController;
  /**
   * Compositor-level keyboard shortcut bindings.
   * コンポジターレベルのキーボードショートカットバインド。
   *
   * @example
   * ```ts
   * COMPOSITOR.key.bind("launch-terminal", "Super+T", () => {
   *   COMPOSITOR.process.spawn({ command: ["kitty"] });
   * });
   * // Tap binding (fires on key release) / タップバインド（キーリリース時に発火）
   * COMPOSITOR.key.bind("launcher", "Super", openLauncher, { on: "release" });
   * ```
   */
  key: KeyBindingController;
  /**
   * Compositor-level pointer (mouse) configuration.
   * コンポジターレベルのポインター（マウス）設定。
   *
   * @example Drag windows with Super held / Super を押しながらウィンドウをドラッグ
   * ```ts
   * COMPOSITOR.pointer.bindWindowMoveModifier("Super");
   * ```
   */
  pointer: PointerController;
  runtime: RuntimeController;
  /**
   * Per-window scene-tree composition. Assign a function to control what chrome
   * (borders, shadows, managed-window placement) wraps each toplevel window.
   * ウィンドウごとのシーンツリー合成。各トップレベルウィンドウをラップするクローム
   * （ボーダー・影・ManagedWindow の配置）を制御する関数を割り当てます。
   *
   * @example
   * ```tsx
   * COMPOSITOR.window.composition = (window) => (
   *   <ManagedWindow rect={layoutRect(window)} zIndex={getZIndex(window)}>
   *     <WindowBorder style={{ borderRadius: 8, border: { px: 1, color: "#ffffff33" } }}>
   *       <ClientWindow />
   *     </WindowBorder>
   *   </ManagedWindow>
   * );
   * ```
   */
  window: CompositorWindowController;
  /**
   * Layer-shell surface access and usable-area queries.
   * レイヤーシェルサーフェスへのアクセスと使用可能エリアのクエリ。
   *
   * @example Place windows below the top bar / トップバーの下にウィンドウを配置
   * ```ts
   * const usable = COMPOSITOR.layer.usableArea("DP-1");
   * // usable.y is already offset past the bar / usable.y はバーの下から始まります
   * ```
   *
   * @example Get reserved insets / 予約済みインセットを取得
   * ```ts
   * const { top, bottom } = COMPOSITOR.layer.reservedInsets("DP-1");
   * ```
   */
  layer: LayerController;
  /**
   * Debug knobs — toggles and overlays that don't affect production behavior.
   * デバッグ用トグルとオーバーレイ。本番動作には影響しません。
   *
   * @example Toggle FPS overlay / FPS オーバーレイを切り替え
   * ```ts
   * COMPOSITOR.key.bind("toggle-fps", "Super+Shift+F", () => {
   *   COMPOSITOR.debug.fpsCounter = !COMPOSITOR.debug.fpsCounter;
   * });
   * ```
   */
  debug: DebugController;
  /**
   * Optional default display mode applied before `output.configure` fires.
   * `output.configure` が呼ばれる前に適用されるデフォルト表示モード（省略可）。
   */
  display?: DisplayConfig;
}

export interface PreloadController {}

/**
 * Debug knobs. Toggles here are surfaced into compositor overlays / logs and
 * do not affect production behavior. Read-write properties take effect on the
 * next scheduler tick.
 */
export interface DebugController {
  /**
   * When true, the compositor draws a small FPS / frame-time overlay in the
   * top-left corner of every output. The overlay uses a pre-rasterized glyph
   * atlas (built once on first enable) so toggling it has near-zero per-frame
   * cost beyond the composite of ~6-8 glyph buffers.
   */
  fpsCounter: boolean;

  /**
   * Enable or disable the built-in timescope profiler. Disabling after it was
   * enabled dumps `summary.txt` and `report.html` under `~/shoji_wm/logs/`.
   */
  enableProfile(enabled: boolean): void;

  /**
   * Current timescope profiler state.
   */
  readonly profile: boolean;
}

export type SSDRebuildSuppressionViolationPolicy =
  "warn" | "fallback-last" | "fallback" | "throw" | "suppress-unsafe";

export interface SSDRebuildSuppressionOptions {
  /**
   * Allow updates that only affect <ManagedWindow> props to stay on the managed-window fast path.
   * Decoration tree/style/text/image/shader updates are treated as violations.
   */
  allowManagedWindowOnly?: boolean;
  /**
   * Restrict suppression to specific windows. Decoration updates for other
   * windows fall back to the normal rebuild path instead of being delayed by
   * an unrelated animation.
   */
  windowIds?: readonly string[];
  /**
   * Restrict suppression to specific layers. Decoration updates for other
   * layers fall back to the normal rebuild path.
   */
  layerIds?: readonly string[];
  /**
   * - "fallback": warn and fall back to the normal SSD rebuild path for the violating update.
   * - "warn": warn and keep suppressing SSD rebuilds, applying only managed-window updates.
   * - "fallback-last": warn, keep suppressing during the active scope, then rebuild
   *   windows/layers that had decoration-affecting changes when the scope is released.
   * - "throw": throw immediately when a decoration-affecting update is detected.
   * - "suppress-unsafe": keep suppressing without warning. Intended for tightly-scoped
   *   benchmarking or code that can prove only <ManagedWindow> props change.
   */
  onViolation?: SSDRebuildSuppressionViolationPolicy;
}

export interface SSDRebuildSuppressionHandle {
  release(): void;
}

export interface RuntimeController {
  suppressSSDRebuild(
    options?: SSDRebuildSuppressionOptions,
  ): SSDRebuildSuppressionHandle;
  withSSDRebuildSuppressed<T>(
    options: SSDRebuildSuppressionOptions | undefined,
    callback: () => T,
  ): T;
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
  sizeConstraints: import("./signals").ReadonlySignal<WindowSizeConstraints>;
  isResizable: import("./signals").ReadonlySignal<boolean>;
  isTransient: import("./signals").ReadonlySignal<boolean>;
  parentId: import("./signals").ReadonlySignal<string | undefined>;
  icon: import("./signals").ReadonlySignal<WindowIcon | undefined>;
  interaction: import("./signals").ReadonlySignal<WindowCompositionInteractionSnapshot>;
  transformOriginX: import("./signals").Signal<number>;
  transformOriginY: import("./signals").Signal<number>;
  transformTranslateX: import("./signals").Signal<number>;
  transformTranslateY: import("./signals").Signal<number>;
  transformScaleX: import("./signals").Signal<number>;
  transformScaleY: import("./signals").Signal<number>;
  transformOpacity: import("./signals").Signal<number>;
}

export interface ReactiveWaylandLayerSignals {
  id: import("./signals").ReadonlySignal<string>;
  namespace: import("./signals").ReadonlySignal<string | undefined>;
  layer: import("./signals").ReadonlySignal<WaylandLayerKind>;
  outputName: import("./signals").ReadonlySignal<string>;
  positionX: import("./signals").ReadonlySignal<number>;
  positionY: import("./signals").ReadonlySignal<number>;
  positionWidth: import("./signals").ReadonlySignal<number>;
  positionHeight: import("./signals").ReadonlySignal<number>;
  anchor: import("./signals").ReadonlySignal<WaylandLayerAnchor>;
  exclusiveZone: import("./signals").ReadonlySignal<WaylandLayerExclusiveZone>;
  exclusiveEdge: import("./signals").ReadonlySignal<WaylandLayerEdge | null>;
  margin: import("./signals").ReadonlySignal<WaylandLayerMargin>;
  keyboardInteractivity: import("./signals").ReadonlySignal<WaylandLayerKeyboardInteractivity>;
  desiredSize: import("./signals").ReadonlySignal<WaylandLayerDesiredSize>;
}

export interface ReactiveWaylandWindow extends WaylandWindow {
  readonly signals: ReactiveWaylandWindowSignals;
}

export interface ReactiveWaylandLayer extends WaylandLayer {
  readonly signals: ReactiveWaylandLayerSignals;
}

export interface WaylandWindowActions {
  close(): void;
  maximize(): void;
  unmaximize(): void;
  minimize(): void;
  fullscreen(): void;
  unfullscreen(): void;
  focus(): void;
  scheduleAnimation(options: ManagedWindowScheduleAnimationOptions): void;
  cancelAnimation(channel?: string): void;
  setCloseAnimationDuration(durationMs: number): void;
  isXWayland(): boolean;
}

export interface CompositorWindowController {
  /**
   * Per-window composition function. Returns the scene tree (chrome,
   * managed-window placements, effects) for a given `WaylandWindow`. The
   * compositor reevaluates this whenever the window's reactive inputs
   * change, so signal reads inside the function automatically track
   * dependencies.
   *
   * Set to `null` (the default) to leave windows undecorated.
   */
  composition: WindowCompositionFunction | null;
  /** Client/server-side window decoration negotiation policy. */
  readonly decoration: CompositorWindowDecorationController;
  /**
   * Request keyboard focus for `window`. The compositor raises it, updates
   * keyboard focus, and emits the usual focus-changed notifications — so
   * `isFocused` signals, composition reevaluation, and
   * `COMPOSITOR.event.onFocus` listeners all fire just as they would for
   * a user-initiated focus change.
   */
  focus(window: WaylandWindow): void;
}

export interface ReactiveWaylandWindowHandle {
  readonly window: ReactiveWaylandWindow;
  readonly transform: WindowTransform;
  update(snapshot: WaylandWindowSnapshot): void;
  updateManagedWindow(state: ManagedWindowState): void;
}

export interface ReactiveWaylandLayerHandle {
  readonly layer: ReactiveWaylandLayer;
  update(snapshot: WaylandLayerSnapshot): void;
}

export type WindowActionType =
  | "close"
  | "maximize"
  | "unmaximize"
  | "minimize"
  | "fullscreen"
  | "unfullscreen";

export interface WindowActionDescriptor {
  kind: "window-action";
  action: WindowActionType;
}

export type SerializableCompositionChild =
  SerializedCompositionNode | PrimitiveChild;

export interface SerializedCompositionNode {
  kind: CompositionNodeType;
  nodeId: string;
  props: Record<string, unknown>;
  children: SerializableCompositionChild[];
}
