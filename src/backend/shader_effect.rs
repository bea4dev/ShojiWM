use std::{collections::HashMap, fs, sync::Mutex};

use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
        element::{Element, Id, Kind, RenderElement, UnderlyingStorage},
        element::texture::TextureRenderElement,
        gles::{
            element::TextureShaderElement, GlesError, GlesFrame, GlesPixelProgram, GlesRenderer,
            GlesTexProgram, GlesTexture, Uniform, UniformName,
        },
        utils::{CommitCounter, OpaqueRegions},
        Bind, Offscreen, Renderer,
    },
    },
    utils::{user_data::UserDataMap, Buffer, Logical, Physical, Rectangle, Scale, Transform},
};

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

#[derive(Debug, Default)]
struct ShaderProgramCache(Mutex<HashMap<String, GlesPixelProgram>>);
#[derive(Debug, Default)]
struct BackdropShaderProgramCache(Mutex<HashMap<String, GlesTexProgram>>);
#[derive(Debug, Default)]
struct BlurShaderProgramCache(Mutex<Option<GlesTexProgram>>);

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

fn blur_shader_program(renderer: &mut GlesRenderer) -> Result<GlesTexProgram, ShaderEffectError> {
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

    if let Some(program) = renderer
        .egl_context()
        .user_data()
        .get::<BlurShaderProgramCache>()
        .expect("blur shader cache should be initialized")
        .0
        .lock()
        .unwrap()
        .clone()
    {
        return Ok(program);
    }

    let program = renderer.compile_custom_texture_shader(
        include_str!("backdrop_blur.frag"),
        &[
            UniformName::new(
                "texel_step",
                smithay::backend::renderer::gles::UniformType::_2f,
            ),
            UniformName::new(
                "radius",
                smithay::backend::renderer::gles::UniformType::_1f,
            ),
        ],
    )?;
    *renderer
        .egl_context()
        .user_data()
        .get::<BlurShaderProgramCache>()
        .expect("blur shader cache should be initialized")
        .0
        .lock()
        .unwrap() = Some(program.clone());
    Ok(program)
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
    let program = blur_shader_program(renderer)?;
    let mut current = texture;

    for _ in 0..passes.max(1) {
        let mut target = Offscreen::<GlesTexture>::create_buffer(
            renderer,
            Fourcc::Abgr8888,
            size.into(),
        )?;
        let inner = TextureRenderElement::from_static_texture(
            Id::new(),
            renderer.context_id(),
            smithay::utils::Point::<f64, Physical>::from((0.0, 0.0)),
            current.clone(),
            1,
            Transform::Normal,
            Some(1.0),
            None,
            None,
            None,
            Kind::Unspecified,
        );
        let element = TextureShaderElement::new(
            inner,
            program.clone(),
            vec![
                Uniform::new(
                    "texel_step",
                    [1.0f32 / size.0.max(1) as f32, 1.0f32 / size.1.max(1) as f32],
                ),
                Uniform::new("radius", radius.max(1) as f32),
            ],
        );

        let mut framebuffer = renderer.bind(&mut target)?;
        let mut damage_tracker =
            smithay::backend::renderer::damage::OutputDamageTracker::new(size, 1.0, Transform::Normal);
        let _ = damage_tracker
            .render_output(renderer, &mut framebuffer, 0, &[element], [0.0, 0.0, 0.0, 0.0])
            .map_err(|_| GlesError::FramebufferBindingError)?;
        drop(framebuffer);
        current = target;
    }

    Ok(current)
}
