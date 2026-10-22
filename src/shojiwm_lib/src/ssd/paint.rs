//! What a decoration node paints by itself — shadows, background, border and
//! user paint shaders — resolved to whole physical pixels.
//!
//! Every item carries a [`PaintGeometry`] taken straight from the layout's
//! root-local physical pixel grid (see `LayoutFrame`). The renderer only
//! translates it to the output; nothing is re-snapped, so a custom paint
//! shader sees exactly the pixels the layout produced.
//!
//! Paint order inside one node, back to front:
//!
//! 1. outer `boxShadow`s (last entry at the back)
//! 2. built-in fill (only below a `<ShaderEffect>` pipeline)
//! 3. the `<ShaderEffect>` pipeline output
//! 4. inset `boxShadow`s (inside a `<ShaderEffect>`: above the effect)
//! 5. `paint` — the user shader, or the built-in background + border
//! 6. label / icon content, then the children
//! 7. `overlay`
//!
//! For an ordinary node, 4 and 5 swap: the inset shadows sit above the
//! background, clipped to the padding box so they never cover the border.

use std::collections::BTreeMap;

use super::{
    BorderFit, BoxShadow, Color, ComputedDecorationNode, DecorationNodeKind, LayoutFrame,
    PaintShader, ResolvedDecorationClip, ResolvedLayoutEdges, ResolvedLayoutValue,
    ResolvedLogicalRect, ShaderModule, ShaderUniformValue, round_half_up,
};

/// A rect in root-local physical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PxRect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl PxRect {
    pub const fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self { x, y, w, h }
    }

    pub(crate) fn from_resolved(rect: ResolvedLogicalRect) -> Self {
        Self::new(
            rect.x.raw(),
            rect.y.raw(),
            rect.width.raw().max(0),
            rect.height.raw().max(0),
        )
    }

    pub(crate) fn to_resolved(self) -> ResolvedLogicalRect {
        ResolvedLogicalRect::from_px(self.x, self.y, self.w, self.h)
    }

    pub fn right(self) -> i32 {
        self.x + self.w
    }

    pub fn bottom(self) -> i32 {
        self.y + self.h
    }

    pub fn is_empty(self) -> bool {
        self.w <= 0 || self.h <= 0
    }

    /// Grows each side by `[top, right, bottom, left]` pixels.
    pub fn outset(self, [top, right, bottom, left]: [i32; 4]) -> Self {
        Self::new(
            self.x - left,
            self.y - top,
            (self.w + left + right).max(0),
            (self.h + top + bottom).max(0),
        )
    }

    pub fn union(self, other: Self) -> Self {
        if self.is_empty() {
            return other;
        }
        if other.is_empty() {
            return self;
        }
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        Self::new(
            x,
            y,
            self.right().max(other.right()) - x,
            self.bottom().max(other.bottom()) - y,
        )
    }

    pub fn translated(self, dx: i32, dy: i32) -> Self {
        Self::new(self.x + dx, self.y + dy, self.w, self.h)
    }
}

/// A rounded clip: `radius` is `[top-left, top-right, bottom-right, bottom-left]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PxClip {
    pub rect: PxRect,
    pub radius: [i32; 4],
}

impl PxClip {
    fn from_resolved(clip: ResolvedDecorationClip) -> Self {
        let rect = PxRect::from_resolved(clip.rect);
        Self {
            rect,
            radius: [clamp_radius(clip.radius.raw(), rect); 4],
        }
    }

    fn is_rounded(self) -> bool {
        self.radius.iter().any(|radius| *radius > 0)
    }
}

/// Node geometry handed to a paint program, in root-local physical pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PaintGeometry {
    /// The layout scale the pixels were resolved at.
    pub scale: f64,
    /// The node's border box.
    pub node: PxRect,
    /// The area the program draws: the node plus outsets / shadow extents.
    pub draw: PxRect,
    /// Border widths `[top, right, bottom, left]`.
    pub border: [i32; 4],
    /// Outer corner radii `[top-left, top-right, bottom-right, bottom-left]`.
    pub radius: [i32; 4],
    /// Padding `[top, right, bottom, left]`.
    pub padding: [i32; 4],
    /// The content box (inside border and padding).
    pub content: PxRect,
    /// The inner edge of the border.
    pub inner: PxRect,
    pub inner_radius: [i32; 4],
    /// The area inside `inner` belongs to the window's client surface and is
    /// not filled (`<WindowBorder>`).
    pub hole: bool,
    /// The clip inherited from the ancestors.
    pub clip: Option<PxClip>,
    /// The nearest rounded ancestor clip, kept separately so that a
    /// rectangular clip deeper in the tree does not lose the rounding.
    pub rounded_clip: Option<PxClip>,
}

/// A resolved `box-shadow` layer, in physical pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ShadowPx {
    pub offset: [i32; 2],
    /// Gaussian sigma (half the CSS blur radius).
    pub sigma: f32,
    pub spread: i32,
    pub color: Color,
    pub inset: bool,
}

impl ShadowPx {
    /// How far the shadow reaches past the (spread) shape.
    pub fn blur_extent(self) -> i32 {
        (self.sigma * 3.0).ceil() as i32
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum PaintProgram {
    /// The built-in background and/or border.
    Box { fill: bool, border: bool },
    /// A built-in `box-shadow` layer.
    Shadow(ShadowPx),
    /// A user `paint` / `overlay` shader.
    Custom(ShaderModule),
}

#[derive(Debug, Clone, PartialEq)]
pub struct PaintItem {
    pub program: PaintProgram,
    pub geometry: PaintGeometry,
    pub background: Color,
    /// `[top, right, bottom, left]`.
    pub border_colors: [Color; 4],
    pub opacity: f32,
    pub uniforms: BTreeMap<String, ShaderUniformValue>,
}

impl PaintItem {
    pub fn is_custom(&self) -> bool {
        matches!(self.program, PaintProgram::Custom(_))
    }

    /// A rect, relative to `geometry.draw`, where a built-in program is known
    /// to output nothing: the box an outer shadow sits under, or the hollow
    /// middle of a border without a fill. Drawing skips it, so a window's
    /// shadow and frame cost only their rim instead of the whole window.
    pub fn transparent_interior(&self) -> Option<PxRect> {
        let g = &self.geometry;
        let (rect, radius) = match self.program {
            PaintProgram::Shadow(shadow) if !shadow.inset => (g.node, g.radius),
            PaintProgram::Box { fill, .. } if !fill || g.hole || self.background.a == 0 => {
                (g.inner, g.inner_radius)
            }
            _ => return None,
        };
        // A corner arc stays outside the square cut `r * (1 - 1/sqrt 2)` in
        // from its corner; one more pixel covers the antialiased edge.
        let max_radius = radius.into_iter().max().unwrap_or(0).max(0) as f64;
        let inset = (max_radius * (1.0 - std::f64::consts::FRAC_1_SQRT_2)).ceil() as i32 + 1;
        let interior = PxRect {
            x: rect.x - g.draw.x + inset,
            y: rect.y - g.draw.y + inset,
            w: rect.w - 2 * inset,
            h: rect.h - 2 * inset,
        };
        (interior.w > 0 && interior.h > 0).then_some(interior)
    }
}

/// The paint slots of one node, as stable-key suffixes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PaintSlot {
    OuterShadow(usize),
    Fill,
    InsetShadow(usize),
    /// The built-in border above a `<ShaderEffect>`.
    Border,
    /// Built-in background + border of an ordinary node.
    Box,
    /// The user `paint` shader.
    Paint,
    Overlay,
}

impl PaintSlot {
    pub(crate) fn key(self) -> String {
        match self {
            Self::OuterShadow(index) => format!("shadow-{index}"),
            Self::Fill => "fill".into(),
            Self::InsetShadow(index) => format!("inset-shadow-{index}"),
            Self::Border => "border".into(),
            Self::Box => "box".into(),
            Self::Paint => "paint".into(),
            Self::Overlay => "overlay".into(),
        }
    }

    pub(crate) fn source_kind(self) -> &'static str {
        match self {
            Self::OuterShadow(_) | Self::InsetShadow(_) => "shadow",
            Self::Fill => "fill",
            Self::Border => "border",
            Self::Box => "box",
            Self::Paint => "paint",
            Self::Overlay => "overlay",
        }
    }
}

fn effective_background(node: &ComputedDecorationNode) -> Option<Color> {
    node.style.background.filter(|color| color.a > 0)
}

fn visible_border(node: &ComputedDecorationNode) -> bool {
    node.style
        .border_sides()
        .iter()
        .any(|side| side.is_some_and(|border| border.width > 0.0 && border.color.a > 0))
}

fn paints_anything(node: &ComputedDecorationNode) -> bool {
    !matches!(node.kind, DecorationNodeKind::WindowSlot)
        && node.style.visible != Some(false)
        && node.style.opacity.is_none_or(|opacity| opacity > 0.0)
}

/// The slots a node paints *behind* its own content and children, front to
/// back.
pub(crate) fn back_slots(node: &ComputedDecorationNode) -> Vec<PaintSlot> {
    let mut slots = Vec::new();
    if !paints_anything(node) {
        return slots;
    }
    let is_effect = matches!(node.kind, DecorationNodeKind::ShaderEffect(_));
    let custom = node.style.paint.is_some();
    let builtin_box = !custom && (effective_background(node).is_some() || visible_border(node));
    let inset = node
        .style
        .box_shadow
        .iter()
        .enumerate()
        .filter(|(_, shadow)| shadow.inset && shadow.color.a > 0)
        .map(|(index, _)| PaintSlot::InsetShadow(index));
    let outer = node
        .style
        .box_shadow
        .iter()
        .enumerate()
        .filter(|(_, shadow)| !shadow.inset && shadow.color.a > 0)
        .map(|(index, _)| PaintSlot::OuterShadow(index));

    if is_effect {
        if custom {
            slots.push(PaintSlot::Paint);
        } else if visible_border(node) {
            slots.push(PaintSlot::Border);
        }
        slots.extend(inset);
        // The `<ShaderEffect>` pipeline itself sits here (`:shader`).
        if !custom && effective_background(node).is_some() {
            slots.push(PaintSlot::Fill);
        }
    } else {
        slots.extend(inset);
        if custom {
            slots.push(PaintSlot::Paint);
        } else if builtin_box {
            slots.push(PaintSlot::Box);
        }
    }
    slots.extend(outer);
    slots
}

pub(crate) fn has_overlay(node: &ComputedDecorationNode) -> bool {
    paints_anything(node) && node.style.overlay.is_some()
}

/// Whether the `:shader` effect slot sits between `Fill` and the inset
/// shadows of `back_slots` (as opposed to being absent).
pub(crate) fn effect_slot_index(node: &ComputedDecorationNode, slots: &[PaintSlot]) -> usize {
    debug_assert!(matches!(node.kind, DecorationNodeKind::ShaderEffect(_)));
    slots
        .iter()
        .position(|slot| matches!(slot, PaintSlot::Fill | PaintSlot::OuterShadow(_)))
        .unwrap_or(slots.len())
}

fn clamp_radius(radius: i32, rect: PxRect) -> i32 {
    radius.clamp(0, (rect.w.min(rect.h) / 2).max(0))
}

fn scale_len(value: ResolvedLayoutValue, factor: f64) -> i32 {
    if factor == 1.0 {
        return value.raw();
    }
    let scaled = round_half_up(value.raw() as f64 * factor);
    // A non-zero width stays visible.
    if value.raw() > 0 {
        scaled.max(1)
    } else {
        scaled.max(0)
    }
}

fn edges_array(edges: ResolvedLayoutEdges, (sx, sy): (f64, f64)) -> [i32; 4] {
    [
        scale_len(edges.top, sy),
        scale_len(edges.right, sx),
        scale_len(edges.bottom, sy),
        scale_len(edges.left, sx),
    ]
}

fn logical_px(value: f64, scale: f64) -> i32 {
    if value.is_finite() {
        round_half_up(value * scale)
    } else {
        0
    }
}

/// Clip state flowing down the tree while paint items are collected.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct PaintClipState {
    pub clip: Option<ResolvedDecorationClip>,
    pub nearest_rounded: Option<ResolvedDecorationClip>,
}

impl PaintClipState {
    /// The clip state the children of `node` inherit.
    pub(crate) fn for_children(self, node: &ComputedDecorationNode) -> Self {
        let child_clip = children_clip(node).or(self.clip);
        let nearest_rounded = match child_clip {
            Some(clip) if clip.radius.raw() > 0 => Some(clip),
            _ => self.nearest_rounded,
        };
        Self {
            clip: child_clip,
            nearest_rounded,
        }
    }
}

/// The clip a node imposes on its descendants (already intersected with the
/// inherited one by the layout).
pub(crate) fn children_clip(node: &ComputedDecorationNode) -> Option<ResolvedDecorationClip> {
    if matches!(
        node.style.effective_border_fit(&node.kind),
        BorderFit::FitChildren
    ) && node.style.has_border()
    {
        return Some(
            node.resolved_effective_clip
                .unwrap_or(ResolvedDecorationClip {
                    rect: node.resolved_content_rect,
                    radius: (node.resolved_border_radius - node.resolved_border_width)
                        .max(ResolvedLayoutValue::ZERO),
                }),
        );
    }
    node.resolved_effective_clip
}

/// The geometry shared by every paint item of `node`.
pub(crate) fn node_geometry(node: &ComputedDecorationNode, clips: PaintClipState) -> PaintGeometry {
    let scale = node.frame.scale;
    let factor = node.transform_scale;
    let rect = PxRect::from_resolved(node.resolved_rect);
    let border = edges_array(node.style.resolved_border_edges(scale), factor);
    let padding = edges_array(node.style.resolved_padding(scale), factor);
    let radius = clamp_radius(
        scale_len(node.resolved_border_radius, factor.0.min(factor.1)),
        rect,
    );
    let [top, right, bottom, left] = border;

    let fit_children = matches!(
        node.style.effective_border_fit(&node.kind),
        BorderFit::FitChildren
    ) && node.style.has_border();
    let (inner, inner_radius) = if fit_children {
        let clip = children_clip(node).expect("a fit-children border always clips");
        let inner = PxRect::from_resolved(clip.rect);
        (
            inner,
            [clamp_radius(scale_len(clip.radius, factor.0.min(factor.1)), inner); 4],
        )
    } else {
        let inner = rect.outset([-top, -right, -bottom, -left]);
        let corner = |a: i32, b: i32| clamp_radius((radius - a.max(b)).max(0), inner);
        (
            inner,
            [
                corner(top, left),
                corner(top, right),
                corner(bottom, right),
                corner(bottom, left),
            ],
        )
    };

    let clip = clips.clip.map(PxClip::from_resolved);
    let rounded_clip = clips
        .nearest_rounded
        .map(PxClip::from_resolved)
        .filter(|rounded| rounded.is_rounded() && Some(*rounded) != clip);

    PaintGeometry {
        scale,
        node: rect,
        draw: rect,
        border,
        radius: [radius; 4],
        padding,
        content: PxRect::from_resolved(node.resolved_content_rect),
        inner,
        inner_radius,
        hole: matches!(node.kind, DecorationNodeKind::WindowBorder) && node.style.has_border(),
        clip,
        rounded_clip,
    }
}

fn resolve_shadow(shadow: &BoxShadow, scale: f64, factor: (f64, f64)) -> ShadowPx {
    let uniform = factor.0.min(factor.1);
    ShadowPx {
        offset: [
            logical_px(shadow.offset_x * factor.0, scale),
            logical_px(shadow.offset_y * factor.1, scale),
        ],
        sigma: (shadow.blur.max(0.0) * scale * uniform * 0.5) as f32,
        spread: logical_px(shadow.spread * uniform, scale),
        color: shadow.color,
        inset: shadow.inset,
    }
}

fn outsets_px(shader: &PaintShader, scale: f64) -> [i32; 4] {
    [
        logical_px(shader.outsets.top, scale).max(0),
        logical_px(shader.outsets.right, scale).max(0),
        logical_px(shader.outsets.bottom, scale).max(0),
        logical_px(shader.outsets.left, scale).max(0),
    ]
}

/// Builds the item of one paint slot. `None` when the slot draws nothing.
pub(crate) fn paint_item(
    node: &ComputedDecorationNode,
    slot: PaintSlot,
    geometry: PaintGeometry,
) -> Option<PaintItem> {
    let scale = node.frame.scale;
    let background = effective_background(node).unwrap_or(Color::TRANSPARENT);
    let border_colors = node.style.border_sides().map(|side| {
        side.filter(|border| border.width > 0.0)
            .map(|border| border.color)
            .unwrap_or(Color::TRANSPARENT)
    });
    let opacity = node.style.opacity.unwrap_or(1.0).clamp(0.0, 1.0);
    let item = |program, geometry, uniforms| PaintItem {
        program,
        geometry,
        background,
        border_colors,
        opacity,
        uniforms,
    };

    match slot {
        PaintSlot::Box => Some(item(
            PaintProgram::Box {
                fill: background.a > 0,
                border: visible_border(node),
            },
            geometry,
            BTreeMap::new(),
        )),
        PaintSlot::Fill => Some(item(
            PaintProgram::Box {
                fill: true,
                border: false,
            },
            geometry,
            BTreeMap::new(),
        )),
        PaintSlot::Border => Some(item(
            PaintProgram::Box {
                fill: false,
                border: true,
            },
            geometry,
            BTreeMap::new(),
        )),
        PaintSlot::Paint | PaintSlot::Overlay => {
            let shader = if slot == PaintSlot::Paint {
                node.style.paint.as_ref()?
            } else {
                node.style.overlay.as_ref()?
            };
            let draw = geometry.node.outset(outsets_px(shader, scale));
            Some(item(
                PaintProgram::Custom(shader.shader.clone()),
                PaintGeometry { draw, ..geometry },
                shader.uniforms.clone(),
            ))
        }
        PaintSlot::OuterShadow(index) | PaintSlot::InsetShadow(index) => {
            let shadow = resolve_shadow(
                node.style.box_shadow.get(index)?,
                scale,
                node.transform_scale,
            );
            let draw = if shadow.inset {
                geometry.node
            } else {
                let reach = shadow.spread.max(0) + shadow.blur_extent();
                geometry
                    .node
                    .translated(shadow.offset[0], shadow.offset[1])
                    .outset([reach; 4])
                    .union(geometry.node)
            };
            Some(item(
                PaintProgram::Shadow(shadow),
                PaintGeometry { draw, ..geometry },
                BTreeMap::new(),
            ))
        }
    }
}

/// The logical bounds of an item, for culling and damage.
pub(crate) fn item_logical_rect(frame: LayoutFrame, item: &PaintItem) -> super::LogicalRect {
    let mut draw = item.geometry.draw;
    if let Some(clip) = item.geometry.clip {
        let x = draw.x.max(clip.rect.x);
        let y = draw.y.max(clip.rect.y);
        let right = draw.right().min(clip.rect.right());
        let bottom = draw.bottom().min(clip.rect.bottom());
        draw = PxRect::new(x, y, (right - x).max(0), (bottom - y).max(0));
    }
    frame.logical_rect(draw.to_resolved())
}
