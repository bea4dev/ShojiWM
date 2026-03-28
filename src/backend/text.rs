use std::{
    cell::RefCell,
    collections::{HashMap, HashSet, hash_map::DefaultHasher},
    hash::{Hash, Hasher},
    time::{Duration, Instant},
};

use cosmic_text::{
    Align, Attrs, Buffer, Color as CosmicColor, Family as CosmicFamily, FontSystem, Metrics,
    Shaping, Style as CosmicStyle, SwashCache, Weight as CosmicWeight, Wrap,
};
use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            element::{
                memory::{MemoryRenderBuffer, MemoryRenderBufferRenderElement},
                Kind,
            },
            gles::{GlesError, GlesRenderer},
        },
    },
    desktop::{Space, Window},
    output::Output,
    utils::{Logical, Physical, Point, Rectangle, Scale as OutputScale},
};
use crate::{
    backend::async_assets::{AsyncAssetJob, AsyncAssetJobSender},
    ssd::{Color, LogicalRect, WindowDecorationState},
};

thread_local! {
    static LABEL_MEASURER: RefCell<LabelMeasurer> = RefCell::new(LabelMeasurer::new());
}

#[derive(Debug, Clone)]
pub struct CachedDecorationLabel {
    pub owner_node_id: Option<String>,
    pub order: usize,
    pub rect: LogicalRect,
    pub clip_rect: Option<LogicalRect>,
    pub clip_radius: i32,
    pub text: String,
    pub color: Color,
    pub buffer: MemoryRenderBuffer,
}

#[derive(Debug, Clone)]
pub struct RenderedLabelPixels {
    pub width: i32,
    pub height: i32,
    pub pixels: Vec<u8>,
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
    pub raster_scale: i32,
}

#[derive(Debug)]
pub struct TextRasterizer {
    font_system: FontSystem,
    swash_cache: SwashCache,
    async_job_sender: Option<AsyncAssetJobSender>,
    async_buffers: HashMap<u64, TimedTextBuffer>,
    async_in_flight: HashSet<u64>,
}

#[derive(Debug, Clone)]
struct TimedTextBuffer {
    buffer: MemoryRenderBuffer,
    last_used_at: Instant,
}

#[derive(Debug)]
struct LabelMeasurer {
    font_system: FontSystem,
    cache: HashMap<u64, (i32, i32)>,
}

impl TextRasterizer {
    pub fn new(async_job_sender: Option<AsyncAssetJobSender>) -> Self {
        Self {
            font_system: FontSystem::new(),
            swash_cache: SwashCache::new(),
            async_job_sender,
            async_buffers: HashMap::new(),
            async_in_flight: HashSet::new(),
        }
    }

    pub fn render_label(&mut self, spec: &LabelSpec) -> Option<CachedDecorationLabel> {
        self.prune_async_buffers();
        let spec_hash = hash_label_spec(spec);
        if let Some(cached) = self.async_buffers.get_mut(&spec_hash) {
            cached.last_used_at = Instant::now();
            return Some(CachedDecorationLabel {
                owner_node_id: None,
                order: 0,
                rect: spec.rect,
                clip_rect: None,
                clip_radius: 0,
                text: spec.text.clone(),
                color: spec.color,
                buffer: cached.buffer.clone(),
            });
        }

        if let Some(async_job_sender) = &self.async_job_sender {
            if self.async_in_flight.insert(spec_hash) {
                let _ = async_job_sender.send(AsyncAssetJob::Text {
                    spec_hash,
                    spec: spec.clone(),
                });
            }
            return None;
        }

        let rendered = self.render_label_pixels(spec)?;
        Some(CachedDecorationLabel {
            owner_node_id: None,
            order: 0,
            rect: spec.rect,
            clip_rect: None,
            clip_radius: 0,
            text: spec.text.clone(),
            color: spec.color,
            buffer: MemoryRenderBuffer::from_slice(
                &rendered.pixels,
                Fourcc::Argb8888,
                (rendered.width, rendered.height),
                spec.raster_scale.max(1),
                smithay::utils::Transform::Normal,
                None,
            ),
        })
    }

    pub fn render_label_pixels(&mut self, spec: &LabelSpec) -> Option<RenderedLabelPixels> {
        if spec.text.is_empty() || spec.rect.width <= 0 || spec.rect.height <= 0 {
            return None;
        }

        let raster_scale = spec.raster_scale.max(1);
        let target_width = (spec.rect.width * raster_scale).max(1);
        let target_height = (spec.rect.height * raster_scale).max(1);
        let font_size = spec.font_size.max(1) as f32 * raster_scale as f32;
        let line_height = spec.line_height.unwrap_or(spec.font_size.max(1) + 4) as f32
            * raster_scale as f32;
        let metrics = Metrics::new(font_size, line_height.max(1.0));
        let mut buffer = Buffer::new(&mut self.font_system, metrics);

        let attrs = attrs_for_spec(spec);
        let alignment = alignment_for_spec(spec.text_align.as_deref());

        {
            let mut buffer = buffer.borrow_with(&mut self.font_system);
            buffer.set_size(Some(target_width as f32), Some(target_height as f32));
            buffer.set_wrap(Wrap::None);
            buffer.set_text(&spec.text, &attrs, Shaping::Advanced, alignment);
            buffer.shape_until_scroll(false);
        }

        let runs = buffer.layout_runs().collect::<Vec<_>>();
        let text_height = runs
            .iter()
            .map(|run| run.line_top + run.line_height)
            .fold(0.0f32, f32::max);
        let y_offset = ((target_height as f32 - text_height) * 0.5).max(0.0);

        let mut pixels = vec![0u8; (target_width * target_height * 4) as usize];
        let text_color = CosmicColor::rgba(spec.color.r, spec.color.g, spec.color.b, spec.color.a);
        {
            let mut buffer = buffer.borrow_with(&mut self.font_system);
            buffer.draw(&mut self.swash_cache, text_color, |x, y, w, h, color| {
                for off_y in 0..h as i32 {
                    for off_x in 0..w as i32 {
                        let px = x + off_x;
                        let py = y + off_y + y_offset as i32;
                        if px < 0 || py < 0 || px >= target_width || py >= target_height {
                            continue;
                        }
                        blend_pixel(
                            &mut pixels,
                            target_width,
                            px,
                            py,
                            color.as_rgba_tuple(),
                        );
                    }
                }
            });
        }

        Some(RenderedLabelPixels {
            width: target_width,
            height: target_height,
            pixels,
        })
    }

    pub fn handle_async_ready(
        &mut self,
        spec_hash: u64,
        width: i32,
        height: i32,
        raster_scale: i32,
        pixels: Vec<u8>,
    ) {
        self.async_in_flight.remove(&spec_hash);
        self.async_buffers.insert(
            spec_hash,
            TimedTextBuffer {
                buffer: MemoryRenderBuffer::from_slice(
                    &pixels,
                    Fourcc::Argb8888,
                    (width, height),
                    raster_scale.max(1),
                    smithay::utils::Transform::Normal,
                    None,
                ),
                last_used_at: Instant::now(),
            },
        );
    }

    pub fn handle_async_miss(&mut self, spec_hash: u64) {
        self.async_in_flight.remove(&spec_hash);
    }

    fn prune_async_buffers(&mut self) {
        const TTL: Duration = Duration::from_secs(5);
        let now = Instant::now();
        self.async_buffers
            .retain(|_, entry| now.duration_since(entry.last_used_at) <= TTL);
    }
}

impl LabelMeasurer {
    fn new() -> Self {
        Self {
            font_system: FontSystem::new(),
            cache: HashMap::new(),
        }
    }

    fn measure(&mut self, spec: &LabelSpec) -> (i32, i32) {
        let spec_hash = hash_label_measurement_spec(spec);
        if let Some(cached) = self.cache.get(&spec_hash).copied() {
            return cached;
        }

        let font_size = spec.font_size.max(1) as f32;
        let line_height = spec.line_height.unwrap_or((font_size.ceil() as i32) + 4) as f32;
        let metrics = Metrics::new(font_size, line_height.max(1.0));
        let mut buffer = Buffer::new(&mut self.font_system, metrics);
        let attrs = attrs_for_spec(spec);

        {
            let mut buffer = buffer.borrow_with(&mut self.font_system);
            buffer.set_size(None, None);
            buffer.set_wrap(Wrap::None);
            buffer.set_text(&spec.text, &attrs, Shaping::Advanced, None);
            buffer.shape_until_scroll(false);
        }

        let runs = buffer.layout_runs().collect::<Vec<_>>();
        let width = runs
            .iter()
            .map(|run| run.line_w.ceil() as i32)
            .max()
            .unwrap_or(0)
            .max(1);
        let height = runs
            .iter()
            .map(|run| (run.line_top + run.line_height).ceil() as i32)
            .max()
            .unwrap_or(line_height.ceil() as i32)
            .max(1);

        self.cache.insert(spec_hash, (width, height));
        (width, height)
    }
}

pub fn measure_label_intrinsic(spec: &LabelSpec) -> (i32, i32) {
    LABEL_MEASURER.with(|measurer| measurer.borrow_mut().measure(spec))
}

pub fn hash_label_spec(spec: &LabelSpec) -> u64 {
    let mut hasher = DefaultHasher::new();
    spec.text.hash(&mut hasher);
    spec.rect.width.hash(&mut hasher);
    spec.rect.height.hash(&mut hasher);
    spec.color.r.hash(&mut hasher);
    spec.color.g.hash(&mut hasher);
    spec.color.b.hash(&mut hasher);
    spec.color.a.hash(&mut hasher);
    spec.font_size.hash(&mut hasher);
    spec.text_align.hash(&mut hasher);
    spec.line_height.hash(&mut hasher);
    spec.raster_scale.hash(&mut hasher);
    spec.font_family.hash(&mut hasher);
    spec.font_weight
        .as_ref()
        .and_then(|value| serde_json::to_string(value).ok())
        .hash(&mut hasher);
    hasher.finish()
}

fn hash_label_measurement_spec(spec: &LabelSpec) -> u64 {
    let mut hasher = DefaultHasher::new();
    spec.text.hash(&mut hasher);
    spec.font_size.hash(&mut hasher);
    spec.line_height.hash(&mut hasher);
    spec.font_family.hash(&mut hasher);
    spec.font_weight
        .as_ref()
        .and_then(|value| serde_json::to_string(value).ok())
        .hash(&mut hasher);
    hasher.finish()
}

smithay::render_elements! {
    pub DecorationTextureElements<=GlesRenderer>;
    Memory=MemoryRenderBufferRenderElement<GlesRenderer>,
    Clipped=crate::backend::clipped_memory::ClippedMemoryElement,
}

pub fn text_elements_for_window(
    renderer: &mut GlesRenderer,
    space: &Space<Window>,
    decorations: &HashMap<Window, WindowDecorationState>,
    output: &Output,
    window: &Window,
    alpha: f32,
) -> Result<Vec<DecorationTextureElements>, GlesError> {
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
        .filter_map(|label| memory_text_element(renderer, label, output_geo, scale, alpha).transpose())
        .collect()
}

pub fn ordered_text_elements_for_window(
    renderer: &mut GlesRenderer,
    space: &Space<Window>,
    decorations: &HashMap<Window, WindowDecorationState>,
    output: &Output,
    window: &Window,
    alpha: f32,
) -> Result<Vec<(usize, DecorationTextureElements)>, GlesError> {
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
        .filter_map(|label| {
            memory_text_element(renderer, label, output_geo, scale, alpha)
                .transpose()
                .map(|result| result.map(|element| (label.order, element)))
        })
        .collect()
}

pub fn ordered_text_elements_for_decoration(
    renderer: &mut GlesRenderer,
    decoration: &WindowDecorationState,
    output_geo: Rectangle<i32, Logical>,
    scale: OutputScale<f64>,
    alpha: f32,
) -> Result<Vec<(usize, DecorationTextureElements)>, GlesError> {
    decoration
        .text_buffers
        .iter()
        .filter_map(|label| {
            memory_text_element(renderer, label, output_geo, scale, alpha)
                .transpose()
                .map(|result| result.map(|element| (label.order, element)))
        })
        .collect()
}

pub fn text_elements_for_decoration(
    renderer: &mut GlesRenderer,
    decoration: &WindowDecorationState,
    output_geo: Rectangle<i32, Logical>,
    scale: OutputScale<f64>,
    alpha: f32,
) -> Result<Vec<DecorationTextureElements>, GlesError> {
    decoration
        .text_buffers
        .iter()
        .filter_map(|label| memory_text_element(renderer, label, output_geo, scale, alpha).transpose())
        .collect()
}

fn memory_text_element(
    renderer: &mut GlesRenderer,
    label: &CachedDecorationLabel,
    output_geo: Rectangle<i32, Logical>,
    scale: OutputScale<f64>,
    alpha: f32,
) -> Result<Option<DecorationTextureElements>, GlesError> {
    if intersect_logical_rect(label.rect, output_geo).is_none() {
        return Ok(None);
    }

    let local = Point::from((
        label.rect.x - output_geo.loc.x,
        label.rect.y - output_geo.loc.y,
    ));
    let physical: Point<i32, Physical> = local.to_f64().to_physical_precise_round(scale);
    let element = MemoryRenderBufferRenderElement::from_buffer(
        renderer,
        physical.to_f64(),
        &label.buffer,
        Some(alpha.clamp(0.0, 1.0)),
        None,
        None,
        Kind::Unspecified,
    )?;
    if let Some(clip_rect) = label.clip_rect {
        let clipped = crate::backend::clipped_memory::ClippedMemoryElement::new(
            renderer,
            element,
            scale,
            label.rect,
            clip_rect,
            label.clip_radius,
        )?;
        Ok(Some(DecorationTextureElements::Clipped(clipped)))
    } else {
        Ok(Some(DecorationTextureElements::Memory(element)))
    }
}

fn attrs_for_spec<'a>(spec: &'a LabelSpec) -> Attrs<'a> {
    let mut attrs = Attrs::new()
        .color(CosmicColor::rgba(
            spec.color.r,
            spec.color.g,
            spec.color.b,
            spec.color.a,
        ))
        .weight(CosmicWeight(parse_font_weight(spec.font_weight.as_ref())));

    if let Some(family) = spec
        .font_family
        .as_ref()
        .and_then(|families| families.first())
    {
        attrs = attrs.family(CosmicFamily::Name(family.as_str()));
    }

    attrs.style(CosmicStyle::Normal)
}

fn alignment_for_spec(text_align: Option<&str>) -> Option<Align> {
    match text_align {
        Some("center") => Some(Align::Center),
        Some("end") => Some(Align::End),
        _ => None,
    }
}

fn blend_pixel(pixels: &mut [u8], width: i32, x: i32, y: i32, rgba: (u8, u8, u8, u8)) {
    let index = ((y * width + x) * 4) as usize;
    let (src_r, src_g, src_b, src_a) = rgba;
    if src_a == 0 {
        return;
    }

    let dst_b = pixels[index];
    let dst_g = pixels[index + 1];
    let dst_r = pixels[index + 2];
    let dst_a = pixels[index + 3];

    let src_a_u16 = u16::from(src_a);
    let inv_a = 255u16.saturating_sub(src_a_u16);
    let src_r_pm = (u16::from(src_r) * src_a_u16) / 255;
    let src_g_pm = (u16::from(src_g) * src_a_u16) / 255;
    let src_b_pm = (u16::from(src_b) * src_a_u16) / 255;

    let out_a = src_a_u16 + ((u16::from(dst_a) * inv_a) / 255);
    let out_r = src_r_pm + ((u16::from(dst_r) * inv_a) / 255);
    let out_g = src_g_pm + ((u16::from(dst_g) * inv_a) / 255);
    let out_b = src_b_pm + ((u16::from(dst_b) * inv_a) / 255);

    pixels[index] = out_b.min(255) as u8;
    pixels[index + 1] = out_g.min(255) as u8;
    pixels[index + 2] = out_r.min(255) as u8;
    pixels[index + 3] = out_a.min(255) as u8;
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

    (right > left && bottom > top).then(|| LogicalRect::new(left, top, right - left, bottom - top))
}
