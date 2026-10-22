//! Shader effect pipelines: the Rust side of `shoji_wm`'s `compileEffect`,
//! `shaderStage`, `dualKawaseBlur` and friends.
//!
//! ```
//! use shojiwm_rs::prelude::*;
//!
//! let titlebar_glass = Effect::new(backdrop_source())
//!     .capture_padding(24)
//!     .invalidate(Invalidate::on_source_damage_box(8))
//!     .stage(dual_kawase_blur(4, 2))
//!     .stage(shader_stage("/path/to/liquid-glass.frag").uniform("glass_tint", 0.9));
//! # let _ = titlebar_glass;
//! ```
//!
//! Uniform values may be reactive. Inside a window composition a changing
//! uniform is sent to the compositor as a single-uniform patch, without
//! rebuilding the pipeline.

use std::collections::BTreeMap;

use shojiwm_lib::ssd::{
    BackdropBlur, BlendMode, CompiledEffect, EffectAlphaMode, EffectDependency, EffectInput,
    EffectInvalidationPolicy, EffectOutsets, EffectRegion, EffectStage, EffectStateResizePolicy,
    EffectStateTexture, EffectStateTextureFormat, NoiseKind, NoiseStage, ShaderModule,
    ShaderStage as CompiledShaderStage, ShaderUniformValue, WindowEffectConfig, WindowEffectSlot,
    WindowSourceInclude,
};

use crate::{
    assets,
    reactive::{Memo, Prop, ReadSignal, Signal},
};

/// A shader uniform value.
#[derive(Debug, Clone, PartialEq)]
pub enum Uniform {
    Float(f32),
    Vec2([f32; 2]),
    Vec3([f32; 3]),
    Vec4([f32; 4]),
    FloatArray(Vec<f32>),
    Vec2Array(Vec<[f32; 2]>),
    Vec3Array(Vec<[f32; 3]>),
    Vec4Array(Vec<[f32; 4]>),
}

impl From<Uniform> for ShaderUniformValue {
    fn from(value: Uniform) -> Self {
        match value {
            Uniform::Float(value) => Self::Float(value),
            Uniform::Vec2(value) => Self::Vec2(value),
            Uniform::Vec3(value) => Self::Vec3(value),
            Uniform::Vec4(value) => Self::Vec4(value),
            Uniform::FloatArray(value) => Self::FloatArray(value),
            Uniform::Vec2Array(value) => Self::Vec2Array(value),
            Uniform::Vec3Array(value) => Self::Vec3Array(value),
            Uniform::Vec4Array(value) => Self::Vec4Array(value),
        }
    }
}

macro_rules! uniform_from {
    ($($ty:ty => |$value:ident| $body:expr),* $(,)?) => {$(
        impl From<$ty> for Uniform {
            fn from($value: $ty) -> Self {
                $body
            }
        }
        impl From<$ty> for Prop<Uniform> {
            fn from(value: $ty) -> Self {
                Prop::Static(value.into())
            }
        }
        impl From<Signal<$ty>> for Prop<Uniform> {
            fn from(signal: Signal<$ty>) -> Self {
                Prop::derive(move || signal.get().into())
            }
        }
        impl From<Memo<$ty>> for Prop<Uniform> {
            fn from(memo: Memo<$ty>) -> Self {
                Prop::derive(move || memo.get().into())
            }
        }
        impl From<ReadSignal<$ty>> for Prop<Uniform> {
            fn from(signal: ReadSignal<$ty>) -> Self {
                Prop::derive(move || signal.get().into())
            }
        }
        impl From<Prop<$ty>> for Prop<Uniform> {
            fn from(prop: Prop<$ty>) -> Self {
                prop.map(Into::into)
            }
        }
    )*};
}

uniform_from! {
    f32 => |value| Uniform::Float(value),
    f64 => |value| Uniform::Float(value as f32),
    i32 => |value| Uniform::Float(value as f32),
    [f32; 2] => |value| Uniform::Vec2(value),
    [f32; 3] => |value| Uniform::Vec3(value),
    [f32; 4] => |value| Uniform::Vec4(value),
    [f64; 2] => |value| Uniform::Vec2(value.map(|component| component as f32)),
    [f64; 3] => |value| Uniform::Vec3(value.map(|component| component as f32)),
    [f64; 4] => |value| Uniform::Vec4(value.map(|component| component as f32)),
    Vec<f32> => |value| Uniform::FloatArray(value),
    Vec<[f32; 2]> => |value| Uniform::Vec2Array(value),
    Vec<[f32; 3]> => |value| Uniform::Vec3Array(value),
    Vec<[f32; 4]> => |value| Uniform::Vec4Array(value),
}

/// What part of a surface a source captures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Include {
    #[default]
    Full,
    RootSurface,
}

impl From<Include> for WindowSourceInclude {
    fn from(include: Include) -> Self {
        match include {
            Include::Full => Self::Full,
            Include::RootSurface => Self::RootSurface,
        }
    }
}

/// A texture an effect reads.
#[derive(Debug, Clone)]
pub enum Source {
    Backdrop,
    XrayBackdrop,
    Window(Include),
    Layer(Include),
    Popup(Include),
    Shader(Box<ShaderStage>),
    Image(String),
    Named(String),
    State(StateTexture),
}

/// The composited scene beneath the surface (`backdropSource()`).
pub fn backdrop_source() -> Source {
    Source::Backdrop
}

/// The scene beneath, ignoring other windows (`xrayBackdropSource()`).
pub fn xray_backdrop_source() -> Source {
    Source::XrayBackdrop
}

pub fn window_source() -> Source {
    Source::Window(Include::Full)
}

pub fn layer_source() -> Source {
    Source::Layer(Include::Full)
}

pub fn popup_source() -> Source {
    Source::Popup(Include::Full)
}

/// An image file, resolved against the config's asset root.
pub fn image_source(path: &str) -> Source {
    Source::Image(assets::resolve(path))
}

/// A texture saved earlier in the pipeline with [`save`] (`get()` in TS).
pub fn saved(name: &str) -> Source {
    Source::Named(name.to_owned())
}

/// Read a persistent state texture (`stateSource()`).
pub fn state_source(state: &StateTexture) -> Source {
    Source::State(state.clone())
}

/// A shader generating the input itself (`shaderInput()`).
pub fn shader_input(stage: ShaderStage) -> Source {
    Source::Shader(Box::new(stage))
}

/// A texture that survives between frames (`stateTexture()`).
#[derive(Debug, Clone, PartialEq)]
pub struct StateTexture {
    pub name: String,
    pub scale: f32,
    pub format: EffectStateTextureFormat,
    pub resize: EffectStateResizePolicy,
}

pub fn state_texture(name: &str) -> StateTexture {
    StateTexture {
        name: name.to_owned(),
        scale: 1.0,
        format: EffectStateTextureFormat::Rgba8,
        resize: EffectStateResizePolicy::Clear,
    }
}

impl StateTexture {
    pub fn scale(mut self, scale: f32) -> Self {
        self.scale = scale;
        self
    }

    pub fn format(mut self, format: EffectStateTextureFormat) -> Self {
        self.format = format;
        self
    }

    pub fn resize(mut self, resize: EffectStateResizePolicy) -> Self {
        self.resize = resize;
        self
    }

    fn compile(&self) -> EffectStateTexture {
        EffectStateTexture {
            name: self.name.clone(),
            scale: self.scale,
            format: self.format,
            resize: self.resize,
        }
    }
}

/// One fragment shader pass.
#[derive(Debug, Clone)]
pub struct ShaderStage {
    path: String,
    uniforms: Vec<(String, Prop<Uniform>)>,
    textures: Vec<(String, Source)>,
}

/// A shader stage; `path` is resolved against the config's asset root
/// (`shaderStage(loadShader(path))`).
pub fn shader_stage(path: &str) -> ShaderStage {
    ShaderStage {
        path: assets::resolve(path),
        uniforms: Vec::new(),
        textures: Vec::new(),
    }
}

impl ShaderStage {
    pub fn uniform(mut self, name: &str, value: impl Into<Prop<Uniform>>) -> Self {
        self.uniforms.push((name.to_owned(), value.into()));
        self
    }

    pub fn texture(mut self, name: &str, source: Source) -> Self {
        self.textures.push((name.to_owned(), source));
        self
    }

    fn compile(&self, read: &mut UniformReader<'_>, stage_index: usize) -> CompiledShaderStage {
        CompiledShaderStage {
            shader: ShaderModule {
                path: self.path.clone(),
            },
            uniforms: self
                .uniforms
                .iter()
                .map(|(name, value)| (name.clone(), read(stage_index, name, value)))
                .collect::<BTreeMap<_, _>>(),
            textures: self
                .textures
                .iter()
                .map(|(name, source)| (name.clone(), source.compile(read)))
                .collect(),
        }
    }
}

/// A paint shader for [`Element::paint`](crate::view::Element::paint) and
/// [`Element::overlay`](crate::view::Element::overlay): a GLSL file defining
/// `vec4 paint_main(PaintContext ctx)` that returns a premultiplied color
/// (`paintShader` in TypeScript). The compositor hands it the node geometry
/// in whole physical pixels.
///
/// ```
/// use shojiwm_rs::prelude::*;
///
/// let glow = paint_shader("/path/to/glow.frag").uniform("strength", 0.6).outsets(12.0);
/// # let _ = glow;
/// ```
#[derive(Debug, Clone)]
pub struct PaintShader {
    path: String,
    uniforms: Vec<(String, Prop<Uniform>)>,
    outsets: shojiwm_lib::ssd::Edges,
}

/// A paint shader; `path` is resolved against the config's asset root.
pub fn paint_shader(path: &str) -> PaintShader {
    PaintShader {
        path: assets::resolve(path),
        uniforms: Vec::new(),
        outsets: shojiwm_lib::ssd::Edges::default(),
    }
}

impl PaintShader {
    pub fn uniform(mut self, name: &str, value: impl Into<Prop<Uniform>>) -> Self {
        self.uniforms.push((name.to_owned(), value.into()));
        self
    }

    /// Logical pixels drawn around the node on every side (glows, shadows).
    pub fn outsets(mut self, outset: f64) -> Self {
        self.outsets = shojiwm_lib::ssd::Edges::all(outset.max(0.0));
        self
    }

    pub fn outset_edges(mut self, outsets: shojiwm_lib::ssd::Edges) -> Self {
        self.outsets = outsets;
        self
    }

    pub(crate) fn uniform_props(&self) -> impl Iterator<Item = (&str, &Prop<Uniform>)> {
        self.uniforms
            .iter()
            .map(|(name, value)| (name.as_str(), value))
    }

    pub(crate) fn compile(
        &self,
        read: &mut UniformReader<'_>,
        stage_index: usize,
    ) -> shojiwm_lib::ssd::PaintShader {
        shojiwm_lib::ssd::PaintShader {
            shader: ShaderModule {
                path: self.path.clone(),
            },
            uniforms: self
                .uniforms
                .iter()
                .map(|(name, value)| (name.clone(), read(stage_index, name, value)))
                .collect(),
            outsets: self.outsets,
        }
    }
}

/// When the compositor re-runs an effect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invalidate(EffectInvalidationPolicy);

impl Invalidate {
    /// Re-run when the captured source is damaged, padded by `padding` px.
    pub fn on_source_damage_box(padding: i32) -> Self {
        Self(EffectInvalidationPolicy::OnSourceDamageBox {
            damage_padding: padding.max(0),
        })
    }

    pub fn always() -> Self {
        Self(EffectInvalidationPolicy::Always)
    }

    /// Re-run only while `dirty_when` is set, on top of `base`.
    pub fn manual(dirty_when: bool, base: Option<Invalidate>) -> Self {
        Self(EffectInvalidationPolicy::Manual {
            dirty_when,
            base: base.map(|base| Box::new(base.0)),
        })
    }
}

/// One step of a pipeline.
#[derive(Debug, Clone)]
pub enum Stage {
    Shader(ShaderStage),
    Noise(f32),
    Blur(BackdropBlur),
    Save(String),
    Blend {
        input: Source,
        mode: BlendMode,
        alpha: f32,
    },
    Unit(Box<Effect>),
    RenderTo {
        target: StateTexture,
        effect: Box<Effect>,
        depends_on: Option<Vec<EffectDependency>>,
    },
}

impl From<ShaderStage> for Stage {
    fn from(stage: ShaderStage) -> Self {
        Self::Shader(stage)
    }
}

pub fn dual_kawase_blur(radius: i32, passes: i32) -> Stage {
    Stage::Blur(BackdropBlur {
        radius: radius.max(0),
        passes: passes.clamp(0, 8),
    })
}

pub fn noise(amount: f32) -> Stage {
    Stage::Noise(amount.clamp(0.0, 1.0))
}

/// Keep the current texture under `name` for [`get`].
pub fn save(name: &str) -> Stage {
    Stage::Save(name.to_owned())
}

pub fn blend(input: Source, mode: BlendMode, alpha: f32) -> Stage {
    Stage::Blend {
        input,
        mode,
        alpha: alpha.clamp(0.0, 1.0),
    }
}

/// Run `effect` as a nested pipeline.
pub fn unit(effect: Effect) -> Stage {
    Stage::Unit(Box::new(effect))
}

/// Render `effect` into `target` every time the outer pipeline runs.
pub fn render_to(target: &StateTexture, effect: Effect) -> Stage {
    Stage::RenderTo {
        target: target.clone(),
        effect: Box::new(effect.invalidate(Invalidate::always())),
        depends_on: None,
    }
}

/// Render `effect` into `target` only when one of `depends_on` changed.
pub fn render_to_if_dirty(target: &StateTexture, depends_on: &[Source], effect: Effect) -> Stage {
    let depends_on = depends_on
        .iter()
        .filter_map(|source| match source {
            Source::Window(_) => Some(EffectDependency::WindowSource),
            Source::Layer(_) => Some(EffectDependency::LayerSource),
            Source::Popup(_) => Some(EffectDependency::PopupSource),
            _ => {
                tracing::warn!(
                    "render_to_if_dirty: only window, layer and popup sources can be dependencies"
                );
                None
            }
        })
        .collect();
    Stage::RenderTo {
        target: target.clone(),
        effect: Box::new(effect.invalidate(Invalidate::always())),
        depends_on: Some(depends_on),
    }
}

/// A compiled-on-demand effect (`compileEffect()`).
#[derive(Debug, Clone)]
pub struct Effect {
    input: Source,
    capture_padding: i32,
    invalidate: Invalidate,
    pipeline: Vec<Stage>,
    alpha: EffectAlphaMode,
}

impl Effect {
    pub fn new(input: Source) -> Self {
        Self {
            input,
            capture_padding: 0,
            invalidate: Invalidate::on_source_damage_box(0),
            pipeline: Vec::new(),
            alpha: EffectAlphaMode::Opaque,
        }
    }

    pub fn capture_padding(mut self, padding: i32) -> Self {
        self.capture_padding = padding;
        self
    }

    pub fn invalidate(mut self, invalidate: Invalidate) -> Self {
        self.invalidate = invalidate;
        self
    }

    /// Keep the pipeline's alpha (`alpha: "preserve"`); default is opaque.
    pub fn preserve_alpha(mut self) -> Self {
        self.alpha = EffectAlphaMode::Preserve;
        self
    }

    pub fn stage(mut self, stage: impl Into<Stage>) -> Self {
        self.pipeline.push(stage.into());
        self
    }

    pub fn stages(mut self, stages: impl IntoIterator<Item = Stage>) -> Self {
        self.pipeline.extend(stages);
        self
    }

    /// Whether some uniform may change over time.
    pub fn has_reactive_uniforms(&self) -> bool {
        fn stage(stage: &ShaderStage) -> bool {
            stage.uniforms.iter().any(|(_, value)| !value.is_static())
                || stage
                    .textures
                    .iter()
                    .any(|(_, source)| source_reactive(source))
        }
        fn source_reactive(source: &Source) -> bool {
            matches!(source, Source::Shader(shader) if stage(shader))
        }
        source_reactive(&self.input)
            || self.pipeline.iter().any(|entry| match entry {
                Stage::Shader(shader) => stage(shader),
                Stage::Blend { input, .. } => source_reactive(input),
                Stage::Unit(effect) | Stage::RenderTo { effect, .. } => {
                    effect.has_reactive_uniforms()
                }
                _ => false,
            })
    }

    /// Compile with every uniform read through the current tracking context.
    pub fn compile(&self) -> CompiledEffect {
        self.compile_with(&mut |_, _, value| value.get().into())
    }

    /// Compile, reading uniform `name` of pipeline stage `stage_index`
    /// through `read` ([`SHADER_INPUT_STAGE_INDEX`](shojiwm_lib::runtime_api::SHADER_INPUT_STAGE_INDEX)
    /// for the input shader).
    pub(crate) fn compile_with(&self, read: &mut UniformReader<'_>) -> CompiledEffect {
        CompiledEffect {
            input: self.input.compile_input(read),
            capture_padding: self.capture_padding,
            invalidate: self.invalidate.0.clone(),
            pipeline: self
                .pipeline
                .iter()
                .enumerate()
                .map(|(index, stage)| stage.compile(read, index))
                .collect(),
            alpha: self.alpha,
        }
    }

    pub(crate) fn shader_stages(&self) -> impl Iterator<Item = (usize, &ShaderStage)> {
        let input = match &self.input {
            Source::Shader(stage) => {
                Some((shojiwm_lib::runtime_api::SHADER_INPUT_STAGE_INDEX, &**stage))
            }
            _ => None,
        };
        input.into_iter().chain(
            self.pipeline
                .iter()
                .enumerate()
                .filter_map(|(index, stage)| match stage {
                    Stage::Shader(shader) => Some((index, shader)),
                    _ => None,
                }),
        )
    }
}

impl ShaderStage {
    pub(crate) fn uniform_props(&self) -> impl Iterator<Item = (&str, &Prop<Uniform>)> {
        self.uniforms
            .iter()
            .map(|(name, value)| (name.as_str(), value))
    }
}

pub(crate) type UniformReader<'a> =
    dyn FnMut(usize, &str, &Prop<Uniform>) -> ShaderUniformValue + 'a;

impl Source {
    fn compile_input(&self, read: &mut UniformReader<'_>) -> EffectInput {
        match self {
            Self::Shader(stage) => EffectInput::Shader(
                stage.compile(read, shojiwm_lib::runtime_api::SHADER_INPUT_STAGE_INDEX),
            ),
            other => other.compile(read),
        }
    }

    fn compile(&self, _read: &mut UniformReader<'_>) -> EffectInput {
        match self {
            Self::Backdrop => EffectInput::Backdrop,
            Self::XrayBackdrop => EffectInput::XrayBackdrop,
            Self::Window(include) => EffectInput::WindowSource((*include).into()),
            Self::Layer(include) => EffectInput::LayerSource((*include).into()),
            Self::Popup(include) => EffectInput::PopupSource((*include).into()),
            // Nested texture shaders are not addressable by patches; their
            // uniforms are read as part of the owning node.
            Self::Shader(stage) => EffectInput::Shader(stage.compile(
                &mut |_, _, value: &Prop<Uniform>| value.get().into(),
                usize::MAX - 1,
            )),
            Self::Image(path) => EffectInput::Image(path.clone()),
            Self::Named(name) => EffectInput::Named(name.clone()),
            Self::State(state) => EffectInput::State(state.compile()),
        }
    }
}

impl Stage {
    fn compile(&self, read: &mut UniformReader<'_>, index: usize) -> EffectStage {
        match self {
            Self::Shader(stage) => EffectStage::Shader(stage.compile(read, index)),
            Self::Noise(amount) => EffectStage::Noise(NoiseStage {
                kind: NoiseKind::Salt,
                amount: *amount,
            }),
            Self::Blur(blur) => EffectStage::DualKawaseBlur(*blur),
            Self::Save(name) => EffectStage::Save(name.clone()),
            Self::Blend { input, mode, alpha } => EffectStage::Blend {
                input: input.compile(read),
                mode: *mode,
                alpha: *alpha,
            },
            Self::Unit(effect) => EffectStage::Unit(Box::new(effect.compile())),
            Self::RenderTo {
                target,
                effect,
                depends_on,
            } => EffectStage::RenderTo {
                target: target.compile(),
                effect: Box::new(effect.compile()),
                depends_on: depends_on.clone(),
            },
        }
    }
}

/// An effect bound to a slot of a window, layer or popup
/// (`compileWindowEffect` / `compileLayerEffect` / `compilePopupEffect`).
#[derive(Debug, Clone)]
pub struct SurfaceEffect {
    effect: Effect,
    outsets: EffectOutsets,
    region: EffectRegion,
}

impl SurfaceEffect {
    pub fn new(effect: Effect) -> Self {
        Self {
            effect,
            outsets: EffectOutsets::default(),
            region: EffectRegion::Surface,
        }
    }

    /// The same outset on every side.
    pub fn outsets(mut self, outset: i32) -> Self {
        let outset = outset.max(0);
        self.outsets = EffectOutsets {
            left: outset,
            right: outset,
            top: outset,
            bottom: outset,
        };
        self
    }

    pub fn outset_edges(mut self, outsets: EffectOutsets) -> Self {
        self.outsets = outsets;
        self
    }

    /// Narrow a layer backdrop to part of the surface.
    pub fn region(mut self, region: EffectRegion) -> Self {
        self.region = region;
        self
    }

    fn compile(&self) -> WindowEffectSlot {
        WindowEffectSlot {
            effect: self.effect.compile(),
            outsets: self.outsets,
            region: self.region,
        }
    }
}

impl From<Effect> for SurfaceEffect {
    fn from(effect: Effect) -> Self {
        Self::new(effect)
    }
}

/// Effects of one surface, by slot. Empty means "no effect".
#[derive(Debug, Clone, Default)]
pub struct SurfaceEffects {
    pub behind: Option<SurfaceEffect>,
    pub behind_root_surface: Option<SurfaceEffect>,
    pub in_front: Option<SurfaceEffect>,
    pub replace: Option<SurfaceEffect>,
    pub replace_subsurfaces: Option<SurfaceEffect>,
    pub behind_subsurfaces: Option<SurfaceEffect>,
}

impl SurfaceEffects {
    pub fn none() -> Self {
        Self::default()
    }

    pub fn behind(effect: impl Into<SurfaceEffect>) -> Self {
        Self {
            behind: Some(effect.into()),
            ..Self::default()
        }
    }

    pub(crate) fn compile(&self) -> WindowEffectConfig {
        let slot = |slot: &Option<SurfaceEffect>| slot.as_ref().map(SurfaceEffect::compile);
        WindowEffectConfig {
            behind: slot(&self.behind),
            behind_root_surface: slot(&self.behind_root_surface),
            in_front: slot(&self.in_front),
            replace: slot(&self.replace),
            replace_subsurfaces: slot(&self.replace_subsurfaces),
            behind_subsurfaces: slot(&self.behind_subsurfaces),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reactive::Scope;

    #[test]
    fn compiles_a_blur_with_a_reactive_uniform() {
        let scope = Scope::root();
        let tint = scope.signal(0.5_f64);
        let effect = Effect::new(backdrop_source())
            .capture_padding(24)
            .invalidate(Invalidate::on_source_damage_box(8))
            .stage(dual_kawase_blur(4, 2))
            .stage(
                shader_stage("/shaders/glass.frag")
                    .uniform("tint", tint)
                    .uniform("depth", 0.2),
            );
        assert!(effect.has_reactive_uniforms());
        let compiled = effect.compile();
        assert_eq!(compiled.capture_padding, 24);
        assert_eq!(
            compiled.blur_stage(),
            Some(BackdropBlur {
                radius: 4,
                passes: 2
            })
        );
        let shader = compiled.last_shader_stage().unwrap();
        assert_eq!(shader.uniforms["tint"], ShaderUniformValue::Float(0.5));
        tint.set(1.0);
        let shader = effect.compile().last_shader_stage().cloned().unwrap();
        assert_eq!(shader.uniforms["tint"], ShaderUniformValue::Float(1.0));
        scope.dispose();
    }
}
