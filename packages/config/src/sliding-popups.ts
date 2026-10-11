import { cubicBezier, type WaylandWindow } from "shoji_wm";
import type { WindowPosition } from "shoji_wm/types";

// Sliding popups: a port of KWin's slidingpopups effect
// (kwin/src/plugins/slidingpopups/slidingpopups.cpp, Plasma 6.7).
//
// A plasmashell popup (Kickoff, the calendar, the tray popups) slides out of
// the panel it belongs to while it fades in, and slides back in when it
// closes:
//
//            ┌──────────┐  rest
//            │  popup   │
//            │  ↑ 152px │  opens: from 152 px towards the panel, OutCubic
//   ═════════╧══panel═══╧══════════════════════════════════════════
//
// KWin's numbers:
// - distance: 8 font heights (Noto Sans 10: 19 px, so 152 px), never more
//   than the popup itself
// - fade: the opacity follows the slide, but only when the popup is taller
//   than the distance (else it just slides out from behind the panel)
// - time: 200 ms times kdeglobals AnimationDurationFactor (0.25 here)
// - easing: OutCubic opening; closing runs the same curve backwards, which
//   is InCubic
// - the popup is clipped at the panel edge. Here the panel (a layer-shell
//   surface above the windows) covers that part instead.
//
// plasmashell asks KWin for the slide over org_kde_kwin_slide_manager (the
// panel edge). ShojiWM finds the edge itself: the panel next to the popup.

/** KWin's default SlideInTime / SlideOutTime. */
const KWIN_SLIDE_MS = 200;
/** kdeglobals [KDE] AnimationDurationFactor on this machine. */
const ANIMATION_DURATION_FACTOR = 0.25;
/** KWin's `animationTime(200ms)`. */
const SLIDE_MS = KWIN_SLIDE_MS * ANIMATION_DURATION_FACTOR;
/** QFontMetrics(QGuiApplication::font()).height() * 8, for Noto Sans 10 pt. */
const SLIDE_LENGTH = 19 * 8;
/** A slower slide for testing (debug IPC); `null` = `SLIDE_MS`. */
let debugSlideMs: number | null = null;

/** Qt's OutCubic and InCubic (KWin's OutCubic run backwards), as Béziers. */
const OUT_CUBIC = cubicBezier(0.33, 1, 0.68, 1);
const IN_CUBIC = cubicBezier(0.32, 0, 0.67, 0);

const CHANNEL = "sliding-popup";

type Edge = "top" | "bottom" | "left" | "right";

/** The panels' rects in global logical px (set by the config). */
let panelRects: () => readonly WindowPosition[] = () => [];

export function configureSlidingPopups(options: { panels: () => readonly WindowPosition[] }) {
  panelRects = options.panels;
}

/** Slow the slide down for testing (`null`: back to KWin's time). */
export function slidingPopupsDebug(durationMs: number | null) {
  debugSlideMs = durationMs;
}

function slideMs(): number {
  return debugSlideMs ?? SLIDE_MS;
}

/**
 * The panel edge the popup comes out of: the side of the nearest panel it
 * faces. `null` when there is no panel.
 */
function popupEdge(popup: WindowPosition): Edge | null {
  let best: { edge: Edge; gap: number } | null = null;
  for (const panel of panelRects()) {
    const horizontal = panel.width >= panel.height;
    let edge: Edge;
    let gap: number;
    if (horizontal) {
      const above = popup.y + popup.height / 2 < panel.y + panel.height / 2;
      // A popup above a bottom panel slides up out of its top edge.
      edge = above ? "bottom" : "top";
      gap = above ? panel.y - (popup.y + popup.height) : popup.y - (panel.y + panel.height);
    } else {
      const leftOf = popup.x + popup.width / 2 < panel.x + panel.width / 2;
      edge = leftOf ? "right" : "left";
      gap = leftOf ? panel.x - (popup.x + popup.width) : popup.x - (panel.x + panel.width);
    }
    const distance = Math.abs(gap);
    if (!best || distance < best.gap) {
      best = { edge, gap: distance };
    }
  }
  return best?.edge ?? null;
}

/** Where the popup sits at the start of opening (or the end of closing). */
function hiddenOffset(window: WaylandWindow): { x: number; y: number; fades: boolean } | null {
  const rect = window.rect;
  const edge = popupEdge(rect);
  if (!edge) {
    return null;
  }
  const along = edge === "top" || edge === "bottom" ? rect.height : rect.width;
  const distance = Math.min(along, SLIDE_LENGTH);
  // KWin: `if (slideLength < geo.height()) data.multiplyOpacity(t)`.
  const fades = SLIDE_LENGTH < along;
  switch (edge) {
    case "bottom":
      return { x: 0, y: distance, fades };
    case "top":
      return { x: 0, y: -distance, fades };
    case "right":
      return { x: distance, y: 0, fades };
    case "left":
      return { x: -distance, y: 0, fades };
  }
}

/** A plasmashell popup appeared: slide it out of its panel. */
export function slidePopupIn(window: WaylandWindow) {
  const hidden = hiddenOffset(window);
  if (!hidden) {
    return;
  }
  const duration = slideMs();
  // Keep the popup drawn while it slides back in on close.
  window.setCloseAnimationDuration(duration);
  window.scheduleAnimation({
    channel: CHANNEL,
    // The rect, not `offset`: the blur behind the popup follows the rect.
    rect: {
      from: { x: hidden.x, y: hidden.y, width: 0, height: 0 },
      to: { x: 0, y: 0, width: 0, height: 0 },
      duration,
      easing: OUT_CUBIC,
      mode: "add",
    },
    opacity: {
      from: hidden.fades ? 0 : 1,
      to: 1,
      duration,
      easing: OUT_CUBIC,
      mode: "multiply",
    },
  });
}

/** A plasmashell popup closes (Qt hides it): slide it back into its panel. */
export function slidePopupOut(window: WaylandWindow) {
  const hidden = hiddenOffset(window);
  if (!hidden) {
    return;
  }
  const duration = slideMs();
  window.scheduleAnimation({
    channel: CHANNEL,
    rect: {
      from: { x: 0, y: 0, width: 0, height: 0 },
      to: { x: hidden.x, y: hidden.y, width: 0, height: 0 },
      duration,
      easing: IN_CUBIC,
      mode: "add",
    },
    opacity: {
      from: 1,
      to: hidden.fades ? 0 : 1,
      duration,
      easing: IN_CUBIC,
      mode: "multiply",
    },
  });
}
