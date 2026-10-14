import {
  createAnimationController,
  type AnimationController,
} from "./animation";
import { markRuntimeDirty, withoutCompositionOwnership } from "./runtime-hooks";
import {
  createComponentStateStore,
  createElementNode,
  withComponentRenderRoot,
} from "./runtime";
import {
  serializeCompositionTree,
  type CompositionSerializationContext,
} from "./serialize";
import { effect as watch, read, signal, type ReadonlySignal } from "./signals";
import type {
  CompositionChild,
  CompositionRenderable,
  MaybeSignal,
  WaylandWindow,
} from "./types";

/**
 * What a drop would do, as the source and the target agreed: copy, move, ask
 * the user, or nothing (no target accepts the data).
 * ドロップしたときの操作（ソースとターゲットの合意結果）。コピー・移動・ユーザーに
 * 確認・なし（受け入れるターゲットがない）。
 */
export type DragAction = "none" | "copy" | "move" | "ask";

/** The actions the dragged data allows. / ドラッグ中のデータが許す操作。 */
export interface DragActions {
  copy: boolean;
  move: boolean;
  ask: boolean;
}

export interface DragPoint {
  x: number;
  y: number;
}

/**
 * The image the application drags along, if it gave one.
 * アプリがドラッグ中に表示する画像（指定された場合）。
 */
export interface DragIconInfo {
  /** Logical size. / 論理サイズ。 */
  width: number;
  height: number;
  /**
   * Where the icon's top-left corner sits relative to the pointer (usually
   * negative: the pointer is inside the icon).
   * ポインターから見たアイコン左上の位置（通常は負の値：ポインターはアイコンの内側）。
   */
  offsetX: number;
  offsetY: number;
}

/** How a drag ended. / ドラッグの終わり方。 */
export interface DragEnd {
  /**
   * `"dropped"`: the button was released (over a target or not).
   * `"cancelled"`: the drag ended some other way (the application gave up).
   * `"dropped"`: ボタンが離された（ターゲットの上かどうかは問わない）。
   * `"cancelled"`: それ以外で終わった（アプリが中断した等）。
   */
  kind: "dropped" | "cancelled";
  /**
   * The target accepted the data: the drop goes through. A drop the target
   * refused (or one over nothing) is `false`.
   * ターゲットがデータを受け入れた（ドロップ成立）。拒否・何もない所へのドロップは `false`。
   */
  accepted: boolean;
  /** The window the drop landed on. / ドロップ先のウィンドウ。 */
  target: WaylandWindow | null;
  /**
   * The pointer when the drag ended (global logical coordinates).
   * ドラッグ終了時のポインター位置（グローバル論理座標）。
   */
  position: DragPoint;
}

/**
 * How a drag view is placed and drawn. Every value may be a signal, so an
 * animation can drive it.
 * ドラッグビューの配置と描画。すべてシグナルにでき、アニメーションで動かせます。
 */
export interface DragViewOptions {
  /**
   * `"pointer"` (default): `x` / `y` offset the view from the pointer, which
   * the compositor follows on its own (moving the mouse costs the config
   * nothing). `"global"`: `x` / `y` are global logical coordinates.
   * `"pointer"`（既定）: `x` / `y` はポインターからのずれ。追従はコンポジターが行う
   * （マウス移動で設定側の処理は走らない）。`"global"`: `x` / `y` はグローバル論理座標。
   */
  anchor?: MaybeSignal<"pointer" | "global">;
  /**
   * Where the view's anchor point goes. The anchor point is the pointer's spot
   * in the view: with `anchor: "pointer"` and `x = y = 0` the icon sits under
   * the pointer exactly where the application put it.
   * ビューのアンカー点の位置。アンカー点はビュー内でポインターが当たる点で、
   * `anchor: "pointer"` かつ `x = y = 0` ならアイコンはアプリが指定した位置に来ます。
   */
  x?: MaybeSignal<number>;
  y?: MaybeSignal<number>;
  /** Opacity of the whole view, 0..1. / ビュー全体の不透明度（0〜1）。 */
  opacity?: MaybeSignal<number>;
  /** Scale around the anchor point. / アンカー点を中心とした拡大率。 */
  scale?: MaybeSignal<number>;
  /**
   * What to draw: a tree like a window decoration, with `<DragIcon/>` where
   * the application's icon goes. A function is re-run whenever a signal it
   * reads changes (`drag.action`, ...). Omitted: the icon alone.
   * Paint shaders and `<ShaderEffect>` work as in decorations; an effect on
   * `backdropSource()` sees the windows and the view's own parts behind it.
   * While `scale` is not 1, what an effect shows is slightly off from what is
   * behind it. Interactive parts (`onClick`) are ignored, and
   * `windowSource()` has nothing to show.
   * 描く内容。ウィンドウ装飾と同じツリーで、アプリのアイコンを置く場所に
   * `<DragIcon/>` を書きます。関数にすると、読んだシグナル（`drag.action` 等）が
   * 変わるたびに再実行されます。省略時はアイコンのみ。paint シェーダーと
   * `<ShaderEffect>` は装飾と同じく使え、`backdropSource()` のエフェクトには
   * 背後のウィンドウとビュー自身の背後の部分が映ります。`scale` が 1 以外の間は
   * 映る内容が背後と少しずれます。`onClick` などの操作は無視され、
   * `windowSource()` には何も映りません。
   */
  content?: CompositionRenderable | (() => CompositionRenderable);
  /**
   * How long (ms) the view may stay after the drag ended before the
   * compositor removes it, in case `dispose()` never comes. Default 5000.
   * ドラッグ終了後、`dispose()` されないまま残れる時間（ミリ秒）。既定 5000。
   */
  maxLinger?: number;
}

/** A shown drag view. / 表示中のドラッグビュー。 */
export interface DragView {
  /** Remove the view. / ビューを消します。 */
  dispose(): void;
  readonly disposed: boolean;
  /** Resolves once the view is disposed of. / 破棄されると解決します。 */
  readonly closed: Promise<void>;
}

/**
 * One drag, from start to end, and the views drawn for it.
 * 1回のドラッグ（開始から終了まで）と、そのために描くビュー。
 */
export interface DragSession {
  readonly id: number;
  /**
   * The formats the data is offered in. The data itself is never exposed.
   * データが提供される形式。データの中身は公開されません。
   */
  readonly mimeTypes: readonly string[];
  readonly actions: DragActions;
  /** The action a drop would take now. / 今ドロップした場合の操作。 */
  readonly action: ReadonlySignal<DragAction>;
  /** The window under the pointer. / ポインターの下のウィンドウ。 */
  readonly target: ReadonlySignal<WaylandWindow | null>;
  /** The window the drag started from. / ドラッグ元のウィンドウ。 */
  readonly sourceWindow: WaylandWindow | null;
  readonly device: "pointer" | "touch";
  /** The pointer when the drag started. / ドラッグ開始時のポインター位置。 */
  readonly origin: DragPoint;
  /** The application's icon; `null` without one. / アプリのアイコン（無ければ `null`）。 */
  readonly icon: ReadonlySignal<DragIconInfo | null>;
  /** `null` until the drag ends. / ドラッグが終わるまで `null`。 */
  readonly ended: ReadonlySignal<DragEnd | null>;
  /**
   * Animations of this drag, like `window.animation`.
   * このドラッグのアニメーション（`window.animation` と同じ）。
   */
  readonly animation: AnimationController;
  /**
   * Draw a view for this drag. It stays until `dispose()`, also after the
   * drag ended, so a drop or cancel can animate. Call it again for another view.
   * このドラッグのビューを描きます。ドラッグ終了後も `dispose()` まで残るので、
   * ドロップやキャンセルをアニメーションできます。
   */
  show(options?: DragViewOptions): DragView;
  /**
   * Fires once, when the drag ends. Returns an unsubscribe function.
   * ドラッグ終了時に1回呼ばれます。解除関数を返します。
   */
  onEnd(listener: (end: DragEnd) => void): () => void;
}

export type DragStartListener = (drag: DragSession) => void;

export interface DndEventController {
  /**
   * A drag started. While any listener is registered, the compositor draws
   * only the views the listeners show; with none, it draws the application's
   * icon under the pointer.
   * ドラッグが始まりました。リスナーがある間はリスナーが `show()` したビューだけが
   * 描かれます。リスナーが無ければアプリのアイコンをポインター位置に描きます。
   */
  onStart(listener: DragStartListener): () => void;
}

/** `COMPOSITOR.dnd`: drag and drop. / ドラッグアンドドロップ。 */
export interface DndController {
  readonly event: DndEventController;
  /** The drag in progress, or `null`. / 進行中のドラッグ（無ければ `null`）。 */
  readonly current: ReadonlySignal<DragSession | null>;
}

interface WireDragIcon {
  width: number;
  height: number;
  offsetX: number;
  offsetY: number;
}

interface WireDrag {
  id: number;
  mimeTypes: string[];
  actions: DragActions;
  action: DragAction;
  sourceWindowId: string | null;
  targetWindowId: string | null;
  device: "pointer" | "touch";
  origin: DragPoint;
  icon: WireDragIcon | null;
}

/** @internal What the compositor sends. */
export type DndEvent =
  | { phase: "start"; drag: WireDrag }
  | {
      phase: "update";
      dragId: number;
      action: DragAction;
      targetWindowId: string | null;
      icon: WireDragIcon | null;
    }
  | {
      phase: "end";
      dragId: number;
      kind: "dropped" | "cancelled";
      accepted: boolean;
      targetWindowId: string | null;
      position: DragPoint;
    };

type NativeDragViewCommand =
  | { op: "show"; viewId: number; dragId: number; maxLingerMs?: number }
  | {
      op: "props";
      viewId: number;
      props: {
        anchor: "pointer" | "global";
        x: number;
        y: number;
        opacity: number;
        scale: number;
      };
    }
  | { op: "content"; viewId: number; tree: unknown }
  | { op: "dispose"; viewId: number };

function native(command: NativeDragViewCommand): void {
  (
    globalThis as {
      __SHOJI_DND_VIEW__?: (command: NativeDragViewCommand) => void;
    }
  ).__SHOJI_DND_VIEW__?.(command);
}

const DEFAULT_MAX_LINGER_MS = 5000;

const startListeners = new Set<DragStartListener>();
let listenersChanged = false;
let resolveWindow: (id: string) => WaylandWindow | null = () => null;
let nextViewId = 1;

interface SessionState {
  session: DragSession;
  setAction(action: DragAction): void;
  setTarget(target: WaylandWindow | null): void;
  setIcon(icon: DragIconInfo | null): void;
  end(end: DragEnd): void;
  views: Set<ViewState>;
}

interface ViewState {
  view: DragView;
  dispose(): void;
  /** Wall-clock deadline once the drag ended, after which the compositor dropped it. */
  deadlineMs?: number;
  maxLingerMs: number;
}

const sessions = new Map<number, SessionState>();
const [currentDrag, setCurrentDrag] = signal<DragSession | null>(null);

// Labels and images in a drag view are never clicked: the pointer is busy
// dragging. Handlers are accepted and ignored.
const VIEW_SERIALIZATION_CONTEXT: CompositionSerializationContext = {
  registerClickHandler: () => "dnd-view-noop",
  registerInteractionHandler: () => "dnd-view-noop",
  registerShaderUniformBinding: () => {},
};

function iconInfo(icon: WireDragIcon | null): DragIconInfo | null {
  if (!icon || icon.width <= 0 || icon.height <= 0) {
    return null;
  }
  return {
    width: icon.width,
    height: icon.height,
    offsetX: icon.offsetX,
    offsetY: icon.offsetY,
  };
}

/**
 * Replace `<DragIcon/>` with the window slot the icon is drawn in, or with
 * its fallback when there is no icon. Only the first one gets the icon.
 */
function resolveDragIcons(
  node: CompositionChild,
  hasIcon: boolean,
  placed: { value: boolean },
): CompositionChild[] {
  if (typeof node === "string" || typeof node === "number") {
    return [node];
  }
  if (node.type === "DragIcon") {
    if (hasIcon && !placed.value) {
      placed.value = true;
      return [createElementNode("Window", {}, node.key)];
    }
    return node.children.flatMap((child) => resolveDragIcons(child, hasIcon, placed));
  }
  return [
    {
      ...node,
      children: node.children.flatMap((child) => resolveDragIcons(child, hasIcon, placed)),
    },
  ];
}

function createView(state: SessionState, dragId: number, options: DragViewOptions): DragView {
  const viewId = nextViewId++;
  const maxLingerMs =
    options.maxLinger !== undefined && Number.isFinite(options.maxLinger)
      ? Math.max(0, options.maxLinger)
      : DEFAULT_MAX_LINGER_MS;
  native({ op: "show", viewId, dragId, maxLingerMs });

  let disposed = false;
  let resolveClosed!: () => void;
  const closed = new Promise<void>((resolve) => {
    resolveClosed = resolve;
  });
  const stops: Array<() => void> = [];

  let lastProps = "";
  stops.push(
    withoutCompositionOwnership(() =>
      watch(() => {
        const props = {
          anchor: read(options.anchor ?? "pointer"),
          x: Number(read(options.x ?? 0)),
          y: Number(read(options.y ?? 0)),
          opacity: Number(read(options.opacity ?? 1)),
          scale: Number(read(options.scale ?? 1)),
        };
        const key = JSON.stringify(props);
        if (disposed || key === lastProps) {
          return;
        }
        lastProps = key;
        native({ op: "props", viewId, props });
      }),
    ),
  );

  const content = options.content;
  if (content !== undefined) {
    const store = createComponentStateStore();
    stops.push(
      withoutCompositionOwnership(() =>
        watch(() => {
          const hasIcon = state.session.icon.value !== null;
          const rendered = withComponentRenderRoot(`dnd-view:${viewId}`, store, () =>
            typeof content === "function" ? content() : content,
          );
          if (disposed) {
            return;
          }
          if (rendered == null || rendered === false || rendered === true) {
            native({ op: "content", viewId, tree: null });
            return;
          }
          const resolved = resolveDragIcons(rendered, hasIcon, { value: false });
          const root =
            resolved.length === 1 && typeof resolved[0] === "object"
              ? resolved[0]
              : createElementNode("Fragment", { children: resolved });
          native({
            op: "content",
            viewId,
            tree: serializeCompositionTree(root, VIEW_SERIALIZATION_CONTEXT),
          });
        }),
      ),
    );
  }

  const viewState: ViewState = {
    view: undefined as unknown as DragView,
    maxLingerMs,
    dispose() {
      if (disposed) {
        return;
      }
      disposed = true;
      for (const stop of stops) {
        stop();
      }
      state.views.delete(viewState);
      native({ op: "dispose", viewId });
      resolveClosed();
    },
  };
  if (state.session.ended.peek()) {
    viewState.deadlineMs = Date.now() + maxLingerMs;
  }
  const view: DragView = {
    dispose: () => viewState.dispose(),
    get disposed() {
      return disposed;
    },
    closed,
  };
  viewState.view = view;
  state.views.add(viewState);
  return view;
}

function createSession(wire: WireDrag): SessionState {
  const [action, setAction] = signal<DragAction>(wire.action);
  const [target, setTarget] = signal<WaylandWindow | null>(
    wire.targetWindowId === null ? null : resolveWindow(wire.targetWindowId),
  );
  const [icon, setIcon] = signal<DragIconInfo | null>(iconInfo(wire.icon));
  const [ended, setEnded] = signal<DragEnd | null>(null);
  const endListeners = new Set<(end: DragEnd) => void>();
  const animation = createAnimationController(() => markRuntimeDirty());

  const state: SessionState = {
    session: undefined as unknown as DragSession,
    views: new Set(),
    setAction,
    setTarget,
    setIcon,
    end(end) {
      if (ended.peek()) {
        return;
      }
      const now = Date.now();
      for (const view of state.views) {
        view.deadlineMs ??= now + view.maxLingerMs;
      }
      setEnded(end);
      for (const listener of Array.from(endListeners)) {
        listener(end);
      }
      endListeners.clear();
    },
  };
  const session: DragSession = {
    id: wire.id,
    mimeTypes: Object.freeze([...wire.mimeTypes]),
    actions: { ...wire.actions },
    action,
    target,
    sourceWindow:
      wire.sourceWindowId === null ? null : resolveWindow(wire.sourceWindowId),
    device: wire.device,
    origin: { ...wire.origin },
    icon,
    ended,
    animation,
    show(options = {}) {
      return createView(state, wire.id, options);
    },
    onEnd(listener) {
      const end = ended.peek();
      if (end) {
        listener(end);
        return () => {};
      }
      endListeners.add(listener);
      return () => endListeners.delete(listener);
    },
  };
  state.session = session;
  return state;
}

/**
 * Forget the sessions whose views are all gone, and the views the compositor
 * already removed after their linger.
 */
function sweep(nowMs: number): void {
  for (const [id, state] of sessions) {
    for (const view of Array.from(state.views)) {
      if (view.deadlineMs !== undefined && nowMs >= view.deadlineMs) {
        view.dispose();
      }
    }
    if (state.session.ended.peek() && state.views.size === 0) {
      sessions.delete(id);
    }
  }
}

/** @internal Route a compositor drag event. Returns whether a listener ran. */
export function dispatchDndEvent(event: DndEvent): boolean {
  sweep(Date.now());
  switch (event.phase) {
    case "start": {
      const state = createSession(event.drag);
      sessions.set(event.drag.id, state);
      setCurrentDrag(state.session);
      for (const listener of Array.from(startListeners)) {
        listener(state.session);
      }
      return startListeners.size > 0;
    }
    case "update": {
      const state = sessions.get(event.dragId);
      if (!state) {
        return false;
      }
      state.setAction(event.action);
      state.setTarget(
        event.targetWindowId === null ? null : resolveWindow(event.targetWindowId),
      );
      const icon = iconInfo(event.icon);
      const previous = state.session.icon.peek();
      if (JSON.stringify(icon) !== JSON.stringify(previous)) {
        state.setIcon(icon);
      }
      return true;
    }
    case "end": {
      const state = sessions.get(event.dragId);
      if (currentDrag.peek()?.id === event.dragId) {
        setCurrentDrag(null);
      }
      if (!state) {
        return false;
      }
      state.end({
        kind: event.kind,
        accepted: event.accepted,
        target:
          event.targetWindowId === null ? null : resolveWindow(event.targetWindowId),
        position: { ...event.position },
      });
      if (state.views.size === 0) {
        sessions.delete(event.dragId);
      }
      return true;
    }
  }
}

/** @internal How the runtime finds a window by id. */
export function installDndWindowResolver(
  resolver: (id: string) => WaylandWindow | null,
): void {
  resolveWindow = resolver;
}

/** @internal Whether any drag listener is registered. */
export function hasDndStartListeners(): boolean {
  return startListeners.size > 0;
}

/** @internal Whether the listeners changed since the last call. */
export function takeDndListenersChanged(): boolean {
  const changed = listenersChanged;
  listenersChanged = false;
  return changed;
}

export const DND_CONTROLLER: DndController = {
  event: {
    onStart(listener) {
      startListeners.add(listener);
      listenersChanged = true;
      return () => {
        if (startListeners.delete(listener)) {
          listenersChanged = true;
        }
      };
    },
  },
  current: currentDrag,
};

/** Props for `<DragIcon/>`. / `<DragIcon/>` の props。 */
export interface DragIconProps {
  /**
   * Drawn instead when the application gave no icon.
   * アプリがアイコンを指定しなかったときに代わりに描くもの。
   */
  children?: CompositionRenderable | CompositionRenderable[];
}

/**
 * Where the application's drag icon goes in a drag view's `content`. Its
 * children are drawn instead when there is no icon.
 * ドラッグビューの `content` 内でアプリのドラッグアイコンを置く場所。
 * アイコンが無い場合は子要素を代わりに描きます。
 *
 * @example
 * ```tsx
 * drag.show({
 *   content: () => (
 *     <Box direction="column" gap={4}>
 *       <DragIcon><Label text="📄" /></DragIcon>
 *       {drag.action.value === "copy" && <Label text="+ Copy" />}
 *     </Box>
 *   ),
 * });
 * ```
 */
export function DragIcon(props: DragIconProps): CompositionChild {
  return createElementNode("DragIcon", props as Record<string, unknown>);
}
