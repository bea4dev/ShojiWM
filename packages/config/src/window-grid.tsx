/**
 * Window grid: an overview style window switcher (a `SwitcherLayout`, see
 * `./window-switcher.tsx` for what every switcher shares).
 *
 * `Super+Tab` lays every window out side by side in a flat grid filling the
 * output, most recently used first. It stays open until a window is picked:
 *
 * - hovering a window selects it, a click switches to it, a click on empty
 *   space returns to the desktop
 * - `Tab` / `Shift+Tab`, arrow keys and the mouse wheel move the selection;
 *   `Return` / `Space` switches to it
 * - like Alt+Tab, holding `Super` and pressing `Tab` (or an arrow key) switches
 *   when `Super` goes up; a quick `Super+Tab` just opens the grid
 * - `Escape` returns to the desktop unchanged
 */
import { COMPOSITOR, type WaylandWindow } from "shoji_wm";
import {
  createWindowSwitcher,
  flatPose,
  mod,
  type Rect,
  type SwitcherControls,
  type SwitcherLayout,
  type Viewport,
  type WindowSwitcher,
  type WindowSwitcherOptions,
} from "./window-switcher";

/** How far the desktop dims behind the grid. */
const DIM_ALPHA = 0.55;
/** Space kept clear around the grid, inside the usable area (logical px). */
const PADDING = 48;
/** Space between windows (logical px); room for their shadows. */
const GAP = 40;
/** Windows are never drawn larger than this. */
const MAX_SCALE = 1;

/** A window's place in the grid (logical px from the output's top-left corner). */
interface Cell {
  centerX: number;
  centerY: number;
  scale: number;
  row: number;
}

interface GridState {
  windows: readonly WaylandWindow[];
  /** The grid for the last viewport it was laid out on. */
  cells: Cell[];
  cellsKey: string;
  /** `Tab` or an arrow key went down while `Super` was held. */
  navigatedWithSuper: boolean;
}

export function createWindowGrid(options: WindowSwitcherOptions): WindowSwitcher {
  return createWindowSwitcher(WINDOW_GRID_LAYOUT, options);
}

const WINDOW_GRID_LAYOUT: SwitcherLayout<GridState> = {
  name: "window-grid",
  dimAlpha: DIM_ALPHA,

  begin: (windows) => ({
    windows,
    cells: [],
    cellsKey: "",
    navigatedWithSuper: false,
  }),

  // The previous window, so `Return` right away switches back like Alt+Tab.
  initialSelection: (count) => (count > 1 ? 1 : 0),

  pose(state, index, _rect, viewport) {
    const cell = cellsFor(state, viewport)[index];
    return flatPose(cell.centerX, cell.centerY, cell.scale, viewport);
  },

  onKey(state, event, controls) {
    let next: number | null = null;
    switch (event.key) {
      case "Tab":
      case "ISO_Left_Tab":
        next = controls.selected + (event.modifiers.shift ? -1 : 1);
        break;
      case "Right":
        next = controls.selected + 1;
        break;
      case "Left":
        next = controls.selected - 1;
        break;
      case "Up":
      case "Down":
        next = neighbourRow(state, controls, event.key === "Up" ? -1 : 1);
        break;
      default:
        return false;
    }
    if (event.modifiers.super) {
      state.navigatedWithSuper = true;
    }
    if (next !== null) {
      controls.select(mod(next, state.windows.length));
    }
    return true;
  },

  onSuperReleased(state, controls) {
    if (state.navigatedWithSuper) {
      controls.commit();
    }
  },

  onPointerMotion(state, event, controls) {
    const window = controls.windowAt(event.position.x, event.position.y);
    if (window) {
      controls.select(state.windows.indexOf(window));
    }
  },

  onScroll(state, event, controls) {
    if (event.source !== "wheel") {
      return;
    }
    const clicks = (event.discreteY ?? Math.sign(event.deltaY) * 120) / 120;
    const steps = Math.sign(clicks) * Math.max(1, Math.round(Math.abs(clicks)));
    controls.select(mod(controls.selected + steps, state.windows.length));
  },

  onClickOutside: (_state, controls) => controls.cancel(),
};

/** The window in the row above (`-1`) or below (`1`) nearest the selection. */
function neighbourRow(state: GridState, controls: SwitcherControls, direction: number): number | null {
  const cells = state.cells;
  const from = cells[mod(controls.selected, cells.length)];
  if (!from) {
    return null;
  }
  let best: number | null = null;
  cells.forEach((cell, index) => {
    if (cell.row !== from.row + direction) {
      return;
    }
    if (best === null || Math.abs(cell.centerX - from.centerX) < Math.abs(cells[best].centerX - from.centerX)) {
      best = index;
    }
  });
  return best;
}

function cellsFor(state: GridState, viewport: Viewport): Cell[] {
  const area = gridArea(viewport);
  const key = `${area.x},${area.y},${area.width},${area.height}`;
  if (key !== state.cellsKey) {
    state.cells = layoutGrid(state.windows.map((window) => window.rect), area);
    state.cellsKey = key;
  }
  return state.cells;
}

/** Where the grid goes: the output's usable area (clear of bars), padded. */
function gridArea({ output, width, height }: Viewport): Rect {
  const usable = COMPOSITOR.layer.usableArea(output.name);
  const x = usable ? usable.x - output.position.x : 0;
  const y = usable ? usable.y - output.position.y : 0;
  return {
    x: x + PADDING,
    y: y + PADDING,
    width: Math.max(1, (usable?.width ?? width) - PADDING * 2),
    height: Math.max(1, (usable?.height ?? height) - PADDING * 2),
  };
}

/**
 * Rows of windows in order, each row centred, the rows centred together. The
 * column count is the one that shows the windows largest overall.
 */
export function layoutGrid(sizes: readonly { width: number; height: number }[], area: Rect): Cell[] {
  const count = sizes.length;
  if (count === 0) {
    return [];
  }
  let best: { columns: number; scales: number[]; score: number } | null = null;
  for (let columns = 1; columns <= count; columns++) {
    const rows = Math.ceil(count / columns);
    const cellWidth = (area.width - (columns - 1) * GAP) / columns;
    const cellHeight = (area.height - (rows - 1) * GAP) / rows;
    if (cellWidth <= 0 || cellHeight <= 0) {
      continue;
    }
    const scales = sizes.map((size) =>
      Math.min(MAX_SCALE, cellWidth / Math.max(size.width, 1), cellHeight / Math.max(size.height, 1)),
    );
    const score = sizes.reduce(
      (sum, size, index) => sum + size.width * size.height * scales[index] ** 2,
      0,
    );
    if (!best || score >= best.score) {
      best = { columns, scales, score };
    }
  }
  if (!best) {
    return sizes.map(() => ({
      centerX: area.x + area.width / 2,
      centerY: area.y + area.height / 2,
      scale: 0,
      row: 0,
    }));
  }
  const { columns, scales } = best;
  const rows: number[][] = [];
  for (let start = 0; start < count; start += columns) {
    rows.push(Array.from({ length: Math.min(columns, count - start) }, (_, offset) => start + offset));
  }
  const rowHeights = rows.map((row) => Math.max(...row.map((index) => sizes[index].height * scales[index])));
  const totalHeight = rowHeights.reduce((sum, rowHeight) => sum + rowHeight, 0) + (rows.length - 1) * GAP;
  const cells: Cell[] = new Array(count);
  let top = area.y + (area.height - totalHeight) / 2;
  rows.forEach((row, rowIndex) => {
    const widths = row.map((index) => sizes[index].width * scales[index]);
    const rowWidth = widths.reduce((sum, width) => sum + width, 0) + (row.length - 1) * GAP;
    let left = area.x + (area.width - rowWidth) / 2;
    row.forEach((index, column) => {
      cells[index] = {
        centerX: left + widths[column] / 2,
        centerY: top + rowHeights[rowIndex] / 2,
        scale: scales[index],
        row: rowIndex,
      };
      left += widths[column] + GAP;
    });
    top += rowHeights[rowIndex] + GAP;
  });
  return cells;
}
