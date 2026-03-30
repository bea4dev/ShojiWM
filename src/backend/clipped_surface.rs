use cgmath::{ElementWise, Matrix3, Vector2};
use smithay::{
    backend::renderer::{
        element::{
            surface::WaylandSurfaceRenderElement, utils::CropRenderElement, Element, Id, Kind, RenderElement,
            UnderlyingStorage,
        },
        gles::{GlesError, GlesFrame, GlesRenderer, GlesTexProgram, Uniform, UniformName},
        utils::{CommitCounter, DamageSet, OpaqueRegions},
    },
    utils::{Buffer, Logical, Physical, Point, Rectangle, Scale, Transform},
};

use crate::ssd::ContentClip;
use crate::backend::visual::{
    SnappedLogicalRect, snapped_logical_radius,
    snapped_logical_rect_relative_with_mode,
};

#[derive(Debug)]
enum ClippedSurfaceInner {
    Simple(CropRenderElement<WaylandSurfaceRenderElement<GlesRenderer>>),
    Mapped(WaylandSurfaceRenderElement<GlesRenderer>),
}

#[derive(Debug)]
pub struct ClippedSurfaceElement {
    inner: ClippedSurfaceInner,
    program: GlesTexProgram,
    clip_rect: SnappedLogicalRect,
    corner_radius: [f32; 4],
    simple_clip_size: Option<[f32; 2]>,
    rect_bounds_enabled: f32,
    output_scale: f32,
    clip_scale: f32,
}

#[derive(Debug)]
struct ClippedSurfaceProgram(GlesTexProgram);

impl ClippedSurfaceElement {
    pub fn new(
        renderer: &mut GlesRenderer,
        inner: WaylandSurfaceRenderElement<GlesRenderer>,
        output_scale: Scale<f64>,
        clip_scale: Scale<f64>,
        output_origin: Point<i32, Logical>,
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
                    UniformName::new(
                        "clip_scale",
                        smithay::backend::renderer::gles::UniformType::_1f,
                    ),
                    UniformName::new(
                        "clip_size",
                        smithay::backend::renderer::gles::UniformType::_2f,
                    ),
                    UniformName::new(
                        "corner_radius",
                        smithay::backend::renderer::gles::UniformType::_4f,
                    ),
                    UniformName::new(
                        "rect_bounds_enabled",
                        smithay::backend::renderer::gles::UniformType::_1f,
                    ),
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

        let local_clip = Rectangle::new(
            Point::from((
                clip.rect.loc.x - output_origin.x,
                clip.rect.loc.y - output_origin.y,
            )),
            clip.rect.size,
        );
        let snapped_clip_rect = snapped_logical_rect_relative_with_mode(
            crate::ssd::LogicalRect::new(
                clip.rect.loc.x,
                clip.rect.loc.y,
                clip.rect.size.w,
                clip.rect.size.h,
            ),
            output_origin,
            clip_scale,
            clip.snap_mode,
        );
        let physical_left = (snapped_clip_rect.x as f64 * output_scale.x).round() as i32;
        let physical_top = (snapped_clip_rect.y as f64 * output_scale.y).round() as i32;
        let physical_right =
            ((snapped_clip_rect.x + snapped_clip_rect.width) as f64 * output_scale.x).round() as i32;
        let physical_bottom =
            ((snapped_clip_rect.y + snapped_clip_rect.height) as f64 * output_scale.y).round() as i32;
        let physical_clip: Rectangle<i32, Physical> = Rectangle::new(
            Point::from((physical_left, physical_top)),
            (
                (physical_right - physical_left).max(0),
                (physical_bottom - physical_top).max(0),
            )
                .into(),
        );
        let original_physical_clip = physical_clip;
        if std::env::var_os("SHOJI_GAP_DEBUG").is_some() {
            tracing::info!(
                output_origin = ?output_origin,
                output_scale = ?output_scale,
                clip_scale = ?clip_scale,
                raw_geometry_output = ?inner.geometry(output_scale),
                raw_geometry_clip = ?inner.geometry(clip_scale),
                raw_src = ?inner.src(),
                raw_buffer_size = ?inner.buffer_size(),
                raw_view = ?inner.view(),
                raw_transform = ?inner.transform(),
                clip = ?clip,
                "gap debug clipped surface raw element"
            );
        }

        let inner = if (clip_scale.x - output_scale.x).abs() < f64::EPSILON
            && (clip_scale.y - output_scale.y).abs() < f64::EPSILON
        {
            let expanded_physical_clip = Rectangle::new(
                physical_clip.loc,
                (
                    physical_clip.size.w.saturating_add(1),
                    physical_clip.size.h.saturating_add(1),
                )
                    .into(),
            );
            match CropRenderElement::from_element(inner, output_scale, expanded_physical_clip) {
                Some(cropped) => ClippedSurfaceInner::Simple(cropped),
                None => return Err(GlesError::FramebufferBindingError),
            }
        } else {
            ClippedSurfaceInner::Mapped(inner)
        };

        if std::env::var_os("SHOJI_GAP_DEBUG").is_some() {
            match &inner {
                ClippedSurfaceInner::Simple(cropped) => {
                    let geometry = cropped.geometry(output_scale);
                    let src = cropped.src();
                    let physical_right = physical_clip.loc.x + physical_clip.size.w;
                    let physical_bottom = physical_clip.loc.y + physical_clip.size.h;
                    let geometry_right = geometry.loc.x + geometry.size.w;
                    let geometry_bottom = geometry.loc.y + geometry.size.h;
                    let src_right = src.loc.x + src.size.w;
                    let src_bottom = src.loc.y + src.size.h;
                    tracing::info!(
                        local_clip = ?local_clip,
                        physical_clip = ?original_physical_clip,
                        expanded_physical_clip = ?Rectangle::new(
                            original_physical_clip.loc,
                            (
                                original_physical_clip.size.w.saturating_add(1),
                                original_physical_clip.size.h.saturating_add(1),
                            )
                                .into()
                        ),
                        output_origin = ?output_origin,
                        output_scale = ?output_scale,
                        clip_scale = ?clip_scale,
                        cropped_geometry = ?geometry,
                        cropped_src = ?src,
                        physical_right,
                        physical_bottom,
                        geometry_right,
                        geometry_bottom,
                        src_right,
                        src_bottom,
                        "gap debug clipped surface simple crop"
                    );
                }
                ClippedSurfaceInner::Mapped(mapped) => {
                    tracing::info!(
                        local_clip = ?local_clip,
                        physical_clip = ?physical_clip,
                        output_origin = ?output_origin,
                        output_scale = ?output_scale,
                        clip_scale = ?clip_scale,
                        mapped_geometry = ?mapped.geometry(output_scale),
                        mapped_src = ?mapped.src(),
                        "gap debug clipped surface mapped crop"
                    );
                }
            }
        }

        Ok(Self {
            inner,
            program: program.0.clone(),
            clip_rect: snapped_clip_rect,
            corner_radius: if (clip_scale.x - output_scale.x).abs() < f64::EPSILON
                && (clip_scale.y - output_scale.y).abs() < f64::EPSILON
            {
                let radius = snapped_logical_radius(clip.radius, output_scale) * output_scale.x as f32;
                [0.0, 0.0, radius, radius]
            } else {
                let radius = snapped_logical_radius(clip.radius, clip_scale);
                [radius, radius, radius, radius]
            },
            simple_clip_size: if (clip_scale.x - output_scale.x).abs() < f64::EPSILON
                && (clip_scale.y - output_scale.y).abs() < f64::EPSILON
            {
                Some([
                    original_physical_clip.size.w as f32,
                    original_physical_clip.size.h as f32,
                ])
            } else {
                None
            },
            rect_bounds_enabled: if (clip_scale.x - output_scale.x).abs() < f64::EPSILON
                && (clip_scale.y - output_scale.y).abs() < f64::EPSILON
            {
                0.0
            } else {
                1.0
            },
            output_scale: output_scale.x as f32,
            clip_scale: clip_scale.x as f32,
        })
    }

    fn uniforms(&self) -> Vec<Uniform<'static>> {
        let scale = Scale::from(self.output_scale as f64);
        let (clip_size, corner_radius, input_to_clip_array) = match &self.inner {
            ClippedSurfaceInner::Simple(inner) => {
                let geometry = inner.geometry(scale);
                let clip_size = self.simple_clip_size.map_or(
                    Vector2::new(geometry.size.w as f32, geometry.size.h as f32),
                    |size| Vector2::new(size[0].max(1.0), size[1].max(1.0)),
                );
                let input_to_clip_array = [
                    geometry.size.w as f32 / clip_size.x,
                    0.0,
                    0.0,
                    0.0,
                    geometry.size.h as f32 / clip_size.y,
                    0.0,
                    0.0,
                    0.0,
                    1.0,
                ];
                (
                    clip_size,
                    self.corner_radius,
                    input_to_clip_array,
                )
            }
            ClippedSurfaceInner::Mapped(inner) => {
                let element_geometry = inner.geometry(scale);

                let element_loc = Vector2::new(
                    element_geometry.loc.x as f32 / self.output_scale.max(0.0001),
                    element_geometry.loc.y as f32 / self.output_scale.max(0.0001),
                );
                let element_size = Vector2::new(
                    element_geometry.size.w as f32 / self.output_scale.max(0.0001),
                    element_geometry.size.h as f32 / self.output_scale.max(0.0001),
                );

                let clip_loc = Vector2::new(self.clip_rect.x, self.clip_rect.y);
                let clip_size = Vector2::new(self.clip_rect.width, self.clip_rect.height);

                let buffer_size = inner.buffer_size();
                let buffer_size = Vector2::new(buffer_size.w as f32, buffer_size.h as f32);

                let view = inner.view();
                let src_loc = Vector2::new(view.src.loc.x as f32, view.src.loc.y as f32);
                let src_size = Vector2::new(view.src.size.w as f32, view.src.size.h as f32);

                let transform = match inner.transform() {
                    Transform::_90 => Transform::_270,
                    Transform::_270 => Transform::_90,
                    other => other,
                };

                let tm = transform.matrix();
                let transform_matrix = Matrix3::from_translation(Vector2::new(0.5, 0.5))
                    * Matrix3::from_cols(tm[0], tm[1], tm[2])
                    * Matrix3::from_translation(Vector2::new(-0.5, -0.5));

                let input_to_clip = transform_matrix
                    * Matrix3::from_nonuniform_scale(
                        element_size.x / clip_size.x,
                        element_size.y / clip_size.y,
                    )
                    * Matrix3::from_translation((element_loc - clip_loc).div_element_wise(element_size))
                    * Matrix3::from_nonuniform_scale(
                        buffer_size.x / src_size.x,
                        buffer_size.y / src_size.y,
                    )
                    * Matrix3::from_translation(-src_loc.div_element_wise(buffer_size));

                (
                    clip_size,
                    self.corner_radius,
                    [
                        input_to_clip.x.x,
                        input_to_clip.x.y,
                        input_to_clip.x.z,
                        input_to_clip.y.x,
                        input_to_clip.y.y,
                        input_to_clip.y.z,
                        input_to_clip.z.x,
                        input_to_clip.z.y,
                        input_to_clip.z.z,
                    ],
                )
            }
        };

        vec![
            Uniform::new("clip_scale", self.clip_scale),
            Uniform::new("clip_size", [clip_size.x, clip_size.y]),
            Uniform::new("corner_radius", corner_radius),
            Uniform::new("rect_bounds_enabled", self.rect_bounds_enabled),
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
        match &self.inner {
            ClippedSurfaceInner::Simple(inner) => inner.id(),
            ClippedSurfaceInner::Mapped(inner) => inner.id(),
        }
    }

    fn current_commit(&self) -> CommitCounter {
        match &self.inner {
            ClippedSurfaceInner::Simple(inner) => inner.current_commit(),
            ClippedSurfaceInner::Mapped(inner) => inner.current_commit(),
        }
    }

    fn geometry(&self, scale: Scale<f64>) -> Rectangle<i32, Physical> {
        match &self.inner {
            ClippedSurfaceInner::Simple(inner) => inner.geometry(scale),
            ClippedSurfaceInner::Mapped(inner) => inner.geometry(scale),
        }
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        match &self.inner {
            ClippedSurfaceInner::Simple(inner) => inner.src(),
            ClippedSurfaceInner::Mapped(inner) => inner.src(),
        }
    }

    fn transform(&self) -> Transform {
        match &self.inner {
            ClippedSurfaceInner::Simple(inner) => inner.transform(),
            ClippedSurfaceInner::Mapped(inner) => inner.transform(),
        }
    }

    fn damage_since(
        &self,
        scale: Scale<f64>,
        commit: Option<CommitCounter>,
    ) -> DamageSet<i32, Physical> {
        let damage = match &self.inner {
            ClippedSurfaceInner::Simple(inner) => inner.damage_since(scale, commit),
            ClippedSurfaceInner::Mapped(inner) => inner.damage_since(scale, commit),
        };

        if std::env::var_os("SHOJI_GAP_DEBUG").is_some() {
            match &self.inner {
                ClippedSurfaceInner::Simple(inner) => {
                    tracing::info!(
                        scale = ?scale,
                        commit = ?commit,
                        geometry = ?inner.geometry(scale),
                        damage = ?damage,
                        "gap debug clipped surface simple damage"
                    );
                }
                ClippedSurfaceInner::Mapped(inner) => {
                    tracing::info!(
                        scale = ?scale,
                        commit = ?commit,
                        geometry = ?inner.geometry(scale),
                        damage = ?damage,
                        "gap debug clipped surface mapped damage"
                    );
                }
            }
        }

        damage
    }

    fn opaque_regions(&self, _scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        OpaqueRegions::default()
    }

    fn alpha(&self) -> f32 {
        match &self.inner {
            ClippedSurfaceInner::Simple(inner) => inner.alpha(),
            ClippedSurfaceInner::Mapped(inner) => inner.alpha(),
        }
    }

    fn kind(&self) -> Kind {
        match &self.inner {
            ClippedSurfaceInner::Simple(inner) => inner.kind(),
            ClippedSurfaceInner::Mapped(inner) => inner.kind(),
        }
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
        match &self.inner {
            ClippedSurfaceInner::Simple(inner) => RenderElement::<GlesRenderer>::draw(
                inner,
                frame,
                src,
                dst,
                damage,
                opaque_regions,
                cache,
            ),
            ClippedSurfaceInner::Mapped(inner) => {
                frame.override_default_tex_program(self.program.clone(), self.uniforms());
                let result = RenderElement::<GlesRenderer>::draw(
                    inner,
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
        }
    }

    fn underlying_storage(&self, _renderer: &mut GlesRenderer) -> Option<UnderlyingStorage<'_>> {
        None
    }
}
