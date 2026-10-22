//! Node styles: the Rust counterpart of the TypeScript `SSDStyle` object.
//!
//! Every field accepts a plain value or anything reactive (`Signal`, `Memo`,
//! [`derive`](crate::reactive::derive)), so one style can mix fixed and
//! changing properties:
//!
//! ```
//! use shojiwm_rs::prelude::*;
//!
//! let scope = Scope::root();
//! let hover = scope.signal(false);
//! let style = Style::new()
//!     .size(16.0, 16.0)
//!     .border_radius(8.0)
//!     .background(hex("#FFFFFF20"))
//!     .border(1.0, hover.map(|hover| if *hover { hex("#00000000") } else { hex("#F0808030") }));
//! # let _ = style;
//! ```

use shojiwm_lib::ssd::{
    AlignItems, BorderFit, BorderStyle, BoxShadow, Color, DecorationStyle, Edges, JustifyContent,
    NodeTransform, Overflow, PointerEvents, PositionOffsets, StylePosition,
};

use crate::reactive::Prop;

/// Parse `#RRGGBB` or `#RRGGBBAA` (or the short `#RGB` / `#RGBA`). Usable in `const` items, where a typo is a
/// compile error; at runtime an invalid string panics.
pub const fn hex(input: &str) -> Color {
    const fn digit(byte: u8) -> u8 {
        match byte {
            b'0'..=b'9' => byte - b'0',
            b'a'..=b'f' => byte - b'a' + 10,
            b'A'..=b'F' => byte - b'A' + 10,
            _ => panic!("invalid hex digit in color"),
        }
    }
    const fn pair(bytes: &[u8], at: usize) -> u8 {
        digit(bytes[at]) * 16 + digit(bytes[at + 1])
    }
    let bytes = input.as_bytes();
    assert!(
        !bytes.is_empty() && bytes[0] == b'#',
        "color must start with '#'"
    );
    match bytes.len() {
        4 => Color::rgba(
            digit(bytes[1]) * 17,
            digit(bytes[2]) * 17,
            digit(bytes[3]) * 17,
            255,
        ),
        5 => Color::rgba(
            digit(bytes[1]) * 17,
            digit(bytes[2]) * 17,
            digit(bytes[3]) * 17,
            digit(bytes[4]) * 17,
        ),
        7 => Color::rgba(pair(bytes, 1), pair(bytes, 3), pair(bytes, 5), 255),
        9 => Color::rgba(
            pair(bytes, 1),
            pair(bytes, 3),
            pair(bytes, 5),
            pair(bytes, 7),
        ),
        _ => panic!("color must be #RGB, #RGBA, #RRGGBB or #RRGGBBAA"),
    }
}

/// `Color` from 0-255 channels.
pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Color {
    Color::rgba(r, g, b, a)
}

/// A border: width in logical pixels and color.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Border {
    pub width: f64,
    pub color: Color,
}

impl Border {
    pub fn new(width: f64, color: Color) -> Self {
        Self { width, color }
    }
}

impl From<Border> for BorderStyle {
    fn from(border: Border) -> Self {
        BorderStyle {
            width: border.width,
            color: border.color,
        }
    }
}

/// A CSS `box-shadow` layer drawn around the node (`x`, `y`, `blur` in
/// logical pixels). Adjust `spread` with struct update syntax:
/// `BoxShadow { spread: 2.0, ..shadow(0.0, 4.0, 12.0, hex("#00000060")) }`.
pub const fn shadow(x: f64, y: f64, blur: f64, color: Color) -> BoxShadow {
    BoxShadow {
        offset_x: x,
        offset_y: y,
        blur,
        spread: 0.0,
        color,
        inset: false,
    }
}

/// Like [`shadow`], drawn inside the padding box.
pub const fn inset_shadow(x: f64, y: f64, blur: f64, color: Color) -> BoxShadow {
    BoxShadow {
        inset: true,
        ..shadow(x, y, blur, color)
    }
}

/// Font weight, as a CSS number (`600`) or keyword (`"bold"`).
#[derive(Debug, Clone, PartialEq)]
pub enum FontWeight {
    Number(u32),
    Keyword(String),
}

impl From<u32> for FontWeight {
    fn from(value: u32) -> Self {
        Self::Number(value)
    }
}

impl From<&str> for FontWeight {
    fn from(value: &str) -> Self {
        Self::Keyword(value.to_owned())
    }
}

/// Per-node transform (`style.transform`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Transform2D {
    pub translate_x: f32,
    pub translate_y: f32,
    pub scale_x: f32,
    pub scale_y: f32,
}

impl Default for Transform2D {
    fn default() -> Self {
        Self {
            translate_x: 0.0,
            translate_y: 0.0,
            scale_x: 1.0,
            scale_y: 1.0,
        }
    }
}

macro_rules! style_fields {
    ($($(#[$doc:meta])* $field:ident: $ty:ty),* $(,)?) => {
        /// A reactive style. Unset fields keep the compositor defaults.
        #[derive(Clone, Default)]
        pub struct Style {
            $($field: Option<Prop<$ty>>,)*
        }

        impl Style {
            $(
                $(#[$doc])*
                pub fn $field(mut self, value: impl Into<Prop<$ty>>) -> Self {
                    self.$field = Some(value.into());
                    self
                }
            )*

            /// Fields set in `other` override the ones in `self`.
            pub fn merge(mut self, other: &Style) -> Self {
                $(
                    if let Some(value) = &other.$field {
                        self.$field = Some(value.clone());
                    }
                )*
                self
            }

            /// Whether every field is a fixed value.
            pub fn is_static(&self) -> bool {
                true $(&& self.$field.as_ref().is_none_or(Prop::is_static))*
            }
        }
    };
}

style_fields! {
    width: f64,
    height: f64,
    min_width: f64,
    min_height: f64,
    max_width: f64,
    max_height: f64,
    flex_grow: f32,
    flex_shrink: f32,
    gap: f64,
    padding: f64,
    padding_x: f64,
    padding_y: f64,
    padding_top: f64,
    padding_right: f64,
    padding_bottom: f64,
    padding_left: f64,
    margin: f64,
    margin_x: f64,
    margin_y: f64,
    margin_top: f64,
    margin_right: f64,
    margin_bottom: f64,
    margin_left: f64,
    position: StylePosition,
    z_index: i32,
    inset: f64,
    top: f64,
    right: f64,
    bottom: f64,
    left: f64,
    overflow: Overflow,
    pointer_events: PointerEvents,
    transform: Transform2D,
    justify_content: JustifyContent,
    align_items: AlignItems,
    background: Color,
    color: Color,
    opacity: f32,
    /// Border on all four sides.
    border_all: Border,
    border_top: Border,
    border_right: Border,
    border_bottom: Border,
    border_left: Border,
    border_fit: BorderFit,
    border_radius: f64,
    /// CSS-like shadows; the first entry is painted on top.
    box_shadow: Vec<BoxShadow>,
    visible: bool,
    cursor: String,
    font_size: f64,
    font_weight: FontWeight,
    font_family: Vec<String>,
    text_align: String,
    line_height: f64,
}

impl Style {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn size(self, width: impl Into<Prop<f64>>, height: impl Into<Prop<f64>>) -> Self {
        self.width(width).height(height)
    }

    /// `border: { px, color }`; `color` may be reactive on its own.
    pub fn border(mut self, width: f64, color: impl Into<Prop<Color>>) -> Self {
        let color = color.into();
        self.border_all = Some(color.map(move |color| Border::new(width, color)));
        self
    }

    pub fn relative(self) -> Self {
        self.position(StylePosition::Relative)
    }

    pub fn absolute(self) -> Self {
        self.position(StylePosition::Absolute)
    }

    pub fn pointer_events_none(self) -> Self {
        self.pointer_events(PointerEvents::None)
    }

    pub fn fonts<S: Into<String>>(self, families: impl IntoIterator<Item = S>) -> Self {
        self.font_family(families.into_iter().map(Into::into).collect::<Vec<_>>())
    }

    /// Read every field (tracked) into the compositor's style.
    pub(crate) fn resolve(&self) -> DecorationStyle {
        fn read<T: Clone>(prop: &Option<Prop<T>>) -> Option<T> {
            prop.as_ref().map(Prop::get)
        }
        let edges = |all: &Option<Prop<f64>>,
                     x: &Option<Prop<f64>>,
                     y: &Option<Prop<f64>>,
                     top: &Option<Prop<f64>>,
                     right: &Option<Prop<f64>>,
                     bottom: &Option<Prop<f64>>,
                     left: &Option<Prop<f64>>| {
            let base = read(all).unwrap_or(0.0);
            let horizontal = read(x).unwrap_or(base);
            let vertical = read(y).unwrap_or(base);
            Edges {
                top: read(top).unwrap_or(vertical),
                right: read(right).unwrap_or(horizontal),
                bottom: read(bottom).unwrap_or(vertical),
                left: read(left).unwrap_or(horizontal),
            }
        };
        let inset = read(&self.inset);
        DecorationStyle {
            width: read(&self.width),
            height: read(&self.height),
            min_width: read(&self.min_width),
            min_height: read(&self.min_height),
            max_width: read(&self.max_width),
            max_height: read(&self.max_height),
            flex_grow: read(&self.flex_grow),
            flex_shrink: read(&self.flex_shrink),
            padding: edges(
                &self.padding,
                &self.padding_x,
                &self.padding_y,
                &self.padding_top,
                &self.padding_right,
                &self.padding_bottom,
                &self.padding_left,
            ),
            margin: edges(
                &self.margin,
                &self.margin_x,
                &self.margin_y,
                &self.margin_top,
                &self.margin_right,
                &self.margin_bottom,
                &self.margin_left,
            ),
            gap: read(&self.gap),
            position: read(&self.position),
            z_index: read(&self.z_index),
            inset: PositionOffsets {
                top: read(&self.top).or(inset),
                right: read(&self.right).or(inset),
                bottom: read(&self.bottom).or(inset),
                left: read(&self.left).or(inset),
            },
            overflow: read(&self.overflow),
            pointer_events: read(&self.pointer_events),
            transform: read(&self.transform).map(|transform| NodeTransform {
                translate_x: transform.translate_x,
                translate_y: transform.translate_y,
                scale_x: transform.scale_x,
                scale_y: transform.scale_y,
            }),
            justify_content: read(&self.justify_content),
            align_items: read(&self.align_items),
            background: read(&self.background),
            color: read(&self.color),
            opacity: read(&self.opacity),
            border: read(&self.border_all).map(Into::into),
            border_top: read(&self.border_top).map(Into::into),
            border_right: read(&self.border_right).map(Into::into),
            border_bottom: read(&self.border_bottom).map(Into::into),
            border_left: read(&self.border_left).map(Into::into),
            border_fit: read(&self.border_fit),
            border_radius: read(&self.border_radius),
            visible: read(&self.visible),
            cursor: read(&self.cursor),
            font_size: read(&self.font_size),
            font_weight: read(&self.font_weight).map(|weight| match weight {
                FontWeight::Number(value) => serde_json::Value::from(value),
                FontWeight::Keyword(value) => serde_json::Value::from(value),
            }),
            font_family: read(&self.font_family),
            text_align: read(&self.text_align),
            text_overflow: None,
            line_height: read(&self.line_height),
            box_shadow: read(&self.box_shadow).unwrap_or_default(),
            paint: None,
            overlay: None,
        }
    }
}

impl From<&str> for Prop<Color> {
    fn from(value: &str) -> Self {
        Prop::Static(hex(value))
    }
}

impl From<BoxShadow> for Prop<Vec<BoxShadow>> {
    fn from(value: BoxShadow) -> Self {
        Prop::Static(vec![value])
    }
}

impl<const N: usize> From<[BoxShadow; N]> for Prop<Vec<BoxShadow>> {
    fn from(value: [BoxShadow; N]) -> Self {
        Prop::Static(value.to_vec())
    }
}

impl From<u32> for Prop<FontWeight> {
    fn from(value: u32) -> Self {
        Prop::Static(FontWeight::Number(value))
    }
}

impl<const N: usize> From<[&str; N]> for Prop<Vec<String>> {
    fn from(value: [&str; N]) -> Self {
        Prop::Static(value.iter().map(|value| (*value).to_owned()).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reactive::Scope;

    #[test]
    fn hex_parses_both_lengths() {
        assert_eq!(hex("#d7ba7d"), Color::rgba(0xd7, 0xba, 0x7d, 0xff));
        assert_eq!(hex("#1f243080"), Color::rgba(0x1f, 0x24, 0x30, 0x80));
        const FOCUSED: Color = hex("#4f5666");
        assert_eq!(FOCUSED.a, 255);
    }

    #[test]
    fn edges_follow_the_typescript_precedence() {
        let style = Style::new()
            .padding(1.0)
            .padding_x(2.0)
            .padding_right(3.0)
            .resolve();
        assert_eq!(
            style.padding,
            Edges {
                top: 1.0,
                right: 3.0,
                bottom: 1.0,
                left: 2.0,
            }
        );
    }

    #[test]
    fn reactive_border_color() {
        let scope = Scope::root();
        let hover = scope.signal(false);
        let style = Style::new().border(
            1.0,
            hover.map(|hover| {
                if *hover {
                    hex("#000000")
                } else {
                    hex("#ffffff")
                }
            }),
        );
        assert_eq!(style.resolve().border.unwrap().color, hex("#ffffff"));
        hover.set(true);
        assert_eq!(style.resolve().border.unwrap().color, hex("#000000"));
        assert!(!style.is_static());
        scope.dispose();
    }
}
