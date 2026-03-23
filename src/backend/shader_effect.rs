use std::{
    cmp::max,
    collections::HashMap,
    env, fs,
    io::Cursor,
    sync::{
        Mutex,
    },
};
use std::cell::RefCell;

use png::ColorType;
use resvg::{tiny_skia, usvg};
use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            damage::OutputDamageTracker,
            element::{Element, Id, Kind, RenderElement, UnderlyingStorage},
            element::texture::TextureRenderElement,
            gles::{
                element::TextureShaderElement, ffi, link_program, GlesError, GlesFrame,
                GlesPixelProgram, GlesRenderer, GlesTexProgram, GlesTexture, Uniform,
                UniformName,
            },
            utils::{CommitCounter, OpaqueRegions},
            Bind, ExportMem, Frame as _, FrameContext as _, ImportMem, Offscreen, Renderer, Texture, ContextId,
        },
    },
    utils::{user_data::UserDataMap, Buffer, Logical, Physical, Point, Rectangle, Scale, Size, Transform},
};
use tracing::{trace, warn};

use crate::ssd::{BlendMode, CompiledEffect, EffectInput, EffectStage, LogicalRect, NoiseKind, NoiseStage, ShaderModule, ShaderStage, ShaderUniformValue};

#[derive(Debug, Clone)]
pub struct CachedShaderEffect {
    pub stable_key: String,
    pub order: usize,
    pub rect: LogicalRect,
    pub shader: CompiledEffect,
    pub clip_rect: Option<LogicalRect>,
    pub clip_radius: i32,
}

#[derive(Debug, Clone)]
pub struct CachedBackdropTexture {
    pub signature: u64,
    pub texture: GlesTexture,
    pub id: Id,
    pub commit_counter: CommitCounter,
    pub sub_elements: HashMap<String, CachedBackdropSubElement>,
}

#[derive(Debug, Clone)]
pub struct CachedBackdropSubElement {
    pub id: Id,
    pub commit_counter: CommitCounter,
}

impl Default for CachedBackdropSubElement {
    fn default() -> Self {
        Self {
            id: Id::new(),
            commit_counter: CommitCounter::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ShaderEffectSpec {
    pub rect: Rectangle<i32, Logical>,
    pub shader: CompiledEffect,
    pub alpha_bits: u32,
    pub render_scale: f32,
    pub clip_rect: Option<Rectangle<i32, Logical>>,
    pub clip_radius: i32,
}

#[derive(Debug, Clone)]
pub struct ShaderEffectElementState {
    id: Id,
    commit_counter: CommitCounter,
    last_spec: Option<ShaderEffectSpec>,
}

impl Default for ShaderEffectElementState {
    fn default() -> Self {
        Self {
            id: Id::new(),
            commit_counter: CommitCounter::default(),
            last_spec: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct StableShaderEffectElement {
    shader: GlesPixelProgram,
    id: Id,
    commit_counter: CommitCounter,
    area: Rectangle<i32, Logical>,
    alpha: f32,
    additional_uniforms: Vec<Uniform<'static>>,
    kind: Kind,
}

#[derive(Debug, Clone)]
pub struct StableBackdropFramebufferElement {
    shader: CompiledEffect,
    program: GlesTexProgram,
    id: Id,
    commit_counter: CommitCounter,
    area: Rectangle<i32, Logical>,
    alpha: f32,
    render_scale: f32,
    clip_rect: Option<Rectangle<i32, Logical>>,
    clip_radius: i32,
    kind: Kind,
}

#[derive(Debug, Clone)]
pub struct StableBackdropTextureElement {
    texture: GlesTexture,
    program: GlesTexProgram,
    id: Id,
    commit_counter: CommitCounter,
    area: Rectangle<i32, Logical>,
    src: Rectangle<f64, Buffer>,
    alpha: f32,
    render_scale: f32,
    clip_rect: Option<Rectangle<i32, Logical>>,
    clip_radius: i32,
    uv_offset: [f32; 2],
    uv_scale: [f32; 2],
    kind: Kind,
}

#[derive(Debug, Default)]
struct BackdropFramebufferCache {
    framebuffer: Option<GlesTexture>,
    blurred: Option<GlesTexture>,
    sample_src: Option<Rectangle<f64, Buffer>>,
}

#[derive(Debug, Default)]
struct ShaderProgramCache(Mutex<HashMap<String, GlesPixelProgram>>);
#[derive(Debug, Clone)]
struct BlurShaderPrograms {
    down: BlurProgramInternal,
    up: BlurProgramInternal,
    renderer_context_id: ContextId<GlesTexture>,
}

#[derive(Debug, Default)]
struct BlurShaderProgramCache(Mutex<Option<BlurShaderPrograms>>);
#[derive(Debug, Default)]
struct TextureStageProgramCache(Mutex<HashMap<String, GlesTexProgram>>);
#[derive(Debug)]
struct DisplayTextureProgram(GlesTexProgram);
#[derive(Debug)]
struct NoiseSaltProgram(GlesTexProgram);
#[derive(Debug)]
struct OpaqueFinishProgram(GlesTexProgram);
#[derive(Debug, Default)]
struct ImageTextureCache(Mutex<HashMap<(String, i32, i32), GlesTexture>>);

struct EffectExecutionContext {
    backdrop: GlesTexture,
    size: (i32, i32),
    named: HashMap<String, GlesTexture>,
}

#[derive(Debug, Clone, Copy)]
struct BlurProgramInternal {
    program: ffi::types::GLuint,
    uniform_tex: ffi::types::GLint,
    uniform_half_pixel: ffi::types::GLint,
    uniform_offset: ffi::types::GLint,
    attrib_vert: ffi::types::GLint,
}

#[derive(Debug, Clone)]
struct BlendProgramInternal {
    program: ffi::types::GLuint,
    uniform_tex: ffi::types::GLint,
    uniform_tex2: ffi::types::GLint,
    uniform_blend_mode: ffi::types::GLint,
    uniform_blend_alpha: ffi::types::GLint,
    attrib_vert: ffi::types::GLint,
}

#[derive(Debug, Clone)]
struct BlendPrograms {
    program: BlendProgramInternal,
    renderer_context_id: ContextId<GlesTexture>,
}

#[derive(Debug, Default)]
struct BlendProgramCache(Mutex<Option<BlendPrograms>>);

#[derive(Debug, thiserror::Error)]
pub enum ShaderEffectError {
    #[error("failed to read shader source at {path}: {source}")]
    ReadShader {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error(transparent)]
    Gles(#[from] GlesError),
}

impl ShaderEffectElementState {
    pub fn element(
        &mut self,
        renderer: &mut GlesRenderer,
        spec: ShaderEffectSpec,
    ) -> Result<StableShaderEffectElement, ShaderEffectError> {
        if self.last_spec.as_ref() != Some(&spec) {
            self.commit_counter.increment();
            self.last_spec = Some(spec.clone());
        }

        let shader = compile_shader_program(renderer, &spec.shader)?;
        Ok(StableShaderEffectElement {
            shader,
            id: self.id.clone(),
            commit_counter: self.commit_counter,
            area: spec.rect,
            alpha: f32::from_bits(spec.alpha_bits).clamp(0.0, 1.0),
            additional_uniforms: uniforms_for_spec(&spec),
            kind: Kind::Unspecified,
        })
    }

    pub fn backdrop_element(
        &mut self,
        renderer: &mut GlesRenderer,
        spec: ShaderEffectSpec,
    ) -> Result<StableBackdropFramebufferElement, ShaderEffectError> {
        if self.last_spec.as_ref() != Some(&spec) {
            self.commit_counter.increment();
            self.last_spec = Some(spec.clone());
        }

        let program = compile_display_texture_program(renderer)?;
        Ok(StableBackdropFramebufferElement {
            shader: spec.shader,
            program,
            id: self.id.clone(),
            commit_counter: self.commit_counter,
            area: spec.rect,
            alpha: f32::from_bits(spec.alpha_bits).clamp(0.0, 1.0),
            render_scale: spec.render_scale,
            clip_rect: spec.clip_rect,
            clip_radius: spec.clip_radius,
            kind: Kind::Unspecified,
        })
    }
}

impl Element for StableShaderEffectElement {
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

impl RenderElement<GlesRenderer> for StableShaderEffectElement {
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

impl Element for StableBackdropFramebufferElement {
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

    fn is_framebuffer_effect(&self) -> bool {
        true
    }
}

impl RenderElement<GlesRenderer> for StableBackdropFramebufferElement {
    fn draw(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        _src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
        cache: Option<&UserDataMap>,
    ) -> Result<(), GlesError> {
        let Some(cache) = cache else {
            return Ok(());
        };
        let Some(inner) = cache.get::<RefCell<BackdropFramebufferCache>>() else {
            return Ok(());
        };
        let inner = inner.borrow();
        let Some(texture) = inner.blurred.as_ref().or(inner.framebuffer.as_ref()) else {
            return Ok(());
        };
        let sample_src = inner
            .sample_src
            .unwrap_or_else(|| Rectangle::from_size(texture.size().to_f64()));

        let clip_rect = self
            .clip_rect
            .map(|clip| {
                [
                    (clip.loc.x - self.area.loc.x) as f32,
                    (clip.loc.y - self.area.loc.y) as f32,
                    clip.size.w as f32,
                    clip.size.h as f32,
                ]
            })
            .unwrap_or([0.0, 0.0, 0.0, 0.0]);
        let radius = self.clip_radius.max(0) as f32;
        let full_size = texture.size();
        let uv_offset = [
            sample_src.loc.x as f32 / full_size.w.max(1) as f32,
            sample_src.loc.y as f32 / full_size.h.max(1) as f32,
        ];
        let uv_scale = [
            sample_src.size.w as f32 / full_size.w.max(1) as f32,
            sample_src.size.h as f32 / full_size.h.max(1) as f32,
        ];

        frame.render_texture_from_to(
            texture,
            sample_src,
            dst,
            damage,
            opaque_regions,
            Transform::Normal,
            self.alpha,
            Some(&self.program),
            &[
                Uniform::new("uv_offset", uv_offset),
                Uniform::new("uv_scale", uv_scale),
                Uniform::new(
                    "rect_size",
                    [self.area.size.w as f32, self.area.size.h as f32],
                ),
                Uniform::new("render_scale", self.render_scale.max(1.0)),
                Uniform::new(
                    "clip_enabled",
                    if clip_rect[2] > 0.0 && clip_rect[3] > 0.0 {
                        1.0f32
                    } else {
                        0.0f32
                    },
                ),
                Uniform::new("clip_rect", clip_rect),
                Uniform::new("clip_radius", [radius, radius, radius, radius]),
            ],
        )
    }

    fn capture_framebuffer(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        _src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        cache: &UserDataMap,
    ) -> Result<(), GlesError> {
        let inner = cache.get_or_insert::<RefCell<BackdropFramebufferCache>, _>(|| {
            RefCell::new(BackdropFramebufferCache::default())
        });
        let mut inner = inner.borrow_mut();
        let blur_padding = self
            .shader
            .blur_stage()
            .map(|blur| {
                let radius = blur.radius.max(1);
                let passes = blur.passes.max(1);
                (radius * passes * 24 + 32).max(32)
            })
            .unwrap_or(0);
        let output_rect = Rectangle::from_size(frame.output_size());
        let expanded_dst = Rectangle::new(
            (dst.loc.x - blur_padding, dst.loc.y - blur_padding).into(),
            (
                dst.size.w + blur_padding * 2,
                dst.size.h + blur_padding * 2,
            )
                .into(),
        );
        let clamped_dst = match expanded_dst.intersection(output_rect) {
            Some(clamped) => clamped,
            None => return Ok(()),
        };
        let size = Size::<i32, Buffer>::from((clamped_dst.size.w, clamped_dst.size.h));

        {
            let mut guard = frame.renderer();
            let renderer = guard.as_mut();
            let recreate = inner
                .framebuffer
                .as_ref()
                .map_or(true, |fb| fb.size() != size);
            if recreate {
                inner.framebuffer = Some(renderer.create_buffer(Fourcc::Abgr8888, size)?);
            }
            inner.blurred = None;
            inner.sample_src = Some(Rectangle::new(
                (
                    (dst.loc.x - clamped_dst.loc.x) as f64,
                    (dst.loc.y - clamped_dst.loc.y) as f64,
                )
                    .into(),
                (dst.size.w as f64, dst.size.h as f64).into(),
            ));
        }

        let framebuffer_texture = inner
            .framebuffer
            .as_ref()
            .expect("framebuffer texture should exist")
            .clone();

        frame.with_context(|gl| unsafe {
            while gl.GetError() != ffi::NO_ERROR {}

            let mut current_fbo = 0i32;
            gl.GetIntegerv(ffi::DRAW_FRAMEBUFFER_BINDING, &mut current_fbo as *mut _);
            gl.Disable(ffi::SCISSOR_TEST);

            let mut fbo = 0;
            gl.GenFramebuffers(1, &mut fbo as *mut _);
            gl.BindFramebuffer(ffi::DRAW_FRAMEBUFFER, fbo);
            gl.FramebufferTexture2D(
                ffi::DRAW_FRAMEBUFFER,
                ffi::COLOR_ATTACHMENT0,
                ffi::TEXTURE_2D,
                framebuffer_texture.tex_id(),
                0,
            );
            gl.BlitFramebuffer(
                clamped_dst.loc.x,
                clamped_dst.loc.y,
                clamped_dst.loc.x + clamped_dst.size.w,
                clamped_dst.loc.y + clamped_dst.size.h,
                0,
                0,
                size.w,
                size.h,
                ffi::COLOR_BUFFER_BIT,
                ffi::LINEAR,
            );
            gl.BindFramebuffer(ffi::DRAW_FRAMEBUFFER, current_fbo as u32);
            gl.Enable(ffi::SCISSOR_TEST);
            gl.DeleteFramebuffers(1, &fbo as *const _);
        })?;

        if let Some(blur) = self.shader.blur_stage() {
            let mut guard = frame.renderer();
            let renderer = guard.as_mut();
            match preblur_backdrop_texture(
                renderer,
                framebuffer_texture,
                (size.w, size.h),
                blur.radius,
                blur.passes,
            ) {
                Ok(texture) => inner.blurred = Some(texture),
                Err(err) => warn!(?err, "failed to preblur backdrop framebuffer"),
            }
        }

        Ok(())
    }

    fn underlying_storage(&self, _renderer: &mut GlesRenderer) -> Option<UnderlyingStorage<'_>> {
        None
    }
}

impl Element for StableBackdropTextureElement {
    fn id(&self) -> &Id {
        &self.id
    }

    fn current_commit(&self) -> CommitCounter {
        self.commit_counter
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        self.src
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

impl RenderElement<GlesRenderer> for StableBackdropTextureElement {
    fn draw(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
        _cache: Option<&UserDataMap>,
    ) -> Result<(), GlesError> {
        let clip_rect = self
            .clip_rect
            .map(|clip| {
                [
                    (clip.loc.x - self.area.loc.x) as f32,
                    (clip.loc.y - self.area.loc.y) as f32,
                    clip.size.w as f32,
                    clip.size.h as f32,
                ]
            })
            .unwrap_or([0.0, 0.0, 0.0, 0.0]);
        let radius = self.clip_radius.max(0) as f32;

        frame.render_texture_from_to(
            &self.texture,
            src,
            dst,
            damage,
            opaque_regions,
            Transform::Normal,
            self.alpha,
            Some(&self.program),
            &[
                Uniform::new("uv_offset", self.uv_offset),
                Uniform::new("uv_scale", self.uv_scale),
                Uniform::new(
                    "rect_size",
                    [self.area.size.w as f32, self.area.size.h as f32],
                ),
                Uniform::new("render_scale", self.render_scale.max(1.0)),
                Uniform::new(
                    "clip_enabled",
                    if clip_rect[2] > 0.0 && clip_rect[3] > 0.0 {
                        1.0f32
                    } else {
                        0.0f32
                    },
                ),
                Uniform::new("clip_rect", clip_rect),
                Uniform::new("clip_radius", [radius, radius, radius, radius]),
            ],
        )
    }
}

fn compile_shader_program(
    renderer: &mut GlesRenderer,
    shader: &CompiledEffect,
) -> Result<GlesPixelProgram, ShaderEffectError> {
    if renderer
        .egl_context()
        .user_data()
        .get::<ShaderProgramCache>()
        .is_none()
    {
        renderer
            .egl_context()
            .user_data()
            .insert_if_missing(ShaderProgramCache::default);
    }

    let shader_module = shader
        .last_shader_stage()
        .expect("pixel shader effects should always have a final shader stage");
    let cache_key = format!("pixel:{}", shader_module.shader.path);
    if let Some(program) = renderer
        .egl_context()
        .user_data()
        .get::<ShaderProgramCache>()
        .expect("shader effect cache should be initialized")
        .0
        .lock()
        .unwrap()
        .get(&cache_key)
        .cloned()
    {
        return Ok(program);
    }

    let source = fs::read_to_string(&shader_module.shader.path).map_err(|source| ShaderEffectError::ReadShader {
        path: shader_module.shader.path.clone(),
        source,
    })?;
    let program = renderer.compile_custom_pixel_shader(
        wrap_pixel_shader_source(&source),
        &[
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
    )?;
    renderer
        .egl_context()
        .user_data()
        .get::<ShaderProgramCache>()
        .expect("shader effect cache should be initialized")
        .0
        .lock()
        .unwrap()
        .insert(cache_key, program.clone());
    Ok(program)
}

pub fn compile_backdrop_shader_program(
    renderer: &mut GlesRenderer,
    shader: &ShaderModule,
) -> Result<GlesTexProgram, ShaderEffectError> {
    compile_texture_program(renderer, &shader.path, "display", true, None)
}

fn compile_display_texture_program(
    renderer: &mut GlesRenderer,
) -> Result<GlesTexProgram, ShaderEffectError> {
    if renderer
        .egl_context()
        .user_data()
        .get::<DisplayTextureProgram>()
        .is_none()
    {
        let program = renderer.compile_custom_texture_shader(
            wrap_backdrop_shader_source(
                r#"
vec4 shader_main(vec2 uv, vec2 rect_size) {
    return texture2D(tex, uv);
}
"#,
            ),
            &[
                UniformName::new("uv_offset", smithay::backend::renderer::gles::UniformType::_2f),
                UniformName::new("uv_scale", smithay::backend::renderer::gles::UniformType::_2f),
                UniformName::new("rect_size", smithay::backend::renderer::gles::UniformType::_2f),
                UniformName::new("render_scale", smithay::backend::renderer::gles::UniformType::_1f),
                UniformName::new("clip_enabled", smithay::backend::renderer::gles::UniformType::_1f),
                UniformName::new("clip_rect", smithay::backend::renderer::gles::UniformType::_4f),
                UniformName::new("clip_radius", smithay::backend::renderer::gles::UniformType::_4f),
            ],
        )?;
        renderer
            .egl_context()
            .user_data()
            .insert_if_missing(|| DisplayTextureProgram(program));
    }

    Ok(renderer
        .egl_context()
        .user_data()
        .get::<DisplayTextureProgram>()
        .expect("display texture shader should be initialized")
        .0
        .clone())
}

fn compile_noise_salt_program(
    renderer: &mut GlesRenderer,
) -> Result<GlesTexProgram, ShaderEffectError> {
    if renderer
        .egl_context()
        .user_data()
        .get::<NoiseSaltProgram>()
        .is_none()
    {
        let program = renderer.compile_custom_texture_shader(
            wrap_texture_stage_source(include_str!("noise_salt.frag")),
            &[
                UniformName::new("rect_size", smithay::backend::renderer::gles::UniformType::_2f),
                UniformName::new(
                    "noise_amount",
                    smithay::backend::renderer::gles::UniformType::_1f,
                ),
            ],
        )?;
        renderer
            .egl_context()
            .user_data()
            .insert_if_missing(|| NoiseSaltProgram(program));
    }

    Ok(renderer
        .egl_context()
        .user_data()
        .get::<NoiseSaltProgram>()
        .expect("noise salt shader should be initialized")
        .0
        .clone())
}

fn compile_opaque_finish_program(
    renderer: &mut GlesRenderer,
) -> Result<GlesTexProgram, ShaderEffectError> {
    if renderer
        .egl_context()
        .user_data()
        .get::<OpaqueFinishProgram>()
        .is_none()
    {
        let program = renderer.compile_custom_texture_shader(
            wrap_texture_stage_source(
                r#"
vec4 shader_main(vec2 uv, vec2 rect_size) {
    vec4 color = texture2D(tex, uv);
    color.a = 1.0;
    return color;
}
"#,
            ),
            &[UniformName::new(
                "rect_size",
                smithay::backend::renderer::gles::UniformType::_2f,
            )],
        )?;
        renderer
            .egl_context()
            .user_data()
            .insert_if_missing(|| OpaqueFinishProgram(program));
    }

    Ok(renderer
        .egl_context()
        .user_data()
        .get::<OpaqueFinishProgram>()
        .expect("opaque finish shader should be initialized")
        .0
        .clone())
}

fn blend_shader_programs(
    renderer: &mut GlesRenderer,
) -> Result<BlendPrograms, ShaderEffectError> {
    if renderer
        .egl_context()
        .user_data()
        .get::<BlendProgramCache>()
        .is_none()
    {
        renderer
            .egl_context()
            .user_data()
            .insert_if_missing(BlendProgramCache::default);
    }

    if let Some(programs) = renderer
        .egl_context()
        .user_data()
        .get::<BlendProgramCache>()
        .expect("blend shader cache should be initialized")
        .0
        .lock()
        .unwrap()
        .clone()
    {
        return Ok(programs);
    }

    let renderer_context_id = renderer.context_id();
    let programs = renderer.with_context(|gl| unsafe {
        let program = link_program(gl, include_str!("backdrop_blur.vert"), include_str!("blend_raw.frag"))?;
        let vert = c"vert";
        let tex = c"tex";
        let tex2 = c"tex2";
        let blend_mode = c"blend_mode";
        let blend_alpha = c"blend_alpha";

        Ok::<_, GlesError>(BlendPrograms {
            program: BlendProgramInternal {
                program,
                uniform_tex: gl.GetUniformLocation(program, tex.as_ptr()),
                uniform_tex2: gl.GetUniformLocation(program, tex2.as_ptr()),
                uniform_blend_mode: gl.GetUniformLocation(program, blend_mode.as_ptr()),
                uniform_blend_alpha: gl.GetUniformLocation(program, blend_alpha.as_ptr()),
                attrib_vert: gl.GetAttribLocation(program, vert.as_ptr()),
            },
            renderer_context_id,
        })
    })??;

    *renderer
        .egl_context()
        .user_data()
        .get::<BlendProgramCache>()
        .expect("blend shader cache should be initialized")
        .0
        .lock()
        .unwrap() = Some(programs.clone());

    Ok(programs)
}

fn compile_texture_stage_program(
    renderer: &mut GlesRenderer,
    stage: &ShaderStage,
) -> Result<GlesTexProgram, ShaderEffectError> {
    compile_texture_program(renderer, &stage.shader.path, "stage", false, Some(&stage.uniforms))
}

fn compile_texture_program(
    renderer: &mut GlesRenderer,
    path: &str,
    namespace: &str,
    with_clip: bool,
    uniforms: Option<&std::collections::BTreeMap<String, ShaderUniformValue>>,
) -> Result<GlesTexProgram, ShaderEffectError> {
    if renderer
        .egl_context()
        .user_data()
        .get::<TextureStageProgramCache>()
        .is_none()
    {
        renderer
            .egl_context()
            .user_data()
            .insert_if_missing(TextureStageProgramCache::default);
    }

    let mut cache_key = format!("{namespace}:{path}:{with_clip}");
    if let Some(uniforms) = uniforms {
        for (name, value) in uniforms {
            let kind = match value {
                ShaderUniformValue::Float(_) => "f1",
                ShaderUniformValue::Vec2(_) => "f2",
                ShaderUniformValue::Vec3(_) => "f3",
                ShaderUniformValue::Vec4(_) => "f4",
            };
            cache_key.push(':');
            cache_key.push_str(name);
            cache_key.push(':');
            cache_key.push_str(kind);
        }
    }
    if let Some(program) = renderer
        .egl_context()
        .user_data()
        .get::<TextureStageProgramCache>()
        .expect("texture stage cache should be initialized")
        .0
        .lock()
        .unwrap()
        .get(&cache_key)
        .cloned()
    {
        return Ok(program);
    }

    let source = fs::read_to_string(path).map_err(|source| ShaderEffectError::ReadShader {
        path: path.to_string(),
        source,
    })?;
    let wrapped = if with_clip {
        wrap_backdrop_shader_source(&source)
    } else {
        wrap_texture_stage_source(&source)
    };
    let mut uniform_names = if with_clip {
        vec![
            UniformName::new("uv_offset", smithay::backend::renderer::gles::UniformType::_2f),
            UniformName::new("uv_scale", smithay::backend::renderer::gles::UniformType::_2f),
            UniformName::new("rect_size", smithay::backend::renderer::gles::UniformType::_2f),
            UniformName::new("render_scale", smithay::backend::renderer::gles::UniformType::_1f),
            UniformName::new("clip_enabled", smithay::backend::renderer::gles::UniformType::_1f),
            UniformName::new("clip_rect", smithay::backend::renderer::gles::UniformType::_4f),
            UniformName::new("clip_radius", smithay::backend::renderer::gles::UniformType::_4f),
        ]
    } else {
        vec![UniformName::new(
            "rect_size",
            smithay::backend::renderer::gles::UniformType::_2f,
        )]
    };
    if let Some(uniforms) = uniforms {
        for (name, value) in uniforms {
            let ty = match value {
                ShaderUniformValue::Float(_) => smithay::backend::renderer::gles::UniformType::_1f,
                ShaderUniformValue::Vec2(_) => smithay::backend::renderer::gles::UniformType::_2f,
                ShaderUniformValue::Vec3(_) => smithay::backend::renderer::gles::UniformType::_3f,
                ShaderUniformValue::Vec4(_) => smithay::backend::renderer::gles::UniformType::_4f,
            };
            uniform_names.push(UniformName::new(name.clone(), ty));
        }
    }
    let program = renderer.compile_custom_texture_shader(wrapped, &uniform_names)?;
    renderer
        .egl_context()
        .user_data()
        .get::<TextureStageProgramCache>()
        .expect("texture stage cache should be initialized")
        .0
        .lock()
        .unwrap()
        .insert(cache_key, program.clone());
    Ok(program)
}

fn blur_shader_programs(
    renderer: &mut GlesRenderer,
) -> Result<BlurShaderPrograms, ShaderEffectError> {
    if renderer
        .egl_context()
        .user_data()
        .get::<BlurShaderProgramCache>()
        .is_none()
    {
        renderer
            .egl_context()
            .user_data()
            .insert_if_missing(BlurShaderProgramCache::default);
    }

    if let Some(programs) = renderer
        .egl_context()
        .user_data()
        .get::<BlurShaderProgramCache>()
        .expect("blur shader cache should be initialized")
        .0
        .lock()
        .unwrap()
        .clone()
    {
        return Ok(programs);
    }

    let renderer_context_id = renderer.context_id();
    let programs = renderer.with_context(|gl| unsafe {
        let down = compile_blur_program(gl, include_str!("backdrop_blur_down.frag"))?;
        let up = compile_blur_program(gl, include_str!("backdrop_blur_up.frag"))?;
        Ok::<_, GlesError>(BlurShaderPrograms {
            down,
            up,
            renderer_context_id,
        })
    })??;
    *renderer
        .egl_context()
        .user_data()
        .get::<BlurShaderProgramCache>()
        .expect("blur shader cache should be initialized")
        .0
        .lock()
        .unwrap() = Some(programs.clone());
    Ok(programs)
}

unsafe fn compile_blur_program(
    gl: &ffi::Gles2,
    src: &str,
) -> Result<BlurProgramInternal, GlesError> {
    let program = unsafe { link_program(gl, include_str!("backdrop_blur.vert"), src)? };

    let vert = c"vert";
    let tex = c"tex";
    let half_pixel = c"half_pixel";
    let offset = c"offset";

    Ok(BlurProgramInternal {
        program,
        uniform_tex: unsafe { gl.GetUniformLocation(program, tex.as_ptr()) },
        uniform_half_pixel: unsafe { gl.GetUniformLocation(program, half_pixel.as_ptr()) },
        uniform_offset: unsafe { gl.GetUniformLocation(program, offset.as_ptr()) },
        attrib_vert: unsafe { gl.GetAttribLocation(program, vert.as_ptr()) },
    })
}

fn wrap_pixel_shader_source(source: &str) -> String {
    format!(
        r#"
precision highp float;

uniform float alpha;
uniform vec2 size;
uniform float render_scale;
uniform float clip_enabled;
uniform vec4 clip_rect;
uniform vec4 clip_radius;

varying vec2 v_coords;

float rounded_rect_alpha(vec2 coords, vec2 rect_size, vec4 radius) {{
    if (coords.x < 0.0 || coords.y < 0.0 || coords.x > rect_size.x || coords.y > rect_size.y) {{
        return 0.0;
    }}

    vec2 center;
    float r;

    if (coords.x < radius.x && coords.y < radius.x) {{
        r = radius.x;
        center = vec2(r, r);
    }} else if (coords.x > rect_size.x - radius.y && coords.y < radius.y) {{
        r = radius.y;
        center = vec2(rect_size.x - r, r);
    }} else if (coords.x > rect_size.x - radius.z && coords.y > rect_size.y - radius.z) {{
        r = radius.z;
        center = vec2(rect_size.x - r, rect_size.y - r);
    }} else if (coords.x < radius.w && coords.y > rect_size.y - radius.w) {{
        r = radius.w;
        center = vec2(r, rect_size.y - r);
    }} else {{
        return 1.0;
    }}

    float dist = distance(coords, center);
    float half_px = 0.5 / max(render_scale, 1.0);
    return 1.0 - smoothstep(r - half_px, r + half_px, dist);
}}

{source}

void main() {{
    vec2 coords = v_coords * size;
    vec4 color = shader_main(coords, size);
    color.a *= alpha;
    color.rgb *= color.a;
    if (clip_enabled > 0.5) {{
        vec2 clip_coords = coords - clip_rect.xy;
        color *= rounded_rect_alpha(clip_coords, clip_rect.zw, clip_radius);
    }}
    gl_FragColor = color;
}}
"#
    )
}

fn wrap_backdrop_shader_source(source: &str) -> String {
    format!(
        r#"
//_DEFINES_

#if defined(EXTERNAL)
#extension GL_OES_EGL_image_external : require
#endif

precision mediump float;

#if defined(EXTERNAL)
uniform samplerExternalOES tex;
#else
uniform sampler2D tex;
#endif

uniform float alpha;
uniform vec2 uv_offset;
uniform vec2 uv_scale;
uniform vec2 rect_size;
uniform float render_scale;
uniform float clip_enabled;
uniform vec4 clip_rect;
uniform vec4 clip_radius;

varying vec2 v_coords;

#if defined(DEBUG_FLAGS)
uniform float tint;
#endif

float rounded_rect_alpha(vec2 coords, vec2 rect_size, vec4 radius) {{
    if (coords.x < 0.0 || coords.y < 0.0 || coords.x > rect_size.x || coords.y > rect_size.y) {{
        return 0.0;
    }}

    vec2 center;
    float r;

    if (coords.x < radius.x && coords.y < radius.x) {{
        r = radius.x;
        center = vec2(r, r);
    }} else if (coords.x > rect_size.x - radius.y && coords.y < radius.y) {{
        r = radius.y;
        center = vec2(rect_size.x - r, r);
    }} else if (coords.x > rect_size.x - radius.z && coords.y > rect_size.y - radius.z) {{
        r = radius.z;
        center = vec2(rect_size.x - r, rect_size.y - r);
    }} else if (coords.x < radius.w && coords.y > rect_size.y - radius.w) {{
        r = radius.w;
        center = vec2(r, rect_size.y - r);
    }} else {{
        return 1.0;
    }}

    float dist = distance(coords, center);
    float half_px = 0.5 / max(render_scale, 1.0);
    return 1.0 - smoothstep(r - half_px, r + half_px, dist);
}}

{source}

void main() {{
    vec2 local_uv = (v_coords - uv_offset) / max(uv_scale, vec2(0.0001));
    vec4 color = shader_main(v_coords, rect_size);
    color.a *= alpha;
    color.rgb *= color.a;

    if (clip_enabled > 0.5) {{
        vec2 coords = local_uv * rect_size;
        vec2 clip_coords = coords - clip_rect.xy;
        color *= rounded_rect_alpha(clip_coords, clip_rect.zw, clip_radius);
    }}

#if defined(DEBUG_FLAGS)
    if (tint == 1.0)
        color = vec4(0.0, 0.2, 0.0, 0.2) + color * 0.8;
#endif

    gl_FragColor = color;
}}
"#
    )
}

fn wrap_texture_stage_source(source: &str) -> String {
    format!(
        r#"
//_DEFINES_

#if defined(EXTERNAL)
#extension GL_OES_EGL_image_external : require
#endif

precision mediump float;

#if defined(EXTERNAL)
uniform samplerExternalOES tex;
#else
uniform sampler2D tex;
#endif

uniform vec2 rect_size;

varying vec2 v_coords;

{source}

void main() {{
    gl_FragColor = shader_main(v_coords, rect_size);
}}
"#
    )
}

fn uniforms_for_spec(spec: &ShaderEffectSpec) -> Vec<Uniform<'static>> {
    let clip_rect = spec
        .clip_rect
        .map(|rect| {
            [
                (rect.loc.x - spec.rect.loc.x) as f32,
                (rect.loc.y - spec.rect.loc.y) as f32,
                rect.size.w as f32,
                rect.size.h as f32,
            ]
        })
        .unwrap_or([0.0f32, 0.0f32, 0.0f32, 0.0f32]);
    let clip_radius = spec.clip_radius.max(0) as f32;
    vec![
        Uniform::new("render_scale", spec.render_scale.max(1.0)),
        Uniform::new(
            "clip_enabled",
            if spec.clip_rect.is_some() { 1.0f32 } else { 0.0f32 },
        ),
        Uniform::new("clip_rect", clip_rect),
        Uniform::new(
            "clip_radius",
            [clip_radius, clip_radius, clip_radius, clip_radius],
        ),
    ]
}

pub fn backdrop_shader_element(
    renderer: &mut GlesRenderer,
    element_id: Id,
    commit_counter: CommitCounter,
    texture: GlesTexture,
    display_rect: Rectangle<i32, Logical>,
    sample_rect: Rectangle<i32, Logical>,
    captured_rect: Rectangle<i32, Logical>,
    _shader: &CompiledEffect,
    alpha: f32,
    render_scale: f32,
    clip_rect: Option<Rectangle<i32, Logical>>,
    clip_radius: i32,
) -> Result<StableBackdropTextureElement, ShaderEffectError> {
    let program = compile_display_texture_program(renderer)?;
    let src = Rectangle::new(
        smithay::utils::Point::from((
            (sample_rect.loc.x - captured_rect.loc.x) as f64,
            (sample_rect.loc.y - captured_rect.loc.y) as f64,
        )),
        (sample_rect.size.w as f64, sample_rect.size.h as f64).into(),
    );
    let uv_offset = [
        (sample_rect.loc.x - captured_rect.loc.x) as f32 / captured_rect.size.w.max(1) as f32,
        (sample_rect.loc.y - captured_rect.loc.y) as f32 / captured_rect.size.h.max(1) as f32,
    ];
    let uv_scale = [
        sample_rect.size.w as f32 / captured_rect.size.w.max(1) as f32,
        sample_rect.size.h as f32 / captured_rect.size.h.max(1) as f32,
    ];
    Ok(StableBackdropTextureElement {
        texture,
        program,
        id: element_id,
        commit_counter,
        area: display_rect,
        src,
        alpha: alpha.clamp(0.0, 1.0),
        render_scale,
        clip_rect,
        clip_radius,
        uv_offset,
        uv_scale,
        kind: Kind::Unspecified,
    })
}

pub fn apply_effect_pipeline(
    renderer: &mut GlesRenderer,
    texture: GlesTexture,
    size: (i32, i32),
    sample_region: Option<Rectangle<i32, Buffer>>,
    output_size: Option<(i32, i32)>,
    effect: &CompiledEffect,
) -> Result<GlesTexture, ShaderEffectError> {
    let mut ctx = EffectExecutionContext {
        backdrop: texture,
        size,
        named: HashMap::new(),
    };
    run_effect_pipeline(renderer, effect, &mut ctx, sample_region, output_size)
}

fn run_effect_pipeline(
    renderer: &mut GlesRenderer,
    effect: &CompiledEffect,
    ctx: &mut EffectExecutionContext,
    sample_region: Option<Rectangle<i32, Buffer>>,
    output_size: Option<(i32, i32)>,
) -> Result<GlesTexture, ShaderEffectError> {
    let mut current = resolve_effect_input(renderer, &effect.input, ctx)?;
    let mut current_size = ctx.size;
    let mut pending_sample_region = sample_region;

    for stage in &effect.pipeline {
        current = match stage {
            EffectStage::Noise(noise) => {
                apply_noise_stage(renderer, current, current_size, noise.clone())?
            }
            EffectStage::DualKawaseBlur(blur) => {
                preblur_backdrop_texture(renderer, current, current_size, blur.radius, blur.passes)?
            }
            EffectStage::Shader(shader) => {
                if let Some(region) = pending_sample_region.take() {
                    let target_size = output_size.unwrap_or((region.size.w, region.size.h));
                    current = crop_texture_region(renderer, current, current_size, region, target_size)?;
                    current_size = target_size;
                }
                apply_texture_shader_stage(renderer, current, current_size, shader)?
            }
            EffectStage::Save(name) => {
                ctx.named.insert(name.clone(), current.clone());
                current
            }
            EffectStage::Blend { input, mode, alpha } => {
                if let Some(region) = pending_sample_region.take() {
                    let target_size = output_size.unwrap_or((region.size.w, region.size.h));
                    current = crop_texture_region(renderer, current, current_size, region, target_size)?;
                    current_size = target_size;
                }
                let other = resolve_effect_input(renderer, input, ctx)?;
                apply_blend_stage(renderer, current, other, current_size, *mode, *alpha)?
            }
            EffectStage::Unit(effect) => {
                let _ = run_effect_pipeline(renderer, effect, ctx, pending_sample_region, output_size)?;
                current
            }
        };
    }

    if effect.is_backdrop() {
        if let Some(region) = pending_sample_region.take() {
            let target_size = output_size.unwrap_or((region.size.w, region.size.h));
            current = crop_texture_region(renderer, current, current_size, region, target_size)?;
            current_size = target_size;
        }
        let program = compile_opaque_finish_program(renderer)?;
        current = apply_texture_program(
            renderer,
            current,
            current_size,
            program,
            vec![Uniform::new("rect_size", [current_size.0 as f32, current_size.1 as f32])],
        )?;
    }

    Ok(current)
}

fn resolve_effect_input(
    renderer: &mut GlesRenderer,
    input: &EffectInput,
    ctx: &mut EffectExecutionContext,
) -> Result<GlesTexture, ShaderEffectError> {
    match input {
        EffectInput::Backdrop => Ok(ctx.backdrop.clone()),
        EffectInput::Named(name) => ctx
            .named
            .get(name)
            .cloned()
            .ok_or(ShaderEffectError::Gles(GlesError::FramebufferBindingError)),
        EffectInput::Image(path) => load_image_texture(renderer, path, ctx.size),
    }
}

fn apply_texture_shader_stage(
    renderer: &mut GlesRenderer,
    texture: GlesTexture,
    size: (i32, i32),
    stage: &ShaderStage,
) -> Result<GlesTexture, ShaderEffectError> {
    let program = compile_texture_stage_program(renderer, stage)?;
    let mut uniforms = vec![Uniform::new("rect_size", [size.0 as f32, size.1 as f32])];
    for (name, value) in &stage.uniforms {
        let uniform = match value {
            ShaderUniformValue::Float(value) => Uniform::new(name.clone(), *value),
            ShaderUniformValue::Vec2(value) => Uniform::new(name.clone(), *value),
            ShaderUniformValue::Vec3(value) => Uniform::new(name.clone(), *value),
            ShaderUniformValue::Vec4(value) => Uniform::new(name.clone(), *value),
        };
        uniforms.push(uniform);
    }
    apply_texture_program(renderer, texture, size, program, uniforms)
}

fn apply_noise_stage(
    renderer: &mut GlesRenderer,
    texture: GlesTexture,
    size: (i32, i32),
    noise: NoiseStage,
) -> Result<GlesTexture, ShaderEffectError> {
    match noise.kind {
        NoiseKind::Salt => {
            let program = compile_noise_salt_program(renderer)?;
            apply_texture_program(
                renderer,
                texture,
                size,
                program,
                vec![
                    Uniform::new("rect_size", [size.0 as f32, size.1 as f32]),
                    Uniform::new("noise_amount", noise.amount),
                ],
            )
        }
    }
}

fn apply_blend_stage(
    renderer: &mut GlesRenderer,
    current: GlesTexture,
    other: GlesTexture,
    size: (i32, i32),
    mode: BlendMode,
    alpha: f32,
) -> Result<GlesTexture, ShaderEffectError> {
    let programs = blend_shader_programs(renderer)?;
    if programs.renderer_context_id != renderer.context_id() {
        return Err(ShaderEffectError::Gles(GlesError::FramebufferBindingError));
    }

    let target =
        Offscreen::<GlesTexture>::create_buffer(renderer, Fourcc::Abgr8888, size.into())?;
    renderer.with_context(|gl| unsafe {
        while gl.GetError() != ffi::NO_ERROR {}

        gl.Disable(ffi::BLEND);
        gl.Disable(ffi::SCISSOR_TEST);
        gl.ActiveTexture(ffi::TEXTURE0);

        let mut fbo = 0;
        gl.GenFramebuffers(1, &mut fbo as *mut _);
        gl.BindFramebuffer(ffi::DRAW_FRAMEBUFFER, fbo);
        gl.FramebufferTexture2D(
            ffi::DRAW_FRAMEBUFFER,
            ffi::COLOR_ATTACHMENT0,
            ffi::TEXTURE_2D,
            target.tex_id(),
            0,
        );

        gl.Viewport(0, 0, size.0, size.1);
        gl.UseProgram(programs.program.program);
        gl.Uniform1i(programs.program.uniform_tex, 0);
        gl.Uniform1i(programs.program.uniform_tex2, 1);
        gl.Uniform1f(programs.program.uniform_blend_mode, blend_mode_value(mode));
        gl.Uniform1f(programs.program.uniform_blend_alpha, alpha.clamp(0.0, 1.0));

        let vertices: [f32; 12] = [
            0.0, 0.0, 0.0, 1.0, 1.0, 1.0,
            0.0, 0.0, 1.0, 1.0, 1.0, 0.0,
        ];
        gl.EnableVertexAttribArray(programs.program.attrib_vert as u32);
        gl.BindBuffer(ffi::ARRAY_BUFFER, 0);
        gl.VertexAttribPointer(
            programs.program.attrib_vert as u32,
            2,
            ffi::FLOAT,
            ffi::FALSE,
            0,
            vertices.as_ptr().cast(),
        );

        gl.ActiveTexture(ffi::TEXTURE0);
        gl.BindTexture(ffi::TEXTURE_2D, current.tex_id());
        gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_MIN_FILTER, ffi::LINEAR as i32);
        gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_MAG_FILTER, ffi::LINEAR as i32);

        gl.ActiveTexture(ffi::TEXTURE1);
        gl.BindTexture(ffi::TEXTURE_2D, other.tex_id());
        gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_MIN_FILTER, ffi::LINEAR as i32);
        gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_MAG_FILTER, ffi::LINEAR as i32);

        gl.DrawArrays(ffi::TRIANGLES, 0, 6);

        gl.BindTexture(ffi::TEXTURE_2D, 0);
        gl.ActiveTexture(ffi::TEXTURE0);
        gl.BindTexture(ffi::TEXTURE_2D, 0);
        gl.DisableVertexAttribArray(programs.program.attrib_vert as u32);
        gl.UseProgram(0);
        gl.BindFramebuffer(ffi::DRAW_FRAMEBUFFER, 0);
        gl.Enable(ffi::SCISSOR_TEST);
        gl.DeleteFramebuffers(1, &fbo as *const _);
        Ok::<_, GlesError>(())
    })??;

    Ok(target)
}

fn blend_mode_value(mode: BlendMode) -> f32 {
    match mode {
        BlendMode::Normal => 0.0,
        BlendMode::Add => 1.0,
        BlendMode::Screen => 2.0,
        BlendMode::Multiply => 3.0,
    }
}

fn crop_texture_region(
    renderer: &mut GlesRenderer,
    texture: GlesTexture,
    _size: (i32, i32),
    region: Rectangle<i32, Buffer>,
    output_size: (i32, i32),
) -> Result<GlesTexture, ShaderEffectError> {
    let mut target = Offscreen::<GlesTexture>::create_buffer(
        renderer,
        Fourcc::Abgr8888,
        output_size.into(),
    )?;
    let element = TextureRenderElement::from_static_texture(
        Id::new(),
        renderer.context_id(),
        Point::<f64, Physical>::from((0.0, 0.0)),
        texture,
        1,
        Transform::Normal,
        Some(1.0),
        Some(Rectangle::new(
            Point::from((region.loc.x as f64, region.loc.y as f64)),
            (region.size.w as f64, region.size.h as f64).into(),
        )),
        Some(output_size.into()),
        None,
        Kind::Unspecified,
    );
    let mut framebuffer = renderer.bind(&mut target)?;
    let mut damage_tracker =
        OutputDamageTracker::new((region.size.w, region.size.h), 1.0, Transform::Normal);
    let _ = damage_tracker
        .render_output(renderer, &mut framebuffer, 0, &[element], [0.0, 0.0, 0.0, 0.0])
        .map_err(|_| GlesError::FramebufferBindingError)?;
    drop(framebuffer);
    Ok(target)
}

fn apply_texture_program(
    renderer: &mut GlesRenderer,
    texture: GlesTexture,
    size: (i32, i32),
    program: GlesTexProgram,
    uniforms: Vec<Uniform<'static>>,
) -> Result<GlesTexture, ShaderEffectError> {
    let mut target = Offscreen::<GlesTexture>::create_buffer(renderer, Fourcc::Abgr8888, size.into())?;
    let inner = TextureRenderElement::from_static_texture(
        Id::new(),
        renderer.context_id(),
        Point::<f64, Physical>::from((0.0, 0.0)),
        texture,
        1,
        Transform::Normal,
        Some(1.0),
        None,
        Some((size.0, size.1).into()),
        None,
        Kind::Unspecified,
    );
    let element = TextureShaderElement::new(inner, program, uniforms);
    let mut framebuffer = renderer.bind(&mut target)?;
    let mut damage_tracker = OutputDamageTracker::new(size, 1.0, Transform::Normal);
    let _ = damage_tracker
        .render_output(renderer, &mut framebuffer, 0, &[element], [0.0, 0.0, 0.0, 0.0])
        .map_err(|_| GlesError::FramebufferBindingError)?;
    drop(framebuffer);
    Ok(target)
}

fn load_image_texture(
    renderer: &mut GlesRenderer,
    path: &str,
    size: (i32, i32),
) -> Result<GlesTexture, ShaderEffectError> {
    if renderer
        .egl_context()
        .user_data()
        .get::<ImageTextureCache>()
        .is_none()
    {
        renderer
            .egl_context()
            .user_data()
            .insert_if_missing(ImageTextureCache::default);
    }

    let cache_key = (path.to_string(), size.0, size.1);
    if let Some(texture) = renderer
        .egl_context()
        .user_data()
        .get::<ImageTextureCache>()
        .expect("image texture cache should exist")
        .0
        .lock()
        .unwrap()
        .get(&cache_key)
        .cloned()
    {
        return Ok(texture);
    }

    let bytes = fs::read(path).map_err(|source| ShaderEffectError::ReadShader {
        path: path.to_string(),
        source,
    })?;
    let extension = std::path::Path::new(path)
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase());
    let rgba = match extension.as_deref() {
        Some("png") => decode_png_and_scale(&bytes, size.0, size.1),
        Some("svg") => decode_svg_and_scale(&bytes, size.0, size.1),
        _ => decode_png_and_scale(&bytes, size.0, size.1),
    }
    .ok_or(ShaderEffectError::Gles(GlesError::FramebufferBindingError))?;

    let texture = renderer.import_memory(&rgba, Fourcc::Abgr8888, size.into(), false)?;
    renderer
        .egl_context()
        .user_data()
        .get::<ImageTextureCache>()
        .expect("image texture cache should exist")
        .0
        .lock()
        .unwrap()
        .insert(cache_key, texture.clone());
    Ok(texture)
}

fn decode_png_and_scale(bytes: &[u8], target_width: i32, target_height: i32) -> Option<Vec<u8>> {
    let decoder = png::Decoder::new(Cursor::new(bytes));
    let mut reader = decoder.read_info().ok()?;
    let mut buffer = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buffer).ok()?;
    let source = &buffer[..info.buffer_size()];
    let rgba = match info.color_type {
        ColorType::Rgba => source.to_vec(),
        ColorType::Rgb => source
            .chunks_exact(3)
            .flat_map(|chunk| [chunk[0], chunk[1], chunk[2], 255])
            .collect(),
        ColorType::GrayscaleAlpha => source
            .chunks_exact(2)
            .flat_map(|chunk| [chunk[0], chunk[0], chunk[0], chunk[1]])
            .collect(),
        ColorType::Grayscale => source
            .iter()
            .flat_map(|value| [*value, *value, *value, 255])
            .collect(),
        _ => return None,
    };

    Some(scale_rgba(
        &rgba,
        info.width as i32,
        info.height as i32,
        target_width,
        target_height,
    ))
}

fn decode_svg_and_scale(bytes: &[u8], target_width: i32, target_height: i32) -> Option<Vec<u8>> {
    let options = usvg::Options::default();
    let tree = usvg::Tree::from_data(bytes, &options).ok()?;
    let mut pixmap = tiny_skia::Pixmap::new(target_width as u32, target_height as u32)?;
    let size = tree.size();
    let sx = target_width as f32 / size.width();
    let sy = target_height as f32 / size.height();
    let transform = tiny_skia::Transform::from_scale(sx, sy);
    resvg::render(&tree, transform, &mut pixmap.as_mut());
    Some(pixmap.data().to_vec())
}

fn scale_rgba(
    rgba: &[u8],
    source_width: i32,
    source_height: i32,
    target_width: i32,
    target_height: i32,
) -> Vec<u8> {
    if source_width == target_width && source_height == target_height {
        return rgba.to_vec();
    }

    let mut scaled = vec![0u8; (target_width * target_height * 4) as usize];
    for y in 0..target_height {
        for x in 0..target_width {
            let source_x = ((x as f32 / target_width as f32) * source_width as f32).floor() as i32;
            let source_y = ((y as f32 / target_height as f32) * source_height as f32).floor() as i32;
            let source_x = source_x.clamp(0, source_width - 1);
            let source_y = source_y.clamp(0, source_height - 1);
            let source_index = ((source_y * source_width + source_x) * 4) as usize;
            let target_index = ((y * target_width + x) * 4) as usize;
            scaled[target_index..target_index + 4]
                .copy_from_slice(&rgba[source_index..source_index + 4]);
        }
    }
    scaled
}

pub fn preblur_backdrop_texture(
    renderer: &mut GlesRenderer,
    texture: GlesTexture,
    size: (i32, i32),
    radius: i32,
    passes: i32,
) -> Result<GlesTexture, ShaderEffectError> {
    if radius <= 0 || passes <= 0 {
        return Ok(texture);
    }

    let programs = blur_shader_programs(renderer)?;
    let passes = passes.clamp(1, 8) as usize;
    let offset = radius.max(1) as f32;
    let debug = env::var_os("SHOJI_BACKDROP_DEBUG").is_some();
    if programs.renderer_context_id != renderer.context_id() {
        return Err(ShaderEffectError::Gles(GlesError::FramebufferBindingError));
    }

    let mut levels = Vec::with_capacity(passes + 1);
    let mut current = texture;
    let mut current_size = size;
    if debug {
        debug_texture_readback(renderer, &mut current, size, "backdrop-source");
    }
    levels.push((current.clone(), current_size));

    for _ in 0..passes {
        let next_size = (
            max(1, current_size.0 / 2),
            max(1, current_size.1 / 2),
        );
        current = blur_texture_pass(
            renderer,
            current,
            next_size,
            &programs.down,
            [0.5f32 / next_size.0 as f32, 0.5f32 / next_size.1 as f32],
            offset,
        )?;
        current_size = next_size;
        levels.push((current.clone(), current_size));
    }

    for idx in (1..levels.len()).rev() {
        let (src_texture, src_size) = levels[idx].clone();
        let dst_size = levels[idx - 1].1;
        current = blur_texture_pass(
            renderer,
            src_texture,
            dst_size,
            &programs.up,
            [0.5f32 / src_size.0 as f32, 0.5f32 / src_size.1 as f32],
            offset,
        )?;
        levels[idx - 1].0 = current.clone();
    }

    if debug {
        debug_texture_readback(renderer, &mut current, size, "backdrop-blurred");
    }

    Ok(current)
}

fn blur_texture_pass(
    renderer: &mut GlesRenderer,
    texture: GlesTexture,
    output_size: (i32, i32),
    program: &BlurProgramInternal,
    half_pixel: [f32; 2],
    offset: f32,
) -> Result<GlesTexture, ShaderEffectError> {
    let target =
        Offscreen::<GlesTexture>::create_buffer(renderer, Fourcc::Abgr8888, output_size.into())?;
    renderer.with_context(|gl| unsafe {
        while gl.GetError() != ffi::NO_ERROR {}

        gl.Disable(ffi::BLEND);
        gl.Disable(ffi::SCISSOR_TEST);
        gl.ActiveTexture(ffi::TEXTURE0);

        let mut fbo = 0;
        gl.GenFramebuffers(1, &mut fbo as *mut _);
        gl.BindFramebuffer(ffi::DRAW_FRAMEBUFFER, fbo);
        gl.FramebufferTexture2D(
            ffi::DRAW_FRAMEBUFFER,
            ffi::COLOR_ATTACHMENT0,
            ffi::TEXTURE_2D,
            target.tex_id(),
            0,
        );

        gl.Viewport(0, 0, output_size.0, output_size.1);
        gl.UseProgram(program.program);
        gl.Uniform1i(program.uniform_tex, 0);
        gl.Uniform2f(program.uniform_half_pixel, half_pixel[0], half_pixel[1]);
        gl.Uniform1f(program.uniform_offset, offset);

        let vertices: [f32; 12] = [
            0.0, 0.0, 0.0, 1.0, 1.0, 1.0,
            0.0, 0.0, 1.0, 1.0, 1.0, 0.0,
        ];
        gl.EnableVertexAttribArray(program.attrib_vert as u32);
        gl.BindBuffer(ffi::ARRAY_BUFFER, 0);
        gl.VertexAttribPointer(
            program.attrib_vert as u32,
            2,
            ffi::FLOAT,
            ffi::FALSE,
            0,
            vertices.as_ptr().cast(),
        );

        gl.BindTexture(ffi::TEXTURE_2D, texture.tex_id());
        gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_MIN_FILTER, ffi::LINEAR as i32);
        gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_MAG_FILTER, ffi::LINEAR as i32);
        gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_WRAP_S, ffi::CLAMP_TO_EDGE as i32);
        gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_WRAP_T, ffi::CLAMP_TO_EDGE as i32);
        gl.DrawArrays(ffi::TRIANGLES, 0, 6);

        gl.DisableVertexAttribArray(program.attrib_vert as u32);
        gl.BindFramebuffer(ffi::DRAW_FRAMEBUFFER, 0);
        gl.DeleteFramebuffers(1, &fbo as *const _);
    })?;

    Ok(target)
}

fn debug_texture_readback(
    renderer: &mut GlesRenderer,
    texture: &mut GlesTexture,
    size: (i32, i32),
    label: &str,
) {
    let Ok(framebuffer) = renderer.bind(texture) else {
        return;
    };
    let Ok(mapping) = renderer.copy_framebuffer(
        &framebuffer,
        Rectangle::from_size((size.0, size.1).into()),
        Fourcc::Abgr8888,
    ) else {
        return;
    };
    let Ok(copy): Result<&[u8], _> = renderer.map_texture(&mapping) else {
        return;
    };

    if size.0 <= 0 || size.1 <= 0 {
        return;
    }

    let cx = (size.0 / 2).max(0) as usize;
    let cy = (size.1 / 2).max(0) as usize;
    let idx = (cy * size.0 as usize + cx) * 4;
    if idx + 3 >= copy.len() {
        return;
    }

    trace!(
        label,
        width = size.0,
        height = size.1,
        center = ?[copy[idx], copy[idx + 1], copy[idx + 2], copy[idx + 3]],
        "shader texture readback"
    );
}
