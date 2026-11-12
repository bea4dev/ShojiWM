import assert from "node:assert/strict";
import { afterEach, beforeEach, test } from "node:test";
import {
  bindLidPolicy,
  type LidPolicyEnvironment,
  type LidPolicyOptions,
} from "../packages/config/src/lid-policy";
import type {
  LidSwitchListener,
  OutputChangeListener,
  OutputInfo,
  PowerState,
  ResumeEvent,
  ResumeListener,
} from "../packages/shoji_wm/src/index";

const HOLDOFF = 30_000;

let now = 0;
const realNow = Date.now;
let calls: string[];
let panels: string[];
let outputs: OutputInfo[];
let state: PowerState | undefined;
let lidListeners: LidSwitchListener[];
let outputListeners: OutputChangeListener[];
let resumeListeners: ResumeListener[];

function remove<T>(list: T[], item: T): () => void {
  return () => {
    const index = list.indexOf(item);
    if (index >= 0) list.splice(index, 1);
  };
}

const env: LidPolicyEnvironment = {
  onLidSwitch: (listener) => (lidListeners.push(listener), remove(lidListeners, listener)),
  onOutputChange: (listener) => (outputListeners.push(listener), remove(outputListeners, listener)),
  outputs: () => outputs,
  setPower: (power, options) => { panels.push(`${options.output}:${power}`); },
  powerState: () => state,
  onResume: (listener) => (resumeListeners.push(listener), remove(resumeListeners, listener)),
  takeOver: (lid) => { calls.push(`take-over:${lid}`); },
  perform: (action) => { calls.push(action); },
};

function bind(options: LidPolicyOptions = {}) {
  return bindLidPolicy(options, env);
}

function lid(closed: boolean, initial = false) {
  for (const listener of [...lidListeners]) listener({ closed, initial });
}

function resume(event: ResumeEvent) {
  for (const listener of [...resumeListeners]) listener(event);
}

function setOutputs(names: string[]) {
  outputs = names.map((name) => ({ name, enabled: true }) as OutputInfo);
  for (const listener of [...outputListeners]) listener({} as never);
}

function logind(next: Partial<PowerState> = {}) {
  state = {
    externalPower: false,
    logindLidActions: { lid: "suspend", externalPower: "ignore", docked: "ignore" },
    ...next,
  };
}

/** Let `onSleep` promises and the sleep after them run. */
const settle = () => new Promise<void>((done) => setImmediate(done));

beforeEach(() => {
  now = 1_000_000;
  Date.now = () => now;
  calls = [];
  panels = [];
  outputs = [];
  state = undefined;
  lidListeners = [];
  outputListeners = [];
  resumeListeners = [];
});

afterEach(() => {
  Date.now = realNow;
});

test("follows logind by power source, only for a movement after the holdoff", async () => {
  logind();
  setOutputs(["eDP-1"]);
  const unbind = bind();
  assert.deepEqual(calls, ["take-over:true"]);

  // Closed at startup: where the lid is, not a movement.
  lid(true, true);
  // A movement within the holdoff after the start.
  now += 1_000;
  lid(false);
  lid(true);
  await settle();
  assert.deepEqual(calls, ["take-over:true"]);

  now += HOLDOFF;
  lid(false);
  lid(true);
  await settle();
  assert.deepEqual(calls, ["take-over:true", "suspend"]);

  // On external power logind is set to ignore it.
  logind({ externalPower: true });
  lid(false);
  lid(true);
  await settle();
  assert.deepEqual(calls, ["take-over:true", "suspend"]);

  unbind();
  assert.deepEqual(calls, ["take-over:true", "suspend", "take-over:false"]);
});

test("battery and external power each have their own action", async () => {
  logind();
  setOutputs(["eDP-1"]);
  const unbind = bind({ onBattery: "hibernate", onExternalPower: "suspend" });
  now += HOLDOFF;
  lid(true);
  await settle();
  logind({ externalPower: true });
  lid(false);
  lid(true);
  await settle();
  assert.deepEqual(calls, ["take-over:true", "hibernate", "suspend"]);
  unbind();
});

test("with another display on, the built-in panel follows the lid", async () => {
  logind();
  setOutputs(["eDP-1", "DP-1"]);
  const unbind = bind();
  now += HOLDOFF;

  lid(true);
  await settle();
  // logind's HandleLidSwitchDocked is ignore; the panel goes dark.
  assert.deepEqual(calls, ["take-over:true"]);
  assert.deepEqual(panels, ["eDP-1:off"]);
  lid(false);
  assert.deepEqual(panels, ["eDP-1:off", "eDP-1:on"]);

  // Unplugging the display with the lid shut runs the battery action.
  lid(true);
  setOutputs(["eDP-1"]);
  await settle();
  assert.deepEqual(panels, ["eDP-1:off", "eDP-1:on", "eDP-1:off", "eDP-1:on"]);
  assert.deepEqual(calls, ["take-over:true", "suspend"]);
  unbind();
});

test("a lid closed at boot on a dock keeps the panel dark but runs nothing", async () => {
  logind({ logindLidActions: { lid: "suspend", externalPower: "suspend", docked: "suspend" } });
  setOutputs(["eDP-1", "DP-1"]);
  const unbind = bind();
  now += HOLDOFF;
  lid(true, true);
  assert.deepEqual(panels, ["eDP-1:off"]);
  setOutputs(["eDP-1"]);
  await settle();
  assert.deepEqual(panels, ["eDP-1:off", "eDP-1:on"]);
  assert.deepEqual(calls, ["take-over:true"]);
  unbind();
});

test("a resume that finds the lid closed runs nothing and restarts the holdoff", async () => {
  logind();
  setOutputs(["eDP-1"]);
  const resumes: ResumeEvent[] = [];
  const unbind = bind({ onBattery: "hibernate", onResume: (event) => resumes.push(event) });
  now += HOLDOFF;
  lid(true);
  await settle();
  assert.deepEqual(calls, ["take-over:true", "hibernate"]);

  // The switch is stuck: no open came, and the resume says closed.
  resume({ lidClosed: true });
  assert.deepEqual(resumes, [{ lidClosed: true }]);
  now += 1_000;
  lid(false);
  lid(true);
  await settle();
  assert.deepEqual(calls, ["take-over:true", "hibernate"]);

  now += HOLDOFF;
  lid(false);
  lid(true);
  await settle();
  assert.deepEqual(calls, ["take-over:true", "hibernate", "hibernate"]);
  unbind();
});

test("onSleep runs before the sleep, which waits for its promise", async () => {
  logind({ externalPower: true });
  setOutputs(["eDP-1"]);
  let unlock: () => void = () => {};
  const seen: unknown[] = [];
  const unbind = bind({
    onExternalPower: "suspend",
    onSleep: (context) => {
      seen.push(context);
      return new Promise<void>((done) => { unlock = done; });
    },
  });
  now += HOLDOFF;
  lid(true);
  await settle();
  assert.deepEqual(seen, [{ action: "suspend", externalPower: true, externalDisplay: false }]);
  // Waiting for the locker.
  assert.deepEqual(calls, ["take-over:true"]);
  unlock();
  await settle();
  assert.deepEqual(calls, ["take-over:true", "suspend"]);
  unbind();
});

test("a failing onSleep still sleeps, and non-sleep actions skip it", async () => {
  logind();
  setOutputs(["eDP-1"]);
  const warn = console.warn;
  console.warn = () => {};
  let called = 0;
  const unbind = bind({
    onSleep: () => {
      called++;
      throw new Error("locker missing");
    },
  });
  now += HOLDOFF;
  lid(true);
  await settle();
  assert.deepEqual(calls, ["take-over:true", "suspend"]);
  unbind();

  const unbindLock = bind({ onBattery: "lock", onSleep: () => { called++; } });
  now += HOLDOFF;
  lid(false);
  lid(true);
  await settle();
  console.warn = warn;
  assert.equal(called, 1);
  assert.deepEqual(calls.slice(-1), ["lock"]);
  unbindLock();
});

test("a function decides with the situation", () => {
  logind({ externalPower: true });
  setOutputs(["eDP-1"]);
  const seen: unknown[] = [];
  const unbind = bind({ onExternalPower: (context) => { seen.push(context); } });
  now += HOLDOFF;
  lid(true);
  assert.deepEqual(seen, [{ externalPower: true, externalDisplay: false }]);
  unbind();
});

test("an onSleep that never finishes is given up on after the timeout", async () => {
  logind();
  setOutputs(["eDP-1"]);
  const unbind = bind({
    onSleep: () => new Promise<void>(() => {}),
    sleepCallbackTimeoutMs: 10,
  });
  now += HOLDOFF;
  lid(true);
  await settle();
  assert.deepEqual(calls, ["take-over:true"]);
  await new Promise((done) => setTimeout(done, 30));
  assert.deepEqual(calls, ["take-over:true", "suspend"]);
  unbind();
});
