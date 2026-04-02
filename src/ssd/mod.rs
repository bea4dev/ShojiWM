//! Server-side decoration data model.
//!
//! This module defines the Rust-side AST for future TypeScript/TSX based SSD descriptions.
//! At this stage the focus is limited to:
//!
//! - a stable node tree format
//! - a minimal style representation
//! - validation rules around the reserved client content slot (`WindowSlot`)
//!
//! Rendering, hit-testing and TS bridging are implemented in later milestones.

mod bridge;
mod evaluator;
mod interaction;
mod integration;
mod window_model;

use smithay::utils::Logical;

use crate::backend::text::{LabelSpec, measure_label_intrinsic};

pub use bridge::{
    DecorationBridgeError, WireDecorationChild, WireDecorationNode, WireProps, WireStyle,
    WireCompiledEffect, WireWindowAction, decode_tree_json,
};
pub use evaluator::{
    DecorationEvaluationError, DecorationEvaluationResult, DecorationEvaluator,
    DecorationHandlerInvocation, DecorationSchedulerTick, LayerEffectEvaluationResult,
    NodeDecorationEvaluator, RuntimeLayerEffectAssignment, RuntimeWindowAction,
    StaticDecorationEvaluator, evaluate_dynamic_decoration,
};
pub use interaction::DecorationInteractionSnapshot;
pub use integration::{
    CachedDecorationBuffer, ContentClip, DecorationRuntimeEvaluator, WindowDecorationState,
};
pub use window_model::{
    LayerKindSnapshot, LayerPositionSnapshot, OutputModeSnapshot, OutputPositionSnapshot,
    TransformOrigin, WaylandLayerSnapshot, WaylandOutputSnapshot, WaylandWindowAction,
    WaylandWindowSnapshot, WindowIconSnapshot, WindowPositionSnapshot, WindowTransform,
    layer_runtime_id,
};

/// Top-level decoration tree.
#[derive(Debug, Clone, PartialEq)]
pub struct DecorationTree {
    pub root: DecorationNode,
}

impl DecorationTree {
    pub fn new(root: DecorationNode) -> Self {
        Self { root }
    }

    /// Validate structural constraints required by the compositor.
    ///
    /// Current rules:
    ///
    /// - exactly one [`DecorationNodeKind::WindowSlot`] must exist
    /// - a window slot must not have children
    pub fn validate(&self) -> Result<DecorationTreeSummary, DecorationValidationError> {
        let mut stats = ValidationStats::default();
        validate_node(&self.root, &mut stats)?;

        match stats.window_slot_count {
            0 => Err(DecorationValidationError::MissingWindowSlot),
            1 => Ok(DecorationTreeSummary {
                window_slot_count: 1,
            }),
            count => Err(DecorationValidationError::MultipleWindowSlots { count }),
        }
    }

    /// Compute layout geometry for the decoration tree within the provided bounds.
    pub fn layout(
        &self,
        bounds: LogicalRect,
    ) -> Result<ComputedDecorationTree, DecorationLayoutError> {
        self.layout_with_scale(bounds, 1.0)
    }

    pub fn layout_with_scale(
        &self,
        bounds: LogicalRect,
        scale: f64,
    ) -> Result<ComputedDecorationTree, DecorationLayoutError> {
        self.validate()?;

        let mut root = layout_node_with_scale(&self.root, bounds, None, None, scale)?;
        root.sync_root_bounds(scale);
        if root.window_slot_rect().is_none() {
            return Err(DecorationLayoutError::MissingComputedWindowSlot);
        }

        Ok(ComputedDecorationTree { root })
    }
}

/// Minimal validation output for later phases to build on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecorationTreeSummary {
    pub window_slot_count: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ComputedDecorationTree {
    pub root: ComputedDecorationNode,
}

impl ComputedDecorationTree {
    pub fn window_slot_rect(&self) -> Option<LogicalRect> {
        self.root.window_slot_rect()
    }

    pub fn bounds_rect(&self) -> LogicalRect {
        self.root.bounds_rect()
    }

    /// Lower the computed layout tree into minimal render primitives.
    pub fn render_primitives(&self) -> Vec<DecorationRenderPrimitive> {
        let mut primitives = Vec::new();
        collect_render_primitives(&self.root, &mut primitives);
        primitives
    }

    /// Hit-test a logical point against the computed decoration tree.
    ///
    /// Priority order:
    ///
    /// 1. button actions
    /// 2. resize edges on the outer window border
    /// 3. client content slot
    /// 4. move on decoration chrome
    /// 5. outside
    pub fn hit_test(&self, point: LogicalPoint) -> DecorationHitTestResult {
        if let Some(action) = find_button_action(&self.root, point) {
            return DecorationHitTestResult::Action(action);
        }

        if let Some(slot_rect) = self.window_slot_rect() {
            if let Some(border) = self.root.window_border_style() {
                if let Some(edges) = hit_test_resize_edges(self.root.rect, border.width, point) {
                    return DecorationHitTestResult::Resize(edges);
                }
            }

            if slot_rect.contains(point) {
                return DecorationHitTestResult::ClientArea;
            }
        }

        if self.root.rect.contains(point) {
            return DecorationHitTestResult::Move;
        }

        DecorationHitTestResult::Outside
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ComputedDecorationNode {
    pub stable_id: Option<String>,
    pub kind: DecorationNodeKind,
    pub style: DecorationStyle,
    pub rect: LogicalRect,
    pub(crate) resolved_rect: ResolvedLogicalRect,
    pub(crate) resolved_content_rect: ResolvedLogicalRect,
    pub(crate) resolved_border_width: ResolvedLayoutValue,
    pub(crate) resolved_border_radius: ResolvedLayoutValue,
    pub effective_clip: Option<DecorationClip>,
    pub(crate) resolved_effective_clip: Option<ResolvedDecorationClip>,
    pub children: Vec<ComputedDecorationNode>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecorationClip {
    pub rect: LogicalRect,
    pub radius: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ResolvedDecorationClip {
    pub rect: ResolvedLogicalRect,
    pub radius: ResolvedLayoutValue,
}

impl ResolvedDecorationClip {
    fn round_to_logical_clip(self) -> DecorationClip {
        DecorationClip {
            rect: self.rect.round_to_logical_rect(),
            radius: self.radius.round_to_i32(),
        }
    }
}

impl ComputedDecorationNode {
    pub fn window_slot_rect(&self) -> Option<LogicalRect> {
        if matches!(self.kind, DecorationNodeKind::WindowSlot) {
            return Some(self.rect);
        }

        self.children.iter().find_map(Self::window_slot_rect)
    }

    pub(crate) fn resolved_window_slot_rect(&self) -> Option<ResolvedLogicalRect> {
        if matches!(self.kind, DecorationNodeKind::WindowSlot) {
            return Some(self.resolved_rect);
        }

        self.children
            .iter()
            .find_map(Self::resolved_window_slot_rect)
    }

    fn window_border_style(&self) -> Option<BorderStyle> {
        if matches!(self.kind, DecorationNodeKind::WindowBorder) {
            return self.style.border;
        }

        self.children.iter().find_map(Self::window_border_style)
    }

    pub(crate) fn bounds_rect(&self) -> LogicalRect {
        self.resolved_bounds_rect().round_to_logical_rect()
    }

    pub(crate) fn resolved_bounds_rect(&self) -> ResolvedLogicalRect {
        let mut min_x = self.resolved_rect.x;
        let mut min_y = self.resolved_rect.y;
        let mut max_x = self.resolved_rect.x + self.resolved_rect.width;
        let mut max_y = self.resolved_rect.y + self.resolved_rect.height;

        if !self.children.is_empty() {
            let inset = ResolvedLayoutEdges {
                top: self.resolved_content_rect.y - self.resolved_rect.y,
                left: self.resolved_content_rect.x - self.resolved_rect.x,
                right: (self.resolved_rect.x + self.resolved_rect.width)
                    - (self.resolved_content_rect.x + self.resolved_content_rect.width),
                bottom: (self.resolved_rect.y + self.resolved_rect.height)
                    - (self.resolved_content_rect.y + self.resolved_content_rect.height),
            };
            let mut child_min_x = ResolvedLayoutValue::from_raw(i32::MAX);
            let mut child_min_y = ResolvedLayoutValue::from_raw(i32::MAX);
            let mut child_max_x = ResolvedLayoutValue::from_raw(i32::MIN);
            let mut child_max_y = ResolvedLayoutValue::from_raw(i32::MIN);

            for child in &self.children {
                let child_bounds = child.resolved_bounds_rect();
                child_min_x = child_min_x.min(child_bounds.x);
                child_min_y = child_min_y.min(child_bounds.y);
                child_max_x = child_max_x.max(child_bounds.x + child_bounds.width);
                child_max_y = child_max_y.max(child_bounds.y + child_bounds.height);
            }

            min_x = min_x.min(child_min_x - inset.left);
            min_y = min_y.min(child_min_y - inset.top);
            max_x = max_x.max(child_max_x + inset.right);
            max_y = max_y.max(child_max_y + inset.bottom);
        }

        ResolvedLogicalRect {
            x: min_x,
            y: min_y,
            width: ResolvedLayoutValue::from_raw((max_x.raw() - min_x.raw()).max(0)),
            height: ResolvedLayoutValue::from_raw((max_y.raw() - min_y.raw()).max(0)),
        }
    }

    pub(crate) fn resolved_children_union_rect(&self) -> Option<ResolvedLogicalRect> {
        let mut children = self.children.iter();
        let first = children.next()?;
        let mut union = first.resolved_bounds_rect();

        for child in children {
            let child_bounds = child.resolved_bounds_rect();
            let min_x = union.x.min(child_bounds.x);
            let min_y = union.y.min(child_bounds.y);
            let max_x = (union.x + union.width).max(child_bounds.x + child_bounds.width);
            let max_y = (union.y + union.height).max(child_bounds.y + child_bounds.height);
            union = ResolvedLogicalRect {
                x: min_x,
                y: min_y,
                width: ResolvedLayoutValue::from_raw((max_x.raw() - min_x.raw()).max(0)),
                height: ResolvedLayoutValue::from_raw((max_y.raw() - min_y.raw()).max(0)),
            };
        }

        Some(union)
    }

    pub(crate) fn sync_root_bounds(&mut self, scale: f64) {
        self.resolved_rect = self.resolved_bounds_rect();
        self.rect = self.resolved_rect.round_to_logical_rect();
        self.resolved_content_rect = self.resolved_rect.inset(self.style.resolved_content_inset(scale));
        self.resolved_effective_clip =
            effective_clip_for_node_resolved(&self.to_decoration_node(), None, self.resolved_content_rect, scale);
        self.effective_clip = self
            .resolved_effective_clip
            .map(ResolvedDecorationClip::round_to_logical_clip);
    }

    fn to_decoration_node(&self) -> DecorationNode {
        DecorationNode {
            stable_id: self.stable_id.clone(),
            kind: self.kind.clone(),
            style: self.style.clone(),
            children: self.children.iter().map(Self::to_decoration_node).collect(),
        }
    }

    pub fn rects_for_stable_ids(
        &self,
        node_ids: &std::collections::HashSet<&str>,
        rects: &mut Vec<LogicalRect>,
    ) {
        if self
            .stable_id
            .as_deref()
            .is_some_and(|stable_id| node_ids.contains(stable_id))
        {
            rects.push(self.rect);
        }

        for child in &self.children {
            child.rects_for_stable_ids(node_ids, rects);
        }
    }
}

/// Minimal renderer-facing primitive set for milestone 1.
#[derive(Debug, Clone, PartialEq)]
pub enum DecorationRenderPrimitive {
    FillRect {
        rect: LogicalRect,
        color: Color,
        radius: Option<i32>,
    },
    BorderRect {
        rect: LogicalRect,
        width: i32,
        color: Color,
        radius: Option<i32>,
    },
    Label {
        rect: LogicalRect,
        text: String,
        color: Color,
    },
    AppIcon {
        rect: LogicalRect,
    },
    ShaderEffect {
        rect: LogicalRect,
        shader: CompiledEffect,
    },
    WindowSlot {
        rect: LogicalRect,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecorationHitTestResult {
    Outside,
    Move,
    Resize(ResizeEdges),
    Action(WindowAction),
    ClientArea,
}

bitflags::bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct ResizeEdges: u32 {
        const TOP = 0b0001;
        const BOTTOM = 0b0010;
        const LEFT = 0b0100;
        const RIGHT = 0b1000;

        const TOP_LEFT = Self::TOP.bits() | Self::LEFT.bits();
        const TOP_RIGHT = Self::TOP.bits() | Self::RIGHT.bits();
        const BOTTOM_LEFT = Self::BOTTOM.bits() | Self::LEFT.bits();
        const BOTTOM_RIGHT = Self::BOTTOM.bits() | Self::RIGHT.bits();
    }
}

#[derive(Debug, Default)]
struct ValidationStats {
    window_slot_count: usize,
}

fn validate_node(
    node: &DecorationNode,
    stats: &mut ValidationStats,
) -> Result<(), DecorationValidationError> {
    if matches!(node.kind, DecorationNodeKind::WindowSlot) {
        stats.window_slot_count += 1;
        if !node.children.is_empty() {
            return Err(DecorationValidationError::WindowSlotHasChildren);
        }
    }

    for child in &node.children {
        validate_node(child, stats)?;
    }

    Ok(())
}

/// A single node inside the decoration tree.
#[derive(Debug, Clone, PartialEq)]
pub struct DecorationNode {
    pub stable_id: Option<String>,
    pub kind: DecorationNodeKind,
    pub style: DecorationStyle,
    pub children: Vec<DecorationNode>,
}

impl DecorationNode {
    pub fn new(kind: DecorationNodeKind) -> Self {
        Self {
            stable_id: None,
            kind,
            style: DecorationStyle::default(),
            children: Vec::new(),
        }
    }

    pub fn with_style(mut self, style: DecorationStyle) -> Self {
        self.style = style;
        self
    }

    pub fn with_children(mut self, children: Vec<DecorationNode>) -> Self {
        self.children = children;
        self
    }

    pub fn push_child(&mut self, child: DecorationNode) {
        self.children.push(child);
    }

    pub fn layout_equivalent(&self, other: &Self) -> bool {
        self.stable_id == other.stable_id
            && kind_layout_equivalent(&self.kind, &other.kind)
            && layout_style_equivalent(&self.style, &other.style)
            && self.children.len() == other.children.len()
            && self
                .children
                .iter()
                .zip(other.children.iter())
                .all(|(left, right)| left.layout_equivalent(right))
    }
}

/// Supported node kinds for the initial SSD DSL.
#[derive(Debug, Clone, PartialEq)]
pub enum DecorationNodeKind {
    Box(BoxNode),
    Label(LabelNode),
    Button(ButtonNode),
    AppIcon,
    ShaderEffect(ShaderEffectNode),
    WindowBorder,
    /// Reserved anchor where the client surface is placed.
    WindowSlot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BoxNode {
    pub direction: LayoutDirection,
}

impl Default for BoxNode {
    fn default() -> Self {
        Self {
            direction: LayoutDirection::Column,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabelNode {
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ButtonNode {
    pub action: WindowAction,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ShaderEffectNode {
    pub direction: LayoutDirection,
    pub shader: CompiledEffect,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShaderModule {
    pub path: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ShaderUniformValue {
    Float(f32),
    Vec2([f32; 2]),
    Vec3([f32; 3]),
    Vec4([f32; 4]),
}

#[derive(Debug, Clone, PartialEq)]
pub struct ShaderStage {
    pub shader: ShaderModule,
    pub uniforms: std::collections::BTreeMap<String, ShaderUniformValue>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum EffectInput {
    Backdrop,
    XrayBackdrop,
    Shader(ShaderStage),
    Image(String),
    Named(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoiseKind {
    Salt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlendMode {
    Normal,
    Add,
    Screen,
    Multiply,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EffectInvalidationPolicy {
    OnSourceDamageBox { anti_artifact_margin: i32 },
    Always,
    Manual {
        dirty_when: bool,
        base: Option<Box<EffectInvalidationPolicy>>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct NoiseStage {
    pub kind: NoiseKind,
    pub amount: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub enum EffectStage {
    Shader(ShaderStage),
    Noise(NoiseStage),
    DualKawaseBlur(BackdropBlur),
    Save(String),
    Blend {
        input: EffectInput,
        mode: BlendMode,
        alpha: f32,
    },
    Unit(Box<CompiledEffect>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct CompiledEffect {
    pub input: EffectInput,
    pub invalidate: EffectInvalidationPolicy,
    pub pipeline: Vec<EffectStage>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BackgroundEffectConfig {
    pub effect: CompiledEffect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackdropBlur {
    pub radius: i32,
    pub passes: i32,
}

impl CompiledEffect {
    pub fn is_backdrop(&self) -> bool {
        matches!(self.input, EffectInput::Backdrop | EffectInput::XrayBackdrop)
    }

    pub fn is_texture_backed(&self) -> bool {
        matches!(
            self.input,
            EffectInput::Backdrop | EffectInput::XrayBackdrop | EffectInput::Shader(_)
        )
    }

    pub fn uses_backdrop_input(&self) -> bool {
        self.input == EffectInput::Backdrop
            || self.pipeline.iter().any(|stage| match stage {
                EffectStage::Blend { input, .. } => *input == EffectInput::Backdrop,
                EffectStage::Unit(effect) => effect.uses_backdrop_input(),
                _ => false,
            })
    }

    pub fn uses_xray_backdrop_input(&self) -> bool {
        self.input == EffectInput::XrayBackdrop
            || self.pipeline.iter().any(|stage| match stage {
                EffectStage::Blend { input, .. } => *input == EffectInput::XrayBackdrop,
                EffectStage::Unit(effect) => effect.uses_xray_backdrop_input(),
                _ => false,
            })
    }

    pub fn blur_stage(&self) -> Option<BackdropBlur> {
        self.pipeline.iter().find_map(|stage| match stage {
            EffectStage::DualKawaseBlur(blur) => Some(*blur),
            _ => None,
        })
    }

    pub fn last_shader_stage(&self) -> Option<&ShaderStage> {
        self.pipeline
            .iter()
            .rev()
            .find_map(|stage| match stage {
                EffectStage::Shader(shader) => Some(shader),
                _ => None,
            })
            .or_else(|| match &self.input {
                EffectInput::Shader(shader) => Some(shader),
                _ => None,
            })
    }

    pub fn invalidate_policy(&self) -> EffectInvalidationPolicy {
        self.invalidate.clone()
    }
}

/// Minimal action surface required by milestone 1.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WindowAction {
    Close,
    Maximize,
    Minimize,
    RuntimeHandler(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutDirection {
    Row,
    Column,
}

/// Minimal typed style object.
///
/// This is intentionally narrower than the final style surface described in the docs. It exists
/// to lock in core concepts early without overcommitting to full CSS compatibility.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DecorationStyle {
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub min_width: Option<i32>,
    pub min_height: Option<i32>,
    pub max_width: Option<i32>,
    pub max_height: Option<i32>,
    pub flex_grow: Option<f32>,
    pub flex_shrink: Option<f32>,
    pub padding: Edges,
    pub margin: Edges,
    pub gap: Option<i32>,
    pub justify_content: Option<JustifyContent>,
    pub align_items: Option<AlignItems>,
    pub background: Option<Color>,
    pub color: Option<Color>,
    pub opacity: Option<f32>,
    pub border: Option<BorderStyle>,
    pub border_top: Option<BorderStyle>,
    pub border_right: Option<BorderStyle>,
    pub border_bottom: Option<BorderStyle>,
    pub border_left: Option<BorderStyle>,
    pub border_radius: Option<i32>,
    pub visible: Option<bool>,
    pub cursor: Option<String>,
    pub font_size: Option<i32>,
    pub font_weight: Option<serde_json::Value>,
    pub font_family: Option<Vec<String>>,
    pub text_align: Option<String>,
    pub line_height: Option<i32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Edges {
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
    pub left: i32,
}

impl Edges {
    pub fn all(value: i32) -> Self {
        Self {
            top: value,
            right: value,
            bottom: value,
            left: value,
        }
    }

    pub fn symmetric(horizontal: i32, vertical: i32) -> Self {
        Self {
            top: vertical,
            right: horizontal,
            bottom: vertical,
            left: horizontal,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JustifyContent {
    Start,
    Center,
    End,
    SpaceBetween,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlignItems {
    Start,
    Center,
    End,
    Stretch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BorderStyle {
    pub width: i32,
    pub color: Color,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub const TRANSPARENT: Self = Self::rgba(0, 0, 0, 0);
    pub const WHITE: Self = Self::rgba(255, 255, 255, 255);
    pub const BLACK: Self = Self::rgba(0, 0, 0, 255);

    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    pub fn with_opacity(self, opacity: Option<f32>) -> Self {
        let Some(opacity) = opacity else {
            return self;
        };
        let alpha = ((self.a as f32) * opacity.clamp(0.0, 1.0)).round() as u8;
        Self { a: alpha, ..self }
    }
}

/// Future-facing slot geometry marker used by the layout phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LogicalRect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub _kind: std::marker::PhantomData<Logical>,
}

impl LogicalRect {
    pub fn new(x: i32, y: i32, width: i32, height: i32) -> Self {
        Self {
            x,
            y,
            width: width.max(0),
            height: height.max(0),
            _kind: std::marker::PhantomData,
        }
    }

    pub fn inset(self, edges: Edges) -> Self {
        let width = (self.width - edges.left - edges.right).max(0);
        let height = (self.height - edges.top - edges.bottom).max(0);
        Self::new(self.x + edges.left, self.y + edges.top, width, height)
    }

    pub fn contains(self, point: LogicalPoint) -> bool {
        point.x >= self.x
            && point.y >= self.y
            && point.x < self.x + self.width
            && point.y < self.y + self.height
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LogicalPoint {
    pub x: i32,
    pub y: i32,
}

impl LogicalPoint {
    pub const fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }
}

#[allow(dead_code)]
const RESOLVED_LAYOUT_SUBPIXELS: i32 = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub(crate) struct ResolvedLayoutValue(i32);

impl ResolvedLayoutValue {
    const ZERO: Self = Self(0);

    const fn from_raw(raw: i32) -> Self {
        Self(raw)
    }

    const fn raw(self) -> i32 {
        self.0
    }

    const fn from_i32(value: i32) -> Self {
        Self(value * RESOLVED_LAYOUT_SUBPIXELS)
    }

    fn from_f32(value: f32) -> Self {
        Self((value * RESOLVED_LAYOUT_SUBPIXELS as f32).round() as i32)
    }

    fn to_f32(self) -> f32 {
        self.0 as f32 / RESOLVED_LAYOUT_SUBPIXELS as f32
    }

    fn round_to_i32(self) -> i32 {
        self.to_f32().round() as i32
    }

    fn snap_edge(self, scale: f64) -> Self {
        let scale = scale.abs().max(0.0001);
        Self::from_f32((((self.to_f32() as f64) * scale).round() / scale) as f32)
    }
}

impl std::ops::Add for ResolvedLayoutValue {
    type Output = Self;

    fn add(self, rhs: Self) -> Self::Output {
        Self(self.0 + rhs.0)
    }
}

impl std::ops::Sub for ResolvedLayoutValue {
    type Output = Self;

    fn sub(self, rhs: Self) -> Self::Output {
        Self(self.0 - rhs.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct ResolvedLayoutEdges {
    top: ResolvedLayoutValue,
    right: ResolvedLayoutValue,
    bottom: ResolvedLayoutValue,
    left: ResolvedLayoutValue,
}

impl ResolvedLayoutEdges {
    fn from_edges(edges: Edges) -> Self {
        Self {
            top: ResolvedLayoutValue::from_i32(edges.top),
            right: ResolvedLayoutValue::from_i32(edges.right),
            bottom: ResolvedLayoutValue::from_i32(edges.bottom),
            left: ResolvedLayoutValue::from_i32(edges.left),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct ResolvedLogicalRect {
    x: ResolvedLayoutValue,
    y: ResolvedLayoutValue,
    width: ResolvedLayoutValue,
    height: ResolvedLayoutValue,
}

impl ResolvedLogicalRect {
    fn from_logical(rect: LogicalRect) -> Self {
        Self {
            x: ResolvedLayoutValue::from_i32(rect.x),
            y: ResolvedLayoutValue::from_i32(rect.y),
            width: ResolvedLayoutValue::from_i32(rect.width),
            height: ResolvedLayoutValue::from_i32(rect.height),
        }
    }

    fn left(self) -> ResolvedLayoutValue {
        self.x
    }

    fn top(self) -> ResolvedLayoutValue {
        self.y
    }

    fn right(self) -> ResolvedLayoutValue {
        self.x + self.width
    }

    fn bottom(self) -> ResolvedLayoutValue {
        self.y + self.height
    }

    fn inset(self, edges: ResolvedLayoutEdges) -> Self {
        let left = self.x + edges.left;
        let top = self.y + edges.top;
        let right = self.right() - edges.right;
        let bottom = self.bottom() - edges.bottom;
        Self {
            x: left,
            y: top,
            width: ResolvedLayoutValue::from_raw((right.raw() - left.raw()).max(0)),
            height: ResolvedLayoutValue::from_raw((bottom.raw() - top.raw()).max(0)),
        }
    }

    fn snapped_size(self, scale_x: f64, scale_y: f64) -> (ResolvedLayoutValue, ResolvedLayoutValue) {
        let left = self.left().snap_edge(scale_x);
        let top = self.top().snap_edge(scale_y);
        let right = self.right().snap_edge(scale_x);
        let bottom = self.bottom().snap_edge(scale_y);
        (
            ResolvedLayoutValue::from_raw((right.raw() - left.raw()).max(0)),
            ResolvedLayoutValue::from_raw((bottom.raw() - top.raw()).max(0)),
        )
    }

    fn round_to_logical_rect(self) -> LogicalRect {
        LogicalRect::new(
            self.x.round_to_i32(),
            self.y.round_to_i32(),
            self.width.round_to_i32(),
            self.height.round_to_i32(),
        )
    }

    pub(crate) fn to_precise_logical_rect(self) -> crate::backend::visual::PreciseLogicalRect {
        crate::backend::visual::PreciseLogicalRect {
            x: self.x.to_f32(),
            y: self.y.to_f32(),
            width: self.width.to_f32(),
            height: self.height.to_f32(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecorationValidationError {
    MissingWindowSlot,
    MultipleWindowSlots { count: usize },
    WindowSlotHasChildren,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecorationLayoutError {
    Validation(DecorationValidationError),
    MissingComputedWindowSlot,
}

impl From<DecorationValidationError> for DecorationLayoutError {
    fn from(value: DecorationValidationError) -> Self {
        Self::Validation(value)
    }
}

pub(super) fn layout_node(
    node: &DecorationNode,
    rect: LogicalRect,
    inherited_clip: Option<DecorationClip>,
    window_slot_size: Option<(i32, i32)>,
) -> Result<ComputedDecorationNode, DecorationLayoutError> {
    layout_node_with_scale(node, rect, inherited_clip, window_slot_size, 1.0)
}

pub(super) fn layout_node_with_scale(
    node: &DecorationNode,
    rect: LogicalRect,
    inherited_clip: Option<DecorationClip>,
    window_slot_size: Option<(i32, i32)>,
    scale: f64,
) -> Result<ComputedDecorationNode, DecorationLayoutError> {
    layout_node_resolved(
        node,
        ResolvedLogicalRect::from_logical(rect),
        inherited_clip.map(|clip| ResolvedDecorationClip {
            rect: ResolvedLogicalRect::from_logical(clip.rect),
            radius: ResolvedLayoutValue::from_i32(clip.radius),
        }),
        window_slot_size,
        scale,
    )
}

fn layout_node_resolved(
    node: &DecorationNode,
    resolved_rect: ResolvedLogicalRect,
    inherited_clip: Option<ResolvedDecorationClip>,
    window_slot_size: Option<(i32, i32)>,
    scale: f64,
) -> Result<ComputedDecorationNode, DecorationLayoutError> {
    let resolved_border_width = node
        .style
        .border
        .map(|border| ResolvedLayoutValue::from_i32(border.width.max(0)).snap_edge(scale))
        .unwrap_or(ResolvedLayoutValue::ZERO);
    let resolved_border_radius =
        ResolvedLayoutValue::from_i32(node.style.border_radius.unwrap_or(0).max(0)).snap_edge(scale);
    let content_rect = resolved_rect.inset(node.style.resolved_content_inset(scale));
    let effective_clip =
        effective_clip_for_node_resolved(node, inherited_clip, content_rect, scale);

    let children = match &node.kind {
        DecorationNodeKind::Box(layout) => layout_box_children(
            node,
            content_rect,
            layout.direction,
            effective_clip,
            window_slot_size,
            scale,
        )?,
        DecorationNodeKind::ShaderEffect(effect) => layout_box_children(
            node,
            content_rect,
            effect.direction,
            effective_clip,
            window_slot_size,
            scale,
        )?,
        _ if node.children.is_empty() => Vec::new(),
        _ => node
            .children
            .iter()
            .map(|child| {
                layout_node_resolved(
                    child,
                    content_rect,
                    effective_clip,
                    window_slot_size,
                    scale,
                )
            })
            .collect::<Result<Vec<_>, _>>()?,
    };

    Ok(ComputedDecorationNode {
        stable_id: node.stable_id.clone(),
        kind: node.kind.clone(),
        style: node.style.clone(),
        rect: resolved_rect.round_to_logical_rect(),
        resolved_rect,
        resolved_content_rect: content_rect,
        resolved_border_width,
        resolved_border_radius,
        effective_clip: effective_clip.map(ResolvedDecorationClip::round_to_logical_clip),
        resolved_effective_clip: effective_clip,
        children,
    })
}

pub(super) fn reapply_tree_preserving_layout(
    computed: &mut ComputedDecorationNode,
    node: &DecorationNode,
    inherited_clip: Option<ResolvedDecorationClip>,
    scale: f64,
) {
    computed.stable_id = node.stable_id.clone();
    computed.kind = node.kind.clone();
    computed.style = node.style.clone();
    let content_rect = computed
        .resolved_rect
        .inset(node.style.resolved_content_inset(scale));
    let effective_clip = effective_clip_for_node_resolved(node, inherited_clip, content_rect, scale);
    computed.rect = computed.resolved_rect.round_to_logical_rect();
    computed.resolved_content_rect = content_rect;
    computed.resolved_border_width = node
        .style
        .border
        .map(|border| ResolvedLayoutValue::from_i32(border.width.max(0)).snap_edge(scale))
        .unwrap_or(ResolvedLayoutValue::ZERO);
    computed.resolved_border_radius =
        ResolvedLayoutValue::from_i32(node.style.border_radius.unwrap_or(0).max(0)).snap_edge(scale);
    computed.effective_clip = effective_clip.map(ResolvedDecorationClip::round_to_logical_clip);
    computed.resolved_effective_clip = effective_clip;

    for (computed_child, node_child) in computed.children.iter_mut().zip(node.children.iter()) {
        reapply_tree_preserving_layout(
            computed_child,
            node_child,
            computed.resolved_effective_clip,
            scale,
        );
    }
}

fn layout_box_children(
    node: &DecorationNode,
    content_rect: ResolvedLogicalRect,
    direction: LayoutDirection,
    effective_clip: Option<ResolvedDecorationClip>,
    window_slot_size: Option<(i32, i32)>,
    scale: f64,
) -> Result<Vec<ComputedDecorationNode>, DecorationLayoutError> {
    if node.children.is_empty() {
        return Ok(Vec::new());
    }

    let gap = node.style.resolved_gap(scale);
    let main_available = direction.main_len_resolved(content_rect);
    let cross_available = direction.cross_len_resolved(content_rect);
    let total_gap = ResolvedLayoutValue::from_raw(
        gap.raw() * (node.children.len().saturating_sub(1) as i32),
    );

    let mut base_sizes = Vec::with_capacity(node.children.len());
    let mut flexes = Vec::with_capacity(node.children.len());
    let mut shrink_factors = Vec::with_capacity(node.children.len());
    let mut auto_main_flags = Vec::with_capacity(node.children.len());

    let mut base_sum = ResolvedLayoutValue::ZERO;
    let mut total_flex = 0.0f32;
    let mut total_shrink = 0.0f32;

    for child in &node.children {
        let base = child.preferred_main_size_resolved(direction, window_slot_size, scale);
        let flex = child.flex_grow_for_layout();
        let shrink = child.flex_shrink_for_layout(direction);
        let auto_main = child.expands_auto_main_axis(direction, window_slot_size, scale);
        base_sizes.push(base);
        flexes.push(flex);
        shrink_factors.push(shrink);
        auto_main_flags.push(auto_main);
        base_sum = base_sum + base;
        total_flex += flex;
        total_shrink += shrink;
    }

    let remaining = ResolvedLayoutValue::from_raw(
        (main_available.raw() - total_gap.raw() - base_sum.raw()).max(0),
    );
    let overflow = ResolvedLayoutValue::from_raw(
        (total_gap.raw() + base_sum.raw() - main_available.raw()).max(0),
    );
    let mut allocated = base_sizes;

    if remaining.raw() > 0 && total_flex > 0.0 {
        let mut distributed = ResolvedLayoutValue::ZERO;
        let mut flex_indices = flexes
            .iter()
            .enumerate()
            .filter_map(|(idx, flex)| (*flex > 0.0).then_some(idx))
            .peekable();

        while let Some(idx) = flex_indices.next() {
            let share = if flex_indices.peek().is_none() {
                ResolvedLayoutValue::from_raw(remaining.raw() - distributed.raw())
            } else {
                ResolvedLayoutValue::from_raw(
                    ((remaining.raw() as f32) * (flexes[idx] / total_flex)).round() as i32,
                )
            };
            allocated[idx] = allocated[idx] + share;
            distributed = distributed + share;
        }
    } else if remaining.raw() > 0 {
        if let Some(idx) = auto_main_flags
            .iter()
            .enumerate()
            .rev()
            .find_map(|(idx, auto)| (*auto).then_some(idx))
        {
            allocated[idx] = allocated[idx] + remaining;
        }
    } else if overflow.raw() > 0 && total_shrink > 0.0 {
        let shrink_indices = shrink_factors
            .iter()
            .enumerate()
            .filter_map(|(idx, shrink)| (*shrink > 0.0).then_some(idx))
            .collect::<Vec<_>>();
        let mut remaining_overflow = overflow.raw();

        for (position, idx) in shrink_indices.iter().copied().enumerate() {
            if remaining_overflow <= 0 {
                break;
            }

            let requested = if position + 1 == shrink_indices.len() {
                remaining_overflow
            } else {
                (((overflow.raw() as f32) * (shrink_factors[idx] / total_shrink)).round() as i32)
                    .max(0)
                    .min(remaining_overflow)
            };
            let actual = requested.min(allocated[idx].raw().max(0));
            allocated[idx] =
                ResolvedLayoutValue::from_raw((allocated[idx].raw() - actual).max(0));
            remaining_overflow -= actual;
        }

        if remaining_overflow > 0 {
            for idx in shrink_indices.iter().copied().rev() {
                if remaining_overflow <= 0 {
                    break;
                }
                let actual = remaining_overflow.min(allocated[idx].raw().max(0));
                allocated[idx] =
                    ResolvedLayoutValue::from_raw((allocated[idx].raw() - actual).max(0));
                remaining_overflow -= actual;
            }
        }
    }

    let mut cursor = direction.main_origin_resolved(content_rect);
    let mut children = Vec::with_capacity(node.children.len());

    for (child, main_size) in node.children.iter().zip(allocated.into_iter()) {
        let child_align = node.style.align_items;
        let cross_size = child.preferred_cross_size_resolved(
            direction,
            cross_available,
            child_align,
            window_slot_size,
            scale,
        );
        let cross_origin = direction.cross_origin_for_child_resolved(
            content_rect,
            child_align,
            cross_size,
        );

        let child_rect = direction.rect_resolved(cursor, cross_origin, main_size, cross_size);
        children.push(layout_node_resolved(
            child,
            child_rect,
            effective_clip,
            window_slot_size,
            scale,
        )?);
        cursor = cursor + main_size + gap;
    }

    Ok(children)
}

impl DecorationNode {
    fn preferred_main_size_resolved(
        &self,
        direction: LayoutDirection,
        window_slot_size: Option<(i32, i32)>,
        scale: f64,
    ) -> ResolvedLayoutValue {
        let explicit = match direction {
            LayoutDirection::Row => self.style.width,
            LayoutDirection::Column => self.style.height,
        };

        let fallback = explicit
            .map(|value| ResolvedLayoutValue::from_i32(value).snap_edge(scale))
            .unwrap_or_else(|| {
                self.auto_size_resolved(window_slot_size, scale)
                    .map(|(width, height)| match direction {
                        LayoutDirection::Row => width,
                        LayoutDirection::Column => height,
                    })
                    .unwrap_or_else(|| match self.kind {
                        DecorationNodeKind::WindowSlot => ResolvedLayoutValue::ZERO,
                        _ => ResolvedLayoutValue::ZERO,
                    })
            });

        self.style.clamp_main_resolved(direction, fallback, scale)
    }

    fn preferred_main_size(&self, direction: LayoutDirection, window_slot_size: Option<(i32, i32)>) -> i32 {
        let explicit = match direction {
            LayoutDirection::Row => self.style.width,
            LayoutDirection::Column => self.style.height,
        };

        let fallback = explicit.unwrap_or_else(|| {
            self.auto_size(window_slot_size)
                .map(|(width, height)| match direction {
                    LayoutDirection::Row => width,
                    LayoutDirection::Column => height,
                })
                .unwrap_or_else(|| match self.kind {
                    DecorationNodeKind::WindowSlot => 0,
                    _ => 0,
                })
        });

        self.style.clamp_main(direction, fallback)
    }

    fn preferred_cross_size_resolved(
        &self,
        direction: LayoutDirection,
        available_cross: ResolvedLayoutValue,
        align: Option<AlignItems>,
        window_slot_size: Option<(i32, i32)>,
        scale: f64,
    ) -> ResolvedLayoutValue {
        let explicit = match direction {
            LayoutDirection::Row => self.style.height,
            LayoutDirection::Column => self.style.width,
        };

        let fallback = explicit
            .map(|value| ResolvedLayoutValue::from_i32(value).snap_edge(scale))
            .unwrap_or_else(|| {
                if matches!(align.unwrap_or(AlignItems::Stretch), AlignItems::Stretch)
                    && available_cross.raw() > 0
                {
                    return available_cross;
                }

                self.auto_size_resolved(window_slot_size, scale)
                    .map(|(width, height)| match direction {
                        LayoutDirection::Row => height,
                        LayoutDirection::Column => width,
                    })
                    .unwrap_or(available_cross)
            });

        self.style.clamp_cross_resolved(direction, fallback, scale)
    }

    fn preferred_cross_size(
        &self,
        direction: LayoutDirection,
        available_cross: i32,
        align: Option<AlignItems>,
        window_slot_size: Option<(i32, i32)>,
    ) -> i32 {
        let explicit = match direction {
            LayoutDirection::Row => self.style.height,
            LayoutDirection::Column => self.style.width,
        };

        let fallback = explicit.unwrap_or_else(|| {
            if matches!(align.unwrap_or(AlignItems::Stretch), AlignItems::Stretch) && available_cross > 0 {
                return available_cross;
            }

            self.auto_size(window_slot_size)
                .map(|(width, height)| match direction {
                    LayoutDirection::Row => height,
                    LayoutDirection::Column => width,
                })
                .unwrap_or(available_cross)
        });

        self.style.clamp_cross(direction, fallback)
    }

    fn flex_grow_for_layout(&self) -> f32 {
        self.style.flex_grow.unwrap_or_else(|| {
            if matches!(self.kind, DecorationNodeKind::WindowSlot) {
                1.0
            } else {
                0.0
            }
        })
    }

    fn flex_shrink_for_layout(&self, direction: LayoutDirection) -> f32 {
        self.style.flex_shrink.unwrap_or_else(|| {
            let explicit_main_size = match direction {
                LayoutDirection::Row => self.style.width,
                LayoutDirection::Column => self.style.height,
            };

            if explicit_main_size.is_none() || matches!(self.kind, DecorationNodeKind::WindowSlot) {
                1.0
            } else {
                0.0
            }
        })
    }

    fn expands_auto_main_axis(
        &self,
        direction: LayoutDirection,
        window_slot_size: Option<(i32, i32)>,
        scale: f64,
    ) -> bool {
        let explicit_main_size = match direction {
            LayoutDirection::Row => self.style.width,
            LayoutDirection::Column => self.style.height,
        };

        explicit_main_size.is_none() && self.auto_size_resolved(window_slot_size, scale).is_some()
    }

    fn intrinsic_size(&self, window_slot_size: Option<(i32, i32)>) -> Option<(i32, i32)> {
        match &self.kind {
            DecorationNodeKind::Label(label) => {
                let font_size = self.style.font_size.unwrap_or(13).max(1);
                let line_height = self.style.line_height.unwrap_or(font_size + 4).max(font_size);
                let spec = LabelSpec {
                    rect: LogicalRect::new(0, 0, 0, 0),
                    rect_precise: None,
                    text: label.text.clone(),
                    color: self.style.color.unwrap_or(Color::WHITE).with_opacity(self.style.opacity),
                    font_size,
                    font_weight: self.style.font_weight.clone(),
                    font_family: self.style.font_family.clone(),
                    text_align: self.style.text_align.clone(),
                    line_height: Some(line_height),
                    raster_scale: 1,
                };
                Some(measure_label_intrinsic(&spec))
            }
            DecorationNodeKind::WindowSlot => window_slot_size,
            _ => None,
        }
    }

    fn intrinsic_size_resolved(
        &self,
        window_slot_size: Option<(i32, i32)>,
    ) -> Option<(ResolvedLayoutValue, ResolvedLayoutValue)> {
        self.intrinsic_size(window_slot_size).map(|(width, height)| {
            (
                ResolvedLayoutValue::from_i32(width),
                ResolvedLayoutValue::from_i32(height),
            )
        })
    }

    fn auto_size(&self, window_slot_size: Option<(i32, i32)>) -> Option<(i32, i32)> {
        self.intrinsic_size(window_slot_size)
            .or_else(|| self.content_based_size(window_slot_size))
    }

    fn auto_size_resolved(
        &self,
        window_slot_size: Option<(i32, i32)>,
        scale: f64,
    ) -> Option<(ResolvedLayoutValue, ResolvedLayoutValue)> {
        self.intrinsic_size_resolved(window_slot_size)
            .or_else(|| self.content_based_size_resolved(window_slot_size, scale))
    }

    fn content_based_size(&self, window_slot_size: Option<(i32, i32)>) -> Option<(i32, i32)> {
        match &self.kind {
            DecorationNodeKind::Box(layout) => Some(self.stack_content_size(layout.direction, window_slot_size)),
            DecorationNodeKind::ShaderEffect(effect) => {
                Some(self.stack_content_size(effect.direction, window_slot_size))
            }
            DecorationNodeKind::WindowBorder => Some(self.overlay_content_size(window_slot_size)),
            _ => None,
        }
    }

    fn content_based_size_resolved(
        &self,
        window_slot_size: Option<(i32, i32)>,
        scale: f64,
    ) -> Option<(ResolvedLayoutValue, ResolvedLayoutValue)> {
        match &self.kind {
            DecorationNodeKind::Box(layout) => {
                Some(self.stack_content_size_resolved(layout.direction, window_slot_size, scale))
            }
            DecorationNodeKind::ShaderEffect(effect) => {
                Some(self.stack_content_size_resolved(effect.direction, window_slot_size, scale))
            }
            DecorationNodeKind::WindowBorder => {
                Some(self.overlay_content_size_resolved(window_slot_size, scale))
            }
            _ => None,
        }
    }

    fn stack_content_size(&self, direction: LayoutDirection, window_slot_size: Option<(i32, i32)>) -> (i32, i32) {
        let inset = self.style.content_inset();
        if self.children.is_empty() {
            return (
                inset.left + inset.right,
                inset.top + inset.bottom,
            );
        }

        let gap = self.style.gap.unwrap_or(0).max(0);
        let mut main_sum = 0;
        let mut cross_max = 0;

        for child in &self.children {
            let child_main = child.preferred_main_size(direction, window_slot_size).max(0);
            let child_cross = child
                .preferred_cross_size(direction, 0, child.style.align_items, window_slot_size)
                .max(0);
            main_sum += child_main;
            cross_max = cross_max.max(child_cross);
        }

        main_sum += gap * self.children.len().saturating_sub(1) as i32;

        match direction {
            LayoutDirection::Row => (
                main_sum + inset.left + inset.right,
                cross_max + inset.top + inset.bottom,
            ),
            LayoutDirection::Column => (
                cross_max + inset.left + inset.right,
                main_sum + inset.top + inset.bottom,
            ),
        }
    }

    fn stack_content_size_resolved(
        &self,
        direction: LayoutDirection,
        window_slot_size: Option<(i32, i32)>,
        scale: f64,
    ) -> (ResolvedLayoutValue, ResolvedLayoutValue) {
        let inset = self.style.resolved_content_inset(scale);
        if self.children.is_empty() {
            return (
                inset.left + inset.right,
                inset.top + inset.bottom,
            );
        }

        let gap = self.style.resolved_gap(scale);
        let mut main_sum = ResolvedLayoutValue::ZERO;
        let mut cross_max = ResolvedLayoutValue::ZERO;

        for child in &self.children {
            let child_main = child.preferred_main_size_resolved(direction, window_slot_size, scale);
            let child_cross = child.preferred_cross_size_resolved(
                direction,
                ResolvedLayoutValue::ZERO,
                child.style.align_items,
                window_slot_size,
                scale,
            );
            main_sum = main_sum + child_main;
            cross_max = cross_max.max(child_cross);
        }

        main_sum = main_sum
            + ResolvedLayoutValue::from_raw(gap.raw() * self.children.len().saturating_sub(1) as i32);

        match direction {
            LayoutDirection::Row => (
                main_sum + inset.left + inset.right,
                cross_max + inset.top + inset.bottom,
            ),
            LayoutDirection::Column => (
                cross_max + inset.left + inset.right,
                main_sum + inset.top + inset.bottom,
            ),
        }
    }

    fn overlay_content_size(&self, window_slot_size: Option<(i32, i32)>) -> (i32, i32) {
        let inset = self.style.content_inset();
        let mut width = 0;
        let mut height = 0;

        for child in &self.children {
            width = width.max(child.preferred_main_size(LayoutDirection::Row, window_slot_size).max(0));
            height = height.max(child.preferred_main_size(LayoutDirection::Column, window_slot_size).max(0));
        }

        (
            width + inset.left + inset.right,
            height + inset.top + inset.bottom,
        )
    }

    fn overlay_content_size_resolved(
        &self,
        window_slot_size: Option<(i32, i32)>,
        scale: f64,
    ) -> (ResolvedLayoutValue, ResolvedLayoutValue) {
        let inset = self.style.resolved_content_inset(scale);
        let mut width = ResolvedLayoutValue::ZERO;
        let mut height = ResolvedLayoutValue::ZERO;

        for child in &self.children {
            width = width.max(
                child.preferred_main_size_resolved(LayoutDirection::Row, window_slot_size, scale),
            );
            height = height.max(
                child.preferred_main_size_resolved(LayoutDirection::Column, window_slot_size, scale),
            );
        }

        (
            width + inset.left + inset.right,
            height + inset.top + inset.bottom,
        )
    }
}

impl DecorationStyle {
    fn resolved_content_inset(&self, scale: f64) -> ResolvedLayoutEdges {
        let border = self
            .border
            .map(|border| ResolvedLayoutValue::from_i32(border.width).snap_edge(scale))
            .unwrap_or(ResolvedLayoutValue::ZERO);
        let padding = ResolvedLayoutEdges::from_edges(self.padding);
        ResolvedLayoutEdges {
            top: padding.top.snap_edge(scale) + border,
            right: padding.right.snap_edge(scale) + border,
            bottom: padding.bottom.snap_edge(scale) + border,
            left: padding.left.snap_edge(scale) + border,
        }
    }

    fn resolved_gap(&self, scale: f64) -> ResolvedLayoutValue {
        ResolvedLayoutValue::from_i32(self.gap.unwrap_or(0).max(0)).snap_edge(scale)
    }

    fn content_inset(&self) -> Edges {
        let border = self.border.map(|border| border.width).unwrap_or(0).max(0);
        Edges {
            top: self.padding.top + border,
            right: self.padding.right + border,
            bottom: self.padding.bottom + border,
            left: self.padding.left + border,
        }
    }

    fn clamp_main(&self, direction: LayoutDirection, value: i32) -> i32 {
        match direction {
            LayoutDirection::Row => clamp_size(value, self.min_width, self.max_width),
            LayoutDirection::Column => clamp_size(value, self.min_height, self.max_height),
        }
    }

    fn clamp_main_resolved(
        &self,
        direction: LayoutDirection,
        value: ResolvedLayoutValue,
        scale: f64,
    ) -> ResolvedLayoutValue {
        clamp_size_resolved(
            value,
            match direction {
                LayoutDirection::Row => self.min_width,
                LayoutDirection::Column => self.min_height,
            },
            match direction {
                LayoutDirection::Row => self.max_width,
                LayoutDirection::Column => self.max_height,
            },
            scale,
        )
    }

    fn clamp_cross(&self, direction: LayoutDirection, value: i32) -> i32 {
        match direction {
            LayoutDirection::Row => clamp_size(value, self.min_height, self.max_height),
            LayoutDirection::Column => clamp_size(value, self.min_width, self.max_width),
        }
    }

    fn clamp_cross_resolved(
        &self,
        direction: LayoutDirection,
        value: ResolvedLayoutValue,
        scale: f64,
    ) -> ResolvedLayoutValue {
        clamp_size_resolved(
            value,
            match direction {
                LayoutDirection::Row => self.min_height,
                LayoutDirection::Column => self.min_width,
            },
            match direction {
                LayoutDirection::Row => self.max_height,
                LayoutDirection::Column => self.max_width,
            },
            scale,
        )
    }
}

fn kind_layout_equivalent(left: &DecorationNodeKind, right: &DecorationNodeKind) -> bool {
    match (left, right) {
        (DecorationNodeKind::Box(left), DecorationNodeKind::Box(right)) => left.direction == right.direction,
        (DecorationNodeKind::Label(left), DecorationNodeKind::Label(right)) => left.text == right.text,
        (DecorationNodeKind::Button(_), DecorationNodeKind::Button(_)) => true,
        (DecorationNodeKind::AppIcon, DecorationNodeKind::AppIcon) => true,
        (DecorationNodeKind::ShaderEffect(left), DecorationNodeKind::ShaderEffect(right)) => {
            left.direction == right.direction
        }
        (DecorationNodeKind::WindowBorder, DecorationNodeKind::WindowBorder) => true,
        (DecorationNodeKind::WindowSlot, DecorationNodeKind::WindowSlot) => true,
        _ => false,
    }
}

fn layout_style_equivalent(left: &DecorationStyle, right: &DecorationStyle) -> bool {
    left.width == right.width
        && left.height == right.height
        && left.min_width == right.min_width
        && left.min_height == right.min_height
        && left.max_width == right.max_width
        && left.max_height == right.max_height
        && left.flex_grow == right.flex_grow
        && left.flex_shrink == right.flex_shrink
        && left.padding == right.padding
        && left.margin == right.margin
        && left.gap == right.gap
        && left.justify_content == right.justify_content
        && left.align_items == right.align_items
        && left.border.map(|border| border.width) == right.border.map(|border| border.width)
        && left.border_top.map(|border| border.width) == right.border_top.map(|border| border.width)
        && left.border_right.map(|border| border.width) == right.border_right.map(|border| border.width)
        && left.border_bottom.map(|border| border.width) == right.border_bottom.map(|border| border.width)
        && left.border_left.map(|border| border.width) == right.border_left.map(|border| border.width)
        && left.font_size == right.font_size
        && left.font_weight == right.font_weight
        && left.font_family == right.font_family
        && left.line_height == right.line_height
        && left.visible == right.visible
}

fn clamp_size(value: i32, min: Option<i32>, max: Option<i32>) -> i32 {
    let mut value = value.max(0);
    if let Some(min) = min {
        value = value.max(min.max(0));
    }
    if let Some(max) = max {
        value = value.min(max.max(0));
    }
    value
}

fn clamp_size_resolved(
    value: ResolvedLayoutValue,
    min: Option<i32>,
    max: Option<i32>,
    scale: f64,
) -> ResolvedLayoutValue {
    let mut value = ResolvedLayoutValue::from_raw(value.raw().max(0));
    if let Some(min) = min {
        value = value.max(ResolvedLayoutValue::from_i32(min.max(0)).snap_edge(scale));
    }
    if let Some(max) = max {
        value = value.min(ResolvedLayoutValue::from_i32(max.max(0)).snap_edge(scale));
    }
    value
}

impl LayoutDirection {
    fn main_origin_resolved(self, rect: ResolvedLogicalRect) -> ResolvedLayoutValue {
        match self {
            LayoutDirection::Row => rect.x,
            LayoutDirection::Column => rect.y,
        }
    }

    fn main_len_resolved(self, rect: ResolvedLogicalRect) -> ResolvedLayoutValue {
        match self {
            LayoutDirection::Row => rect.width,
            LayoutDirection::Column => rect.height,
        }
    }

    fn cross_len_resolved(self, rect: ResolvedLogicalRect) -> ResolvedLayoutValue {
        match self {
            LayoutDirection::Row => rect.height,
            LayoutDirection::Column => rect.width,
        }
    }

    fn main_origin(self, rect: LogicalRect) -> i32 {
        match self {
            LayoutDirection::Row => rect.x,
            LayoutDirection::Column => rect.y,
        }
    }

    fn main_len(self, rect: LogicalRect) -> i32 {
        match self {
            LayoutDirection::Row => rect.width,
            LayoutDirection::Column => rect.height,
        }
    }

    fn cross_len(self, rect: LogicalRect) -> i32 {
        match self {
            LayoutDirection::Row => rect.height,
            LayoutDirection::Column => rect.width,
        }
    }

    fn cross_origin_for_child(
        self,
        rect: LogicalRect,
        align: Option<AlignItems>,
        child_cross_size: i32,
    ) -> i32 {
        let align = align.unwrap_or(AlignItems::Stretch);
        let available = self.cross_len(rect);
        let remaining = (available - child_cross_size).max(0);

        match (self, align) {
            (LayoutDirection::Row, AlignItems::Center) => rect.y + remaining / 2,
            (LayoutDirection::Row, AlignItems::End) => rect.y + remaining,
            (LayoutDirection::Row, _) => rect.y,
            (LayoutDirection::Column, AlignItems::Center) => rect.x + remaining / 2,
            (LayoutDirection::Column, AlignItems::End) => rect.x + remaining,
            (LayoutDirection::Column, _) => rect.x,
        }
    }

    fn cross_origin_for_child_resolved(
        self,
        rect: ResolvedLogicalRect,
        align: Option<AlignItems>,
        child_cross_size: ResolvedLayoutValue,
    ) -> ResolvedLayoutValue {
        let align = align.unwrap_or(AlignItems::Stretch);
        let available = self.cross_len_resolved(rect);
        let remaining = ResolvedLayoutValue::from_raw((available.raw() - child_cross_size.raw()).max(0));

        match (self, align) {
            (LayoutDirection::Row, AlignItems::Center) => {
                rect.y + ResolvedLayoutValue::from_raw(remaining.raw() / 2)
            }
            (LayoutDirection::Row, AlignItems::End) => rect.y + remaining,
            (LayoutDirection::Row, _) => rect.y,
            (LayoutDirection::Column, AlignItems::Center) => {
                rect.x + ResolvedLayoutValue::from_raw(remaining.raw() / 2)
            }
            (LayoutDirection::Column, AlignItems::End) => rect.x + remaining,
            (LayoutDirection::Column, _) => rect.x,
        }
    }

    fn rect_resolved(
        self,
        main_origin: ResolvedLayoutValue,
        cross_origin: ResolvedLayoutValue,
        main_len: ResolvedLayoutValue,
        cross_len: ResolvedLayoutValue,
    ) -> ResolvedLogicalRect {
        match self {
            LayoutDirection::Row => ResolvedLogicalRect {
                x: main_origin,
                y: cross_origin,
                width: main_len,
                height: cross_len,
            },
            LayoutDirection::Column => ResolvedLogicalRect {
                x: cross_origin,
                y: main_origin,
                width: cross_len,
                height: main_len,
            },
        }
    }

    fn rect(self, main_origin: i32, cross_origin: i32, main_len: i32, cross_len: i32) -> LogicalRect {
        match self {
            LayoutDirection::Row => LogicalRect::new(main_origin, cross_origin, main_len, cross_len),
            LayoutDirection::Column => LogicalRect::new(cross_origin, main_origin, cross_len, main_len),
        }
    }
}

fn collect_render_primitives(
    node: &ComputedDecorationNode,
    primitives: &mut Vec<DecorationRenderPrimitive>,
) {
    if node.style.visible == Some(false) {
        return;
    }

    match &node.kind {
        DecorationNodeKind::Label(label) => primitives.push(DecorationRenderPrimitive::Label {
            rect: node.rect,
            text: label.text.clone(),
            color: node
                .style
                .color
                .unwrap_or(Color::WHITE)
                .with_opacity(node.style.opacity),
        }),
        DecorationNodeKind::AppIcon => {
            primitives.push(DecorationRenderPrimitive::AppIcon { rect: node.rect })
        }
        DecorationNodeKind::ShaderEffect(effect) => {
            primitives.push(DecorationRenderPrimitive::ShaderEffect {
                rect: node.rect,
                shader: effect.shader.clone(),
            })
        }
        DecorationNodeKind::WindowSlot => {
            primitives.push(DecorationRenderPrimitive::WindowSlot { rect: node.rect })
        }
        _ => {}
    }

    if let Some(border) = node.style.border {
        primitives.push(DecorationRenderPrimitive::BorderRect {
            rect: node.rect,
            width: border.width,
            color: border.color.with_opacity(node.style.opacity),
            radius: node.style.border_radius,
        });
    }

    for child in node.children.iter().rev() {
        collect_render_primitives(child, primitives);
    }

    if let Some(background) = node.style.background.map(|color| color.with_opacity(node.style.opacity)) {
        if matches!(node.kind, DecorationNodeKind::WindowBorder) {
            if let Some(slot_rect) = node.window_slot_rect() {
                push_fill_rect_with_hole(
                    primitives,
                    node.rect,
                    slot_rect,
                    background,
                    node.style.border_radius,
                );
            } else {
                primitives.push(DecorationRenderPrimitive::FillRect {
                    rect: node.rect,
                    color: background,
                    radius: node.style.border_radius,
                });
            }
        } else {
            primitives.push(DecorationRenderPrimitive::FillRect {
                rect: node.rect,
                color: background,
                radius: node.style.border_radius,
            });
        }
    }
}

fn push_fill_rect_with_hole(
    primitives: &mut Vec<DecorationRenderPrimitive>,
    rect: LogicalRect,
    hole: LogicalRect,
    color: Color,
    radius: Option<i32>,
) {
    let top_height = (hole.y - rect.y).max(0);
    let bottom_y = hole.y + hole.height;
    let bottom_height = (rect.y + rect.height - bottom_y).max(0);
    let left_width = (hole.x - rect.x).max(0);
    let right_x = hole.x + hole.width;
    let right_width = (rect.x + rect.width - right_x).max(0);

    let candidates = [
        LogicalRect::new(rect.x, rect.y, rect.width, top_height),
        LogicalRect::new(rect.x, bottom_y, rect.width, bottom_height),
        LogicalRect::new(rect.x, hole.y, left_width, hole.height),
        LogicalRect::new(right_x, hole.y, right_width, hole.height),
    ];

    for candidate in candidates {
        if candidate.width > 0 && candidate.height > 0 {
            primitives.push(DecorationRenderPrimitive::FillRect {
                rect: candidate,
                color,
                radius,
            });
        }
    }
}

fn find_button_action(
    node: &ComputedDecorationNode,
    point: LogicalPoint,
) -> Option<WindowAction> {
    for child in node.children.iter().rev() {
        if let Some(action) = find_button_action(child, point) {
            return Some(action);
        }
    }

    match &node.kind {
        DecorationNodeKind::Button(button) if node.rect.contains(point) => Some(button.action.clone()),
        _ => None,
    }
}

fn hit_test_resize_edges(
    rect: LogicalRect,
    border_width: i32,
    point: LogicalPoint,
) -> Option<ResizeEdges> {
    let border_width = border_width.max(0);
    if border_width == 0 || !rect.contains(point) {
        return None;
    }

    let on_left = point.x < rect.x + border_width;
    let on_right = point.x >= rect.x + rect.width - border_width;
    let on_top = point.y < rect.y + border_width;
    let on_bottom = point.y >= rect.y + rect.height - border_width;

    let mut edges = ResizeEdges::empty();
    if on_left {
        edges |= ResizeEdges::LEFT;
    }
    if on_right {
        edges |= ResizeEdges::RIGHT;
    }
    if on_top {
        edges |= ResizeEdges::TOP;
    }
    if on_bottom {
        edges |= ResizeEdges::BOTTOM;
    }

    (!edges.is_empty()).then_some(edges)
}

fn effective_clip_for_node(
    node: &DecorationNode,
    inherited_clip: Option<DecorationClip>,
    content_rect: LogicalRect,
) -> Option<DecorationClip> {
    effective_clip_for_node_resolved(
        node,
        inherited_clip.map(|clip| ResolvedDecorationClip {
            rect: ResolvedLogicalRect::from_logical(clip.rect),
            radius: ResolvedLayoutValue::from_i32(clip.radius),
        }),
        ResolvedLogicalRect::from_logical(content_rect),
        1.0,
    )
    .map(|clip| clip.round_to_logical_clip())
}

fn effective_clip_for_node_resolved(
    node: &DecorationNode,
    inherited_clip: Option<ResolvedDecorationClip>,
    content_rect: ResolvedLogicalRect,
    scale: f64,
) -> Option<ResolvedDecorationClip> {
    let node_clip = node.style.border.map(|border| ResolvedDecorationClip {
        rect: content_rect,
        radius: ResolvedLayoutValue::from_i32(node.style.border_radius.unwrap_or(0))
            - ResolvedLayoutValue::from_i32(border.width.max(0)).snap_edge(scale),
    });

    match (inherited_clip, node_clip) {
        (Some(parent), Some(current)) => intersect_resolved_decoration_clips(parent, current),
        (Some(parent), None) => Some(parent),
        (None, Some(current)) => Some(current),
        (None, None) => None,
    }
}

fn intersect_decoration_clips(
    left: DecorationClip,
    right: DecorationClip,
) -> Option<DecorationClip> {
    let x1 = left.rect.x.max(right.rect.x);
    let y1 = left.rect.y.max(right.rect.y);
    let x2 = (left.rect.x + left.rect.width).min(right.rect.x + right.rect.width);
    let y2 = (left.rect.y + left.rect.height).min(right.rect.y + right.rect.height);

    (x2 > x1 && y2 > y1).then_some(DecorationClip {
        rect: LogicalRect::new(x1, y1, x2 - x1, y2 - y1),
        radius: left.radius.min(right.radius),
    })
}

fn intersect_resolved_decoration_clips(
    left: ResolvedDecorationClip,
    right: ResolvedDecorationClip,
) -> Option<ResolvedDecorationClip> {
    let x1 = left.rect.x.max(right.rect.x);
    let y1 = left.rect.y.max(right.rect.y);
    let x2 = left.rect.right().min(right.rect.right());
    let y2 = left.rect.bottom().min(right.rect.bottom());

    (x2.raw() > x1.raw() && y2.raw() > y1.raw()).then_some(ResolvedDecorationClip {
        rect: ResolvedLogicalRect {
            x: x1,
            y: y1,
            width: ResolvedLayoutValue::from_raw(x2.raw() - x1.raw()),
            height: ResolvedLayoutValue::from_raw(y2.raw() - y1.raw()),
        },
        radius: left.radius.min(right.radius),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_tree() -> DecorationTree {
        DecorationTree::new(
            DecorationNode::new(DecorationNodeKind::WindowBorder).with_children(vec![
                DecorationNode::new(DecorationNodeKind::Box(BoxNode {
                    direction: LayoutDirection::Column,
                }))
                .with_children(vec![
                    DecorationNode::new(DecorationNodeKind::Label(LabelNode {
                        text: "Title".into(),
                    })),
                    DecorationNode::new(DecorationNodeKind::WindowSlot),
                ]),
            ]),
        )
    }

    #[test]
    fn valid_tree_has_single_window_slot() {
        let summary = sample_tree().validate().expect("tree should be valid");
        assert_eq!(summary.window_slot_count, 1);
    }

    #[test]
    fn tree_without_window_slot_is_rejected() {
        let tree = DecorationTree::new(DecorationNode::new(DecorationNodeKind::WindowBorder));
        assert_eq!(
            tree.validate(),
            Err(DecorationValidationError::MissingWindowSlot)
        );
    }

    #[test]
    fn tree_with_multiple_window_slots_is_rejected() {
        let tree = DecorationTree::new(
            DecorationNode::new(DecorationNodeKind::Box(BoxNode::default())).with_children(vec![
                DecorationNode::new(DecorationNodeKind::WindowSlot),
                DecorationNode::new(DecorationNodeKind::WindowSlot),
            ]),
        );

        assert_eq!(
            tree.validate(),
            Err(DecorationValidationError::MultipleWindowSlots { count: 2 })
        );
    }

    #[test]
    fn window_slot_must_not_have_children() {
        let tree = DecorationTree::new(
            DecorationNode::new(DecorationNodeKind::WindowSlot).with_children(vec![
                DecorationNode::new(DecorationNodeKind::Label(LabelNode {
                    text: "illegal".into(),
                })),
            ]),
        );

        assert_eq!(
            tree.validate(),
            Err(DecorationValidationError::WindowSlotHasChildren)
        );
    }

    #[test]
    fn window_border_insets_content_by_border_width() {
        let mut root = DecorationNode::new(DecorationNodeKind::WindowBorder);
        root.style.border = Some(BorderStyle {
            width: 2,
            color: Color::WHITE,
        });
        root.push_child(DecorationNode::new(DecorationNodeKind::WindowSlot));

        let layout = DecorationTree::new(root)
            .layout(LogicalRect::new(0, 0, 100, 50))
            .expect("layout should succeed");

        assert_eq!(layout.window_slot_rect(), Some(LogicalRect::new(2, 2, 96, 46)));
    }

    #[test]
    fn column_box_allocates_remaining_space_to_window_slot() {
        let titlebar = DecorationNode::new(DecorationNodeKind::Box(BoxNode {
            direction: LayoutDirection::Row,
        }))
        .with_style(DecorationStyle {
            height: Some(28),
            ..Default::default()
        });

        let root = DecorationNode::new(DecorationNodeKind::Box(BoxNode {
            direction: LayoutDirection::Column,
        }))
        .with_children(vec![titlebar, DecorationNode::new(DecorationNodeKind::WindowSlot)]);

        let layout = DecorationTree::new(root)
            .layout(LogicalRect::new(0, 0, 800, 600))
            .expect("layout should succeed");

        let slot = layout.window_slot_rect().expect("slot must exist");
        assert_eq!(slot, LogicalRect::new(0, 28, 800, 572));
    }

    #[test]
    fn row_box_distributes_remaining_space_to_flex_child() {
        let left = DecorationNode::new(DecorationNodeKind::Label(LabelNode {
            text: "title".into(),
        }))
        .with_style(DecorationStyle {
            width: Some(100),
            ..Default::default()
        });
        let spacer = DecorationNode::new(DecorationNodeKind::Box(BoxNode {
            direction: LayoutDirection::Row,
        }))
        .with_style(DecorationStyle {
            flex_grow: Some(1.0),
            ..Default::default()
        });
        let right = DecorationNode::new(DecorationNodeKind::Button(ButtonNode {
            action: WindowAction::Close,
        }))
        .with_style(DecorationStyle {
            width: Some(20),
            ..Default::default()
        });

        let root = DecorationNode::new(DecorationNodeKind::Box(BoxNode {
            direction: LayoutDirection::Row,
        }))
        .with_style(DecorationStyle {
            gap: Some(4),
            ..Default::default()
        })
        .with_children(vec![left, spacer, right, DecorationNode::new(DecorationNodeKind::WindowSlot)]);

        let layout = DecorationTree::new(root)
            .layout(LogicalRect::new(0, 0, 300, 30))
            .expect("layout should succeed");

        let spacer_rect = &layout.root.children[1].rect;
        let right_rect = &layout.root.children[2].rect;

        assert_eq!(*spacer_rect, LogicalRect::new(104, 0, 84, 30));
        assert_eq!(*right_rect, LogicalRect::new(192, 0, 20, 30));
    }

    #[test]
    fn shader_effect_root_preserves_child_window_border_auto_size() {
        let titlebar = DecorationNode::new(DecorationNodeKind::Box(BoxNode {
            direction: LayoutDirection::Row,
        }))
        .with_style(DecorationStyle {
            height: Some(30),
            ..Default::default()
        })
        .with_children(vec![
            DecorationNode::new(DecorationNodeKind::Label(LabelNode {
                text: "Title".into(),
            })),
            DecorationNode::new(DecorationNodeKind::WindowSlot),
        ]);

        let bordered = DecorationNode::new(DecorationNodeKind::WindowBorder)
            .with_style(DecorationStyle {
                border: Some(BorderStyle {
                    width: 2,
                    color: Color::WHITE,
                }),
                background: Some(Color::BLACK),
                ..Default::default()
            })
            .with_children(vec![
                DecorationNode::new(DecorationNodeKind::Box(BoxNode {
                    direction: LayoutDirection::Column,
                }))
                .with_children(vec![titlebar]),
            ]);

        let root = DecorationNode::new(DecorationNodeKind::ShaderEffect(ShaderEffectNode {
            direction: LayoutDirection::Column,
            shader: CompiledEffect {
                input: EffectInput::Backdrop,
                invalidate: EffectInvalidationPolicy::Always,
                pipeline: Vec::new(),
            },
        }))
        .with_style(DecorationStyle {
            padding: Edges {
                top: 6,
                right: 6,
                bottom: 6,
                left: 6,
            },
            ..Default::default()
        })
        .with_children(vec![bordered]);

        let layout = DecorationTree::new(root)
            .layout(LogicalRect::new(0, 0, 300, 200))
            .expect("layout should succeed");

        let border_rect = layout.root.children[0].rect;
        assert!(border_rect.height > 0);
        assert!(border_rect.width > 0);
    }

    #[test]
    fn row_child_in_column_stretches_on_cross_axis_by_default() {
        let titlebar = DecorationNode::new(DecorationNodeKind::Box(BoxNode {
            direction: LayoutDirection::Row,
        }))
        .with_style(DecorationStyle {
            height: Some(30),
            ..Default::default()
        })
        .with_children(vec![DecorationNode::new(DecorationNodeKind::Label(LabelNode {
            text: "Title".into(),
        }))]);

        let root = DecorationNode::new(DecorationNodeKind::Box(BoxNode {
            direction: LayoutDirection::Column,
        }))
        .with_children(vec![titlebar, DecorationNode::new(DecorationNodeKind::WindowSlot)]);

        let layout = DecorationTree::new(root)
            .layout_for_client(LogicalRect::new(50, 100, 800, 600))
            .expect("layout should succeed");

        let titlebar_rect = layout.root.children[0].rect;
        assert_eq!(titlebar_rect.width, 800);
        assert_eq!(titlebar_rect.height, 30);
    }

    #[test]
    fn child_align_items_does_not_override_parent_cross_axis_stretch() {
        let titlebar = DecorationNode::new(DecorationNodeKind::Box(BoxNode {
            direction: LayoutDirection::Row,
        }))
        .with_style(DecorationStyle {
            height: Some(30),
            align_items: Some(AlignItems::Center),
            ..Default::default()
        })
        .with_children(vec![DecorationNode::new(DecorationNodeKind::Label(LabelNode {
            text: "Title".into(),
        }))]);

        let root = DecorationNode::new(DecorationNodeKind::Box(BoxNode {
            direction: LayoutDirection::Column,
        }))
        .with_children(vec![titlebar, DecorationNode::new(DecorationNodeKind::WindowSlot)]);

        let layout = DecorationTree::new(root)
            .layout_for_client(LogicalRect::new(50, 100, 800, 600))
            .expect("layout should succeed");

        let titlebar_rect = layout.root.children[0].rect;
        assert_eq!(titlebar_rect.width, 800);
        assert_eq!(titlebar_rect.height, 30);
    }

    #[test]
    fn computed_bounds_include_overflowing_children() {
        let child = DecorationNode::new(DecorationNodeKind::WindowBorder)
            .with_style(DecorationStyle {
                border: Some(BorderStyle {
                    width: 2,
                    color: Color::WHITE,
                }),
                background: Some(Color::BLACK),
                ..Default::default()
            })
            .with_children(vec![DecorationNode::new(DecorationNodeKind::WindowSlot)]);

        let root = DecorationNode::new(DecorationNodeKind::ShaderEffect(ShaderEffectNode {
            direction: LayoutDirection::Column,
            shader: CompiledEffect {
                input: EffectInput::Backdrop,
                invalidate: EffectInvalidationPolicy::Always,
                pipeline: Vec::new(),
            },
        }))
        .with_style(DecorationStyle {
            padding: Edges {
                top: 6,
                right: 6,
                bottom: 6,
                left: 6,
            },
            ..Default::default()
        })
        .with_children(vec![child]);

        let layout = DecorationTree::new(root)
            .layout_for_client(LogicalRect::new(50, 100, 800, 600))
            .expect("layout should succeed");

        let bounds = layout.bounds_rect();
        let slot = layout.window_slot_rect().expect("slot should exist");
        assert!(bounds.x <= slot.x);
        assert!(bounds.y <= slot.y);
        assert!(bounds.x + bounds.width >= slot.x + slot.width);
        assert!(bounds.y + bounds.height >= slot.y + slot.height);
    }

    #[test]
    fn render_primitives_include_border_background_label_and_slot() {
        let title = DecorationNode::new(DecorationNodeKind::Label(LabelNode {
            text: "Shoji".into(),
        }))
        .with_style(DecorationStyle {
            height: Some(24),
            color: Some(Color::BLACK),
            ..Default::default()
        });

        let root = DecorationNode::new(DecorationNodeKind::WindowBorder)
            .with_style(DecorationStyle {
                background: Some(Color::WHITE),
                border: Some(BorderStyle {
                    width: 2,
                    color: Color::BLACK,
                }),
                ..Default::default()
            })
            .with_children(vec![
                DecorationNode::new(DecorationNodeKind::Box(BoxNode {
                    direction: LayoutDirection::Column,
                }))
                .with_children(vec![title, DecorationNode::new(DecorationNodeKind::WindowSlot)]),
            ]);

        let layout = DecorationTree::new(root)
            .layout(LogicalRect::new(0, 0, 100, 40))
            .expect("layout should succeed");

        let primitives = layout.render_primitives();

        assert!(primitives.iter().any(|primitive| matches!(
            primitive,
            DecorationRenderPrimitive::FillRect { rect, color, .. }
                if *rect == LogicalRect::new(0, 0, 100, 26) && *color == Color::WHITE
        )));
        assert!(primitives.iter().any(|primitive| matches!(
            primitive,
            DecorationRenderPrimitive::BorderRect { rect, width, color, .. }
                if *rect == LogicalRect::new(0, 0, 100, 40) && *width == 2 && *color == Color::BLACK
        )));
        assert!(primitives.iter().any(|primitive| matches!(
            primitive,
            DecorationRenderPrimitive::Label { text, .. } if text == "Shoji"
        )));
        assert!(primitives.iter().any(|primitive| matches!(
            primitive,
            DecorationRenderPrimitive::WindowSlot { rect } if *rect == LogicalRect::new(2, 26, 96, 12)
        )));
    }

    #[test]
    fn render_primitives_are_ordered_front_to_back_for_smithay_rendering() {
        let root = DecorationNode::new(DecorationNodeKind::Box(BoxNode {
            direction: LayoutDirection::Column,
        }))
        .with_style(DecorationStyle {
            background: Some(Color::WHITE),
            border: Some(BorderStyle {
                width: 1,
                color: Color::BLACK,
            }),
            ..Default::default()
        })
        .with_children(vec![
            DecorationNode::new(DecorationNodeKind::Box(BoxNode {
                direction: LayoutDirection::Column,
            }))
            .with_style(DecorationStyle {
                height: Some(4),
                background: Some(Color::rgba(255, 0, 0, 255)),
                ..Default::default()
            }),
            DecorationNode::new(DecorationNodeKind::WindowSlot),
        ]);

        let layout = DecorationTree::new(root)
            .layout(LogicalRect::new(0, 0, 10, 10))
            .expect("layout should succeed");

        let primitives = layout.render_primitives();

        let root_border_index = primitives
            .iter()
            .position(|primitive| matches!(
                primitive,
                DecorationRenderPrimitive::BorderRect { rect, width, color, .. }
                    if *rect == LogicalRect::new(0, 0, 10, 10) && *width == 1 && *color == Color::BLACK
            ))
            .expect("root border should exist");
        let child_background_index = primitives
            .iter()
            .position(|primitive| matches!(
                primitive,
                DecorationRenderPrimitive::FillRect { rect, color, .. }
                    if *rect == LogicalRect::new(1, 1, 8, 4) && *color == Color::rgba(255, 0, 0, 255)
            ))
            .expect("child background should exist");
        let root_background_index = primitives
            .iter()
            .position(|primitive| matches!(
                primitive,
                DecorationRenderPrimitive::FillRect { rect, color, .. }
                    if *rect == LogicalRect::new(0, 0, 10, 10) && *color == Color::WHITE
            ))
            .expect("root background should exist");

        assert!(root_border_index < child_background_index);
        assert!(child_background_index < root_background_index);
    }

    #[test]
    fn render_primitives_apply_opacity_to_colors() {
        let root = DecorationNode::new(DecorationNodeKind::WindowBorder).with_style(DecorationStyle {
            background: Some(Color::rgba(255, 0, 0, 255)),
            opacity: Some(0.5),
            ..Default::default()
        });

        let layout = DecorationTree::new(root)
            .layout(LogicalRect::new(0, 0, 10, 10))
            .expect_err("layout should fail without window slot");
        assert_eq!(layout, DecorationLayoutError::Validation(DecorationValidationError::MissingWindowSlot));

        let root = DecorationNode::new(DecorationNodeKind::WindowBorder)
            .with_style(DecorationStyle {
                background: Some(Color::rgba(255, 0, 0, 255)),
                opacity: Some(0.5),
                ..Default::default()
            })
            .with_children(vec![DecorationNode::new(DecorationNodeKind::WindowSlot)]);

        let layout = DecorationTree::new(root)
            .layout(LogicalRect::new(0, 0, 10, 10))
            .expect("layout should succeed");

        let primitives = layout.render_primitives();
        assert!(primitives.iter().all(|primitive| !matches!(
            primitive,
            DecorationRenderPrimitive::FillRect { .. }
        )));
        assert!(primitives.iter().any(|primitive| matches!(
            primitive,
            DecorationRenderPrimitive::WindowSlot { rect } if *rect == LogicalRect::new(0, 0, 10, 10)
        )));
    }

    #[test]
    fn invisible_subtree_emits_no_primitives() {
        let root = DecorationNode::new(DecorationNodeKind::WindowBorder).with_children(vec![
            DecorationNode::new(DecorationNodeKind::Box(BoxNode::default())).with_style(DecorationStyle {
                visible: Some(false),
                background: Some(Color::WHITE),
                ..Default::default()
            })
            .with_children(vec![DecorationNode::new(DecorationNodeKind::WindowSlot)]),
        ]);

        let layout = DecorationTree::new(root)
            .layout(LogicalRect::new(0, 0, 10, 10))
            .expect("layout should succeed");

        assert!(layout.render_primitives().is_empty());
    }

    #[test]
    fn hit_test_returns_button_action_before_move() {
        let root = DecorationNode::new(DecorationNodeKind::Box(BoxNode {
            direction: LayoutDirection::Row,
        }))
        .with_children(vec![
            DecorationNode::new(DecorationNodeKind::Button(ButtonNode {
                action: WindowAction::Close,
            }))
            .with_style(DecorationStyle {
                width: Some(20),
                ..Default::default()
            }),
            DecorationNode::new(DecorationNodeKind::WindowSlot),
        ]);

        let layout = DecorationTree::new(root)
            .layout(LogicalRect::new(0, 0, 100, 30))
            .expect("layout should succeed");

        assert_eq!(
            layout.hit_test(LogicalPoint::new(10, 10)),
            DecorationHitTestResult::Action(WindowAction::Close)
        );
    }

    #[test]
    fn hit_test_returns_client_area_inside_window_slot() {
        let root = DecorationNode::new(DecorationNodeKind::Box(BoxNode {
            direction: LayoutDirection::Column,
        }))
        .with_children(vec![
            DecorationNode::new(DecorationNodeKind::Box(BoxNode::default())).with_style(
                DecorationStyle {
                    height: Some(20),
                    ..Default::default()
                },
            ),
            DecorationNode::new(DecorationNodeKind::WindowSlot),
        ]);

        let layout = DecorationTree::new(root)
            .layout(LogicalRect::new(0, 0, 100, 60))
            .expect("layout should succeed");

        assert_eq!(
            layout.hit_test(LogicalPoint::new(10, 30)),
            DecorationHitTestResult::ClientArea
        );
    }

    #[test]
    fn hit_test_returns_move_on_titlebar_area() {
        let root = DecorationNode::new(DecorationNodeKind::Box(BoxNode {
            direction: LayoutDirection::Column,
        }))
        .with_children(vec![
            DecorationNode::new(DecorationNodeKind::Label(LabelNode {
                text: "title".into(),
            }))
            .with_style(DecorationStyle {
                height: Some(20),
                ..Default::default()
            }),
            DecorationNode::new(DecorationNodeKind::WindowSlot),
        ]);

        let layout = DecorationTree::new(root)
            .layout(LogicalRect::new(0, 0, 100, 60))
            .expect("layout should succeed");

        assert_eq!(
            layout.hit_test(LogicalPoint::new(10, 10)),
            DecorationHitTestResult::Move
        );
    }

    #[test]
    fn hit_test_returns_resize_on_window_border() {
        let root = DecorationNode::new(DecorationNodeKind::WindowBorder)
            .with_style(DecorationStyle {
                border: Some(BorderStyle {
                    width: 4,
                    color: Color::WHITE,
                }),
                ..Default::default()
            })
            .with_children(vec![DecorationNode::new(DecorationNodeKind::WindowSlot)]);

        let layout = DecorationTree::new(root)
            .layout(LogicalRect::new(0, 0, 100, 60))
            .expect("layout should succeed");

        assert_eq!(
            layout.hit_test(LogicalPoint::new(1, 1)),
            DecorationHitTestResult::Resize(ResizeEdges::TOP_LEFT)
        );
        assert_eq!(
            layout.hit_test(LogicalPoint::new(50, 1)),
            DecorationHitTestResult::Resize(ResizeEdges::TOP)
        );
    }

    #[test]
    fn resolved_layout_snaps_size_from_edges() {
        let rect = ResolvedLogicalRect {
            x: ResolvedLayoutValue::from_i32(1953),
            y: ResolvedLayoutValue::from_i32(82),
            width: ResolvedLayoutValue::from_i32(1512),
            height: ResolvedLayoutValue::from_i32(906),
        };

        let (snapped_width, snapped_height) = rect.snapped_size(1.6, 1.6);
        assert_eq!(snapped_width.round_to_i32(), 1512);
        assert_eq!(snapped_height.round_to_i32(), 906);
        assert_eq!(
            ((((rect.right().to_f32() as f64) * 1.6).round()
                - ((rect.left().to_f32() as f64) * 1.6).round()) as i32),
            2419
        );
    }

    #[test]
    fn resolved_layout_inset_preserves_subpixel_border_width() {
        let rect = ResolvedLogicalRect::from_logical(LogicalRect::new(0, 0, 18, 18));
        let border = ResolvedLayoutValue::from_f32(1.875);
        let inset = rect.inset(ResolvedLayoutEdges {
            top: border,
            right: border,
            bottom: border,
            left: border,
        });

        assert_eq!(inset.x.to_f32(), 1.875);
        assert_eq!(inset.width.to_f32(), 14.25);
    }

    #[test]
    fn layout_preserves_subpixel_child_offsets_at_fractional_scale() {
        let root = DecorationNode::new(DecorationNodeKind::WindowBorder)
            .with_style(DecorationStyle {
                border: Some(BorderStyle {
                    width: 1,
                    color: Color::WHITE,
                }),
                padding: Edges {
                    top: 4,
                    right: 4,
                    bottom: 4,
                    left: 4,
                },
                ..Default::default()
            })
            .with_children(vec![
                DecorationNode::new(DecorationNodeKind::Box(BoxNode {
                    direction: LayoutDirection::Row,
                }))
                .with_style(DecorationStyle {
                    gap: Some(3),
                    ..Default::default()
                })
                .with_children(vec![
                    DecorationNode::new(DecorationNodeKind::Label(LabelNode {
                        text: "A".into(),
                    }))
                    .with_style(DecorationStyle {
                        width: Some(11),
                        height: Some(20),
                        ..Default::default()
                    }),
                    DecorationNode::new(DecorationNodeKind::AppIcon).with_style(DecorationStyle {
                        width: Some(11),
                        height: Some(20),
                        ..Default::default()
                    }),
                    DecorationNode::new(DecorationNodeKind::WindowSlot),
                ]),
            ]);

        let layout = DecorationTree::new(root)
            .layout_for_client_with_scale(LogicalRect::new(50, 40, 200, 120), 1.6)
            .expect("layout should succeed");

        let row = &layout.root.children[0];
        let label = &row.children[0];
        let icon = &row.children[1];

        assert_eq!(
            (icon.resolved_rect.x - label.resolved_rect.x - label.resolved_rect.width).to_f32(),
            3.125
        );
    }

    #[test]
    fn stretched_column_shrinks_auto_child_to_fit_fractional_height() {
        let top_border = DecorationNode::new(DecorationNodeKind::Box(BoxNode {
            direction: LayoutDirection::Row,
        }))
        .with_style(DecorationStyle {
            height: Some(2),
            background: Some(Color::BLACK),
            ..Default::default()
        });

        let bottom_border = DecorationNode::new(DecorationNodeKind::Box(BoxNode {
            direction: LayoutDirection::Row,
        }))
        .with_style(DecorationStyle {
            height: Some(2),
            background: Some(Color::BLACK),
            ..Default::default()
        });

        let middle_column = DecorationNode::new(DecorationNodeKind::Box(BoxNode {
            direction: LayoutDirection::Column,
        }))
        .with_children(vec![
            DecorationNode::new(DecorationNodeKind::Box(BoxNode {
                direction: LayoutDirection::Row,
            }))
            .with_style(DecorationStyle {
                height: Some(30),
                ..Default::default()
            }),
            DecorationNode::new(DecorationNodeKind::Box(BoxNode {
                direction: LayoutDirection::Row,
            }))
            .with_style(DecorationStyle {
                height: Some(30),
                ..Default::default()
            }),
            DecorationNode::new(DecorationNodeKind::WindowSlot),
        ]);

        let anchor_column = DecorationNode::new(DecorationNodeKind::Box(BoxNode {
            direction: LayoutDirection::Column,
        }))
        .with_children(vec![top_border, middle_column, bottom_border]);

        let root = DecorationNode::new(DecorationNodeKind::WindowBorder)
            .with_style(DecorationStyle {
                border: Some(BorderStyle {
                    width: 2,
                    color: Color::WHITE,
                }),
                border_radius: Some(20),
                ..Default::default()
            })
            .with_children(vec![
                DecorationNode::new(DecorationNodeKind::Box(BoxNode {
                    direction: LayoutDirection::Row,
                }))
                .with_children(vec![
                    DecorationNode::new(DecorationNodeKind::Box(BoxNode {
                        direction: LayoutDirection::Row,
                    }))
                    .with_style(DecorationStyle {
                        width: Some(2),
                        ..Default::default()
                    }),
                    anchor_column,
                    DecorationNode::new(DecorationNodeKind::Box(BoxNode {
                        direction: LayoutDirection::Row,
                    }))
                    .with_style(DecorationStyle {
                        width: Some(2),
                        ..Default::default()
                    }),
                ]),
            ]);

        let layout = DecorationTree::new(root)
            .layout_for_client_with_scale(LogicalRect::new(82, 39, 1512, 906), 1.25)
            .expect("layout should succeed");

        let stretched_column = &layout.root.children[0].children[1];
        let top = &stretched_column.children[0];
        let middle = &stretched_column.children[1];
        let bottom = &stretched_column.children[2];

        assert_eq!(top.resolved_rect.y, stretched_column.resolved_rect.y);
        assert_eq!(
            bottom.resolved_rect.bottom(),
            stretched_column.resolved_rect.bottom()
        );
        assert_eq!(
            top.resolved_rect.height.raw()
                + middle.resolved_rect.height.raw()
                + bottom.resolved_rect.height.raw(),
            stretched_column.resolved_rect.height.raw()
        );
    }

    #[test]
    fn reapply_preserves_subpixel_offsets_at_fractional_scale() {
        let original = DecorationNode::new(DecorationNodeKind::Box(BoxNode {
            direction: LayoutDirection::Row,
        }))
        .with_style(DecorationStyle {
            gap: Some(3),
            ..Default::default()
        })
        .with_children(vec![
            DecorationNode::new(DecorationNodeKind::Label(LabelNode {
                text: "A".into(),
            }))
            .with_style(DecorationStyle {
                width: Some(11),
                height: Some(20),
                color: Some(Color::WHITE),
                ..Default::default()
            }),
            DecorationNode::new(DecorationNodeKind::AppIcon).with_style(DecorationStyle {
                width: Some(11),
                height: Some(20),
                ..Default::default()
            }),
            DecorationNode::new(DecorationNodeKind::WindowSlot),
        ]);

        let updated = DecorationNode::new(DecorationNodeKind::Box(BoxNode {
            direction: LayoutDirection::Row,
        }))
        .with_style(DecorationStyle {
            gap: Some(3),
            background: Some(Color::BLACK),
            ..Default::default()
        })
        .with_children(vec![
            DecorationNode::new(DecorationNodeKind::Label(LabelNode {
                text: "A".into(),
            }))
            .with_style(DecorationStyle {
                width: Some(11),
                height: Some(20),
                color: Some(Color::BLACK),
                ..Default::default()
            }),
            DecorationNode::new(DecorationNodeKind::AppIcon).with_style(DecorationStyle {
                width: Some(11),
                height: Some(20),
                ..Default::default()
            }),
            DecorationNode::new(DecorationNodeKind::WindowSlot),
        ]);

        let mut layout = DecorationTree::new(original)
            .layout_for_client_with_scale(LogicalRect::new(50, 40, 200, 120), 1.6)
            .expect("layout should succeed");
        reapply_tree_preserving_layout(&mut layout.root, &updated, None, 1.6);

        let label = &layout.root.children[0];
        let icon = &layout.root.children[1];
        assert_eq!(
            (icon.resolved_rect.x - label.resolved_rect.x - label.resolved_rect.width).to_f32(),
            3.125
        );
    }

    #[test]
    fn explicit_fixed_size_snaps_to_scale_quantum() {
        let tree = DecorationTree::new(
            DecorationNode::new(DecorationNodeKind::Box(BoxNode {
                direction: LayoutDirection::Row,
            }))
            .with_children(vec![
                DecorationNode::new(DecorationNodeKind::Button(ButtonNode {
                    action: WindowAction::Close,
                }))
                .with_style(DecorationStyle {
                    width: Some(18),
                    height: Some(18),
                    ..Default::default()
                }),
                DecorationNode::new(DecorationNodeKind::WindowSlot),
            ]),
        );

        let layout = tree
            .layout_for_client_with_scale(LogicalRect::new(0, 0, 100, 60), 1.6)
            .expect("layout should succeed");
        let button = &layout.root.children[0];

        assert_eq!(button.resolved_rect.width.to_f32(), 18.125);
        assert_eq!(button.resolved_rect.height.to_f32(), 18.125);
    }
}
