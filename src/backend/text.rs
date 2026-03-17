use std::{collections::HashMap, sync::Arc};

use fontdb::{Database, Family, Query, Source, Stretch, Style, Weight};
use fontdue::{Font, FontSettings, layout::{CoordinateSystem, Layout, LayoutSettings, TextStyle}};
use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            element::{
                Kind,
                memory::{MemoryRenderBuffer, MemoryRenderBufferRenderElement},
            },
            gles::{GlesError, GlesRenderer},
        },
    },
    desktop::{Space, Window},
    output::Output,
    utils::{Logical, Physical, Point, Rectangle, Scale as OutputScale},
};
use tracing::debug;

use crate::ssd::{Color, LogicalRect, WindowDecorationState};

#[derive(Debug, Clone)]
pub struct CachedDecorationLabel {
    pub rect: LogicalRect,
    pub text: String,
    pub color: Color,
    pub buffer: MemoryRenderBuffer,
}

#[derive(Debug, Clone)]
pub struct LabelSpec {
    pub rect: LogicalRect,
    pub text: String,
    pub color: Color,
    pub font_size: i32,
    pub font_weight: Option<serde_json::Value>,
    pub font_family: Option<Vec<String>>,
    pub text_align: Option<String>,
    pub line_height: Option<i32>,
}

#[derive(Debug, Default)]
pub struct TextRasterizer {
    database: Database,
    fonts: HashMap<FontCacheKey, Arc<Font>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct FontCacheKey {
    family: Option<String>,
    weight: u16,
}

impl TextRasterizer {
    pub fn new() -> Self {
        let mut database = Database::new();
        database.load_system_fonts();
        Self {
            database,
            fonts: HashMap::new(),
        }
    }

    pub fn render_label(&mut self, spec: &LabelSpec) -> Option<CachedDecorationLabel> {
        if spec.text.is_empty() || spec.rect.width <= 0 || spec.rect.height <= 0 {
            return None;
        }

        let weight = parse_font_weight(spec.font_weight.as_ref());
        let font = self.resolve_font(spec.font_family.as_deref(), weight)?;
        let font_size = spec.font_size.max(1) as f32;
        let line_height = spec.line_height.unwrap_or((font_size.ceil() as i32) + 4);

        let mut measure_layout = Layout::new(CoordinateSystem::PositiveYDown);
        measure_layout.reset(&LayoutSettings {
            max_width: None,
            max_height: None,
            ..LayoutSettings::default()
        });
        measure_layout.append(&[font.as_ref()], &TextStyle::new(&spec.text, font_size, 0));
        let glyphs = measure_layout.glyphs();
        let text_width = glyphs
            .iter()
            .map(|glyph| glyph.x + glyph.width as f32)
            .fold(0.0, f32::max);
        let text_height = glyphs
            .iter()
            .map(|glyph| glyph.y + glyph.height as f32)
            .fold(0.0, f32::max);

        let x_offset = match spec.text_align.as_deref() {
            Some("center") => ((spec.rect.width as f32 - text_width) * 0.5).max(0.0),
            Some("end") => (spec.rect.width as f32 - text_width).max(0.0),
            _ => 0.0,
        };
        let y_offset =
            ((spec.rect.height as f32 - line_height as f32) * 0.5).max(0.0)
                + ((line_height as f32 - text_height) * 0.5).max(0.0);

        let mut layout = Layout::new(CoordinateSystem::PositiveYDown);
        layout.reset(&LayoutSettings {
            x: x_offset,
            y: y_offset,
            max_width: Some(spec.rect.width as f32),
            max_height: Some(spec.rect.height as f32),
            ..LayoutSettings::default()
        });
        layout.append(&[font.as_ref()], &TextStyle::new(&spec.text, font_size, 0));

        let mut pixels = vec![0u8; (spec.rect.width * spec.rect.height * 4) as usize];
        for glyph in layout.glyphs() {
            let (metrics, bitmap) = font.rasterize_config(glyph.key);
            if metrics.width == 0 || metrics.height == 0 {
                continue;
            }

            for y in 0..metrics.height {
                for x in 0..metrics.width {
                    let coverage = bitmap[y * metrics.width + x];
                    if coverage == 0 {
                        continue;
                    }
                    let target_x = glyph.x as i32 + x as i32;
                    let target_y = glyph.y as i32 + y as i32;
                    if target_x < 0
                        || target_y < 0
                        || target_x >= spec.rect.width
                        || target_y >= spec.rect.height
                    {
                        continue;
                    }

                    let index = ((target_y * spec.rect.width + target_x) * 4) as usize;
                    let alpha = ((coverage as f32 / 255.0) * spec.color.a as f32).round() as u8;
                    let premultiplied = alpha as u16;
                    let blue = ((u16::from(spec.color.b) * premultiplied) / 255) as u8;
                    let green = ((u16::from(spec.color.g) * premultiplied) / 255) as u8;
                    let red = ((u16::from(spec.color.r) * premultiplied) / 255) as u8;
                    pixels[index] = blue;
                    pixels[index + 1] = green;
                    pixels[index + 2] = red;
                    pixels[index + 3] = alpha.max(pixels[index + 3]);
                }
            }
        }

        Some(CachedDecorationLabel {
            rect: spec.rect,
            text: spec.text.clone(),
            color: spec.color,
            buffer: MemoryRenderBuffer::from_slice(
                &pixels,
                Fourcc::Argb8888,
                (spec.rect.width, spec.rect.height),
                1,
                smithay::utils::Transform::Normal,
                None,
            ),
        })
    }

    fn resolve_font(&mut self, family: Option<&[String]>, weight: u16) -> Option<Arc<Font>> {
        let key = FontCacheKey {
            family: family.and_then(|families| families.first().cloned()),
            weight,
        };
        if let Some(font) = self.fonts.get(&key) {
            return Some(font.clone());
        }

        let face = if let Some(names) = family {
            let mut families = names
                .iter()
                .map(|name| Family::Name(name.as_str()))
                .collect::<Vec<_>>();
            families.extend([Family::SansSerif, Family::Serif, Family::Monospace]);
            let query = Query {
                families: &families,
                weight: Weight(weight),
                stretch: Stretch::Normal,
                style: Style::Normal,
            };
            self.database
                .query(&query)
                .or_else(|| self.pick_matching_face(Some(names), weight))
        } else {
            self.pick_default_face(weight)
        }?;
        let font = self.load_font_for_face(face)?;
        let font = Arc::new(font);
        self.fonts.insert(key, font.clone());
        Some(font)
    }

    fn pick_matching_face(&self, family: Option<&[String]>, weight: u16) -> Option<fontdb::ID> {
        let requested = family.map(|values| {
            values
                .iter()
                .map(|value| value.to_ascii_lowercase())
                .collect::<Vec<_>>()
        });
        let mut best: Option<(fontdb::ID, i32)> = None;

        for face in self.database.faces() {
            let family_score = if let Some(requested) = &requested {
                if face
                    .families
                    .iter()
                    .any(|(name, _)| requested.iter().any(|value| name.eq_ignore_ascii_case(value)))
                {
                    0
                } else {
                    1_000
                }
            } else {
                0
            };

            let style_penalty = if face.style == Style::Normal { 0 } else { 100 };
            let stretch_penalty = if face.stretch == Stretch::Normal { 0 } else { 25 };
            let mono_penalty = if family.is_none() && face.monospaced { 50 } else { 0 };
            let source_penalty = face_source_penalty(&face.source);
            let weight_penalty = (i32::from(face.weight.0) - i32::from(weight)).abs();
            let score =
                family_score + style_penalty + stretch_penalty + mono_penalty + source_penalty + weight_penalty;

            match best {
                Some((_, current_score)) if current_score <= score => {}
                _ => best = Some((face.id, score)),
            }
        }

        if best.is_none() {
            debug!(requested_family = ?family, weight, "no fallback font face available");
        }

        best.map(|(id, _)| id)
    }

    fn pick_default_face(&self, weight: u16) -> Option<fontdb::ID> {
        let sans_family = self
            .database
            .family_name(&Family::SansSerif)
            .to_ascii_lowercase();
        let mut best: Option<(fontdb::ID, i32)> = None;

        for face in self.database.faces() {
            let names = face
                .families
                .iter()
                .map(|(name, _)| name.to_ascii_lowercase())
                .collect::<Vec<_>>();

            let exact_sans = names.iter().any(|name| name == &sans_family);
            let contains_sans = names.iter().any(|name| name.contains("sans"));
            let contains_serif = names.iter().any(|name| name.contains("serif"));
            let contains_emoji = names.iter().any(|name| name.contains("emoji"));
            let contains_cjk = names.iter().any(|name| name.contains("cjk"));

            let family_score = if exact_sans {
                0
            } else if contains_sans {
                25
            } else if contains_serif {
                140
            } else {
                80
            };
            let style_penalty = if face.style == Style::Normal { 0 } else { 100 };
            let stretch_penalty = if face.stretch == Stretch::Normal { 0 } else { 25 };
            let mono_penalty = if face.monospaced { 120 } else { 0 };
            let emoji_penalty = if contains_emoji { 600 } else { 0 };
            let cjk_penalty = if contains_cjk { 220 } else { 0 };
            let source_penalty = face_source_penalty(&face.source);
            let weight_penalty = (i32::from(face.weight.0) - i32::from(weight)).abs();
            let score = family_score
                + style_penalty
                + stretch_penalty
                + mono_penalty
                + emoji_penalty
                + cjk_penalty
                + source_penalty
                + weight_penalty;

            match best {
                Some((_, current_score)) if current_score <= score => {}
                _ => best = Some((face.id, score)),
            }
        }

        if best.is_none() {
            debug!(weight, "no default font face available");
        }

        best.map(|(id, _)| id)
    }

    fn load_font_for_face(&self, face: fontdb::ID) -> Option<Font> {
        let (source, face_index) = self.database.face_source(face)?;
        match source {
            Source::Binary(data) => Font::from_bytes(
                data.as_ref().as_ref().to_vec(),
                FontSettings {
                    collection_index: face_index,
                    ..FontSettings::default()
                },
            )
            .ok(),
            Source::File(path) => std::fs::read(&path)
                .ok()
                .and_then(|bytes| {
                    Font::from_bytes(
                        bytes,
                        FontSettings {
                            collection_index: face_index,
                            ..FontSettings::default()
                        },
                    )
                    .ok()
                }),
            Source::SharedFile(_, data) => Font::from_bytes(
                data.as_ref().as_ref().to_vec(),
                FontSettings {
                    collection_index: face_index,
                    ..FontSettings::default()
                },
            )
            .ok(),
        }
    }
}

fn face_source_penalty(source: &Source) -> i32 {
    match source {
        Source::Binary(_) => 0,
        Source::File(path) | Source::SharedFile(path, _) => {
            let extension = path
                .extension()
                .and_then(|ext| ext.to_str())
                .unwrap_or_default()
                .to_ascii_lowercase();
            match extension.as_str() {
                "ttc" | "otc" => 180,
                _ => 0,
            }
        }
    }
}

pub fn text_elements_for_window(
    renderer: &mut GlesRenderer,
    space: &Space<Window>,
    decorations: &HashMap<Window, WindowDecorationState>,
    output: &Output,
    window: &Window,
) -> Result<Vec<MemoryRenderBufferRenderElement<GlesRenderer>>, GlesError> {
    let Some(output_geo) = space.output_geometry(output) else {
        return Ok(Vec::new());
    };
    let scale = OutputScale::from(output.current_scale().fractional_scale());
    let Some(decoration) = decorations.get(window) else {
        return Ok(Vec::new());
    };

    decoration
        .text_buffers
        .iter()
        .filter_map(|label| memory_text_element(renderer, label, output_geo, scale).transpose())
        .collect()
}

fn memory_text_element(
    renderer: &mut GlesRenderer,
    label: &CachedDecorationLabel,
    output_geo: Rectangle<i32, Logical>,
    scale: OutputScale<f64>,
) -> Result<Option<MemoryRenderBufferRenderElement<GlesRenderer>>, GlesError> {
    if intersect_logical_rect(label.rect, output_geo).is_none() {
        return Ok(None);
    }

    let local = Point::from((label.rect.x - output_geo.loc.x, label.rect.y - output_geo.loc.y));
    let physical: Point<i32, Physical> = local.to_f64().to_physical_precise_round(scale);
    let element = MemoryRenderBufferRenderElement::from_buffer(
        renderer,
        physical.to_f64(),
        &label.buffer,
        None,
        None,
        None,
        Kind::Unspecified,
    )?;
    Ok(Some(element))
}

fn parse_font_weight(value: Option<&serde_json::Value>) -> u16 {
    match value {
        Some(serde_json::Value::Number(number)) => number.as_u64().unwrap_or(400) as u16,
        Some(serde_json::Value::String(weight)) => match weight.as_str() {
            "normal" => 400,
            "medium" => 500,
            "semibold" => 600,
            "bold" => 700,
            _ => 400,
        },
        _ => 400,
    }
}

fn intersect_logical_rect(
    rect: LogicalRect,
    output_geo: Rectangle<i32, Logical>,
) -> Option<LogicalRect> {
    let left = rect.x.max(output_geo.loc.x);
    let top = rect.y.max(output_geo.loc.y);
    let right = (rect.x + rect.width).min(output_geo.loc.x + output_geo.size.w);
    let bottom = (rect.y + rect.height).min(output_geo.loc.y + output_geo.size.h);

    (right > left && bottom > top)
        .then(|| LogicalRect::new(left, top, right - left, bottom - top))
}
