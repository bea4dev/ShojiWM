use smithay::{
    backend::renderer::{
        element::Kind,
        gles::{GlesPixelProgram, GlesRenderer, Uniform, UniformName, element::PixelShaderElement},
    },
    utils::{Logical, Rectangle},
};

#[derive(Debug, Clone, Copy)]
pub struct RoundedClip {
    pub rect: Rectangle<i32, Logical>,
    pub radius: i32,
}

#[derive(Debug, Clone, Copy)]
pub enum RoundedShapeKind {
    Fill,
    Border { width: i32 },
}

#[derive(Debug, Clone, Copy)]
pub struct RoundedRectSpec {
    pub rect: Rectangle<i32, Logical>,
    pub color: [f32; 4],
    pub radius: i32,
    pub shape: RoundedShapeKind,
    pub clip: Option<RoundedClip>,
    pub render_scale: f32,
}

#[derive(Debug)]
struct RoundedRectProgram(GlesPixelProgram);

pub fn element_for_spec(
    renderer: &mut GlesRenderer,
    spec: RoundedRectSpec,
) -> Result<PixelShaderElement, smithay::backend::renderer::gles::GlesError> {
    if renderer
        .egl_context()
        .user_data()
        .get::<RoundedRectProgram>()
        .is_none()
    {
        let compiled = RoundedRectProgram(
            renderer.compile_custom_pixel_shader(
                include_str!("rounded_rect.frag"),
                &[
                    UniformName::new("color", smithay::backend::renderer::gles::UniformType::_4f),
                    UniformName::new("corner_radius", smithay::backend::renderer::gles::UniformType::_4f),
                    UniformName::new("border_width", smithay::backend::renderer::gles::UniformType::_1f),
                    UniformName::new("render_scale", smithay::backend::renderer::gles::UniformType::_1f),
                    UniformName::new("clip_enabled", smithay::backend::renderer::gles::UniformType::_1f),
                    UniformName::new("clip_rect", smithay::backend::renderer::gles::UniformType::_4f),
                    UniformName::new("clip_radius", smithay::backend::renderer::gles::UniformType::_4f),
                ],
            )?,
        );
        renderer
            .egl_context()
            .user_data()
            .insert_if_missing(|| compiled);
    }

    let program = renderer
        .egl_context()
        .user_data()
        .get::<RoundedRectProgram>()
        .expect("rounded rect shader should be cached");

    let border_width = match spec.shape {
        RoundedShapeKind::Fill => 0.0,
        RoundedShapeKind::Border { width } => width.max(0) as f32,
    };

    let local_clip_rect = spec
        .clip
        .map(|clip| {
            [
                clip.rect.loc.x as f32,
                clip.rect.loc.y as f32,
                clip.rect.size.w as f32,
                clip.rect.size.h as f32,
            ]
        })
        .unwrap_or([0.0, 0.0, 0.0, 0.0]);
    let clip_radius = spec.clip.map(|clip| clip.radius.max(0) as f32).unwrap_or(0.0);
    let radius = spec.radius.max(0) as f32;

    Ok(PixelShaderElement::new(
        program.0.clone(),
        spec.rect,
        None,
        1.0,
        vec![
            Uniform::new("color", spec.color),
            Uniform::new("corner_radius", [radius, radius, radius, radius]),
            Uniform::new("border_width", border_width),
            Uniform::new("render_scale", spec.render_scale.max(1.0)),
            Uniform::new("clip_enabled", if spec.clip.is_some() { 1.0f32 } else { 0.0f32 }),
            Uniform::new("clip_rect", local_clip_rect),
            Uniform::new("clip_radius", [clip_radius, clip_radius, clip_radius, clip_radius]),
        ],
        Kind::Unspecified,
    ))
}
