use cgmath::{ElementWise, Matrix3, Vector2};
use smithay::{
    backend::renderer::{
        element::{Element, Id, Kind, RenderElement, UnderlyingStorage, surface::WaylandSurfaceRenderElement},
        gles::{GlesError, GlesFrame, GlesRenderer, GlesTexProgram, Uniform, UniformName},
        utils::{CommitCounter, DamageSet, OpaqueRegions},
    },
    utils::{Buffer, Physical, Rectangle, Scale, Transform},
};

use crate::ssd::ContentClip;

#[derive(Debug)]
pub struct ClippedSurfaceElement {
    inner: WaylandSurfaceRenderElement<GlesRenderer>,
    program: GlesTexProgram,
    clip: ContentClip,
    scale: f32,
}

#[derive(Debug)]
struct ClippedSurfaceProgram(GlesTexProgram);

impl ClippedSurfaceElement {
    pub fn new(
        renderer: &mut GlesRenderer,
        inner: WaylandSurfaceRenderElement<GlesRenderer>,
        scale: Scale<f64>,
        clip: ContentClip,
    ) -> Result<Self, GlesError> {
        if renderer
            .egl_context()
            .user_data()
            .get::<ClippedSurfaceProgram>()
            .is_none()
        {
            let compiled = ClippedSurfaceProgram(renderer.compile_custom_texture_shader(
                include_str!("clipped_surface.frag"),
                &[
                    UniformName::new("clip_scale", smithay::backend::renderer::gles::UniformType::_1f),
                    UniformName::new("clip_size", smithay::backend::renderer::gles::UniformType::_2f),
                    UniformName::new("corner_radius", smithay::backend::renderer::gles::UniformType::_4f),
                    UniformName::new(
                        "input_to_clip",
                        smithay::backend::renderer::gles::UniformType::Matrix3x3,
                    ),
                ],
            )?);
            renderer
                .egl_context()
                .user_data()
                .insert_if_missing(|| compiled);
        }

        let program = renderer
            .egl_context()
            .user_data()
            .get::<ClippedSurfaceProgram>()
            .expect("clipped surface shader should be cached");

        Ok(Self {
            inner,
            program: program.0.clone(),
            clip,
            scale: scale.x as f32,
        })
    }

    fn uniforms(&self) -> Vec<Uniform<'static>> {
        let scale = Scale::from(self.scale as f64);
        let element_geometry = self.inner.geometry(scale);
        let clip_geometry: Rectangle<i32, Physical> = self.clip.rect.to_physical_precise_round(scale);

        let element_loc = Vector2::new(element_geometry.loc.x as f32, element_geometry.loc.y as f32);
        let element_size = Vector2::new(element_geometry.size.w as f32, element_geometry.size.h as f32);

        let clip_loc = Vector2::new(clip_geometry.loc.x as f32, clip_geometry.loc.y as f32);
        let clip_size = Vector2::new(clip_geometry.size.w as f32, clip_geometry.size.h as f32);

        let buffer_size = self.inner.buffer_size();
        let buffer_size = Vector2::new(buffer_size.w as f32, buffer_size.h as f32);

        let view = self.inner.view();
        let src_loc = Vector2::new(view.src.loc.x as f32, view.src.loc.y as f32);
        let src_size = Vector2::new(view.src.size.w as f32, view.src.size.h as f32);

        let transform = match self.inner.transform() {
            Transform::_90 => Transform::_270,
            Transform::_270 => Transform::_90,
            other => other,
        };

        let tm = transform.matrix();
        let transform_matrix = Matrix3::from_translation(Vector2::new(0.5, 0.5))
            * Matrix3::from_cols(tm[0], tm[1], tm[2])
            * Matrix3::from_translation(Vector2::new(-0.5, -0.5));

        let input_to_clip = transform_matrix
            * Matrix3::from_nonuniform_scale(element_size.x / clip_size.x, element_size.y / clip_size.y)
            * Matrix3::from_translation((element_loc - clip_loc).div_element_wise(element_size))
            * Matrix3::from_nonuniform_scale(buffer_size.x / src_size.x, buffer_size.y / src_size.y)
            * Matrix3::from_translation(-src_loc.div_element_wise(buffer_size));

        let radius = self.clip.radius.max(0) as f32;
        let input_to_clip_array = [
            input_to_clip.x.x,
            input_to_clip.x.y,
            input_to_clip.x.z,
            input_to_clip.y.x,
            input_to_clip.y.y,
            input_to_clip.y.z,
            input_to_clip.z.x,
            input_to_clip.z.y,
            input_to_clip.z.z,
        ];

        vec![
            Uniform::new("clip_scale", self.scale),
            Uniform::new("clip_size", [clip_size.x, clip_size.y]),
            Uniform::new("corner_radius", [radius, radius, radius, radius]),
            Uniform::new(
                "input_to_clip",
                smithay::backend::renderer::gles::UniformValue::Matrix3x3 {
                    matrices: vec![input_to_clip_array],
                    transpose: false,
                },
            ),
        ]
    }
}

impl Element for ClippedSurfaceElement {
    fn id(&self) -> &Id {
        self.inner.id()
    }

    fn current_commit(&self) -> CommitCounter {
        self.inner.current_commit()
    }

    fn geometry(&self, scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.inner.geometry(scale)
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        self.inner.src()
    }

    fn transform(&self) -> Transform {
        self.inner.transform()
    }

    fn damage_since(&self, scale: Scale<f64>, commit: Option<CommitCounter>) -> DamageSet<i32, Physical> {
        self.inner.damage_since(scale, commit)
    }

    fn opaque_regions(&self, _scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        OpaqueRegions::default()
    }

    fn alpha(&self) -> f32 {
        self.inner.alpha()
    }

    fn kind(&self) -> Kind {
        self.inner.kind()
    }
}

impl RenderElement<GlesRenderer> for ClippedSurfaceElement {
    fn draw(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
        cache: Option<&smithay::utils::user_data::UserDataMap>,
    ) -> Result<(), GlesError> {
        frame.override_default_tex_program(self.program.clone(), self.uniforms());
        let result = RenderElement::<GlesRenderer>::draw(
            &self.inner,
            frame,
            src,
            dst,
            damage,
            opaque_regions,
            cache,
        );
        frame.clear_tex_program_override();
        result
    }

    fn underlying_storage(&self, _renderer: &mut GlesRenderer) -> Option<UnderlyingStorage<'_>> {
        None
    }
}
