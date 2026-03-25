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
    WireWindowAction, WireBackgroundEffectConfig, decode_tree_json,
};
pub use evaluator::{
    DecorationEvaluationError, DecorationEvaluationResult, DecorationEvaluator,
    DecorationHandlerInvocation, DecorationSchedulerTick, NodeDecorationEvaluator,
    RuntimeWindowAction, StaticDecorationEvaluator, evaluate_dynamic_decoration,
};
pub use interaction::DecorationInteractionSnapshot;
pub use integration::{
    CachedDecorationBuffer, ContentClip, DecorationRuntimeEvaluator, WindowDecorationState,
};
pub use window_model::{
    TransformOrigin, WaylandWindowAction, WaylandWindowSnapshot, WindowIconSnapshot,
    WindowPositionSnapshot, WindowTransform,
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
        self.validate()?;

        let root = layout_node(&self.root, bounds, None)?;
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
    pub kind: DecorationNodeKind,
    pub style: DecorationStyle,
    pub rect: LogicalRect,
    pub effective_clip: Option<DecorationClip>,
    pub children: Vec<ComputedDecorationNode>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecorationClip {
    pub rect: LogicalRect,
    pub radius: i32,
}

impl ComputedDecorationNode {
    pub fn window_slot_rect(&self) -> Option<LogicalRect> {
        if matches!(self.kind, DecorationNodeKind::WindowSlot) {
            return Some(self.rect);
        }

        self.children.iter().find_map(Self::window_slot_rect)
    }

    fn window_border_style(&self) -> Option<BorderStyle> {
        if matches!(self.kind, DecorationNodeKind::WindowBorder) {
            return self.style.border;
        }

        self.children.iter().find_map(Self::window_border_style)
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
    pub kind: DecorationNodeKind,
    pub style: DecorationStyle,
    pub children: Vec<DecorationNode>,
}

impl DecorationNode {
    pub fn new(kind: DecorationNodeKind) -> Self {
        Self {
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

fn layout_node(
    node: &DecorationNode,
    rect: LogicalRect,
    inherited_clip: Option<DecorationClip>,
) -> Result<ComputedDecorationNode, DecorationLayoutError> {
    let content_rect = rect.inset(node.style.content_inset());
    let effective_clip = effective_clip_for_node(node, inherited_clip, content_rect);

    let children = match &node.kind {
        DecorationNodeKind::Box(layout) => {
            layout_box_children(node, content_rect, layout.direction, effective_clip)?
        }
        DecorationNodeKind::ShaderEffect(effect) => {
            layout_box_children(node, content_rect, effect.direction, effective_clip)?
        }
        _ if node.children.is_empty() => Vec::new(),
        _ => node
            .children
            .iter()
            .map(|child| layout_node(child, content_rect, effective_clip))
            .collect::<Result<Vec<_>, _>>()?,
    };

    Ok(ComputedDecorationNode {
        kind: node.kind.clone(),
        style: node.style.clone(),
        rect,
        effective_clip,
        children,
    })
}

fn layout_box_children(
    node: &DecorationNode,
    content_rect: LogicalRect,
    direction: LayoutDirection,
    effective_clip: Option<DecorationClip>,
) -> Result<Vec<ComputedDecorationNode>, DecorationLayoutError> {
    if node.children.is_empty() {
        return Ok(Vec::new());
    }

    let gap = node.style.gap.unwrap_or(0).max(0);
    let main_available = direction.main_len(content_rect);
    let cross_available = direction.cross_len(content_rect);
    let total_gap = gap * (node.children.len().saturating_sub(1) as i32);

    let mut base_sizes = Vec::with_capacity(node.children.len());
    let mut flexes = Vec::with_capacity(node.children.len());

    let mut base_sum = 0;
    let mut total_flex = 0.0f32;

    for child in &node.children {
        let base = child.preferred_main_size(direction).max(0);
        let flex = child.flex_grow_for_layout();
        base_sizes.push(base);
        flexes.push(flex);
        base_sum += base;
        total_flex += flex;
    }

    let remaining = (main_available - total_gap - base_sum).max(0);
    let mut allocated = base_sizes;

    if remaining > 0 && total_flex > 0.0 {
        let mut distributed = 0;
        let mut flex_indices = flexes
            .iter()
            .enumerate()
            .filter_map(|(idx, flex)| (*flex > 0.0).then_some(idx))
            .peekable();

        while let Some(idx) = flex_indices.next() {
            let share = if flex_indices.peek().is_none() {
                remaining - distributed
            } else {
                ((remaining as f32) * (flexes[idx] / total_flex)).round() as i32
            };
            allocated[idx] += share.max(0);
            distributed += share.max(0);
        }
    }

    let mut cursor = direction.main_origin(content_rect);
    let mut children = Vec::with_capacity(node.children.len());

    for (child, main_size) in node.children.iter().zip(allocated.into_iter()) {
        let cross_size = child.preferred_cross_size(direction, cross_available);
        let cross_origin = direction.cross_origin_for_child(
            content_rect,
            child.style.align_items.or(node.style.align_items),
            cross_size,
        );

        let child_rect = direction.rect(cursor, cross_origin, main_size, cross_size);
        children.push(layout_node(child, child_rect, effective_clip)?);
        cursor += main_size + gap;
    }

    Ok(children)
}

impl DecorationNode {
    fn preferred_main_size(&self, direction: LayoutDirection) -> i32 {
        let explicit = match direction {
            LayoutDirection::Row => self.style.width,
            LayoutDirection::Column => self.style.height,
        };

        let fallback = explicit.unwrap_or_else(|| {
            self.intrinsic_size()
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

    fn preferred_cross_size(&self, direction: LayoutDirection, available_cross: i32) -> i32 {
        let explicit = match direction {
            LayoutDirection::Row => self.style.height,
            LayoutDirection::Column => self.style.width,
        };

        let fallback = explicit.unwrap_or_else(|| {
            self.intrinsic_size()
                .map(|(width, height)| match direction {
                    LayoutDirection::Row => height,
                    LayoutDirection::Column => width,
                })
                .unwrap_or_else(|| match self.style.align_items.unwrap_or(AlignItems::Stretch) {
                    AlignItems::Stretch => available_cross,
                    _ => available_cross,
                })
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

    fn intrinsic_size(&self) -> Option<(i32, i32)> {
        match &self.kind {
            DecorationNodeKind::Label(label) => {
                let font_size = self.style.font_size.unwrap_or(13).max(1);
                let line_height = self.style.line_height.unwrap_or(font_size + 4).max(font_size);
                let spec = LabelSpec {
                    rect: LogicalRect::new(0, 0, 0, 0),
                    text: label.text.clone(),
                    color: self.style.color.unwrap_or(Color::WHITE).with_opacity(self.style.opacity),
                    font_size,
                    font_weight: self.style.font_weight.clone(),
                    font_family: self.style.font_family.clone(),
                    text_align: self.style.text_align.clone(),
                    line_height: Some(line_height),
                };
                Some(measure_label_intrinsic(&spec))
            }
            _ => None,
        }
    }
}

impl DecorationStyle {
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

    fn clamp_cross(&self, direction: LayoutDirection, value: i32) -> i32 {
        match direction {
            LayoutDirection::Row => clamp_size(value, self.min_height, self.max_height),
            LayoutDirection::Column => clamp_size(value, self.min_width, self.max_width),
        }
    }
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

impl LayoutDirection {
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
    let node_clip = node.style.border.map(|border| DecorationClip {
        rect: content_rect,
        radius: (node.style.border_radius.unwrap_or(0) - border.width.max(0)).max(0),
    });

    match (inherited_clip, node_clip) {
        (Some(parent), Some(current)) => intersect_decoration_clips(parent, current),
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
}
