use serde::Deserialize;

use super::{
    AlignItems, BorderStyle, BoxNode, ButtonNode, Color, DecorationNode, DecorationNodeKind,
    DecorationStyle, Edges, JustifyContent, LayoutDirection, LabelNode, WindowAction,
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
    pub id: Option<String>,
    pub style: WireStyle,
    pub on_click: Option<WireWindowAction>,
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
    pub border_radius: Option<i32>,
    pub visible: Option<bool>,
    pub cursor: Option<String>,
    pub font_size: Option<i32>,
    pub font_weight: Option<serde_json::Value>,
    pub font_family: Option<WireFontFamily>,
    pub text_align: Option<String>,
    pub line_height: Option<i32>,
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
    #[error("unsupported dimension keyword: {0}")]
    UnsupportedDimensionKeyword(String),
    #[error("invalid direction: {0}")]
    InvalidDirection(String),
    #[error("invalid alignItems value: {0}")]
    InvalidAlignItems(String),
    #[error("invalid justifyContent value: {0}")]
    InvalidJustifyContent(String),
    #[error("invalid color string: {0}")]
    InvalidColor(String),
}

pub fn decode_tree_json(input: &str) -> Result<DecorationNode, DecorationBridgeError> {
    let wire: WireDecorationNode =
        serde_json::from_str(input).map_err(|err| DecorationBridgeError::InvalidJson(err.to_string()))?;
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
                action: value.props.on_click.unwrap_or(WireWindowAction::Close).into(),
            }),
            "AppIcon" => DecorationNodeKind::AppIcon,
            "Window" => DecorationNodeKind::WindowSlot,
            "WindowBorder" => DecorationNodeKind::WindowBorder,
            "Fragment" => DecorationNodeKind::Box(BoxNode {
                direction: LayoutDirection::Column,
            }),
            other => return Err(DecorationBridgeError::UnsupportedNodeKind(other.to_string())),
        };

        let style = DecorationStyle::try_from(value.props.style)?;
        let children = value
            .children
            .into_iter()
            .map(TryInto::try_into)
            .collect::<Result<Vec<_>, _>>()?;

        Ok(DecorationNode {
            kind,
            style,
            children,
        })
    }
}

impl TryFrom<WireDecorationChild> for DecorationNode {
    type Error = DecorationBridgeError;

    fn try_from(value: WireDecorationChild) -> Result<Self, Self::Error> {
        match value {
            WireDecorationChild::Node(node) => node.try_into(),
            WireDecorationChild::Primitive(_) => Err(DecorationBridgeError::UnsupportedPrimitiveChild),
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
            gap: value.gap,
            justify_content: value
                .justify_content
                .map(parse_justify_content)
                .transpose()?,
            align_items: value.align_items.map(parse_align_items).transpose()?,
            background: value.background.map(|s| parse_color(&s)).transpose()?,
            color: value.color.map(|s| parse_color(&s)).transpose()?,
            opacity: value.opacity,
            border: value.border.map(|border| parse_border(border)).transpose()?,
            border_top: value.border_top.map(|border| parse_border(border)).transpose()?,
            border_right: value.border_right.map(|border| parse_border(border)).transpose()?,
            border_bottom: value.border_bottom.map(|border| parse_border(border)).transpose()?,
            border_left: value.border_left.map(|border| parse_border(border)).transpose()?,
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
        other => Err(DecorationBridgeError::InvalidJustifyContent(other.to_string())),
    }
}

fn parse_dimension(input: Option<WireDimension>) -> Result<Option<i32>, DecorationBridgeError> {
    match input {
        Some(WireDimension::Pixels(value)) => Ok(Some(value)),
        Some(WireDimension::Keyword(keyword)) => Err(DecorationBridgeError::UnsupportedDimensionKeyword(keyword)),
        None => Ok(None),
    }
}

fn parse_border(input: WireBorderValue) -> Result<BorderStyle, DecorationBridgeError> {
    Ok(BorderStyle {
        width: input.px,
        color: parse_color(&input.color)?,
    })
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
