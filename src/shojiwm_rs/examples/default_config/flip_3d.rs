//! Port of `packages/config/src/flip-3d.tsx`: a Windows Vista style window
//! switcher (a `SwitcherLayout`, see `window_switcher.rs` for what every
//! switcher shares).
//!
//! `Super+Tab` lifts every window into a receding diagonal stack, most
//! recently used in front. Hold `Super` and press `Tab` to flip through it,
//! release `Super` to switch to the front window. While it is open:
//!
//! - `Tab` / `Shift+Tab`, arrow keys, the mouse wheel, touchpad scrolling and
//!   three-finger swipes flip the stack
//! - `Return` / `Space`, a click on a window, or releasing `Super` switches
//! - `Escape` returns to the desktop unchanged

use shojiwm_rs::{
    prelude::*,
    ssd::{GestureSwipeEventSnapshot, GestureSwipePhaseSnapshot},
};

use crate::{
    window_manager::WindowManager,
    window_switcher::{
        LayoutSession, Pose, SwitcherAction, SwitcherLayout, Viewport, WindowSwitcher, camera_distance, clamp01,
    },
};

/// How quickly flipping follows the selection (time constant, ms).
const SCROLL_TIME_CONSTANT_MS: f64 = 70.0;
/// Windows shown in the stack at once.
const VISIBLE_WINDOWS: f64 = 7.0;
/// How far the desktop dims behind the stack.
const DIM_ALPHA: f64 = 0.45;
// The stack, after Windows Vista: the front window low on the right, the rest
// receding to the upper left, every window turned about 40° and seen slightly
// from above through a long lens (almost no perspective within a window).
/// Each window's turn about the vertical axis (degrees; the right edge recedes).
const TURN_DEGREES: f64 = 20.0;
/// Each window's tilt about the horizontal axis (degrees; far edges rise).
const PITCH_DEGREES: f64 = 6.0;
/// Where the front window's centre lands, as fractions of the output from its centre (+y up).
const FRONT_X: f64 = 0.13;
const FRONT_Y: f64 = -0.05;
/// Each window further back moves on screen by this much (fractions of the output).
const STEP_X: f64 = -0.085;
const STEP_Y: f64 = 0.048;
/// Each window further back is drawn this much smaller: scale 1 / (1 + slot × this).
const SHRINK_PER_SLOT: f64 = 0.3;
/// The front window's largest size, as fractions of the output.
const FRONT_MAX_WIDTH: f64 = 0.5;
const FRONT_MAX_HEIGHT: f64 = 0.5;
/// Touchpad scrolling / swiping distance per window (logical px).
const SCROLL_STEP_PX: f64 = 80.0;
const SWIPE_STEP_PX: f64 = 140.0;

pub fn create_flip_3d(wm: WindowManager, stack_order: impl Fn(Window) -> i32 + 'static) -> WindowSwitcher {
    WindowSwitcher::new(Flip3D, wm, stack_order)
}

struct Flip3D;

impl SwitcherLayout for Flip3D {
    fn name(&self) -> &'static str {
        "flip-3d"
    }

    fn dim_alpha(&self) -> f64 {
        DIM_ALPHA
    }

    // Like Alt+Tab, the previous window comes to the front: the stack flips
    // once as it opens.
    fn initial_selection(&self, count: usize) -> i64 {
        if count > 1 { 1 } else { 0 }
    }

    fn begin(&self, windows: &[Window]) -> Box<dyn LayoutSession> {
        Box::new(FlipSession {
            count: windows.len(),
            scroll: 0.0,
            scroll_remainder: 0.0,
            swipe_applied_steps: 0,
        })
    }
}

struct FlipSession {
    count: usize,
    /// Where the stack is, easing towards the selection.
    scroll: f64,
    scroll_remainder: f64,
    swipe_applied_steps: i64,
}

fn flip(selected: i64, steps: i64) -> SwitcherAction {
    if steps == 0 {
        SwitcherAction::None
    } else {
        SwitcherAction::Select(selected + steps)
    }
}

impl LayoutSession for FlipSession {
    fn pose(&self, index: usize, rect: Rect, viewport: &Viewport, _selected: i64) -> Pose {
        slot_pose(
            slot_of(index, self.scroll, self.count),
            self.count,
            rect,
            viewport.width,
            viewport.height,
        )
    }

    fn tick(&mut self, selected: i64, dt_ms: f64) -> bool {
        let distance = selected as f64 - self.scroll;
        if distance.abs() < 0.001 {
            self.scroll = selected as f64;
        } else {
            self.scroll += distance * (1.0 - (-dt_ms / SCROLL_TIME_CONSTANT_MS).exp());
        }
        self.scroll != selected as f64
    }

    fn settle(&mut self, selected: i64) {
        self.scroll = selected as f64;
    }

    fn on_key(&mut self, event: &InputGrabKeyEvent, selected: i64) -> Option<SwitcherAction> {
        match event.key.as_str() {
            "Tab" | "ISO_Left_Tab" => Some(flip(selected, if event.modifiers.shift { -1 } else { 1 })),
            "Right" | "Down" => Some(flip(selected, 1)),
            "Left" | "Up" => Some(flip(selected, -1)),
            _ => None,
        }
    }

    fn on_super_released(&mut self, _selected: i64) -> SwitcherAction {
        SwitcherAction::Commit
    }

    fn on_scroll(&mut self, event: &InputGrabScrollEvent, selected: i64) -> SwitcherAction {
        if event.source == "wheel" {
            let clicks = event.discrete_y.unwrap_or(event.delta_y.signum() * 120.0) / 120.0;
            return flip(selected, clicks.signum() as i64 * clicks.abs().round().max(1.0) as i64);
        }
        self.scroll_remainder += event.delta_y;
        let steps = (self.scroll_remainder / SCROLL_STEP_PX).trunc();
        self.scroll_remainder -= steps * SCROLL_STEP_PX;
        flip(selected, steps as i64)
    }

    fn on_swipe(&mut self, event: &GestureSwipeEventSnapshot, selected: i64) -> SwitcherAction {
        if event.phase == GestureSwipePhaseSnapshot::Begin {
            self.swipe_applied_steps = 0;
            return SwitcherAction::None;
        }
        let travel = if event.total_x.abs() > event.total_y.abs() {
            -event.total_x
        } else {
            event.total_y
        };
        let steps = (travel / SWIPE_STEP_PX).trunc() as i64;
        let applied = self.swipe_applied_steps;
        self.swipe_applied_steps = steps;
        flip(selected, steps - applied)
    }
}

/// The window's place in the stack. `slot` 0 is the front; the stack recedes
/// up and to the left. Slots below 0 have flipped past the front and fade
/// out; the last visible slot fades in.
fn slot_pose(slot: f64, count: usize, rect: Rect, width: f64, height: f64) -> Pose {
    let fit = 1.0_f64
        .min(width * FRONT_MAX_WIDTH / rect.width)
        .min(height * FRONT_MAX_HEIGHT / rect.height);
    // Depth that draws the slot 1 / (1 + slot × SHRINK_PER_SLOT) as large,
    // and the world position that puts its centre on its spot on screen.
    let depth = slot * SHRINK_PER_SLOT * camera_distance(height);
    let depth_scale = 1.0 + slot * SHRINK_PER_SLOT;
    let last = (count as f64).min(VISIBLE_WINDOWS);
    let opacity = if slot < 0.0 {
        clamp01(1.0 + slot * 2.0)
    } else if slot > last - 1.0 {
        clamp01((last - 0.5 - slot) * 2.0)
    } else {
        1.0
    };
    Pose {
        x: (FRONT_X + slot * STEP_X) * width * depth_scale,
        y: (FRONT_Y + slot * STEP_Y) * height * depth_scale,
        z: -depth,
        rotate_x: PITCH_DEGREES,
        rotate_y: TURN_DEGREES,
        scale: fit,
        opacity,
    }
}

fn slot_of(index: usize, scroll: f64, count: usize) -> f64 {
    (index as f64 - scroll + 0.5).rem_euclid(count as f64) - 0.5
}
