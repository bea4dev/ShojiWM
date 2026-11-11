import assert from "node:assert/strict";
import { afterEach, test } from "node:test";
import { COMPOSITOR } from "../packages/shoji_wm/src/index";

type Call = [r: number, g: number, b: number, a: number];
const runtime = globalThis as { __SHOJI_CLEAR_COLOR__?: (...args: Call) => void };

function installNative(): Call[] {
  const calls: Call[] = [];
  runtime.__SHOJI_CLEAR_COLOR__ = (...args) => { calls.push(args); };
  return calls;
}

afterEach(() => { delete runtime.__SHOJI_CLEAR_COLOR__; });

test("setClearColor sends premultiplied RGBA", () => {
  const calls = installNative();
  COMPOSITOR.output.setClearColor("#000");
  COMPOSITOR.output.setClearColor([1, 0.5, 0, 0.5]);
  assert.deepEqual(calls, [[0, 0, 0, 1], [0.5, 0.25, 0, 0.5]]);
});

test("an invalid colour throws instead of reaching the compositor", () => {
  const calls = installNative();
  assert.throws(() => COMPOSITOR.output.setClearColor("black"), TypeError);
  assert.deepEqual(calls, []);
});
