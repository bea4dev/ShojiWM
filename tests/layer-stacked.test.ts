import assert from "node:assert/strict";
import { test } from "node:test";
import { LAYER_CONTROLLER, updateLayerSnapshots } from "../packages/shoji_wm/src/layer";
import type {
  WaylandLayerKind,
  WaylandLayerSnapshot,
} from "../packages/shoji_wm/src/index";

function layer(id: string, kind: WaylandLayerKind, outputName = "DP-1"): WaylandLayerSnapshot {
  return {
    id,
    namespace: id,
    layer: kind,
    outputName,
    position: { x: 0, y: 0, width: 1920, height: 32 },
    anchor: { top: true, bottom: false, left: true, right: true },
    exclusiveZone: { mode: "neutral" },
    exclusiveEdge: null,
    margin: { top: 0, right: 0, bottom: 0, left: 0 },
    keyboardInteractivity: "none",
    desiredSize: { width: 0, height: 0 },
  };
}

const ids = () => LAYER_CONTROLLER.stacked.map((snapshot) => snapshot.id);

test("layers are listed from the bottom of the stack to its top", () => {
  // The ids sort the other way round, and the compositor lists the bar first.
  updateLayerSnapshots([
    layer("z-bar", "top"),
    layer("y-osd", "overlay"),
    layer("x-wallpaper", "background"),
    layer("a-dock", "top"),
  ]);
  assert.deepEqual(ids(), ["x-wallpaper", "z-bar", "a-dock", "y-osd"]);

  // The same surfaces stacked the other way within the top layer.
  updateLayerSnapshots([
    layer("a-dock", "top"),
    layer("z-bar", "top"),
    layer("x-wallpaper", "background"),
  ]);
  assert.deepEqual(ids(), ["x-wallpaper", "a-dock", "z-bar"]);
});

test("numeric-looking ids keep the compositor's order", () => {
  // Object keys that look like integers enumerate in numeric order.
  updateLayerSnapshots([layer("20", "top"), layer("3", "top", "HDMI-A-1")]);
  assert.deepEqual(ids(), ["20", "3"]);
});

test("stacked hands out copies", () => {
  updateLayerSnapshots([layer("bar", "top")]);
  const [first] = LAYER_CONTROLLER.stacked;
  (first.position as { x: number }).x = 99;
  assert.equal(LAYER_CONTROLLER.stacked[0].position.x, 0);
});
