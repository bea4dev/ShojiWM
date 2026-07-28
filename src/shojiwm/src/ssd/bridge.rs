use serde::Deserialize;

use super::{
    AlignItems, BackdropBlur, BackgroundEffectConfig, BlendMode, BorderFit, BorderStyle, BoxNode,
    ButtonNode, Color, CompiledEffect, DecorationInteractionHandlers, DecorationNode,
    DecorationNodeKind, DecorationStateChangeHandler, DecorationStyle, Edges, EffectAlphaMode,
    EffectInput, EffectInvalidationPolicy, EffectOutsets, EffectStage, ImageNode, JustifyContent,
    LabelNode, LayoutDirection, NodeTransform, NoiseKind, NoiseStage, Overflow, PointerEvents,
    PositionOffsets, ShaderEffectNode, ShaderModule, ShaderStage, ShaderUniformValue,
    StylePosition, WindowAction, WindowBorderInteraction, WindowEffectConfig, WindowEffectSlot,
    WindowResizeHitArea, WindowSourceInclude,
};

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum WireDecorationChild {
    Node(WireDecorationNode),
    Primitive(serde_json::Value),
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct WireDecorationNode {
    pub kind: String,
    #[serde(rename = "nodeId")]
    pub node_id: Option<String>,
    #[serde(default)]
    pub props: WireProps,
    #[serde(default)]
    pub children: Vec<WireDecorationChild>,
}

#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct WireProps {
    pub direction: Option<String>,
    pub split: Option<String>,
    pub text: Option<String>,
    pub icon: Option<serde_json::Value>,
    pub shader: Option<WireCompiledEffect>,
    pub src: Option<String>,
    pub fit: Option<String>,
    pub id: Option<String>,
    pub style: WireStyle,
    pub on_click: Option<WireOnClick>,
    pub on_hover_change: Option<WireStateChangeHandler>,
    pub on_active_change: Option<WireStateChangeHandler>,
    pub interaction: Option<WireWindowBorderInteraction>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WireWindowBorderInteraction {
    pub resize_hit_area: Option<WireResizeHitArea>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
pub enum WireResizeHitArea {
    Uniform(i32),
    Detailed(WireResizeHitAreaFields),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WireResizeHitAreaFields {
    pub edge_px: Option<i32>,
    pub corner_px: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WireShaderModule {
    pub kind: String,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WireShaderStageFields {
    pub shader: WireShaderModule,
    #[serde(default)]
    pub uniforms: std::collections::BTreeMap<String, WireShaderUniformValue>,
    #[serde(default)]
    pub textures: std::collections::BTreeMap<String, WireEffectInput>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum WireShaderUniformValue {
    Float(f32),
    Vec(Vec<f32>),
    Array(WireShaderUniformArray),
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WireShaderUniformArray {
    pub kind: String,
    pub element: String,
    pub values: Vec<WireShaderUniformArrayElement>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum WireShaderUniformArrayElement {
    Float(f32),
    Vec(Vec<f32>),
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WireDualKawaseBlurStageFields {
    pub radius: Option<i32>,
    pub passes: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum WireEffectStage {
    ShaderStage(WireShaderStageFields),
    DualKawaseBlur(WireDualKawaseBlurStageFields),
    Noise(WireNoiseStageFields),
    Save(WireSaveStageFields),
    Blend(WireBlendStageFields),
    Unit(WireUnitStageFields),
    RenderTo(WireRenderToStageFields),
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WireCompiledEffect {
    pub kind: String,
    pub input: Option<WireEffectInput>,
    #[serde(default)]
    pub capture_padding: i32,
    pub invalidate: Option<WireEffectInvalidationPolicy>,
    #[serde(default)]
    pub pipeline: Vec<WireEffectStage>,
    /// Output alpha handling: "opaque" (default) or "preserve".
    /// See `EffectAlphaMode` for the semantics.
    #[serde(default)]
    pub alpha: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
pub enum WireEffectInvalidationPolicy {
    OnSourceDamageBox {
        damage_padding: i32,
    },
    Always,
    Manual {
        dirty_when: bool,
        base: Option<Box<WireAutomaticEffectInvalidationPolicy>>,
    },
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
pub enum WireAutomaticEffectInvalidationPolicy {
    OnSourceDamageBox { damage_padding: i32 },
    Always,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum WireEffectInput {
    BackdropSource,
    XrayBackdropSource,
    WindowSource { include: Option<String> },
    LayerSource { include: Option<String> },
    PopupSource { include: Option<String> },
    ShaderInput(WireShaderStageFields),
    ImageSource { path: String },
    NamedTexture { name: String },
    StateSource { state: WireStateTexture },
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WireNoiseStageFields {
    pub noise_kind: Option<String>,
    pub amount: Option<f32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WireSaveStageFields {
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WireBlendStageFields {
    pub input: WireEffectInput,
    pub mode: Option<String>,
    pub alpha: Option<f32>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WireUnitStageFields {
    pub effect: WireCompiledEffect,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WireStateTexture {
    pub kind: String,
    pub name: String,
    pub scale: f32,
    pub format: String,
    pub resize: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WireRenderToStageFields {
    pub target: WireStateTexture,
    pub effect: WireCompiledEffect,
}

pub type WireBackgroundEffectConfig = WireCompiledEffect;

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WireWindowEffectConfig {
    pub behind: Option<WireWindowEffectSlot>,
    #[serde(rename = "behindRootSurface")]
    pub behind_root_surface: Option<WireWindowEffectSlot>,
    #[serde(rename = "inFront")]
    pub in_front: Option<WireWindowEffectSlot>,
    pub replace: Option<WireWindowEffectSlot>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WireWindowEffectSlot {
    pub kind: String,
    pub effect: WireCompiledEffect,
    pub outsets: Option<WireEffectOutsets>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum WireEffectOutsets {
    Uniform(i32),
    Edges {
        left: Option<i32>,
        right: Option<i32>,
        top: Option<i32>,
        bottom: Option<i32>,
    },
}

#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct WireStyle {
    pub width: Option<WireDimension>,
    pub height: Option<WireDimension>,
    pub min_width: Option<i32>,
    pub min_height: Option<i32>,
    pub max_width: Option<i32>,
    pub max_height: Option<i32>,
    pub flex_grow: Option<f32>,
    pub flex_shrink: Option<f32>,
    pub gap: Option<i32>,
    pub padding: Option<i32>,
    pub padding_x: Option<i32>,
    pub padding_y: Option<i32>,
    pub padding_top: Option<i32>,
    pub padding_right: Option<i32>,
    pub padding_bottom: Option<i32>,
    pub padding_left: Option<i32>,
    pub margin: Option<i32>,
    pub margin_x: Option<i32>,
    pub margin_y: Option<i32>,
    pub margin_top: Option<i32>,
    pub margin_right: Option<i32>,
    pub margin_bottom: Option<i32>,
    pub margin_left: Option<i32>,
    pub position: Option<String>,
    pub z_index: Option<i32>,
    pub inset: Option<i32>,
    pub top: Option<i32>,
    pub right: Option<i32>,
    pub bottom: Option<i32>,
    pub left: Option<i32>,
    pub overflow: Option<String>,
    pub pointer_events: Option<String>,
    pub transform: Option<WireNodeTransform>,
    pub align_items: Option<String>,
    pub justify_content: Option<String>,
    pub background: Option<String>,
    pub color: Option<String>,
    pub opacity: Option<f32>,
    pub border: Option<WireBorderValue>,
    pub border_top: Option<WireBorderValue>,
    pub border_right: Option<WireBorderValue>,
    pub border_bottom: Option<WireBorderValue>,
    pub border_left: Option<WireBorderValue>,
    pub border_fit: Option<String>,
    pub border_radius: Option<i32>,
    pub visible: Option<bool>,
    pub cursor: Option<String>,
    pub font_size: Option<i32>,
    pub font_weight: Option<serde_json::Value>,
    pub font_family: Option<WireFontFamily>,
    pub text_align: Option<String>,
    pub line_height: Option<i32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct WireNodeTransform {
    pub translate_x: Option<f32>,
    pub translate_y: Option<f32>,
    pub scale: Option<f32>,
    pub scale_x: Option<f32>,
    pub scale_y: Option<f32>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum WireFontFamily {
    Single(String),
    Multiple(Vec<String>),
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum WireDimension {
    Pixels(i32),
    Keyword(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct WireBorderValue {
    pub px: i32,
    pub color: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WireWindowAction {
    Close,
    Maximize,
    Unmaximize,
    Minimize,
}

#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum DecorationBridgeError {
    #[error("failed to decode decoration json: {0}")]
    InvalidJson(String),
    #[error("primitive child nodes are not supported in the rust bridge yet")]
    UnsupportedPrimitiveChild,
    #[error("unsupported node kind: {0}")]
    UnsupportedNodeKind(String),
    #[error("invalid shader descriptor")]
    InvalidShaderDescriptor,
    #[error("invalid shader type: {0}")]
    InvalidShaderType(String),
    #[error("invalid effect input")]
    InvalidEffectInput,
    #[error("unsupported dimension keyword: {0}")]
    UnsupportedDimensionKeyword(String),
    #[error("invalid direction: {0}")]
    InvalidDirection(String),
    #[error("invalid alignItems value: {0}")]
    InvalidAlignItems(String),
    #[error("invalid justifyContent value: {0}")]
    InvalidJustifyContent(String),
    #[error("invalid borderFit value: {0}")]
    InvalidBorderFit(String),
    #[error("invalid position value: {0}")]
    InvalidPosition(String),
    #[error("invalid overflow value: {0}")]
    InvalidOverflow(String),
    #[error("invalid pointerEvents value: {0}")]
    InvalidPointerEvents(String),
    #[error("invalid color string: {0}")]
    InvalidColor(String),
    #[error("invalid image fit value: {0}")]
    InvalidImageFit(String),
}

pub fn decode_tree_json(input: &str) -> Result<DecorationNode, DecorationBridgeError> {
    let wire: WireDecorationNode = serde_json::from_str(input)
        .map_err(|err| DecorationBridgeError::InvalidJson(err.to_string()))?;
    wire.try_into()
}

impl TryFrom<WireDecorationNode> for DecorationNode {
    type Error = DecorationBridgeError;

    fn try_from(value: WireDecorationNode) -> Result<Self, Self::Error> {
        let kind = match value.kind.as_str() {
            "Box" => DecorationNodeKind::Box(BoxNode {
                direction: parse_direction(value.props.direction.or(value.props.split))?,
            }),
            "Label" => DecorationNodeKind::Label(LabelNode {
                text: value.props.text.unwrap_or_default(),
            }),
            "Button" => DecorationNodeKind::Button(ButtonNode {
                action: value
                    .props
                    .on_click
                    .unwrap_or(WireOnClick::Action(WireWindowAction::Close))
                    .try_into()?,
            }),
            "AppIcon" => DecorationNodeKind::AppIcon,
            "Image" => DecorationNodeKind::Image(ImageNode {
                src: value.props.src.clone().unwrap_or_default(),
                fit: parse_image_fit(value.props.fit.as_deref())?,
            }),
            "ShaderEffect" => DecorationNodeKind::ShaderEffect(ShaderEffectNode {
                direction: parse_direction(value.props.direction.or(value.props.split))?,
                shader: value
                    .props
                    .shader
                    .ok_or(DecorationBridgeError::InvalidShaderDescriptor)?
                    .try_into()?,
            }),
            "Window" => DecorationNodeKind::WindowSlot,
            "WindowBorder" => DecorationNodeKind::WindowBorder,
            "ManagedWindow" => DecorationNodeKind::Box(BoxNode {
                direction: LayoutDirection::Column,
            }),
            "Fragment" => DecorationNodeKind::Box(BoxNode {
                direction: LayoutDirection::Column,
            }),
            other => {
                return Err(DecorationBridgeError::UnsupportedNodeKind(
                    other.to_string(),
                ));
            }
        };

        let window_border_interaction = if matches!(kind, DecorationNodeKind::WindowBorder) {
            value
                .props
                .interaction
                .map(TryInto::try_into)
                .transpose()?
                .unwrap_or_default()
        } else {
            WindowBorderInteraction::default()
        };
        let style = DecorationStyle::try_from(value.props.style)?;
        let children = value
            .children
            .into_iter()
            .map(TryInto::try_into)
            .collect::<Result<Vec<_>, _>>()?;

        Ok(DecorationNode {
            stable_id: value.node_id,
            interaction: DecorationInteractionHandlers {
                hover_change: value
                    .props
                    .on_hover_change
                    .map(TryInto::try_into)
                    .transpose()?,
                active_change: value
                    .props
                    .on_active_change
                    .map(TryInto::try_into)
                    .transpose()?,
            },
            window_border_interaction,
            kind,
            style,
            children,
        })
    }
}

impl TryFrom<WireWindowBorderInteraction> for WindowBorderInteraction {
    type Error = DecorationBridgeError;

    fn try_from(value: WireWindowBorderInteraction) -> Result<Self, Self::Error> {
        Ok(Self {
            resize_hit_area: value
                .resize_hit_area
                .map(WireResizeHitArea::into_resize_hit_area),
        })
    }
}

impl WireResizeHitArea {
    fn into_resize_hit_area(self) -> WindowResizeHitArea {
        match self {
            WireResizeHitArea::Uniform(width) => WindowResizeHitArea::uniform(width),
            WireResizeHitArea::Detailed(fields) => WindowResizeHitArea {
                edge_width: fields.edge_px,
                corner_width: fields.corner_px,
            },
        }
    }
}

impl TryFrom<WireCompiledEffect> for CompiledEffect {
    type Error = DecorationBridgeError;

    fn try_from(value: WireCompiledEffect) -> Result<Self, Self::Error> {
        if value.kind != "compiled-effect" {
            return Err(DecorationBridgeError::InvalidShaderDescriptor);
        }

        let input = decode_effect_input(value.input.unwrap_or(WireEffectInput::BackdropSource))?;

        let mut stages = Vec::with_capacity(value.pipeline.len());
        for stage in value.pipeline {
            match stage {
                WireEffectStage::ShaderStage(stage) => {
                    if stage.shader.kind != "shader-module" || stage.shader.path.is_empty() {
                        return Err(DecorationBridgeError::InvalidShaderDescriptor);
                    }
                    if stage.textures.len() > 7
                        || stage
                            .textures
                            .keys()
                            .any(|name| is_reserved_effect_binding_name(name))
                        || stage
                            .uniforms
                            .keys()
                            .any(|name| is_reserved_effect_binding_name(name))
                        || stage
                            .textures
                            .keys()
                            .any(|name| stage.uniforms.contains_key(name))
                    {
                        return Err(DecorationBridgeError::InvalidShaderDescriptor);
                    }
                    let mut uniforms = std::collections::BTreeMap::new();
                    for (name, value) in stage.uniforms {
                        let value = decode_shader_uniform(value)
                            .ok_or(DecorationBridgeError::InvalidShaderDescriptor)?;
                        uniforms.insert(name, value);
                    }
                    let textures = stage
                        .textures
                        .into_iter()
                        .map(|(name, input)| Ok((name, decode_effect_input(input)?)))
                        .collect::<Result<_, DecorationBridgeError>>()?;
                    stages.push(EffectStage::Shader(ShaderStage {
                        shader: ShaderModule {
                            path: stage.shader.path,
                        },
                        uniforms,
                        textures,
                    }));
                }
                WireEffectStage::DualKawaseBlur(stage) => {
                    stages.push(EffectStage::DualKawaseBlur(BackdropBlur {
                        radius: stage.radius.unwrap_or(8).max(0),
                        passes: stage.passes.unwrap_or(2).clamp(0, 8),
                    }));
                }
                WireEffectStage::Noise(stage) => {
                    let kind = match stage.noise_kind.as_deref().unwrap_or("salt") {
                        "salt" => NoiseKind::Salt,
                        other => {
                            return Err(DecorationBridgeError::InvalidShaderType(
                                other.to_string(),
                            ));
                        }
                    };
                    stages.push(EffectStage::Noise(NoiseStage {
                        kind,
                        amount: stage.amount.unwrap_or(0.01).clamp(0.0, 1.0),
                    }));
                }
                WireEffectStage::Save(stage) => {
                    if stage.name.is_empty() {
                        return Err(DecorationBridgeError::InvalidShaderDescriptor);
                    }
                    stages.push(EffectStage::Save(stage.name));
                }
                WireEffectStage::Blend(stage) => {
                    let input = decode_effect_input(stage.input)?;
                    let mode = match stage.mode.as_deref().unwrap_or("normal") {
                        "normal" => BlendMode::Normal,
                        "add" => BlendMode::Add,
                        "screen" => BlendMode::Screen,
                        "multiply" => BlendMode::Multiply,
                        other => {
                            return Err(DecorationBridgeError::InvalidShaderType(
                                other.to_string(),
                            ));
                        }
                    };
                    stages.push(EffectStage::Blend {
                        input,
                        mode,
                        alpha: stage.alpha.unwrap_or(1.0).clamp(0.0, 1.0),
                    });
                }
                WireEffectStage::Unit(stage) => {
                    stages.push(EffectStage::Unit(Box::new(stage.effect.try_into()?)));
                }
                WireEffectStage::RenderTo(stage) => {
                    stages.push(EffectStage::RenderTo {
                        target: decode_state_texture(stage.target)?,
                        effect: Box::new(stage.effect.try_into()?),
                    });
                }
            }
        }

        if stages.is_empty() && !matches!(input, EffectInput::Shader(_)) {
            return Err(DecorationBridgeError::InvalidShaderDescriptor);
        }

        let invalidate = match value
            .invalidate
            .unwrap_or(WireEffectInvalidationPolicy::OnSourceDamageBox { damage_padding: 0 })
        {
            WireEffectInvalidationPolicy::OnSourceDamageBox { damage_padding } => {
                EffectInvalidationPolicy::OnSourceDamageBox {
                    damage_padding: damage_padding.max(0),
                }
            }
            WireEffectInvalidationPolicy::Always => EffectInvalidationPolicy::Always,
            WireEffectInvalidationPolicy::Manual { dirty_when, base } => {
                EffectInvalidationPolicy::Manual {
                    dirty_when,
                    base: base
                        .map(|policy| Box::new(decode_automatic_invalidation_policy(*policy))),
                }
            }
        };

        let alpha = match value.alpha.as_deref() {
            None | Some("opaque") => EffectAlphaMode::Opaque,
            Some("preserve") => EffectAlphaMode::Preserve,
            Some(_) => return Err(DecorationBridgeError::InvalidShaderDescriptor),
        };

        let effect = CompiledEffect {
            input,
            capture_padding: value.capture_padding.max(0),
            invalidate,
            pipeline: stages,
            alpha,
        };
        validate_effect_state_descriptors(&effect)?;
        Ok(effect)
    }
}

impl TryFrom<WireBackgroundEffectConfig> for BackgroundEffectConfig {
    type Error = DecorationBridgeError;

    fn try_from(value: WireBackgroundEffectConfig) -> Result<Self, Self::Error> {
        Ok(BackgroundEffectConfig {
            effect: value.try_into()?,
        })
    }
}

impl TryFrom<WireWindowEffectConfig> for WindowEffectConfig {
    type Error = DecorationBridgeError;

    fn try_from(value: WireWindowEffectConfig) -> Result<Self, Self::Error> {
        Ok(WindowEffectConfig {
            behind: value.behind.map(TryInto::try_into).transpose()?,
            behind_root_surface: value
                .behind_root_surface
                .map(TryInto::try_into)
                .transpose()?,
            in_front: value.in_front.map(TryInto::try_into).transpose()?,
            replace: value.replace.map(TryInto::try_into).transpose()?,
        })
    }
}

impl TryFrom<WireWindowEffectSlot> for WindowEffectSlot {
    type Error = DecorationBridgeError;

    fn try_from(value: WireWindowEffectSlot) -> Result<Self, Self::Error> {
        if value.kind != "window-effect"
            && value.kind != "layer-effect"
            && value.kind != "popup-effect"
        {
            return Err(DecorationBridgeError::InvalidShaderDescriptor);
        }

        Ok(WindowEffectSlot {
            effect: value.effect.try_into()?,
            outsets: decode_effect_outsets(value.outsets),
        })
    }
}

fn decode_effect_outsets(value: Option<WireEffectOutsets>) -> EffectOutsets {
    match value {
        Some(WireEffectOutsets::Uniform(value)) => {
            let value = value.max(0);
            EffectOutsets {
                left: value,
                right: value,
                top: value,
                bottom: value,
            }
        }
        Some(WireEffectOutsets::Edges {
            left,
            right,
            top,
            bottom,
        }) => EffectOutsets {
            left: left.unwrap_or(0).max(0),
            right: right.unwrap_or(0).max(0),
            top: top.unwrap_or(0).max(0),
            bottom: bottom.unwrap_or(0).max(0),
        },
        None => EffectOutsets::default(),
    }
}

fn decode_effect_input(value: WireEffectInput) -> Result<EffectInput, DecorationBridgeError> {
    Ok(match value {
        WireEffectInput::BackdropSource => EffectInput::Backdrop,
        WireEffectInput::XrayBackdropSource => EffectInput::XrayBackdrop,
        WireEffectInput::WindowSource { include } => {
            let include = match include.as_deref().unwrap_or("full") {
                "full" => WindowSourceInclude::Full,
                "root-surface" => WindowSourceInclude::RootSurface,
                _ => return Err(DecorationBridgeError::InvalidEffectInput),
            };
            EffectInput::WindowSource(include)
        }
        WireEffectInput::LayerSource { include } => {
            let include = match include.as_deref().unwrap_or("full") {
                "full" => WindowSourceInclude::Full,
                "root-surface" => WindowSourceInclude::RootSurface,
                _ => return Err(DecorationBridgeError::InvalidEffectInput),
            };
            EffectInput::LayerSource(include)
        }
        WireEffectInput::PopupSource { include } => {
            let include = match include.as_deref().unwrap_or("full") {
                "full" => WindowSourceInclude::Full,
                "root-surface" => WindowSourceInclude::RootSurface,
                _ => return Err(DecorationBridgeError::InvalidEffectInput),
            };
            EffectInput::PopupSource(include)
        }
        WireEffectInput::ShaderInput(stage) => {
            if stage.shader.kind != "shader-module" || stage.shader.path.is_empty() {
                return Err(DecorationBridgeError::InvalidEffectInput);
            }
            if stage.textures.len() > 7
                || stage
                    .textures
                    .keys()
                    .any(|name| is_reserved_effect_binding_name(name))
                || stage
                    .uniforms
                    .keys()
                    .any(|name| is_reserved_effect_binding_name(name))
                || stage
                    .textures
                    .keys()
                    .any(|name| stage.uniforms.contains_key(name))
            {
                return Err(DecorationBridgeError::InvalidEffectInput);
            }
            let mut uniforms = std::collections::BTreeMap::new();
            for (name, value) in stage.uniforms {
                let value = decode_shader_uniform(value)
                    .ok_or(DecorationBridgeError::InvalidEffectInput)?;
                uniforms.insert(name, value);
            }
            EffectInput::Shader(ShaderStage {
                shader: ShaderModule {
                    path: stage.shader.path,
                },
                uniforms,
                textures: stage
                    .textures
                    .into_iter()
                    .map(|(name, input)| Ok((name, decode_effect_input(input)?)))
                    .collect::<Result<_, DecorationBridgeError>>()?,
            })
        }
        WireEffectInput::ImageSource { path } => {
            if path.is_empty() {
                return Err(DecorationBridgeError::InvalidEffectInput);
            }
            EffectInput::Image(path)
        }
        WireEffectInput::NamedTexture { name } => {
            if name.is_empty() {
                return Err(DecorationBridgeError::InvalidEffectInput);
            }
            EffectInput::Named(name)
        }
        WireEffectInput::StateSource { state } => EffectInput::State(decode_state_texture(state)?),
    })
}

fn decode_state_texture(
    value: WireStateTexture,
) -> Result<crate::ssd::EffectStateTexture, DecorationBridgeError> {
    if value.kind != "state-texture"
        || value.name.is_empty()
        || !value.scale.is_finite()
        || value.scale <= 0.0
        || value.scale > 8.0
    {
        return Err(DecorationBridgeError::InvalidShaderDescriptor);
    }
    let format = match value.format.as_str() {
        "rgba8" => crate::ssd::EffectStateTextureFormat::Rgba8,
        "rg16f" => crate::ssd::EffectStateTextureFormat::Rg16f,
        "rgba16f" => crate::ssd::EffectStateTextureFormat::Rgba16f,
        _ => return Err(DecorationBridgeError::InvalidShaderDescriptor),
    };
    let resize = match value.resize.as_str() {
        "clear" => crate::ssd::EffectStateResizePolicy::Clear,
        "stretch" => crate::ssd::EffectStateResizePolicy::Stretch,
        _ => return Err(DecorationBridgeError::InvalidShaderDescriptor),
    };
    Ok(crate::ssd::EffectStateTexture {
        name: value.name,
        scale: value.scale,
        format,
        resize,
    })
}

fn validate_effect_state_descriptors(effect: &CompiledEffect) -> Result<(), DecorationBridgeError> {
    fn insert(
        descriptors: &mut std::collections::BTreeMap<String, crate::ssd::EffectStateTexture>,
        descriptor: &crate::ssd::EffectStateTexture,
    ) -> Result<(), DecorationBridgeError> {
        if descriptors
            .get(&descriptor.name)
            .is_some_and(|existing| existing != descriptor)
        {
            return Err(DecorationBridgeError::InvalidShaderDescriptor);
        }
        descriptors.insert(descriptor.name.clone(), descriptor.clone());
        Ok(())
    }

    fn visit_input(
        input: &EffectInput,
        descriptors: &mut std::collections::BTreeMap<String, crate::ssd::EffectStateTexture>,
    ) -> Result<(), DecorationBridgeError> {
        match input {
            EffectInput::State(descriptor) => insert(descriptors, descriptor),
            EffectInput::Shader(shader) => {
                for input in shader.textures.values() {
                    visit_input(input, descriptors)?;
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn visit_effect(
        effect: &CompiledEffect,
        descriptors: &mut std::collections::BTreeMap<String, crate::ssd::EffectStateTexture>,
    ) -> Result<(), DecorationBridgeError> {
        visit_input(&effect.input, descriptors)?;
        for stage in &effect.pipeline {
            match stage {
                EffectStage::Shader(shader) => {
                    for input in shader.textures.values() {
                        visit_input(input, descriptors)?;
                    }
                }
                EffectStage::Blend { input, .. } => visit_input(input, descriptors)?,
                EffectStage::Unit(effect) => visit_effect(effect, descriptors)?,
                EffectStage::RenderTo { target, effect } => {
                    insert(descriptors, target)?;
                    visit_effect(effect, descriptors)?;
                }
                _ => {}
            }
        }
        Ok(())
    }

    visit_effect(effect, &mut std::collections::BTreeMap::new())
}

fn decode_shader_uniform(value: WireShaderUniformValue) -> Option<ShaderUniformValue> {
    match value {
        WireShaderUniformValue::Float(value) => Some(ShaderUniformValue::Float(value)),
        WireShaderUniformValue::Vec(value) => match value.as_slice() {
            [x, y] => Some(ShaderUniformValue::Vec2([*x, *y])),
            [x, y, z] => Some(ShaderUniformValue::Vec3([*x, *y, *z])),
            [x, y, z, w] => Some(ShaderUniformValue::Vec4([*x, *y, *z, *w])),
            _ => None,
        },
        WireShaderUniformValue::Array(array) => {
            if array.kind != "uniform-array" || array.values.is_empty() {
                return None;
            }
            match array.element.as_str() {
                "float" => array
                    .values
                    .into_iter()
                    .map(|entry| match entry {
                        WireShaderUniformArrayElement::Float(value) => Some(value),
                        WireShaderUniformArrayElement::Vec(_) => None,
                    })
                    .collect::<Option<Vec<_>>>()
                    .map(ShaderUniformValue::FloatArray),
                "vec2" => decode_shader_uniform_vectors::<2>(array.values)
                    .map(ShaderUniformValue::Vec2Array),
                "vec3" => decode_shader_uniform_vectors::<3>(array.values)
                    .map(ShaderUniformValue::Vec3Array),
                "vec4" => decode_shader_uniform_vectors::<4>(array.values)
                    .map(ShaderUniformValue::Vec4Array),
                _ => None,
            }
        }
    }
}

fn decode_shader_uniform_vectors<const N: usize>(
    values: Vec<WireShaderUniformArrayElement>,
) -> Option<Vec<[f32; N]>> {
    values
        .into_iter()
        .map(|entry| match entry {
            WireShaderUniformArrayElement::Vec(values) => values.try_into().ok(),
            WireShaderUniformArrayElement::Float(_) => None,
        })
        .collect()
}

fn is_reserved_effect_binding_name(name: &str) -> bool {
    name.is_empty()
        || matches!(
            name,
            "tex" | "effect_texture_size_px" | "effect_content_rect_px"
        )
}

fn decode_automatic_invalidation_policy(
    value: WireAutomaticEffectInvalidationPolicy,
) -> EffectInvalidationPolicy {
    match value {
        WireAutomaticEffectInvalidationPolicy::OnSourceDamageBox { damage_padding } => {
            EffectInvalidationPolicy::OnSourceDamageBox {
                damage_padding: damage_padding.max(0),
            }
        }
        WireAutomaticEffectInvalidationPolicy::Always => EffectInvalidationPolicy::Always,
    }
}

impl TryFrom<WireDecorationChild> for DecorationNode {
    type Error = DecorationBridgeError;

    fn try_from(value: WireDecorationChild) -> Result<Self, Self::Error> {
        match value {
            WireDecorationChild::Node(node) => node.try_into(),
            WireDecorationChild::Primitive(_) => {
                Err(DecorationBridgeError::UnsupportedPrimitiveChild)
            }
        }
    }
}

impl TryFrom<WireStyle> for DecorationStyle {
    type Error = DecorationBridgeError;

    fn try_from(value: WireStyle) -> Result<Self, Self::Error> {
        Ok(DecorationStyle {
            width: parse_dimension(value.width)?,
            height: parse_dimension(value.height)?,
            min_width: value.min_width,
            min_height: value.min_height,
            max_width: value.max_width,
            max_height: value.max_height,
            flex_grow: value.flex_grow,
            flex_shrink: value.flex_shrink,
            padding: edges_from_parts(
                value.padding,
                value.padding_x,
                value.padding_y,
                value.padding_top,
                value.padding_right,
                value.padding_bottom,
                value.padding_left,
            ),
            margin: edges_from_parts(
                value.margin,
                value.margin_x,
                value.margin_y,
                value.margin_top,
                value.margin_right,
                value.margin_bottom,
                value.margin_left,
            ),
            position: value.position.map(parse_position).transpose()?,
            z_index: value.z_index,
            inset: position_offsets_from_parts(
                value.inset,
                value.top,
                value.right,
                value.bottom,
                value.left,
            ),
            overflow: value.overflow.map(parse_overflow).transpose()?,
            pointer_events: value.pointer_events.map(parse_pointer_events).transpose()?,
            transform: value.transform.map(parse_node_transform),
            gap: value.gap,
            justify_content: value
                .justify_content
                .map(parse_justify_content)
                .transpose()?,
            align_items: value.align_items.map(parse_align_items).transpose()?,
            background: value.background.map(|s| parse_color(&s)).transpose()?,
            color: value.color.map(|s| parse_color(&s)).transpose()?,
            opacity: value.opacity,
            border: value
                .border
                .map(|border| parse_border(border))
                .transpose()?,
            border_top: value
                .border_top
                .map(|border| parse_border(border))
                .transpose()?,
            border_right: value
                .border_right
                .map(|border| parse_border(border))
                .transpose()?,
            border_bottom: value
                .border_bottom
                .map(|border| parse_border(border))
                .transpose()?,
            border_left: value
                .border_left
                .map(|border| parse_border(border))
                .transpose()?,
            border_fit: value.border_fit.map(parse_border_fit).transpose()?,
            border_radius: value.border_radius,
            visible: value.visible,
            cursor: value.cursor,
            font_size: value.font_size,
            font_weight: value.font_weight,
            font_family: value.font_family.map(|family| match family {
                WireFontFamily::Single(name) => vec![name],
                WireFontFamily::Multiple(names) => names,
            }),
            text_align: value.text_align,
            line_height: value.line_height,
        })
    }
}

fn parse_border_fit(input: String) -> Result<BorderFit, DecorationBridgeError> {
    match input.as_str() {
        "normal" => Ok(BorderFit::Normal),
        "fit-children" => Ok(BorderFit::FitChildren),
        other => Err(DecorationBridgeError::InvalidBorderFit(other.into())),
    }
}

fn parse_position(input: String) -> Result<StylePosition, DecorationBridgeError> {
    match input.as_str() {
        "relative" => Ok(StylePosition::Relative),
        "absolute" => Ok(StylePosition::Absolute),
        other => Err(DecorationBridgeError::InvalidPosition(other.into())),
    }
}

fn parse_overflow(input: String) -> Result<Overflow, DecorationBridgeError> {
    match input.as_str() {
        "visible" => Ok(Overflow::Visible),
        "hidden" => Ok(Overflow::Hidden),
        other => Err(DecorationBridgeError::InvalidOverflow(other.into())),
    }
}

fn parse_pointer_events(input: String) -> Result<PointerEvents, DecorationBridgeError> {
    match input.as_str() {
        "auto" => Ok(PointerEvents::Auto),
        "none" => Ok(PointerEvents::None),
        other => Err(DecorationBridgeError::InvalidPointerEvents(other.into())),
    }
}

fn parse_node_transform(input: WireNodeTransform) -> NodeTransform {
    let scale = input.scale.unwrap_or(1.0);
    NodeTransform {
        translate_x: input.translate_x.unwrap_or(0.0),
        translate_y: input.translate_y.unwrap_or(0.0),
        scale_x: input.scale_x.unwrap_or(scale),
        scale_y: input.scale_y.unwrap_or(scale),
    }
}

fn parse_image_fit(input: Option<&str>) -> Result<crate::ssd::ImageFit, DecorationBridgeError> {
    match input.unwrap_or("contain") {
        "contain" => Ok(crate::ssd::ImageFit::Contain),
        "cover" => Ok(crate::ssd::ImageFit::Cover),
        "fill" => Ok(crate::ssd::ImageFit::Fill),
        other => Err(DecorationBridgeError::InvalidImageFit(other.to_string())),
    }
}

fn parse_direction(input: Option<String>) -> Result<LayoutDirection, DecorationBridgeError> {
    match input.as_deref().unwrap_or("column") {
        "row" | "horizontal" => Ok(LayoutDirection::Row),
        "column" | "vertical" => Ok(LayoutDirection::Column),
        other => Err(DecorationBridgeError::InvalidDirection(other.to_string())),
    }
}

fn parse_align_items(input: String) -> Result<AlignItems, DecorationBridgeError> {
    match input.as_str() {
        "start" => Ok(AlignItems::Start),
        "center" => Ok(AlignItems::Center),
        "end" => Ok(AlignItems::End),
        "stretch" => Ok(AlignItems::Stretch),
        other => Err(DecorationBridgeError::InvalidAlignItems(other.to_string())),
    }
}

fn parse_justify_content(input: String) -> Result<JustifyContent, DecorationBridgeError> {
    match input.as_str() {
        "start" => Ok(JustifyContent::Start),
        "center" => Ok(JustifyContent::Center),
        "end" => Ok(JustifyContent::End),
        "space-between" => Ok(JustifyContent::SpaceBetween),
        other => Err(DecorationBridgeError::InvalidJustifyContent(
            other.to_string(),
        )),
    }
}

fn parse_dimension(input: Option<WireDimension>) -> Result<Option<i32>, DecorationBridgeError> {
    match input {
        Some(WireDimension::Pixels(value)) => Ok(Some(value)),
        Some(WireDimension::Keyword(keyword)) => {
            Err(DecorationBridgeError::UnsupportedDimensionKeyword(keyword))
        }
        None => Ok(None),
    }
}

fn parse_border(input: WireBorderValue) -> Result<BorderStyle, DecorationBridgeError> {
    Ok(BorderStyle {
        width: input.px,
        color: parse_color(&input.color)?,
    })
}

fn position_offsets_from_parts(
    inset: Option<i32>,
    top: Option<i32>,
    right: Option<i32>,
    bottom: Option<i32>,
    left: Option<i32>,
) -> PositionOffsets {
    PositionOffsets {
        top: top.or(inset),
        right: right.or(inset),
        bottom: bottom.or(inset),
        left: left.or(inset),
    }
}

fn edges_from_parts(
    all: Option<i32>,
    horizontal: Option<i32>,
    vertical: Option<i32>,
    top: Option<i32>,
    right: Option<i32>,
    bottom: Option<i32>,
    left: Option<i32>,
) -> Edges {
    let base = all.unwrap_or(0);
    let horizontal = horizontal.unwrap_or(base);
    let vertical = vertical.unwrap_or(base);

    Edges {
        top: top.unwrap_or(vertical),
        right: right.unwrap_or(horizontal),
        bottom: bottom.unwrap_or(vertical),
        left: left.unwrap_or(horizontal),
    }
}

fn parse_color(input: &str) -> Result<Color, DecorationBridgeError> {
    let trimmed = input.trim();
    let hex = trimmed
        .strip_prefix('#')
        .ok_or_else(|| DecorationBridgeError::InvalidColor(trimmed.to_string()))?;

    match hex.len() {
        6 => {
            let r = u8::from_str_radix(&hex[0..2], 16)
                .map_err(|_| DecorationBridgeError::InvalidColor(trimmed.to_string()))?;
            let g = u8::from_str_radix(&hex[2..4], 16)
                .map_err(|_| DecorationBridgeError::InvalidColor(trimmed.to_string()))?;
            let b = u8::from_str_radix(&hex[4..6], 16)
                .map_err(|_| DecorationBridgeError::InvalidColor(trimmed.to_string()))?;
            Ok(Color::rgba(r, g, b, 255))
        }
        8 => {
            let r = u8::from_str_radix(&hex[0..2], 16)
                .map_err(|_| DecorationBridgeError::InvalidColor(trimmed.to_string()))?;
            let g = u8::from_str_radix(&hex[2..4], 16)
                .map_err(|_| DecorationBridgeError::InvalidColor(trimmed.to_string()))?;
            let b = u8::from_str_radix(&hex[4..6], 16)
                .map_err(|_| DecorationBridgeError::InvalidColor(trimmed.to_string()))?;
            let a = u8::from_str_radix(&hex[6..8], 16)
                .map_err(|_| DecorationBridgeError::InvalidColor(trimmed.to_string()))?;
            Ok(Color::rgba(r, g, b, a))
        }
        _ => Err(DecorationBridgeError::InvalidColor(trimmed.to_string())),
    }
}

impl From<WireWindowAction> for WindowAction {
    fn from(value: WireWindowAction) -> Self {
        match value {
            WireWindowAction::Close => WindowAction::Close,
            WireWindowAction::Maximize => WindowAction::Maximize,
            WireWindowAction::Unmaximize => WindowAction::Unmaximize,
            WireWindowAction::Minimize => WindowAction::Minimize,
        }
    }
}

impl TryFrom<WireOnClick> for WindowAction {
    type Error = DecorationBridgeError;

    fn try_from(value: WireOnClick) -> Result<Self, Self::Error> {
        match value {
            WireOnClick::Action(action) => Ok(action.into()),
            WireOnClick::RuntimeHandler(handler) => {
                if handler.kind == "runtime-handler" {
                    Ok(WindowAction::RuntimeHandler(handler.id))
                } else {
                    Err(DecorationBridgeError::UnsupportedNodeKind(handler.kind))
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ssd::{DecorationNodeKind, LayoutDirection};

    #[test]
    fn decode_simple_window_border_tree() {
        let json = r##"
        {
          "kind": "WindowBorder",
          "props": {
            "interaction": {
              "resizeHitArea": { "edgePx": 8, "cornerPx": 14 }
            },
            "style": {
              "border": { "px": 1, "color": "#ffffff" }
            }
          },
          "children": [
            {
              "kind": "Box",
              "props": { "direction": "column" },
              "children": [
                { "kind": "Label", "props": { "text": "Title" }, "children": [] },
                { "kind": "Window", "props": {}, "children": [] }
              ]
            }
          ]
        }
        "##;

        let tree = decode_tree_json(json).expect("json should decode");

        assert!(matches!(tree.kind, DecorationNodeKind::WindowBorder));
        assert_eq!(tree.style.border.unwrap().width, 1);
        assert_eq!(
            tree.window_border_interaction
                .resize_hit_area
                .expect("resize hit area should decode"),
            WindowResizeHitArea {
                edge_width: Some(8),
                corner_width: Some(14),
            }
        );
        assert!(matches!(
            tree.children[0].kind,
            DecorationNodeKind::Box(BoxNode {
                direction: LayoutDirection::Column
            })
        ));
    }

    #[test]
    fn invalid_color_is_rejected() {
        let json = r##"
        {
          "kind": "WindowBorder",
          "props": { "style": { "background": "red" } },
          "children": [{ "kind": "Window", "props": {}, "children": [] }]
        }
        "##;

        let err = decode_tree_json(json).expect_err("invalid colors must fail");
        assert_eq!(err, DecorationBridgeError::InvalidColor("red".into()));
    }

    #[test]
    fn primitive_children_are_rejected_by_bridge() {
        let json = r##"
        {
          "kind": "Label",
          "props": { "text": "Title" },
          "children": ["hello"]
        }
        "##;

        let err = decode_tree_json(json).expect_err("primitive children are unsupported");
        assert_eq!(err, DecorationBridgeError::UnsupportedPrimitiveChild);
    }

    #[test]
    fn decode_interaction_change_handlers() {
        let json = r##"
        {
          "kind": "Button",
          "nodeId": "root.Button[0]",
          "props": {
            "onHoverChange": {
              "kind": "runtime-state-handler",
              "trueId": "hover-true",
              "falseId": "hover-false"
            },
            "onActiveChange": {
              "kind": "runtime-state-handler",
              "trueId": "active-true",
              "falseId": "active-false"
            }
          },
          "children": []
        }
        "##;

        let tree = decode_tree_json(json).expect("json should decode");

        assert_eq!(tree.stable_id.as_deref(), Some("root.Button[0]"));
        assert_eq!(
            tree.interaction
                .hover_change
                .as_ref()
                .map(|handler| handler.handler_for(true)),
            Some("hover-true")
        );
        assert_eq!(
            tree.interaction
                .active_change
                .as_ref()
                .map(|handler| handler.handler_for(false)),
            Some("active-false")
        );
    }

    #[test]
    fn decode_image_node_props() {
        let json = r##"
        {
          "kind": "Image",
          "nodeId": "root.Image[0]",
          "props": {
            "src": "/tmp/icon.svg",
            "fit": "cover",
            "style": { "width": 12, "height": 8 }
          },
          "children": []
        }
        "##;

        let tree = decode_tree_json(json).expect("json should decode");

        assert_eq!(tree.stable_id.as_deref(), Some("root.Image[0]"));
        assert_eq!(tree.style.width, Some(12));
        assert_eq!(tree.style.height, Some(8));
        assert!(matches!(
            tree.kind,
            DecorationNodeKind::Image(ImageNode {
                src,
                fit: crate::ssd::ImageFit::Cover,
            }) if src == "/tmp/icon.svg"
        ));
    }

    #[test]
    fn decode_layer_effect_assignment_with_invalidation_and_outsets() {
        let wire: WireWindowEffectConfig = serde_json::from_str(
            r#"{
                "behind": {
                    "kind": "layer-effect",
                    "effect": {
                        "kind": "compiled-effect",
                        "input": { "kind": "layer-source", "include": "full" },
                        "capturePadding": 24,
                        "invalidate": {
                            "kind": "on-source-damage-box",
                            "damagePadding": 12
                        },
                        "pipeline": [{ "kind": "noise", "noiseKind": "salt", "amount": 0.1 }]
                    },
                    "outsets": { "left": 4, "right": 8, "top": 2, "bottom": 6 }
                }
            }"#,
        )
        .expect("layer effect assignment should deserialize");

        let effects: WindowEffectConfig = wire.try_into().expect("layer effect should decode");
        let behind = effects.behind.expect("behind effect should exist");
        assert!(matches!(
            behind.effect.input,
            EffectInput::LayerSource(WindowSourceInclude::Full)
        ));
        assert!(matches!(
            behind.effect.invalidate,
            EffectInvalidationPolicy::OnSourceDamageBox { damage_padding: 12 }
        ));
        assert_eq!(behind.effect.capture_padding, 24);
        assert_eq!(
            behind.outsets,
            EffectOutsets {
                left: 4,
                right: 8,
                top: 2,
                bottom: 6,
            }
        );
    }

    #[test]
    fn decode_shader_stage_named_texture_input() {
        let wire: WireCompiledEffect = serde_json::from_str(
            r#"{
                "kind": "compiled-effect",
                "input": { "kind": "backdrop-source" },
                "pipeline": [{
                    "kind": "shader-stage",
                    "shader": { "kind": "shader-module", "path": "/tmp/mask.frag" },
                    "textures": {
                        "layer_mask": { "kind": "layer-source", "include": "full" }
                    }
                }]
            }"#,
        )
        .expect("named texture effect should deserialize");

        let effect: CompiledEffect = wire.try_into().expect("named texture effect should decode");
        let EffectStage::Shader(stage) = &effect.pipeline[0] else {
            panic!("expected shader stage");
        };
        assert!(matches!(
            stage.textures.get("layer_mask"),
            Some(EffectInput::LayerSource(WindowSourceInclude::Full))
        ));
    }

    #[test]
    fn decode_shader_uniform_arrays() {
        let wire: WireCompiledEffect = serde_json::from_str(
            r#"{
                "kind": "compiled-effect",
                "input": { "kind": "backdrop-source" },
                "pipeline": [{
                    "kind": "shader-stage",
                    "shader": { "kind": "shader-module", "path": "/tmp/array.frag" },
                    "uniforms": {
                        "weights": {
                            "kind": "uniform-array",
                            "element": "float",
                            "values": [0.25, 0.75]
                        },
                        "points": {
                            "kind": "uniform-array",
                            "element": "vec2",
                            "values": [[1.0, 2.0], [3.0, 4.0]]
                        }
                    }
                }]
            }"#,
        )
        .expect("uniform arrays should deserialize");

        let effect: CompiledEffect = wire.try_into().expect("uniform arrays should decode");
        let EffectStage::Shader(stage) = &effect.pipeline[0] else {
            panic!("expected shader stage");
        };
        assert_eq!(
            stage.uniforms.get("weights"),
            Some(&ShaderUniformValue::FloatArray(vec![0.25, 0.75]))
        );
        assert_eq!(
            stage.uniforms.get("points"),
            Some(&ShaderUniformValue::Vec2Array(vec![[1.0, 2.0], [3.0, 4.0]]))
        );
    }

    #[test]
    fn decode_persistent_state_render_pass() {
        let wire: WireCompiledEffect = serde_json::from_str(
            r#"{
                "kind": "compiled-effect",
                "input": { "kind": "layer-source", "include": "full" },
                "pipeline": [{
                    "kind": "render-to",
                    "target": {
                        "kind": "state-texture",
                        "name": "velocity",
                        "scale": 0.5,
                        "format": "rg16f",
                        "resize": "clear"
                    },
                    "effect": {
                        "kind": "compiled-effect",
                        "input": {
                            "kind": "state-source",
                            "state": {
                                "kind": "state-texture",
                                "name": "velocity",
                                "scale": 0.5,
                                "format": "rg16f",
                                "resize": "clear"
                            }
                        },
                        "invalidate": { "kind": "always" },
                        "pipeline": [{
                            "kind": "shader-stage",
                            "shader": {
                                "kind": "shader-module",
                                "path": "/tmp/velocity.frag"
                            }
                        }]
                    }
                }]
            }"#,
        )
        .expect("persistent state effect should deserialize");

        let effect: CompiledEffect = wire.try_into().expect("state effect should decode");
        let EffectStage::RenderTo { target, effect } = &effect.pipeline[0] else {
            panic!("expected render-to stage");
        };
        assert_eq!(target.name, "velocity");
        assert_eq!(target.scale, 0.5);
        assert!(matches!(
            target.format,
            crate::ssd::EffectStateTextureFormat::Rg16f
        ));
        assert!(matches!(
            effect.input,
            EffectInput::State(crate::ssd::EffectStateTexture {
                ref name,
                scale: 0.5,
                ..
            }) if name == "velocity"
        ));
    }
}
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum WireOnClick {
    Action(WireWindowAction),
    RuntimeHandler(WireRuntimeHandler),
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct WireRuntimeHandler {
    pub kind: String,
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WireStateChangeHandler {
    pub kind: String,
    pub true_id: String,
    pub false_id: String,
}

impl TryFrom<WireStateChangeHandler> for DecorationStateChangeHandler {
    type Error = DecorationBridgeError;

    fn try_from(value: WireStateChangeHandler) -> Result<Self, Self::Error> {
        if value.kind != "runtime-state-handler" {
            return Err(DecorationBridgeError::UnsupportedNodeKind(value.kind));
        }

        Ok(Self {
            true_handler: value.true_id,
            false_handler: value.false_id,
        })
    }
}
