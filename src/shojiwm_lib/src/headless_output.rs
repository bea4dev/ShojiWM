//! Outputs with no connector behind them, made and removed by the config
//! (`COMPOSITOR.output.create` / `remove`).
//!
//! Once made, a headless output is an ordinary output: it has wl_output and
//! xdg-output globals, layer surfaces and windows can go on it, it is in the
//! config's output list and the display configuration places it, the pointer
//! crosses onto it, and screen capture records it. Nothing scans it out. The
//! TTY backend runs it in the same render loop as its real outputs (see
//! `backend::tty::headless`); a nested session has no headless outputs.

use smithay::output::{Mode, Output, PhysicalProperties, Scale, Subpixel};
use tracing::{info, warn};

use crate::state::ShojiWM;

/// Refresh rate of a headless output, in mHz.
const REFRESH: i32 = 60_000;
/// Largest width or height of a headless output, in pixels.
const MAX_SIZE: i32 = 16_384;

/// A headless output change requested by the config runtime.
#[derive(Debug, Clone, PartialEq)]
pub enum RuntimeHeadlessOutputRequest {
    Create {
        name: String,
        width: i32,
        height: i32,
        scale: Option<f64>,
    },
    Remove {
        name: String,
    },
}

impl ShojiWM {
    pub fn apply_runtime_headless_output_request(&mut self, request: RuntimeHeadlessOutputRequest) {
        let result = match &request {
            RuntimeHeadlessOutputRequest::Create {
                name,
                width,
                height,
                scale,
            } => self.create_headless_output(name, *width, *height, *scale),
            RuntimeHeadlessOutputRequest::Remove { name } => self.remove_headless_output(name),
        };
        if let Err(error) = result {
            warn!(?request, %error, "headless output request failed");
        }
    }

    /// Whether `name` is a headless output, and where it ranks among them:
    /// they are placed after the connected outputs, in the order made.
    pub fn headless_output_index(&self, name: &str) -> Option<usize> {
        self.headless_outputs
            .iter()
            .position(|output| output.name() == name)
    }

    pub fn create_headless_output(
        &mut self,
        name: &str,
        width: i32,
        height: i32,
        scale: Option<f64>,
    ) -> Result<(), String> {
        if self.winit_backend.is_some() {
            return Err("a nested session has no headless outputs".into());
        }
        if name.is_empty() {
            return Err("a headless output needs a name".into());
        }
        if !(1..=MAX_SIZE).contains(&width) || !(1..=MAX_SIZE).contains(&height) {
            return Err(format!(
                "a headless output is 1 to {MAX_SIZE} pixels wide and high, not {width}x{height}"
            ));
        }
        if let Some(scale) = scale
            && !(scale.is_finite() && scale > 0.0)
        {
            return Err(format!("{scale} is not an output scale"));
        }
        if self
            .runtime_connected_outputs()
            .iter()
            .any(|output| output.name() == name)
        {
            return Err(format!("an output named {name} exists already"));
        }

        let output = Output::new(
            name.to_string(),
            PhysicalProperties {
                size: (0, 0).into(),
                subpixel: Subpixel::Unknown,
                make: "ShojiWM".into(),
                model: "Headless".into(),
                serial_number: String::new(),
            },
        );
        let mode = Mode {
            size: (width, height).into(),
            refresh: REFRESH,
        };
        output.set_preferred(mode);
        output.change_current_state(
            Some(mode),
            None,
            scale.map(Scale::Fractional),
            Some((0, 0).into()),
        );
        self.seed_xwayland_refresh_override_from_output(&output, "headless-output-created");
        self.create_output_global(&output);
        self.space.map_output(&output, (0, 0));
        self.headless_outputs.push(output.clone());
        if let Err(error) = crate::backend::tty::attach_headless_output(self, &output) {
            self.headless_outputs.pop();
            crate::backend::tty::forget_output(self, &output);
            return Err(error.to_string());
        }
        info!(
            output = name,
            width,
            height,
            ?scale,
            "created headless output"
        );
        self.apply_runtime_display_configuration();
        self.notify_runtime_outputs_changed();
        self.schedule_redraw();
        Ok(())
    }

    pub fn remove_headless_output(&mut self, name: &str) -> Result<(), String> {
        let Some(index) = self.headless_output_index(name) else {
            return Err(format!("{name} is not a headless output"));
        };
        let output = self.headless_outputs.remove(index);
        crate::backend::tty::detach_headless_output(self, &output);
        crate::backend::tty::forget_output(self, &output);
        info!(output = name, "removed headless output");
        Ok(())
    }
}
