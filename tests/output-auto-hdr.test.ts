import assert from "node:assert/strict";
import { test } from "node:test";
import {
  configureOutputs,
  takePendingDisplayConfig,
  updateOutputState,
} from "../packages/shoji_wm/src/output";

test('hdr: "auto" reaches the compositor as is, alone or inside the HDR settings', () => {
  updateOutputState({
    "DP-1": { name: "DP-1", position: { x: 0, y: 0 }, scale: 1, availableModes: [] },
    "DP-2": { name: "DP-2", position: { x: 0, y: 0 }, scale: 1, availableModes: [] },
  });
  configureOutputs(() => ({
    "DP-1": { mode: "extend", hdr: "auto" },
    "DP-2": { mode: "extend", hdr: { enabled: "auto", sdrLuminance: 250 } },
  }));
  const config = takePendingDisplayConfig();
  assert.equal(config?.["DP-1"]?.hdr, "auto");
  assert.deepEqual(config?.["DP-2"]?.hdr, { enabled: "auto", sdrLuminance: 250 });
});
