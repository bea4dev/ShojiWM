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
mod integration;
mod interaction;
mod window_model;

use smithay::utils::Logical;

use crate::backend::text::{LabelSpec, measure_label_intrinsic};

pub use bridge::{
    DecorationBridgeError, WireCompiledEffect, WireDecorationChild, WireDecorationNode, WireProps,
    WireStyle, WireWindowAction, decode_tree_json,
};
pub use evaluator::{
    DecorationEvaluationError, DecorationEvaluationResult, DecorationEvaluator,
    DecorationHandlerInvocation, DecorationKeyBindingInvocation, DecorationSchedulerTick,
    LayerEffectEvaluationResult, NodeDecorationEvaluator, RuntimeLayerEffectAssignment,
    RuntimeWindowAction, StaticDecorationEvaluator, evaluate_dynamic_decoration,
};
pub use integration::{
    CachedDecorationBuffer, ContentClip, DecorationRuntimeEvaluator, WindowDecorationState,
};
pub use interaction::DecorationInteractionSnapshot;
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

    pub fn interaction_target_at(
        &self,
        point: LogicalPoint,
    ) -> Option<DecorationInteractionTarget> {
        find_interaction_target(&self.root, point)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ComputedDecorationNode {
    pub stable_id: Option<String>,
    pub interaction: DecorationInteractionHandlers,
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

        if !self.children.is_empty() && !matches!(self.style.overflow, Some(Overflow::Hidden)) {
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

    pub(crate) fn resolved_layout_bounds_rect(&self) -> ResolvedLogicalRect {
        let mut min_x = self.resolved_rect.x;
        let mut min_y = self.resolved_rect.y;
        let mut max_x = self.resolved_rect.x + self.resolved_rect.width;
        let mut max_y = self.resolved_rect.y + self.resolved_rect.height;

        let mut has_flow_child = false;
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

        for child in self
            .children
            .iter()
            .filter(|child| !child.style.is_absolute_positioned())
        {
            has_flow_child = true;
            let child_bounds = child.resolved_layout_bounds_rect();
            child_min_x = child_min_x.min(child_bounds.x);
            child_min_y = child_min_y.min(child_bounds.y);
            child_max_x = child_max_x.max(child_bounds.x + child_bounds.width);
            child_max_y = child_max_y.max(child_bounds.y + child_bounds.height);
        }

        if has_flow_child {
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

    pub(crate) fn sync_root_bounds(&mut self, scale: f64) {
        self.resolved_rect = self.resolved_layout_bounds_rect();
        self.rect = self.resolved_rect.round_to_logical_rect();
        self.resolved_content_rect = self
            .resolved_rect
            .inset(self.style.resolved_content_inset(scale));
        self.resolved_effective_clip = effective_clip_for_node_resolved(
            &self.to_decoration_node(),
            None,
            self.resolved_content_rect,
            scale,
        );
        self.effective_clip = self
            .resolved_effective_clip
            .map(ResolvedDecorationClip::round_to_logical_clip);
    }

    fn to_decoration_node(&self) -> DecorationNode {
        DecorationNode {
            stable_id: self.stable_id.clone(),
            interaction: self.interaction.clone(),
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
    Image {
        rect: LogicalRect,
        src: String,
        fit: ImageFit,
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

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DecorationInteractionHandlers {
    pub hover_change: Option<DecorationStateChangeHandler>,
    pub active_change: Option<DecorationStateChangeHandler>,
}

impl DecorationInteractionHandlers {
    fn has_any(&self) -> bool {
        self.hover_change.is_some() || self.active_change.is_some()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecorationStateChangeHandler {
    pub true_handler: String,
    pub false_handler: String,
}

impl DecorationStateChangeHandler {
    pub fn handler_for(&self, state: bool) -> &str {
        if state {
            &self.true_handler
        } else {
            &self.false_handler
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecorationInteractionTarget {
    pub node_id: String,
    pub handlers: DecorationInteractionHandlers,
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
    pub interaction: DecorationInteractionHandlers,
    pub kind: DecorationNodeKind,
    pub style: DecorationStyle,
    pub children: Vec<DecorationNode>,
}

impl DecorationNode {
    pub fn new(kind: DecorationNodeKind) -> Self {
        Self {
            stable_id: None,
            interaction: DecorationInteractionHandlers::default(),
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
    Image(ImageNode),
    ShaderEffect(ShaderEffectNode),
    WindowBorder,
    /// Reserved anchor where the client surface is placed.
    WindowSlot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageFit {
    Contain,
    Cover,
    Fill,
}

impl Default for ImageFit {
    fn default() -> Self {
        ImageFit::Contain
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageNode {
    pub src: String,
    pub fit: ImageFit,
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
    OnSourceDamageBox {
        anti_artifact_margin: i32,
    },
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
        matches!(
            self.input,
            EffectInput::Backdrop | EffectInput::XrayBackdrop
        )
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
    pub position: Option<StylePosition>,
    pub z_index: Option<i32>,
    pub inset: PositionOffsets,
    pub overflow: Option<Overflow>,
    pub pointer_events: Option<PointerEvents>,
    pub transform: Option<NodeTransform>,
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
    pub border_fit: Option<BorderFit>,
    pub border_radius: Option<i32>,
    pub visible: Option<bool>,
    pub cursor: Option<String>,
    pub font_size: Option<i32>,
    pub font_weight: Option<serde_json::Value>,
    pub font_family: Option<Vec<String>>,
    pub text_align: Option<String>,
    pub line_height: Option<i32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StylePosition {
    Relative,
    Absolute,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Overflow {
    Visible,
    Hidden,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointerEvents {
    Auto,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NodeTransform {
    pub translate_x: f32,
    pub translate_y: f32,
    pub scale_x: f32,
    pub scale_y: f32,
}

impl Default for NodeTransform {
    fn default() -> Self {
        Self {
            translate_x: 0.0,
            translate_y: 0.0,
            scale_x: 1.0,
            scale_y: 1.0,
        }
    }
}

impl NodeTransform {
    fn is_identity(self) -> bool {
        self.translate_x.abs() < f32::EPSILON
            && self.translate_y.abs() < f32::EPSILON
            && (self.scale_x - 1.0).abs() < f32::EPSILON
            && (self.scale_y - 1.0).abs() < f32::EPSILON
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PositionOffsets {
    pub top: Option<i32>,
    pub right: Option<i32>,
    pub bottom: Option<i32>,
    pub left: Option<i32>,
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
pub enum BorderFit {
    Normal,
    FitChildren,
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

impl DecorationStyle {
    fn is_absolute_positioned(&self) -> bool {
        matches!(self.position, Some(StylePosition::Absolute))
    }

    fn establishes_containing_block(&self) -> bool {
        matches!(
            self.position,
            Some(StylePosition::Relative | StylePosition::Absolute)
        )
    }

    fn z_index_or_zero(&self) -> i32 {
        self.z_index.unwrap_or(0)
    }

    fn pointer_events_enabled(&self) -> bool {
        !matches!(self.pointer_events, Some(PointerEvents::None))
    }

    fn clips_children(&self) -> bool {
        matches!(self.overflow, Some(Overflow::Hidden))
            || (self.border.is_some() && !matches!(self.overflow, Some(Overflow::Visible)))
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

    fn snapped_size(
        self,
        scale_x: f64,
        scale_y: f64,
    ) -> (ResolvedLayoutValue, ResolvedLayoutValue) {
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

    fn transform_around(self, origin: (f32, f32), transform: NodeTransform) -> Self {
        let left =
            origin.0 + (self.x.to_f32() - origin.0) * transform.scale_x + transform.translate_x;
        let top =
            origin.1 + (self.y.to_f32() - origin.1) * transform.scale_y + transform.translate_y;
        let right = origin.0
            + (self.right().to_f32() - origin.0) * transform.scale_x
            + transform.translate_x;
        let bottom = origin.1
            + (self.bottom().to_f32() - origin.1) * transform.scale_y
            + transform.translate_y;
        let min_x = left.min(right);
        let min_y = top.min(bottom);
        let max_x = left.max(right);
        let max_y = top.max(bottom);
        Self {
            x: ResolvedLayoutValue::from_f32(min_x),
            y: ResolvedLayoutValue::from_f32(min_y),
            width: ResolvedLayoutValue::from_f32((max_x - min_x).max(0.0)),
            height: ResolvedLayoutValue::from_f32((max_y - min_y).max(0.0)),
        }
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
        ResolvedLogicalRect::from_logical(rect),
    )
}

fn layout_node_resolved(
    node: &DecorationNode,
    resolved_rect: ResolvedLogicalRect,
    inherited_clip: Option<ResolvedDecorationClip>,
    window_slot_size: Option<(i32, i32)>,
    scale: f64,
    containing_block: ResolvedLogicalRect,
) -> Result<ComputedDecorationNode, DecorationLayoutError> {
    let resolved_border_width = node
        .style
        .border
        .map(|border| ResolvedLayoutValue::from_i32(border.width.max(0)).snap_edge(scale))
        .unwrap_or(ResolvedLayoutValue::ZERO);
    let resolved_border_radius =
        ResolvedLayoutValue::from_i32(node.style.border_radius.unwrap_or(0).max(0))
            .snap_edge(scale);
    let content_rect = resolved_rect.inset(node.style.resolved_content_inset(scale));
    let effective_clip =
        effective_clip_for_node_resolved(node, inherited_clip, content_rect, scale);
    let child_containing_block = if node.style.establishes_containing_block() {
        content_rect
    } else {
        containing_block
    };

    let children = match &node.kind {
        DecorationNodeKind::Box(layout) => layout_box_children(
            node,
            content_rect,
            layout.direction,
            effective_clip,
            window_slot_size,
            scale,
            child_containing_block,
        )?,
        DecorationNodeKind::ShaderEffect(effect) => layout_box_children(
            node,
            content_rect,
            effect.direction,
            effective_clip,
            window_slot_size,
            scale,
            child_containing_block,
        )?,
        // Buttons act as flex containers for their child icon / label so that
        // explicit child `width` / `height` are honored. Without this, all
        // non-absolute children collapse into the parent's content rect via
        // the fallback arm below.
        DecorationNodeKind::Button(_) => layout_box_children(
            node,
            content_rect,
            LayoutDirection::Column,
            effective_clip,
            window_slot_size,
            scale,
            child_containing_block,
        )?,
        _ if node.children.is_empty() => Vec::new(),
        _ => node
            .children
            .iter()
            .map(|child| {
                let child_rect = if child.style.is_absolute_positioned() {
                    absolute_child_rect_resolved(
                        child,
                        child_containing_block,
                        window_slot_size,
                        scale,
                    )
                } else {
                    content_rect
                };
                layout_node_resolved(
                    child,
                    child_rect,
                    effective_clip,
                    window_slot_size,
                    scale,
                    child_containing_block,
                )
            })
            .collect::<Result<Vec<_>, _>>()?,
    };

    let mut computed = ComputedDecorationNode {
        stable_id: node.stable_id.clone(),
        interaction: node.interaction.clone(),
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
    };

    apply_node_transform(&mut computed);
    Ok(computed)
}

pub(super) fn reapply_tree_preserving_layout(
    computed: &mut ComputedDecorationNode,
    node: &DecorationNode,
    inherited_clip: Option<ResolvedDecorationClip>,
    scale: f64,
) {
    computed.stable_id = node.stable_id.clone();
    computed.interaction = node.interaction.clone();
    computed.kind = node.kind.clone();
    computed.style = node.style.clone();
    let content_rect = computed
        .resolved_rect
        .inset(node.style.resolved_content_inset(scale));
    let effective_clip =
        effective_clip_for_node_resolved(node, inherited_clip, content_rect, scale);
    computed.rect = computed.resolved_rect.round_to_logical_rect();
    computed.resolved_content_rect = content_rect;
    computed.resolved_border_width = node
        .style
        .border
        .map(|border| ResolvedLayoutValue::from_i32(border.width.max(0)).snap_edge(scale))
        .unwrap_or(ResolvedLayoutValue::ZERO);
    computed.resolved_border_radius =
        ResolvedLayoutValue::from_i32(node.style.border_radius.unwrap_or(0).max(0))
            .snap_edge(scale);
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
    containing_block: ResolvedLogicalRect,
) -> Result<Vec<ComputedDecorationNode>, DecorationLayoutError> {
    if node.children.is_empty() {
        return Ok(Vec::new());
    }

    let flow_children = node
        .children
        .iter()
        .enumerate()
        .filter(|(_, child)| !child.style.is_absolute_positioned())
        .collect::<Vec<_>>();
    let gap = node.style.resolved_gap(scale);
    let main_available = direction.main_len_resolved(content_rect);
    let cross_available = direction.cross_len_resolved(content_rect);
    let total_gap =
        ResolvedLayoutValue::from_raw(gap.raw() * (flow_children.len().saturating_sub(1) as i32));

    let mut base_sizes = Vec::with_capacity(flow_children.len());
    let mut flexes = Vec::with_capacity(flow_children.len());
    let mut shrink_factors = Vec::with_capacity(flow_children.len());
    let mut auto_main_flags = Vec::with_capacity(flow_children.len());

    let mut base_sum = ResolvedLayoutValue::ZERO;
    let mut total_flex = 0.0f32;
    let mut total_shrink = 0.0f32;

    for (_, child) in &flow_children {
        let margin = child.style.resolved_margin(scale);
        let margin_main = direction.main_start_margin_resolved(margin)
            + direction.main_end_margin_resolved(margin);
        let base =
            child.preferred_main_size_resolved(direction, window_slot_size, scale) + margin_main;
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
            allocated[idx] = ResolvedLayoutValue::from_raw((allocated[idx].raw() - actual).max(0));
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

    let allocated_main_sum = allocated
        .iter()
        .copied()
        .fold(ResolvedLayoutValue::ZERO, |sum, value| sum + value);
    let flow_child_count = flow_children.len();
    let remaining_after_allocation = ResolvedLayoutValue::from_raw(
        (main_available.raw() - total_gap.raw() - allocated_main_sum.raw()).max(0),
    );
    let justify_content = node.style.justify_content.unwrap_or(JustifyContent::Start);
    let (main_offset, gap_extra, gap_extra_remainder) = match justify_content {
        JustifyContent::Center => (
            ResolvedLayoutValue::from_raw(remaining_after_allocation.raw() / 2),
            ResolvedLayoutValue::ZERO,
            0,
        ),
        JustifyContent::End => (remaining_after_allocation, ResolvedLayoutValue::ZERO, 0),
        JustifyContent::SpaceBetween if flow_child_count > 1 => {
            let gap_count = (flow_child_count - 1) as i32;
            (
                ResolvedLayoutValue::ZERO,
                ResolvedLayoutValue::from_raw(remaining_after_allocation.raw() / gap_count),
                remaining_after_allocation.raw() % gap_count,
            )
        }
        _ => (ResolvedLayoutValue::ZERO, ResolvedLayoutValue::ZERO, 0),
    };

    let mut cursor = direction.main_origin_resolved(content_rect) + main_offset;
    let mut children = vec![None; node.children.len()];
    let layout_debug_enabled = std::env::var_os("SHOJI_GAP_LAYOUT_CHILD_DEBUG").is_some();
    let direction_name = match direction {
        LayoutDirection::Row => "row",
        LayoutDirection::Column => "column",
    };
    let parent_stable_id = node.stable_id.as_deref().unwrap_or("<none>");

    for (flow_position, ((index, child), main_size)) in flow_children
        .into_iter()
        .zip(allocated.into_iter())
        .enumerate()
    {
        let margin = child.style.resolved_margin(scale);
        let margin_main_start = direction.main_start_margin_resolved(margin);
        let margin_main_end = direction.main_end_margin_resolved(margin);
        let margin_cross_start = direction.cross_start_margin_resolved(margin);
        let margin_cross_end = direction.cross_end_margin_resolved(margin);
        let child_main_size = ResolvedLayoutValue::from_raw(
            (main_size.raw() - margin_main_start.raw() - margin_main_end.raw()).max(0),
        );
        let child_available_cross = ResolvedLayoutValue::from_raw(
            (cross_available.raw() - margin_cross_start.raw() - margin_cross_end.raw()).max(0),
        );
        let child_align = node.style.align_items;
        let cross_size = child.preferred_cross_size_resolved(
            direction,
            child_available_cross,
            child_align,
            window_slot_size,
            scale,
        );
        let margin_box_cross_size = cross_size + margin_cross_start + margin_cross_end;
        let margin_box_cross_origin = direction.cross_origin_for_child_resolved(
            content_rect,
            child_align,
            margin_box_cross_size,
            scale,
        );
        let cross_origin = margin_box_cross_origin + margin_cross_start;

        let child_rect = direction.rect_resolved(
            cursor + margin_main_start,
            cross_origin,
            child_main_size,
            cross_size,
        );
        if layout_debug_enabled {
            let scale_f32 = scale.abs().max(0.0001) as f32;
            let (parent_cross_start, parent_cross_len, child_cross_start, child_cross_len) =
                match direction {
                    LayoutDirection::Row => (
                        content_rect.y.to_f32(),
                        content_rect.height.to_f32(),
                        child_rect.y.to_f32(),
                        child_rect.height.to_f32(),
                    ),
                    LayoutDirection::Column => (
                        content_rect.x.to_f32(),
                        content_rect.width.to_f32(),
                        child_rect.x.to_f32(),
                        child_rect.width.to_f32(),
                    ),
                };
            let parent_cross_start_px = (parent_cross_start * scale_f32).round() as i32;
            let parent_cross_end_px =
                ((parent_cross_start + parent_cross_len) * scale_f32).round() as i32;
            let child_cross_start_px = (child_cross_start * scale_f32).round() as i32;
            let child_cross_len_px = (child_cross_len * scale_f32).round().max(0.0) as i32;
            let child_cross_end_px = child_cross_start_px + child_cross_len_px;
            let child_center_twice_px = child_cross_start_px * 2 + child_cross_len_px;
            let parent_center_twice_px = parent_cross_start_px + parent_cross_end_px;
            let child_stable_id = child.stable_id.as_deref().unwrap_or("<none>");
            let child_kind = match &child.kind {
                DecorationNodeKind::Box(_) => "box",
                DecorationNodeKind::Label(_) => "label",
                DecorationNodeKind::Button(_) => "button",
                DecorationNodeKind::AppIcon => "app-icon",
                DecorationNodeKind::Image(_) => "image",
                DecorationNodeKind::ShaderEffect(_) => "shader-effect",
                DecorationNodeKind::WindowBorder => "window-border",
                DecorationNodeKind::WindowSlot => "window-slot",
            };
            tracing::info!(
                parent_stable_id,
                child_stable_id,
                child_kind,
                child_index = index,
                direction = direction_name,
                align = ?child_align.unwrap_or(AlignItems::Stretch),
                scale,
                content_rect = ?content_rect,
                cursor_raw = cursor.raw(),
                cursor_resolved = cursor.to_f32(),
                main_size_raw = main_size.raw(),
                main_size_resolved = main_size.to_f32(),
                cross_available_raw = cross_available.raw(),
                cross_available_resolved = cross_available.to_f32(),
                cross_size_raw = cross_size.raw(),
                cross_size_resolved = cross_size.to_f32(),
                cross_origin_raw = cross_origin.raw(),
                cross_origin_resolved = cross_origin.to_f32(),
                parent_cross_start_px,
                parent_cross_end_px,
                child_cross_start_px,
                child_cross_len_px,
                child_cross_end_px,
                center_delta_twice_px = child_center_twice_px - parent_center_twice_px,
                child_rect = ?child_rect,
                "gap layout child placement"
            );
        }
        children[index] = Some(layout_node_resolved(
            child,
            child_rect,
            effective_clip,
            window_slot_size,
            scale,
            containing_block,
        )?);
        let distributed_gap_remainder = (flow_position as i32).min(gap_extra_remainder.max(0));
        let next_distributed_gap_remainder =
            ((flow_position + 1) as i32).min(gap_extra_remainder.max(0));
        let gap_remainder_step = ResolvedLayoutValue::from_raw(
            next_distributed_gap_remainder - distributed_gap_remainder,
        );
        cursor = cursor + main_size + gap + gap_extra + gap_remainder_step;
    }

    for (index, child) in node.children.iter().enumerate() {
        if child.style.is_absolute_positioned() {
            let child_rect =
                absolute_child_rect_resolved(child, containing_block, window_slot_size, scale);
            children[index] = Some(layout_node_resolved(
                child,
                child_rect,
                effective_clip,
                window_slot_size,
                scale,
                containing_block,
            )?);
        }
    }

    Ok(children
        .into_iter()
        .map(|child| child.expect("every child must be laid out"))
        .collect())
}

fn absolute_child_rect_resolved(
    child: &DecorationNode,
    containing_block: ResolvedLogicalRect,
    window_slot_size: Option<(i32, i32)>,
    scale: f64,
) -> ResolvedLogicalRect {
    let offsets = child.style.inset;
    let margin = child.style.resolved_margin(scale);
    let left = offsets
        .left
        .map(|value| ResolvedLayoutValue::from_i32(value).snap_edge(scale));
    let right = offsets
        .right
        .map(|value| ResolvedLayoutValue::from_i32(value).snap_edge(scale));
    let top = offsets
        .top
        .map(|value| ResolvedLayoutValue::from_i32(value).snap_edge(scale));
    let bottom = offsets
        .bottom
        .map(|value| ResolvedLayoutValue::from_i32(value).snap_edge(scale));

    let auto_size = child.auto_size_resolved(window_slot_size, scale);
    let width = match (child.style.width, left, right) {
        (Some(width), _, _) => ResolvedLayoutValue::from_i32(width).snap_edge(scale),
        (None, Some(left), Some(right)) => ResolvedLayoutValue::from_raw(
            (containing_block.width.raw()
                - left.raw()
                - right.raw()
                - margin.left.raw()
                - margin.right.raw())
            .max(0),
        ),
        _ => auto_size
            .map(|(width, _)| width)
            .unwrap_or(ResolvedLayoutValue::ZERO),
    };
    let height = match (child.style.height, top, bottom) {
        (Some(height), _, _) => ResolvedLayoutValue::from_i32(height).snap_edge(scale),
        (None, Some(top), Some(bottom)) => ResolvedLayoutValue::from_raw(
            (containing_block.height.raw()
                - top.raw()
                - bottom.raw()
                - margin.top.raw()
                - margin.bottom.raw())
            .max(0),
        ),
        _ => auto_size
            .map(|(_, height)| height)
            .unwrap_or(ResolvedLayoutValue::ZERO),
    };

    let x = if let Some(left) = left {
        containing_block.x + left + margin.left
    } else if let Some(right) = right {
        containing_block.x + containing_block.width - right - margin.right - width
    } else {
        containing_block.x + margin.left
    };
    let y = if let Some(top) = top {
        containing_block.y + top + margin.top
    } else if let Some(bottom) = bottom {
        containing_block.y + containing_block.height - bottom - margin.bottom - height
    } else {
        containing_block.y + margin.top
    };

    ResolvedLogicalRect {
        x,
        y,
        width,
        height,
    }
}

fn apply_node_transform(node: &mut ComputedDecorationNode) {
    let Some(transform) = node.style.transform else {
        return;
    };
    if transform.is_identity() {
        return;
    }

    let origin = (
        (node.resolved_rect.x + node.resolved_rect.width).to_f32()
            - node.resolved_rect.width.to_f32() * 0.5,
        (node.resolved_rect.y + node.resolved_rect.height).to_f32()
            - node.resolved_rect.height.to_f32() * 0.5,
    );
    transform_subtree(node, origin, transform);
}

fn transform_subtree(
    node: &mut ComputedDecorationNode,
    origin: (f32, f32),
    transform: NodeTransform,
) {
    node.resolved_rect = node.resolved_rect.transform_around(origin, transform);
    node.rect = node.resolved_rect.round_to_logical_rect();
    node.resolved_content_rect = node
        .resolved_content_rect
        .transform_around(origin, transform);
    if let Some(clip) = &mut node.resolved_effective_clip {
        clip.rect = clip.rect.transform_around(origin, transform);
    }
    node.effective_clip = node
        .resolved_effective_clip
        .map(ResolvedDecorationClip::round_to_logical_clip);

    for child in &mut node.children {
        transform_subtree(child, origin, transform);
    }
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

    fn preferred_main_size(
        &self,
        direction: LayoutDirection,
        window_slot_size: Option<(i32, i32)>,
    ) -> i32 {
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
            if matches!(align.unwrap_or(AlignItems::Stretch), AlignItems::Stretch)
                && available_cross > 0
            {
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
                let line_height = self
                    .style
                    .line_height
                    .unwrap_or(font_size + 4)
                    .max(font_size);
                let spec = LabelSpec {
                    rect: LogicalRect::new(0, 0, 0, 0),
                    rect_precise: None,
                    text: label.text.clone(),
                    color: self
                        .style
                        .color
                        .unwrap_or(Color::WHITE)
                        .with_opacity(self.style.opacity),
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
        self.intrinsic_size(window_slot_size)
            .map(|(width, height)| {
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
            DecorationNodeKind::Box(layout) => {
                Some(self.stack_content_size(layout.direction, window_slot_size))
            }
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

    fn stack_content_size(
        &self,
        direction: LayoutDirection,
        window_slot_size: Option<(i32, i32)>,
    ) -> (i32, i32) {
        let inset = self.style.content_inset();
        if self.children.is_empty() {
            return (inset.left + inset.right, inset.top + inset.bottom);
        }

        let flow_children = self
            .children
            .iter()
            .filter(|child| !child.style.is_absolute_positioned())
            .collect::<Vec<_>>();
        if flow_children.is_empty() {
            return (inset.left + inset.right, inset.top + inset.bottom);
        }

        let gap = self.style.gap.unwrap_or(0).max(0);
        let mut main_sum = 0;
        let mut cross_max = 0;

        for child in &flow_children {
            let child_main = child
                .preferred_main_size(direction, window_slot_size)
                .max(0)
                + direction.main_margin(child.style.margin);
            let child_cross = child
                .preferred_cross_size(direction, 0, child.style.align_items, window_slot_size)
                .max(0)
                + direction.cross_margin(child.style.margin);
            main_sum += child_main;
            cross_max = cross_max.max(child_cross);
        }

        main_sum += gap * flow_children.len().saturating_sub(1) as i32;

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
            return (inset.left + inset.right, inset.top + inset.bottom);
        }

        let flow_children = self
            .children
            .iter()
            .filter(|child| !child.style.is_absolute_positioned())
            .collect::<Vec<_>>();
        if flow_children.is_empty() {
            return (inset.left + inset.right, inset.top + inset.bottom);
        }

        let gap = self.style.resolved_gap(scale);
        let mut main_sum = ResolvedLayoutValue::ZERO;
        let mut cross_max = ResolvedLayoutValue::ZERO;

        for child in &flow_children {
            let margin = child.style.resolved_margin(scale);
            let child_main = child.preferred_main_size_resolved(direction, window_slot_size, scale)
                + direction.main_start_margin_resolved(margin)
                + direction.main_end_margin_resolved(margin);
            let child_cross = child.preferred_cross_size_resolved(
                direction,
                ResolvedLayoutValue::ZERO,
                child.style.align_items,
                window_slot_size,
                scale,
            ) + direction.cross_start_margin_resolved(margin)
                + direction.cross_end_margin_resolved(margin);
            main_sum = main_sum + child_main;
            cross_max = cross_max.max(child_cross);
        }

        main_sum = main_sum
            + ResolvedLayoutValue::from_raw(
                gap.raw() * flow_children.len().saturating_sub(1) as i32,
            );

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

        for child in self
            .children
            .iter()
            .filter(|child| !child.style.is_absolute_positioned())
        {
            width = width.max(
                child
                    .preferred_main_size(LayoutDirection::Row, window_slot_size)
                    .max(0)
                    + LayoutDirection::Row.main_margin(child.style.margin),
            );
            height = height.max(
                child
                    .preferred_main_size(LayoutDirection::Column, window_slot_size)
                    .max(0)
                    + LayoutDirection::Column.main_margin(child.style.margin),
            );
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

        for child in self
            .children
            .iter()
            .filter(|child| !child.style.is_absolute_positioned())
        {
            let margin = child.style.resolved_margin(scale);
            width = width.max(
                child.preferred_main_size_resolved(LayoutDirection::Row, window_slot_size, scale)
                    + LayoutDirection::Row.main_start_margin_resolved(margin)
                    + LayoutDirection::Row.main_end_margin_resolved(margin),
            );
            height = height.max(
                child.preferred_main_size_resolved(
                    LayoutDirection::Column,
                    window_slot_size,
                    scale,
                ) + LayoutDirection::Column.main_start_margin_resolved(margin)
                    + LayoutDirection::Column.main_end_margin_resolved(margin),
            );
        }

        (
            width + inset.left + inset.right,
            height + inset.top + inset.bottom,
        )
    }
}

impl DecorationStyle {
    pub(crate) fn effective_border_fit(&self, kind: &DecorationNodeKind) -> BorderFit {
        self.border_fit.unwrap_or(match kind {
            DecorationNodeKind::WindowBorder => BorderFit::FitChildren,
            _ => BorderFit::Normal,
        })
    }

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

    fn resolved_margin(&self, scale: f64) -> ResolvedLayoutEdges {
        let margin = ResolvedLayoutEdges::from_edges(self.margin);
        ResolvedLayoutEdges {
            top: margin.top.snap_edge(scale),
            right: margin.right.snap_edge(scale),
            bottom: margin.bottom.snap_edge(scale),
            left: margin.left.snap_edge(scale),
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
        (DecorationNodeKind::Box(left), DecorationNodeKind::Box(right)) => {
            left.direction == right.direction
        }
        (DecorationNodeKind::Label(left), DecorationNodeKind::Label(right)) => {
            left.text == right.text
        }
        (DecorationNodeKind::Button(_), DecorationNodeKind::Button(_)) => true,
        (DecorationNodeKind::AppIcon, DecorationNodeKind::AppIcon) => true,
        (DecorationNodeKind::Image(left), DecorationNodeKind::Image(right)) => {
            left.src == right.src && left.fit == right.fit
        }
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
        && left.position == right.position
        && left.inset == right.inset
        && left.overflow == right.overflow
        && left.transform == right.transform
        && left.justify_content == right.justify_content
        && left.align_items == right.align_items
        && left.border.map(|border| border.width) == right.border.map(|border| border.width)
        && left.border_top.map(|border| border.width) == right.border_top.map(|border| border.width)
        && left.border_right.map(|border| border.width)
            == right.border_right.map(|border| border.width)
        && left.border_bottom.map(|border| border.width)
            == right.border_bottom.map(|border| border.width)
        && left.border_left.map(|border| border.width)
            == right.border_left.map(|border| border.width)
        && left.border_fit == right.border_fit
        && left.border_radius == right.border_radius
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
    fn main_start_margin_resolved(self, margin: ResolvedLayoutEdges) -> ResolvedLayoutValue {
        match self {
            LayoutDirection::Row => margin.left,
            LayoutDirection::Column => margin.top,
        }
    }

    fn main_end_margin_resolved(self, margin: ResolvedLayoutEdges) -> ResolvedLayoutValue {
        match self {
            LayoutDirection::Row => margin.right,
            LayoutDirection::Column => margin.bottom,
        }
    }

    fn cross_start_margin_resolved(self, margin: ResolvedLayoutEdges) -> ResolvedLayoutValue {
        match self {
            LayoutDirection::Row => margin.top,
            LayoutDirection::Column => margin.left,
        }
    }

    fn cross_end_margin_resolved(self, margin: ResolvedLayoutEdges) -> ResolvedLayoutValue {
        match self {
            LayoutDirection::Row => margin.bottom,
            LayoutDirection::Column => margin.right,
        }
    }

    fn main_margin(self, margin: Edges) -> i32 {
        match self {
            LayoutDirection::Row => margin.left + margin.right,
            LayoutDirection::Column => margin.top + margin.bottom,
        }
    }

    fn cross_margin(self, margin: Edges) -> i32 {
        match self {
            LayoutDirection::Row => margin.top + margin.bottom,
            LayoutDirection::Column => margin.left + margin.right,
        }
    }

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
        scale: f64,
    ) -> ResolvedLayoutValue {
        let align = align.unwrap_or(AlignItems::Stretch);
        let scale = scale.abs().max(0.0001) as f32;

        match (self, align) {
            (LayoutDirection::Row, AlignItems::Center) => {
                let top_px = (rect.y.to_f32() * scale).round() as i32;
                let bottom_px = ((rect.y.to_f32() + rect.height.to_f32()) * scale).round() as i32;
                let child_px = (child_cross_size.to_f32() * scale).round().max(0.0) as i32;
                let aligned_px = top_px + ((bottom_px - top_px - child_px).max(0) / 2);
                ResolvedLayoutValue::from_f32(aligned_px as f32 / scale)
            }
            (LayoutDirection::Row, AlignItems::End) => {
                let bottom_px = ((rect.y.to_f32() + rect.height.to_f32()) * scale).round() as i32;
                let child_px = (child_cross_size.to_f32() * scale).round().max(0.0) as i32;
                let aligned_px = bottom_px - child_px;
                ResolvedLayoutValue::from_f32(aligned_px as f32 / scale)
            }
            (LayoutDirection::Row, _) => {
                let top_px = (rect.y.to_f32() * scale).round() as i32;
                ResolvedLayoutValue::from_f32(top_px as f32 / scale)
            }
            (LayoutDirection::Column, AlignItems::Center) => {
                let left_px = (rect.x.to_f32() * scale).round() as i32;
                let right_px = ((rect.x.to_f32() + rect.width.to_f32()) * scale).round() as i32;
                let child_px = (child_cross_size.to_f32() * scale).round().max(0.0) as i32;
                let aligned_px = left_px + ((right_px - left_px - child_px).max(0) / 2);
                ResolvedLayoutValue::from_f32(aligned_px as f32 / scale)
            }
            (LayoutDirection::Column, AlignItems::End) => {
                let right_px = ((rect.x.to_f32() + rect.width.to_f32()) * scale).round() as i32;
                let child_px = (child_cross_size.to_f32() * scale).round().max(0.0) as i32;
                let aligned_px = right_px - child_px;
                ResolvedLayoutValue::from_f32(aligned_px as f32 / scale)
            }
            (LayoutDirection::Column, _) => {
                let left_px = (rect.x.to_f32() * scale).round() as i32;
                ResolvedLayoutValue::from_f32(left_px as f32 / scale)
            }
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

    fn rect(
        self,
        main_origin: i32,
        cross_origin: i32,
        main_len: i32,
        cross_len: i32,
    ) -> LogicalRect {
        match self {
            LayoutDirection::Row => {
                LogicalRect::new(main_origin, cross_origin, main_len, cross_len)
            }
            LayoutDirection::Column => {
                LogicalRect::new(cross_origin, main_origin, cross_len, main_len)
            }
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
        DecorationNodeKind::Image(image) => primitives.push(DecorationRenderPrimitive::Image {
            rect: node.rect,
            src: image.src.clone(),
            fit: image.fit,
        }),
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

    for child in paint_ordered_children(node) {
        collect_render_primitives(child, primitives);
    }

    if let Some(background) = node
        .style
        .background
        .map(|color| color.with_opacity(node.style.opacity))
    {
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

fn paint_ordered_children(node: &ComputedDecorationNode) -> Vec<&ComputedDecorationNode> {
    let mut children = node.children.iter().enumerate().collect::<Vec<_>>();
    children.sort_by(|(left_index, left), (right_index, right)| {
        right
            .style
            .z_index_or_zero()
            .cmp(&left.style.z_index_or_zero())
            .then_with(|| right_index.cmp(left_index))
    });
    children.into_iter().map(|(_, child)| child).collect()
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

fn find_button_action(node: &ComputedDecorationNode, point: LogicalPoint) -> Option<WindowAction> {
    if node.style.visible == Some(false) || !node.style.pointer_events_enabled() {
        return None;
    }
    if node
        .effective_clip
        .is_some_and(|clip| !clip.rect.contains(point))
    {
        return None;
    }

    for child in paint_ordered_children(node) {
        if let Some(action) = find_button_action(child, point) {
            return Some(action);
        }
    }

    match &node.kind {
        DecorationNodeKind::Button(button) if node.rect.contains(point) => {
            Some(button.action.clone())
        }
        _ => None,
    }
}

fn find_interaction_target(
    node: &ComputedDecorationNode,
    point: LogicalPoint,
) -> Option<DecorationInteractionTarget> {
    if node.style.visible == Some(false) || !node.style.pointer_events_enabled() {
        return None;
    }
    if node
        .effective_clip
        .is_some_and(|clip| !clip.rect.contains(point))
    {
        return None;
    }

    for child in paint_ordered_children(node) {
        if let Some(target) = find_interaction_target(child, point) {
            return Some(target);
        }
    }

    if node.rect.contains(point) && node.interaction.has_any() {
        let node_id = node.stable_id.clone()?;
        return Some(DecorationInteractionTarget {
            node_id,
            handlers: node.interaction.clone(),
        });
    }

    None
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
    let node_clip = node.style.clips_children().then(|| {
        let border_width = node
            .style
            .border
            .map(|border| border.width.max(0))
            .unwrap_or(0);
        ResolvedDecorationClip {
            rect: content_rect,
            radius: (ResolvedLayoutValue::from_i32(node.style.border_radius.unwrap_or(0))
                - ResolvedLayoutValue::from_i32(border_width).snap_edge(scale))
            .max(ResolvedLayoutValue::ZERO),
        }
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

    if x2 <= x1 || y2 <= y1 {
        return None;
    }

    if rect_contains_logical(left.rect, right.rect) {
        return Some(right);
    }

    if rect_contains_logical(right.rect, left.rect) {
        return Some(left);
    }

    Some(DecorationClip {
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

    if x2.raw() <= x1.raw() || y2.raw() <= y1.raw() {
        return None;
    }

    if resolved_rect_contains(left.rect, right.rect) {
        return Some(right);
    }

    if resolved_rect_contains(right.rect, left.rect) {
        return Some(left);
    }

    Some(ResolvedDecorationClip {
        rect: ResolvedLogicalRect {
            x: x1,
            y: y1,
            width: ResolvedLayoutValue::from_raw(x2.raw() - x1.raw()),
            height: ResolvedLayoutValue::from_raw(y2.raw() - y1.raw()),
        },
        radius: left.radius.min(right.radius),
    })
}

fn rect_contains_logical(outer: LogicalRect, inner: LogicalRect) -> bool {
    outer.x <= inner.x
        && outer.y <= inner.y
        && outer.x + outer.width >= inner.x + inner.width
        && outer.y + outer.height >= inner.y + inner.height
}

fn resolved_rect_contains(outer: ResolvedLogicalRect, inner: ResolvedLogicalRect) -> bool {
    outer.x.raw() <= inner.x.raw()
        && outer.y.raw() <= inner.y.raw()
        && outer.right().raw() >= inner.right().raw()
        && outer.bottom().raw() >= inner.bottom().raw()
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

        assert_eq!(
            layout.window_slot_rect(),
            Some(LogicalRect::new(2, 2, 96, 46))
        );
    }

    #[test]
    fn rectangular_parent_clip_preserves_nested_window_border_radius() {
        let root = DecorationNode::new(DecorationNodeKind::Box(BoxNode {
            direction: LayoutDirection::Column,
        }))
        .with_style(DecorationStyle {
            border: Some(BorderStyle {
                width: 2,
                color: Color::WHITE,
            }),
            ..Default::default()
        })
        .with_children(vec![
            DecorationNode::new(DecorationNodeKind::WindowBorder)
                .with_style(DecorationStyle {
                    border: Some(BorderStyle {
                        width: 2,
                        color: Color::WHITE,
                    }),
                    border_radius: Some(20),
                    ..Default::default()
                })
                .with_children(vec![DecorationNode::new(DecorationNodeKind::WindowSlot)]),
        ]);

        let layout = DecorationTree::new(root)
            .layout(LogicalRect::new(0, 0, 200, 100))
            .expect("layout should succeed");
        let nested = &layout.root.children[0];

        assert_eq!(
            nested
                .resolved_effective_clip
                .expect("nested effective clip")
                .radius
                .round_to_i32(),
            18
        );
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
        .with_children(vec![
            titlebar,
            DecorationNode::new(DecorationNodeKind::WindowSlot),
        ]);

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
        .with_children(vec![
            left,
            spacer,
            right,
            DecorationNode::new(DecorationNodeKind::WindowSlot),
        ]);

        let layout = DecorationTree::new(root)
            .layout(LogicalRect::new(0, 0, 300, 30))
            .expect("layout should succeed");

        let spacer_rect = &layout.root.children[1].rect;
        let right_rect = &layout.root.children[2].rect;

        assert_eq!(*spacer_rect, LogicalRect::new(104, 0, 84, 30));
        assert_eq!(*right_rect, LogicalRect::new(192, 0, 20, 30));
    }

    #[test]
    fn button_lays_out_image_child_with_explicit_size() {
        let image = DecorationNode::new(DecorationNodeKind::Image(ImageNode {
            src: "/tmp/icon.svg".into(),
            fit: ImageFit::Contain,
        }))
        .with_style(DecorationStyle {
            width: Some(4),
            height: Some(4),
            ..Default::default()
        });
        let button = DecorationNode::new(DecorationNodeKind::Button(ButtonNode {
            action: WindowAction::Close,
        }))
        .with_style(DecorationStyle {
            width: Some(16),
            height: Some(16),
            ..Default::default()
        })
        .with_children(vec![image]);
        let root = DecorationNode::new(DecorationNodeKind::Box(BoxNode {
            direction: LayoutDirection::Row,
        }))
        .with_children(vec![
            button,
            DecorationNode::new(DecorationNodeKind::WindowSlot),
        ]);

        let layout = DecorationTree::new(root)
            .layout(LogicalRect::new(0, 0, 100, 20))
            .expect("layout should succeed");

        let image_rect = layout.root.children[0].children[0].rect;
        assert_eq!(image_rect.width, 4);
        assert_eq!(image_rect.height, 4);
    }

    #[test]
    fn button_can_center_image_child_on_both_axes() {
        let image = DecorationNode::new(DecorationNodeKind::Image(ImageNode {
            src: "/tmp/icon.svg".into(),
            fit: ImageFit::Contain,
        }))
        .with_style(DecorationStyle {
            width: Some(4),
            height: Some(4),
            ..Default::default()
        });
        let button = DecorationNode::new(DecorationNodeKind::Button(ButtonNode {
            action: WindowAction::Close,
        }))
        .with_style(DecorationStyle {
            width: Some(16),
            height: Some(16),
            align_items: Some(AlignItems::Center),
            justify_content: Some(JustifyContent::Center),
            ..Default::default()
        })
        .with_children(vec![image]);
        let root = DecorationNode::new(DecorationNodeKind::Box(BoxNode {
            direction: LayoutDirection::Row,
        }))
        .with_children(vec![
            button,
            DecorationNode::new(DecorationNodeKind::WindowSlot),
        ]);

        let layout = DecorationTree::new(root)
            .layout(LogicalRect::new(0, 0, 100, 20))
            .expect("layout should succeed");

        assert_eq!(
            layout.root.children[0].children[0].rect,
            LogicalRect::new(6, 6, 4, 4)
        );
    }

    #[test]
    fn row_box_applies_justify_content_space_between() {
        let child = || {
            DecorationNode::new(DecorationNodeKind::Box(BoxNode {
                direction: LayoutDirection::Row,
            }))
            .with_style(DecorationStyle {
                width: Some(10),
                height: Some(10),
                ..Default::default()
            })
        };
        let toolbar = DecorationNode::new(DecorationNodeKind::Box(BoxNode {
            direction: LayoutDirection::Row,
        }))
        .with_style(DecorationStyle {
            width: Some(100),
            height: Some(10),
            justify_content: Some(JustifyContent::SpaceBetween),
            ..Default::default()
        })
        .with_children(vec![child(), child(), child()]);
        let root = DecorationNode::new(DecorationNodeKind::Box(BoxNode {
            direction: LayoutDirection::Column,
        }))
        .with_children(vec![
            toolbar,
            DecorationNode::new(DecorationNodeKind::WindowSlot),
        ]);

        let layout = DecorationTree::new(root)
            .layout(LogicalRect::new(0, 0, 100, 100))
            .expect("layout should succeed");
        let toolbar = &layout.root.children[0];

        assert_eq!(toolbar.children[0].rect.x, 0);
        assert_eq!(toolbar.children[1].rect.x, 45);
        assert_eq!(toolbar.children[2].rect.x, 90);
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
        .with_children(vec![DecorationNode::new(DecorationNodeKind::Label(
            LabelNode {
                text: "Title".into(),
            },
        ))]);

        let root = DecorationNode::new(DecorationNodeKind::Box(BoxNode {
            direction: LayoutDirection::Column,
        }))
        .with_children(vec![
            titlebar,
            DecorationNode::new(DecorationNodeKind::WindowSlot),
        ]);

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
        .with_children(vec![DecorationNode::new(DecorationNodeKind::Label(
            LabelNode {
                text: "Title".into(),
            },
        ))]);

        let root = DecorationNode::new(DecorationNodeKind::Box(BoxNode {
            direction: LayoutDirection::Column,
        }))
        .with_children(vec![
            titlebar,
            DecorationNode::new(DecorationNodeKind::WindowSlot),
        ]);

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
    fn absolute_children_do_not_participate_in_flex_layout() {
        let overlay = DecorationNode::new(DecorationNodeKind::Box(BoxNode::default())).with_style(
            DecorationStyle {
                position: Some(StylePosition::Absolute),
                inset: PositionOffsets {
                    top: Some(0),
                    right: Some(0),
                    bottom: Some(0),
                    left: Some(0),
                },
                ..Default::default()
            },
        );

        let root = DecorationNode::new(DecorationNodeKind::Box(BoxNode {
            direction: LayoutDirection::Column,
        }))
        .with_style(DecorationStyle {
            position: Some(StylePosition::Relative),
            ..Default::default()
        })
        .with_children(vec![
            overlay,
            DecorationNode::new(DecorationNodeKind::Box(BoxNode::default())).with_style(
                DecorationStyle {
                    height: Some(20),
                    ..Default::default()
                },
            ),
            DecorationNode::new(DecorationNodeKind::WindowSlot),
        ]);

        let layout = DecorationTree::new(root)
            .layout(LogicalRect::new(0, 0, 100, 80))
            .expect("layout should succeed");

        assert_eq!(
            layout.root.children[0].rect,
            LogicalRect::new(0, 0, 100, 80)
        );
        assert_eq!(
            layout.root.children[1].rect,
            LogicalRect::new(0, 0, 100, 20)
        );
        assert_eq!(
            layout.window_slot_rect(),
            Some(LogicalRect::new(0, 20, 100, 60))
        );
    }

    #[test]
    fn row_layout_applies_child_margin_left() {
        let root = DecorationNode::new(DecorationNodeKind::Box(BoxNode {
            direction: LayoutDirection::Row,
        }))
        .with_children(vec![
            DecorationNode::new(DecorationNodeKind::Box(BoxNode::default())).with_style(
                DecorationStyle {
                    width: Some(10),
                    height: Some(10),
                    margin: Edges {
                        left: 32,
                        ..Default::default()
                    },
                    ..Default::default()
                },
            ),
            DecorationNode::new(DecorationNodeKind::WindowSlot),
        ]);

        let layout = DecorationTree::new(root)
            .layout(LogicalRect::new(0, 0, 100, 20))
            .expect("layout should succeed");

        assert_eq!(
            layout.root.children[0].rect,
            LogicalRect::new(32, 0, 10, 10)
        );
        assert_eq!(
            layout.window_slot_rect(),
            Some(LogicalRect::new(42, 0, 58, 20))
        );
    }

    #[test]
    fn absolute_layout_applies_child_margin_left() {
        let root = DecorationNode::new(DecorationNodeKind::Box(BoxNode::default()))
            .with_style(DecorationStyle {
                position: Some(StylePosition::Relative),
                ..Default::default()
            })
            .with_children(vec![
                DecorationNode::new(DecorationNodeKind::Box(BoxNode::default())).with_style(
                    DecorationStyle {
                        position: Some(StylePosition::Absolute),
                        width: Some(10),
                        height: Some(10),
                        inset: PositionOffsets {
                            left: Some(5),
                            ..Default::default()
                        },
                        margin: Edges {
                            left: 7,
                            ..Default::default()
                        },
                        ..Default::default()
                    },
                ),
                DecorationNode::new(DecorationNodeKind::WindowSlot),
            ]);

        let layout = DecorationTree::new(root)
            .layout(LogicalRect::new(0, 0, 100, 20))
            .expect("layout should succeed");

        assert_eq!(
            layout.root.children[0].rect,
            LogicalRect::new(12, 0, 10, 10)
        );
    }

    #[test]
    fn layout_equivalence_detects_absolute_inset_changes() {
        let left = DecorationNode::new(DecorationNodeKind::Box(BoxNode::default())).with_style(
            DecorationStyle {
                position: Some(StylePosition::Absolute),
                inset: PositionOffsets {
                    left: Some(4),
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        let right = DecorationNode::new(DecorationNodeKind::Box(BoxNode::default())).with_style(
            DecorationStyle {
                position: Some(StylePosition::Absolute),
                inset: PositionOffsets {
                    left: Some(8),
                    ..Default::default()
                },
                ..Default::default()
            },
        );

        assert!(!left.layout_equivalent(&right));
    }

    #[test]
    fn layout_equivalence_detects_transform_changes() {
        let left = DecorationNode::new(DecorationNodeKind::Box(BoxNode::default())).with_style(
            DecorationStyle {
                transform: Some(NodeTransform {
                    translate_x: 2.0,
                    ..Default::default()
                }),
                ..Default::default()
            },
        );
        let right = DecorationNode::new(DecorationNodeKind::Box(BoxNode::default())).with_style(
            DecorationStyle {
                transform: Some(NodeTransform {
                    translate_x: 4.0,
                    ..Default::default()
                }),
                ..Default::default()
            },
        );

        assert!(!left.layout_equivalent(&right));
    }

    #[test]
    fn z_index_controls_render_and_button_hit_order() {
        let root =
            DecorationNode::new(DecorationNodeKind::Box(BoxNode::default())).with_children(vec![
                DecorationNode::new(DecorationNodeKind::Button(ButtonNode {
                    action: WindowAction::Maximize,
                }))
                .with_style(DecorationStyle {
                    width: Some(100),
                    height: Some(20),
                    z_index: Some(1),
                    background: Some(Color::rgba(255, 0, 0, 255)),
                    ..Default::default()
                }),
                DecorationNode::new(DecorationNodeKind::Button(ButtonNode {
                    action: WindowAction::Close,
                }))
                .with_style(DecorationStyle {
                    position: Some(StylePosition::Absolute),
                    z_index: Some(10),
                    width: Some(100),
                    height: Some(20),
                    background: Some(Color::rgba(0, 255, 0, 255)),
                    ..Default::default()
                }),
                DecorationNode::new(DecorationNodeKind::WindowSlot),
            ]);

        let layout = DecorationTree::new(root)
            .layout(LogicalRect::new(0, 0, 100, 40))
            .expect("layout should succeed");
        let primitives = layout.render_primitives();
        let first_fill = primitives
            .iter()
            .find_map(|primitive| match primitive {
                DecorationRenderPrimitive::FillRect { color, .. } => Some(*color),
                _ => None,
            })
            .expect("button fill should exist");

        assert_eq!(first_fill, Color::rgba(0, 255, 0, 255));
        assert_eq!(
            layout.hit_test(LogicalPoint::new(5, 5)),
            DecorationHitTestResult::Action(WindowAction::Close)
        );
    }

    #[test]
    fn interaction_target_uses_topmost_paint_ordered_node() {
        let mut lower = DecorationNode::new(DecorationNodeKind::Button(ButtonNode {
            action: WindowAction::Maximize,
        }))
        .with_style(DecorationStyle {
            width: Some(100),
            height: Some(20),
            z_index: Some(1),
            ..Default::default()
        });
        lower.stable_id = Some("lower".into());
        lower.interaction.hover_change = Some(DecorationStateChangeHandler {
            true_handler: "lower-hover-true".into(),
            false_handler: "lower-hover-false".into(),
        });

        let mut upper = DecorationNode::new(DecorationNodeKind::Button(ButtonNode {
            action: WindowAction::Close,
        }))
        .with_style(DecorationStyle {
            position: Some(StylePosition::Absolute),
            z_index: Some(10),
            width: Some(100),
            height: Some(20),
            ..Default::default()
        });
        upper.stable_id = Some("upper".into());
        upper.interaction.hover_change = Some(DecorationStateChangeHandler {
            true_handler: "upper-hover-true".into(),
            false_handler: "upper-hover-false".into(),
        });

        let root =
            DecorationNode::new(DecorationNodeKind::Box(BoxNode::default())).with_children(vec![
                lower,
                upper,
                DecorationNode::new(DecorationNodeKind::WindowSlot),
            ]);

        let layout = DecorationTree::new(root)
            .layout(LogicalRect::new(0, 0, 100, 40))
            .expect("layout should succeed");
        let target = layout
            .interaction_target_at(LogicalPoint::new(5, 5))
            .expect("interaction target should exist");

        assert_eq!(target.node_id, "upper");
        assert_eq!(
            target
                .handlers
                .hover_change
                .as_ref()
                .map(|handler| handler.handler_for(true)),
            Some("upper-hover-true")
        );
    }

    #[test]
    fn pointer_events_none_skips_button_hit_test() {
        let root =
            DecorationNode::new(DecorationNodeKind::Box(BoxNode::default())).with_children(vec![
                DecorationNode::new(DecorationNodeKind::Button(ButtonNode {
                    action: WindowAction::Maximize,
                }))
                .with_style(DecorationStyle {
                    width: Some(100),
                    height: Some(20),
                    ..Default::default()
                }),
                DecorationNode::new(DecorationNodeKind::Button(ButtonNode {
                    action: WindowAction::Close,
                }))
                .with_style(DecorationStyle {
                    position: Some(StylePosition::Absolute),
                    z_index: Some(10),
                    width: Some(100),
                    height: Some(20),
                    pointer_events: Some(PointerEvents::None),
                    ..Default::default()
                }),
                DecorationNode::new(DecorationNodeKind::WindowSlot),
            ]);

        let layout = DecorationTree::new(root)
            .layout(LogicalRect::new(0, 0, 100, 40))
            .expect("layout should succeed");

        assert_eq!(
            layout.hit_test(LogicalPoint::new(5, 5)),
            DecorationHitTestResult::Action(WindowAction::Maximize)
        );
    }

    #[test]
    fn transform_translates_and_scales_subtree_geometry() {
        let root =
            DecorationNode::new(DecorationNodeKind::Box(BoxNode::default())).with_children(vec![
                DecorationNode::new(DecorationNodeKind::Button(ButtonNode {
                    action: WindowAction::Close,
                }))
                .with_style(DecorationStyle {
                    width: Some(20),
                    height: Some(10),
                    transform: Some(NodeTransform {
                        translate_x: 5.0,
                        translate_y: 2.0,
                        scale_x: 2.0,
                        scale_y: 1.0,
                    }),
                    ..Default::default()
                }),
                DecorationNode::new(DecorationNodeKind::WindowSlot),
            ]);

        let layout = DecorationTree::new(root)
            .layout(LogicalRect::new(0, 0, 100, 40))
            .expect("layout should succeed");

        assert_eq!(
            layout.root.children[0].rect,
            LogicalRect::new(-5, 2, 40, 10)
        );
        assert_eq!(
            layout.hit_test(LogicalPoint::new(0, 5)),
            DecorationHitTestResult::Action(WindowAction::Close)
        );
    }

    #[test]
    fn overflow_hidden_keeps_bounds_at_node_rect() {
        let root = DecorationNode::new(DecorationNodeKind::Box(BoxNode::default()))
            .with_style(DecorationStyle {
                position: Some(StylePosition::Relative),
                overflow: Some(Overflow::Hidden),
                ..Default::default()
            })
            .with_children(vec![
                DecorationNode::new(DecorationNodeKind::Box(BoxNode::default())).with_style(
                    DecorationStyle {
                        position: Some(StylePosition::Absolute),
                        width: Some(20),
                        height: Some(20),
                        inset: PositionOffsets {
                            top: Some(-10),
                            left: Some(-10),
                            ..Default::default()
                        },
                        ..Default::default()
                    },
                ),
                DecorationNode::new(DecorationNodeKind::WindowSlot),
            ]);

        let layout = DecorationTree::new(root)
            .layout(LogicalRect::new(0, 0, 100, 40))
            .expect("layout should succeed");

        assert_eq!(layout.bounds_rect(), LogicalRect::new(0, 0, 100, 40));
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
                .with_children(vec![
                    title,
                    DecorationNode::new(DecorationNodeKind::WindowSlot),
                ]),
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
            .position(|primitive| {
                matches!(
                    primitive,
                    DecorationRenderPrimitive::FillRect { rect, color, .. }
                        if *rect == LogicalRect::new(0, 0, 10, 10) && *color == Color::WHITE
                )
            })
            .expect("root background should exist");

        assert!(root_border_index < child_background_index);
        assert!(child_background_index < root_background_index);
    }

    #[test]
    fn render_primitives_apply_opacity_to_colors() {
        let root =
            DecorationNode::new(DecorationNodeKind::WindowBorder).with_style(DecorationStyle {
                background: Some(Color::rgba(255, 0, 0, 255)),
                opacity: Some(0.5),
                ..Default::default()
            });

        let layout = DecorationTree::new(root)
            .layout(LogicalRect::new(0, 0, 10, 10))
            .expect_err("layout should fail without window slot");
        assert_eq!(
            layout,
            DecorationLayoutError::Validation(DecorationValidationError::MissingWindowSlot)
        );

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
        assert!(
            primitives
                .iter()
                .all(|primitive| !matches!(primitive, DecorationRenderPrimitive::FillRect { .. }))
        );
        assert!(primitives.iter().any(|primitive| matches!(
            primitive,
            DecorationRenderPrimitive::WindowSlot { rect } if *rect == LogicalRect::new(0, 0, 10, 10)
        )));
    }

    #[test]
    fn invisible_subtree_emits_no_primitives() {
        let root = DecorationNode::new(DecorationNodeKind::WindowBorder).with_children(vec![
            DecorationNode::new(DecorationNodeKind::Box(BoxNode::default()))
                .with_style(DecorationStyle {
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
                    DecorationNode::new(DecorationNodeKind::Label(LabelNode { text: "A".into() }))
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
            DecorationNode::new(DecorationNodeKind::Label(LabelNode { text: "A".into() }))
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
            DecorationNode::new(DecorationNodeKind::Label(LabelNode { text: "A".into() }))
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
