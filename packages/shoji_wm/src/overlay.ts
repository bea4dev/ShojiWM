import { effect as watch, isSignal, read } from "./signals";
import { compileEffect, type CompileEffectOptions } from "./shader";
import type { CompiledEffectHandle, OutputInfo } from "./types";

export interface SnapshotSourceHandle {
  kind: "snapshot-source";
}

export interface OverlayEffectHandle {
  kind: "overlay-effect";
  effect: CompiledEffectHandle;
}

export interface OverlayOptions {
  effect: OverlayEffectHandle;
  /** `below-layers` is above windows but below Top/Overlay layer-shell surfaces. */
  /** `unlock` captures a lock scene: active lock, snapshot, <=3000 ms, nonpersistent. */
  placement?: "top" | "below-layers" | "unlock";
  /** Remain active after the first frame until disposed or lifecycle cleanup. */
  persistent?: boolean;
  /** Deadline in milliseconds; persistent overlays only bound the first frame. */
  maxDuration?: number;
}

export interface OverlayHandle {
  dispose(): void;
  /** Resolves on disposal, replacement, timeout or compositor lifecycle cleanup. */
  closed: Promise<void>;
}

/** A frozen, cursor-free capture made when the overlay's first frame is rendered. */
export function snapshotSource(): SnapshotSourceHandle {
  return { kind: "snapshot-source" };
}

export function compileOverlayEffect(options: CompileEffectOptions): OverlayEffectHandle {
  return { kind: "overlay-effect", effect: compileEffect(options) };
}

export interface OverlayBridge {
  createOverlay(output: string, placement: string, maxDuration: number, effect: unknown, persistent: boolean): number;
  waitOverlay(id: number): Promise<void>;
  waitOverlayClosed(id: number): Promise<void>;
  updateOverlay(id: number, effect: unknown): void;
  disposeOverlay(id: number): void;
}

let bridge: OverlayBridge | undefined;
let activeRequest: () => number | null = () => null;

/**
 * `currentRequest` returns the id of the compositor request being handled, or
 * `null` between requests.
 */
export function installOverlayBridge(
  value: OverlayBridge,
  currentRequest: () => number | null = () => null,
): void {
  bridge = value;
  activeRequest = currentRequest;
}

function resolve(value: unknown): unknown {
  if (isSignal(value)) return resolve(read(value));
  if (Array.isArray(value)) return value.map(resolve);
  if (value && typeof value === "object") {
    return Object.fromEntries(Object.entries(value).map(([key, entry]) => [key, resolve(entry)]));
  }
  return value;
}

/** Await from an event's detached task, never during module initialization. */
export async function overlay(
  output: string | Pick<OutputInfo, "name">,
  options: OverlayOptions,
): Promise<OverlayHandle> {
  const native = bridge;
  if (!native) throw new Error("Start output overlays from an event after embedded config initialization");
  if (options.effect.kind !== "overlay-effect") throw new TypeError("Expected compileOverlayEffect()");
  if (options.persistent !== undefined && typeof options.persistent !== "boolean") {
    throw new TypeError("persistent must be a boolean");
  }
  const duration = options.maxDuration ?? 10_000;
  if (!Number.isFinite(duration) || duration <= 0 || duration > 2_147_483_647) {
    throw new RangeError("maxDuration must be a positive finite number of milliseconds <= 2147483647");
  }
  // Let a detached event task return its response before requesting a frame. If the
  // handler instead awaits this Promise, its request is still open: fail rather than
  // deadlock the render thread. Another request being handled by then is unrelated.
  const caller = activeRequest();
  await new Promise<void>((resolve) => setTimeout(resolve, 0));
  if (caller !== null && activeRequest() === caller) {
    throw new Error("Cannot await an output overlay from a compositor callback; launch a detached task");
  }
  const id = native.createOverlay(
    typeof output === "string" ? output : output.name,
    options.placement ?? "top", duration, resolve(options.effect.effect), options.persistent ?? false,
  );
  let stopped = false;
  let stop = () => {};
  const dispose = () => {
    if (stopped) return;
    stopped = true;
    stop();
    native.disposeOverlay(id);
  };
  try {
    stop = watch(() => {
      try {
        native.updateOverlay(id, resolve(options.effect.effect));
      } catch (error) {
        dispose();
        console.error("Output overlay update failed", error);
      }
    });
    if (stopped) stop();
    const closed = native.waitOverlayClosed(id).finally(() => {
      stopped = true;
      stop();
    });
    await native.waitOverlay(id);
    return { dispose, closed };
  } catch (error) {
    dispose();
    throw error;
  }
}
