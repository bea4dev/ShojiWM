use std::{
    cell::RefCell,
    collections::{HashMap, HashSet, VecDeque, hash_map::DefaultHasher},
    hash::{Hash, Hasher},
    time::{Duration, Instant},
};

use crate::{
    backend::async_assets::{AsyncAssetJob, AsyncAssetJobSender},
    backend::visual::{
        PreciseLogicalRect, relative_physical_rect_from_root_precise,
        relative_physical_rect_from_root_snapped_edges,
    },
    ssd::{Color, DecorationPart, LogicalRect, WindowDecorationState},
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
                Kind,
                memory::{MemoryRenderBuffer, MemoryRenderBufferRenderElement},
            },
            gles::{GlesError, GlesRenderer},
        },
    },
    desktop::{Space, Window},
    output::Output,
    utils::{Logical, Point, Rectangle, Scale as OutputScale, Size},
};

thread_local! {
    static LABEL_MEASURER: RefCell<LabelMeasurer> = RefCell::new(LabelMeasurer::new());
}

#[derive(Debug, Clone)]
pub struct CachedDecorationLabel {
    pub owner_node_id: Option<String>,
    pub stable_key: String,
    pub order: usize,
    pub rect: LogicalRect,
    pub rect_precise: Option<PreciseLogicalRect>,
    pub rendered_rect: LogicalRect,
    pub rendered_rect_precise: Option<PreciseLogicalRect>,
    /// Exact rasterization scale (the layout scale), so the buffer maps
    /// one-to-one onto the node's physical pixels.
    pub raster_scale: f64,
    pub clip_rect: Option<LogicalRect>,
    pub clip_radius: i32,
    pub clip_rect_precise: Option<PreciseLogicalRect>,
    pub clip_radius_precise: Option<f32>,
    pub text: String,
    pub color: Color,
    /// Logical pixels the buffer reaches past `rect` on each side: room for
    /// the text shadow, which is not cut at the label's box. Whole logical
    /// pixels, so the raster and the output scale both map it exactly.
    pub overflow: i32,
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
    pub rect_precise: Option<PreciseLogicalRect>,
    pub text: String,
    pub color: Color,
    /// Logical font size; fractional sizes are allowed.
    pub font_size: f32,
    pub font_weight: Option<serde_json::Value>,
    pub font_family: Option<Vec<String>>,
    pub text_align: Option<String>,
    /// `"ellipsis"`: text wider than the label ends in "…".
    pub text_overflow: Option<String>,
    pub line_height: Option<f32>,
    /// A copy of the text, offset, blurred and tinted, drawn under it.
    pub text_shadow: Option<crate::ssd::TextShadow>,
    /// Exact rasterization scale (the layout scale), so the buffer maps
    /// one-to-one onto the node's physical pixels.
    pub raster_scale: f64,
}

#[derive(Debug)]
pub struct TextRasterizer {
    font_system: FontSystem,
    swash_cache: SwashCache,
    async_job_sender: Option<AsyncAssetJobSender>,
    async_buffers: HashMap<u64, TimedTextBuffer>,
    async_in_flight: HashSet<u64>,
    async_missing: HashSet<u64>,
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
            async_missing: HashSet::new(),
        }
    }

    pub fn render_label(&mut self, spec: &LabelSpec) -> Option<CachedDecorationLabel> {
        self.prune_async_buffers();
        let spec_hash = hash_label_spec(spec);
        if let Some(cached) = self.async_buffers.get_mut(&spec_hash) {
            cached.last_used_at = Instant::now();
            return Some(CachedDecorationLabel {
                owner_node_id: None,
                stable_key: String::new(),
                order: 0,
                rect: spec.rect,
                rect_precise: spec.rect_precise,
                rendered_rect: spec.rect,
                rendered_rect_precise: spec.rect_precise,
                raster_scale: spec.raster_scale,
                clip_rect: None,
                clip_radius: 0,
                clip_rect_precise: None,
                clip_radius_precise: None,
                text: spec.text.clone(),
                color: spec.color,
                overflow: shadow_overflow(spec),
                buffer: cached.buffer.clone(),
            });
        }

        if let Some(async_job_sender) = &self.async_job_sender {
            if self.async_missing.contains(&spec_hash) {
                return None;
            }
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
            stable_key: String::new(),
            order: 0,
            rect: spec.rect,
            rect_precise: spec.rect_precise,
            rendered_rect: spec.rect,
            rendered_rect_precise: spec.rect_precise,
            raster_scale: spec.raster_scale,
            clip_rect: None,
            clip_radius: 0,
            clip_rect_precise: None,
            clip_radius_precise: None,
            text: spec.text.clone(),
            color: spec.color,
            overflow: shadow_overflow(spec),
            buffer: MemoryRenderBuffer::from_slice(
                &rendered.pixels,
                Fourcc::Argb8888,
                (rendered.width, rendered.height),
                1,
                smithay::utils::Transform::Normal,
                None,
            ),
        })
    }

    pub fn render_label_pixels(&mut self, spec: &LabelSpec) -> Option<RenderedLabelPixels> {
        if spec.text.is_empty() || spec.rect.width <= 0 || spec.rect.height <= 0 {
            return None;
        }

        let raster_scale = spec.raster_scale.max(0.0001) as f32;
        let logical_width = spec
            .rect_precise
            .map(|rect| rect.width)
            .unwrap_or(spec.rect.width as f32)
            .max(0.0);
        let logical_height = spec
            .rect_precise
            .map(|rect| rect.height)
            .unwrap_or(spec.rect.height as f32)
            .max(0.0);
        let target_width = (logical_width * raster_scale).round().max(1.0) as i32;
        let target_height = (logical_height * raster_scale).round().max(1.0) as i32;
        let font_size = spec.font_size.max(1.0) * raster_scale;
        let line_height = spec
            .line_height
            .unwrap_or(spec.font_size.max(1.0) + crate::ssd::DEFAULT_LABEL_LINE_GAP as f32)
            * raster_scale;
        let metrics = Metrics::new(font_size, line_height.max(1.0));
        let mut buffer = Buffer::new(&mut self.font_system, metrics);

        let attrs = attrs_for_spec(spec);
        let alignment = alignment_for_spec(spec.text_align.as_deref());

        let shape = |buffer: &mut Buffer, font_system: &mut FontSystem, text: &str| {
            let mut buffer = buffer.borrow_with(font_system);
            buffer.set_size(Some(target_width as f32), Some(target_height as f32));
            buffer.set_wrap(Wrap::None);
            buffer.set_text(text, &attrs, Shaping::Advanced, alignment);
            buffer.shape_until_scroll(false);
            buffer
                .layout_runs()
                .map(|run| run.line_w)
                .fold(0.0f32, f32::max)
        };
        let width = shape(&mut buffer, &mut self.font_system, &spec.text);
        if spec.text_overflow.as_deref() == Some("ellipsis") && width > target_width as f32 {
            let font_system = &mut self.font_system;
            let text = ellipsize(&spec.text, |candidate| {
                shape(&mut buffer, font_system, candidate) <= target_width as f32
            });
            shape(&mut buffer, font_system, &text);
        }

        let runs = buffer.layout_runs().collect::<Vec<_>>();
        let text_height = runs
            .iter()
            .map(|run| run.line_top + run.line_height)
            .fold(0.0f32, f32::max);
        let y_offset = ((target_height as f32 - text_height) * 0.5).max(0.0);

        // The buffer reaches `pad` past the box on each side, for the shadow;
        // the text keeps its place in the box.
        let pad = (f64::from(shadow_overflow(spec)) * f64::from(raster_scale)).round() as i32;
        let buffer_width = target_width + 2 * pad;
        let buffer_height = target_height + 2 * pad;
        let mut pixels = vec![0u8; (buffer_width * buffer_height * 4) as usize];

        // The shadow first, under the text: the glyphs' coverage, offset,
        // blurred, then tinted.
        if let Some(shadow) = spec.text_shadow.filter(|shadow| shadow.color.a > 0) {
            let dx = (shadow.offset_x * f64::from(raster_scale)).round() as i32;
            let dy = (shadow.offset_y * f64::from(raster_scale)).round() as i32;
            let mut mask = vec![0f32; (buffer_width * buffer_height) as usize];
            {
                let mut buffer = buffer.borrow_with(&mut self.font_system);
                let full = CosmicColor::rgba(255, 255, 255, 255);
                buffer.draw(&mut self.swash_cache, full, |x, y, w, h, color| {
                    let coverage = f32::from(color.a()) / 255.0;
                    for off_y in 0..h as i32 {
                        for off_x in 0..w as i32 {
                            let px = x + off_x + dx + pad;
                            let py = y + off_y + y_offset as i32 + dy + pad;
                            if px < 0 || py < 0 || px >= buffer_width || py >= buffer_height {
                                continue;
                            }
                            let cell = &mut mask[(py * buffer_width + px) as usize];
                            *cell = (*cell + coverage).min(1.0);
                        }
                    }
                });
            }
            let spread = (shadow.spread * f64::from(raster_scale)).round() as i32;
            grow_mask(&mut mask, buffer_width, buffer_height, spread);
            // CSS: the Gaussian's sigma is half the blur radius.
            let sigma = shadow.blur * f64::from(raster_scale) / 2.0;
            blur_mask(&mut mask, buffer_width, buffer_height, sigma);
            for py in 0..buffer_height {
                for px in 0..buffer_width {
                    let coverage = mask[(py * buffer_width + px) as usize];
                    if coverage <= 0.0 {
                        continue;
                    }
                    let alpha = (coverage * f32::from(shadow.color.a)).round().min(255.0) as u8;
                    blend_pixel(
                        &mut pixels,
                        buffer_width,
                        px,
                        py,
                        (shadow.color.r, shadow.color.g, shadow.color.b, alpha),
                    );
                }
            }
        }

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
                            buffer_width,
                            px + pad,
                            py + pad,
                            color.as_rgba_tuple(),
                        );
                    }
                }
            });
        }

        Some(RenderedLabelPixels {
            width: buffer_width,
            height: buffer_height,
            pixels,
        })
    }

    pub fn handle_async_ready(&mut self, spec_hash: u64, width: i32, height: i32, pixels: Vec<u8>) {
        self.async_in_flight.remove(&spec_hash);
        self.async_missing.remove(&spec_hash);
        self.async_buffers.insert(
            spec_hash,
            TimedTextBuffer {
                buffer: MemoryRenderBuffer::from_slice(
                    &pixels,
                    Fourcc::Argb8888,
                    (width, height),
                    1,
                    smithay::utils::Transform::Normal,
                    None,
                ),
                last_used_at: Instant::now(),
            },
        );
    }

    pub fn handle_async_miss(&mut self, spec_hash: u64) {
        self.async_in_flight.remove(&spec_hash);
        self.async_missing.insert(spec_hash);
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

        let font_size = spec.font_size.max(1.0);
        let line_height = spec
            .line_height
            .unwrap_or(font_size.ceil() + crate::ssd::DEFAULT_LABEL_LINE_GAP as f32);
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

/// The longest prefix of `text` that `fits` with "…" after it (just "…"
/// when not even one character does). `text` itself is taken not to fit.
fn ellipsize(text: &str, mut fits: impl FnMut(&str) -> bool) -> String {
    // Prefix ends after the first, second, ... character, short of the whole.
    let ends = text
        .char_indices()
        .map(|(index, _)| index)
        .skip(1)
        .collect::<Vec<_>>();
    let truncated = |end: usize| format!("{}…", text[..end].trim_end());
    let (mut low, mut high) = (0usize, ends.len());
    while low < high {
        let middle = (low + high).div_ceil(2);
        if fits(&truncated(ends[middle - 1])) {
            low = middle;
        } else {
            high = middle - 1;
        }
    }
    if low == 0 {
        "…".to_string()
    } else {
        truncated(ends[low - 1])
    }
}

pub fn hash_label_spec(spec: &LabelSpec) -> u64 {
    let mut hasher = DefaultHasher::new();
    spec.text.hash(&mut hasher);
    spec.rect.width.hash(&mut hasher);
    spec.rect.height.hash(&mut hasher);
    spec.rect_precise
        .map(|rect| {
            (
                (rect.width * 1024.0).round() as i32,
                (rect.height * 1024.0).round() as i32,
            )
        })
        .hash(&mut hasher);
    spec.color.r.hash(&mut hasher);
    spec.color.g.hash(&mut hasher);
    spec.color.b.hash(&mut hasher);
    spec.color.a.hash(&mut hasher);
    spec.font_size.to_bits().hash(&mut hasher);
    spec.text_align.hash(&mut hasher);
    spec.text_overflow.hash(&mut hasher);
    spec.line_height.map(f32::to_bits).hash(&mut hasher);
    spec.text_shadow
        .map(|shadow| {
            (
                shadow.offset_x.to_bits(),
                shadow.offset_y.to_bits(),
                shadow.blur.to_bits(),
                shadow.spread.to_bits(),
                (
                    shadow.color.r,
                    shadow.color.g,
                    shadow.color.b,
                    shadow.color.a,
                ),
            )
        })
        .hash(&mut hasher);
    spec.raster_scale.to_bits().hash(&mut hasher);
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
    spec.font_size.to_bits().hash(&mut hasher);
    spec.line_height.map(f32::to_bits).hash(&mut hasher);
    spec.font_family.hash(&mut hasher);
    spec.font_weight
        .as_ref()
        .and_then(|value| serde_json::to_string(value).ok())
        .hash(&mut hasher);
    hasher.finish()
}

smithay::render_elements! {
    pub DecorationTextureElements<=GlesRenderer>;
    Memory=crate::backend::clipped_memory::ExactMemoryElement,
    Clipped=crate::backend::clipped_memory::ClippedMemoryElement,
}

/// The text buffers of `decoration` drawn in `part`.
fn text_buffers_for(
    decoration: &WindowDecorationState,
    part: DecorationPart,
) -> impl Iterator<Item = &CachedDecorationLabel> {
    let scopes = decoration.popup_scopes();
    decoration
        .text_buffers
        .iter()
        .filter(move |label| scopes.includes(part, &label.stable_key))
}

/// The text elements of one `part` of `decoration`, with their paint order.
pub fn ordered_text_elements_for_part(
    renderer: &mut GlesRenderer,
    decoration: &WindowDecorationState,
    part: DecorationPart,
    output_geo: Rectangle<i32, Logical>,
    scale: OutputScale<f64>,
    alpha: f32,
) -> Result<Vec<(usize, DecorationTextureElements)>, GlesError> {
    text_buffers_for(decoration, part)
        .filter_map(|label| {
            memory_text_element(
                renderer,
                label,
                decoration.layout.root.rect,
                decoration.root_subpixel_offset,
                output_geo,
                scale,
                alpha,
            )
            .transpose()
            .map(|result| result.map(|element| (label.order, element)))
        })
        .collect()
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

    text_buffers_for(decoration, DecorationPart::Window)
        .filter_map(|label| {
            memory_text_element(
                renderer,
                label,
                decoration.layout.root.rect,
                decoration.root_subpixel_offset,
                output_geo,
                scale,
                alpha,
            )
            .transpose()
        })
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

    text_buffers_for(decoration, DecorationPart::Window)
        .filter_map(|label| {
            memory_text_element(
                renderer,
                label,
                decoration.layout.root.rect,
                decoration.root_subpixel_offset,
                output_geo,
                scale,
                alpha,
            )
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
    text_buffers_for(decoration, DecorationPart::Window)
        .filter_map(|label| {
            memory_text_element(
                renderer,
                label,
                decoration.layout.root.rect,
                decoration.root_subpixel_offset,
                output_geo,
                scale,
                alpha,
            )
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
    text_buffers_for(decoration, DecorationPart::Window)
        .filter_map(|label| {
            memory_text_element(
                renderer,
                label,
                decoration.layout.root.rect,
                decoration.root_subpixel_offset,
                output_geo,
                scale,
                alpha,
            )
            .transpose()
        })
        .collect()
}

pub(crate) fn memory_text_element(
    renderer: &mut GlesRenderer,
    label: &CachedDecorationLabel,
    root_rect: LogicalRect,
    root_subpixel: crate::backend::visual::RootSubpixelEdges,
    output_geo: Rectangle<i32, Logical>,
    scale: OutputScale<f64>,
    alpha: f32,
) -> Result<Option<DecorationTextureElements>, GlesError> {
    // The box and its shadow's reach: a glow may cross onto this output.
    let reach = LogicalRect::new(
        label.rect.x - label.overflow,
        label.rect.y - label.overflow,
        label.rect.width + 2 * label.overflow,
        label.rect.height + 2 * label.overflow,
    );
    if intersect_logical_rect(reach, output_geo).is_none() {
        return Ok(None);
    }

    let physical = label
        .rect_precise
        .map(|rect| {
            relative_physical_rect_from_root_precise(
                rect,
                root_rect,
                root_subpixel,
                output_geo,
                scale,
            )
        })
        .unwrap_or_else(|| {
            relative_physical_rect_from_root_snapped_edges(
                label.rect,
                root_rect,
                root_subpixel,
                output_geo,
                scale,
            )
        });
    // The buffer reaches `overflow` logical pixels past the box on each side
    // (the shadow), at this output's scale.
    let grow_x = (f64::from(label.overflow) * scale.x).round() as i32;
    let grow_y = (f64::from(label.overflow) * scale.y).round() as i32;
    let physical = Rectangle::new(
        physical.loc - Point::from((grow_x, grow_y)),
        physical.size + Size::from((2 * grow_x, 2 * grow_y)),
    );
    let overflow_logical = f64::from(label.overflow);
    let element = MemoryRenderBufferRenderElement::from_buffer(
        renderer,
        physical.loc.to_f64(),
        &label.buffer,
        Some(alpha.clamp(0.0, 1.0)),
        None,
        None,
        Kind::Unspecified,
    )?;
    let element = crate::backend::clipped_memory::ExactMemoryElement::new(element, Some(physical));
    let clip_rect = label.clip_rect.or_else(|| {
        label.clip_rect_precise.map(|clip_rect| {
            LogicalRect::new(
                clip_rect.x.round() as i32,
                clip_rect.y.round() as i32,
                clip_rect.width.round().max(0.0) as i32,
                clip_rect.height.round().max(0.0) as i32,
            )
        })
    });
    if let Some(clip_rect) = clip_rect {
        let clipped = crate::backend::clipped_memory::ClippedMemoryElement::new(
            renderer,
            element,
            scale,
            {
                let grow = label.overflow;
                LogicalRect::new(
                    label.rect.x - root_rect.x - grow,
                    label.rect.y - root_rect.y - grow,
                    label.rect.width + 2 * grow,
                    label.rect.height + 2 * grow,
                )
            },
            LogicalRect::new(
                clip_rect.x - root_rect.x,
                clip_rect.y - root_rect.y,
                clip_rect.width,
                clip_rect.height,
            ),
            label.clip_radius,
            Some(PreciseLogicalRect {
                x: label
                    .rect_precise
                    .map(|rect| rect.x - root_rect.x as f32)
                    .unwrap_or((label.rect.x - root_rect.x) as f32)
                    - overflow_logical as f32,
                y: label
                    .rect_precise
                    .map(|rect| rect.y - root_rect.y as f32)
                    .unwrap_or((label.rect.y - root_rect.y) as f32)
                    - overflow_logical as f32,
                width: label
                    .rect_precise
                    .map(|rect| rect.width)
                    .unwrap_or(label.rect.width as f32)
                    + 2.0 * overflow_logical as f32,
                height: label
                    .rect_precise
                    .map(|rect| rect.height)
                    .unwrap_or(label.rect.height as f32)
                    + 2.0 * overflow_logical as f32,
            }),
            label.clip_rect_precise.map(|clip_rect| PreciseLogicalRect {
                x: clip_rect.x - root_rect.x as f32,
                y: clip_rect.y - root_rect.y as f32,
                width: clip_rect.width,
                height: clip_rect.height,
            }),
            label.clip_radius_precise,
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

/// Logical pixels a label's buffer reaches past its box on each side so its
/// shadow is whole: the blur's reach (three sigmas, CSS's sigma being half
/// the blur radius), the spread and the offset, rounded up. 0 without a
/// shadow.
pub(crate) fn shadow_overflow(spec: &LabelSpec) -> i32 {
    let Some(shadow) = spec.text_shadow.filter(|shadow| shadow.color.a > 0) else {
        return 0;
    };
    let offset = shadow.offset_x.abs().max(shadow.offset_y.abs());
    (1.5 * shadow.blur + shadow.spread + offset).ceil() as i32
}

/// Grows a coverage mask in place by `radius` pixels: each pixel takes the
/// highest coverage within that distance, along rows then columns.
fn grow_mask(mask: &mut [f32], width: i32, height: i32, radius: i32) {
    if radius <= 0 || width <= 0 || height <= 0 {
        return;
    }
    let (width, height) = (width as usize, height as usize);
    let radius = radius as usize;
    let mut line = Vec::new();
    let mut window = VecDeque::new();
    for y in 0..height {
        line.clear();
        line.extend_from_slice(&mask[y * width..(y + 1) * width]);
        max_filter_line(&line, radius, &mut window, |x, value| {
            mask[y * width + x] = value
        });
    }
    for x in 0..width {
        line.clear();
        line.extend((0..height).map(|y| mask[y * width + x]));
        max_filter_line(&line, radius, &mut window, |y, value| {
            mask[y * width + x] = value
        });
    }
}

/// The highest value within `radius` of each index along a line. `window`
/// holds the indices that can still be a maximum, their values decreasing,
/// so each index goes in and out once: the cost does not grow with the
/// radius.
fn max_filter_line(
    line: &[f32],
    radius: usize,
    window: &mut VecDeque<usize>,
    mut write: impl FnMut(usize, f32),
) {
    window.clear();
    let mut next = 0;
    for index in 0..line.len() {
        let end = (index + radius + 1).min(line.len());
        while next < end {
            while window.back().is_some_and(|&back| line[back] <= line[next]) {
                window.pop_back();
            }
            window.push_back(next);
            next += 1;
        }
        let start = index.saturating_sub(radius);
        while window.front().is_some_and(|&front| front < start) {
            window.pop_front();
        }
        // `index` itself is in its window, so the window is never empty.
        write(index, line[window[0]]);
    }
}

/// Blurs a coverage mask in place with three box blurs, horizontal then
/// vertical, which together come close to a Gaussian of this sigma (in
/// pixels). A sigma under half a pixel leaves the mask as it is.
fn blur_mask(mask: &mut [f32], width: i32, height: i32, sigma: f64) {
    if sigma < 0.5 || width <= 0 || height <= 0 {
        return;
    }
    // Three boxes of width 2r + 1 add up to a variance of (4r^2 + 4r) / 4.
    let radius = (((4.0 * sigma * sigma + 1.0).sqrt() - 1.0) / 2.0)
        .round()
        .max(1.0) as i32;
    let (width, height) = (width as usize, height as usize);
    let mut line = Vec::new();
    for _ in 0..3 {
        for y in 0..height {
            line.clear();
            line.extend_from_slice(&mask[y * width..(y + 1) * width]);
            box_blur_line(&line, radius, |x, value| mask[y * width + x] = value);
        }
        for x in 0..width {
            line.clear();
            line.extend((0..height).map(|y| mask[y * width + x]));
            box_blur_line(&line, radius, |y, value| mask[y * width + x] = value);
        }
    }
}

/// One box blur of radius `radius` along a line; outside the line counts as
/// empty.
fn box_blur_line(line: &[f32], radius: i32, mut write: impl FnMut(usize, f32)) {
    let len = line.len() as i32;
    let at = |index: i32| {
        if index < 0 || index >= len {
            0.0
        } else {
            line[index as usize]
        }
    };
    let span = (2 * radius + 1) as f32;
    let mut sum: f32 = (-radius..=radius).map(at).sum();
    for index in 0..len {
        write(index as usize, sum / span);
        sum += at(index + radius + 1) - at(index - radius);
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

#[cfg(test)]
mod tests {
    use super::{LabelSpec, blur_mask, ellipsize, grow_mask, shadow_overflow};
    use crate::ssd::{Color, LogicalRect, TextShadow};

    fn at_most(chars: usize) -> impl FnMut(&str) -> bool {
        move |text: &str| text.chars().count() <= chars
    }

    fn spec_with_shadow(shadow: Option<TextShadow>, raster_scale: f64) -> LabelSpec {
        LabelSpec {
            rect: LogicalRect::new(0, 0, 100, 18),
            rect_precise: None,
            text: "Title".into(),
            color: Color::WHITE,
            font_size: 12.0,
            font_weight: None,
            font_family: None,
            text_align: None,
            text_overflow: None,
            line_height: None,
            text_shadow: shadow,
            raster_scale,
        }
    }

    #[test]
    fn shadow_overflow_covers_the_blur_and_the_offset() {
        let glow = TextShadow {
            offset_x: 0.0,
            offset_y: 0.0,
            blur: 6.0,
            spread: 0.0,
            color: Color::WHITE,
        };
        assert_eq!(shadow_overflow(&spec_with_shadow(None, 1.0)), 0);
        // sigma 3, three sigmas.
        assert_eq!(shadow_overflow(&spec_with_shadow(Some(glow), 1.0)), 9);
        // Logical pixels: the scale does not change it; the offset and the
        // spread add to it, rounded up.
        let line = TextShadow {
            offset_y: 1.0,
            spread: 0.5,
            ..glow
        };
        assert_eq!(shadow_overflow(&spec_with_shadow(Some(line), 2.0)), 11);
    }

    #[test]
    fn grow_mask_widens_a_point_to_a_square() {
        let mut mask = vec![0.0f32; 7 * 7];
        mask[3 * 7 + 3] = 0.5;
        grow_mask(&mut mask, 7, 7, 2);
        assert_eq!(mask[7 + 1], 0.5);
        assert_eq!(mask[5 * 7 + 5], 0.5);
        assert_eq!(mask[0], 0.0);
    }

    #[test]
    fn grow_mask_takes_the_highest_value_in_reach() {
        let (width, height, radius) = (13usize, 5usize, 3usize);
        let mask: Vec<f32> = (0..width * height)
            .map(|index| ((index * 7919) % 101) as f32 / 100.0)
            .collect();
        let mut grown = mask.clone();
        grow_mask(&mut grown, width as i32, height as i32, radius as i32);
        for y in 0..height {
            for x in 0..width {
                let mut expected = 0.0f32;
                for sy in y.saturating_sub(radius)..(y + radius + 1).min(height) {
                    for sx in x.saturating_sub(radius)..(x + radius + 1).min(width) {
                        expected = expected.max(mask[sy * width + sx]);
                    }
                }
                assert_eq!(grown[y * width + x], expected, "at ({x}, {y})");
            }
        }
    }

    #[test]
    fn blur_mask_keeps_the_total_and_spreads_it() {
        let mut mask = vec![0.0f32; 21 * 21];
        mask[10 * 21 + 10] = 1.0;
        blur_mask(&mut mask, 21, 21, 2.0);
        let total: f32 = mask.iter().sum();
        assert!((total - 1.0).abs() < 1e-3, "total {total}");
        assert!(mask[10 * 21 + 10] < 0.2);
        assert!(mask[10 * 21 + 12] > 0.0);
    }

    #[test]
    fn blur_mask_leaves_a_sharp_shadow_alone() {
        let mut mask = vec![0.0f32; 9];
        mask[4] = 1.0;
        blur_mask(&mut mask, 3, 3, 0.0);
        assert_eq!(mask[4], 1.0);
    }

    #[test]
    fn ellipsize_keeps_the_longest_prefix_that_fits() {
        assert_eq!(ellipsize("Hello world", at_most(6)), "Hello…");
        assert_eq!(ellipsize("Hello world", at_most(9)), "Hello wo…");
    }

    #[test]
    fn ellipsize_cuts_on_character_boundaries() {
        assert_eq!(ellipsize("ようこそ世界", at_most(3)), "よう…");
    }

    #[test]
    fn ellipsize_falls_back_to_the_ellipsis_alone() {
        assert_eq!(ellipsize("Hello", at_most(0)), "…");
        assert_eq!(ellipsize("H", at_most(1)), "…");
    }
}
