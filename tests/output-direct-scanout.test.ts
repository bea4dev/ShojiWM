import assert from "node:assert/strict";
import { test } from "node:test";
import {
  configureOutputs,
  takePendingDisplayConfig,
  updateOutputState,
} from "../packages/shoji_wm/src/output";

test("directScanout reaches the compositor with an output's config, and only when set", () => {
  updateOutputState({
    "DP-1": { name: "DP-1", position: { x: 0, y: 0 }, scale: 1, availableModes: [] },
    "eDP-1": { name: "eDP-1", position: { x: 0, y: 0 }, scale: 1, availableModes: [] },
  });
  configureOutputs(() => ({
    "DP-1": { mode: "extend", directScanout: false },
    "eDP-1": { mode: "extend" },
  }));
  const config = takePendingDisplayConfig();
  assert.equal(config?.["DP-1"]?.directScanout, false);
  assert.equal(config?.["eDP-1"]?.directScanout, undefined);
  assert.equal(JSON.stringify(config?.["eDP-1"]).includes("directScanout"), false);
});
