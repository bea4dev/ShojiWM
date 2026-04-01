use smithay::{
    backend::renderer::{
        element::{Element, Id, Kind, RenderElement, UnderlyingStorage},
        gles::{GlesError, GlesFrame, GlesPixelProgram, GlesRenderer, Uniform, UniformName},
        utils::{CommitCounter, OpaqueRegions},
    },
    utils::{user_data::UserDataMap, Buffer, Logical, Physical, Rectangle, Scale, Transform},
};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RoundedClip {
    pub rect: crate::backend::visual::SnappedLogicalRect,
    pub radius: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RoundedInnerMode {
    None,
    DerivedInset,
    Explicit(RoundedClip),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RoundedShapeKind {
    Fill,
    Border { width: f32 },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RoundedRectSpec {
    pub rect: Rectangle<i32, Logical>,
    pub geometry: Rectangle<i32, Physical>,
    pub color: [f32; 4],
    pub alpha: f32,
    pub radius: f32,
    pub corner_radii: [f32; 4],
    pub shape: RoundedShapeKind,
    pub inner_mode: RoundedInnerMode,
    pub clip: Option<RoundedClip>,
    pub outer_render_scale: f32,
    pub inner_render_scale: f32,
    pub clip_render_scale: f32,
    pub debug_inner_only: f32,
    pub debug_clip_only: f32,
    pub debug_shell_only: f32,
}

#[derive(Debug, Clone)]
pub struct RoundedElementState {
    id: Id,
    commit_counter: CommitCounter,
    last_spec: Option<RoundedRectSpec>,
}

impl Default for RoundedElementState {
    fn default() -> Self {
        Self {
            id: Id::new(),
            commit_counter: CommitCounter::default(),
            last_spec: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct StableRoundedElement {
    shader: GlesPixelProgram,
    id: Id,
    commit_counter: CommitCounter,
    area: Rectangle<i32, Logical>,
    geometry: Rectangle<i32, Physical>,
    alpha: f32,
    additional_uniforms: Vec<Uniform<'static>>,
    kind: Kind,
}

#[derive(Debug)]
struct RoundedRectProgram(GlesPixelProgram);

impl RoundedElementState {
    pub fn element(
        &mut self,
        renderer: &mut GlesRenderer,
        spec: RoundedRectSpec,
    ) -> Result<StableRoundedElement, GlesError> {
        if self.last_spec.as_ref() != Some(&spec) {
            self.commit_counter.increment();
            self.last_spec = Some(spec);
        }

        let shader = shader_program(renderer)?;
        let uniforms = uniforms_for_spec(spec);

        Ok(StableRoundedElement {
            shader,
            id: self.id.clone(),
            commit_counter: self.commit_counter,
            area: spec.rect,
            geometry: spec.geometry,
            alpha: spec.alpha.clamp(0.0, 1.0),
            additional_uniforms: uniforms,
            kind: Kind::Unspecified,
        })
    }
}

impl Element for StableRoundedElement {
    fn id(&self) -> &Id {
        &self.id
    }

    fn current_commit(&self) -> CommitCounter {
        self.commit_counter
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        Rectangle::from_size(self.area.size.to_f64().to_buffer(1.0, Transform::Normal))
    }

    fn geometry(&self, scale: Scale<f64>) -> Rectangle<i32, Physical> {
        let _ = scale;
        self.geometry
    }

    fn opaque_regions(&self, _scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        OpaqueRegions::default()
    }

    fn alpha(&self) -> f32 {
        self.alpha
    }

    fn kind(&self) -> Kind {
        self.kind
    }
}

impl RenderElement<GlesRenderer> for StableRoundedElement {
    fn draw(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        _opaque_regions: &[Rectangle<i32, Physical>],
        _cache: Option<&UserDataMap>,
    ) -> Result<(), GlesError> {
        frame.render_pixel_shader_to(
            &self.shader,
            src,
            dst,
            self.area.size.to_buffer(1, Transform::Normal),
            Some(damage),
            self.alpha,
            &self.additional_uniforms,
        )
    }

    fn underlying_storage(&self, _renderer: &mut GlesRenderer) -> Option<UnderlyingStorage<'_>> {
        None
    }
}

fn shader_program(renderer: &mut GlesRenderer) -> Result<GlesPixelProgram, GlesError> {
    if renderer
        .egl_context()
        .user_data()
        .get::<RoundedRectProgram>()
        .is_none()
    {
        let compiled = RoundedRectProgram(renderer.compile_custom_pixel_shader(
            include_str!("rounded_rect.frag"),
            &[
                UniformName::new("color", smithay::backend::renderer::gles::UniformType::_4f),
                UniformName::new(
                    "corner_radius",
                    smithay::backend::renderer::gles::UniformType::_4f,
                ),
                UniformName::new(
                    "border_width",
                    smithay::backend::renderer::gles::UniformType::_1f,
                ),
                UniformName::new(
                    "inner_enabled",
                    smithay::backend::renderer::gles::UniformType::_1f,
                ),
                UniformName::new(
                    "inner_rect",
                    smithay::backend::renderer::gles::UniformType::_4f,
                ),
                UniformName::new(
                    "inner_radius",
                    smithay::backend::renderer::gles::UniformType::_4f,
                ),
                UniformName::new(
                    "outer_render_scale",
                    smithay::backend::renderer::gles::UniformType::_1f,
                ),
                UniformName::new(
                    "inner_render_scale",
                    smithay::backend::renderer::gles::UniformType::_1f,
                ),
                UniformName::new(
                    "clip_render_scale",
                    smithay::backend::renderer::gles::UniformType::_1f,
                ),
                UniformName::new(
                    "debug_inner_only",
                    smithay::backend::renderer::gles::UniformType::_1f,
                ),
                UniformName::new(
                    "debug_clip_only",
                    smithay::backend::renderer::gles::UniformType::_1f,
                ),
                UniformName::new(
                    "debug_shell_only",
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

    Ok(renderer
        .egl_context()
        .user_data()
        .get::<RoundedRectProgram>()
        .expect("rounded rect shader should be cached")
        .0
        .clone())
}

fn uniforms_for_spec(spec: RoundedRectSpec) -> Vec<Uniform<'static>> {
    let border_width = match spec.shape {
        RoundedShapeKind::Fill => 0.0,
        RoundedShapeKind::Border { width } => width.max(0.0),
    };

    let local_clip_rect = spec
        .clip
        .map(|clip| {
            [
                clip.rect.x,
                clip.rect.y,
                clip.rect.width,
                clip.rect.height,
            ]
        })
        .unwrap_or([0.0, 0.0, 0.0, 0.0]);
    let clip_radius = spec.clip.map(|clip| clip.radius.max(0.0)).unwrap_or(0.0);
    let (inner_enabled, local_inner_rect, inner_radius) = match spec.inner_mode {
        RoundedInnerMode::None | RoundedInnerMode::DerivedInset => {
            (0.0f32, [0.0, 0.0, 0.0, 0.0], 0.0f32)
        }
        RoundedInnerMode::Explicit(inner) => (
            1.0f32,
            [
                inner.rect.x,
                inner.rect.y,
                inner.rect.width,
                inner.rect.height,
            ],
            inner.radius.max(0.0),
        ),
    };
    let fallback_radius = spec.radius.max(0.0);
    let corner_radii = spec.corner_radii.map(|radius| radius.max(0.0));
    let uses_custom_corner_radii = corner_radii.iter().any(|radius| *radius > 0.0)
        || fallback_radius > 0.0;

    vec![
        Uniform::new("color", spec.color),
        Uniform::new(
            "corner_radius",
            if uses_custom_corner_radii {
                corner_radii
            } else {
                [fallback_radius, fallback_radius, fallback_radius, fallback_radius]
            },
        ),
        Uniform::new("border_width", border_width),
        Uniform::new("inner_enabled", inner_enabled),
        Uniform::new("inner_rect", local_inner_rect),
        Uniform::new(
            "inner_radius",
            [inner_radius, inner_radius, inner_radius, inner_radius],
        ),
        Uniform::new("outer_render_scale", spec.outer_render_scale.max(0.0001)),
        Uniform::new("inner_render_scale", spec.inner_render_scale.max(0.0001)),
        Uniform::new("clip_render_scale", spec.clip_render_scale.max(0.0001)),
        Uniform::new("debug_inner_only", spec.debug_inner_only),
        Uniform::new("debug_clip_only", spec.debug_clip_only),
        Uniform::new("debug_shell_only", spec.debug_shell_only),
        Uniform::new(
            "clip_enabled",
            if spec.clip.is_some() { 1.0f32 } else { 0.0f32 },
        ),
        Uniform::new("clip_rect", local_clip_rect),
        Uniform::new(
            "clip_radius",
            [clip_radius, clip_radius, clip_radius, clip_radius],
        ),
    ]
}
