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
  /** Output whose frames drive this window's runtime work. / このウィンドウのランタイム処理を駆動する出力。 */
  readonly outputName?: string;
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
  /**
   * The output this window belongs to: the one containing its center, else
   * the one it overlaps, else the nearest. Its frames drive the window's
   * animations and polls. `undefined` only while no output exists.
   *
   * このウィンドウが属する出力（中心を含む出力、なければ重なる出力、なければ最寄りの
   * 出力）。この出力のフレームがウィンドウのアニメーションと poll を駆動します。
   * 出力が 1 つも無い間だけ `undefined`。
   */
  readonly output: import("./signals").ReadonlySignal<string | undefined>;
  /**
   * `createPoll` bound to this window's output; follows the window when it
   * moves to another output.
   * このウィンドウの出力に結び付いた `createPoll`。ウィンドウが別の出力へ移ると追従します。
   *
   * @example
   * ```ts
   * window.createPoll(16, (handle) => step(handle.nowMs));
   * ```
   */
  createPoll(
    intervalMs: number,
    callback: import("./scheduler").PollCallback,
    options?: { dirty?: import("./scheduler").PollDirtyMode },
  ): import("./scheduler").PollHandle;
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
  min?: WindowSize | null;
  max?: WindowSize | null;
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
  /**
   * Rendering policy resolved by `COMPOSITOR.rendering.surfacePolicy` for this
   * window's client surface tree. Evaluated inside the managed-window
   * dependency scope, so window-state signals read by the callback re-trigger
   * evaluation. `undefined` keeps compositor defaults.
   * `COMPOSITOR.rendering.surfacePolicy` がこのウィンドウのクライアント
   * サーフェスツリーに対して解決したレンダリングポリシー。managed-window の
   * 依存スコープ内で評価されるため、コールバックが読んだウィンドウ状態
   * signal の変化で再評価されます。`undefined` はコンポジターデフォルト。
   */
  surfacePolicy?: SurfacePolicy;
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
  | "Popup"
  | "Fragment";

/**
 * Base props inherited by all composition components.
 * すべての合成コンポーネントが継承するベース props。
 */
export interface ComponentProps {
  children?: CompositionRenderable | CompositionRenderable[];
  /**
   * Replaces the node's built-in background/border painting with a paint
   * shader (see {@link paintShader}). On `<ShaderEffect/>` it is drawn over
   * the effect output.
   * ノード組み込みの背景・枠描画をペイントシェーダーで置き換えます。
   */
  paint?: PaintShaderHandle;
  /**
   * A paint shader drawn above the node's children (focus rings, sheens).
   * 子要素より上に描くペイントシェーダー（フォーカスリング等）。
   */
  overlay?: PaintShaderHandle;
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

/**
 * Extra logical pixels a paint shader draws around its node (glows, custom
 * shadows): one value for every side, or per side.
 * ペイントシェーダーがノードの外側に描く論理ピクセル量。
 */
export type PaintOutsets =
  | number
  | { top?: number; right?: number; bottom?: number; left?: number };

/**
 * A paint shader: a GLSL file defining `vec4 paint_main(PaintContext ctx)`.
 * Create it with {@link paintShader}.
 * `vec4 paint_main(PaintContext ctx)` を定義した GLSL ファイル。
 */
export interface PaintShaderHandle {
  kind: "paint-shader";
  shader: ShaderModuleHandle;
  uniforms?: ShaderUniformMap;
  outsets?: PaintOutsets;
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

export type EffectStateTextureFormat = "rgba8" | "rg16f" | "rgba16f";
export type EffectStateResizePolicy = "clear" | "stretch";

/**
 * Descriptor for a persistent texture owned by each effect instance.
 * Sharing this handle does not share its GPU storage between surfaces.
 */
export interface StateTextureHandle {
  kind: "state-texture";
  name: string;
  scale: number;
  format: EffectStateTextureFormat;
  resize: EffectStateResizePolicy;
}

/** Read the latest value of a persistent state texture at this pipeline point. */
export interface StateTextureSourceHandle {
  kind: "state-source";
  state: StateTextureHandle;
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

/** Render a side pipeline into persistent state without replacing `current`. */
export interface RenderToStageHandle {
  kind: "render-to";
  target: StateTextureHandle;
  effect: CompiledEffectHandle;
  /**
   * Set by `renderToIfDirty()` only. When present, the side pipeline is
   * skipped while none of these sources changed since the state was written.
   */
  dependsOn?: EffectDependencyHandle[];
}

/**
 * A subject source a `renderToIfDirty()` side pipeline can depend on.
 * `renderToIfDirty()` のサイドパイプラインが依存先として宣言できるソース。
 */
export type EffectDependencyHandle =
  | WindowSourceHandle
  | LayerSourceHandle
  | PopupSourceHandle;

export type EffectInputHandle =
  | import("./overlay").SnapshotSourceHandle
  | BackdropSourceHandle
  | XrayBackdropSourceHandle
  | ShaderInputHandle
  | ImageSourceHandle
  | NamedTextureHandle
  | StateTextureSourceHandle
  | WindowSourceHandle
  | LayerSourceHandle
  | PopupSourceHandle;

export type EffectStageHandle =
  | ShaderStageHandle
  | NoiseStageHandle
  | DualKawaseBlurStageHandle
  | SaveStageHandle
  | BlendStageHandle
  | UnitStageHandle
  | RenderToStageHandle;

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
  /**
   * Replaces the window's subsurfaces (embedded video, previews, ...) separately
   * from the rest of the window, over their own bounds, so subsurfaces reaching
   * outside the window are covered too. `windowSource()` reads the subsurfaces.
   * While this or `behindSubsurfaces` is set, the window sources of the other
   * slots leave subsurfaces out.
   * Use `effect_frame_uv()` in the shader to line a mask up with the window.
   * TTY backend only.
   * ウィンドウのサブサーフェス（埋め込み動画・プレビューなど）を、ウィンドウ本体とは
   * 別に、サブサーフェス自身の範囲で置き換えます。ウィンドウからはみ出した
   * サブサーフェスにも適用されます。`windowSource()` はサブサーフェスを読みます。
   * これか `behindSubsurfaces` を設定している間は、他のスロットのウィンドウソースから
   * サブサーフェスが除かれます。
   * マスクをウィンドウに揃えるにはシェーダーで `effect_frame_uv()` を使います。
   * TTY バックエンドのみ。
   */
  replaceSubsurfaces?: WindowEffectHandle | null;
  /**
   * Drawn behind the window's subsurfaces (e.g. their drop shadow), over their
   * own bounds plus `outsets`, so subsurfaces outside the window get it too.
   * `windowSource()` reads the subsurfaces as they are, before
   * `replaceSubsurfaces`. Subsurfaces are left out of the other slots' window
   * sources as with `replaceSubsurfaces`. TTY backend only.
   * ウィンドウのサブサーフェスの背後に描画します（影など）。サブサーフェス自身の範囲
   * ＋`outsets` で処理するため、ウィンドウからはみ出したサブサーフェスにも適用されます。
   * `windowSource()` は `replaceSubsurfaces` 適用前のサブサーフェスを読みます。
   * `replaceSubsurfaces` と同様、他のスロットのウィンドウソースからサブサーフェスが
   * 除かれます。TTY バックエンドのみ。
   */
  behindSubsurfaces?: WindowEffectHandle | null;
}

/**
 * The part of a layer surface that a backdrop `behind` effect covers. The effect
 * captures, runs its pipeline and is re-run only over the bounding box of that
 * part (plus `outsets`), not over the whole surface. Worth setting when the
 * surface is much larger than what it draws — a fixed-size shell surface, or a
 * full-screen one with the bar cut out by an input mask.
 * - `"surface"` (default): the whole surface.
 * - `"input"`: the input region (QuickShell's `mask`). A surface without one
 *   takes input everywhere, so this is then the whole surface.
 * - `"blur-region"`: the blur region the client asked for through
 *   ext-background-effect (QuickShell's `BackgroundEffect.blurRegion`). Nothing
 *   is drawn while the client asks for none.
 *
 * Anything drawn outside the region gets no effect, so pad it with `outsets`
 * where the surface draws past its region (antialiasing, shapes that merge).
 * The region is part of the same commit as the buffer, so the two never
 * disagree. Only a backdrop `behind` accepts a region other than `"surface"`.
 *
 * レイヤーサーフェスのうち、バックドロップの `behind` エフェクトが覆う部分です。
 * キャプチャ・パイプライン実行・再計算の判定を、サーフェス全体ではなくその部分の
 * 外接矩形（＋`outsets`）だけで行います。サーフェスが描画内容よりずっと大きい
 * とき（固定サイズのシェル、入力マスクでバーだけ切り出した全画面サーフェスなど）
 * に指定すると軽くなります。
 * - `"surface"`（既定）: サーフェス全体。
 * - `"input"`: 入力領域（QuickShell の `mask`）。未設定のサーフェスは全体で入力を
 *   受けるので、その場合はサーフェス全体になります。
 * - `"blur-region"`: ext-background-effect でクライアントが要求したブラー領域
 *   （QuickShell の `BackgroundEffect.blurRegion`）。要求が無い間は何も描きません。
 *
 * 領域の外に描かれたものにはエフェクトが掛からないため、領域からはみ出して描く分
 * （アンチエイリアスや形の融合など）は `outsets` で広げてください。領域はバッファと
 * 同じ commit で切り替わるため、見た目とずれることはありません。`"surface"` 以外を
 * 指定できるのはバックドロップの `behind` だけです。
 */
export type LayerEffectRegion = "surface" | "input" | "blur-region";

export interface LayerEffectHandle {
  kind: "layer-effect";
  effect: CompiledEffectHandle;
  outsets?: EffectOutsets;
  region?: LayerEffectRegion;
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
   * May be a signal, and shader uniforms inside it may be signals too: the
   * compositor picks up a new value on the next frame.
   * Signal にでき、中のシェーダーユニフォームにも Signal を使えます。値が変わると
   * 次のフレームから反映されます。
   *
   * @example
   * ```ts
   * COMPOSITOR.effect.background_effect = compileEffect({
   *   input: backdropSource(),
   *   pipeline: [dualKawaseBlur({ radius: 4, passes: 2 })],
   * });
   * ```
   */
  background_effect: MaybeSignal<CompiledEffectHandle | null>;
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
 * about. Popups are delivered as `kind: "popup"`; toplevel windows as
 * `kind: "toplevel"` with the live `WaylandWindow` so the policy can read
 * window state (signal reads subscribe — the policy re-evaluates when the
 * state changes). The union may grow to layer surfaces without breaking
 * existing callbacks.
 * `COMPOSITOR.rendering.surfacePolicy` コールバックが判断対象とするサーフェス。
 * ポップアップは `kind: "popup"`、トップレベルウィンドウは `kind: "toplevel"`
 * で配送されます。toplevel は生きた `WaylandWindow` を持ち、ウィンドウ状態を
 * 参照できます（signal の読み取りは購読され、状態変化で再評価されます）。
 * 将来レイヤーが union に追加されても既存コールバックは壊れません。
 */
export type SurfacePolicyTarget =
  | ({ kind: "popup" } & WaylandPopup)
  | { kind: "toplevel"; window: WaylandWindow };

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
  /**
   * How frames are paced, for every output or per output (a function of the
   * output, re-evaluated as signals it reads change).
   *
   * - `"throughput"` (default): render the next frame while the previous one
   *   waits for its vblank (render-ahead / triple buffering). A frame gets up
   *   to one extra refresh period, so heavy frames stop missing vblanks, at
   *   the cost of one frame of latency while frames are continuous. Isolated
   *   updates (typing) are not delayed.
   * - `"low-latency"`: render each frame only once the previous one is on
   *   screen.
   *
   * The compositor always uses low latency for tearing and fullscreen
   * (direct scanout) windows, whatever this says. Animations and polls step
   * on each output's own frame clock either way, so their timing does not
   * change with this setting.
   *
   * フレームの出し方。全出力共通、または出力ごと（出力を受け取る関数。読んだ signal の
   * 変化で再評価）に指定します。
   *
   * - `"throughput"`（既定）: 前のフレームが vblank を待っている間に次のフレームを描画
   *   します（先行描画／トリプルバッファ）。1 フレームに最大 1 リフレッシュ分の余裕が
   *   増え重いフレームでも vblank を逃しにくくなる代わりに、連続描画中は 1 フレーム遅延が
   *   増えます。単発の更新（タイピングなど）は遅れません。
   * - `"low-latency"`: 前のフレームが表示されてから次を描画します。
   *
   * テアリング中とフルスクリーン（ダイレクトスキャンアウト）のウィンドウでは、設定に
   * 関係なく常に低遅延になります。アニメーションと poll はどちらでも出力ごとのフレーム
   * 時計で進むため、この設定でタイミングは変わりません。
   *
   * @example Low latency on AC power only / AC 電源のときだけ低遅延
   * ```ts
   * COMPOSITOR.rendering.framePacing = () =>
   *   onBattery() ? "throughput" : "low-latency";
   * ```
   */
  framePacing?: FramePacing | ((output: OutputInfo) => FramePacing);
  /**
   * How each output is put together. Return the default stacking
   * (`<DefaultComposition />`) or rearrange it: render parts into textures
   * with `renderTexture`, draw them flat with `<TextureView>` or in 3D with
   * `<Scene3D>`/`<Plane>`, show windows of hidden workspaces with
   * `<Windows windows={...} />`. Re-evaluated only when signals it reads
   * change. Session lock, cursor and error overlays always stay on top.
   * Without it every output uses the default stacking.
   *
   * 各出力の組み立て方。既定の重なり（`<DefaultComposition />`）を返すか、
   * 組み替えます: `renderTexture` で一部をテクスチャに描き、`<TextureView>` で平面に、
   * `<Scene3D>`/`<Plane>` で 3D に描き、`<Windows windows={...} />` で非表示の
   * ワークスペースのウィンドウも表示できます。読んだ signal が変わったときだけ
   * 再評価されます。ロック画面・カーソル・エラー表示は常に最前面です。
   * 未設定ならすべての出力が既定の重なりです。
   *
   * @example
   * ```tsx
   * COMPOSITOR.rendering.composition = (output) => <DefaultComposition />;
   * ```
   */
  composition?: (
    output: OutputInfo,
  ) => import("./types").CompositionRenderable | import("./types").CompositionRenderable[];
}

/** Frame pacing of an output; see `COMPOSITOR.rendering.framePacing`. / 出力のフレームの出し方。 */
export type FramePacing = "throughput" | "low-latency";

export interface OutputMode {
  width: number;
  height: number;
  refreshRate: number;
  /**
   * DRM pixel clock in kHz. Absent on backends with no DRM mode behind them.
   * Cannot be derived from the fields above — blanking is time actually spent
   * transmitting, so 3840x2160@60 clocks at 594 MHz, not the 497.7 MHz its
   * active pixels imply. Link-bandwidth arithmetic must use this.
   * DRM のピクセルクロック（kHz）。ブランキングを含むため上記の値からは
   * 算出できません。帯域計算にはこの値を使ってください。
   */
  clockKhz?: number;
}

/** What an HDMI sink says its link can carry. / HDMI シンクのリンク能力。 */
export interface HdmiLinkInfo {
  /** Inferred from what the sink advertises, e.g. "HDMI 2.0". */
  standard: string;
  /** Max TMDS character rate in kHz, when the sink states one. */
  maxTmdsKhz?: number;
  /** Total usable link bandwidth in Gbit/s. */
  maxBandwidthGbps?: number;
}

/** Panel power for `COMPOSITOR.output.setPower`. / `COMPOSITOR.output.setPower` で指定する電源状態。 */
export type OutputPower = "on" | "off" | "toggle";

export interface OutputPowerOptions {
  /** Output name; every output when omitted. / 出力名。省略すると全出力。 */
  output?: string;
  /**
   * Only when switching off: switch back on at the next key press, click,
   * pointer motion, scroll or touch. Releases do not count, so a key binding
   * can use it. Defaults to `false`.
   * 消灯時のみ有効。次のキー押下・クリック・ポインター移動・スクロール・タッチで
   * 再点灯します。キーを離す操作は数えないので、キーバインドからも使えます。既定は `false`。
   */
  wakeOnInput?: boolean;
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

/**
 * Output rotation/flip, following the standard `wl_output.transform` enum.
 * Rotations are counter-clockwise in wl_output terms; visually,
 * `rotate-90` turns the picture 90° for a monitor physically rotated
 * clockwise into portrait orientation.
 * 標準の `wl_output.transform` enum に従う出力の回転・反転。
 * `rotate-90` は時計回りに縦置きしたモニター向けの回転です。
 */
export type OutputTransform =
  | "normal"
  | "rotate-90"
  | "rotate-180"
  | "rotate-270"
  | "flipped"
  | "flipped-90"
  | "flipped-180"
  | "flipped-270";

/**
 * How a panel's subpixels are physically arranged, as advertised to clients in
 * `wl_output.geometry`. Describes the panel as built: never adjust it for
 * `transform`.
 * パネルのサブピクセルの物理的な配列（`wl_output.geometry` でクライアントに
 * 通知されます）。製造時のパネルそのものを表すため、`transform` に合わせて
 * 変えないでください。
 */
export type OutputSubpixel =
  | "unknown"
  | "none"
  | "horizontal-rgb"
  | "horizontal-bgr"
  | "vertical-rgb"
  | "vertical-bgr";

export interface OutputExtendConfigEntry {
  mode?: "extend";
  /**
   * Preferred display mode, matched against the physical (untransformed)
   * mode list regardless of `transform`.
   * 希望する表示モード。`transform` に関係なく物理（回転前）のモード一覧と
   * 照合されます。
   */
  resolution?: OutputResolutionPreference;
  position?: OutputPositionPreference;
  scale?: number;
  /**
   * Rotation/flip applied to this output. Omitting the field means
   * `"normal"` — the draft is declarative, so dropping a previously set
   * transform resets the output to its unrotated orientation.
   * この出力に適用する回転・反転。省略時は `"normal"`（ドラフトは宣言的
   * なので、設定を消すと回転なしに戻ります）。
   */
  transform?: OutputTransform;
  /**
   * Physical subpixel layout of this panel, which clients such as foot use for
   * subpixel text antialiasing. Omit it to keep what the kernel reported,
   * which is `"unknown"` for most panels.
   * このパネルの物理的なサブピクセル配列。foot などのクライアントがサブピクセル
   * 単位の文字のアンチエイリアスに使います。省略するとカーネルが報告した値
   * （多くのパネルでは `"unknown"`）のままになります。
   */
  subpixel?: OutputSubpixel;
  /**
   * Drive this output as HDR10 (PQ/BT.2020 signaling) when its EDID
   * advertises ST 2084 support; ignored otherwise. Experimental. SDR content
   * keeps its look at `hdrSdrLuminance`; HDR content from color-managed
   * clients (Chrome, mpv, games) is shown up to the display's peak.
   * この出力の EDID が ST 2084 対応を示す場合、HDR10(PQ/BT.2020)で
   * 駆動します（実験的機能）。SDR の内容は `hdrSdrLuminance` の明るさで
   * そのまま表示され、色管理に対応したクライアント（Chrome・mpv・ゲーム）の
   * HDR の内容はディスプレイの最大輝度まで表示されます。
   */
  hdr?: boolean;
  /**
   * Real peak luminance of this display in cd/m². Only needed when the EDID
   * advertises ST 2084 but omits its luminance fields, which is common — the
   * compositor otherwise has to assume 1000. Ignored outside 50..=10000.
   * このディスプレイの実際の最大輝度（cd/m²）。EDID が ST 2084 対応を示し
   * ながら輝度情報を持たない場合にのみ必要です（その場合は 1000 と仮定され
   * ます）。50〜10000 の範囲外は無視されます。
   */
  hdrMaxLuminance?: number;
  /**
   * Real black level of this display in cd/m². Ignored outside 0..=10.
   * このディスプレイの実際の黒レベル（cd/m²）。0〜10 の範囲外は無視されます。
   */
  hdrMinLuminance?: number;
  /**
   * What SDR white (ordinary windows) is shown at on this HDR output: a
   * luminance in cd/m² (10..=1000), or `"backlight"` to follow the panel's
   * brightness setting, which the panel itself ignores in HDR. Unset follows
   * the backlight on panels that have one (laptops), else 203 (ITU-R BT.2408).
   * HDR content above SDR white gets whatever is left up to the peak.
   * HDR 出力で SDR の白（通常のウィンドウ）を表示する明るさ。cd/m² の数値
   * （10〜1000）か、パネルの明るさ設定に連動する `"backlight"`（HDR 中は
   * パネル自体が明るさ設定を無視するため）。省略時はバックライトのある
   * パネル（ノート PC）では連動、それ以外は 203（ITU-R BT.2408）。SDR の白
   * から最大輝度までが HDR の内容に使われます。
   */
  hdrSdrLuminance?: number | "backlight";
  /**
   * How colors are shown on this HDR output. `"native"` (default) stretches
   * them over the panel's own gamut, as the panel itself does in SDR mode, so
   * a wide-gamut panel looks the same with HDR on and off. It applies to all
   * content alike, so color-managed clients (Chrome, HDR video) get the same
   * vivid look as every other window. `"srgb"` shows exact colors, which look
   * duller on such a panel.
   * HDR 出力での色の出し方。`"native"`（既定）は SDR モードのパネルと同じく
   * パネル本来の色域に広げて表示し、広色域パネルでも HDR のオン・オフで見た目が
   * 変わりません。すべての内容に同じく効くので、色管理に対応したクライアント
   * （Chrome や HDR 動画）も他のウィンドウと同じ鮮やかさになります。`"srgb"` は
   * 正確な色で、そうしたパネルではくすんで見えます。
   */
  hdrSdrGamut?: "native" | "srgb";
}

export interface OutputDisabledConfigEntry {
  mode: "disabled";
}

export interface OutputMirrorConfigEntry {
  mode: "mirror";
  source: string;
  /** Same as {@link OutputExtendConfigEntry.subpixel}. / 同上。 */
  subpixel?: OutputSubpixel;
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
  /**
   * Current mode size in the output's *transformed* orientation: on a
   * `rotate-90`/`rotate-270` output, width/height are swapped relative to the
   * physical mode, so `resolution / scale` is always the logical size.
   * `availableModes` stay physical.
   * 現在のモードサイズ（transform 適用後の向き）。`rotate-90`/`rotate-270`
   * では物理モードに対して幅と高さが入れ替わるため、`resolution / scale` が
   * 常に論理サイズになります。`availableModes` は物理のままです。
   */
  resolution?: OutputMode;
  position: {
    x: number;
    y: number;
  };
  scale: number;
  /** Currently applied transform. / 現在適用されている transform。 */
  transform?: OutputTransform;
  /**
   * Subpixel layout currently advertised in `wl_output.geometry`.
   * 現在 `wl_output.geometry` で通知しているサブピクセル配列。
   */
  subpixel?: OutputSubpixel;
  /**
   * Subpixel layout the kernel reported for this connector, which `subpixel`
   * falls back to when the display config names none.
   * このコネクタについてカーネルが報告したサブピクセル配列。表示設定で指定が
   * ない場合、`subpixel` はこの値になります。
   */
  detectedSubpixel?: OutputSubpixel;
  availableModes: OutputMode[];
  /** EDID advertises HDR (CTA-861 static metadata block). */
  hdrSupported: boolean;
  /**
   * Link capability for HDMI connectors. Absent for non-HDMI outputs and for
   * sinks publishing no vendor block.
   *
   * Supporting HDR and being able to deliver it are different questions. PQ
   * needs 10 bits per component, which costs `pixelClock * 10/8` of TMDS
   * character rate; 3840x2160@60 therefore needs 742.5 MHz and does not fit
   * HDMI 2.0's 600 MHz ceiling, even on a sink whose EDID advertises both the
   * mode and ST 2084. The driver then falls back to 8 bpc or subsampled
   * chroma, and PQ tolerates neither.
   * HDMI コネクタのリンク能力。HDR 対応と実際に伝送できるかは別問題です。
   */
  hdmi?: HdmiLinkInfo;
}

export interface OutputInfo extends OutputStateSnapshot {
  name: string;
  enabled: boolean;
  transform: OutputTransform;
  subpixel: OutputSubpixel;
  detectedSubpixel: OutputSubpixel;
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
  /**
   * Switches panels on or off (DPMS) without changing the layout: windows stay
   * where they are and the output stays connected and enabled. `"toggle"`
   * switches the targeted outputs off when any of them is on, otherwise on.
   * Only the TTY backend has panels to switch; nested sessions ignore it.
   * Idle daemons can do the same through wlr-output-power-management (`wlopm`).
   * レイアウトを変えずにパネルの電源を切り替えます（DPMS）。ウィンドウはそのままで、
   * 出力も接続・有効のままです。`"toggle"` は対象のどれかが点灯中なら消灯、
   * そうでなければ点灯します。TTY バックエンドのみ対応で、ネスト実行では無視されます。
   * アイドルデーモンからは wlr-output-power-management（`wlopm`）で同じ操作ができます。
   */
  setPower(power: OutputPower, options?: OutputPowerOptions): void;
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
 * Registers compositor-level keyboard and discrete mouse-wheel shortcuts.
 * コンポジターレベルのキーボードおよびマウスホイールショートカットを登録します。
 *
 * Shortcuts are identified by a string `id` (shown in help UIs). The `shortcut`
 * string uses modifier+key notation such as `"Super+T"`, `"Super+Shift+Left"`,
 * or `"Super+WheelScrollUp"`.
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
   * Register a keyboard or mouse-wheel shortcut. `id` must be unique;
   * registering the same id twice replaces the previous binding.
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
export type InputMotionSpace = "logical" | "physical";
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
  /**
   * Coordinate space relative pointer motion is interpreted in.
   * Defaults to `"logical"`.
   * 相対ポインター移動を解釈する座標空間です。既定は `"logical"` です。
   *
   * libinput reports motion in a scale-independent unit; treating one unit as
   * one logical pixel is a compositor-side convention. Under fractional
   * scaling the smallest possible movement then lands between physical pixels
   * (one unit at scale 1.5 spans 1.5 of them), and because the cursor is drawn
   * on whole physical pixels the rounding turns steady movement into an uneven
   * 2,1,2,1 step pattern. `"physical"` divides by the scale of the output
   * under the pointer so one unit is exactly one physical pixel and every step
   * is uniform.
   * libinput はスケール非依存の単位で移動量を報告し、その 1 単位を論理 1px と
   * みなすのはコンポジター側の慣習です。フラクショナルスケールではこの慣習に
   * より最小移動量が物理ピクセルの間に落ち（scale 1.5 なら 1 単位が 1.5 物理
   * px）、カーソルは整数の物理ピクセルにしか描けないため、丸めによって一定速度
   * の移動が 2,1,2,1 の不均一なステップになります。`"physical"` はポインターが
   * 乗っている出力のスケールで割り、1 単位をちょうど物理 1px にすることで
   * ステップを均一にします。
   *
   * The pointer necessarily travels `scale` times less far per hand movement,
   * and that cannot be undone in software — scaling the delta back up restores
   * the fractional step. Raise the device's hardware DPI (or `pointerAccel`)
   * instead so it emits proportionally more units.
   * この設定では手の移動量あたりのポインター移動が scale 分の 1 になります。
   * ソフト側で戻すことはできません（掛け直すと半端なステップが復活するため）。
   * 速度はデバイス側の DPI（または `pointerAccel`）を上げて補ってください。
   */
  motionSpace?: InputMotionSpace;
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
  /**
   * Take all keyboard, pointer button, scroll and swipe input until the
   * returned grab is released, for modal UIs drawn by the compositor (a
   * window switcher). Clients lose the pointer meanwhile; the cursor still
   * moves. Key bindings do not fire while a grab is held. The compositor ends
   * the grab itself (`onCancel`) when the screen locks or a handler throws.
   * 返り値のグラブを解放するまで、キーボード・ポインタボタン・スクロール・スワイプの
   * 入力をすべて受け取ります（ウィンドウスイッチャーなどのモーダル UI 用）。
   * その間クライアントはポインタを失い、キーバインドは発火しません。
   *
   * @example
   * ```ts
   * const grab = COMPOSITOR.input.grab({
   *   onKey(event) {
   *     if (event.key === "Escape" && event.state === "pressed") grab.release();
   *   },
   * });
   * ```
   */
  grab(options: import("./input-grab").InputGrabOptions): import("./input-grab").InputGrab;
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

/**
 * One CSS `box-shadow` layer. Lengths are logical pixels.
 * CSS の `box-shadow` 1 層分。長さは論理ピクセル。
 */
export interface BoxShadowValue {
  /** Horizontal offset. / 水平オフセット。 */
  x?: MaybeSignal<number>;
  /** Vertical offset. / 垂直オフセット。 */
  y?: MaybeSignal<number>;
  /** Blur radius (CSS semantics). / ぼかし半径（CSS と同じ意味）。 */
  blur?: MaybeSignal<number>;
  /** Grows (positive) or shrinks (negative) the shadow. / 影の拡大・縮小。 */
  spread?: MaybeSignal<number>;
  color: MaybeSignal<string>;
  /** Draw inside the padding box. / パディングボックスの内側に描く。 */
  inset?: MaybeSignal<boolean>;
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
  /**
   * CSS-like shadows; the first entry is painted on top.
   * CSS 風の影。先頭の要素が最前面に描かれます。
   */
  boxShadow?: MaybeSignal<BoxShadowValue | BoxShadowValue[]>;
  visible?: MaybeSignal<boolean>;
  cursor?: MaybeSignal<string>;
  fontSize?: MaybeSignal<number>;
  fontWeight?: MaybeSignal<FontWeight>;
  fontFamily?: MaybeSignal<FontFamily>;
  textAlign?: MaybeSignal<"start" | "center" | "end">;
  lineHeight?: MaybeSignal<number>;
}

/** The side of the anchor a `<Popup>` goes to. / `<Popup>` を出す基準の辺。 */
export type PopupPlacement = "top" | "bottom" | "left" | "right";
/** Alignment of a `<Popup>` along that side. / その辺に沿った揃え位置。 */
export type PopupAlign = "start" | "center" | "end";
/**
 * What a `<Popup>` does when it would leave the output.
 * `<Popup>` が出力からはみ出すときの挙動。
 */
export type PopupCollision = "flip" | "none";
/** Stacking layer of a `<Popup>`. / `<Popup>` の重なり順レイヤー。 */
export type PopupLayer = "top" | "window";
/**
 * How a `<Popup>` takes part in input, like the HTML `popover` attribute.
 * `<Popup>` の入力への関わり方（HTML の `popover` 属性に相当）。
 *
 * - `"hint"`: a tooltip; pointer input falls through. / ツールチップ。入力は素通り。
 * - `"auto"`: takes input and is light-dismissed (`onOpenChange`). /
 *   入力を受け、外側クリック等で閉じる要求が来る。
 * - `"manual"`: takes input; only the config closes it. / 入力を受け、閉じるのは設定側だけ。
 */
export type PopupMode = "hint" | "auto" | "manual";
/**
 * Why the compositor asks an `"auto"` popup to close (or, with `trigger`,
 * why it closed by itself).
 * `"auto"` のポップアップに閉じる要求が来た理由。
 */
export type PopupOpenChangeReason =
  | "outside-press"
  | "escape"
  | "anchor-gone"
  | "other-popup";
/** What opens a `<Popup>` by itself. / `<Popup>` を自動で開くきっかけ。 */
export type PopupTrigger = "hover" | "click";

/**
 * Props of {@link Popup}. The children are laid out as a column, sized by
 * their content.
 * {@link Popup} の props。子要素は縦に並び、中身の大きさに合わせます。
 */
export interface PopupProps extends ComponentProps {
  /**
   * Whether the popup is shown (default `true`). Toggling it does not lay the
   * window out again.
   * 表示するかどうか（既定 `true`）。切り替えてもウィンドウの再レイアウトは起きません。
   */
  open?: MaybeSignal<boolean>;
  /**
   * The side of the parent (the anchor) to show the popup on. Default
   * `"bottom"`.
   * 親（基準）のどちら側に出すか。既定 `"bottom"`。
   */
  placement?: MaybeSignal<PopupPlacement>;
  /** Alignment along that side. Default `"center"`. / その辺での揃え位置。既定 `"center"`。 */
  align?: MaybeSignal<PopupAlign>;
  /** Distance from the anchor in logical pixels. Default `0`. / 基準からの距離（論理 px）。既定 `0`。 */
  offset?: MaybeSignal<number>;
  /**
   * `"flip"` (default): move to the opposite side when that has more room on
   * the output, and slide along the side to stay on it. `"none"`: keep the
   * requested position.
   * `"flip"`（既定）: 出力内の余白が反対側の方が大きければそちらへ移り、辺に沿って
   * 画面内に収まるようずらします。`"none"`: 指定位置のまま。
   */
  collision?: MaybeSignal<PopupCollision>;
  /**
   * `"top"` (default): above every window, below layer-shell bars.
   * `"window"`: right above its own window, so higher windows cover it.
   * `"top"`（既定）: 全ウィンドウの上、レイヤーシェルのバーの下。
   * `"window"`: 自分のウィンドウのすぐ上（上にあるウィンドウには隠れます）。
   */
  layer?: MaybeSignal<PopupLayer>;
  /**
   * How the popup takes part in input. Default `"hint"`.
   * 入力への関わり方。既定 `"hint"`。
   */
  mode?: MaybeSignal<PopupMode>;
  /**
   * `"auto"` only: Escape asks it to close, and does not reach the focused
   * window while it is open. Default `true`.
   * `"auto"` のみ: Esc で閉じる要求を出し、開いている間 Esc はウィンドウに届かない。既定 `true`。
   */
  closeOnEscape?: MaybeSignal<boolean>;
  /**
   * `"auto"` only: a press outside the popup and its parent asks it to
   * close. Default `true`.
   * `"auto"` のみ: ポップアップと親の外を押すと閉じる要求を出す。既定 `true`。
   */
  closeOnOutsidePress?: MaybeSignal<boolean>;
  /**
   * Called with `(false, reason)` when the compositor asks an `"auto"`
   * popup to close. Set your `open` signal to follow it, or keep it open.
   * `"auto"` のポップアップに閉じる要求が来たとき `(false, reason)` で呼ばれます。
   */
  onOpenChange?: (open: boolean, reason: PopupOpenChangeReason) => void;
  /**
   * Called when the pointer enters / leaves the parent (the anchor) or, for
   * an interactive popup, the popup itself (like HTML `interestfor`).
   * ポインターが親（基準）に、対話型なら自身にも、出入りしたとき呼ばれます。
   */
  onInterestChange?: (interested: boolean) => void;
  /** Called when the parent (the anchor) is pressed. / 親（基準）が押されたとき呼ばれます。 */
  onAnchorPress?: () => void;
  /**
   * Let the popup open and close itself instead of `open`: `"hover"` opens
   * after the pointer rested `openDelay` ms on the parent and closes
   * `closeDelay` ms after it left both the parent and the popup; `"click"`
   * toggles on a press of the parent. Close requests (`onOpenChange`) close
   * it. Set once; do not switch it on and off.
   * `open` の代わりに自分で開閉します。`"hover"`: 親に `openDelay` ms 置くと開き、
   * 親とポップアップの両方から離れて `closeDelay` ms で閉じる。`"click"`: 親を押すたびに開閉。
   */
  trigger?: PopupTrigger;
  /** `trigger="hover"`: delay before opening, ms. Default `500`. / 開くまでの待ち時間（ms、既定 `500`）。 */
  openDelay?: number;
  /**
   * `trigger="hover"`: delay before closing, ms; lets the pointer cross the
   * gap between the parent and the popup. Default `200`.
   * 閉じるまでの猶予（ms、既定 `200`）。親とポップアップの隙間を渡れるようにします。
   */
  closeDelay?: number;
  /** Visual styling of the popup box itself. / ポップアップ自体のスタイル。 */
  style?: SSDStyle;
  /** Stable node id for targeted invalidation. / ターゲット無効化のための安定したノード ID。 */
  id?: string;
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

export interface CompositorEffectController extends CompositorEffectConfig {
  /** Render an output effect; resolves after capture and successful first GPU evaluation. */
  overlay: typeof import("./overlay").overlay;
}

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
  get effect(): CompositorEffectController;
  set effect(value: CompositorEffectConfig);
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
  output: import("./signals").ReadonlySignal<string | undefined>;
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
