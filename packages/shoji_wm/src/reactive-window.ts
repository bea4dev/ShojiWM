import { signal, type Signal } from "./signals";
import { createWindowAnimationController } from "./animation";
import type {
  DecorationInteractionSnapshot,
  MaybeSignal,
  ReactiveWaylandWindow,
  ReactiveWaylandWindowHandle,
  TransformOrigin,
  WaylandWindowActions,
  WaylandWindowSnapshot,
  WindowIcon,
} from "./types";

interface MutableWindowSignals {
  id: Signal<string>;
  title: Signal<string>;
  appId: Signal<string | undefined>;
  positionX: Signal<number>;
  positionY: Signal<number>;
  positionWidth: Signal<number>;
  positionHeight: Signal<number>;
  isFocused: Signal<boolean>;
  isFloating: Signal<boolean>;
  isMaximized: Signal<boolean>;
  isFullscreen: Signal<boolean>;
  isXwayland: Signal<boolean>;
  icon: Signal<WindowIcon | undefined>;
  interaction: Signal<DecorationInteractionSnapshot>;
  transformOriginX: Signal<number>;
  transformOriginY: Signal<number>;
  transformTranslateX: Signal<number>;
  transformTranslateY: Signal<number>;
  transformScaleX: Signal<number>;
  transformScaleY: Signal<number>;
  transformOpacity: Signal<number>;
}

export function createReactiveWindow(
  snapshot: WaylandWindowSnapshot,
  actions: WaylandWindowActions,
): ReactiveWaylandWindowHandle {
  const signals: MutableWindowSignals = {
    id: signal(snapshot.id),
    title: signal(snapshot.title),
    appId: signal(snapshot.appId),
    positionX: signal(snapshot.position.x),
    positionY: signal(snapshot.position.y),
    positionWidth: signal(snapshot.position.width),
    positionHeight: signal(snapshot.position.height),
    isFocused: signal(snapshot.isFocused),
    isFloating: signal(snapshot.isFloating),
    isMaximized: signal(snapshot.isMaximized),
    isFullscreen: signal(snapshot.isFullscreen),
    isXwayland: signal(snapshot.isXwayland),
    icon: signal(snapshot.icon),
    interaction: signal(snapshot.interaction),
    transformOriginX: signal(0.5),
    transformOriginY: signal(0.5),
    transformTranslateX: signal(0),
    transformTranslateY: signal(0),
    transformScaleX: signal(1),
    transformScaleY: signal(1),
    transformOpacity: signal(1),
  };

  let transformOrigin: MaybeSignal<TransformOrigin> = { x: 0.5, y: 0.5 };
  let transformTranslateX: MaybeSignal<number> = 0;
  let transformTranslateY: MaybeSignal<number> = 0;
  let transformScaleX: MaybeSignal<number> = 1;
  let transformScaleY: MaybeSignal<number> = 1;
  let transformOpacity: MaybeSignal<number> = 1;

  const position = {
    get x() {
      return signals.positionX.value;
    },
    get y() {
      return signals.positionY.value;
    },
    get width() {
      return signals.positionWidth.value;
    },
    get height() {
      return signals.positionHeight.value;
    },
  };

  const transform = {
    get origin() {
      return transformOrigin;
    },
    set origin(value) {
      transformOrigin = value;
    },
    get translateX() {
      return transformTranslateX;
    },
    set translateX(value) {
      transformTranslateX = value;
    },
    get translateY() {
      return transformTranslateY;
    },
    set translateY(value) {
      transformTranslateY = value;
    },
    get scaleX() {
      return transformScaleX;
    },
    set scaleX(value) {
      transformScaleX = value;
    },
    get scaleY() {
      return transformScaleY;
    },
    set scaleY(value) {
      transformScaleY = value;
    },
    get opacity() {
      return transformOpacity;
    },
    set opacity(value) {
      transformOpacity = value;
    },
  };

  const window: ReactiveWaylandWindow = {
    get id() {
      return signals.id.value;
    },
    title: signals.title,
    appId: signals.appId,
    get position() {
      return position;
    },
    isFocused: signals.isFocused,
    isFloating: signals.isFloating,
    isMaximized: signals.isMaximized,
    isFullscreen: signals.isFullscreen,
    icon: signals.icon,
    interaction: signals.interaction,
    get transform() {
      return transform;
    },
    animation: createWindowAnimationController(snapshot.id),
    signals,
    close: actions.close,
    maximize: actions.maximize,
    minimize: actions.minimize,
    isXWayland() {
      return signals.isXwayland.value;
    },
  };

  return {
    window,
    transform,
    update(nextSnapshot) {
      signals.id.value = nextSnapshot.id;
      signals.title.value = nextSnapshot.title;
      signals.appId.value = nextSnapshot.appId;
      signals.positionX.value = nextSnapshot.position.x;
      signals.positionY.value = nextSnapshot.position.y;
      signals.positionWidth.value = nextSnapshot.position.width;
      signals.positionHeight.value = nextSnapshot.position.height;
      signals.isFocused.value = nextSnapshot.isFocused;
      signals.isFloating.value = nextSnapshot.isFloating;
      signals.isMaximized.value = nextSnapshot.isMaximized;
      signals.isFullscreen.value = nextSnapshot.isFullscreen;
      signals.isXwayland.value = nextSnapshot.isXwayland;
      signals.icon.value = nextSnapshot.icon;
      signals.interaction.value = nextSnapshot.interaction;
    },
  };
}
