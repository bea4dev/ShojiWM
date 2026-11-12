import type {
  PowerAction,
  PowerController,
  PowerState,
  PowerStateListener,
  ResumeEvent,
  ResumeListener,
} from "./types";

const POWER_ACTIONS: readonly PowerAction[] = [
  "suspend",
  "hibernate",
  "hybrid-sleep",
  "suspend-then-hibernate",
  "sleep",
  "lock",
  "poweroff",
  "reboot",
  "halt",
];

type NativePower = (action: string, lid: boolean) => void;

function nativePower(): NativePower | undefined {
  return (globalThis as { __SHOJI_POWER__?: NativePower }).__SHOJI_POWER__;
}

let currentState: PowerState | undefined;
const stateListeners = new Set<PowerStateListener>();
const resumeListeners = new Set<ResumeListener>();

/** @internal The compositor reported a new power state. */
export function emitPowerState(state: PowerState): void {
  currentState = state;
  for (const listener of stateListeners) {
    listener(state);
  }
}

/** @internal The system came back from a suspend or hibernation. */
export function emitResume(event: ResumeEvent): void {
  for (const listener of resumeListeners) {
    listener(event);
  }
}

function perform(action: PowerAction): void {
  if (!POWER_ACTIONS.includes(action)) {
    throw new TypeError(`unknown power action ${String(action)}`);
  }
  nativePower()?.(action, false);
}

export const POWER_CONTROLLER: PowerController = {
  takeOver(options) {
    nativePower()?.("take-over", options.lid === true);
  },

  suspend() {
    perform("suspend");
  },

  hibernate() {
    perform("hibernate");
  },

  perform,

  get state() {
    return currentState;
  },

  onStateChange(listener) {
    stateListeners.add(listener);
    return () => stateListeners.delete(listener);
  },

  onResume(listener) {
    resumeListeners.add(listener);
    return () => resumeListeners.delete(listener);
  },
};
