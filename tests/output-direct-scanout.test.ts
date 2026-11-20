import assert from "node:assert/strict";
import { test } from "node:test";
import {
  configureOutputs,
  takePendingDisplayConfig,
  updateOutputState,
} from "../packages/shoji_wm/src/output";

test("directScanout reaches the compositor with an output's config, and only when set", () => {
  updateOutputState({
    "DP-1": { name: "DP-1", position: { x: 0, y: 0 }, scale: 1, availableModes: [], hdrSupported: false },
    "eDP-1": { name: "eDP-1", position: { x: 0, y: 0 }, scale: 1, availableModes: [], hdrSupported: false },
  });
  configureOutputs(() => ({
    "DP-1": { mode: "extend", directScanout: false },
    "eDP-1": { mode: "extend" },
  }));
  const config = takePendingDisplayConfig();
  const dp1 = config?.["DP-1"];
  const edp1 = config?.["eDP-1"];
  assert.ok(dp1?.mode === "extend" && edp1?.mode === "extend");
  assert.equal(dp1.directScanout, false);
  assert.equal(edp1.directScanout, undefined);
  assert.equal(JSON.stringify(config?.["eDP-1"]).includes("directScanout"), false);
});
