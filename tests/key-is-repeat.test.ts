import assert from "node:assert/strict";
import { test } from "node:test";
import { KEY_BINDING_CONTROLLER, installKeyBridge } from "../packages/shoji_wm/src/key";

test("COMPOSITOR.key.isRepeat reads whether the handler runs for a held key's repeat", () => {
  let repeat = false;
  installKeyBridge({ isKeyRepeat: () => repeat });
  assert.equal(KEY_BINDING_CONTROLLER.isRepeat(), false);
  repeat = true;
  assert.equal(KEY_BINDING_CONTROLLER.isRepeat(), true);
});
