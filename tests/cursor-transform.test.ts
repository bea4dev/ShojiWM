import assert from "node:assert/strict";
import { afterEach, test } from "node:test";
import { COMPOSITOR } from "../packages/shoji_wm/src/index";

type Call = [rotate: number, scaleX: number, scaleY: number];
const runtime = globalThis as { __SHOJI_CURSOR_TRANSFORM__?: (...args: Call) => void };

function installNative(): Call[] {
  const calls: Call[] = [];
  runtime.__SHOJI_CURSOR_TRANSFORM__ = (...args) => { calls.push(args); };
  return calls;
}

afterEach(() => { delete runtime.__SHOJI_CURSOR_TRANSFORM__; });

test("setTransform fills omitted fields with the identity", () => {
  const calls = installNative();
  COMPOSITOR.cursor.setTransform({ rotate: -12, scaleX: 1.3 });
  COMPOSITOR.cursor.setTransform({ scaleY: -1 });
  COMPOSITOR.cursor.setTransform(null);
  assert.deepEqual(calls, [[-12, 1.3, 1], [0, 1, -1], [0, 1, 1]]);
});

test("a zero, oversized or non-finite value throws instead of reaching the compositor", () => {
  const calls = installNative();
  assert.throws(() => COMPOSITOR.cursor.setTransform({ scaleX: 0 }));
  assert.throws(() => COMPOSITOR.cursor.setTransform({ scaleY: 9 }));
  assert.throws(() => COMPOSITOR.cursor.setTransform({ rotate: Number.NaN }));
  assert.deepEqual(calls, []);
});
