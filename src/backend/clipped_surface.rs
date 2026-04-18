use cgmath::{ElementWise, Matrix3, Vector2, Vector3};
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

use crate::backend::visual::{
    PreciseLogicalRect, SnappedLogicalRect, precise_logical_rect_in_element_space,
};
use crate::ssd::ContentClip;

#[derive(Debug, Clone, Copy, PartialEq)]
struct SampleUvCompensation {
    uv_tl: [f32; 2],
    uv_br: [f32; 2],
    adjusted_uv_br: [f32; 2],
    buffer_size: [f32; 2],
    snap_axes: [f32; 2],
    sampled_texels: [f32; 2],
    projected_pixels: [f32; 2],
    misalignment: [f32; 2],
    enabled: bool,
}

fn compute_sample_uv_compensation(
    src_loc: Vector2<f32>,
    src_size: Vector2<f32>,
    buffer_size: Vector2<f32>,
    sampled_pixels: Vector2<f32>,
    projected_pixels: Vector2<f32>,
) -> SampleUvCompensation {
    let safe_buffer = Vector2::new(buffer_size.x.max(1.0), buffer_size.y.max(1.0));
    let uv_tl = src_loc.div_element_wise(safe_buffer);
    let uv_br = (src_loc + src_size).div_element_wise(safe_buffer);
    let uv_range = uv_br - uv_tl;
    let misalignment = sampled_pixels - projected_pixels;
    let absolute_misalignment = Vector2::new(misalignment.x.abs(), misalignment.y.abs());
    let compensation_min = 0.5;
    let compensation_max = 2.0;
    // Compensate when sampled texels and projected pixels differ by at least
    // half a pixel on an axis, but only for tiny mismatches that look like
    // raster-grid rounding drift (for example 1919 -> 1920). Large differences
    // indicate intentional scaling and must not be compensated.
    let snap_x = (absolute_misalignment.x >= compensation_min
        && absolute_misalignment.x <= compensation_max) as i32 as f32;
    let snap_y = (absolute_misalignment.y >= compensation_min
        && absolute_misalignment.y <= compensation_max) as i32 as f32;
    let effective_misalignment = Vector2::new(
        if snap_x > 0.5 {
            absolute_misalignment.x
        } else {
            0.0
        },
        if snap_y > 0.5 {
            absolute_misalignment.y
        } else {
            0.0
        },
    );
    let safe_projected_pixels = Vector2::new(projected_pixels.x.max(1.0), projected_pixels.y.max(1.0));
    let adjusted_uv_br = uv_br
        + effective_misalignment
            .div_element_wise(safe_projected_pixels)
            .mul_element_wise(uv_range);
    let enabled = snap_x > 0.5 || snap_y > 0.5;

    SampleUvCompensation {
        uv_tl: [uv_tl.x, uv_tl.y],
        uv_br: [uv_br.x, uv_br.y],
        adjusted_uv_br: [adjusted_uv_br.x, adjusted_uv_br.y],
        buffer_size: [safe_buffer.x, safe_buffer.y],
        snap_axes: [snap_x, snap_y],
        sampled_texels: [sampled_pixels.x, sampled_pixels.y],
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
    debug_label: Option<String>,
    clip_rect: SnappedLogicalRect,
    corner_radius: [f32; 4],
    rect_bounds_enabled: f32,
    output_scale: f32,
    clip_scale: f32,
    sample_uv_tl: [f32; 2],
    sample_uv_br: [f32; 2],
    adjusted_sample_uv_br: [f32; 2],
    sample_buffer_size: [f32; 2],
    sample_uv_snap_axes: [f32; 2],
    sample_uv_compensation_enabled: f32,
}

#[derive(Debug)]
struct ClippedSurfaceProgram(GlesTexProgram);

fn clipped_uniforms_debug_enabled() -> bool {
    std::env::var_os("SHOJI_CLIPPED_UNIFORMS_DEBUG")
        .is_some_and(|value| value != "0" && !value.is_empty())
        || std::env::var_os("SHOJI_GAP_DEBUG")
            .is_some_and(|value| value != "0" && !value.is_empty())
}

impl ClippedSurfaceElement {
    pub fn new(
        renderer: &mut GlesRenderer,
        inner: WaylandSurfaceRenderElement<GlesRenderer>,
        output_scale: Scale<f64>,
        clip_scale: Scale<f64>,
        output_origin: Point<i32, Logical>,
        clip: ContentClip,
        forced_geometry: Option<Rectangle<i32, Physical>>,
        debug_label: Option<String>,
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
                        "sample_uv_snap_axes",
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

        let element_geometry = inner.geometry(output_scale);
        // Keep the actual surface draw quad on the geometry reported by the
        // Wayland surface element. For xwayland-satellite this is the only
        // geometry that is guaranteed to stay in lockstep with the received
        // buffer size and viewport destination. Shrinking the draw quad to the
        // SSD slot by one rounded pixel is what exposed the sharp edge
        // artifact: it introduced a second raster grid after Smithay had
        // already snapped the surface. We still use the SSD clip to constrain
        // visibility inside the slot, but avoid a second geometry snap.
        let render_geometry = element_geometry;
        let output_scale_x = output_scale.x.abs().max(0.0001) as f32;
        let output_scale_y = output_scale.y.abs().max(0.0001) as f32;
        // element_geometry.loc is in output-local physical coordinates.
        // clip.mask_rect_precise is in global logical coordinates (derived from the decoration
        // layout which uses space.element_location as origin).
        // To subtract them correctly we must work in the same coordinate space.
        // Adding output_origin converts the output-local logical position to global logical,
        // so that (clip.x - element_rect_precise.x) gives the element-relative clip offset
        // on any output, not just on the primary output whose origin is (0, 0).
        let element_rect_precise = PreciseLogicalRect {
            x: element_geometry.loc.x as f32 / output_scale_x + output_origin.x as f32,
            y: element_geometry.loc.y as f32 / output_scale_y + output_origin.y as f32,
            width: element_geometry.size.w as f32 / output_scale_x,
            height: element_geometry.size.h as f32 / output_scale_y,
        };
        // element_rect_logical uses only width/height (size, not position), so it is not
        // affected by the global/output-local distinction.
        let element_rect_logical = SnappedLogicalRect {
            x: 0.0,
            y: 0.0,
            width: element_rect_precise.width,
            height: element_rect_precise.height,
        };
        let local_clip = Rectangle::new(
            Point::from((
                (clip.mask_rect_precise.x - element_rect_precise.x).round() as i32,
                (clip.mask_rect_precise.y - element_rect_precise.y).round() as i32,
            )),
            clip.mask_rect.size,
        );
        let mut snapped_clip_rect =
            precise_logical_rect_in_element_space(clip.mask_rect_precise, element_rect_precise);
        let clip_size_delta_px = (
            ((snapped_clip_rect.width - element_rect_logical.width) * output_scale_x).abs(),
            ((snapped_clip_rect.height - element_rect_logical.height) * output_scale_y).abs(),
        );
        // Keep the clip rect on the shared-edge grid unless the difference is
        // just floating point noise. Treating a real 1px size difference as
        // equivalent collapses the client width back to the surface geometry
        // and reintroduces the visible right-edge gap against decoration boxes.
        if clip_size_delta_px.0 <= 0.01 && clip_size_delta_px.1 <= 0.01 {
            snapped_clip_rect.x = element_rect_logical.x;  // = 0.0: clip starts at element origin
            snapped_clip_rect.y = element_rect_logical.y;  // = 0.0
            snapped_clip_rect.width = element_rect_logical.width;
            snapped_clip_rect.height = element_rect_logical.height;
        }
        let physical_left = (((clip.mask_rect_precise.x - element_rect_precise.x) as f64)
            * output_scale.x)
            .round() as i32;
        let physical_top = (((clip.mask_rect_precise.y - element_rect_precise.y) as f64)
            * output_scale.y)
            .round() as i32;
        let physical_right = ((((clip.mask_rect_precise.x + clip.mask_rect_precise.width)
            - element_rect_precise.x) as f64)
            * output_scale.x)
            .round() as i32;
        let physical_bottom = ((((clip.mask_rect_precise.y + clip.mask_rect_precise.height)
            - element_rect_precise.y) as f64)
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
        let buffer_size = inner.buffer_size();
        let buffer_size = Vector2::new(buffer_size.w as f32, buffer_size.h as f32);
        let view = inner.view();
        let src_loc = Vector2::new(view.src.loc.x as f32, view.src.loc.y as f32);
        let src_size = Vector2::new(view.src.size.w as f32, view.src.size.h as f32);
        let sampled_pixels = if src_size.x > 0.0 && src_size.y > 0.0 {
            src_size
        } else {
            Vector2::new(element_geometry.size.w as f32, element_geometry.size.h as f32)
        };
        let view_dst_pixels = Vector2::new(
            view.dst.w.max(0) as f32 * output_scale_x,
            view.dst.h.max(0) as f32 * output_scale_y,
        );
        // Match the sampling to the viewport destination that the client asked
        // the compositor to project to. For xwayland-satellite we have seen
        // one-pixel mismatches between source texels and projected pixels
        // (e.g. 1919 -> 1920), which can blur the entire surface if we do not
        // bias the mismatch to the final texel.
        let projected_pixels = if view.dst.w > 0 && view.dst.h > 0 {
            view_dst_pixels
        } else {
            Vector2::new(render_geometry.size.w as f32, render_geometry.size.h as f32)
        };
        let sample_uv_compensation = compute_sample_uv_compensation(
            src_loc,
            src_size,
            buffer_size,
            sampled_pixels,
            projected_pixels,
        );
        if std::env::var_os("SHOJI_GAP_DEBUG").is_some() {
            tracing::info!(
                debug_label = ?debug_label,
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
                forced_geometry = ?forced_geometry,
                view_dst_pixels = ?view_dst_pixels,
                projected_pixels = ?projected_pixels,
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
            debug_label,
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
            sample_uv_snap_axes: sample_uv_compensation.snap_axes,
            sample_uv_compensation_enabled: if sample_uv_compensation.enabled {
                1.0
            } else {
                0.0
            },
        })
    }

    pub fn debug_label(&self) -> Option<&str> {
        self.debug_label.as_deref()
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

        if clipped_uniforms_debug_enabled() {
            let map_uv = |u: f32, v: f32| {
                let mapped = Matrix3::from_cols(
                    Vector3::new(
                        input_to_clip_array[0],
                        input_to_clip_array[1],
                        input_to_clip_array[2],
                    ),
                    Vector3::new(
                        input_to_clip_array[3],
                        input_to_clip_array[4],
                        input_to_clip_array[5],
                    ),
                    Vector3::new(
                        input_to_clip_array[6],
                        input_to_clip_array[7],
                        input_to_clip_array[8],
                    ),
                ) * Vector3::new(u, v, 1.0);

                [mapped.x * clip_size.x, mapped.y * clip_size.y]
            };

            tracing::info!(
                debug_label = self.debug_label(),
                geometry = ?self.geometry,
                clip_rect = ?self.clip_rect,
                clip_size = ?[clip_size.x, clip_size.y],
                corner_radius = ?corner_radius,
                input_to_clip = ?input_to_clip_array,
                sample_uv_tl = ?self.sample_uv_tl,
                sample_uv_br = ?self.sample_uv_br,
                adjusted_sample_uv_br = ?self.adjusted_sample_uv_br,
                sample_buffer_size = ?self.sample_buffer_size,
                sample_uv_snap_axes = ?self.sample_uv_snap_axes,
                sample_uv_compensation_enabled = self.sample_uv_compensation_enabled,
                clip_coords_at_uv_00 = ?map_uv(0.0, 0.0),
                clip_coords_at_uv_10 = ?map_uv(1.0, 0.0),
                clip_coords_at_uv_01 = ?map_uv(0.0, 1.0),
                clip_coords_at_uv_11 = ?map_uv(1.0, 1.0),
                "clipped surface uniforms"
            );
        }

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
            Uniform::new("sample_uv_snap_axes", self.sample_uv_snap_axes),
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
                let uniforms = self.uniforms();
                if std::env::var_os("SHOJI_GAP_DEBUG")
                    .is_some_and(|value| value != "0" && !value.is_empty())
                {
                    let view = inner.view();
                    let view_dst_pixels = (
                        view.dst.w.max(0) as f64 * self.output_scale as f64,
                        view.dst.h.max(0) as f64 * self.output_scale as f64,
                    );
                    let src_scale = (
                        dst.size.w as f64 / src.size.w.max(1.0),
                        dst.size.h as f64 / src.size.h.max(1.0),
                    );
                    let dst_vs_view = (
                        dst.size.w as f64 - view_dst_pixels.0,
                        dst.size.h as f64 - view_dst_pixels.1,
                    );
                    tracing::info!(
                        debug_label = self.debug_label(),
                        src = ?src,
                        dst = ?dst,
                        element_geometry = ?self.geometry,
                        inner_geometry = ?inner.geometry(Scale::from((1.0, 1.0))),
                        inner_src = ?inner.src(),
                        inner_view = ?view,
                        inner_transform = ?inner.transform(),
                        inner_alpha = inner.alpha(),
                        inner_kind = ?inner.kind(),
                        src_scale = ?src_scale,
                        view_dst_pixels = ?view_dst_pixels,
                        dst_minus_view_dst = ?dst_vs_view,
                        damage = ?damage,
                        opaque_regions = ?opaque_regions,
                        uniform_count = uniforms.len(),
                        "gap debug clipped surface draw begin"
                    );
                }
                frame.override_default_tex_program(self.program.clone(), uniforms);
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
                if std::env::var_os("SHOJI_GAP_DEBUG")
                    .is_some_and(|value| value != "0" && !value.is_empty())
                {
                    tracing::info!(
                        debug_label = self.debug_label(),
                        src = ?src,
                        dst = ?dst,
                        result = ?result.as_ref().map(|_| ()),
                        "gap debug clipped surface draw end"
                    );
                }
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
            Vector2::new(1230.0, 796.0),
            Vector2::new(1230.0, 795.0),
        );

        assert!(compensation.enabled);
        assert_eq!(compensation.adjusted_uv_br[0], compensation.uv_br[0]);
        assert!(compensation.adjusted_uv_br[1] > compensation.uv_br[1]);
    }

    #[test]
    fn shrinks_sampling_for_fractional_projection_mismatch() {
        let compensation = compute_sample_uv_compensation(
            Vector2::new(0.0, 0.0),
            Vector2::new(1890.0, 1063.0),
            Vector2::new(1890.0, 1063.0),
            Vector2::new(1890.0, 1063.0),
            Vector2::new(1890.2539, 1062.2559),
        );

        assert!(compensation.enabled);
        assert_eq!(compensation.adjusted_uv_br[0], compensation.uv_br[0]);
        assert!(compensation.adjusted_uv_br[1] > compensation.uv_br[1]);
    }

    #[test]
    fn compensates_when_source_texels_are_one_pixel_shorter_on_both_axes() {
        let compensation = compute_sample_uv_compensation(
            Vector2::new(0.0, 0.0),
            Vector2::new(1919.0, 1199.0),
            Vector2::new(1919.0, 1199.0),
            Vector2::new(1919.0, 1199.0),
            Vector2::new(1920.0, 1200.0),
        );

        assert!(compensation.enabled);
        assert_eq!(compensation.snap_axes, [1.0, 1.0]);
        assert!(compensation.adjusted_uv_br[0] > compensation.uv_br[0]);
        assert!(compensation.adjusted_uv_br[1] > compensation.uv_br[1]);
    }

    #[test]
    fn leaves_intentional_large_scaling_mismatch_unchanged() {
        let compensation = compute_sample_uv_compensation(
            Vector2::new(0.0, 0.0),
            Vector2::new(640.0, 480.0),
            Vector2::new(640.0, 480.0),
            Vector2::new(640.0, 480.0),
            Vector2::new(800.0, 600.0),
        );

        assert!(!compensation.enabled);
        assert_eq!(compensation.snap_axes, [0.0, 0.0]);
        assert_eq!(compensation.adjusted_uv_br, compensation.uv_br);
    }
}
