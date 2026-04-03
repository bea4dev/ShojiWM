use smithay::{
    backend::renderer::{
        element::{
            Element, Id, Kind, RenderElement, UnderlyingStorage,
            memory::MemoryRenderBufferRenderElement,
        },
        gles::{GlesError, GlesFrame, GlesRenderer, GlesTexProgram, Uniform, UniformName},
        utils::{CommitCounter, DamageSet, OpaqueRegions},
    },
    utils::{Buffer, Physical, Rectangle, Scale, Transform},
};

use crate::backend::visual::PreciseLogicalRect;
use crate::ssd::LogicalRect;

#[derive(Debug)]
pub struct ClippedMemoryElement {
    inner: MemoryRenderBufferRenderElement<GlesRenderer>,
    program: GlesTexProgram,
    clip_rect: PreciseLogicalRect,
    clip_radius: f32,
    scale: f32,
    clip_scale_x: f32,
    clip_scale_y: f32,
}

#[derive(Debug)]
struct ClippedMemoryProgram(GlesTexProgram);

impl ClippedMemoryElement {
    pub fn new(
        renderer: &mut GlesRenderer,
        inner: MemoryRenderBufferRenderElement<GlesRenderer>,
        scale: Scale<f64>,
        element_rect: LogicalRect,
        clip_rect: LogicalRect,
        clip_radius: i32,
        element_rect_precise: Option<PreciseLogicalRect>,
        clip_rect_precise: Option<PreciseLogicalRect>,
        clip_radius_precise: Option<f32>,
    ) -> Result<Self, GlesError> {
        if renderer
            .egl_context()
            .user_data()
            .get::<ClippedMemoryProgram>()
            .is_none()
        {
            let compiled = ClippedMemoryProgram(renderer.compile_custom_texture_shader(
                include_str!("clipped_memory.frag"),
                &[
                    UniformName::new(
                        "element_size",
                        smithay::backend::renderer::gles::UniformType::_2f,
                    ),
                    UniformName::new(
                        "render_scale",
                        smithay::backend::renderer::gles::UniformType::_1f,
                    ),
                    UniformName::new(
                        "clip_enabled",
                        smithay::backend::renderer::gles::UniformType::_1f,
                    ),
                    UniformName::new(
                        "clip_rect",
                        smithay::backend::renderer::gles::UniformType::_4f,
                    ),
                    UniformName::new(
                        "clip_radius",
                        smithay::backend::renderer::gles::UniformType::_4f,
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
            .get::<ClippedMemoryProgram>()
            .expect("clipped memory shader should be cached");

        let src = inner.src();
        let clip_scale_x = if element_rect.width > 0 {
            src.size.w as f32 / element_rect.width as f32
        } else {
            1.0
        };
        let clip_scale_y = if element_rect.height > 0 {
            src.size.h as f32 / element_rect.height as f32
        } else {
            1.0
        };
        let element_rect_precise = element_rect_precise.unwrap_or(PreciseLogicalRect {
            x: element_rect.x as f32,
            y: element_rect.y as f32,
            width: element_rect.width as f32,
            height: element_rect.height as f32,
        });
        let clip_rect_precise = clip_rect_precise.unwrap_or(PreciseLogicalRect {
            x: clip_rect.x as f32,
            y: clip_rect.y as f32,
            width: clip_rect.width as f32,
            height: clip_rect.height as f32,
        });

        Ok(Self {
            inner,
            program: program.0.clone(),
            clip_rect: PreciseLogicalRect {
                x: clip_rect_precise.x - element_rect_precise.x,
                y: clip_rect_precise.y - element_rect_precise.y,
                width: clip_rect_precise.width,
                height: clip_rect_precise.height,
            },
            clip_radius: clip_radius_precise.unwrap_or(clip_radius as f32).max(0.0),
            scale: clip_scale_x.max(clip_scale_y).max(scale.x as f32),
            clip_scale_x,
            clip_scale_y,
        })
    }

    fn uniforms(&self) -> Vec<Uniform<'static>> {
        let src = self.inner.src();
        let element_width = src.size.w as f32;
        let element_height = src.size.h as f32;
        let radius = self.clip_radius.max(0.0);

        vec![
            Uniform::new("element_size", [element_width, element_height]),
            Uniform::new("render_scale", self.scale.max(0.0001)),
            Uniform::new("clip_enabled", 1.0f32),
            Uniform::new(
                "clip_rect",
                [
                    self.clip_rect.x * self.clip_scale_x,
                    self.clip_rect.y * self.clip_scale_y,
                    self.clip_rect.width * self.clip_scale_x,
                    self.clip_rect.height * self.clip_scale_y,
                ],
            ),
            Uniform::new(
                "clip_radius",
                [
                    radius * self.clip_scale_x.min(self.clip_scale_y),
                    radius * self.clip_scale_x.min(self.clip_scale_y),
                    radius * self.clip_scale_x.min(self.clip_scale_y),
                    radius * self.clip_scale_x.min(self.clip_scale_y),
                ],
            ),
        ]
    }
}

impl Element for ClippedMemoryElement {
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

    fn damage_since(
        &self,
        scale: Scale<f64>,
        commit: Option<CommitCounter>,
    ) -> DamageSet<i32, Physical> {
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

impl RenderElement<GlesRenderer> for ClippedMemoryElement {
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
