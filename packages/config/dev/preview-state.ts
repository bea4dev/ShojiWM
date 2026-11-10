// Knobs for the preview, re-read on every reload (Super+Shift+R, or
// `shoji_wm -r` against the nested compositor's socket).
export const PREVIEW = {
  /** The nested output's scale. */
  scale: 1,
  /**
   * Draw this caption button of the focused window as if the pointer were on
   * it: "close", "maximize:pressed", ... or "" for none.
   */
  button: "",
  /**
   * Run once after this reload, on the focused window: "maximize" (toggles),
   * "fullscreen" (toggles), "tile" (toggles tiling of the workspace),
   * "switcher" (opens the window switcher), "drag" (moves the focused window
   * around for `dragSeconds`, to profile with), "drag-all" (moves every
   * window, like a tiled workspace scrolling), "focus-cycle" (moves the tile
   * focus back and forth, like Super+Ctrl+Left/Right), "switcher-cycle"
   * (opens the switcher and flips it for `dragSeconds`), or "".
   */
  action: "",
  /** The window switcher's style, as WINDOW_SWITCHER_STYLE in src/index.tsx. */
  switcher: "flip-3d" as "flip-3d" | "grid",
  dragSeconds: 10,
  /** Profile a "drag" / "drag-all" / "focus-cycle" / "switcher-cycle" with timescope (dumped to $HOME/shoji_wm/logs). */
  profile: false,
};
