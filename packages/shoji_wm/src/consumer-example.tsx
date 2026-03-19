import { type WaylandWindow } from "shoji_wm";
import { defaultWindowDecoration } from "shoji_wm/default-decoration";

export const exampleDecoration = (window: WaylandWindow) =>
  defaultWindowDecoration(window);
