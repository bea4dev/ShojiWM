use std::{
    cmp::max,
    collections::HashMap,
    env, fs,
    io::BufWriter,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Mutex,
    },
};
use std::cell::RefCell;

use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            element::{Element, Id, Kind, RenderElement, UnderlyingStorage},
            element::texture::TextureRenderElement,
            gles::{
                element::TextureShaderElement, ffi, link_program, GlesError, GlesFrame,
                GlesPixelProgram, GlesRenderer, GlesTexProgram, GlesTexture, Uniform,
                UniformName,
            },
            utils::{CommitCounter, OpaqueRegions},
            Bind, ExportMem, Frame as _, FrameContext as _, Offscreen, Renderer, Texture, ContextId,
        },
    },
    utils::{user_data::UserDataMap, Buffer, Logical, Physical, Rectangle, Scale, Size, Transform},
};
use tracing::{trace, warn};

use crate::ssd::{CompiledShader, LogicalRect, ShaderType};

#[derive(Debug, Clone)]
pub struct CachedShaderEffect {
    pub stable_key: String,
    pub order: usize,
    pub rect: LogicalRect,
    pub shader: CompiledShader,
    pub clip_rect: Option<LogicalRect>,
    pub clip_radius: i32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ShaderEffectSpec {
    pub rect: Rectangle<i32, Logical>,
    pub shader: CompiledShader,
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
    shader: CompiledShader,
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

#[derive(Debug, Default)]
struct BackdropFramebufferCache {
    framebuffer: Option<GlesTexture>,
    blurred: Option<GlesTexture>,
    sample_src: Option<Rectangle<f64, Buffer>>,
}

static BACKDROP_DUMP_REQUESTED: AtomicBool = AtomicBool::new(false);
static BACKDROP_DUMP_COUNTER: AtomicU64 = AtomicU64::new(0);

pub fn request_backdrop_dump() {
    BACKDROP_DUMP_REQUESTED.store(true, Ordering::SeqCst);
}

pub fn consume_backdrop_dump_request() -> Option<u64> {
    BACKDROP_DUMP_REQUESTED
        .swap(false, Ordering::SeqCst)
        .then(|| BACKDROP_DUMP_COUNTER.fetch_add(1, Ordering::SeqCst))
}

#[derive(Debug, Default)]
struct ShaderProgramCache(Mutex<HashMap<String, GlesPixelProgram>>);
#[derive(Debug, Default)]
struct BackdropShaderProgramCache(Mutex<HashMap<String, GlesTexProgram>>);
#[derive(Debug, Clone)]
struct BlurShaderPrograms {
    down: BlurProgramInternal,
    up: BlurProgramInternal,
    renderer_context_id: ContextId<GlesTexture>,
}

#[derive(Debug, Default)]
struct BlurShaderProgramCache(Mutex<Option<BlurShaderPrograms>>);

#[derive(Debug, Clone, Copy)]
struct BlurProgramInternal {
    program: ffi::types::GLuint,
    uniform_tex: ffi::types::GLint,
    uniform_half_pixel: ffi::types::GLint,
    uniform_offset: ffi::types::GLint,
    attrib_vert: ffi::types::GLint,
}

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

        let program = compile_backdrop_shader_program(renderer, &spec.shader)?;
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
        trace!(dst = ?dst, alpha = self.alpha, "drawing backdrop framebuffer effect");
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
        trace!(dst = ?dst, "capturing backdrop framebuffer effect");
        let inner = cache.get_or_insert::<RefCell<BackdropFramebufferCache>, _>(|| {
            RefCell::new(BackdropFramebufferCache::default())
        });
        let mut inner = inner.borrow_mut();
        let blur_padding = self
            .shader
            .blur
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

        if let Some(blur) = self.shader.blur {
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

        if BACKDROP_DUMP_REQUESTED.swap(false, Ordering::SeqCst) {
            let mut guard = frame.renderer();
            let renderer = guard.as_mut();
            let dump_id = BACKDROP_DUMP_COUNTER.fetch_add(1, Ordering::SeqCst);
            if let Some(framebuffer) = inner.framebuffer.as_mut() {
                dump_backdrop_texture_png(renderer, framebuffer, size, dump_path(dump_id, "source"));
            }
            if let Some(blurred) = inner.blurred.as_mut() {
                dump_backdrop_texture_png(renderer, blurred, size, dump_path(dump_id, "blurred"));
            }
        }

        Ok(())
    }

    fn underlying_storage(&self, _renderer: &mut GlesRenderer) -> Option<UnderlyingStorage<'_>> {
        None
    }
}

fn dump_path(id: u64, suffix: &str) -> PathBuf {
    PathBuf::from(format!("/tmp/shoji_backdrop_dump_{id}_{suffix}.png"))
}

pub fn dump_backdrop_texture_png(
    renderer: &mut GlesRenderer,
    texture: &mut GlesTexture,
    size: Size<i32, Buffer>,
    path: PathBuf,
) {
    let Ok(framebuffer) = renderer.bind(texture) else {
        return;
    };
    let Ok(mapping) = renderer.copy_framebuffer(
        &framebuffer,
        Rectangle::from_size((size.w, size.h).into()),
        Fourcc::Abgr8888,
    ) else {
        return;
    };
    let Ok(copy): Result<&[u8], _> = renderer.map_texture(&mapping) else {
        return;
    };

    let Ok(file) = std::fs::File::create(&path) else {
        return;
    };
    let writer = BufWriter::new(file);
    let mut encoder = png::Encoder::new(writer, size.w.max(0) as u32, size.h.max(0) as u32);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let Ok(mut png_writer) = encoder.write_header() else {
        return;
    };
    if png_writer.write_image_data(copy).is_ok() {
        trace!(path = %path.display(), "wrote backdrop dump png");
    }
}

fn compile_shader_program(
    renderer: &mut GlesRenderer,
    shader: &CompiledShader,
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

    let cache_key = format!("{:?}:{}", shader.shader_type, shader.path);
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

    let source = fs::read_to_string(&shader.path).map_err(|source| ShaderEffectError::ReadShader {
        path: shader.path.clone(),
        source,
    })?;
    let program = match shader.shader_type {
        ShaderType::Pixel => renderer.compile_custom_pixel_shader(
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
        )?,
        ShaderType::Backdrop => renderer.compile_custom_pixel_shader(
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
        )?,
    };
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
    shader: &CompiledShader,
) -> Result<GlesTexProgram, ShaderEffectError> {
    if renderer
        .egl_context()
        .user_data()
        .get::<BackdropShaderProgramCache>()
        .is_none()
    {
        renderer
            .egl_context()
            .user_data()
            .insert_if_missing(BackdropShaderProgramCache::default);
    }

    let cache_key = format!("{:?}:{}", shader.shader_type, shader.path);
    if let Some(program) = renderer
        .egl_context()
        .user_data()
        .get::<BackdropShaderProgramCache>()
        .expect("backdrop shader cache should be initialized")
        .0
        .lock()
        .unwrap()
        .get(&cache_key)
        .cloned()
    {
        return Ok(program);
    }

    let source = fs::read_to_string(&shader.path).map_err(|source| ShaderEffectError::ReadShader {
        path: shader.path.clone(),
        source,
    })?;
    let program = renderer.compile_custom_texture_shader(
        wrap_backdrop_shader_source(&source),
        &[
            UniformName::new(
                "uv_offset",
                smithay::backend::renderer::gles::UniformType::_2f,
            ),
            UniformName::new(
                "uv_scale",
                smithay::backend::renderer::gles::UniformType::_2f,
            ),
            UniformName::new(
                "rect_size",
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
    )?;
    renderer
        .egl_context()
        .user_data()
        .get::<BackdropShaderProgramCache>()
        .expect("backdrop shader cache should be initialized")
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
    texture: GlesTexture,
    display_rect: Rectangle<i32, Logical>,
    sample_rect: Rectangle<i32, Logical>,
    captured_rect: Rectangle<i32, Logical>,
    shader: &CompiledShader,
    alpha: f32,
    render_scale: f32,
    clip_rect: Option<Rectangle<i32, Logical>>,
    clip_radius: i32,
) -> Result<TextureShaderElement, ShaderEffectError> {
    let program = compile_backdrop_shader_program(renderer, shader)?;
    let inner = TextureRenderElement::from_static_texture(
        Id::new(),
        renderer.context_id(),
        smithay::utils::Point::<f64, Physical>::from((
            display_rect.loc.x as f64,
            display_rect.loc.y as f64,
        )),
        texture,
        1,
        Transform::Normal,
        Some(alpha.clamp(0.0, 1.0)),
        Some(Rectangle::new(
            smithay::utils::Point::from((
                (sample_rect.loc.x - captured_rect.loc.x) as f64,
                (sample_rect.loc.y - captured_rect.loc.y) as f64,
            )),
            (sample_rect.size.w as f64, sample_rect.size.h as f64).into(),
        )),
        Some((display_rect.size.w, display_rect.size.h).into()),
        None,
        Kind::Unspecified,
    );
    let clip_rect = clip_rect
        .map(|clip| {
            [
                (clip.loc.x - display_rect.loc.x) as f32,
                (clip.loc.y - display_rect.loc.y) as f32,
                clip.size.w as f32,
                clip.size.h as f32,
            ]
        })
        .unwrap_or([0.0, 0.0, 0.0, 0.0]);
    let radius = clip_radius.max(0) as f32;
    let uv_offset = [
        (sample_rect.loc.x - captured_rect.loc.x) as f32 / captured_rect.size.w.max(1) as f32,
        (sample_rect.loc.y - captured_rect.loc.y) as f32 / captured_rect.size.h.max(1) as f32,
    ];
    let uv_scale = [
        sample_rect.size.w as f32 / captured_rect.size.w.max(1) as f32,
        sample_rect.size.h as f32 / captured_rect.size.h.max(1) as f32,
    ];
    Ok(TextureShaderElement::new(
        inner,
        program,
        vec![
            Uniform::new("uv_offset", uv_offset),
            Uniform::new("uv_scale", uv_scale),
            Uniform::new(
                "rect_size",
                [display_rect.size.w as f32, display_rect.size.h as f32],
            ),
            Uniform::new("render_scale", render_scale.max(1.0)),
            Uniform::new("clip_enabled", if clip_rect[2] > 0.0 && clip_rect[3] > 0.0 { 1.0f32 } else { 0.0f32 }),
            Uniform::new("clip_rect", clip_rect),
            Uniform::new("clip_radius", [radius, radius, radius, radius]),
        ],
    ))
}

pub fn preblur_backdrop_texture(
    renderer: &mut GlesRenderer,
    texture: GlesTexture,
    size: (i32, i32),
    radius: i32,
    passes: i32,
) -> Result<GlesTexture, ShaderEffectError> {
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
