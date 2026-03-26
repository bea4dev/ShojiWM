import { markLayerDirty } from "./runtime-hooks";
import { signal, type Signal } from "./signals";
import { createAnimationController, type AnimationController } from "./animation";
import type {
  CompiledEffectHandle,
  LayerPosition,
  ReactiveWaylandLayer,
  ReactiveWaylandLayerHandle,
  WaylandLayerKind,
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
}

export function createReactiveLayer(
  snapshot: WaylandLayerSnapshot,
  animation: AnimationController = createAnimationController(snapshot.id),
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
  };

  let effect: CompiledEffectHandle | null = null;

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
    animation,
    get effect() {
      return effect;
    },
    set effect(value) {
      effect = value;
      markLayerDirty(signals.id.peek());
    },
    signals,
  };

  return {
    layer,
    update(nextSnapshot) {
      signals.id.value = nextSnapshot.id;
      signals.namespace.value = nextSnapshot.namespace;
      signals.layer.value = nextSnapshot.layer;
      signals.outputName.value = nextSnapshot.outputName;
      signals.positionX.value = nextSnapshot.position.x;
      signals.positionY.value = nextSnapshot.position.y;
      signals.positionWidth.value = nextSnapshot.position.width;
      signals.positionHeight.value = nextSnapshot.position.height;
    },
  };
}
