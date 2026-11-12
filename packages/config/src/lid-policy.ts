// What closing the laptop lid does, set per power source the way Windows and
// KDE Plasma set it: one action on battery, one on external power.
//
// The policy takes the lid from systemd-logind (`COMPOSITOR.power.takeOver`),
// so logind no longer suspends on its own, and adds what logind cannot do:
//
// - While the lid is closed and another display is on, the built-in panel is
//   switched off (DPMS) and work goes on there.
// - Some lid switches stick at "closed". An action runs only when the lid
//   moves: not for a lid that is closed when the switch comes up or the config
//   starts (`initial`), not for one a resume finds closed, and not within
//   `holdoffMs` of the start or of a resume (logind's `HoldoffTimeoutSec`).
import {
  COMPOSITOR,
  type LidSwitchListener,
  type OutputChangeListener,
  type OutputInfo,
  type OutputPower,
  type OutputPowerOptions,
  type PowerAction,
  type PowerState,
  type ResumeEvent,
  type ResumeListener,
} from "shoji_wm";

/**
 * What closing the lid does: a logind action (`"suspend"`, `"hibernate"`,
 * ...), `"ignore"`, `"logind"` for what `logind.conf` says in the situation
 * (including its `HandleLidSwitchDocked` when another display is on), or a
 * function that decides.
 */
export type LidCloseAction =
  | PowerAction
  | "ignore"
  | "logind"
  | ((context: LidCloseContext) => void);

export interface LidCloseContext {
  /** On external power; `undefined` when not known. */
  externalPower: boolean | undefined;
  /** Another display (not the built-in panel) is on. */
  externalDisplay: boolean;
}

export interface LidSleepContext extends LidCloseContext {
  /** The sleep the policy is about to ask logind for. */
  action: PowerAction;
}

export interface LidPolicyOptions {
  /** On battery. Default `"logind"` (`HandleLidSwitch`). */
  onBattery?: LidCloseAction;
  /** On external power. Default `"logind"` (`HandleLidSwitchExternalPower`). */
  onExternalPower?: LidCloseAction;
  /**
   * Called before the policy puts the machine to sleep (suspend, hibernate,
   * ...). A returned promise is waited for, at most `sleepCallbackTimeoutMs`,
   * so a screen locker can come up first.
   */
  onSleep?: (context: LidSleepContext) => void | Promise<void>;
  /** Called after every resume, whoever started the sleep. */
  onResume?: (event: ResumeEvent) => void;
  /**
   * Switch the built-in panel off while the lid is closed and another display
   * is on. Default `true`.
   */
  internalPanelOff?: boolean;
  /** Closing the lid does nothing this long after the start and a resume. Default 30 s. */
  holdoffMs?: number;
  /** Longest wait for `onSleep`'s promise. Default 5 s, logind's `InhibitDelayMaxSec`. */
  sleepCallbackTimeoutMs?: number;
}

/** What the policy needs from COMPOSITOR; tests pass their own. */
export interface LidPolicyEnvironment {
  onLidSwitch(listener: LidSwitchListener): () => void;
  onOutputChange(listener: OutputChangeListener): () => void;
  outputs(): OutputInfo[];
  setPower(power: OutputPower, options: OutputPowerOptions): void;
  powerState(): PowerState | undefined;
  onResume(listener: ResumeListener): () => void;
  takeOver(lid: boolean): void;
  perform(action: PowerAction): void;
}

function compositorEnvironment(): LidPolicyEnvironment {
  return {
    onLidSwitch: (listener) => COMPOSITOR.event.onLidSwitch(listener),
    onOutputChange: (listener) => COMPOSITOR.event.onOutputChange(listener),
    outputs: () => COMPOSITOR.output.outputs,
    setPower: (power, options) => COMPOSITOR.output.setPower(power, options),
    powerState: () => COMPOSITOR.power.state,
    onResume: (listener) => COMPOSITOR.power.onResume(listener),
    takeOver: (lid) => COMPOSITOR.power.takeOver({ lid }),
    perform: (action) => COMPOSITOR.power.perform(action),
  };
}

const POWER_ACTIONS: readonly string[] = [
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

const SLEEP_ACTIONS: readonly string[] = [
  "suspend",
  "hibernate",
  "hybrid-sleep",
  "suspend-then-hibernate",
  "sleep",
];

/** Built-in panels, by connector name. */
function isInternal(output: OutputInfo): boolean {
  return /^(eDP|LVDS|DSI)/.test(output.name);
}

function isHeadless(output: OutputInfo): boolean {
  return output.make === "ShojiWM" && output.model === "Headless";
}

/**
 * Take the lid from logind and do what `options` say when it closes. Returns
 * a function that gives the lid back.
 */
export function bindLidPolicy(
  options: LidPolicyOptions = {},
  env: LidPolicyEnvironment = compositorEnvironment(),
): () => void {
  const holdoffMs = options.holdoffMs ?? 30_000;
  const sleepCallbackTimeoutMs = options.sleepCallbackTimeoutMs ?? 5_000;
  const internalPanelOff = options.internalPanelOff ?? true;

  let lidClosed = false;
  // Whether the lid closed while we watched, as opposed to being closed when
  // we started or after a resume: a broken switch can report closed for a
  // lid that is open.
  let closedByMovement = false;
  let holdoffUntil = Date.now() + holdoffMs;
  let externalDisplay = false;
  let sleeping = false;
  // Built-in panels this policy switched off, to switch back on.
  const panelsOff = new Set<string>();

  const hasExternalDisplay = () =>
    env
      .outputs()
      .some((output) => output.enabled && !isInternal(output) && !isHeadless(output));

  // The panel follows the lid even when its state is only initial: a laptop
  // booted shut on a dock keeps its panel dark.
  const syncPanels = () => {
    const off = internalPanelOff && lidClosed && externalDisplay;
    for (const output of env.outputs()) {
      if (!isInternal(output)) {
        continue;
      }
      if (off && !panelsOff.has(output.name)) {
        panelsOff.add(output.name);
        env.setPower("off", { output: output.name });
      } else if (!off && panelsOff.delete(output.name)) {
        env.setPower("on", { output: output.name });
      }
    }
  };

  const context = (): LidCloseContext => ({
    externalPower: env.powerState()?.externalPower,
    externalDisplay,
  });

  // logind's settings are plain strings and may name actions this does not
  // run (`kexec`).
  const resolve = (): LidCloseAction | string => {
    const state = env.powerState();
    const action = state?.externalPower
      ? (options.onExternalPower ?? "logind")
      : (options.onBattery ?? "logind");
    if (action !== "logind") {
      return action;
    }
    const logind = state?.logindLidActions;
    if (!logind) {
      return "ignore";
    }
    if (externalDisplay) {
      return logind.docked;
    }
    return state?.externalPower ? logind.externalPower : logind.lid;
  };

  const sleep = async (action: PowerAction) => {
    if (sleeping) {
      return;
    }
    sleeping = true;
    try {
      const callback = options.onSleep?.({ ...context(), action });
      if (callback) {
        let timer: ReturnType<typeof setTimeout> | undefined;
        await Promise.race([
          callback,
          new Promise<void>((done) => {
            timer = setTimeout(done, sleepCallbackTimeoutMs);
          }),
        ]);
        clearTimeout(timer);
      }
    } catch (error) {
      // Sleep anyway: a laptop shut in a bag must not stay awake.
      console.warn("lid policy: onSleep failed", error);
    } finally {
      sleeping = false;
    }
    env.perform(action);
  };

  const runCloseAction = () => {
    if (Date.now() < holdoffUntil) {
      return;
    }
    const action = resolve();
    if (typeof action === "function") {
      action(context());
    } else if (action === "ignore") {
      // Nothing to do.
    } else if (SLEEP_ACTIONS.includes(action)) {
      void sleep(action as PowerAction);
    } else if (POWER_ACTIONS.includes(action)) {
      env.perform(action as PowerAction);
    } else {
      console.warn(`lid policy: lid action ${action} is not supported; ignoring it`);
    }
  };

  const unsubscribeLid = env.onLidSwitch((event) => {
    lidClosed = event.closed;
    closedByMovement = event.closed && !event.initial;
    externalDisplay = hasExternalDisplay();
    syncPanels();
    if (closedByMovement) {
      runCloseAction();
    }
  });

  const unsubscribeOutputs = env.onOutputChange(() => {
    const hadExternalDisplay = externalDisplay;
    externalDisplay = hasExternalDisplay();
    syncPanels();
    // The last other display went with the lid shut: what closing it now
    // would do, as logind does.
    if (hadExternalDisplay && !externalDisplay && closedByMovement) {
      runCloseAction();
    }
  });

  const unsubscribeResume = env.onResume((event) => {
    holdoffUntil = Date.now() + holdoffMs;
    // A switch that still says closed after a resume may be stuck; it runs
    // nothing until it moves again.
    closedByMovement = false;
    if (event.lidClosed !== undefined) {
      lidClosed = event.lidClosed;
    }
    externalDisplay = hasExternalDisplay();
    syncPanels();
    options.onResume?.(event);
  });

  env.takeOver(true);
  externalDisplay = hasExternalDisplay();

  return () => {
    unsubscribeLid();
    unsubscribeOutputs();
    unsubscribeResume();
    lidClosed = false;
    syncPanels();
    env.takeOver(false);
  };
}
