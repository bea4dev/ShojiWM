import assert from "node:assert/strict";
import { test } from "node:test";
import { SESSION_CONTROLLER } from "../packages/shoji_wm/src/session";
import { installSessionLockBridge } from "../packages/shoji_wm/src/session-lock";

test("COMPOSITOR.session.isLocked reads the compositor's lock state", () => {
  let locked = false;
  installSessionLockBridge({
    configureLockBackground: () => locked,
    isSessionLocked: () => locked,
  });
  assert.equal(SESSION_CONTROLLER.isLocked(), false);
  locked = true;
  assert.equal(SESSION_CONTROLLER.isLocked(), true);
});
