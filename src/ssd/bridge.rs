use serde::Deserialize;

use super::{
    AlignItems, BackdropBlur, BackgroundEffectConfig, BlendMode, BorderFit, BorderStyle, BoxNode,
    ButtonNode, Color, CompiledEffect, DecorationNode, DecorationNodeKind, DecorationStyle, Edges,
    EffectInput, EffectInvalidationPolicy, EffectStage, JustifyContent, LabelNode, LayoutDirection,
    NodeTransform, NoiseKind, NoiseStage, Overflow, PointerEvents, PositionOffsets,
    ShaderEffectNode, ShaderModule, ShaderStage, ShaderUniformValue, StylePosition, WindowAction,
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
    pub id: Option<String>,
    pub style: WireStyle,
    pub on_click: Option<WireOnClick>,
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
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum WireShaderUniformValue {
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
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WireCompiledEffect {
    pub kind: String,
    pub input: Option<WireEffectInput>,
    pub invalidate: Option<WireEffectInvalidationPolicy>,
    #[serde(default)]
    pub pipeline: Vec<WireEffectStage>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
pub enum WireEffectInvalidationPolicy {
    OnSourceDamageBox {
        anti_artifact_margin: i32,
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
    OnSourceDamageBox { anti_artifact_margin: i32 },
    Always,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum WireEffectInput {
    BackdropSource,
    XrayBackdropSource,
    ShaderInput(WireShaderStageFields),
    ImageSource { path: String },
    NamedTexture { name: String },
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

pub type WireBackgroundEffectConfig = WireCompiledEffect;

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
            "Fragment" => DecorationNodeKind::Box(BoxNode {
                direction: LayoutDirection::Column,
            }),
            other => {
                return Err(DecorationBridgeError::UnsupportedNodeKind(
                    other.to_string(),
                ));
            }
        };

        let style = DecorationStyle::try_from(value.props.style)?;
        let children = value
            .children
            .into_iter()
            .map(TryInto::try_into)
            .collect::<Result<Vec<_>, _>>()?;

        Ok(DecorationNode {
            stable_id: value.node_id,
            kind,
            style,
            children,
        })
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
                    let mut uniforms = std::collections::BTreeMap::new();
                    for (name, value) in stage.uniforms {
                        let value = match value {
                            WireShaderUniformValue::Float(value) => {
                                ShaderUniformValue::Float(value)
                            }
                            WireShaderUniformValue::Vec(value) => match value.as_slice() {
                                [x, y] => ShaderUniformValue::Vec2([*x, *y]),
                                [x, y, z] => ShaderUniformValue::Vec3([*x, *y, *z]),
                                [x, y, z, w] => ShaderUniformValue::Vec4([*x, *y, *z, *w]),
                                _ => return Err(DecorationBridgeError::InvalidShaderDescriptor),
                            },
                        };
                        uniforms.insert(name, value);
                    }
                    stages.push(EffectStage::Shader(ShaderStage {
                        shader: ShaderModule {
                            path: stage.shader.path,
                        },
                        uniforms,
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
            }
        }

        if stages.is_empty() && !matches!(input, EffectInput::Shader(_)) {
            return Err(DecorationBridgeError::InvalidShaderDescriptor);
        }

        let invalidate =
            match value
                .invalidate
                .unwrap_or(WireEffectInvalidationPolicy::OnSourceDamageBox {
                    anti_artifact_margin: 0,
                }) {
                WireEffectInvalidationPolicy::OnSourceDamageBox {
                    anti_artifact_margin,
                } => EffectInvalidationPolicy::OnSourceDamageBox {
                    anti_artifact_margin: anti_artifact_margin.max(0),
                },
                WireEffectInvalidationPolicy::Always => EffectInvalidationPolicy::Always,
                WireEffectInvalidationPolicy::Manual { dirty_when, base } => {
                    EffectInvalidationPolicy::Manual {
                        dirty_when,
                        base: base
                            .map(|policy| Box::new(decode_automatic_invalidation_policy(*policy))),
                    }
                }
            };

        Ok(CompiledEffect {
            input,
            invalidate,
            pipeline: stages,
        })
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

fn decode_effect_input(value: WireEffectInput) -> Result<EffectInput, DecorationBridgeError> {
    Ok(match value {
        WireEffectInput::BackdropSource => EffectInput::Backdrop,
        WireEffectInput::XrayBackdropSource => EffectInput::XrayBackdrop,
        WireEffectInput::ShaderInput(stage) => {
            if stage.shader.kind != "shader-module" || stage.shader.path.is_empty() {
                return Err(DecorationBridgeError::InvalidEffectInput);
            }
            let mut uniforms = std::collections::BTreeMap::new();
            for (name, value) in stage.uniforms {
                let value = match value {
                    WireShaderUniformValue::Float(value) => ShaderUniformValue::Float(value),
                    WireShaderUniformValue::Vec(value) => match value.as_slice() {
                        [x, y] => ShaderUniformValue::Vec2([*x, *y]),
                        [x, y, z] => ShaderUniformValue::Vec3([*x, *y, *z]),
                        [x, y, z, w] => ShaderUniformValue::Vec4([*x, *y, *z, *w]),
                        _ => return Err(DecorationBridgeError::InvalidEffectInput),
                    },
                };
                uniforms.insert(name, value);
            }
            EffectInput::Shader(ShaderStage {
                shader: ShaderModule {
                    path: stage.shader.path,
                },
                uniforms,
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
    })
}

fn decode_automatic_invalidation_policy(
    value: WireAutomaticEffectInvalidationPolicy,
) -> EffectInvalidationPolicy {
    match value {
        WireAutomaticEffectInvalidationPolicy::OnSourceDamageBox {
            anti_artifact_margin,
        } => EffectInvalidationPolicy::OnSourceDamageBox {
            anti_artifact_margin: anti_artifact_margin.max(0),
        },
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
