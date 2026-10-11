import type { PointerRegionEvent, PointerRegionListener } from "./events";
import type {
  HotCorner,
  PointerController,
  PointerRegionOptions,
} from "./types";

interface RuntimePointerRegion extends PointerRegionOptions {
  id: string;
}

interface RuntimePointerConfig {
  windowMoveModifier?: string;
  windowResizeModifier?: string;
  regions?: RuntimePointerRegion[];
}

type RegionEventSource = (listener: PointerRegionListener) => () => void;

let regionEventSource: RegionEventSource | null = null;
const hotCornerUnsubscribes = new Map<string, () => void>();

/** @internal Connects `bindHotCorner` to `COMPOSITOR.event.onPointerRegion`. */
export function installPointerRegionEventSource(source: RegionEventSource): void {
  regionEventSource = source;
}

let desiredPointerConfig: RuntimePointerConfig = {};
let stagedPointerConfig: RuntimePointerConfig | null = null;
let pendingPointerConfig = false;

function registrationTarget(): RuntimePointerConfig {
  return stagedPointerConfig ?? desiredPointerConfig;
}

function normalizeModifier(modifier: string): string {
  const parts = modifier
    .split("+")
    .map((part) => part.trim())
    .filter((part) => part.length > 0);

  if (parts.length === 0) {
    throw new Error("window move modifier must not be empty");
  }

  return parts.join("+");
}

export const POINTER_CONTROLLER: PointerController = {
  bindWindowMoveModifier(modifier) {
    registrationTarget().windowMoveModifier = normalizeModifier(modifier);

    if (!stagedPointerConfig) {
      pendingPointerConfig = true;
    }
  },

  bindWindowResizeModifier(modifier) {
    registrationTarget().windowResizeModifier = normalizeModifier(modifier);

    if (!stagedPointerConfig) {
      pendingPointerConfig = true;
    }
  },

  defineRegion(id, options) {
    const region = normalizeRegion(id, options);
    const target = registrationTarget();
    target.regions = [
      ...(target.regions ?? []).filter((existing) => existing.id !== id),
      region,
    ];

    if (!stagedPointerConfig) {
      pendingPointerConfig = true;
    }
  },

  removeRegion(id) {
    const target = registrationTarget();
    target.regions = (target.regions ?? []).filter((existing) => existing.id !== id);

    if (!stagedPointerConfig) {
      pendingPointerConfig = true;
    }
  },

  bindHotCorner(id, corner, handler, options = {}) {
    if (!regionEventSource) {
      throw new Error("bindHotCorner is unavailable before COMPOSITOR is created");
    }
    const regionId = `hot-corner:${id}`;
    const size = options.size ?? 1;
    const trigger = options.pressure === undefined ? "enter" : "pressure";
    const delay = options.delay ?? 0;

    hotCornerUnsubscribes.get(id)?.();
    POINTER_CONTROLLER.defineRegion(regionId, {
      output: options.output,
      anchor: hotCornerAnchor(corner),
      size: { width: size, height: size },
      outerEdgesOnly: true,
      leaveMargin: options.leaveMargin ?? 8,
      pressure: options.pressure,
      ignoreWhenFullscreen: options.ignoreWhenFullscreen ?? true,
      ignoreWhileButtonHeld: options.ignoreWhileButtonHeld ?? true,
    });

    let timer: ReturnType<typeof setTimeout> | undefined;
    const cancel = () => {
      if (timer !== undefined) {
        clearTimeout(timer);
        timer = undefined;
      }
    };
    const unsubscribe = regionEventSource((event: PointerRegionEvent) => {
      if (event.id !== regionId) {
        return;
      }
      if (event.phase === "leave") {
        cancel();
        return;
      }
      if (event.phase !== trigger) {
        return;
      }
      if (trigger === "pressure" || delay <= 0) {
        handler(event);
        return;
      }
      cancel();
      timer = setTimeout(() => {
        timer = undefined;
        handler(event);
      }, delay);
    });

    const remove = () => {
      if (hotCornerUnsubscribes.get(id) !== remove) {
        return;
      }
      hotCornerUnsubscribes.delete(id);
      cancel();
      unsubscribe();
      POINTER_CONTROLLER.removeRegion(regionId);
    };
    hotCornerUnsubscribes.set(id, remove);
    return remove;
  },
};

function hotCornerAnchor(corner: HotCorner): RuntimePointerRegion["anchor"] {
  switch (corner) {
    case "top-left":
    case "top-right":
    case "bottom-left":
    case "bottom-right":
      return corner;
    default:
      throw new Error(`unknown hot corner: ${String(corner)}`);
  }
}

function normalizeRegion(id: string, options: PointerRegionOptions): RuntimePointerRegion {
  if (id.length === 0) {
    throw new Error("pointer region id must not be empty");
  }
  const { width, height } = options.size;
  if (!(width > 0) || !(height > 0)) {
    throw new Error(`pointer region ${id}: size must be positive`);
  }
  return {
    id,
    output: options.output,
    anchor: options.anchor ?? "top-left",
    offset: { x: options.offset?.x ?? 0, y: options.offset?.y ?? 0 },
    size: { width, height },
    outerEdgesOnly: options.outerEdgesOnly ?? false,
    leaveMargin: options.leaveMargin ?? 0,
    pressure: options.pressure,
    ignoreWhenFullscreen: options.ignoreWhenFullscreen ?? false,
    ignoreWhileButtonHeld: options.ignoreWhileButtonHeld ?? false,
  };
}

export function beginPointerConfigRegistration(): void {
  stagedPointerConfig = {};
}

export function commitPointerConfigRegistration(): void {
  if (!stagedPointerConfig) {
    return;
  }

  desiredPointerConfig = stagedPointerConfig;
  stagedPointerConfig = null;
  pendingPointerConfig = true;
}

export function takePendingPointerConfig(): RuntimePointerConfig | undefined {
  if (!pendingPointerConfig) {
    return undefined;
  }

  pendingPointerConfig = false;
  return {
    ...desiredPointerConfig,
    regions: [...(desiredPointerConfig.regions ?? [])],
  };
}
