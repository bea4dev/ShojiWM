//! Two-stage HDR10 output pipeline.
//!
//! Stage 1 composites the output's full element list (windows, decorations,
//! effects, cursor, overlays) into a persistent fp16 offscreen texture with
//! its own damage tracker — the same pattern `render_output_capture_mirror`
//! uses for screencopy, so damage semantics are identical.
//!
//! Stage 2 hands the DRM pass a single [`HdrEncodeElement`] that draws the
//! intermediate through `output_encode.frag`: sRGB EOTF decode →
//! BT.709→BT.2020 gamut matrix → scale to `sdr_nits` absolute luminance →
//! ST 2084 (PQ) encode, straight into the 10-bit scanout buffer.
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
        element::{Element, Id, Kind, RenderElement},
        gles::{
            GlesError, GlesFrame, GlesRenderer, GlesTexProgram, GlesTexture, Uniform, UniformName,
            UniformType,
        },
        utils::{CommitCounter, OpaqueRegions},
    },
    output::Output,
    reexports::drm::control::ModeTypeFlags,
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
fn sdr_reference_nits() -> f32 {
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

struct HdrEncodeProgram(
    GlesTexProgram
);

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
                )
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
    commit_counter: CommitCounter,
    /// The texture holds last frame's composite (buffer age 1) once we've
    /// rendered at least once without errors.
    contents_valid: bool,
}

// ModeTypeFlags is only imported to keep rustc from flagging the drm
// reexport when the probe is compiled out in future cfg work; silence it.
#[allow(dead_code)]
fn _keep_mode_type_flags(
    _flags: ModeTypeFlags
) {}

/// Composite `elements` into the fp16 intermediate and return the single
/// PQ-encode element the DRM pass should render instead of the raw list.
/// Returns `Ok(None)` if the output has no mode yet.
pub fn render_hdr_pipeline<E>(
    renderer: &mut GlesRenderer,
    pipeline: &mut Option<HdrPipeline>,
    output: &Output,
    elements: &[E],
    clear_color: [f32; 4],
) -> Result<Option<HdrEncodeElement>, Box<dyn std::error::Error>>
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
    let size = mode.size;
    let scale: Scale<f64> = output
        .current_scale()
        .fractional_scale()
        .into();
    let transform = output
        .current_transform();

    let recreate = pipeline
        .as_ref()
        .is_none_or(|pipeline| {
        pipeline.size != size || pipeline.scale != scale || pipeline.transform != transform
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
                Fourcc::Abgr16161616f,
                buffer_size
            )?;
        *pipeline = Some(HdrPipeline {
            texture,
            damage_tracker: OutputDamageTracker::new(
                size, 
                scale,
                transform
            ),
            size,
            scale,
            transform,
            element_id: Id::new(),
            commit_counter: CommitCounter::default(),
            contents_valid: false,
        });
    }
    let pipeline = pipeline
        .as_mut()
        .expect(
            "pipeline was just created"
        );

    let program = ensure_encode_program(
        renderer
    )?;

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
    let damaged = match render_result {
        Ok(
            result
        ) => result.damage
            .is_some(),
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
    if damaged {
        pipeline.commit_counter
            .increment();
    }

    let buffer_size = size
        .to_logical(1)
        .to_buffer(
            1,
            Transform::Normal
        );
    Ok(Some(HdrEncodeElement {
        id: pipeline.element_id
            .clone(),
        commit: pipeline.commit_counter,
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
        sdr_nits: sdr_reference_nits(),
    }))
}

/// Fullscreen quad that draws the fp16 intermediate through the PQ encode
/// shader. Reports itself opaque so the DRM compositor skips the clear.
pub struct HdrEncodeElement {
    id: Id,
    commit: CommitCounter,
    texture: GlesTexture,
    program: GlesTexProgram,
    src: Rectangle<f64, Buffer>,
    geometry: Rectangle<i32, Physical>,
    sdr_nits: f32,
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
        self.commit
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
        let result = frame.render_texture_from_to(
            &self.texture,
            src,
            dst,
            damage,
            opaque_regions,
            Transform::Normal,
            1.0,
            Some(&self.program),
            &[Uniform::new("sdr_nits", self.sdr_nits)],
        );
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
