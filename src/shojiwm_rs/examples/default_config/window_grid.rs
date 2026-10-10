//! Port of `packages/config/src/window-grid.tsx`: an overview style window
//! switcher (a `SwitcherLayout`, see `window_switcher.rs` for what every
//! switcher shares).
//!
//! `Super+Tab` lays every window out side by side in a flat grid filling the
//! output, most recently used first. It stays open until a window is picked:
//!
//! - hovering a window selects it, a click switches to it, a click on empty
//!   space returns to the desktop
//! - `Tab` / `Shift+Tab`, arrow keys and the mouse wheel move the selection;
//!   `Return` / `Space` switches to it
//! - like Alt+Tab, holding `Super` and pressing `Tab` (or an arrow key) switches
//!   when `Super` goes up; a quick `Super+Tab` just opens the grid
//! - `Escape` returns to the desktop unchanged

use std::cell::RefCell;

use shojiwm_rs::prelude::*;

use crate::{
    window_manager::WindowManager,
    window_switcher::{LayoutSession, Pose, SwitcherAction, SwitcherLayout, Viewport, WindowSwitcher, flat_pose},
};

/// How far the desktop dims behind the grid.
const DIM_ALPHA: f64 = 0.55;
/// Space kept clear around the grid, inside the usable area (logical px).
const PADDING: f64 = 48.0;
/// Space between windows (logical px); room for their shadows.
const GAP: f64 = 40.0;
/// Windows are never drawn larger than this.
const MAX_SCALE: f64 = 1.0;

pub fn create_window_grid(wm: WindowManager, stack_order: impl Fn(Window) -> i32 + 'static) -> WindowSwitcher {
    WindowSwitcher::new(WindowGrid, wm, stack_order)
}

struct WindowGrid;

impl SwitcherLayout for WindowGrid {
    fn name(&self) -> &'static str {
        "window-grid"
    }

    fn dim_alpha(&self) -> f64 {
        DIM_ALPHA
    }

    // The previous window, so `Return` right away switches back like Alt+Tab.
    fn initial_selection(&self, count: usize) -> i64 {
        if count > 1 { 1 } else { 0 }
    }

    fn begin(&self, windows: &[Window]) -> Box<dyn LayoutSession> {
        Box::new(GridSession {
            windows: windows.to_vec(),
            cells: RefCell::new((None, Vec::new())),
            navigated_with_super: false,
        })
    }
}

/// A window's place in the grid (logical px from the output's top-left corner).
#[derive(Debug, Clone, Copy, PartialEq)]
struct Cell {
    center_x: f64,
    center_y: f64,
    scale: f64,
    row: usize,
}

struct GridSession {
    windows: Vec<Window>,
    /// The grid for the last area it was laid out in.
    cells: RefCell<(Option<Rect>, Vec<Cell>)>,
    /// `Tab` or an arrow key went down while `Super` was held.
    navigated_with_super: bool,
}

impl GridSession {
    fn cell(&self, index: usize, viewport: &Viewport) -> Cell {
        let area = grid_area(viewport);
        let mut cells = self.cells.borrow_mut();
        if cells.0 != Some(area) {
            let sizes: Vec<(f64, f64)> = self
                .windows
                .iter()
                .map(|window| {
                    let rect = window.rect();
                    (rect.width, rect.height)
                })
                .collect();
            *cells = (Some(area), layout_grid(&sizes, area));
        }
        cells.1[index]
    }

    /// The window in the row above (`-1`) or below (`1`) nearest the selection.
    fn neighbour_row(&self, selected: i64, direction: i64) -> Option<i64> {
        let cells = self.cells.borrow();
        let cells = &cells.1;
        let from = cells.get(selected.rem_euclid(cells.len().max(1) as i64) as usize)?;
        let row = from.row as i64 + direction;
        cells
            .iter()
            .enumerate()
            .filter(|(_, cell)| cell.row as i64 == row)
            .min_by(|(_, a), (_, b)| {
                (a.center_x - from.center_x)
                    .abs()
                    .total_cmp(&(b.center_x - from.center_x).abs())
            })
            .map(|(index, _)| index as i64)
    }

    fn select(&self, index: i64) -> SwitcherAction {
        SwitcherAction::Select(index.rem_euclid(self.windows.len().max(1) as i64))
    }
}

impl LayoutSession for GridSession {
    fn pose(&self, index: usize, _rect: Rect, viewport: &Viewport, _selected: i64) -> Pose {
        let cell = self.cell(index, viewport);
        flat_pose(cell.center_x, cell.center_y, cell.scale, viewport)
    }

    fn on_key(&mut self, event: &InputGrabKeyEvent, selected: i64) -> Option<SwitcherAction> {
        let next = match event.key.as_str() {
            "Tab" | "ISO_Left_Tab" => Some(selected + if event.modifiers.shift { -1 } else { 1 }),
            "Right" => Some(selected + 1),
            "Left" => Some(selected - 1),
            "Up" => self.neighbour_row(selected, -1),
            "Down" => self.neighbour_row(selected, 1),
            _ => return None,
        };
        if event.modifiers.logo {
            self.navigated_with_super = true;
        }
        Some(next.map_or(SwitcherAction::None, |index| self.select(index)))
    }

    fn on_super_released(&mut self, _selected: i64) -> SwitcherAction {
        if self.navigated_with_super {
            SwitcherAction::Commit
        } else {
            SwitcherAction::None
        }
    }

    fn on_pointer_motion(&mut self, hit: Option<usize>, _selected: i64) -> SwitcherAction {
        hit.map_or(SwitcherAction::None, |index| SwitcherAction::Select(index as i64))
    }

    fn on_scroll(&mut self, event: &InputGrabScrollEvent, selected: i64) -> SwitcherAction {
        if event.source != "wheel" {
            return SwitcherAction::None;
        }
        let clicks = event.discrete_y.unwrap_or(event.delta_y.signum() * 120.0) / 120.0;
        let steps = clicks.signum() as i64 * clicks.abs().round().max(1.0) as i64;
        self.select(selected + steps)
    }

    fn on_click_outside(&mut self) -> SwitcherAction {
        SwitcherAction::Cancel
    }
}

/// Where the grid goes: the output's usable area (clear of bars), padded.
fn grid_area(viewport: &Viewport) -> Rect {
    let output = &viewport.output;
    let usable = COMPOSITOR.layer.usable_area(&output.name).unwrap_or(Rect::new(
        output.position.x as f64,
        output.position.y as f64,
        viewport.width,
        viewport.height,
    ));
    Rect::new(
        usable.x - output.position.x as f64 + PADDING,
        usable.y - output.position.y as f64 + PADDING,
        (usable.width - PADDING * 2.0).max(1.0),
        (usable.height - PADDING * 2.0).max(1.0),
    )
}

/// Rows of windows in order, each row centred, the rows centred together. The
/// column count is the one that shows the windows largest overall.
fn layout_grid(sizes: &[(f64, f64)], area: Rect) -> Vec<Cell> {
    let count = sizes.len();
    let mut best: Option<(usize, Vec<f64>, f64)> = None;
    for columns in 1..=count {
        let rows = count.div_ceil(columns);
        let cell_width = (area.width - (columns - 1) as f64 * GAP) / columns as f64;
        let cell_height = (area.height - (rows - 1) as f64 * GAP) / rows as f64;
        if cell_width <= 0.0 || cell_height <= 0.0 {
            continue;
        }
        let scales: Vec<f64> = sizes
            .iter()
            .map(|(width, height)| {
                MAX_SCALE
                    .min(cell_width / width.max(1.0))
                    .min(cell_height / height.max(1.0))
            })
            .collect();
        let score: f64 = sizes
            .iter()
            .zip(&scales)
            .map(|((width, height), scale)| width * height * scale * scale)
            .sum();
        if best.as_ref().is_none_or(|(_, _, best_score)| score >= *best_score) {
            best = Some((columns, scales, score));
        }
    }
    let Some((columns, scales, _)) = best else {
        let centre = Cell {
            center_x: area.x + area.width / 2.0,
            center_y: area.y + area.height / 2.0,
            scale: 0.0,
            row: 0,
        };
        return vec![centre; count];
    };
    let rows: Vec<Vec<usize>> = (0..count)
        .collect::<Vec<_>>()
        .chunks(columns)
        .map(<[usize]>::to_vec)
        .collect();
    let row_heights: Vec<f64> = rows
        .iter()
        .map(|row| row.iter().map(|&index| sizes[index].1 * scales[index]).fold(0.0, f64::max))
        .collect();
    let total_height = row_heights.iter().sum::<f64>() + (rows.len() - 1) as f64 * GAP;
    let mut cells = vec![
        Cell {
            center_x: 0.0,
            center_y: 0.0,
            scale: 0.0,
            row: 0,
        };
        count
    ];
    let mut top = area.y + (area.height - total_height) / 2.0;
    for (row_index, row) in rows.iter().enumerate() {
        let widths: Vec<f64> = row.iter().map(|&index| sizes[index].0 * scales[index]).collect();
        let row_width = widths.iter().sum::<f64>() + (row.len() - 1) as f64 * GAP;
        let mut left = area.x + (area.width - row_width) / 2.0;
        for (column, &index) in row.iter().enumerate() {
            cells[index] = Cell {
                center_x: left + widths[column] / 2.0,
                center_y: top + row_heights[row_index] / 2.0,
                scale: scales[index],
                row: row_index,
            };
            left += widths[column] + GAP;
        }
        top += row_heights[row_index] + GAP;
    }
    cells
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_fills_rows_in_order_and_centres_them() {
        let area = Rect::new(0.0, 0.0, 1000.0, 1000.0);
        let cells = layout_grid(&[(400.0, 300.0); 3], area);
        // Two columns fit 400×300 windows unscaled; the third sits centred
        // on its own row.
        assert_eq!(cells.iter().map(|cell| cell.row).collect::<Vec<_>>(), [0, 0, 1]);
        assert!(cells.iter().all(|cell| cell.scale == 1.0));
        assert_eq!(cells[2].center_x, 500.0);
        assert_eq!(cells[0].center_x + cells[1].center_x, 1000.0);
    }
}
