import { signal, type Signal } from "./signals";
import type {
  DecorationInteractionSnapshot,
  ReactiveWaylandWindow,
  ReactiveWaylandWindowHandle,
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
      return {
        x: signals.transformOriginX.value,
        y: signals.transformOriginY.value,
      };
    },
    set origin(value) {
      signals.transformOriginX.value = value.x;
      signals.transformOriginY.value = value.y;
    },
    get translateX() {
      return signals.transformTranslateX.value;
    },
    set translateX(value) {
      signals.transformTranslateX.value = value;
    },
    get translateY() {
      return signals.transformTranslateY.value;
    },
    set translateY(value) {
      signals.transformTranslateY.value = value;
    },
    get scaleX() {
      return signals.transformScaleX.value;
    },
    set scaleX(value) {
      signals.transformScaleX.value = value;
    },
    get scaleY() {
      return signals.transformScaleY.value;
    },
    set scaleY(value) {
      signals.transformScaleY.value = value;
    },
    get opacity() {
      return signals.transformOpacity.value;
    },
    set opacity(value) {
      signals.transformOpacity.value = value;
    },
  };

  const window: ReactiveWaylandWindow = {
    get id() {
      return signals.id.value;
    },
    get title() {
      return signals.title.value;
    },
    get appId() {
      return signals.appId.value;
    },
    get position() {
      return position;
    },
    get isFocused() {
      return signals.isFocused.value;
    },
    get isFloating() {
      return signals.isFloating.value;
    },
    get isMaximized() {
      return signals.isMaximized.value;
    },
    get isFullscreen() {
      return signals.isFullscreen.value;
    },
    get isXwayland() {
      return signals.isXwayland.value;
    },
    get icon() {
      return signals.icon.value;
    },
    get interaction() {
      return signals.interaction.value;
    },
    get transform() {
      return transform;
    },
    signals,
    close: actions.close,
    maximize: actions.maximize,
    minimize: actions.minimize,
    isXWayland: actions.isXWayland,
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
