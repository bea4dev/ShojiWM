import { markLayerDirty, withSnapshotSync } from "./runtime-hooks";
import { signal, type Signal } from "./signals";
import {
  createAnimationController,
  type AnimationController,
} from "./animation";
import { shallowEqual } from "./reconcile";
import type {
  LayerPosition,
  ReactiveWaylandLayer,
  ReactiveWaylandLayerHandle,
  WaylandLayerAnchor,
  WaylandLayerDesiredSize,
  WaylandLayerEdge,
  WaylandLayerExclusiveZone,
  WaylandLayerKeyboardInteractivity,
  WaylandLayerKind,
  WaylandLayerMargin,
  WaylandLayerSnapshot,
} from "./types";

interface MutableLayerSignals {
  id: Signal<string>;
  namespace: Signal<string | undefined>;
  layer: Signal<WaylandLayerKind>;
  outputName: Signal<string>;
  positionX: Signal<number>;
  positionY: Signal<number>;
  positionWidth: Signal<number>;
  positionHeight: Signal<number>;
  anchor: Signal<WaylandLayerAnchor>;
  exclusiveZone: Signal<WaylandLayerExclusiveZone>;
  exclusiveEdge: Signal<WaylandLayerEdge | null>;
  margin: Signal<WaylandLayerMargin>;
  keyboardInteractivity: Signal<WaylandLayerKeyboardInteractivity>;
  desiredSize: Signal<WaylandLayerDesiredSize>;
}

export function createReactiveLayer(
  snapshot: WaylandLayerSnapshot,
  animation: AnimationController = createAnimationController(() =>
    markLayerDirty(snapshot.id),
  ),
): ReactiveWaylandLayerHandle {
  const signals: MutableLayerSignals = {
    id: signal(snapshot.id),
    namespace: signal(snapshot.namespace),
    layer: signal(snapshot.layer),
    outputName: signal(snapshot.outputName),
    positionX: signal(snapshot.position.x),
    positionY: signal(snapshot.position.y),
    positionWidth: signal(snapshot.position.width),
    positionHeight: signal(snapshot.position.height),
    anchor: signal(snapshot.anchor),
    exclusiveZone: signal(snapshot.exclusiveZone),
    exclusiveEdge: signal(snapshot.exclusiveEdge),
    margin: signal(snapshot.margin),
    keyboardInteractivity: signal(snapshot.keyboardInteractivity),
    desiredSize: signal(snapshot.desiredSize),
  };

  const position: LayerPosition = {
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

  const layer: ReactiveWaylandLayer = {
    get id() {
      return signals.id.value;
    },
    namespace: signals.namespace,
    layer: signals.layer,
    outputName: signals.outputName,
    get position() {
      return position;
    },
    anchor: signals.anchor,
    exclusiveZone: signals.exclusiveZone,
    exclusiveEdge: signals.exclusiveEdge,
    margin: signals.margin,
    keyboardInteractivity: signals.keyboardInteractivity,
    desiredSize: signals.desiredSize,
    pid: snapshot.pid,
    animation,
    signals,
  };

  return {
    layer,
    update(nextSnapshot) {
      withSnapshotSync(() => {
        signals.id.value = nextSnapshot.id;
        signals.namespace.value = nextSnapshot.namespace;
        signals.layer.value = nextSnapshot.layer;
        signals.outputName.value = nextSnapshot.outputName;
        signals.positionX.value = nextSnapshot.position.x;
        signals.positionY.value = nextSnapshot.position.y;
        signals.positionWidth.value = nextSnapshot.position.width;
        signals.positionHeight.value = nextSnapshot.position.height;
        // Object fields are deserialized as fresh references on every runtime
        // turn. Avoid notifying dependents when only object identity changed,
        // otherwise layer-effect refresh can mark the whole runtime dirty again
        // and create a self-sustaining redraw loop.
        if (!shallowEqual(signals.anchor.peek(), nextSnapshot.anchor)) {
          signals.anchor.value = nextSnapshot.anchor;
        }
        if (
          !shallowEqual(signals.exclusiveZone.peek(), nextSnapshot.exclusiveZone)
        ) {
          signals.exclusiveZone.value = nextSnapshot.exclusiveZone;
        }
        signals.exclusiveEdge.value = nextSnapshot.exclusiveEdge;
        if (!shallowEqual(signals.margin.peek(), nextSnapshot.margin)) {
          signals.margin.value = nextSnapshot.margin;
        }
        signals.keyboardInteractivity.value = nextSnapshot.keyboardInteractivity;
        if (!shallowEqual(signals.desiredSize.peek(), nextSnapshot.desiredSize)) {
          signals.desiredSize.value = nextSnapshot.desiredSize;
        }
      });
    },
  };
}
