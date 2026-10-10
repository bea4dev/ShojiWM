//! Hardware cursor on HDR10 outputs.
//!
//! Everything on an HDR output normally goes through the fp16 intermediate and
//! the PQ encode pass (`hdr_pipeline`), and the frame flags forbid every plane:
//! an sRGB cursor image on the cursor plane would land, unconverted, in a PQ
//! signal. That makes the cursor a software one, which costs the deadline
//! commit fast path (see `try_fast_cursor_move`) and re-encodes part of the
//! screen on every pointer move.
//!
//! Instead the cursor image is run through the same encode on the CPU, once per
//! image and set of encode parameters, and the PQ pixels go on the cursor plane
//! as an ordinary `Kind::Cursor` memory element. The display controller blends
//! planes on the signal as it is, so the cursor arrives already encoded, like
//! the primary plane. The only difference from the software cursor is that the
//! antialiased edge is blended in PQ instead of the SDR gamma, which is not
//! visible at cursor sizes.
//!
//! If the plane cannot take the element, the DRM pass draws it with GL on top
//! of the encoded frame, which is still correct.

use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            ImportMem,
            element::{
                Element, Id, Kind, RenderElement, UnderlyingStorage, memory::MemoryBuffer,
            },
            gles::{GlesError, GlesFrame, GlesRenderer, GlesTexture},
            utils::CommitCounter,
        },
    },
    reexports::wayland_server::protocol::wl_shm,
    utils::{Buffer, Physical, Rectangle, Scale, Transform, user_data::UserDataMap},
    wayland::shm,
};

use crate::{backend::hdr_pipeline::EncodeParams, drawing::PointerRenderElement};

/// Converted images kept per output. Animated theme cursors cycle through one
/// buffer per frame, so this holds a whole animation of a typical theme.
const CACHE_LIMIT: usize = 32;

struct CacheEntry {
    source: (Id, CommitCounter),
    params: EncodeParams,
    id: Id,
    memory: MemoryBuffer,
    texture: GlesTexture,
    last_used: u64,
}

/// Per-output cache of PQ-encoded cursor images, keyed by the source element's
/// id and commit and the encode parameters.
#[derive(Default)]
pub struct HdrCursorCache {
    entries: Vec<CacheEntry>,
    generation: u64,
}

impl HdrCursorCache {
    /// PQ-encoded replacements for `cursor`, or `None` when it cannot be
    /// converted (no CPU-readable pixels, an unsupported format, or more than
    /// one element: the fast path moves only the plane, so a cursor surface
    /// with subsurfaces must stay in the composite).
    pub fn convert(
        &mut self,
        renderer: &mut GlesRenderer,
        cursor: &[&PointerRenderElement<GlesRenderer>],
        scale: Scale<f64>,
        params: EncodeParams,
    ) -> Option<Vec<PqCursorElement>> {
        let [element] = cursor else {
            return None;
        };
        self.generation += 1;
        let source = (element.id().clone(), element.current_commit());
        let index = match self
            .entries
            .iter()
            .position(|entry| entry.source == source && entry.params == params)
        {
            Some(index) => index,
            None => {
                let memory = encode_storage(element.underlying_storage(renderer)?, &params)?;
                let texture = renderer
                    .import_memory(&memory, memory.format(), memory.size(), false)
                    .ok()?;
                if self.entries.len() >= CACHE_LIMIT
                    && let Some(oldest) = self
                        .entries
                        .iter()
                        .enumerate()
                        .min_by_key(|(_, entry)| entry.last_used)
                        .map(|(index, _)| index)
                {
                    self.entries.swap_remove(oldest);
                }
                self.entries.push(CacheEntry {
                    source,
                    params,
                    id: Id::new(),
                    memory,
                    texture,
                    last_used: 0,
                });
                self.entries.len() - 1
            }
        };
        let entry = &mut self.entries[index];
        entry.last_used = self.generation;
        Some(vec![PqCursorElement {
            id: entry.id.clone(),
            memory: entry.memory.clone(),
            texture: entry.texture.clone(),
            src: element.src(),
            geometry: element.geometry(scale),
            transform: element.transform(),
            alpha: element.alpha(),
        }])
    }
}

/// A PQ-encoded cursor image placed exactly where the source element was.
pub struct PqCursorElement {
    id: Id,
    memory: MemoryBuffer,
    texture: GlesTexture,
    src: Rectangle<f64, Buffer>,
    geometry: Rectangle<i32, Physical>,
    transform: Transform,
    alpha: f32,
}

impl Element for PqCursorElement {
    fn id(&self) -> &Id {
        &self.id
    }

    fn current_commit(&self) -> CommitCounter {
        // Each converted image gets its own id, so the content behind one never
        // changes.
        CommitCounter::default()
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        self.src
    }

    fn transform(&self) -> Transform {
        self.transform
    }

    fn geometry(&self, _scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.geometry
    }

    fn alpha(&self) -> f32 {
        self.alpha
    }

    fn kind(&self) -> Kind {
        Kind::Cursor
    }
}

impl RenderElement<GlesRenderer> for PqCursorElement {
    fn draw(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
        _cache: Option<&UserDataMap>,
    ) -> Result<(), GlesError> {
        frame.render_texture_from_to(
            &self.texture,
            src,
            dst,
            damage,
            opaque_regions,
            self.transform,
            self.alpha,
            None,
            &[],
        )
    }

    fn underlying_storage(&self, _renderer: &mut GlesRenderer) -> Option<UnderlyingStorage<'_>> {
        Some(UnderlyingStorage::Memory(&self.memory))
    }
}

/// Byte offsets of red, green, blue and (if any) alpha in a 4-byte pixel.
fn channel_layout(format: Fourcc) -> Option<([usize; 3], Option<usize>)> {
    // DRM fourccs name the channels of a little-endian 32-bit word from the
    // high end: ARGB8888 is the bytes B, G, R, A.
    match format {
        Fourcc::Argb8888 => Some(([2, 1, 0], Some(3))),
        Fourcc::Xrgb8888 => Some(([2, 1, 0], None)),
        Fourcc::Abgr8888 => Some(([0, 1, 2], Some(3))),
        Fourcc::Xbgr8888 => Some(([0, 1, 2], None)),
        _ => None,
    }
}

fn encode_storage(storage: UnderlyingStorage<'_>, params: &EncodeParams) -> Option<MemoryBuffer> {
    match storage {
        UnderlyingStorage::Memory(memory) => encode_pixels(
            memory,
            memory.format(),
            memory.size().w,
            memory.size().h,
            memory.stride(),
            params,
        ),
        UnderlyingStorage::Wayland(buffer) => shm::with_buffer_contents(buffer, |ptr, len, data| {
            let format = match data.format {
                wl_shm::Format::Argb8888 => Fourcc::Argb8888,
                wl_shm::Format::Xrgb8888 => Fourcc::Xrgb8888,
                other => shm::shm_format_to_fourcc(other)?,
            };
            let start = data.offset as usize;
            let end = start + (data.stride as usize) * (data.height as usize);
            if data.offset < 0 || data.stride < 0 || data.height < 0 || end > len {
                return None;
            }
            // SAFETY: smithay maps the pool for the duration of the callback
            // and `end` was checked against its length.
            let pixels = unsafe { std::slice::from_raw_parts(ptr.add(start), end - start) };
            encode_pixels(pixels, format, data.width, data.height, data.stride, params)
        })
        .ok()
        .flatten(),
    }
}

/// Run premultiplied 8-bit pixels through the encode pass of
/// `output_encode.frag` and return them premultiplied as ARGB8888, the cursor
/// plane's format.
fn encode_pixels(
    pixels: &[u8],
    format: Fourcc,
    width: i32,
    height: i32,
    stride: i32,
    params: &EncodeParams,
) -> Option<MemoryBuffer> {
    let (rgb, alpha) = channel_layout(format)?;
    if width <= 0 || height <= 0 || stride < width * 4 {
        return None;
    }
    if pixels.len() < (stride as usize) * (height as usize - 1) + (width as usize) * 4 {
        return None;
    }
    let mut out = vec![0u8; (width * height * 4) as usize];
    for y in 0..height as usize {
        let row = &pixels[y * stride as usize..];
        for x in 0..width as usize {
            let pixel = &row[x * 4..x * 4 + 4];
            let a = alpha.map_or(255, |index| pixel[index]);
            if a == 0 {
                continue;
            }
            let premultiplied = rgb.map(|index| pixel[index]);
            let [r, g, b] = encode_pixel(premultiplied, a, params);
            let dst = &mut out[(y * width as usize + x) * 4..][..4];
            dst.copy_from_slice(&[b, g, r, a]);
        }
    }
    Some(MemoryBuffer::from_slice(&out, Fourcc::Argb8888, (width, height)))
}

/// One premultiplied pixel through the encode pass: un-premultiply, SDR EOTF,
/// compositing primaries -> BT.2020, scale to SDR white and clamp at the peak,
/// PQ, premultiply again.
pub(crate) fn encode_pixel(premultiplied: [u8; 3], alpha: u8, params: &EncodeParams) -> [u8; 3] {
    let a = alpha as f32 / 255.0;
    let linear = premultiplied.map(|value| {
        let encoded = (value as f32 / 255.0 / a).min(1.0);
        encoded.powf(params.sdr_gamma)
    });
    let m = &params.compositing_to_bt2020;
    let mut out = [0u8; 3];
    for (row, out) in out.iter_mut().enumerate() {
        // Column-major, as uploaded to the shader.
        let bt2020 = m[row] * linear[0] + m[3 + row] * linear[1] + m[6 + row] * linear[2];
        let nits = (bt2020.max(0.0) * params.sdr_nits).min(params.peak_nits);
        *out = (pq_inv_eotf(nits) * a * 255.0).round().clamp(0.0, 255.0) as u8;
    }
    out
}

/// SMPTE ST 2084 inverse EOTF, as in `output_encode.frag`.
fn pq_inv_eotf(nits: f32) -> f32 {
    const M1: f32 = 0.159_301_76;
    const M2: f32 = 78.843_75;
    const C1: f32 = 0.835_937_5;
    const C2: f32 = 18.851_563;
    const C3: f32 = 18.6875;
    let y = (nits / 10000.0).clamp(0.0, 1.0);
    let ym = y.powf(M1);
    ((C1 + C2 * ym) / (1.0 + C3 * ym)).powf(M2)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params() -> EncodeParams {
        EncodeParams {
            sdr_nits: 203.0,
            sdr_gamma: 2.2,
            peak_nits: 1000.0,
            compositing_to_bt2020: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0],
        }
    }

    #[test]
    fn opaque_white_lands_on_sdr_white() {
        // PQ(203 cd/m²) = 0.5806.
        assert_eq!(encode_pixel([255, 255, 255], 255, &params()), [148, 148, 148]);
    }

    #[test]
    fn black_and_transparent_stay_zero() {
        assert_eq!(encode_pixel([0, 0, 0], 255, &params()), [0, 0, 0]);
        let memory = encode_pixels(&[0, 0, 0, 0], Fourcc::Argb8888, 1, 1, 4, &params()).unwrap();
        assert_eq!(&memory[..], &[0, 0, 0, 0]);
    }

    #[test]
    fn translucent_pixels_are_encoded_unpremultiplied_and_premultiplied_again() {
        // Premultiplied half-alpha white is white at half coverage.
        assert_eq!(encode_pixel([128, 128, 128], 128, &params()), [74, 74, 74]);
    }

    #[test]
    fn pixels_come_out_as_argb8888_with_the_matrix_applied() {
        // ABGR8888 bytes R, G, B, A: pure red. The matrix sends red to green
        // only, so the encoded pixel is green.
        let mut params = params();
        params.compositing_to_bt2020 = [0.0, 1.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0];
        let memory = encode_pixels(&[255, 0, 0, 255], Fourcc::Abgr8888, 1, 1, 4, &params).unwrap();
        assert_eq!(memory.format(), Fourcc::Argb8888);
        assert_eq!(&memory[..], &[0, 148, 0, 255]);
    }

    #[test]
    fn rows_respect_the_source_stride() {
        // Two 1-pixel rows with 4 bytes of padding each.
        let pixels = [255, 255, 255, 255, 9, 9, 9, 9, 0, 0, 0, 255, 9, 9, 9, 9];
        let memory = encode_pixels(&pixels, Fourcc::Argb8888, 1, 2, 8, &params()).unwrap();
        assert_eq!(&memory[..], &[148, 148, 148, 255, 0, 0, 0, 255]);
    }
}
