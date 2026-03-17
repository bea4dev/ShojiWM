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
  isFocused: Signal<boolean>;
  isFloating: Signal<boolean>;
  isMaximized: Signal<boolean>;
  isFullscreen: Signal<boolean>;
  isXwayland: Signal<boolean>;
  icon: Signal<WindowIcon | undefined>;
  interaction: Signal<DecorationInteractionSnapshot>;
}

export function createReactiveWindow(
  snapshot: WaylandWindowSnapshot,
  actions: WaylandWindowActions,
): ReactiveWaylandWindowHandle {
  const signals: MutableWindowSignals = {
    id: signal(snapshot.id),
    title: signal(snapshot.title),
    appId: signal(snapshot.appId),
    isFocused: signal(snapshot.isFocused),
    isFloating: signal(snapshot.isFloating),
    isMaximized: signal(snapshot.isMaximized),
    isFullscreen: signal(snapshot.isFullscreen),
    isXwayland: signal(snapshot.isXwayland),
    icon: signal(snapshot.icon),
    interaction: signal(snapshot.interaction),
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
    signals,
    close: actions.close,
    maximize: actions.maximize,
    minimize: actions.minimize,
    isXWayland: actions.isXWayland,
  };

  return {
    window,
    update(nextSnapshot) {
      signals.id.value = nextSnapshot.id;
      signals.title.value = nextSnapshot.title;
      signals.appId.value = nextSnapshot.appId;
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
