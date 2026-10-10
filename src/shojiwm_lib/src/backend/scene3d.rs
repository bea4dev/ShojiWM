//! Offscreen 3D scenes of custom compositions (`<Scene3D>`).
//!
//! A scene draws textured planes with a perspective camera into its own
//! framebuffer, which has a depth buffer and (on GLES 3) 4x MSAA, then
//! resolves into a texture the output draws like any other. The output's own
//! framebuffer never needs a depth attachment, and nothing about it changes
//! while no scene is in the plan.

use std::{
    hash::{Hash, Hasher},
    sync::{Arc, Mutex},
};

use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            ContextId, Offscreen, Renderer, Texture,
            element::{Id, texture::TextureRenderElement},
            gles::{GlesError, GlesRenderer, GlesTexture, ffi, link_program},
            utils::DamageBag,
        },
    },
    utils::{Buffer, Physical, Rectangle, Scale, Size},
};

use super::composition::{Mat4, Object3d, Scene3dSpec};

const VERTEX_SHADER: &str = r#"#version 100
attribute vec2 corner;
uniform mat4 mvp;
uniform vec2 plane_size;
uniform float flip_v;
varying highp vec2 v_uv;
void main() {
    // Texture row 0 is the top of the image; the plane's +Y is up.
    v_uv = vec2(corner.x + 0.5, mix(0.5 - corner.y, 0.5 + corner.y, flip_v));
    gl_Position = mvp * vec4(corner * plane_size, 0.0, 1.0);
}
"#;

const FRAGMENT_SHADER: &str = r#"#version 100
precision mediump float;
uniform sampler2D tex;
uniform float alpha;
// 0: the opaque pass, which draws only opaque texels and writes depth.
// 1: the blend pass, which draws everything else without writing depth, so
// a shadow or a fading plane never hides what is drawn after it.
uniform float blend_pass;
// 1 when the plane is fully opaque and so takes part in the opaque pass.
// Decided on the CPU: with mediump (fp16) `alpha`, 0.9999 would read as 1.
uniform float opaque_plane;
varying highp vec2 v_uv;
void main() {
    vec4 texel = texture2D(tex, v_uv);
    vec4 color = texel * alpha;
    bool solid = opaque_plane > 0.5 && texel.a >= 254.5 / 255.0;
    if (blend_pass < 0.5 ? !solid : (solid || color.a < 1.0 / 255.0)) {
        discard;
    }
    gl_FragColor = color;
}
"#;

/// Smithay stores the pixels it renders into a texture with the image's top
/// row at the GL origin, so a scene flips Y on the way out to match.
const FLIP_OUTPUT_Y: bool = true;

#[derive(Clone)]
struct Scene3dProgram {
    program: ffi::types::GLuint,
    attrib_corner: ffi::types::GLint,
    uniform_mvp: ffi::types::GLint,
    uniform_plane_size: ffi::types::GLint,
    uniform_flip_v: ffi::types::GLint,
    uniform_tex: ffi::types::GLint,
    uniform_alpha: ffi::types::GLint,
    uniform_blend_pass: ffi::types::GLint,
    uniform_opaque_plane: ffi::types::GLint,
}

#[derive(Default)]
struct Scene3dProgramCache(Mutex<Option<Scene3dProgram>>);

/// GL objects of targets dropped since their context was last current.
static RETIRED: Mutex<Vec<(ContextId<GlesTexture>, Framebuffers)>> = Mutex::new(Vec::new());

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Framebuffers {
    size: (i32, i32),
    /// Has the output texture attached; also the draw target without MSAA.
    resolve: ffi::types::GLuint,
    /// Depth for the non-multisampled path.
    depth: ffi::types::GLuint,
    /// Multisampled color and depth, resolved into `resolve`.
    msaa: Option<(ffi::types::GLuint, ffi::types::GLuint, ffi::types::GLuint)>,
}

impl Framebuffers {
    unsafe fn delete(self, gl: &ffi::Gles2) {
        unsafe {
            gl.DeleteFramebuffers(1, &self.resolve);
            gl.DeleteRenderbuffers(1, &self.depth);
            if let Some((fbo, color, depth)) = self.msaa {
                gl.DeleteFramebuffers(1, &fbo);
                gl.DeleteRenderbuffers(1, &color);
                gl.DeleteRenderbuffers(1, &depth);
            }
        }
    }
}

pub struct Scene3dTarget {
    context: Option<ContextId<GlesTexture>>,
    texture: Option<GlesTexture>,
    framebuffers: Option<Framebuffers>,
    antialias: bool,
    id: Id,
    damage: Arc<Mutex<DamageBag<i32, Buffer>>>,
    signature: Option<u64>,
}

impl Drop for Scene3dTarget {
    fn drop(&mut self) {
        if let (Some(context), Some(framebuffers)) = (self.context.clone(), self.framebuffers.take())
        {
            RETIRED.lock().unwrap().push((context, framebuffers));
        }
    }
}

impl Default for Scene3dTarget {
    fn default() -> Self {
        Self::new()
    }
}

impl Scene3dTarget {
    pub fn new() -> Self {
        Self {
            context: None,
            texture: None,
            framebuffers: None,
            antialias: false,
            id: Id::new(),
            damage: Arc::new(Mutex::new(DamageBag::new(4))),
            signature: None,
        }
    }

    /// Whether the GPU state belongs to `renderer` (or there is none yet).
    pub fn matches(&self, renderer: &GlesRenderer) -> bool {
        self.context.as_ref().is_none_or(|context| *context == renderer.context_id())
    }

    /// Redraw the scene if anything it shows changed. `inputs` holds every
    /// texture of the plan by index: its current texture and generation.
    pub fn render(
        &mut self,
        renderer: &mut GlesRenderer,
        spec: &Scene3dSpec,
        inputs: &[Option<(GlesTexture, u64)>],
        size: Size<i32, Physical>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let signature = scene_signature(spec, inputs, size);
        if self.signature == Some(signature) && self.texture.is_some() {
            return Ok(());
        }
        let program = scene_program(renderer)?;
        let texture = match self.texture.take() {
            Some(texture) if texture.size() == (size.w, size.h).into() => texture,
            _ => Offscreen::<GlesTexture>::create_buffer(
                renderer,
                Fourcc::Abgr8888,
                (size.w, size.h).into(),
            )?,
        };
        let context = renderer.context_id();
        let previous = self.framebuffers.take();
        let antialias = spec.antialias;
        let reuse = previous.filter(|framebuffers| {
            framebuffers.size == (size.w, size.h) && self.antialias == antialias
        });
        let retired = {
            let mut retired = RETIRED.lock().unwrap();
            let (current, other): (Vec<_>, Vec<_>) =
                retired.drain(..).partition(|(owner, _)| *owner == context);
            *retired = other;
            current
        };

        let framebuffers = renderer.with_context(|gl| unsafe {
            for (_, framebuffers) in retired {
                framebuffers.delete(gl);
            }
            if previous.is_some() && reuse.is_none() {
                previous.unwrap().delete(gl);
            }
            let framebuffers = match reuse {
                Some(framebuffers) => framebuffers,
                None => create_framebuffers(gl, &texture, size, antialias)?,
            };
            draw_scene(gl, &program, &framebuffers, spec, inputs, size);
            Ok::<_, GlesError>(framebuffers)
        })??;

        self.context = Some(context);
        self.framebuffers = Some(framebuffers);
        self.antialias = antialias;
        self.texture = Some(texture);
        self.signature = Some(signature);
        self.damage
            .lock()
            .unwrap()
            .add([Rectangle::from_size((size.w, size.h).into())]);
        Ok(())
    }

    /// The last rendered frame.
    pub fn texture(&self) -> Option<&GlesTexture> {
        self.texture.as_ref()
    }

    /// Identifies what the last rendered frame shows.
    pub fn signature(&self) -> Option<u64> {
        self.signature
    }

    pub fn element(
        &self,
        renderer: &GlesRenderer,
        geometry: Rectangle<i32, Physical>,
        scale: Scale<f64>,
    ) -> Option<TextureRenderElement<GlesTexture>> {
        let texture = self.texture.as_ref()?;
        Some(super::composition::texture_element(
            renderer,
            &self.id,
            texture,
            &self.damage,
            geometry,
            scale,
            1.0,
        ))
    }
}

fn scene_signature(
    spec: &Scene3dSpec,
    inputs: &[Option<(GlesTexture, u64)>],
    size: Size<i32, Physical>,
) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    (size.w, size.h).hash(&mut hasher);
    // Floats have no Hash; their bit patterns are exact enough for "changed?".
    let floats = |hasher: &mut std::collections::hash_map::DefaultHasher, values: &[f32]| {
        for value in values {
            value.to_bits().hash(hasher);
        }
    };
    floats(&mut hasher, &spec.projection);
    floats(&mut hasher, &spec.view);
    floats(&mut hasher, &spec.clear_color);
    spec.antialias.hash(&mut hasher);
    for object in &spec.objects {
        match object {
            Object3d::Plane { texture, width, height, model, opacity, double_sided } => {
                texture.hash(&mut hasher);
                floats(&mut hasher, &[*width, *height, *opacity]);
                floats(&mut hasher, model);
                double_sided.hash(&mut hasher);
                if let Some(Some((texture, generation))) = inputs.get(*texture) {
                    texture.tex_id().hash(&mut hasher);
                    generation.hash(&mut hasher);
                }
            }
        }
    }
    hasher.finish()
}

fn scene_program(renderer: &mut GlesRenderer) -> Result<Scene3dProgram, GlesError> {
    renderer
        .egl_context()
        .user_data()
        .insert_if_missing(Scene3dProgramCache::default);
    let cache = renderer
        .egl_context()
        .user_data()
        .get::<Scene3dProgramCache>()
        .expect("scene program cache is initialized");
    if let Some(program) = cache.0.lock().unwrap().clone() {
        return Ok(program);
    }
    let program = renderer.with_context(|gl| unsafe {
        let program = link_program(gl, VERTEX_SHADER, FRAGMENT_SHADER)?;
        let uniform = |name: &std::ffi::CStr| gl.GetUniformLocation(program, name.as_ptr());
        Ok::<_, GlesError>(Scene3dProgram {
            program,
            attrib_corner: gl.GetAttribLocation(program, c"corner".as_ptr()),
            uniform_mvp: uniform(c"mvp"),
            uniform_plane_size: uniform(c"plane_size"),
            uniform_flip_v: uniform(c"flip_v"),
            uniform_tex: uniform(c"tex"),
            uniform_alpha: uniform(c"alpha"),
            uniform_blend_pass: uniform(c"blend_pass"),
            uniform_opaque_plane: uniform(c"opaque_plane"),
        })
    })??;
    *renderer
        .egl_context()
        .user_data()
        .get::<Scene3dProgramCache>()
        .expect("scene program cache is initialized")
        .0
        .lock()
        .unwrap() = Some(program.clone());
    Ok(program)
}

unsafe fn gles3(gl: &ffi::Gles2) -> bool {
    unsafe {
        let version = gl.GetString(ffi::VERSION);
        if version.is_null() {
            return false;
        }
        let version = std::ffi::CStr::from_ptr(version.cast()).to_string_lossy();
        // "OpenGL ES 3.2 ..."
        version
            .strip_prefix("OpenGL ES ")
            .and_then(|rest| rest.chars().next())
            .and_then(|major| major.to_digit(10))
            .is_some_and(|major| major >= 3)
    }
}

unsafe fn create_framebuffers(
    gl: &ffi::Gles2,
    texture: &GlesTexture,
    size: Size<i32, Physical>,
    antialias: bool,
) -> Result<Framebuffers, GlesError> {
    unsafe {
        while gl.GetError() != ffi::NO_ERROR {}
        let mut framebuffers = Framebuffers {
            size: (size.w, size.h),
            resolve: 0,
            depth: 0,
            msaa: None,
        };
        let has_gles3 = gles3(gl);
        gl.GenFramebuffers(1, &mut framebuffers.resolve);
        gl.BindFramebuffer(ffi::FRAMEBUFFER, framebuffers.resolve);
        gl.FramebufferTexture2D(
            ffi::FRAMEBUFFER,
            ffi::COLOR_ATTACHMENT0,
            ffi::TEXTURE_2D,
            texture.tex_id(),
            0,
        );
        if antialias && has_gles3 && gl.RenderbufferStorageMultisample.is_loaded() {
            let mut max_samples = 0;
            gl.GetIntegerv(ffi::MAX_SAMPLES, &mut max_samples);
            let samples = max_samples.min(4);
            if samples > 1 {
                let (mut fbo, mut color, mut depth) = (0, 0, 0);
                gl.GenRenderbuffers(1, &mut color);
                gl.BindRenderbuffer(ffi::RENDERBUFFER, color);
                gl.RenderbufferStorageMultisample(ffi::RENDERBUFFER, samples, ffi::RGBA8, size.w, size.h);
                gl.GenRenderbuffers(1, &mut depth);
                gl.BindRenderbuffer(ffi::RENDERBUFFER, depth);
                gl.RenderbufferStorageMultisample(
                    ffi::RENDERBUFFER,
                    samples,
                    ffi::DEPTH_COMPONENT24,
                    size.w,
                    size.h,
                );
                gl.GenFramebuffers(1, &mut fbo);
                gl.BindFramebuffer(ffi::FRAMEBUFFER, fbo);
                gl.FramebufferRenderbuffer(ffi::FRAMEBUFFER, ffi::COLOR_ATTACHMENT0, ffi::RENDERBUFFER, color);
                gl.FramebufferRenderbuffer(ffi::FRAMEBUFFER, ffi::DEPTH_ATTACHMENT, ffi::RENDERBUFFER, depth);
                if gl.CheckFramebufferStatus(ffi::FRAMEBUFFER) == ffi::FRAMEBUFFER_COMPLETE {
                    framebuffers.msaa = Some((fbo, color, depth));
                } else {
                    gl.DeleteFramebuffers(1, &fbo);
                    gl.DeleteRenderbuffers(1, &color);
                    gl.DeleteRenderbuffers(1, &depth);
                }
            }
        }
        if framebuffers.msaa.is_none() {
            gl.GenRenderbuffers(1, &mut framebuffers.depth);
            gl.BindRenderbuffer(ffi::RENDERBUFFER, framebuffers.depth);
            gl.RenderbufferStorage(
                ffi::RENDERBUFFER,
                if has_gles3 { ffi::DEPTH_COMPONENT24 } else { ffi::DEPTH_COMPONENT16 },
                size.w,
                size.h,
            );
            gl.BindFramebuffer(ffi::FRAMEBUFFER, framebuffers.resolve);
            gl.FramebufferRenderbuffer(
                ffi::FRAMEBUFFER,
                ffi::DEPTH_ATTACHMENT,
                ffi::RENDERBUFFER,
                framebuffers.depth,
            );
        }
        gl.BindFramebuffer(ffi::FRAMEBUFFER, framebuffers.resolve);
        let status = gl.CheckFramebufferStatus(ffi::FRAMEBUFFER);
        gl.BindRenderbuffer(ffi::RENDERBUFFER, 0);
        gl.BindFramebuffer(ffi::FRAMEBUFFER, 0);
        if status != ffi::FRAMEBUFFER_COMPLETE {
            framebuffers.delete(gl);
            return Err(GlesError::FramebufferBindingError);
        }
        Ok(framebuffers)
    }
}

/// `a * b` for column-major matrices.
pub fn multiply(a: &Mat4, b: &Mat4) -> Mat4 {
    let mut out = [0.0f32; 16];
    for column in 0..4 {
        for row in 0..4 {
            out[column * 4 + row] = (0..4).map(|k| a[k * 4 + row] * b[column * 4 + k]).sum();
        }
    }
    out
}

/// Conservative visible area: texture alpha and inter-plane depth occlusion
/// would require GPU readback. Clip in homogeneous space so planes crossing
/// the near plane do not disappear or acquire an unbounded area.
pub(super) fn projected_area(
    spec: &Scene3dSpec,
    object: &Object3d,
    size: Size<i32, Physical>,
) -> usize {
    let Object3d::Plane {
        width,
        height,
        model,
        opacity,
        double_sided,
        ..
    } = object;
    if *opacity < 1.0 / 255.0 || *width == 0.0 || *height == 0.0 {
        return 0;
    }
    let mvp = multiply(&multiply(&spec.projection, &spec.view), model);
    let mut polygon: Vec<[f64; 4]> = [(-0.5, -0.5), (0.5, -0.5), (0.5, 0.5), (-0.5, 0.5)]
        .into_iter()
        .map(|(x, y)| {
            std::array::from_fn(|row| {
                mvp[row] as f64 * x * *width as f64
                    + mvp[4 + row] as f64 * y * *height as f64
                    + mvp[12 + row] as f64
            })
        })
        .collect();
    for axis in 0..3 {
        for sign in [-1.0, 1.0] {
            let mut clipped = Vec::new();
            for i in 0..polygon.len() {
                let a = polygon[i];
                let b = polygon[(i + 1) % polygon.len()];
                let da = a[3] + sign * a[axis];
                let db = b[3] + sign * b[axis];
                if da >= 0.0 {
                    clipped.push(a);
                }
                if (da >= 0.0) != (db >= 0.0) {
                    let t = da / (da - db);
                    clipped.push(std::array::from_fn(|j| a[j] + t * (b[j] - a[j])));
                }
            }
            polygon = clipped;
        }
    }
    if polygon.len() < 3 || polygon.iter().any(|p| p[3] <= 0.0) {
        return 0;
    }
    let twice_area: f64 = (0..polygon.len())
        .map(|i| {
            let a = polygon[i];
            let b = polygon[(i + 1) % polygon.len()];
            (a[0] * b[1] - b[0] * a[1]) / (a[3] * b[3])
        })
        .sum();
    if !double_sided && twice_area <= 0.0 {
        return 0;
    }
    (twice_area.abs() / 8.0 * size.w as f64 * size.h as f64).ceil() as usize
}

unsafe fn draw_scene(
    gl: &ffi::Gles2,
    program: &Scene3dProgram,
    framebuffers: &Framebuffers,
    spec: &Scene3dSpec,
    inputs: &[Option<(GlesTexture, u64)>],
    size: Size<i32, Physical>,
) {
    unsafe {
        let draw_fbo = framebuffers.msaa.map_or(framebuffers.resolve, |(fbo, _, _)| fbo);
        gl.BindFramebuffer(ffi::FRAMEBUFFER, draw_fbo);
        gl.Viewport(0, 0, size.w, size.h);
        gl.Disable(ffi::SCISSOR_TEST);
        let [r, g, b, a] = spec.clear_color;
        gl.ClearColor(r, g, b, a);
        gl.DepthMask(ffi::TRUE);
        gl.ClearDepthf(1.0);
        gl.Clear(ffi::COLOR_BUFFER_BIT | ffi::DEPTH_BUFFER_BIT);
        gl.Enable(ffi::DEPTH_TEST);
        gl.DepthFunc(ffi::LEQUAL);
        gl.Enable(ffi::BLEND);
        gl.BlendFunc(ffi::ONE, ffi::ONE_MINUS_SRC_ALPHA);

        let flip: Mat4 = if FLIP_OUTPUT_Y {
            [
                1.0, 0.0, 0.0, 0.0, //
                0.0, -1.0, 0.0, 0.0, //
                0.0, 0.0, 1.0, 0.0, //
                0.0, 0.0, 0.0, 1.0,
            ]
        } else {
            super::composition::IDENTITY
        };
        // The flip mirrors the scene, which turns front faces into back faces.
        gl.FrontFace(if FLIP_OUTPUT_Y { ffi::CW } else { ffi::CCW });
        let view_projection = multiply(&flip, &multiply(&spec.projection, &spec.view));

        // Opaque texels first, with depth; then every translucent texel
        // (shadows, faded planes) far to near over them without depth, so
        // they blend over what lies behind them and hide nothing. Sorting by
        // centre is only approximate for turned planes, which is why
        // translucent texels must not write depth.
        let mut planes: Vec<_> = spec
            .objects
            .iter()
            .map(|object| match object {
                Object3d::Plane { model, .. } => {
                    let view_model = multiply(&spec.view, model);
                    (view_model[14], object)
                }
            })
            .collect();
        planes.sort_by(|left, right| left.0.total_cmp(&right.0));

        gl.UseProgram(program.program);
        gl.Uniform1i(program.uniform_tex, 0);
        gl.Uniform1f(program.uniform_flip_v, 0.0);
        let corners: [f32; 8] = [-0.5, -0.5, 0.5, -0.5, -0.5, 0.5, 0.5, 0.5];
        let attrib = program.attrib_corner as u32;
        gl.EnableVertexAttribArray(attrib);
        gl.BindBuffer(ffi::ARRAY_BUFFER, 0);
        gl.VertexAttribPointer(attrib, 2, ffi::FLOAT, ffi::FALSE, 0, corners.as_ptr().cast());
        gl.ActiveTexture(ffi::TEXTURE0);
        for (blend_pass, (_, object)) in planes
            .iter()
            .map(|plane| (false, plane))
            .chain(planes.iter().map(|plane| (true, plane)))
        {
            let Object3d::Plane { texture, width, height, model, opacity, double_sided } = object;
            let opaque_plane = *opacity >= 1.0;
            if !blend_pass && !opaque_plane {
                continue;
            }
            let Some(Some((texture, _))) = inputs.get(*texture) else {
                continue;
            };
            gl.DepthMask(if blend_pass { ffi::FALSE } else { ffi::TRUE });
            gl.Uniform1f(program.uniform_blend_pass, if blend_pass { 1.0 } else { 0.0 });
            gl.Uniform1f(program.uniform_opaque_plane, if opaque_plane { 1.0 } else { 0.0 });
            if *double_sided {
                gl.Disable(ffi::CULL_FACE);
            } else {
                gl.Enable(ffi::CULL_FACE);
                gl.CullFace(ffi::BACK);
            }
            let mvp = multiply(&view_projection, model);
            gl.UniformMatrix4fv(program.uniform_mvp, 1, ffi::FALSE, mvp.as_ptr());
            gl.Uniform2f(program.uniform_plane_size, *width, *height);
            gl.Uniform1f(program.uniform_alpha, *opacity);
            gl.BindTexture(ffi::TEXTURE_2D, texture.tex_id());
            gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_MIN_FILTER, ffi::LINEAR as i32);
            gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_MAG_FILTER, ffi::LINEAR as i32);
            gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_WRAP_S, ffi::CLAMP_TO_EDGE as i32);
            gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_WRAP_T, ffi::CLAMP_TO_EDGE as i32);
            gl.DrawArrays(ffi::TRIANGLE_STRIP, 0, 4);
        }
        gl.BindTexture(ffi::TEXTURE_2D, 0);
        gl.DisableVertexAttribArray(attrib);
        gl.UseProgram(0);

        if let Some((fbo, _, _)) = framebuffers.msaa {
            gl.BindFramebuffer(ffi::READ_FRAMEBUFFER, fbo);
            gl.BindFramebuffer(ffi::DRAW_FRAMEBUFFER, framebuffers.resolve);
            gl.BlitFramebuffer(
                0,
                0,
                size.w,
                size.h,
                0,
                0,
                size.w,
                size.h,
                ffi::COLOR_BUFFER_BIT,
                ffi::NEAREST,
            );
        }

        // Leave the state the way Smithay's renderer expects it.
        gl.DepthMask(ffi::TRUE);
        gl.Disable(ffi::DEPTH_TEST);
        gl.Disable(ffi::CULL_FACE);
        gl.FrontFace(ffi::CCW);
        gl.BindFramebuffer(ffi::FRAMEBUFFER, 0);
        gl.Enable(ffi::SCISSOR_TEST);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::composition::IDENTITY;
    use smithay::backend::{
        egl::{EGLContext, EGLDisplay, native::EGLSurfacelessDisplay},
        renderer::{
            Bind, ExportMem, ImportMem,
            damage::OutputDamageTracker,
            element::{Kind, texture::TextureRenderElement},
        },
    };
    use smithay::utils::Transform;

    fn renderer() -> GlesRenderer {
        let display = unsafe { EGLDisplay::new(EGLSurfacelessDisplay) }.unwrap();
        let context = EGLContext::new(&display).unwrap();
        unsafe { GlesRenderer::new(context) }.unwrap()
    }

    /// Rows of RGBA pixels as Smithay reads them back (row 0 = image top).
    fn rows(renderer: &mut GlesRenderer, texture: &GlesTexture) -> Vec<Vec<[u8; 4]>> {
        let size = texture.size();
        let map = renderer
            .copy_texture(texture, Rectangle::from_size(size), Fourcc::Abgr8888)
            .unwrap();
        let bytes = renderer.map_texture(&map).unwrap().to_vec();
        bytes
            .chunks(size.w as usize * 4)
            .map(|row| row.chunks(4).map(|px| [px[0], px[1], px[2], px[3]]).collect())
            .collect()
    }

    /// Draw `texture` over a fresh target with Smithay, like an output does.
    fn smithay_draw(renderer: &mut GlesRenderer, texture: &GlesTexture) -> GlesTexture {
        let size = texture.size();
        let element = TextureRenderElement::from_static_texture(
            Id::new(),
            renderer.context_id(),
            (0.0, 0.0),
            texture.clone(),
            1,
            Transform::Normal,
            None,
            None,
            None,
            None,
            Kind::Unspecified,
        );
        let mut target: GlesTexture =
            Offscreen::<GlesTexture>::create_buffer(renderer, Fourcc::Abgr8888, size).unwrap();
        {
            let mut framebuffer = renderer.bind(&mut target).unwrap();
            let mut tracker = OutputDamageTracker::new((size.w, size.h), 1.0, Transform::Normal);
            tracker
                .render_output(renderer, &mut framebuffer, 0, &[element], [0.0; 4])
                .unwrap();
        }
        target
    }

    fn plane(texture: usize, model: Mat4) -> Object3d {
        Object3d::Plane {
            texture,
            width: 2.0,
            height: 2.0,
            model,
            opacity: 1.0,
            double_sided: true,
        }
    }

    fn translate_z(z: f32) -> Mat4 {
        let mut m = IDENTITY;
        m[14] = z;
        m
    }

    #[test]
    fn plane_visibility_respects_clipping_facing_and_opacity() {
        let mut spec = Scene3dSpec {
            rect: None,
            projection: IDENTITY,
            view: IDENTITY,
            clear_color: [0.0; 4],
            antialias: false,
            objects: vec![],
        };
        let size = (100, 100).into();
        assert_eq!(projected_area(&spec, &plane(0, IDENTITY), size), 10000);
        assert_eq!(projected_area(&spec, &plane(0, translate_z(2.0)), size), 0);
        assert_eq!(projected_area(&spec, &plane(0, translate_z(-2.0)), size), 0);
        let mut moved = IDENTITY;
        moved[12] = 3.0;
        assert_eq!(projected_area(&spec, &plane(0, moved), size), 0);
        moved[12] = 1.0;
        assert_eq!(projected_area(&spec, &plane(0, moved), size), 5000);
        let mut object = plane(0, IDENTITY);
        let Object3d::Plane { opacity, .. } = &mut object;
        *opacity = 0.0;
        assert_eq!(projected_area(&spec, &object, size), 0);
        let Object3d::Plane {
            opacity,
            double_sided,
            model,
            ..
        } = &mut object;
        *opacity = 1.0;
        *double_sided = false;
        model[0] = -1.0;
        assert_eq!(projected_area(&spec, &object, size), 0);
        let Object3d::Plane { double_sided, .. } = &mut object;
        *double_sided = true;
        assert_eq!(projected_area(&spec, &object, size), 10000);
        let Object3d::Plane { width, .. } = &mut object;
        *width = -2.0;
        assert_eq!(projected_area(&spec, &object, size), 10000);
        let mut tilted = IDENTITY;
        tilted[2] = 2.0;
        assert_eq!(projected_area(&spec, &plane(0, tilted), size), 5000);
        spec.projection[15] = -1.0;
        assert_eq!(projected_area(&spec, &plane(0, IDENTITY), size), 0);
    }

    #[test]
    #[ignore = "requires surfaceless EGL"]
    fn scene_keeps_orientation_and_depth() {
        let mut renderer = renderer();
        const RED: [u8; 4] = [255, 0, 0, 255];
        const BLUE: [u8; 4] = [0, 0, 255, 255];
        const GREEN: [u8; 4] = [0, 255, 0, 255];
        // 4x4, top half red, bottom half blue, as Smithay imports it.
        let mut pixels = Vec::new();
        for row in 0..4 {
            for _ in 0..4 {
                pixels.extend(if row < 2 { RED } else { BLUE });
            }
        }
        let imported = renderer
            .import_memory(&pixels, Fourcc::Abgr8888, (4, 4).into(), false)
            .unwrap();
        // A texture Smithay rendered (what a render texture holds).
        let rendered = smithay_draw(&mut renderer, &imported);
        assert_eq!(rows(&mut renderer, &rendered)[0][0], RED, "smithay readback is top-down");
        let green = renderer
            .import_memory(&GREEN.repeat(16), Fourcc::Abgr8888, (4, 4).into(), false)
            .unwrap();

        let clear = renderer
            .import_memory(&[0u8; 64], Fourcc::Abgr8888, (4, 4).into(), false)
            .unwrap();
        // Half-transparent black, like a window's shadow.
        let shadow = renderer
            .import_memory(&[0, 0, 0, 128].repeat(16), Fourcc::Abgr8888, (4, 4).into(), false)
            .unwrap();
        let inputs = vec![
            Some((rendered.clone(), 1)),
            Some((green, 1)),
            Some((clear, 1)),
            Some((shadow, 1)),
        ];
        // A plane covering clip space with identity camera: the scene must
        // reproduce the input exactly, top stays top.
        let mut spec = Scene3dSpec {
            rect: None,
            projection: IDENTITY,
            view: IDENTITY,
            clear_color: [0.0; 4],
            antialias: false,
            objects: vec![plane(0, IDENTITY)],
        };
        let mut target = Scene3dTarget::new();
        target.render(&mut renderer, &spec, &inputs, (4, 4).into()).unwrap();
        let scene = target.texture.clone().unwrap();
        let scene_rows = rows(&mut renderer, &scene);
        assert_eq!(scene_rows[0][0], RED, "scene top row: {scene_rows:?}");
        assert_eq!(scene_rows[3][0], BLUE, "scene bottom row: {scene_rows:?}");
        // And drawn by Smithay onto an output, it still reads top-down.
        let shown = smithay_draw(&mut renderer, &scene);
        assert_eq!(rows(&mut renderer, &shown)[0][0], RED);

        // Depth: the green plane is nearer (identity projection keeps z as
        // depth; smaller z is nearer), whatever the draw order.
        spec.objects = vec![plane(1, translate_z(-0.5)), plane(0, translate_z(0.5))];
        target.render(&mut renderer, &spec, &inputs, (4, 4).into()).unwrap();
        assert_eq!(rows(&mut renderer, target.texture.as_ref().unwrap())[1][1], GREEN);
        spec.objects.reverse();
        target.render(&mut renderer, &spec, &inputs, (4, 4).into()).unwrap();
        assert_eq!(rows(&mut renderer, target.texture.as_ref().unwrap())[1][1], GREEN);

        // A fully transparent plane in front hides nothing.
        spec.objects = vec![plane(2, translate_z(-0.5)), plane(1, translate_z(0.5))];
        target.render(&mut renderer, &spec, &inputs, (4, 4).into()).unwrap();
        assert_eq!(rows(&mut renderer, target.texture.as_ref().unwrap())[1][1], GREEN);
        spec.objects.reverse();
        target.render(&mut renderer, &spec, &inputs, (4, 4).into()).unwrap();
        assert_eq!(rows(&mut renderer, target.texture.as_ref().unwrap())[1][1], GREEN);

        // A fading plane in front, drawn first, still lets the opaque plane
        // behind it show through.
        let mut faint = plane(0, translate_z(-0.5));
        if let Object3d::Plane { opacity, .. } = &mut faint {
            *opacity = 0.1;
        }
        spec.objects = vec![faint, plane(1, translate_z(0.5))];
        target.render(&mut renderer, &spec, &inputs, (4, 4).into()).unwrap();
        let blended = rows(&mut renderer, target.texture.as_ref().unwrap())[1][1];
        assert!(blended[1] > 200 && blended[0] < 40, "faint plane hid green: {blended:?}");

        // A translucent texel (a shadow) in front, drawn first, does not hide
        // the opaque plane behind it either.
        spec.objects = vec![plane(3, translate_z(-0.5)), plane(1, translate_z(0.5))];
        target.render(&mut renderer, &spec, &inputs, (4, 4).into()).unwrap();
        let shaded = rows(&mut renderer, target.texture.as_ref().unwrap())[1][1];
        assert!(shaded[1] > 100 && shaded[3] == 255, "shadow hid green: {shaded:?}");

        // A plane a hair short of opaque (the end of a fade) is drawn whole,
        // even where the shader's mediump opacity rounds up to 1.
        let mut almost = plane(1, IDENTITY);
        if let Object3d::Plane { opacity, .. } = &mut almost {
            *opacity = 0.99999;
        }
        spec.objects = vec![almost];
        target.render(&mut renderer, &spec, &inputs, (4, 4).into()).unwrap();
        let almost_rows = rows(&mut renderer, target.texture.as_ref().unwrap());
        assert!(almost_rows[1][1][1] > 250, "nearly opaque plane vanished: {almost_rows:?}");

        // Multisampled path resolves to the same picture.
        spec.antialias = true;
        spec.objects = vec![plane(0, IDENTITY)];
        target.render(&mut renderer, &spec, &inputs, (4, 4).into()).unwrap();
        let scene_rows = rows(&mut renderer, target.texture.as_ref().unwrap());
        assert_eq!(scene_rows[0][0], RED, "msaa scene: {scene_rows:?}");
        assert_eq!(scene_rows[3][3], BLUE, "msaa scene: {scene_rows:?}");
    }
}
