import assert from "node:assert/strict";
import { afterEach, test } from "node:test";
import { COMPOSITOR } from "../packages/shoji_wm/src/index";

type Call = [action: string, name: string, width: number, height: number, scale: number];
const runtime = globalThis as { __SHOJI_HEADLESS_OUTPUT__?: (...args: Call) => void };

function installNative(): Call[] {
  const calls: Call[] = [];
  runtime.__SHOJI_HEADLESS_OUTPUT__ = (...args) => { calls.push(args); };
  return calls;
}

afterEach(() => { delete runtime.__SHOJI_HEADLESS_OUTPUT__; });

test("create names unnamed outputs HEADLESS-<n>, lowest n free", () => {
  const calls = installNative();
  const first = COMPOSITOR.output.create({ width: 800, height: 600 });
  const second = COMPOSITOR.output.create({ width: 1024, height: 768, scale: 1.5 });
  COMPOSITOR.output.remove(first);
  const third = COMPOSITOR.output.create({ width: 640, height: 480 });
  COMPOSITOR.output.remove(second);
  COMPOSITOR.output.remove(third);
  assert.deepEqual([first, second, third], ["HEADLESS-1", "HEADLESS-2", "HEADLESS-1"]);
  assert.deepEqual(calls, [
    ["create", "HEADLESS-1", 800, 600, 0],
    ["create", "HEADLESS-2", 1024, 768, 1.5],
    ["remove", "HEADLESS-1", 0, 0, 0],
    ["create", "HEADLESS-1", 640, 480, 0],
    ["remove", "HEADLESS-2", 0, 0, 0],
    ["remove", "HEADLESS-1", 0, 0, 0],
  ]);
});

test("a given name is kept", () => {
  const calls = installNative();
  assert.equal(COMPOSITOR.output.create({ name: "corner", width: 32, height: 32 }), "corner");
  COMPOSITOR.output.remove("corner");
  assert.deepEqual(calls[0], ["create", "corner", 32, 32, 0]);
});

test("a bad size or scale throws instead of reaching the compositor", () => {
  const calls = installNative();
  assert.throws(() => COMPOSITOR.output.create({ width: 0, height: 600 }), TypeError);
  assert.throws(() => COMPOSITOR.output.create({ width: 800.5, height: 600 }), TypeError);
  assert.throws(() => COMPOSITOR.output.create({ width: 800, height: 600, scale: -1 }), TypeError);
  assert.deepEqual(calls, []);
});
