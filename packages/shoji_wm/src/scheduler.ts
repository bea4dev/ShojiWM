export interface PollHandle {
  cancel(): void;
  readonly cancelled: boolean;
  readonly nowMs: number;
}

export type PollCallback = (handle: PollHandle) => void;
export type PollDirtyMode = "runtime" | "none";

interface SchedulerBridge {
  registerPoll(
    intervalMs: number,
    callback: PollCallback,
    dirtyMode: PollDirtyMode,
  ): PollHandle;
}

let activeBridge: SchedulerBridge | null = null;

export function installSchedulerBridge(bridge: SchedulerBridge | null): void {
  activeBridge = bridge;
}

export function createPoll(intervalMs: number, callback: PollCallback): PollHandle {
  return createManagedPoll(intervalMs, callback, "runtime");
}

export function createManagedPoll(
  intervalMs: number,
  callback: PollCallback,
  dirtyMode: PollDirtyMode,
): PollHandle {
  if (!Number.isFinite(intervalMs) || intervalMs <= 0) {
    throw new Error("createPoll interval must be a positive finite number");
  }

  if (activeBridge) {
    return activeBridge.registerPoll(intervalMs, callback, dirtyMode);
  }

  return createDetachedPollHandle();
}

function createDetachedPollHandle(): PollHandle {
  let cancelled = false;
  let nowMs = 0;

  return {
    cancel() {
      cancelled = true;
    },
    get cancelled() {
      return cancelled;
    },
    get nowMs() {
      return nowMs;
    },
  };
}
