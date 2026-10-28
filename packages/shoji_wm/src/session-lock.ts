export interface LockBackgroundSelection {
  /** Exact runtime window IDs, in the compositor's normal stacking order. */
  windowIds?: string[];
  /** Exact layer-shell runtime IDs; namespaces are selected by the config. */
  layerIds?: string[];
}

interface SessionLockBridge {
  configureLockBackground(background: LockBackgroundSelection): boolean;
}

let bridge: SessionLockBridge | undefined;

export function installSessionLockBridge(value: SessionLockBridge): void { bridge = value; }

/**
 * Select root surface trees behind a transparent lock surface; empty by default.
 * Returns the actual protocol lock state. This never changes authentication or
 * input routing. Selections end with their runtime and do not include popups/SSDs.
 */
export function configureLockBackground(background: LockBackgroundSelection): boolean {
  if (!bridge) throw new Error("Configure lock backgrounds after embedded runtime initialization");
  return bridge.configureLockBackground(background);
}
