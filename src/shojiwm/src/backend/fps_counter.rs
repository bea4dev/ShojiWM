//! FPS / frame-time overlay rendered into the top-left of each output.
//!
//! Glyphs (0-9, '.', ' ', 'F', 'P', 'S', 'm', 's', 'f', 'p') are rasterized
//! once via cosmic-text on first enable and stored as per-character
//! `MemoryRenderBuffer`s. Per frame we only composite those buffers — no text
//! shaping or glyph rasterization happens on the hot path. Frame time is
//! measured by recording `Instant::now()` after each present and consuming
//! the previous frame's value for display (acceptable one-frame lag).

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    time::Instant,
};

use cosmic_text::{
    Align, Attrs, Buffer, Color as CosmicColor, Family as CosmicFamily, FontSystem, Metrics,
    Shaping, SwashCache, Weight as CosmicWeight, Wrap,
};
use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            element::{
                Kind,
                memory::{MemoryRenderBuffer, MemoryRenderBufferRenderElement},
            },
            gles::GlesRenderer,
        },
    },
    utils::{Logical, Physical, Point, Rectangle, Scale as OutputScale, Transform},
};

use crate::backend::text::DecorationTextureElements;

/// Characters the overlay can print. Two short lines ("nn.n fps", "nn.n ms")
/// only need digits, period, space, and a tiny ASCII subset.
const ATLAS_CHARS: &str = "0123456789. fpsmFPSMS";
const FONT_SIZE_LOGICAL: f32 = 14.0;
const LINE_HEIGHT_LOGICAL: f32 = 18.0;
/// Render glyphs at 2x then downscale via the buffer scale so the overlay
/// stays crisp regardless of output scale.
const ATLAS_RASTER_SCALE: i32 = 2;
/// EMA smoothing for the displayed FPS. Higher = snappier but noisier.
const FRAME_EMA_ALPHA: f32 = 0.1;

#[derive(Debug)]
pub struct FpsCounter {
    enabled: bool,
    atlas: Option<GlyphAtlas>,
    per_output: BTreeMap<String, OutputState>,
}

#[derive(Debug, Clone)]
struct GlyphAtlas {
    glyphs: HashMap<char, GlyphEntry>,
    /// Logical-pixel line height to advance vertically between text rows.
    line_height_logical: i32,
    /// Logical-pixel advance width to use when an unknown character is
    /// requested (defensive fallback only — ATLAS_CHARS should cover us).
    fallback_advance_logical: i32,
}

#[derive(Debug, Clone)]
struct GlyphEntry {
    buffer: MemoryRenderBuffer,
    /// Logical-pixel advance for cursor positioning between glyphs.
    advance_logical: i32,
}

#[derive(Debug, Default, Clone, Copy)]
struct OutputState {
    last_present_at: Option<Instant>,
    /// EMA of frame interval in milliseconds. Used for FPS line so the
    /// displayed number doesn't twitch every frame.
    ema_frame_ms: f32,
    /// Most recent raw frame interval — surfaced as the "ms" line so frame
    /// drops are visible.
    last_frame_ms: f32,
}

impl Default for FpsCounter {
    fn default() -> Self {
        Self::new()
    }
}

impl FpsCounter {
    pub fn new() -> Self {
        Self {
            enabled: false,
            atlas: None,
            per_output: BTreeMap::new(),
        }
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        if self.enabled == enabled {
            return;
        }
        self.enabled = enabled;
        if enabled && self.atlas.is_none() {
            self.atlas = Some(build_atlas());
        }
        if !enabled {
            // Drop timing state so re-enabling starts clean rather than
            // showing the gap that elapsed while it was off.
            self.per_output.clear();
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Record that an output just presented a frame. Call AFTER the render
    /// for that output completes (or after submit) so the measured interval
    /// reflects real refresh cadence.
    pub fn record_present(&mut self, output_name: &str) {
        if !self.enabled {
            return;
        }
        let now = Instant::now();
        let state = self.per_output.entry(output_name.to_owned()).or_default();
        if let Some(last) = state.last_present_at {
            let delta_ms = now.duration_since(last).as_secs_f32() * 1000.0;
            // Ignore implausible intervals (long pauses, startup, etc.) so a
            // single outlier doesn't poison the EMA.
            if delta_ms > 0.0 && delta_ms < 1000.0 {
                state.last_frame_ms = delta_ms;
                state.ema_frame_ms = if state.ema_frame_ms <= 0.0 {
                    delta_ms
                } else {
                    state.ema_frame_ms * (1.0 - FRAME_EMA_ALPHA) + delta_ms * FRAME_EMA_ALPHA
                };
            }
        }
        state.last_present_at = Some(now);
    }

    /// Build render elements for the overlay positioned in the top-left of
    /// the given output. Returns an empty Vec when the overlay is disabled or
    /// no frame timing is yet known for this output.
    pub fn render_elements(
        &self,
        renderer: &mut GlesRenderer,
        output_name: &str,
        output_geo: Rectangle<i32, Logical>,
        scale: OutputScale<f64>,
    ) -> Vec<DecorationTextureElements> {
        if !self.enabled {
            return Vec::new();
        }
        let Some(atlas) = self.atlas.as_ref() else {
            return Vec::new();
        };
        let Some(state) = self.per_output.get(output_name) else {
            return Vec::new();
        };
        if state.ema_frame_ms <= 0.0 {
            return Vec::new();
        }

        let fps = 1000.0 / state.ema_frame_ms.max(0.001);
        let line1 = format_fps_line(fps);
        let line2 = format_ms_line(state.last_frame_ms);

        let padding_x_logical: i32 = 8;
        let padding_y_logical: i32 = 8;

        let mut out = Vec::with_capacity(line1.len() + line2.len());
        // Render in OUTPUT-LOCAL physical coordinates: the damage tracker
        // already projects from output-local to whatever surface it ends up
        // painting on. output_geo.loc is the output's GLOBAL position in the
        // space; we want top-left of the output itself.
        emit_line(
            renderer,
            atlas,
            &line1,
            padding_x_logical,
            padding_y_logical,
            scale,
            &mut out,
        );
        emit_line(
            renderer,
            atlas,
            &line2,
            padding_x_logical,
            padding_y_logical + atlas.line_height_logical,
            scale,
            &mut out,
        );
        // Currently output_geo is unused: positioning is purely output-local.
        // Kept in the signature so callers can opt into right-alignment later
        // without changing the call sites.
        let _ = output_geo;
        out
    }
}

fn format_fps_line(fps: f32) -> String {
    format!("{:.1} fps", fps.clamp(0.0, 9999.0))
}

fn format_ms_line(ms: f32) -> String {
    format!("{:.2} ms", ms.clamp(0.0, 999.99))
}

fn emit_line(
    renderer: &mut GlesRenderer,
    atlas: &GlyphAtlas,
    text: &str,
    origin_x_logical: i32,
    origin_y_logical: i32,
    scale: OutputScale<f64>,
    out: &mut Vec<DecorationTextureElements>,
) {
    let mut cursor_x_logical: f64 = origin_x_logical as f64;
    let cursor_y_logical: f64 = origin_y_logical as f64;
    for ch in text.chars() {
        let glyph = atlas.glyphs.get(&ch);
        let advance = glyph
            .map(|g| g.advance_logical as f64)
            .unwrap_or(atlas.fallback_advance_logical as f64);
        if let Some(glyph) = glyph {
            let physical_x = (cursor_x_logical * scale.x).round();
            let physical_y = (cursor_y_logical * scale.y).round();
            let location: Point<f64, Physical> = Point::from((physical_x, physical_y));
            if let Ok(element) = MemoryRenderBufferRenderElement::from_buffer(
                renderer,
                location,
                &glyph.buffer,
                None,
                None,
                None,
                Kind::Unspecified,
            ) {
                out.push(DecorationTextureElements::Memory(element));
            }
        }
        cursor_x_logical += advance;
    }
}

fn build_atlas() -> GlyphAtlas {
    let mut font_system = FontSystem::new();
    let mut swash_cache = SwashCache::new();

    let unique_chars: HashSet<char> = ATLAS_CHARS.chars().collect();
    let mut glyphs = HashMap::with_capacity(unique_chars.len());
    let mut max_advance: i32 = 0;
    for ch in unique_chars {
        if let Some(entry) = rasterize_glyph(&mut font_system, &mut swash_cache, ch) {
            max_advance = max_advance.max(entry.advance_logical);
            glyphs.insert(ch, entry);
        }
    }

    GlyphAtlas {
        glyphs,
        line_height_logical: LINE_HEIGHT_LOGICAL.round() as i32,
        fallback_advance_logical: max_advance.max(FONT_SIZE_LOGICAL.round() as i32 / 2),
    }
}

fn rasterize_glyph(
    font_system: &mut FontSystem,
    swash_cache: &mut SwashCache,
    ch: char,
) -> Option<GlyphEntry> {
    let font_size_px = FONT_SIZE_LOGICAL * ATLAS_RASTER_SCALE as f32;
    let line_height_px = LINE_HEIGHT_LOGICAL * ATLAS_RASTER_SCALE as f32;
    let metrics = Metrics::new(font_size_px, line_height_px.max(font_size_px));

    let attrs = Attrs::new()
        .color(CosmicColor::rgba(255, 255, 255, 255))
        .weight(CosmicWeight(600))
        .family(CosmicFamily::Monospace);

    let bbox_w = (font_size_px * 1.5).ceil() as i32;
    let bbox_h = line_height_px.ceil() as i32;

    let mut buffer = Buffer::new(font_system, metrics);
    let text = ch.to_string();
    {
        let mut b = buffer.borrow_with(font_system);
        b.set_size(Some(bbox_w as f32), Some(bbox_h as f32));
        b.set_wrap(Wrap::None);
        b.set_text(&text, &attrs, Shaping::Advanced, Some(Align::Left));
        b.shape_until_scroll(false);
    }

    // Determine the actual horizontal extent for crisp advance values: use the
    // rightmost glyph cluster position instead of bbox_w which would leave a
    // gap between every character.
    let mut max_right_px: f32 = 0.0;
    for run in buffer.layout_runs() {
        for g in run.glyphs.iter() {
            max_right_px = max_right_px.max(g.x + g.w);
        }
    }
    if max_right_px < 1.0 {
        // Whitespace and similar zero-width glyphs: fall back to a half-em.
        max_right_px = font_size_px * 0.45;
    }
    let glyph_w_px = max_right_px.ceil().max(1.0) as i32;
    let glyph_h_px = bbox_h.max(1);

    let mut pixels = vec![0u8; (glyph_w_px * glyph_h_px * 4) as usize];
    {
        let mut b = buffer.borrow_with(font_system);
        b.draw(
            swash_cache,
            CosmicColor::rgba(255, 255, 255, 255),
            |x, y, w, h, color| {
                for off_y in 0..h as i32 {
                    for off_x in 0..w as i32 {
                        let px = x + off_x;
                        let py = y + off_y;
                        if px < 0 || py < 0 || px >= glyph_w_px || py >= glyph_h_px {
                            continue;
                        }
                        blend_pixel(&mut pixels, glyph_w_px, px, py, color.as_rgba_tuple());
                    }
                }
            },
        );
    }

    let buffer = MemoryRenderBuffer::from_slice(
        &pixels,
        Fourcc::Argb8888,
        (glyph_w_px, glyph_h_px),
        ATLAS_RASTER_SCALE,
        Transform::Normal,
        None,
    );

    let advance_logical = ((glyph_w_px as f32) / ATLAS_RASTER_SCALE as f32)
        .ceil()
        .max(1.0) as i32;

    Some(GlyphEntry {
        buffer,
        advance_logical,
    })
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
