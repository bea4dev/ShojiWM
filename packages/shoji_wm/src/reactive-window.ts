import { signal, type Signal } from "./signals";
import {
  createWindowAnimationController,
  type WindowAnimationController,
} from "./animation";
import { shallowEqual } from "./reconcile";
import { createPoll } from "./scheduler";
import { withSnapshotSync } from "./runtime-hooks";
import { createWindowStateStore } from "./window-state";
import type {
  WindowCompositionInteractionSnapshot,
  MaybeSignal,
  ReactiveWaylandWindow,
  ReactiveWaylandWindowHandle,
  TransformOrigin,
  WaylandWindowActions,
  WaylandWindowSnapshot,
  ManagedWindowState,
  PlasmaSurfaceState,
  WindowIcon,
  WindowDecorationState,
  WindowPosition,
  WindowSizeConstraints,
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
  sizeConstraints: Signal<WindowSizeConstraints>;
  isResizable: Signal<boolean>;
  isTransient: Signal<boolean>;
  parentId: Signal<string | undefined>;
  plasma: Signal<PlasmaSurfaceState | undefined>;
  icon: Signal<WindowIcon | undefined>;
  interaction: Signal<WindowCompositionInteractionSnapshot>;
  decoration: Signal<WindowDecorationState>;
  output: Signal<string | undefined>;
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
  animation: WindowAnimationController = createWindowAnimationController(
    snapshot.id,
  ),
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
    sizeConstraints: signal(snapshot.sizeConstraints),
    isResizable: signal(snapshot.isResizable),
    isTransient: signal(snapshot.isTransient),
    parentId: signal(snapshot.parentId),
    plasma: signal(snapshot.plasma),
    icon: signal(snapshot.icon),
    interaction: signal(snapshot.interaction),
    decoration: signal(snapshot.decoration),
    output: signal(snapshot.outputName),
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
  let snapshotRect: WindowPosition = snapshot.rect ?? snapshot.position;
  let managedRect: WindowPosition | undefined;
  let window: ReactiveWaylandWindow;

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

  const state = createWindowStateStore(() => window);

  window = {
    get id() {
      return signals.id.value;
    },
    title: signals.title,
    appId: signals.appId,
    get position() {
      return position;
    },
    get rect() {
      return managedRect ?? snapshotRect;
    },
    state,
    isFocused: signals.isFocused,
    isFloating: signals.isFloating,
    isMaximized: signals.isMaximized,
    isFullscreen: signals.isFullscreen,
    sizeConstraints: signals.sizeConstraints,
    isResizable: signals.isResizable,
    isTransient: signals.isTransient,
    parentId: signals.parentId,
    plasma: signals.plasma,
    icon: signals.icon,
    interaction: signals.interaction,
    decoration: signals.decoration,
    output: signals.output,
    createPoll(intervalMs, callback, options) {
      return createPoll(intervalMs, callback, {
        output: signals.output,
        dirty: options?.dirty,
      });
    },
    get transform() {
      return transform;
    },
    animation,
    signals,
    close: actions.close,
    maximize: actions.maximize,
    unmaximize: actions.unmaximize,
    minimize: actions.minimize,
    fullscreen: actions.fullscreen,
    unfullscreen: actions.unfullscreen,
    focus: actions.focus,
    scheduleAnimation: actions.scheduleAnimation,
    cancelAnimation: actions.cancelAnimation,
    setCloseAnimationDuration: actions.setCloseAnimationDuration,
    isXWayland() {
      return signals.isXwayland.value;
    },
  };

  return {
    window,
    transform,
    update(nextSnapshot) {
      withSnapshotSync(() => {
        // Primitive fields: WritableSignal.set's built-in Object.is check
        // already suppresses spurious notifies when the value is unchanged.
        signals.id.value = nextSnapshot.id;
        signals.title.value = nextSnapshot.title;
        signals.appId.value = nextSnapshot.appId;
        signals.positionX.value = nextSnapshot.position.x;
        signals.positionY.value = nextSnapshot.position.y;
        signals.positionWidth.value = nextSnapshot.position.width;
        signals.positionHeight.value = nextSnapshot.position.height;
        snapshotRect = nextSnapshot.rect ?? nextSnapshot.position;
        signals.isFocused.value = nextSnapshot.isFocused;
        signals.isFloating.value = nextSnapshot.isFloating;
        signals.isMaximized.value = nextSnapshot.isMaximized;
        signals.isFullscreen.value = nextSnapshot.isFullscreen;
        signals.isXwayland.value = nextSnapshot.isXwayland;
        signals.isResizable.value = nextSnapshot.isResizable;
        signals.isTransient.value = nextSnapshot.isTransient;
        signals.parentId.value = nextSnapshot.parentId;
        signals.output.value = nextSnapshot.outputName;
        // Object fields: every snapshot from Rust deserializes to a *new*
        // object reference with identical content. A naive write would
        // therefore fail Object.is on every turn, fire notify, and re-mark
        // the window dirty — which becomes a self-sustaining
        // evaluate→handle.update→notify→dirty cycle (~250ms scheduler-tick
        // cadence) once anything bootstraps it. Compare structurally before
        // writing so identity-but-not-equality "changes" are squashed.
        if (
          !shallowEqual(
            signals.sizeConstraints.peek(),
            nextSnapshot.sizeConstraints,
          )
        ) {
          signals.sizeConstraints.value = nextSnapshot.sizeConstraints;
        }
        if (!shallowEqual(signals.plasma.peek(), nextSnapshot.plasma)) {
          signals.plasma.value = nextSnapshot.plasma;
        }
        if (!shallowEqual(signals.icon.peek(), nextSnapshot.icon)) {
          signals.icon.value = nextSnapshot.icon;
        }
        if (!shallowEqual(signals.interaction.peek(), nextSnapshot.interaction)) {
          signals.interaction.value = nextSnapshot.interaction;
        }
        if (!shallowEqual(signals.decoration.peek(), nextSnapshot.decoration)) {
          signals.decoration.value = nextSnapshot.decoration;
        }
      });
    },
    updateManagedWindow(state: ManagedWindowState) {
      managedRect = state.rect;
    },
  };
}
