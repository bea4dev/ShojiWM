use std::time::{Duration, Instant};

use smithay::{
    backend::renderer::{
        element::{Id, Kind, solid::SolidColorRenderElement},
        gles::{GlesError, GlesRenderer},
        utils::CommitCounter,
    },
    utils::{Logical, Point, Rectangle, Scale},
};

use crate::{
    backend::text::{self, DecorationTextureElements, LabelSpec, TextRasterizer},
    ssd::{Color, LogicalRect},
};

#[derive(Debug, Clone)]
pub struct ConfigErrorReport {
    pub kind: ConfigErrorKind,
    pub message: String,
    /// When set, the compositor drops the report at this point. Errors stay until fixed;
    /// notices that need no action (hot reload not available) go away on their own.
    pub expires_at: Option<Instant>,
}

#[derive(Debug, Clone, Copy)]
pub enum ConfigErrorKind {
    InitialLoad,
    HotReload,
    /// The runtime cannot hot reload (e.g. a compiled Rust config). Shown briefly.
    HotReloadUnavailable,
    Runtime,
    /// Evaluating the config for a frame failed (window decorations, layer or popup effects),
    /// and the previous frame's result was kept. Owned by the render path like `Effect`: it
    /// comes down once every failed evaluation has succeeded again, because such a failure can
    /// be transient (a window that closes during the evaluation) and nothing else would take
    /// the overlay down before a config reload.
    Frame,
    /// A shader or effect pipeline from the config failed. Unlike the others this one is owned
    /// by the render path, which replaces or clears it as the set of failures changes.
    Effect,
}

/// The per-frame config evaluations (window decorations, layer or popup effects) that failed
/// for an output and have not succeeded since. The `Frame` report stays up while one remains.
#[derive(Debug, Default)]
pub struct FailedFrameEvaluations(std::collections::BTreeSet<(String, &'static str)>);

impl FailedFrameEvaluations {
    pub fn failed(&mut self, output: &str, stage: &'static str) {
        self.0.insert((output.to_string(), stage));
    }

    /// Note a success. `true` when it ends the last failure, so the `Frame` report can come
    /// down.
    pub fn succeeded(&mut self, output: &str, stage: &'static str) -> bool {
        self.0.remove(&(output.to_string(), stage)) && self.0.is_empty()
    }

    pub fn clear(&mut self) {
        self.0.clear();
    }
}

impl ConfigErrorReport {
    pub fn initial_load(error: impl ToString) -> Self {
        Self {
            kind: ConfigErrorKind::InitialLoad,
            message: error.to_string(),
            expires_at: None,
        }
    }

    pub fn hot_reload(error: impl ToString) -> Self {
        Self {
            kind: ConfigErrorKind::HotReload,
            message: error.to_string(),
            expires_at: None,
        }
    }

    pub fn hot_reload_unavailable(message: impl ToString, lifetime: Duration) -> Self {
        Self {
            kind: ConfigErrorKind::HotReloadUnavailable,
            message: message.to_string(),
            expires_at: Some(Instant::now() + lifetime),
        }
    }

    pub fn is_expired(&self, now: Instant) -> bool {
        self.expires_at.is_some_and(|expires_at| expires_at <= now)
    }

    pub fn effect(error: impl ToString) -> Self {
        Self {
            kind: ConfigErrorKind::Effect,
            message: error.to_string(),
            expires_at: None,
        }
    }

    pub fn frame(error: impl ToString) -> Self {
        Self {
            kind: ConfigErrorKind::Frame,
            message: error.to_string(),
            expires_at: None,
        }
    }

    pub fn runtime(error: impl ToString) -> Self {
        Self {
            kind: ConfigErrorKind::Runtime,
            message: error.to_string(),
            expires_at: None,
        }
    }
}

pub fn background_elements_for_output(
    report: Option<&ConfigErrorReport>,
    output_geo: Rectangle<i32, Logical>,
    scale: Scale<f64>,
) -> Vec<SolidColorRenderElement> {
    let Some(report) = report else {
        return Vec::new();
    };

    let margin = 24;
    let width = (output_geo.size.w - margin * 2).clamp(1, 1100);
    let height = match report.kind {
        // Title and a single line of text.
        ConfigErrorKind::HotReloadUnavailable => 84,
        _ => (output_geo.size.h / 3).clamp(160, 420),
    };
    let logical = Rectangle::new(Point::from((margin, margin)), (width, height).into());
    let physical = logical.to_physical_precise_round(scale);

    vec![SolidColorRenderElement::new(
        Id::new(),
        physical,
        CommitCounter::default(),
        [0.08, 0.02, 0.025, 0.92],
        Kind::Unspecified,
    )]
}

pub fn text_elements_for_output(
    renderer: &mut GlesRenderer,
    rasterizer: &mut TextRasterizer,
    report: Option<&ConfigErrorReport>,
    output_geo: Rectangle<i32, Logical>,
    scale: Scale<f64>,
) -> Result<Vec<DecorationTextureElements>, GlesError> {
    let Some(report) = report else {
        return Ok(Vec::new());
    };

    let margin = 24;
    let panel_width = (output_geo.size.w - margin * 2).clamp(1, 1100);
    let text_width = (panel_width - 32).max(1);
    let title = match report.kind {
        ConfigErrorKind::InitialLoad => "ShojiWM config initial load failed",
        ConfigErrorKind::HotReload => "ShojiWM config hot reload failed",
        ConfigErrorKind::HotReloadUnavailable => "ShojiWM config hot reload is not available",
        ConfigErrorKind::Runtime | ConfigErrorKind::Frame => "ShojiWM config runtime error",
        ConfigErrorKind::Effect => "ShojiWM effect error (the effect is disabled)",
    };
    let body = truncate_error_message(&report.message, 3200);
    let lines = wrap_lines(&body, 120);

    let mut out = Vec::new();
    emit_label(
        renderer,
        rasterizer,
        title,
        LogicalRect::new(
            output_geo.loc.x + margin + 16,
            output_geo.loc.y + margin + 14,
            text_width,
            30,
        ),
        18.0,
        Color::rgba(255, 210, 220, 255),
        output_geo,
        scale,
        &mut out,
    )?;

    let mut y = margin + 52;
    for line in lines.into_iter().take(18) {
        emit_label(
            renderer,
            rasterizer,
            &line,
            LogicalRect::new(
                output_geo.loc.x + margin + 16,
                output_geo.loc.y + y,
                text_width,
                22,
            ),
            14.0,
            Color::rgba(255, 238, 242, 255),
            output_geo,
            scale,
            &mut out,
        )?;
        y += 22;
    }

    Ok(out)
}

fn emit_label(
    renderer: &mut GlesRenderer,
    rasterizer: &mut TextRasterizer,
    text: &str,
    rect: LogicalRect,
    font_size: f32,
    color: Color,
    output_geo: Rectangle<i32, Logical>,
    scale: Scale<f64>,
    out: &mut Vec<DecorationTextureElements>,
) -> Result<(), GlesError> {
    let spec = LabelSpec {
        rect,
        rect_precise: None,
        text: text.to_string(),
        color,
        font_size,
        font_weight: None,
        font_family: None,
        text_align: None,
        text_overflow: None,
        line_height: Some(rect.height as f32),
        text_shadow: None,
        raster_scale: scale.x.abs().max(1.0),
    };
    let Some(label) = rasterizer.render_label(&spec) else {
        return Ok(());
    };
    if let Some(element) = text::memory_text_element(
        renderer,
        &label,
        output_rect_as_root(output_geo),
        crate::backend::visual::RootSubpixelEdges::default(),
        output_geo,
        scale,
        1.0,
    )? {
        out.push(element);
    }
    Ok(())
}

fn output_rect_as_root(output_geo: Rectangle<i32, Logical>) -> LogicalRect {
    LogicalRect::new(
        output_geo.loc.x,
        output_geo.loc.y,
        output_geo.size.w,
        output_geo.size.h,
    )
}

fn truncate_error_message(message: &str, max_chars: usize) -> String {
    let mut out = String::new();
    for ch in message.chars().take(max_chars) {
        out.push(ch);
    }
    if message.chars().count() > max_chars {
        out.push_str("\n...");
    }
    out
}

fn wrap_lines(message: &str, max_chars: usize) -> Vec<String> {
    let mut lines = Vec::new();
    for raw in message.lines() {
        let mut current = String::new();
        for word in raw.split_whitespace() {
            if current.is_empty() {
                current.push_str(word);
            } else if current.chars().count() + 1 + word.chars().count() <= max_chars {
                current.push(' ');
                current.push_str(word);
            } else {
                lines.push(current);
                current = word.to_string();
            }
        }
        if current.is_empty() {
            lines.push(String::new());
        } else {
            lines.push(current);
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_frame_report_ends_with_the_last_failure() {
        let mut failures = FailedFrameEvaluations::default();
        failures.failed("DP-2", "window decorations");
        failures.failed("HDMI-A-1", "window decorations");
        // A stage that never failed ends nothing.
        assert!(!failures.succeeded("DP-2", "layer effects"));
        // One output recovers; the other still fails.
        assert!(!failures.succeeded("DP-2", "window decorations"));
        assert!(failures.succeeded("HDMI-A-1", "window decorations"));
        // Later successes do not report the end again.
        assert!(!failures.succeeded("HDMI-A-1", "window decorations"));
    }

    #[test]
    fn only_the_hot_reload_unavailable_notice_expires() {
        let now = Instant::now();
        let notice = ConfigErrorReport::hot_reload_unavailable("x", Duration::from_secs(3));
        assert!(!notice.is_expired(now));
        assert!(notice.is_expired(now + Duration::from_secs(4)));

        let error = ConfigErrorReport::hot_reload("x");
        assert!(!error.is_expired(now + Duration::from_secs(3600)));
    }
}
