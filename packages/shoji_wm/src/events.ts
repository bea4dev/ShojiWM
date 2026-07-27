import type {
  InputDeviceInfo,
  OutputInfo,
  WaylandLayer,
  WaylandWindow,
} from "./types";

export type WindowOpenListener = (window: WaylandWindow) => void;
export type WindowInitialConfigureListener = (window: WaylandWindow) => void;
export type WindowFirstCommitListener = (window: WaylandWindow) => void;
export type WindowCloseListener = (window: WaylandWindow) => void;
export type WindowFocusListener = (
  window: WaylandWindow,
  focused: boolean,
) => void;
export type WindowStartCloseListener = (window: WaylandWindow) => void;
export type LayerCreateListener = (layer: WaylandLayer) => void;
export type LayerUpdateListener = (layer: WaylandLayer) => void;
export type LayerDestroyListener = (layer: WaylandLayer) => void;
export type RuntimeLifecycleReason = "initial" | "reload" | "shutdown";

export type RuntimePersistedState = Record<string, unknown>;

export interface RuntimeEnableEvent {
  readonly reason: Extract<RuntimeLifecycleReason, "initial" | "reload">;
  readonly isReloading: boolean;
  restore<T>(key: string): T | undefined;
  has(key: string): boolean;
}

export interface RuntimeDisableEvent {
  readonly reason: Extract<RuntimeLifecycleReason, "reload" | "shutdown">;
  readonly isReloading: boolean;
  persist<T>(key: string, value: T): void;
  delete(key: string): void;
}

export type RuntimeEnableListener = (event: RuntimeEnableEvent) => void;
export type RuntimeDisableListener = (event: RuntimeDisableEvent) => void;

export interface OutputChangeEvent {
  outputs: OutputInfo[];
  current: Record<string, OutputInfo>;
  added: OutputInfo[];
  removed: OutputInfo[];
  changed: OutputInfo[];
}

export type OutputChangeListener = (event: OutputChangeEvent) => void;

export interface InputDeviceChangeEvent {
  devices: InputDeviceInfo[];
  current: Record<string, InputDeviceInfo>;
  added: InputDeviceInfo[];
  removed: InputDeviceInfo[];
  changed: InputDeviceInfo[];
}

export type InputDeviceChangeListener = (event: InputDeviceChangeEvent) => void;

export interface WindowResizeEdges {
  left: boolean;
  right: boolean;
  top: boolean;
  bottom: boolean;
}

export interface WindowResizePoint {
  x: number;
  y: number;
}

export interface WindowResizeRect {
  x: number;
  y: number;
  width: number;
  height: number;
}

export type WindowResizeSource = "ssd" | "client-csd" | "xwayland";
export type WindowResizePhase = "start" | "update" | "end" | "cancel";

export interface WindowResizeEvent {
  window: WaylandWindow;
  source: WindowResizeSource;
  phase: WindowResizePhase;
  edges: WindowResizeEdges;
  startPointer: WindowResizePoint;
  currentPointer: WindowResizePoint;
  delta: WindowResizePoint;
  startRect: WindowResizeRect;
  currentRect: WindowResizeRect;
  outputName?: string;
  timestamp: number;
}

export type WindowResizeListener = (event: WindowResizeEvent) => void;

export type RuntimeWindowResizeEvent = Omit<WindowResizeEvent, "window">;

export interface WindowMovePoint {
  x: number;
  y: number;
}

export interface WindowMoveRect {
  x: number;
  y: number;
  width: number;
  height: number;
}

export type WindowMoveSource = "ssd" | "modifier" | "client-csd" | "xwayland";
export type WindowMovePhase = "start" | "update" | "end" | "cancel";

export interface WindowMoveEvent {
  window: WaylandWindow;
  source: WindowMoveSource;
  phase: WindowMovePhase;
  startPointer: WindowMovePoint;
  currentPointer: WindowMovePoint;
  delta: WindowMovePoint;
  startRect: WindowMoveRect;
  currentRect: WindowMoveRect;
  outputName?: string;
  modifiers: PointerModifierState;
  timestamp: number;
}

export type WindowMoveListener = (event: WindowMoveEvent) => void;

export type RuntimeWindowMoveEvent = Omit<WindowMoveEvent, "window">;

export type WindowStateRequestSource =
  | "api"
  | "client-csd"
  | "xdg-activation"
  | "xwayland"
  | "keybind";
export type WindowActivateRequestSource =
  | "api"
  | "xdg-activation"
  | "xwayland"
  | "keybind";

export interface WindowMaximizeRequestEvent {
  window: WaylandWindow;
  maximized: boolean;
  source: WindowStateRequestSource;
  timestamp: number;
}

export type WindowMaximizeRequestListener = (
  event: WindowMaximizeRequestEvent,
) => void;

export type RuntimeWindowMaximizeRequestEvent = Omit<
  WindowMaximizeRequestEvent,
  "window"
>;

export interface WindowMinimizeRequestEvent {
  window: WaylandWindow;
  minimized: boolean;
  source: WindowStateRequestSource;
  timestamp: number;
}

export type WindowMinimizeRequestListener = (
  event: WindowMinimizeRequestEvent,
) => void;

export type RuntimeWindowMinimizeRequestEvent = Omit<
  WindowMinimizeRequestEvent,
  "window"
>;

export interface WindowFullscreenRequestEvent {
  window: WaylandWindow;
  fullscreen: boolean;
  /**
   * Output the client asked to go fullscreen on. The protocol's wl_output
   * argument is optional, so this can be undefined — let the window manager
   * pick a sensible default (e.g. the window's current output).
   */
  outputName?: string;
  source: WindowStateRequestSource;
  timestamp: number;
}

export type WindowFullscreenRequestListener = (
  event: WindowFullscreenRequestEvent,
) => void;

export type RuntimeWindowFullscreenRequestEvent = Omit<
  WindowFullscreenRequestEvent,
  "window"
>;

export interface WindowActivateRequestEvent {
  window: WaylandWindow;
  source: WindowActivateRequestSource;
  timestamp: number;
}

export type WindowActivateRequestListener = (
  event: WindowActivateRequestEvent,
) => void;

export type RuntimeWindowActivateRequestEvent = Omit<
  WindowActivateRequestEvent,
  "window"
>;

export interface PointerMovePoint {
  x: number;
  y: number;
}

export interface PointerModifierState {
  super: boolean;
  alt: boolean;
  ctrl: boolean;
  shift: boolean;
}

export type PointerHitTarget =
  | { kind: "none" }
  | { kind: "window"; windowId: string }
  | { kind: "layer"; layerId: string };

export interface PointerMoveEvent {
  position: PointerMovePoint;
  delta: PointerMovePoint;
  target: PointerHitTarget;
  outputName?: string;
  modifiers: PointerModifierState;
  timestamp: number;
}

export type PointerMoveListener = (event: PointerMoveEvent) => void;

export type PointerMoveAsyncListener = (
  event: PointerMoveEvent,
) => void | Promise<void>;

export type GestureSwipePhase = "begin" | "update" | "end" | "cancel";

export interface GestureSwipeEvent {
  phase: GestureSwipePhase;
  fingers: number;
  position?: PointerMovePoint;
  deltaX: number;
  deltaY: number;
  totalX: number;
  totalY: number;
  velocityX: number;
  velocityY: number;
  outputName?: string;
  device?: InputDeviceInfo;
  timestamp: number;
}

export type GestureSwipeListener = (event: GestureSwipeEvent) => void;

export type GestureSwipeAsyncListener = (
  event: GestureSwipeEvent,
) => void | Promise<void>;

export interface RuntimeEventConfig {
  pointerMove: boolean;
  pointerMoveAsync: boolean;
  gestureSwipe: boolean;
  gestureSwipeAsync: boolean;
}

/**
 * Event bus for compositor and window lifecycle events. All `on*` methods
 * return an unsubscribe function; call it to remove the listener.
 * コンポジターとウィンドウのライフサイクルイベントバス。すべての `on*` メソッドは
 * 解除関数を返します。呼び出すとリスナーが削除されます。
 *
 * @example Window lifecycle / ウィンドウライフサイクル
 * ```ts
 * const unsubOpen = COMPOSITOR.event.onOpen((window) => {
 *   console.log("opened", window.id, window.appId);
 * });
 * COMPOSITOR.event.onClose((window) => unsubOpen()); // remove after first close
 * ```
 */
export interface CompositorEventController {
  /**
   * Fires when the compositor config is enabled or reloaded.
   * Use `event.restore` to recover state persisted by `onDisable`.
   * 設定が有効化またはリロードされたときに発火します。
   * `event.restore` で `onDisable` が保存した状態を復元できます。
   *
   * @example
   * ```ts
   * COMPOSITOR.event.onEnable((event) => {
   *   const saved = event.restore<MyState>("my-state");
   *   if (saved) restore(saved);
   * });
   * ```
   */
  onEnable(listener: RuntimeEnableListener): () => void;
  /**
   * Fires when the compositor config is disabled or about to be reloaded.
   * Use `event.persist` to save state that survives a hot-reload.
   * 設定が無効化またはリロード前に発火します。
   * `event.persist` でホットリロードをまたいで状態を保存できます。
   *
   * @example
   * ```ts
   * COMPOSITOR.event.onDisable((event) => {
   *   event.persist("my-state", snapshot());
   * });
   * ```
   */
  onDisable(listener: RuntimeDisableListener): () => void;
  /**
   * Fires when a new toplevel window is mapped (becomes visible).
   * 新しいトップレベルウィンドウがマップされた（表示された）ときに発火します。
   *
   * @example
   * ```ts
   * COMPOSITOR.event.onOpen((window) => {
   *   hybridWM.onOpen(window);
   * });
   * ```
   */
  onOpen(listener: WindowOpenListener): () => void;
  /**
   * Fires after the compositor sends the first xdg_toplevel configure to a new
   * window (before the client has committed any content). Useful for setting
   * initial geometry.
   * コンポジターが新しいウィンドウに最初の xdg_toplevel configure を送った後に発火します
   * （クライアントがコンテンツをコミットする前）。初期ジオメトリの設定に便利です。
   */
  onInitialConfigure(listener: WindowInitialConfigureListener): () => void;
  /**
   * Fires when the client commits its first frame (window content is ready to display).
   * クライアントが最初のフレームをコミットしたとき（ウィンドウコンテンツが表示準備完了）に発火します。
   *
   * @example
   * ```ts
   * COMPOSITOR.event.onFirstCommit((window) => {
   *   // Start open animation when the window first draws itself
   *   window.animation.start(openVar, { from: 0, to: 1, duration: ms(180) });
   * });
   * ```
   */
  onFirstCommit(listener: WindowFirstCommitListener): () => void;
  /**
   * Fires when a toplevel window is fully closed and unmapped.
   * トップレベルウィンドウが完全に閉じてアンマップされたときに発火します。
   */
  onClose(listener: WindowCloseListener): () => void;
  /**
   * Fires when a window gains or loses keyboard focus.
   * `focused` is `true` when the window gains focus.
   * ウィンドウがキーボードフォーカスを得たまたは失ったときに発火します。
   * `focused` がフォーカスを得たときは `true` です。
   *
   * @example
   * ```ts
   * COMPOSITOR.event.onFocus((window, focused) => {
   *   window.animation.start(focusVar, { to: focused ? 1 : 0, duration: ms(120) });
   * });
   * ```
   */
  onFocus(listener: WindowFocusListener): () => void;
  /**
   * Fires when a window begins its close sequence (before the client tears down
   * its surface). Use this to start close animations while the surface is still
   * alive.
   * ウィンドウが閉じるシーケンスを開始したとき（クライアントがサーフェスを破棄する前）に
   * 発火します。サーフェスがまだ生きている間に閉じるアニメーションを開始するのに使います。
   *
   * @example
   * ```ts
   * COMPOSITOR.event.onStartClose((window) => {
   *   window.animation.start(openVar, { to: 0, duration: ms(150) });
   *   window.actions.setCloseAnimationDuration(150);
   * });
   * ```
   */
  onStartClose(listener: WindowStartCloseListener): () => void;
  /**
   * Fires on each phase (`start`, `update`, `end`, `cancel`) of an
   * interactive window resize initiated by the user.
   * ユーザーが開始したインタラクティブなウィンドウリサイズの各フェーズ
   * （`start`・`update`・`end`・`cancel`）で発火します。
   */
  onWindowResize(listener: WindowResizeListener): () => void;
  /**
   * Fires on each phase of an interactive window move.
   * インタラクティブなウィンドウ移動の各フェーズで発火します。
   */
  onWindowMove(listener: WindowMoveListener): () => void;
  /**
   * Fires when a client or keybind requests a window maximize/unmaximize.
   * クライアントまたはキーバインドがウィンドウの最大化・最大化解除を要求したときに発火します。
   *
   * @example
   * ```ts
   * COMPOSITOR.event.onWindowMaximizeRequest((event) => {
   *   hybridWM.onWindowMaximizeRequest(event);
   * });
   * ```
   */
  onWindowMaximizeRequest(listener: WindowMaximizeRequestListener): () => void;
  /**
   * Fires when a client requests a window minimize.
   * クライアントがウィンドウの最小化を要求したときに発火します。
   */
  onWindowMinimizeRequest(listener: WindowMinimizeRequestListener): () => void;
  /**
   * Fires when a client or keybind requests fullscreen or unfullscreen.
   * `event.outputName` is the preferred output (may be `undefined`).
   * クライアントまたはキーバインドがフルスクリーン・フルスクリーン解除を要求したときに発火します。
   * `event.outputName` は希望する出力（`undefined` の場合もあります）。
   */
  onWindowFullscreenRequest(
    listener: WindowFullscreenRequestListener,
  ): () => void;
  /**
   * Fires when a client requests activation (focus steal) via `xdg-activation`
   * or `xwayland`.
   * クライアントが `xdg-activation` や `xwayland` 経由でアクティベーション（フォーカス奪取）
   * を要求したときに発火します。
   */
  onWindowActivateRequest(listener: WindowActivateRequestListener): () => void;
  /**
   * Fires when the set of connected outputs changes (hotplug, resolution change, etc.).
   * 接続中の出力セットが変わったとき（ホットプラグ・解像度変更など）に発火します。
   *
   * @example
   * ```ts
   * COMPOSITOR.event.onOutputChange((event) => {
   *   hybridWM.onOutputChange(event);
   * });
   * ```
   */
  onOutputChange(listener: OutputChangeListener): () => void;
  /**
   * Fires when the set of connected input devices changes (hotplug).
   * 接続中の入力デバイスセットが変わったとき（ホットプラグ）に発火します。
   */
  onInputDeviceChange(listener: InputDeviceChangeListener): () => void;
  /**
   * Fires synchronously on every pointer move event.
   * Keep the listener short: input processing waits for it to return.
   * ポインター移動イベントのたびに同期的に発火します。
   * 入力処理はリスナーの完了を待つため、短い処理に留めてください。
   */
  onPointerMove(listener: PointerMoveListener): () => void;
  /**
   * Fires asynchronously on every pointer move event.
   * The listener may return a `Promise`; the compositor awaits it before
   * completing the runtime dispatch.
   * ポインター移動イベントのたびに非同期で発火します。リスナーは `Promise` を返せます。
   *
   * @example
   * ```ts
   * COMPOSITOR.event.onPointerMoveAsync((event) => {
   *   hybridWM.onPointerMove(event);
   * });
   * ```
   */
  onPointerMoveAsync(listener: PointerMoveAsyncListener): () => void;
  /**
   * Fires synchronously on each phase of a multi-finger touchpad swipe.
   * Keep the listener short: input processing waits for it to return.
   * マルチフィンガータッチパッドスワイプの各フェーズで同期的に発火します。
   * 入力処理はリスナーの完了を待つため、短い処理に留めてください。
   */
  onGestureSwipe(listener: GestureSwipeListener): () => void;
  /**
   * Fires asynchronously on each phase of a multi-finger touchpad swipe gesture.
   * マルチフィンガータッチパッドスワイプジェスチャーの各フェーズで非同期に発火します。
   *
   * @example
   * ```ts
   * COMPOSITOR.event.onGestureSwipeAsync((event) => {
   *   hybridWM.onGestureSwipe(event);
   * });
   * ```
   */
  onGestureSwipeAsync(listener: GestureSwipeAsyncListener): () => void;
  /**
   * Fires when a new layer-shell surface is created.
   * 新しいレイヤーシェルサーフェスが作成されたときに発火します。
   */
  onCreateLayer(listener: LayerCreateListener): () => void;
  /**
   * Fires when a layer-shell surface's committed state changes (anchor, exclusive
   * zone, size, …).
   * レイヤーシェルサーフェスのコミット済み状態が変わったときに発火します。
   */
  onUpdateLayer(listener: LayerUpdateListener): () => void;
  /**
   * Fires when a layer-shell surface is destroyed.
   * レイヤーシェルサーフェスが破棄されたときに発火します。
   */
  onDestroyLayer(listener: LayerDestroyListener): () => void;

  /** @internal Called by the compositor runtime, not config code. */
  emitOpen(window: WaylandWindow): void;
  /** @internal */
  emitInitialConfigure(window: WaylandWindow): void;
  /** @internal */
  emitFirstCommit(window: WaylandWindow): void;
  /** @internal */
  emitClose(window: WaylandWindow): void;
  /** @internal */
  emitFocus(window: WaylandWindow, focused: boolean): void;
  /** @internal */
  emitStartClose(window: WaylandWindow): void;
  /** @internal */
  emitWindowResize(
    window: WaylandWindow,
    event: RuntimeWindowResizeEvent,
  ): boolean;
  /** @internal */
  emitWindowMove(window: WaylandWindow, event: RuntimeWindowMoveEvent): boolean;
  /** @internal */
  emitWindowMaximizeRequest(
    window: WaylandWindow,
    event: RuntimeWindowMaximizeRequestEvent,
  ): boolean;
  /** @internal */
  emitWindowMinimizeRequest(
    window: WaylandWindow,
    event: RuntimeWindowMinimizeRequestEvent,
  ): boolean;
  /** @internal */
  emitWindowFullscreenRequest(
    window: WaylandWindow,
    event: RuntimeWindowFullscreenRequestEvent,
  ): boolean;
  /** @internal */
  emitWindowActivateRequest(
    window: WaylandWindow,
    event: RuntimeWindowActivateRequestEvent,
  ): boolean;
  /** @internal */
  emitOutputChange(event: OutputChangeEvent): void;
  /** @internal */
  emitInputDeviceChange(event: InputDeviceChangeEvent): void;
  /** @internal */
  emitPointerMove(event: PointerMoveEvent): boolean;
  /** @internal */
  emitPointerMoveAsync(event: PointerMoveEvent): Promise<boolean>;
  /** @internal */
  emitGestureSwipe(event: GestureSwipeEvent): boolean;
  /** @internal */
  emitGestureSwipeAsync(event: GestureSwipeEvent): Promise<boolean>;
  /** @internal */
  emitCreateLayer(layer: WaylandLayer): void;
  /** @internal */
  emitUpdateLayer(layer: WaylandLayer): void;
  /** @internal */
  emitDestroyLayer(layer: WaylandLayer): void;
  /** @internal */
  emitEnable(
    reason: RuntimeEnableEvent["reason"],
    state?: RuntimePersistedState,
  ): void;
  /** @internal */
  emitDisable(reason: RuntimeDisableEvent["reason"]): RuntimePersistedState;
  /** @internal */
  takePendingEventConfig(): RuntimeEventConfig | undefined;
}

export function createCompositorEventController(): CompositorEventController {
  const enableListeners = new Set<RuntimeEnableListener>();
  const disableListeners = new Set<RuntimeDisableListener>();
  const openListeners = new Set<WindowOpenListener>();
  const initialConfigureListeners = new Set<WindowInitialConfigureListener>();
  const firstCommitListeners = new Set<WindowFirstCommitListener>();
  const closeListeners = new Set<WindowCloseListener>();
  const focusListeners = new Set<WindowFocusListener>();
  const startCloseListeners = new Set<WindowStartCloseListener>();
  const resizeListeners = new Set<WindowResizeListener>();
  const moveListeners = new Set<WindowMoveListener>();
  const maximizeRequestListeners = new Set<WindowMaximizeRequestListener>();
  const minimizeRequestListeners = new Set<WindowMinimizeRequestListener>();
  const fullscreenRequestListeners =
    new Set<WindowFullscreenRequestListener>();
  const activateRequestListeners = new Set<WindowActivateRequestListener>();
  const outputChangeListeners = new Set<OutputChangeListener>();
  const inputDeviceChangeListeners = new Set<InputDeviceChangeListener>();
  const pointerMoveListeners = new Set<PointerMoveListener>();
  const pointerMoveAsyncListeners = new Set<PointerMoveAsyncListener>();
  const gestureSwipeListeners = new Set<GestureSwipeListener>();
  const gestureSwipeAsyncListeners = new Set<GestureSwipeAsyncListener>();
  const createLayerListeners = new Set<LayerCreateListener>();
  const updateLayerListeners = new Set<LayerUpdateListener>();
  const destroyLayerListeners = new Set<LayerDestroyListener>();
  let pendingEventConfig = false;

  function markEventConfigDirty(): void {
    pendingEventConfig = true;
  }

  return {
    onEnable(listener) {
      enableListeners.add(listener);
      return () => enableListeners.delete(listener);
    },
    onDisable(listener) {
      disableListeners.add(listener);
      return () => disableListeners.delete(listener);
    },
    onOpen(listener) {
      openListeners.add(listener);
      return () => openListeners.delete(listener);
    },
    onInitialConfigure(listener) {
      initialConfigureListeners.add(listener);
      return () => initialConfigureListeners.delete(listener);
    },
    onFirstCommit(listener) {
      firstCommitListeners.add(listener);
      return () => firstCommitListeners.delete(listener);
    },
    onClose(listener) {
      closeListeners.add(listener);
      return () => closeListeners.delete(listener);
    },
    onFocus(listener) {
      focusListeners.add(listener);
      return () => focusListeners.delete(listener);
    },
    onStartClose(listener) {
      startCloseListeners.add(listener);
      return () => startCloseListeners.delete(listener);
    },
    onWindowResize(listener) {
      resizeListeners.add(listener);
      return () => resizeListeners.delete(listener);
    },
    onWindowMove(listener) {
      moveListeners.add(listener);
      return () => moveListeners.delete(listener);
    },
    onWindowMaximizeRequest(listener) {
      maximizeRequestListeners.add(listener);
      return () => maximizeRequestListeners.delete(listener);
    },
    onWindowMinimizeRequest(listener) {
      minimizeRequestListeners.add(listener);
      return () => minimizeRequestListeners.delete(listener);
    },
    onWindowFullscreenRequest(listener) {
      fullscreenRequestListeners.add(listener);
      return () => fullscreenRequestListeners.delete(listener);
    },
    onWindowActivateRequest(listener) {
      activateRequestListeners.add(listener);
      return () => activateRequestListeners.delete(listener);
    },
    onOutputChange(listener) {
      outputChangeListeners.add(listener);
      return () => outputChangeListeners.delete(listener);
    },
    onInputDeviceChange(listener) {
      inputDeviceChangeListeners.add(listener);
      return () => inputDeviceChangeListeners.delete(listener);
    },
    onPointerMove(listener) {
      pointerMoveListeners.add(listener);
      markEventConfigDirty();
      return () => {
        pointerMoveListeners.delete(listener);
        markEventConfigDirty();
      };
    },
    onPointerMoveAsync(listener) {
      pointerMoveAsyncListeners.add(listener);
      markEventConfigDirty();
      return () => {
        pointerMoveAsyncListeners.delete(listener);
        markEventConfigDirty();
      };
    },
    onGestureSwipe(listener) {
      gestureSwipeListeners.add(listener);
      markEventConfigDirty();
      return () => {
        gestureSwipeListeners.delete(listener);
        markEventConfigDirty();
      };
    },
    onGestureSwipeAsync(listener) {
      gestureSwipeAsyncListeners.add(listener);
      markEventConfigDirty();
      return () => {
        gestureSwipeAsyncListeners.delete(listener);
        markEventConfigDirty();
      };
    },
    onCreateLayer(listener) {
      createLayerListeners.add(listener);
      return () => createLayerListeners.delete(listener);
    },
    onUpdateLayer(listener) {
      updateLayerListeners.add(listener);
      return () => updateLayerListeners.delete(listener);
    },
    onDestroyLayer(listener) {
      destroyLayerListeners.add(listener);
      return () => destroyLayerListeners.delete(listener);
    },
    emitOpen(window) {
      for (const listener of openListeners) {
        listener(window);
      }
    },
    emitInitialConfigure(window) {
      for (const listener of initialConfigureListeners) {
        listener(window);
      }
    },
    emitFirstCommit(window) {
      for (const listener of firstCommitListeners) {
        listener(window);
      }
    },
    emitClose(window) {
      for (const listener of closeListeners) {
        listener(window);
      }
    },
    emitFocus(window, focused) {
      for (const listener of focusListeners) {
        listener(window, focused);
      }
    },
    emitStartClose(window) {
      for (const listener of startCloseListeners) {
        listener(window);
      }
    },
    emitWindowResize(window, event) {
      if (resizeListeners.size === 0) {
        return false;
      }
      for (const listener of resizeListeners) {
        listener({ ...event, window });
      }
      return true;
    },
    emitWindowMove(window, event) {
      if (moveListeners.size === 0) {
        return false;
      }
      for (const listener of moveListeners) {
        listener({ ...event, window });
      }
      return true;
    },
    emitWindowMaximizeRequest(window, event) {
      if (maximizeRequestListeners.size === 0) {
        return false;
      }
      for (const listener of maximizeRequestListeners) {
        listener({ ...event, window });
      }
      return true;
    },
    emitWindowMinimizeRequest(window, event) {
      if (minimizeRequestListeners.size === 0) {
        return false;
      }
      for (const listener of minimizeRequestListeners) {
        listener({ ...event, window });
      }
      return true;
    },
    emitWindowFullscreenRequest(window, event) {
      if (fullscreenRequestListeners.size === 0) {
        return false;
      }
      for (const listener of fullscreenRequestListeners) {
        listener({ ...event, window });
      }
      return true;
    },
    emitWindowActivateRequest(window, event) {
      if (activateRequestListeners.size === 0) {
        return false;
      }
      for (const listener of activateRequestListeners) {
        listener({ ...event, window });
      }
      return true;
    },
    emitOutputChange(event) {
      for (const listener of outputChangeListeners) {
        listener(event);
      }
    },
    emitInputDeviceChange(event) {
      for (const listener of inputDeviceChangeListeners) {
        listener(event);
      }
    },
    emitPointerMove(event) {
      if (pointerMoveListeners.size === 0) {
        return false;
      }
      for (const listener of pointerMoveListeners) {
        listener(event);
      }
      return true;
    },
    async emitPointerMoveAsync(event) {
      if (pointerMoveAsyncListeners.size === 0) {
        return false;
      }
      for (const listener of pointerMoveAsyncListeners) {
        await listener(event);
      }
      return true;
    },
    emitGestureSwipe(event) {
      if (gestureSwipeListeners.size === 0) {
        return false;
      }
      for (const listener of gestureSwipeListeners) {
        listener(event);
      }
      return true;
    },
    async emitGestureSwipeAsync(event) {
      if (gestureSwipeAsyncListeners.size === 0) {
        return false;
      }
      for (const listener of gestureSwipeAsyncListeners) {
        await listener(event);
      }
      return true;
    },
    emitCreateLayer(layer) {
      for (const listener of createLayerListeners) {
        listener(layer);
      }
    },
    emitUpdateLayer(layer) {
      for (const listener of updateLayerListeners) {
        listener(layer);
      }
    },
    emitDestroyLayer(layer) {
      for (const listener of destroyLayerListeners) {
        listener(layer);
      }
    },
    emitEnable(reason, state = {}) {
      const snapshot = { ...state };
      const event: RuntimeEnableEvent = {
        reason,
        isReloading: reason === "reload",
        restore(key) {
          return snapshot[key] as never;
        },
        has(key) {
          return Object.prototype.hasOwnProperty.call(snapshot, key);
        },
      };
      for (const listener of enableListeners) {
        listener(event);
      }
    },
    emitDisable(reason) {
      const next: RuntimePersistedState = {};
      const event: RuntimeDisableEvent = {
        reason,
        isReloading: reason === "reload",
        persist(key, value) {
          next[key] = value;
        },
        delete(key) {
          delete next[key];
        },
      };
      for (const listener of disableListeners) {
        listener(event);
      }
      return next;
    },
    takePendingEventConfig() {
      if (!pendingEventConfig) {
        return undefined;
      }
      pendingEventConfig = false;
      return {
        pointerMove: pointerMoveListeners.size > 0,
        pointerMoveAsync: pointerMoveAsyncListeners.size > 0,
        gestureSwipe: gestureSwipeListeners.size > 0,
        gestureSwipeAsync: gestureSwipeAsyncListeners.size > 0,
      };
    },
  };
}
