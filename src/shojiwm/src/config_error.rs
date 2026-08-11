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
}

#[derive(Debug, Clone, Copy)]
pub enum ConfigErrorKind {
    InitialLoad,
    HotReload,
    Runtime,
}

impl ConfigErrorReport {
    pub fn initial_load(error: impl ToString) -> Self {
        Self {
            kind: ConfigErrorKind::InitialLoad,
            message: error.to_string(),
        }
    }

    pub fn hot_reload(error: impl ToString) -> Self {
        Self {
            kind: ConfigErrorKind::HotReload,
            message: error.to_string(),
        }
    }

    pub fn runtime(error: impl ToString) -> Self {
        Self {
            kind: ConfigErrorKind::Runtime,
            message: error.to_string(),
        }
    }
}

pub fn background_elements_for_output(
    report: Option<&ConfigErrorReport>,
    output_geo: Rectangle<i32, Logical>,
    scale: Scale<f64>,
) -> Vec<SolidColorRenderElement> {
    let Some(_) = report else {
        return Vec::new();
    };

    let margin = 24;
    let width = (output_geo.size.w - margin * 2).clamp(1, 1100);
    let height = (output_geo.size.h / 3).clamp(160, 420);
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
        ConfigErrorKind::Runtime => "ShojiWM config runtime error",
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
        18,
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
            14,
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
    font_size: i32,
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
        line_height: Some(rect.height),
        raster_scale: scale.x.ceil().max(1.0) as i32,
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
