import type { SessionController } from "./types";

let startLocked = false;

export const SESSION_CONTROLLER: SessionController = {
  get startLocked(): boolean {
    return startLocked;
  },
  set startLocked(locked: boolean) {
    startLocked = locked === true;
  },
};

/**
 * Whether the config asked to start locked. Sent with every enable; the
 * compositor only honors it on the first one, before any client runs.
 */
export function sessionStartLocked(): boolean {
  return startLocked;
}
