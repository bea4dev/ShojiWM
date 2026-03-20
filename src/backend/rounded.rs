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
    pub rect: Rectangle<i32, Logical>,
    pub radius: i32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RoundedShapeKind {
    Fill,
    Border { width: i32 },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RoundedRectSpec {
    pub rect: Rectangle<i32, Logical>,
    pub color: [f32; 4],
    pub alpha: f32,
    pub radius: i32,
    pub shape: RoundedShapeKind,
    pub clip: Option<RoundedClip>,
    pub render_scale: f32,
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
        self.area.to_physical_precise_round(scale)
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
    let clip_radius = spec
        .clip
        .map(|clip| clip.radius.max(0) as f32)
        .unwrap_or(0.0);
    let radius = spec.radius.max(0) as f32;

    vec![
        Uniform::new("color", spec.color),
        Uniform::new("corner_radius", [radius, radius, radius, radius]),
        Uniform::new("border_width", border_width),
        Uniform::new("render_scale", spec.render_scale.max(1.0)),
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
