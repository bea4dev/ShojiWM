import { peekOutputState } from "./output";
import type {
  LayerController,
  LayerInsets,
  UsableAreaOptions,
  WaylandLayerAnchor,
  WaylandLayerEdge,
  WaylandLayerSnapshot,
  WindowPosition,
} from "./types";

let currentLayerSnapshots: Record<string, WaylandLayerSnapshot> = {};

function cloneSnapshot(snapshot: WaylandLayerSnapshot): WaylandLayerSnapshot {
  return {
    id: snapshot.id,
    namespace: snapshot.namespace,
    layer: snapshot.layer,
    outputName: snapshot.outputName,
    position: { ...snapshot.position },
    anchor: { ...snapshot.anchor },
    exclusiveZone:
      snapshot.exclusiveZone.mode === "exclusive"
        ? { mode: "exclusive", size: snapshot.exclusiveZone.size }
        : { mode: snapshot.exclusiveZone.mode },
    exclusiveEdge: snapshot.exclusiveEdge,
    margin: { ...snapshot.margin },
    keyboardInteractivity: snapshot.keyboardInteractivity,
    desiredSize: { ...snapshot.desiredSize },
    pid: snapshot.pid,
  };
}

function cloneAll(
  state: Record<string, WaylandLayerSnapshot>,
): Record<string, WaylandLayerSnapshot> {
  return Object.fromEntries(
    Object.entries(state).map(([id, snapshot]) => [id, cloneSnapshot(snapshot)]),
  );
}

/**
 * Replace the current layer-snapshot table. Called by the runtime each time
 * the compositor hands over a fresh set of layer snapshots.
 */
export function updateLayerSnapshots(snapshots: WaylandLayerSnapshot[]): void {
  const next: Record<string, WaylandLayerSnapshot> = {};
  for (const snapshot of snapshots) {
    next[snapshot.id] = cloneSnapshot(snapshot);
  }
  currentLayerSnapshots = next;
}

/**
 * Derive which edge an `exclusive` layer reserves space on from its anchor
 * bits, per the wlr-layer-shell spec. Returns `null` for ambiguous
 * configurations (corners, parallel-only, all-four, none), which the spec
 * says must be treated as `Neutral` — i.e., no reservation.
 */
function deriveEdgeFromAnchor(
  anchor: WaylandLayerAnchor,
): WaylandLayerEdge | null {
  const { top, bottom, left, right } = anchor;
  const count =
    (top ? 1 : 0) + (bottom ? 1 : 0) + (left ? 1 : 0) + (right ? 1 : 0);
  if (count === 1) {
    if (top) return "top";
    if (bottom) return "bottom";
    if (left) return "left";
    return "right";
  }
  if (count === 3) {
    // Three edges anchored → the surface stretches along the axis whose
    // two parallel edges are both pinned, and reserves space on the single
    // perpendicular edge that IS anchored. The unanchored edge tells us
    // which side is the "open" one, so the reserved edge is the opposite.
    if (!bottom) return "top";
    if (!top) return "bottom";
    if (!right) return "left";
    return "right";
  }
  return null;
}

function resolveExclusiveEdge(
  layer: WaylandLayerSnapshot,
): WaylandLayerEdge | null {
  return layer.exclusiveEdge ?? deriveEdgeFromAnchor(layer.anchor);
}

function computeReservedInsets(
  outputName: string,
  options?: UsableAreaOptions,
): LayerInsets {
  const insets: LayerInsets = { top: 0, right: 0, bottom: 0, left: 0 };
  for (const snapshot of Object.values(currentLayerSnapshots)) {
    if (snapshot.outputName !== outputName) continue;
    if (snapshot.exclusiveZone.mode !== "exclusive") continue;
    if (options?.filter && !options.filter(snapshot)) continue;
    const edge = resolveExclusiveEdge(snapshot);
    if (edge == null) continue;
    insets[edge] += snapshot.exclusiveZone.size;
  }
  return insets;
}

function computeUsableArea(
  outputName: string,
  options?: UsableAreaOptions,
): WindowPosition | null {
  const output = peekOutputState(outputName);
  if (!output || !output.resolution) {
    return null;
  }
  const widthLogical = output.resolution.width / output.scale;
  const heightLogical = output.resolution.height / output.scale;
  const insets = computeReservedInsets(outputName, options);
  return {
    x: output.position.x + insets.left,
    y: output.position.y + insets.top,
    width: Math.max(0, widthLogical - insets.left - insets.right),
    height: Math.max(0, heightLogical - insets.top - insets.bottom),
  };
}

export const LAYER_CONTROLLER: LayerController = {
  get list() {
    return Object.keys(currentLayerSnapshots);
  },
  get current() {
    return cloneAll(currentLayerSnapshots);
  },
  forOutput(outputName) {
    const result: WaylandLayerSnapshot[] = [];
    for (const snapshot of Object.values(currentLayerSnapshots)) {
      if (snapshot.outputName === outputName) {
        result.push(cloneSnapshot(snapshot));
      }
    }
    return result;
  },
  usableArea(outputName, options) {
    return computeUsableArea(outputName, options);
  },
  reservedInsets(outputName, options) {
    return computeReservedInsets(outputName, options);
  },
};
