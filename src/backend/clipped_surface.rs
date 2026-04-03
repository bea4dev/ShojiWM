use cgmath::{ElementWise, Matrix3, Vector2};
use smithay::{
    backend::renderer::{
        element::{
            Element, Id, Kind, RenderElement, UnderlyingStorage,
            surface::WaylandSurfaceRenderElement,
        },
        gles::{GlesError, GlesFrame, GlesRenderer, GlesTexProgram, Uniform, UniformName},
        utils::{CommitCounter, DamageSet, OpaqueRegions},
    },
    utils::{Buffer, Logical, Physical, Point, Rectangle, Scale, Transform},
};

use crate::backend::visual::{SnappedLogicalRect, precise_logical_rect_in_element_space};
use crate::ssd::ContentClip;

#[derive(Debug, Clone, Copy, PartialEq)]
struct SampleUvCompensation {
    uv_tl: [f32; 2],
    uv_br: [f32; 2],
    adjusted_uv_br: [f32; 2],
    buffer_size: [f32; 2],
    sampled_texels: [f32; 2],
    projected_pixels: [f32; 2],
    misalignment: [f32; 2],
    enabled: bool,
}

fn compute_sample_uv_compensation(
    src_loc: Vector2<f32>,
    src_size: Vector2<f32>,
    buffer_size: Vector2<f32>,
    projected_pixels: Vector2<f32>,
) -> SampleUvCompensation {
    let safe_buffer = Vector2::new(buffer_size.x.max(1.0), buffer_size.y.max(1.0));
    let uv_tl = src_loc.div_element_wise(safe_buffer);
    let uv_br = (src_loc + src_size).div_element_wise(safe_buffer);
    let sampled_texels = (uv_br - uv_tl).mul_element_wise(safe_buffer);
    let misalignment = sampled_texels - projected_pixels;
    let adjusted_uv_br = uv_br - misalignment.div_element_wise(safe_buffer);
    let enabled = misalignment.x.abs() > 0.01 || misalignment.y.abs() > 0.01;

    SampleUvCompensation {
        uv_tl: [uv_tl.x, uv_tl.y],
        uv_br: [uv_br.x, uv_br.y],
        adjusted_uv_br: [adjusted_uv_br.x, adjusted_uv_br.y],
        buffer_size: [safe_buffer.x, safe_buffer.y],
        sampled_texels: [sampled_texels.x, sampled_texels.y],
        projected_pixels: [projected_pixels.x, projected_pixels.y],
        misalignment: [misalignment.x, misalignment.y],
        enabled,
    }
}

#[derive(Debug)]
enum ClippedSurfaceInner {
    Mapped(WaylandSurfaceRenderElement<GlesRenderer>),
}

#[derive(Debug)]
pub struct ClippedSurfaceElement {
    inner: ClippedSurfaceInner,
    geometry: Rectangle<i32, Physical>,
    program: GlesTexProgram,
    clip_rect: SnappedLogicalRect,
    corner_radius: [f32; 4],
    rect_bounds_enabled: f32,
    output_scale: f32,
    clip_scale: f32,
    sample_uv_tl: [f32; 2],
    sample_uv_br: [f32; 2],
    adjusted_sample_uv_br: [f32; 2],
    sample_buffer_size: [f32; 2],
    sample_uv_compensation_enabled: f32,
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
        forced_geometry: Option<Rectangle<i32, Physical>>,
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
                    UniformName::new(
                        "sample_uv_tl",
                        smithay::backend::renderer::gles::UniformType::_2f,
                    ),
                    UniformName::new(
                        "sample_uv_br",
                        smithay::backend::renderer::gles::UniformType::_2f,
                    ),
                    UniformName::new(
                        "adjusted_sample_uv_br",
                        smithay::backend::renderer::gles::UniformType::_2f,
                    ),
                    UniformName::new(
                        "sample_buffer_size",
                        smithay::backend::renderer::gles::UniformType::_2f,
                    ),
                    UniformName::new(
                        "sample_uv_compensation_enabled",
                        smithay::backend::renderer::gles::UniformType::_1f,
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
                clip.mask_rect.loc.x - clip.rect.loc.x,
                clip.mask_rect.loc.y - clip.rect.loc.y,
            )),
            clip.mask_rect.size,
        );
        let mut snapped_clip_rect =
            precise_logical_rect_in_element_space(clip.mask_rect_precise, clip.rect_precise);
        let element_geometry = inner.geometry(output_scale);
        let output_scale_x = output_scale.x.abs().max(0.0001) as f32;
        let output_scale_y = output_scale.y.abs().max(0.0001) as f32;
        let element_rect_logical = SnappedLogicalRect {
            x: element_geometry.loc.x as f32 / output_scale_x,
            y: element_geometry.loc.y as f32 / output_scale_y,
            width: element_geometry.size.w as f32 / output_scale_x,
            height: element_geometry.size.h as f32 / output_scale_y,
        };
        let clip_size_delta_px = (
            ((snapped_clip_rect.width - element_rect_logical.width) * output_scale_x).abs(),
            ((snapped_clip_rect.height - element_rect_logical.height) * output_scale_y).abs(),
        );
        // Keep the clip rect on the shared-edge grid unless the difference is
        // just floating point noise. Treating a real 1px size difference as
        // equivalent collapses the client width back to the surface geometry
        // and reintroduces the visible right-edge gap against decoration boxes.
        if clip_size_delta_px.0 <= 0.01 && clip_size_delta_px.1 <= 0.01 {
            snapped_clip_rect.x = element_rect_logical.x;
            snapped_clip_rect.y = element_rect_logical.y;
            snapped_clip_rect.width = element_rect_logical.width;
            snapped_clip_rect.height = element_rect_logical.height;
        }
        let physical_left = (((clip.mask_rect_precise.x - clip.rect_precise.x) as f64)
            * output_scale.x)
            .round() as i32;
        let physical_top = (((clip.mask_rect_precise.y - clip.rect_precise.y) as f64)
            * output_scale.y)
            .round() as i32;
        let physical_right = ((((clip.mask_rect_precise.x + clip.mask_rect_precise.width)
            - clip.rect_precise.x) as f64)
            * output_scale.x)
            .round() as i32;
        let physical_bottom = ((((clip.mask_rect_precise.y + clip.mask_rect_precise.height)
            - clip.rect_precise.y) as f64)
            * output_scale.y)
            .round() as i32;
        let physical_clip: Rectangle<i32, Physical> = Rectangle::new(
            Point::from((physical_left, physical_top)),
            (
                (physical_right - physical_left).max(0),
                (physical_bottom - physical_top).max(0),
            )
                .into(),
        );
        let render_geometry = forced_geometry.unwrap_or(physical_clip);
        let buffer_size = inner.buffer_size();
        let buffer_size = Vector2::new(buffer_size.w as f32, buffer_size.h as f32);
        let view = inner.view();
        let src_loc = Vector2::new(view.src.loc.x as f32, view.src.loc.y as f32);
        let src_size = Vector2::new(view.src.size.w as f32, view.src.size.h as f32);
        let sample_uv_compensation = compute_sample_uv_compensation(
            src_loc,
            src_size,
            buffer_size,
            Vector2::new(render_geometry.size.w as f32, render_geometry.size.h as f32),
        );
        if std::env::var_os("SHOJI_GAP_DEBUG").is_some() {
            tracing::info!(
                output_origin = ?output_origin,
                output_scale = ?output_scale,
                clip_scale = ?clip_scale,
                raw_geometry_output = ?element_geometry,
                raw_geometry_clip = ?inner.geometry(clip_scale),
                raw_src = ?inner.src(),
                raw_buffer_size = ?inner.buffer_size(),
                raw_view = ?inner.view(),
                raw_transform = ?inner.transform(),
                clip = ?clip,
                aligned_clip_rect_precise = ?snapped_clip_rect,
                aligned_clip_rect = ?snapped_clip_rect,
                element_rect_logical = ?element_rect_logical,
                slot_rect_precise = ?clip.rect_precise,
                mask_rect_precise = ?clip.mask_rect_precise,
                clip_size_delta_px = ?clip_size_delta_px,
                sample_uv_compensation = ?sample_uv_compensation,
                "gap debug clipped surface raw element"
            );
        }

        // Avoid CropRenderElement here.
        //
        // In the SSD client-slot path we observed CropRenderElement intersecting the
        // client geometry one pixel narrower than the intended clip rect, which in
        // turn shifted the source region by one pixel and produced the visible gap on
        // the right edge. Keeping clipping in the shader preserves the original
        // surface geometry and lets the rounded-rect clip operate on the same snapped
        // coordinates as the decoration pass.
        let inner = ClippedSurfaceInner::Mapped(inner);

        if std::env::var_os("SHOJI_GAP_DEBUG").is_some() {
            match &inner {
                ClippedSurfaceInner::Mapped(mapped) => {
                    tracing::info!(
                        local_clip = ?local_clip,
                        physical_clip = ?physical_clip,
                        render_geometry = ?render_geometry,
                        output_origin = ?output_origin,
                        output_scale = ?output_scale,
                        clip_scale = ?clip_scale,
                        mapped_geometry = ?mapped.geometry(output_scale),
                        element_geometry = ?render_geometry,
                        mapped_src = ?mapped.src(),
                        slot_rect = ?clip.rect,
                        slot_rect_precise = ?clip.rect_precise,
                        mask_rect = ?clip.mask_rect,
                        mask_rect_precise = ?clip.mask_rect_precise,
                        corner_radius = ?clip.corner_radii_precise,
                        "gap debug clipped surface mapped crop"
                    );
                }
            }
        }

        Ok(Self {
            inner,
            geometry: render_geometry,
            program: program.0.clone(),
            clip_rect: snapped_clip_rect,
            corner_radius: {
                let scale_x = clip_scale.x.abs().max(0.0001) as f32;
                clip.corner_radii_precise
                    .map(|radius| ((radius.max(0.0) * scale_x).round() / scale_x).max(0.0))
            },
            rect_bounds_enabled: 1.0,
            output_scale: output_scale.x as f32,
            clip_scale: clip_scale.x as f32,
            sample_uv_tl: sample_uv_compensation.uv_tl,
            sample_uv_br: sample_uv_compensation.uv_br,
            adjusted_sample_uv_br: sample_uv_compensation.adjusted_uv_br,
            sample_buffer_size: sample_uv_compensation.buffer_size,
            sample_uv_compensation_enabled: if sample_uv_compensation.enabled {
                1.0
            } else {
                0.0
            },
        })
    }

    fn uniforms(&self) -> Vec<Uniform<'static>> {
        let (clip_size, corner_radius, input_to_clip_array) = match &self.inner {
            ClippedSurfaceInner::Mapped(inner) => {
                let element_geometry = self.geometry;
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
                    * Matrix3::from_translation((-clip_loc).div_element_wise(element_size))
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
            Uniform::new("sample_uv_tl", self.sample_uv_tl),
            Uniform::new("sample_uv_br", self.sample_uv_br),
            Uniform::new("adjusted_sample_uv_br", self.adjusted_sample_uv_br),
            Uniform::new("sample_buffer_size", self.sample_buffer_size),
            Uniform::new(
                "sample_uv_compensation_enabled",
                self.sample_uv_compensation_enabled,
            ),
        ]
    }
}

impl Element for ClippedSurfaceElement {
    fn id(&self) -> &Id {
        match &self.inner {
            ClippedSurfaceInner::Mapped(inner) => inner.id(),
        }
    }

    fn current_commit(&self) -> CommitCounter {
        match &self.inner {
            ClippedSurfaceInner::Mapped(inner) => inner.current_commit(),
        }
    }

    fn geometry(&self, scale: Scale<f64>) -> Rectangle<i32, Physical> {
        let _ = scale;
        self.geometry
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        match &self.inner {
            ClippedSurfaceInner::Mapped(inner) => inner.src(),
        }
    }

    fn transform(&self) -> Transform {
        match &self.inner {
            ClippedSurfaceInner::Mapped(inner) => inner.transform(),
        }
    }

    fn damage_since(
        &self,
        scale: Scale<f64>,
        commit: Option<CommitCounter>,
    ) -> DamageSet<i32, Physical> {
        let damage = match &self.inner {
            ClippedSurfaceInner::Mapped(inner) => inner.damage_since(scale, commit),
        };

        if std::env::var_os("SHOJI_GAP_DEBUG").is_some() {
            match &self.inner {
                ClippedSurfaceInner::Mapped(inner) => {
                    tracing::info!(
                        scale = ?scale,
                        commit = ?commit,
                        geometry = ?self.geometry,
                        inner_geometry = ?inner.geometry(scale),
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
            ClippedSurfaceInner::Mapped(inner) => inner.alpha(),
        }
    }

    fn kind(&self) -> Kind {
        match &self.inner {
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

#[cfg(test)]
mod tests {
    use super::compute_sample_uv_compensation;
    use cgmath::Vector2;

    #[test]
    fn leaves_matching_projection_unchanged() {
        let compensation = compute_sample_uv_compensation(
            Vector2::new(0.0, 0.0),
            Vector2::new(2646.0, 1586.0),
            Vector2::new(2646.0, 1586.0),
            Vector2::new(2646.0, 1586.0),
        );

        assert!(!compensation.enabled);
        assert_eq!(compensation.adjusted_uv_br, compensation.uv_br);
    }

    #[test]
    fn expands_sampling_when_projection_is_one_pixel_wider() {
        let compensation = compute_sample_uv_compensation(
            Vector2::new(0.0, 0.0),
            Vector2::new(2646.0, 1586.0),
            Vector2::new(2646.0, 1586.0),
            Vector2::new(2647.0, 1586.0),
        );

        assert!(compensation.enabled);
        assert!(compensation.adjusted_uv_br[0] > compensation.uv_br[0]);
        assert_eq!(compensation.adjusted_uv_br[1], compensation.uv_br[1]);
    }

    #[test]
    fn shrinks_sampling_when_projection_is_one_pixel_shorter() {
        let compensation = compute_sample_uv_compensation(
            Vector2::new(0.0, 0.0),
            Vector2::new(1230.0, 796.0),
            Vector2::new(1230.0, 796.0),
            Vector2::new(1230.0, 795.0),
        );

        assert!(compensation.enabled);
        assert_eq!(compensation.adjusted_uv_br[0], compensation.uv_br[0]);
        assert!(compensation.adjusted_uv_br[1] < compensation.uv_br[1]);
    }
}
