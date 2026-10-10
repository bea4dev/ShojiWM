//! Two-stage HDR10 output pipeline.
//!
//! Stage 1 composites the output's element list (windows, decorations,
//! effects, overlays; the cursor too unless it goes on the cursor plane, see
//! `hdr_cursor`) into a persistent fp16 offscreen texture with its own damage
//! tracker — the same pattern `render_output_capture_mirror` uses for
//! screencopy, so damage semantics are identical.
//!
//! Stage 2 hands the DRM pass a single [`HdrEncodeElement`] that draws the
//! intermediate through `output_encode.frag`: SDR EOTF decode (pure gamma,
//! not the piecewise sRGB curve — see the shader) →
//! BT.709→BT.2020 gamut matrix → scale to `sdr_nits` absolute luminance →
//! ST 2084 (PQ) encode, straight into the 10-bit scanout buffer. The element
//! carries stage 1's damage, so only what changed is re-encoded.
//!
//! The same two stages serve SDR outputs with a monitor ICC profile: the
//! intermediate is then run through the profile's 3D LUT (`output_icc.frag`,
//! `color::icc`) instead of the PQ encode. [`OutputEncoding`] picks the pass.
//!
//! Compositing itself still happens on sRGB-encoded values: per-element
//! linearization needs sRGB texture views across every draw program and is
//! deliberately out of scope here. The fp16 intermediate exists so PQ-tagged
//! client content (which exceeds the SDR range once decoded) has headroom
//! when that lands.

use smithay::{
    backend::renderer::{
        Bind, Color32F, Offscreen,
        damage::OutputDamageTracker,
        element::{Element, Id, Kind, RenderElement, RenderElementStates},
        ImportMem,
        gles::{
            GlesError, GlesFrame, GlesRenderer, GlesTexProgram, GlesTexture, Uniform, UniformName,
            UniformType, UniformValue, ffi,
        },
        utils::{CommitCounter, DamageBag, DamageSet, DamageSnapshot, OpaqueRegions},
    },
    output::Output,
    utils::{Buffer, Physical, Rectangle, Scale, Size, Transform, user_data::UserDataMap},
};
use tracing::{info, warn};

use smithay::backend::allocator::Fourcc;

/// Probe whether the GL context can render into fp16 (RGBA16F) targets.
/// Smithay only gates the texture allocation on GLES 3.0; actual
/// renderability additionally needs GL_EXT_color_buffer_half_float, which
/// surfaces as a framebuffer-completeness failure on bind.
pub fn probe_fp16_render_support(
    renderer: &mut GlesRenderer
) -> bool {
    let size = Size::<i32, Buffer>::from(
        (
            16, 
            16,
        )
    );
    match Offscreen::<GlesTexture>::create_buffer(
        renderer, 
        Fourcc::Abgr16161616f,
        size
    ) {
        Ok(
            mut texture
        ) => match renderer
            .bind(
                &mut texture
            ) {
            Ok(_) => true,
            Err(
                error
            ) => {
                info!(
                    ?error, 
                    "fp16 render targets unsupported (bind failed)"
                );
                false
            }
        },
        Err(
            error
        ) => {
            info!(
                ?error, 
                "fp16 render targets unsupported (alloc failed)"
            );
            false
        }
    }
}

/// Luminance that sRGB full white maps to on the PQ signal (cd/m²).
/// ITU-R BT.2408 reference white by default; `SHOJI_SDR_NITS` overrides
/// for taste/testing.
pub(crate) fn sdr_reference_nits() -> f32 {
    static NITS: std::sync::OnceLock<f32> = std::sync::OnceLock::new();
    *NITS.get_or_init(|| {
        std::env::var(
            "SHOJI_SDR_NITS"
        )
            .ok()
            .and_then(|value| value
                .trim()
                .parse::<f32>()
                .ok())
            .filter(|nits| (10.0..=1000.0)
                .contains(
                    nits
                ))
            .unwrap_or(
                203.0
            )
    })
}

/// Display gamma assumed for SDR content when encoding it to PQ, from
/// `SHOJI_SDR_GAMMA`, default 2.2.
///
/// 2.2 is sRGB's nominal display gamma. 2.4 is the BT.1886 figure for a dim
/// viewing environment and is the broadcast convention for television, so it is
/// worth trying on a TV — it takes the first code off black from PQ 6.6 down to
/// 3.5, against 51.8 under the piecewise sRGB curve this replaced.
///
/// Clamped to a sane range: below about 1.8 shadows lift again, and above 3.0
/// they crush to nothing.
pub(crate) fn sdr_reference_gamma() -> f32 {
    static GAMMA: std::sync::OnceLock<f32> = std::sync::OnceLock::new();
    *GAMMA.get_or_init(|| {
        std::env::var("SHOJI_SDR_GAMMA")
            .ok()
            .and_then(|value| value.trim().parse::<f32>().ok())
            .filter(|gamma| (1.8..=3.0).contains(gamma))
            .unwrap_or(2.2)
    })
}

/// Everything the PQ encode depends on besides the intermediate's pixels.
/// Shared by the encode pass and the hardware cursor (`hdr_cursor`), which
/// has to reproduce the pass on the CPU.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EncodeParams {
    pub sdr_nits: f32,
    pub sdr_gamma: f32,
    pub peak_nits: f32,
    /// Compositing primaries -> BT.2020, column-major.
    pub compositing_to_bt2020: [f32; 9],
}

impl EncodeParams {
    pub fn new(
        sdr_white_nits: f32,
        peak_nits: f32,
        compositing_primaries: crate::color::primaries::PrimariesChromaticities,
    ) -> Self {
        Self {
            sdr_nits: sdr_white_nits,
            sdr_gamma: sdr_reference_gamma(),
            peak_nits,
            compositing_to_bt2020: crate::color::colorimetry::to_gl_mat3(
                &crate::color::colorimetry::chromaticity_conversion_matrix(
                    compositing_primaries,
                    crate::color::primaries::BT2020,
                ),
            ),
        }
    }
}

/// What the final pass does with the intermediate.
#[derive(Clone, Debug, PartialEq)]
pub enum OutputEncoding {
    /// HDR10: PQ/BT.2020 (`output_encode.frag`).
    Pq(EncodeParams),
    /// SDR through a monitor ICC profile's LUT (`output_icc.frag`).
    Icc(std::sync::Arc<crate::color::icc::IccLut>),
}

/// Damage commits kept for the encode element. The DRM swapchain asks for
/// damage since the commit it last showed in a buffer, at most a few frames
/// back; anything older gets a full re-encode.
const ENCODE_DAMAGE_HISTORY: usize = 8;

struct HdrEncodeProgram(
    GlesTexProgram
);

struct IccEncodeProgram(GlesTexProgram);

/// Texture unit the ICC LUT is bound to; the intermediate is on unit 0.
const ICC_LUT_UNIT: i32 = 1;

fn ensure_icc_program(renderer: &mut GlesRenderer) -> Result<GlesTexProgram, GlesError> {
    if renderer
        .egl_context()
        .user_data()
        .get::<IccEncodeProgram>()
        .is_none()
    {
        let program = renderer.compile_custom_texture_shader(
            include_str!("output_icc.frag"),
            &[
                UniformName::new("lut", UniformType::_1i),
                UniformName::new("lut_size", UniformType::_1f),
            ],
        )?;
        renderer
            .egl_context()
            .user_data()
            .insert_if_missing(|| IccEncodeProgram(program));
    }
    Ok(renderer
        .egl_context()
        .user_data()
        .get::<IccEncodeProgram>()
        .unwrap()
        .0
        .clone())
}

fn ensure_encode_program(
    renderer: &mut GlesRenderer
) -> Result<GlesTexProgram, GlesError> {
    if renderer
        .egl_context()
        .user_data()
        .get::<HdrEncodeProgram>()
        .is_none()
    {
        let program = renderer.compile_custom_texture_shader(
            include_str!(
                "output_encode.frag"
            ),
            &[
                UniformName::new(
                    "sdr_nits", 
                    UniformType::_1f
                ),
                UniformName::new(
                    "sdr_gamma",
                    UniformType::_1f
                ),
                UniformName::new(
                    "peak_nits",
                    UniformType::_1f
                ),
                UniformName::new(
                    "compositing_to_bt2020",
                    UniformType::Matrix3x3
                ),
            ],
        )?;
        renderer
            .egl_context()
            .user_data()
            .insert_if_missing(
                || HdrEncodeProgram(
                    program
                )
            );
    }
    Ok(renderer
        .egl_context()
        .user_data()
        .get::<HdrEncodeProgram>()
        .unwrap()
        .0
        .clone())
}

/// Per-output HDR pipeline state, keyed by output name in
/// `ShojiWM::hdr_pipelines` (mirroring `output_capture_mirrors`).
pub struct HdrPipeline {
    texture: GlesTexture,
    damage_tracker: OutputDamageTracker,
    size: Size<i32, Physical>,
    scale: Scale<f64>,
    transform: Transform,
    /// Stable element id so the DRM damage tracker sees one persistent
    /// element instead of a brand-new fullscreen quad every frame.
    element_id: Id,
    /// Stage-1 damage per commit, so the DRM pass re-encodes only what
    /// changed instead of the whole output.
    damage: DamageBag<i32, Physical>,
    /// Format of `texture`: fp16 for HDR, whatever the GPU renders for ICC.
    format: Fourcc,
    /// Encoding the last frame went through; a change re-encodes everything.
    encoding: Option<OutputEncoding>,
    /// The ICC LUT uploaded for this renderer, by `IccLut::id`.
    icc_lut: Option<(u64, GlesTexture)>,
    /// The texture holds last frame's composite (buffer age 1) once we've
    /// rendered at least once without errors.
    contents_valid: bool,
}

/// Composite `elements` into the fp16 intermediate and return the single
/// PQ-encode element the DRM pass should render instead of the raw list,
/// together with the `RenderElementStates` stage 1 produced for the *real*
/// element list.
///
/// Those states are load-bearing, not diagnostic. The DRM pass only ever sees
/// the one synthetic encode element, so the states it returns name no client
/// `wl_surface` at all. If the caller does not merge these back in,
/// `update_primary_scanout_output` clears every client's primary scanout
/// output on every frame, which collapses their frame callbacks to the 1s idle
/// throttle and stops `wp_presentation` feedback -- a video client is then
/// never told when to present and judders. See the merge in `render_surface`.
///
/// Returns `Ok(None)` if the output has no mode yet.
pub fn render_hdr_pipeline<E>(
    renderer: &mut GlesRenderer,
    pipeline: &mut Option<HdrPipeline>,
    output: &Output,
    elements: &[E],
    clear_color: [f32; 4],
    encoding: OutputEncoding,
    format: Fourcc,
) -> Result<Option<(HdrEncodeElement, RenderElementStates)>, Box<dyn std::error::Error>>
where
    E: RenderElement<GlesRenderer>,
{
    let Some(
        mode
    ) = output
        .current_mode() else {
        return Ok(
            None
        );
    };
    let scale: Scale<f64> = output
        .current_scale()
        .fractional_scale()
        .into();
    let transform = output
        .current_transform();
    // The intermediate is the *upright* scene, like the capture mirror: it is
    // rendered with Transform::Normal at the transformed-orientation size, and
    // the DRM pass applies the output transform once when it draws the encode
    // element. Rendering it with the output transform as well turned rotated
    // and flipped outputs twice.
    let size = transform.transform_size(mode.size);

    let recreate = pipeline
        .as_ref()
        .is_none_or(|pipeline| {
        pipeline.size != size
            || pipeline.scale != scale
            || pipeline.transform != transform
            || pipeline.format != format
    });
    if recreate {
        let buffer_size = size
            .to_logical(1)
            .to_buffer(
                1,
                Transform::Normal
            );
        let texture =
            Offscreen::<GlesTexture>::create_buffer(
                renderer,
                format,
                buffer_size
            )?;
        *pipeline = Some(HdrPipeline {
            texture,
            damage_tracker: OutputDamageTracker::new(
                size,
                scale,
                Transform::Normal
            ),
            size,
            scale,
            transform,
            element_id: Id::new(),
            damage: DamageBag::new(ENCODE_DAMAGE_HISTORY),
            format,
            encoding: None,
            icc_lut: None,
            contents_valid: false,
        });
    }
    let pipeline = pipeline
        .as_mut()
        .expect(
            "pipeline was just created"
        );

    let (program, lut) = match &encoding {
        OutputEncoding::Pq(_) => (ensure_encode_program(renderer)?, None),
        OutputEncoding::Icc(icc) => {
            if pipeline
                .icc_lut
                .as_ref()
                .is_none_or(|(id, _)| *id != icc.id)
            {
                let size = crate::color::icc::LUT_SIZE as i32;
                let texture = renderer.import_memory(
                    &icc.packed_2101010(),
                    Fourcc::Abgr2101010,
                    (size * size, size).into(),
                    false,
                )?;
                pipeline.icc_lut = Some((icc.id, texture));
            }
            (
                ensure_icc_program(renderer)?,
                pipeline.icc_lut.as_ref().map(|(_, texture)| texture.clone()),
            )
        }
    };

    // Stage 1: composite into the fp16 intermediate. Age 1 keeps partial
    // redraws once the texture holds the previous frame.
    let age = if pipeline.contents_valid { 1 } else { 0 };
    let render_result = {
        let mut target = renderer.bind(
            &mut pipeline.texture
        )?;
        pipeline.damage_tracker
            .render_output(
                renderer, 
                &mut target, 
                age, 
                elements, 
                Color32F::new(
                    clear_color[0], 
                    clear_color[1], 
                    clear_color[2], 
                    clear_color[3], 
                ), 
            )
    };
    let (damage, stage1_states) = match render_result {
        Ok(
            result
        ) => (
            result.damage
                .cloned(),
            result.states,
        ),
        Err(
            error
        ) => {
            pipeline.contents_valid = false;
            return Err(
                Box::new(
                    error
                )
            );
        }
    };
    pipeline.contents_valid = true;
    // The intermediate is upright at the origin, exactly like the encode
    // element's geometry, so its damage is the element's damage as is.
    if pipeline.encoding.as_ref() != Some(&encoding) {
        pipeline.encoding = Some(encoding.clone());
        pipeline.damage.add([Rectangle::from_size(size)]);
    } else if let Some(damage) = damage
        && !damage.is_empty()
    {
        pipeline.damage.add(damage);
    }

    let buffer_size = size
        .to_logical(1)
        .to_buffer(
            1,
            Transform::Normal
        );
    Ok(Some((HdrEncodeElement {
        id: pipeline.element_id
            .clone(),
        damage: pipeline.damage.snapshot(),
        texture: pipeline.texture
            .clone(),
        program,
        src: Rectangle::from_size(
            (
                buffer_size.w as f64, 
                buffer_size.h as f64
            ).into()
        ),
        geometry: Rectangle::from_size(
            size
        ),
        encoding,
        lut,
    }, stage1_states)))
}

/// Fullscreen quad that draws the fp16 intermediate through the PQ encode
/// shader. Reports itself opaque so the DRM compositor skips the clear.
pub struct HdrEncodeElement {
    id: Id,
    damage: DamageSnapshot<i32, Physical>,
    texture: GlesTexture,
    program: GlesTexProgram,
    src: Rectangle<f64, Buffer>,
    geometry: Rectangle<i32, Physical>,
    encoding: OutputEncoding,
    /// The ICC LUT texture, for `OutputEncoding::Icc`.
    lut: Option<GlesTexture>,
}

impl Element for HdrEncodeElement {
    fn id(
        &self
    ) -> &Id {
        &self.id
    }

    fn current_commit(
        &self
    ) -> CommitCounter {
        self.damage.current_commit()
    }

    fn damage_since(
        &self,
        _scale: Scale<f64>,
        commit: Option<CommitCounter>,
    ) -> DamageSet<i32, Physical> {
        self.damage
            .damage_since(commit)
            .unwrap_or_else(|| DamageSet::from_slice(&[Rectangle::from_size(self.geometry.size)]))
    }

    fn src(
        &self
    ) -> Rectangle<f64, Buffer> {
        self.src
    }

    fn geometry(
        &self, 
        _scale: Scale<f64>
    ) -> Rectangle<i32, Physical> {
        self.geometry
    }

    fn opaque_regions(
        &self,
        _scale: Scale<f64>
    ) -> OpaqueRegions<i32, Physical> {
        OpaqueRegions::from_slice(
            &[Rectangle::from_size(
                self.geometry.size
            )]
        )
    }

    fn alpha(
        &self
    ) -> f32 {
        1.0
    }

    fn kind(
        &self
    ) -> Kind {
        Kind::Unspecified
    }
}

impl RenderElement<GlesRenderer> for HdrEncodeElement {
    fn draw(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
        _cache: Option<&UserDataMap>,
    ) -> Result<(), GlesError> {
        let uniforms = match &self.encoding {
            OutputEncoding::Pq(params) => vec![
                Uniform::new("sdr_nits", params.sdr_nits),
                Uniform::new("sdr_gamma", params.sdr_gamma),
                Uniform::new("peak_nits", params.peak_nits),
                Uniform::new(
                    "compositing_to_bt2020",
                    UniformValue::Matrix3x3 {
                        matrices: vec![params.compositing_to_bt2020],
                        transpose: false,
                    },
                ),
            ],
            OutputEncoding::Icc(_) => vec![
                Uniform::new("lut", ICC_LUT_UNIT),
                Uniform::new("lut_size", crate::color::icc::LUT_SIZE as f32),
            ],
        };
        // The program only binds `tex`; the LUT goes on its own unit for the
        // duration of the draw.
        if let Some(lut) = &self.lut {
            let lut_id = lut.tex_id();
            frame.with_context(|gl| unsafe {
                gl.ActiveTexture(ffi::TEXTURE0 + ICC_LUT_UNIT as u32);
                gl.BindTexture(ffi::TEXTURE_2D, lut_id);
                gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_MIN_FILTER, ffi::LINEAR as i32);
                gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_MAG_FILTER, ffi::LINEAR as i32);
                gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_WRAP_S, ffi::CLAMP_TO_EDGE as i32);
                gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_WRAP_T, ffi::CLAMP_TO_EDGE as i32);
                gl.ActiveTexture(ffi::TEXTURE0);
            })?;
        }
        let result = frame.render_texture_from_to(
            &self.texture,
            src,
            dst,
            damage,
            opaque_regions,
            Transform::Normal,
            1.0,
            Some(&self.program),
            &uniforms,
        );
        if self.lut.is_some() {
            frame.with_context(|gl| unsafe {
                gl.ActiveTexture(ffi::TEXTURE0 + ICC_LUT_UNIT as u32);
                gl.BindTexture(ffi::TEXTURE_2D, 0);
                gl.ActiveTexture(ffi::TEXTURE0);
            })?;
        }
        if let Err(
            error
        ) = &result {
            warn!(
                ?error,
                "HDR encode pass draw failed"
            );
        }
        result
    }
}
