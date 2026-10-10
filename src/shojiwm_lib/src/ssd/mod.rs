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

pub mod bridge;
mod evaluator;
mod integration;
mod interaction;
pub mod paint;
pub mod popup;
mod popup_input;
pub mod window_model;

use smithay::utils::Logical;

use crate::backend::text::{LabelSpec, measure_label_intrinsic};

pub use bridge::{
    DecorationBridgeError, WireCompiledEffect, WireDecorationChild, WireDecorationNode, WireProps,
    WireStyle, WireWindowAction, WireWindowEffectConfig, decode_tree_json,
};
pub use evaluator::{
    DecorationCachedEvaluationResult, DecorationEvaluationError, DecorationEvaluationResult,
    DecorationEvaluator, DecorationGestureSwipeAsyncInvocation, DecorationHandlerInvocation,
    DecorationKeyBindingInvocation, DecorationPointerMoveAsyncInvocation,
    DecorationSchedulerTick, DecorationWindowMoveInvocation,
    DecorationWindowResizeInvocation, DecorationWindowStateRequestInvocation,
    LayerEffectEvaluationResult, PopupEffectEvaluationResult,
    RuntimeEventConfigUpdate, RuntimeLayerEffectAssignment, RuntimePopupEffectAssignment,
    RuntimeWindowAction, StaticDecorationEvaluator, evaluate_dynamic_decoration,
    validate_layer_effect_config, validate_popup_effect_config,
};
pub use integration::{
    CachedDecorationBuffer, ContentClip, EffectEvaluationCacheEntry,
    WindowDecorationState,
};
#[cfg(test)]
pub(crate) use integration::paint_buffers_for_layout;
pub use interaction::DecorationInteractionSnapshot;
pub(crate) use popup_input::PopupInputState;
pub use popup::{
    DecorationPart, PopupAlign, PopupCollision, PopupDismissHandlers, PopupDismissReason,
    PopupHandlers, PopupInfo, PopupLayer, PopupMode, PopupNode, PopupPlacement, PopupScopes,
    set_popup_viewports,
};
pub use window_model::{
    GestureSwipeEventSnapshot, GestureSwipePhaseSnapshot, LayerKindSnapshot, LayerPositionSnapshot,
    ManagedWindowAnimationEasingSnapshot, ManagedWindowAnimationMode,
    ManagedWindowAnimationSnapshot, ManagedWindowPointAnimationSnapshot,
    ManagedWindowPointSnapshot, ManagedWindowRectAnimationSnapshot, ManagedWindowRectSnapshot,
    HdmiLinkSnapshot, ManagedWindowScalarAnimationSnapshot, ManagedWindowState, OutputModeSnapshot,
    OutputPositionSnapshot, OutputSubpixelSnapshot, OutputTransformSnapshot,
    PointerHitTargetSnapshot,
    PointerModifierStateSnapshot,
    PointerMoveEventSnapshot, PointerMovePointSnapshot, PopupParentKindSnapshot, TransformOrigin,
    WaylandLayerSnapshot, WaylandOutputSnapshot, WaylandPopupSnapshot, WaylandWindowAction,
    WaylandWindowSnapshot, WindowActivateRequestEventSnapshot, WindowActivateRequestSourceSnapshot,
    WindowDecorationDecisionSnapshot, WindowDecorationModeSnapshot,
    WindowDecorationPolicyContextSnapshot, WindowDecorationPolicyReasonSnapshot,
    WindowDecorationProtocolSnapshot, WindowDecorationStateSnapshot,
    WindowFullscreenRequestEventSnapshot, WindowIconSnapshot, WindowMaximizeRequestEventSnapshot,
    WindowMinimizeRequestEventSnapshot, WindowMoveEventSnapshot, WindowMovePhaseSnapshot,
    WindowMoveSourceSnapshot, WindowPositionSnapshot, WindowResizeEdgesSnapshot,
    WindowResizeEventSnapshot, WindowResizePhaseSnapshot, WindowResizePointSnapshot,
    WindowResizeSourceSnapshot, WindowStateRequestSourceSnapshot, WindowTransform,
    layer_runtime_id, popup_runtime_id,
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

        let mut root = layout_node_with_scale(&self.root, bounds, None, scale, (0, 0))?;
        root.sync_root_bounds();
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
    /// The client slot in logical pixels, derived from the root rect and the
    /// decoration insets. Each inset is the physical inset rounded to logical
    /// pixels on its own, so it is the same for every root size: converting a
    /// root size to a client size and back is exact. Rounding the slot's
    /// edges instead would make the right/bottom inset depend on the parity of
    /// the size at fractional scales, and the root -> configure -> layout
    /// round trip would flip the client size by one pixel on every pass.
    pub fn window_slot_rect(&self) -> Option<LogicalRect> {
        let root = &self.root;
        let slot = root.resolved_window_slot_rect()?;
        let frame = root.frame;
        let bounds = root.resolved_rect;
        let left = frame.logical_len_rounded(slot.x - bounds.x);
        let top = frame.logical_len_rounded(slot.y - bounds.y);
        let right = frame.logical_len_rounded(bounds.right() - slot.right());
        let bottom = frame.logical_len_rounded(bounds.bottom() - slot.bottom());
        Some(LogicalRect::new(
            root.rect.x + left,
            root.rect.y + top,
            root.rect.width - left - right,
            root.rect.height - top - bottom,
        ))
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
        self.hit_test_at(point.x as f64, point.y as f64)
    }

    /// `hit_test` for a fractional logical point. The point is mapped onto
    /// the layout's physical pixel grid and tested against the same pixel
    /// rects that are rendered, so input matches what is on screen exactly.
    pub fn hit_test_at(&self, x: f64, y: f64) -> DecorationHitTestResult {
        let point = self.root.frame.layout_point(x, y);
        // Interactive popups sit above everything else of the window.
        if let Some(hit) = popup::popup_hit(&self.root, point) {
            return find_button_action(hit.popup, point)
                .map(DecorationHitTestResult::Action)
                .unwrap_or(DecorationHitTestResult::Popup);
        }
        if let Some(action) = find_button_action(&self.root, point) {
            return DecorationHitTestResult::Action(action);
        }

        if let Some(slot_rect) = self.root.resolved_window_slot_rect() {
            if let Some(border) = self.root.window_border_style() {
                let scale = self.root.frame.scale;
                let hit_area = self
                    .root
                    .window_border_resize_hit_area()
                    .unwrap_or_default();
                let edge_width = hit_area
                    .edge_width
                    .map(|width| ResolvedLayoutValue::from_logical(width as f64, scale))
                    .unwrap_or_else(|| ResolvedLayoutValue::border_width(border.width, scale));
                let corner_width = hit_area
                    .corner_width
                    .map(|width| ResolvedLayoutValue::from_logical(width as f64, scale))
                    .unwrap_or(edge_width);
                if let Some(edges) = hit_test_resize_edges(
                    self.root.resolved_rect,
                    edge_width.raw(),
                    corner_width.raw(),
                    point,
                ) {
                    return DecorationHitTestResult::Resize(edges);
                }
            }

            if slot_rect.contains_point(point) {
                return DecorationHitTestResult::ClientArea;
            }
        }

        if self.root.resolved_rect.contains_point(point) {
            return DecorationHitTestResult::Move;
        }

        DecorationHitTestResult::Outside
    }

    pub fn interaction_target_at(
        &self,
        point: LogicalPoint,
    ) -> Option<DecorationInteractionTarget> {
        self.interaction_target_at_precise(point.x as f64, point.y as f64)
    }

    pub fn interaction_target_at_precise(
        &self,
        x: f64,
        y: f64,
    ) -> Option<DecorationInteractionTarget> {
        self.interaction_targets_at_precise(x, y).into_iter().next()
    }

    /// The nodes with interaction handlers that the pointer at a global
    /// logical point is on, innermost first. Outside popups that is the
    /// innermost one only; inside an interactive popup it is every node of
    /// the chain down to it, the popup's ancestors included (see
    /// `ssd::popup`).
    pub fn interaction_targets_at_precise(&self, x: f64, y: f64) -> Vec<DecorationInteractionTarget> {
        let point = self.root.frame.layout_point(x, y);
        match popup::popup_hit(&self.root, point) {
            Some(hit) => popup::hover_chain(&hit, point),
            None => find_interaction_target(&self.root, point).into_iter().collect(),
        }
    }

    /// Whether the decoration itself takes the pointer at a global logical
    /// point inside the client area: an interactive popup, a button, a resize
    /// edge, or a node with handlers other than the client slot's ancestors
    /// (those only wrap the client). Such points stay on this window where
    /// the client's input region has a hole, since the decoration under the
    /// client shows through it.
    pub fn takes_input_over_client_at(&self, x: f64, y: f64) -> bool {
        if matches!(
            self.hit_test_at(x, y),
            DecorationHitTestResult::Action(_)
                | DecorationHitTestResult::Popup
                | DecorationHitTestResult::Resize(_)
        ) {
            return true;
        }
        let point = self.root.frame.layout_point(x, y);
        has_interaction_target_beside_client(&self.root, point)
    }

    /// Every `<Popup>` whose anchor is shown, for the compositor's input
    /// handling.
    pub fn popups(&self) -> Vec<popup::PopupInfo> {
        popup::popups(&self.root)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ComputedDecorationNode {
    pub stable_id: Option<String>,
    pub interaction: DecorationInteractionHandlers,
    pub window_border_interaction: WindowBorderInteraction,
    pub kind: DecorationNodeKind,
    pub style: DecorationStyle,
    pub rect: LogicalRect,
    pub(crate) resolved_rect: ResolvedLogicalRect,
    pub(crate) resolved_content_rect: ResolvedLogicalRect,
    pub(crate) resolved_border_width: ResolvedLayoutValue,
    pub(crate) resolved_border_radius: ResolvedLayoutValue,
    pub effective_clip: Option<DecorationClip>,
    pub(crate) resolved_effective_clip: Option<ResolvedDecorationClip>,
    pub(crate) frame: LayoutFrame,
    /// Accumulated `transform` scale of this node and its ancestors. Rects
    /// are already transformed; painted lengths (border, radius) use this.
    pub(crate) transform_scale: (f64, f64),
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
    pub(crate) fn to_logical_clip(self, frame: LayoutFrame) -> DecorationClip {
        DecorationClip {
            rect: frame.logical_rect(self.rect),
            radius: frame.logical_len_rounded(self.radius),
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
            return self.style.border.or_else(|| {
                self.style
                    .has_border()
                    .then(|| self.style.border_sides().into_iter().flatten().next())
                    .flatten()
            });
        }

        self.children.iter().find_map(Self::window_border_style)
    }

    fn window_border_resize_hit_area(&self) -> Option<WindowResizeHitArea> {
        if matches!(self.kind, DecorationNodeKind::WindowBorder) {
            return self.window_border_interaction.resize_hit_area;
        }

        self.children
            .iter()
            .find_map(Self::window_border_resize_hit_area)
    }

    pub(crate) fn bounds_rect(&self) -> LogicalRect {
        self.frame.logical_rect(self.resolved_bounds_rect())
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

            for child in self
                .children
                .iter()
                .filter(|child| !matches!(child.kind, DecorationNodeKind::Popup(_)))
            {
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

        for child in self.children.iter().filter(|child| {
            !child.style.is_absolute_positioned()
                && !matches!(child.kind, DecorationNodeKind::Popup(_))
        }) {
            has_flow_child = true;
            let child_bounds = child.resolved_layout_bounds_rect();
            let (layout_min_x, layout_min_y, layout_max_x, layout_max_y) =
                match self.layout_direction_for_bounds() {
                    Some(LayoutDirection::Row) => (
                        child_bounds.x,
                        child.resolved_rect.y,
                        child_bounds.x + child_bounds.width,
                        child.resolved_rect.y + child.resolved_rect.height,
                    ),
                    Some(LayoutDirection::Column) => (
                        child.resolved_rect.x,
                        child_bounds.y,
                        child.resolved_rect.x + child.resolved_rect.width,
                        child_bounds.y + child_bounds.height,
                    ),
                    None => (
                        child_bounds.x,
                        child_bounds.y,
                        child_bounds.x + child_bounds.width,
                        child_bounds.y + child_bounds.height,
                    ),
                };
            child_min_x = child_min_x.min(layout_min_x);
            child_min_y = child_min_y.min(layout_min_y);
            child_max_x = child_max_x.max(layout_max_x);
            child_max_y = child_max_y.max(layout_max_y);
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

    pub(crate) fn sync_root_bounds(&mut self) {
        let frame = self.frame;
        self.resolved_rect = self.resolved_layout_bounds_rect();
        // The extra physical pixels of a sub-pixel root size never change the
        // root's logical rect: that rect feeds the client configure size, which
        // must stay on the integer size the window manager asked for.
        let mut logical_bounds = self.resolved_rect;
        logical_bounds.width = ResolvedLayoutValue::from_raw(
            (logical_bounds.width.raw() - frame.root_extra_px.0).max(0),
        );
        logical_bounds.height = ResolvedLayoutValue::from_raw(
            (logical_bounds.height.raw() - frame.root_extra_px.1).max(0),
        );
        self.rect = frame.logical_rect(logical_bounds);
        self.resolved_content_rect = self
            .resolved_rect
            .inset(self.style.resolved_content_inset(frame.scale));
        self.resolved_effective_clip = effective_clip_for_node_resolved(
            &self.to_decoration_node(),
            None,
            self.resolved_content_rect,
            frame.scale,
        );
        self.effective_clip = self
            .resolved_effective_clip
            .map(|clip| clip.to_logical_clip(frame));
    }

    fn layout_direction_for_bounds(&self) -> Option<LayoutDirection> {
        match &self.kind {
            DecorationNodeKind::Box(layout) => Some(layout.direction),
            DecorationNodeKind::ShaderEffect(effect) => Some(effect.direction),
            DecorationNodeKind::Button(_) => Some(LayoutDirection::Column),
            DecorationNodeKind::Popup(_) => Some(LayoutDirection::Column),
            _ => None,
        }
    }

    fn to_decoration_node(&self) -> DecorationNode {
        DecorationNode {
            stable_id: self.stable_id.clone(),
            interaction: self.interaction.clone(),
            window_border_interaction: self.window_border_interaction,
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
        radius: Option<f64>,
    },
    BorderRect {
        rect: LogicalRect,
        width: f64,
        color: Color,
        radius: Option<f64>,
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
    /// On an interactive `<Popup>`, away from its buttons: nothing happens.
    Popup,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DecorationInteractionHandlers {
    pub hover_change: Option<DecorationStateChangeHandler>,
    pub active_change: Option<DecorationStateChangeHandler>,
    /// `<Popup>` nodes only.
    pub popup: Option<Box<popup::PopupHandlers>>,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WindowBorderInteraction {
    pub resize_hit_area: Option<WindowResizeHitArea>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WindowResizeHitArea {
    pub edge_width: Option<i32>,
    pub corner_width: Option<i32>,
}

impl WindowResizeHitArea {
    pub fn uniform(width: i32) -> Self {
        Self {
            edge_width: Some(width),
            corner_width: Some(width),
        }
    }
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
    pub window_border_interaction: WindowBorderInteraction,
    pub kind: DecorationNodeKind,
    pub style: DecorationStyle,
    pub children: Vec<DecorationNode>,
}

impl DecorationNode {
    pub fn new(kind: DecorationNodeKind) -> Self {
        Self {
            stable_id: None,
            interaction: DecorationInteractionHandlers::default(),
            window_border_interaction: WindowBorderInteraction::default(),
            kind,
            style: DecorationStyle::default(),
            children: Vec::new(),
        }
    }

    pub fn with_style(mut self, style: DecorationStyle) -> Self {
        self.style = style;
        self
    }

    pub fn with_window_border_interaction(mut self, interaction: WindowBorderInteraction) -> Self {
        self.window_border_interaction = interaction;
        self
    }

    pub fn with_children(mut self, children: Vec<DecorationNode>) -> Self {
        self.children = children;
        self
    }

    pub fn push_child(&mut self, child: DecorationNode) {
        self.children.push(child);
    }

    /// Absolutely positioned nodes and popups take no space in their parent.
    fn is_out_of_flow(&self) -> bool {
        self.style.is_absolute_positioned() || matches!(self.kind, DecorationNodeKind::Popup(_))
    }

    /// The style the computed node carries: a hint popup takes no pointer
    /// input, and a closed popup is hidden.
    fn computed_style(&self) -> DecorationStyle {
        let mut style = self.style.clone();
        if let DecorationNodeKind::Popup(popup) = &self.kind {
            if !popup.mode.is_interactive() {
                style.pointer_events = Some(PointerEvents::None);
            }
            if !popup.open {
                style.visible = Some(false);
            }
        }
        style
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
    /// Children laid out as a column next to the parent and drawn outside the
    /// window (see `ssd::popup`).
    Popup(PopupNode),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[derive(Default)]
pub enum ImageFit {
    #[default]
    Contain,
    Cover,
    Fill,
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
    FloatArray(Vec<f32>),
    Vec2Array(Vec<[f32; 2]>),
    Vec3Array(Vec<[f32; 3]>),
    Vec4Array(Vec<[f32; 4]>),
}

impl ShaderUniformValue {
    pub fn shape_matches(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Float(_), Self::Float(_))
            | (Self::Vec2(_), Self::Vec2(_))
            | (Self::Vec3(_), Self::Vec3(_))
            | (Self::Vec4(_), Self::Vec4(_)) => true,
            (Self::FloatArray(left), Self::FloatArray(right)) => left.len() == right.len(),
            (Self::Vec2Array(left), Self::Vec2Array(right)) => left.len() == right.len(),
            (Self::Vec3Array(left), Self::Vec3Array(right)) => left.len() == right.len(),
            (Self::Vec4Array(left), Self::Vec4Array(right)) => left.len() == right.len(),
            _ => false,
        }
    }

    pub(crate) fn shape_key(&self) -> String {
        match self {
            Self::Float(_) => "f1".to_owned(),
            Self::Vec2(_) => "f2".to_owned(),
            Self::Vec3(_) => "f3".to_owned(),
            Self::Vec4(_) => "f4".to_owned(),
            Self::FloatArray(values) => format!("a1x{}", values.len()),
            Self::Vec2Array(values) => format!("a2x{}", values.len()),
            Self::Vec3Array(values) => format!("a3x{}", values.len()),
            Self::Vec4Array(values) => format!("a4x{}", values.len()),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ShaderStage {
    pub shader: ShaderModule,
    pub uniforms: std::collections::BTreeMap<String, ShaderUniformValue>,
    pub textures: std::collections::BTreeMap<String, EffectInput>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectStateTextureFormat {
    Rgba8,
    Rg16f,
    Rgba16f,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectStateResizePolicy {
    Clear,
    Stretch,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EffectStateTexture {
    pub name: String,
    pub scale: f32,
    pub format: EffectStateTextureFormat,
    pub resize: EffectStateResizePolicy,
}

#[derive(Debug, Clone, PartialEq)]
pub enum EffectInput {
    Backdrop,
    XrayBackdrop,
    WindowSource(WindowSourceInclude),
    LayerSource(WindowSourceInclude),
    PopupSource(WindowSourceInclude),
    Shader(ShaderStage),
    Image(String),
    Named(String),
    State(EffectStateTexture),
}

impl EffectInput {
    fn uses_backdrop(&self) -> bool {
        match self {
            Self::Backdrop => true,
            Self::Shader(shader) => shader.textures.values().any(Self::uses_backdrop),
            _ => false,
        }
    }

    fn uses_xray_backdrop(&self) -> bool {
        match self {
            Self::XrayBackdrop => true,
            Self::Shader(shader) => shader.textures.values().any(Self::uses_xray_backdrop),
            _ => false,
        }
    }

    fn uses_window_source(&self) -> bool {
        match self {
            Self::WindowSource(_) => true,
            Self::Shader(shader) => shader.textures.values().any(Self::uses_window_source),
            _ => false,
        }
    }

    fn uses_layer_source(&self) -> bool {
        match self {
            Self::LayerSource(_) => true,
            Self::Shader(shader) => shader.textures.values().any(Self::uses_layer_source),
            _ => false,
        }
    }

    fn uses_popup_source(&self) -> bool {
        match self {
            Self::PopupSource(_) => true,
            Self::Shader(shader) => shader.textures.values().any(Self::uses_popup_source),
            _ => false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowSourceInclude {
    Full,
    RootSurface,
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
        damage_padding: i32,
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
    RenderTo {
        target: EffectStateTexture,
        effect: Box<CompiledEffect>,
        /// `None`: plain `renderTo()`, re-run every time the outer pipeline runs (temporal
        /// feedback relies on this). `Some(deps)`: `renderToIfDirty()`, re-run only when one of
        /// the declared subject sources changed since the state was last written.
        depends_on: Option<Vec<EffectDependency>>,
    },
}

/// A subject source a `renderToIfDirty()` side pipeline declares it depends on. The executor
/// compares a content signature of the captured subject; the include mode is part of what
/// gets captured, so it does not need separate handling here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EffectDependency {
    WindowSource,
    LayerSource,
    PopupSource,
}

/// How the alpha channel of an effect's output is treated when the pipeline
/// result is materialized and composited onto the screen.
///
/// Backdrop captures are rendered into an FBO cleared to transparent black,
/// and any part of the effect rect not covered by scene elements (outsets,
/// anti-artifact margins, screen edges, gaps between elements) keeps alpha 0.
/// The dual-kawase blur chain then smears those border texels inward, so the
/// alpha of a plain backdrop pipeline is *noise*, not signal — compositing it
/// as-is would show dark halos and see-through fringes at the blur edges.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EffectAlphaMode {
    /// Force alpha to 1.0 at the end of the pipeline. Correct for the common
    /// "frosted glass" case: the backdrop is by definition already-composited
    /// screen content, which has no meaningful transparency. This hides the
    /// capture/blur alpha noise described above. Default.
    #[default]
    Opaque,
    /// Keep the pipeline's alpha output intact through the finish pass and
    /// the final composite. For pipelines that intentionally produce
    /// transparency (e.g. masking the blur against a layer's own alpha).
    /// Opting in means the pipeline itself is responsible for producing
    /// meaningful alpha everywhere, including the blur edge regions.
    Preserve,
}

/// How the compositor treats the opaque region a client declared on its
/// surface (`wl_surface.set_opaque_region`).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum OpaqueRegionPolicy {
    /// Honor the declaration: opaque areas can occlude elements below and be
    /// rendered without blending. Correct and fastest for honest clients.
    #[default]
    Trust,
    /// Discard the declaration and treat the whole surface as potentially
    /// transparent. For clients that over-declare (e.g. GTK3 tooltips claim
    /// their full rect opaque despite transparent rounded corners), a trusted
    /// declaration both culls anything composited behind the surface and
    /// paints the "transparent" pixels unblended.
    Ignore,
}

/// Per-surface rendering policy resolved by `COMPOSITOR.rendering.surfacePolicy`.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "camelCase")]
pub struct SurfacePolicy {
    #[serde(default)]
    pub opaque_region: OpaqueRegionPolicy,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CompiledEffect {
    pub input: EffectInput,
    /// Logical padding around the visible content used as the pipeline's
    /// capture and processing area.
    pub capture_padding: i32,
    pub invalidate: EffectInvalidationPolicy,
    pub pipeline: Vec<EffectStage>,
    /// Declared explicitly by the config (`compileEffect({ alpha: ... })`).
    /// Deliberately *not* inferred from pipeline contents (e.g. whether a
    /// layer source is referenced): implicit switching would silently change
    /// edge-artifact handling the moment a texture input is added.
    pub alpha: EffectAlphaMode,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BackgroundEffectConfig {
    pub effect: CompiledEffect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EffectOutsets {
    pub left: i32,
    pub right: i32,
    pub top: i32,
    pub bottom: i32,
}

/// The part of a layer surface that a backdrop `behind` effect covers. The effect
/// captures, runs its pipeline and invalidates over the bounding box of that part
/// (plus outsets) instead of over the whole surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EffectRegion {
    /// The whole surface.
    #[default]
    Surface,
    /// The surface's input region (`wl_surface.set_input_region`). A surface without
    /// one takes input everywhere, so this falls back to the whole surface.
    Input,
    /// The blur region the client asked for through ext-background-effect. No
    /// region means nothing is drawn.
    BlurRegion,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WindowEffectSlot {
    pub effect: CompiledEffect,
    pub outsets: EffectOutsets,
    /// Layer `behind` backdrop effects only; every other slot uses `Surface`.
    pub region: EffectRegion,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct WindowEffectConfig {
    pub behind: Option<WindowEffectSlot>,
    pub behind_root_surface: Option<WindowEffectSlot>,
    pub in_front: Option<WindowEffectSlot>,
    pub replace: Option<WindowEffectSlot>,
    /// Replaces the window's subsurfaces separately from the rest of the window.
    /// While this or `behind_subsurfaces` is set, window sources of the other slots
    /// leave subsurfaces out.
    pub replace_subsurfaces: Option<WindowEffectSlot>,
    /// Drawn behind the window's subsurfaces, over their own bounds.
    pub behind_subsurfaces: Option<WindowEffectSlot>,
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
            EffectInput::Backdrop
                | EffectInput::XrayBackdrop
                | EffectInput::WindowSource(_)
                | EffectInput::LayerSource(_)
                | EffectInput::PopupSource(_)
                | EffectInput::Shader(_)
                | EffectInput::State(_)
        )
    }

    pub fn uses_backdrop_input(&self) -> bool {
        self.input.uses_backdrop()
            || self.pipeline.iter().any(|stage| match stage {
                EffectStage::Blend { input, .. } => input.uses_backdrop(),
                EffectStage::Shader(shader) => {
                    shader.textures.values().any(EffectInput::uses_backdrop)
                }
                EffectStage::Unit(effect) => effect.uses_backdrop_input(),
                EffectStage::RenderTo { effect, .. } => effect.uses_backdrop_input(),
                _ => false,
            })
    }

    pub fn uses_xray_backdrop_input(&self) -> bool {
        self.input.uses_xray_backdrop()
            || self.pipeline.iter().any(|stage| match stage {
                EffectStage::Blend { input, .. } => input.uses_xray_backdrop(),
                EffectStage::Shader(shader) => shader
                    .textures
                    .values()
                    .any(EffectInput::uses_xray_backdrop),
                EffectStage::Unit(effect) => effect.uses_xray_backdrop_input(),
                EffectStage::RenderTo { effect, .. } => effect.uses_xray_backdrop_input(),
                _ => false,
            })
    }

    pub fn uses_window_source_input(&self) -> bool {
        self.input.uses_window_source()
            || self.pipeline.iter().any(|stage| match stage {
                EffectStage::Blend { input, .. } => input.uses_window_source(),
                EffectStage::Shader(shader) => shader
                    .textures
                    .values()
                    .any(EffectInput::uses_window_source),
                EffectStage::Unit(effect) => effect.uses_window_source_input(),
                EffectStage::RenderTo { effect, .. } => effect.uses_window_source_input(),
                _ => false,
            })
    }

    pub fn uses_layer_source_input(&self) -> bool {
        self.input.uses_layer_source()
            || self.pipeline.iter().any(|stage| match stage {
                EffectStage::Blend { input, .. } => input.uses_layer_source(),
                EffectStage::Shader(shader) => {
                    shader.textures.values().any(EffectInput::uses_layer_source)
                }
                EffectStage::Unit(effect) => effect.uses_layer_source_input(),
                EffectStage::RenderTo { effect, .. } => effect.uses_layer_source_input(),
                _ => false,
            })
    }

    pub fn uses_popup_source_input(&self) -> bool {
        self.input.uses_popup_source()
            || self.pipeline.iter().any(|stage| match stage {
                EffectStage::Blend { input, .. } => input.uses_popup_source(),
                EffectStage::Shader(shader) => {
                    shader.textures.values().any(EffectInput::uses_popup_source)
                }
                EffectStage::Unit(effect) => effect.uses_popup_source_input(),
                EffectStage::RenderTo { effect, .. } => effect.uses_popup_source_input(),
                _ => false,
            })
    }

    /// Whether this effect can resolve all of its dynamic inputs from the
    /// framebuffer immediately behind the element.
    pub fn supports_framebuffer_backdrop(&self) -> bool {
        self.uses_backdrop_input()
            && !self.uses_xray_backdrop_input()
            && !self.uses_window_source_input()
            && !self.uses_layer_source_input()
            && !self.uses_popup_source_input()
    }

    /// Layer backdrop effects may additionally sample the layer's own
    /// capture (`layerSource()`) while resolving the backdrop from the
    /// framebuffer.
    pub fn supports_layer_framebuffer_backdrop(&self) -> bool {
        self.uses_backdrop_input()
            && !self.uses_xray_backdrop_input()
            && !self.uses_window_source_input()
            && !self.uses_popup_source_input()
    }

    /// Popup backdrop effects may additionally sample the popup's own
    /// pre-captured texture while resolving the backdrop from the framebuffer.
    pub fn supports_popup_framebuffer_backdrop(&self) -> bool {
        self.uses_backdrop_input()
            && !self.uses_xray_backdrop_input()
            && !self.uses_window_source_input()
            && !self.uses_layer_source_input()
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
            .or(match &self.input {
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
    Unmaximize,
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
    pub width: Option<f64>,
    pub height: Option<f64>,
    pub min_width: Option<f64>,
    pub min_height: Option<f64>,
    pub max_width: Option<f64>,
    pub max_height: Option<f64>,
    pub flex_grow: Option<f32>,
    pub flex_shrink: Option<f32>,
    pub padding: Edges,
    pub margin: Edges,
    pub gap: Option<f64>,
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
    pub border_radius: Option<f64>,
    pub visible: Option<bool>,
    pub cursor: Option<String>,
    pub font_size: Option<f64>,
    pub font_weight: Option<serde_json::Value>,
    pub font_family: Option<Vec<String>>,
    pub text_align: Option<String>,
    pub line_height: Option<f64>,
    /// CSS-like shadows; the first entry is painted on top.
    pub box_shadow: Vec<BoxShadow>,
    /// Replaces the built-in background/border painting of the node.
    pub paint: Option<PaintShader>,
    /// Painted above the node's children.
    pub overlay: Option<PaintShader>,
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

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct PositionOffsets {
    pub top: Option<f64>,
    pub right: Option<f64>,
    pub bottom: Option<f64>,
    pub left: Option<f64>,
}

/// Style edge lengths (padding / margin) in logical pixels. Fractional values
/// are allowed; layout snaps each one to the physical pixel grid.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Edges {
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
    pub left: f64,
}

impl Edges {
    pub fn all(value: f64) -> Self {
        Self {
            top: value,
            right: value,
            bottom: value,
            left: value,
        }
    }

    pub fn symmetric(horizontal: f64, vertical: f64) -> Self {
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

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BorderStyle {
    /// Logical width; any non-zero width covers at least one physical pixel.
    pub width: f64,
    pub color: Color,
}

/// One CSS-like `box-shadow` layer. Lengths are logical pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BoxShadow {
    pub offset_x: f64,
    pub offset_y: f64,
    /// Blur radius (CSS semantics: the Gaussian sigma is half of it).
    pub blur: f64,
    /// Grows (positive) or shrinks (negative) the shadow shape.
    pub spread: f64,
    pub color: Color,
    /// Drawn inside the padding box instead of around the border box.
    pub inset: bool,
}

/// A user paint shader: a GLSL file defining `vec4 paint_main(PaintContext)`.
/// It replaces a node's built-in background/border painting (`paint`) or is
/// drawn above its children (`overlay`). All geometry handed to the shader is
/// in whole physical pixels, resolved by the layout.
#[derive(Debug, Clone, PartialEq)]
pub struct PaintShader {
    pub shader: ShaderModule,
    pub uniforms: std::collections::BTreeMap<String, ShaderUniformValue>,
    /// Logical area drawn around the node in addition to its own rect, for
    /// glows and custom shadows.
    pub outsets: Edges,
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

    pub fn inset_uniform(self, inset: i32) -> Self {
        let width = (self.width - inset * 2).max(0);
        let height = (self.height - inset * 2).max(0);
        Self::new(self.x + inset, self.y + inset, width, height)
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

/// Rounds half-way cases up (towards +inf), the single rounding rule used to
/// put layout lengths on the physical pixel grid.
pub(crate) fn round_half_up(value: f64) -> i32 {
    (value + 0.5).floor() as i32
}

/// Maps the layout's root-local physical pixel grid back to global logical
/// space.
///
/// Layout runs entirely in whole physical pixels relative to the root
/// decoration's origin: every style length is snapped once (`round(v·s)`)
/// when it is resolved, and all positions are sums of those integers. That
/// makes every node edge land exactly on the device grid, independently of
/// where the window sits. Logical values exist only at the boundary, as
/// `origin + px / scale`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct LayoutFrame {
    pub origin_x: f64,
    pub origin_y: f64,
    pub scale: f64,
    /// Physical pixels added to the root beyond `round(size · scale)` to
    /// carry the sub-logical-pixel part of an animated root size. The root's
    /// logical rect deliberately ignores it (see `sync_root_bounds`).
    pub root_extra_px: (i32, i32),
}

impl Default for LayoutFrame {
    fn default() -> Self {
        Self {
            origin_x: 0.0,
            origin_y: 0.0,
            scale: 1.0,
            root_extra_px: (0, 0),
        }
    }
}

impl LayoutFrame {
    pub(crate) fn new(origin_x: f64, origin_y: f64, scale: f64) -> Self {
        Self {
            origin_x,
            origin_y,
            scale: sanitize_layout_scale(scale),
            root_extra_px: (0, 0),
        }
    }

    pub(crate) fn translated(self, dx: f64, dy: f64) -> Self {
        Self {
            origin_x: self.origin_x + dx,
            origin_y: self.origin_y + dy,
            ..self
        }
    }

    /// Maps a global logical point onto the root-local physical pixel grid.
    pub(crate) fn layout_point(self, x: f64, y: f64) -> LayoutPoint {
        LayoutPoint {
            x: (x - self.origin_x) * self.scale,
            y: (y - self.origin_y) * self.scale,
        }
    }

    pub(crate) fn logical_len(self, value: ResolvedLayoutValue) -> f32 {
        (value.0 as f64 / self.scale) as f32
    }

    pub(crate) fn logical_len_rounded(self, value: ResolvedLayoutValue) -> i32 {
        round_half_up(value.0 as f64 / self.scale)
    }

    pub(crate) fn logical_x(self, value: ResolvedLayoutValue) -> f64 {
        self.origin_x + value.0 as f64 / self.scale
    }

    pub(crate) fn logical_y(self, value: ResolvedLayoutValue) -> f64 {
        self.origin_y + value.0 as f64 / self.scale
    }

    pub(crate) fn precise_rect(
        self,
        rect: ResolvedLogicalRect,
    ) -> crate::backend::visual::PreciseLogicalRect {
        crate::backend::visual::PreciseLogicalRect {
            x: self.logical_x(rect.x) as f32,
            y: self.logical_y(rect.y) as f32,
            width: self.logical_len(rect.width),
            height: self.logical_len(rect.height),
        }
    }

    /// Integer logical approximation (edges rounded independently) for
    /// consumers that still work on the logical integer grid, such as hit
    /// testing and protocol-facing sizes.
    pub(crate) fn logical_rect(self, rect: ResolvedLogicalRect) -> LogicalRect {
        let left = round_half_up(self.logical_x(rect.x));
        let top = round_half_up(self.logical_y(rect.y));
        let right = round_half_up(self.logical_x(rect.right()));
        let bottom = round_half_up(self.logical_y(rect.bottom()));
        LogicalRect::new(left, top, right - left, bottom - top)
    }
}

fn sanitize_layout_scale(scale: f64) -> f64 {
    if scale.is_finite() {
        scale.abs().max(0.0001)
    } else {
        1.0
    }
}

/// A point in root-local physical pixels (see `LayoutFrame`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct LayoutPoint {
    x: f64,
    y: f64,
}

/// A layout length or coordinate in whole physical pixels (see `LayoutFrame`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub(crate) struct ResolvedLayoutValue(i32);

impl ResolvedLayoutValue {
    pub(crate) const ZERO: Self = Self(0);

    pub(crate) const fn from_raw(raw: i32) -> Self {
        Self(raw)
    }

    pub(crate) const fn raw(self) -> i32 {
        self.0
    }

    /// Snaps a logical length onto the physical grid of `scale`.
    pub(crate) fn from_logical(value: f64, scale: f64) -> Self {
        Self(round_half_up(value * sanitize_layout_scale(scale)))
    }

    /// Like `from_logical`, but a positive width never collapses to zero: a
    /// thin border stays visible as one physical pixel.
    pub(crate) fn border_width(value: f64, scale: f64) -> Self {
        if value > 0.0 {
            Self(Self::from_logical(value, scale).0.max(1))
        } else {
            Self::ZERO
        }
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
    pub(crate) top: ResolvedLayoutValue,
    pub(crate) right: ResolvedLayoutValue,
    pub(crate) bottom: ResolvedLayoutValue,
    pub(crate) left: ResolvedLayoutValue,
}

impl ResolvedLayoutEdges {
    fn from_edges(edges: Edges, scale: f64) -> Self {
        Self {
            top: ResolvedLayoutValue::from_logical(edges.top, scale),
            right: ResolvedLayoutValue::from_logical(edges.right, scale),
            bottom: ResolvedLayoutValue::from_logical(edges.bottom, scale),
            left: ResolvedLayoutValue::from_logical(edges.left, scale),
        }
    }

}

/// A rect in root-local physical pixels (see `LayoutFrame`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct ResolvedLogicalRect {
    pub(crate) x: ResolvedLayoutValue,
    pub(crate) y: ResolvedLayoutValue,
    pub(crate) width: ResolvedLayoutValue,
    pub(crate) height: ResolvedLayoutValue,
}

impl ResolvedLogicalRect {
    pub(crate) fn from_px(x: i32, y: i32, width: i32, height: i32) -> Self {
        Self {
            x: ResolvedLayoutValue(x),
            y: ResolvedLayoutValue(y),
            width: ResolvedLayoutValue(width.max(0)),
            height: ResolvedLayoutValue(height.max(0)),
        }
    }

    pub(crate) fn right(self) -> ResolvedLayoutValue {
        self.x + self.width
    }

    /// Half-open pixel coverage: a point on the right/bottom edge is outside.
    pub(crate) fn contains_point(self, point: LayoutPoint) -> bool {
        point.x >= self.x.0 as f64
            && point.y >= self.y.0 as f64
            && point.x < self.right().0 as f64
            && point.y < self.bottom().0 as f64
    }

    pub(crate) fn bottom(self) -> ResolvedLayoutValue {
        self.y + self.height
    }

    pub(crate) fn inset(self, edges: ResolvedLayoutEdges) -> Self {
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

    /// Applies a node transform around `origin` (physical pixels). Translation
    /// is given in logical pixels; the transformed edges are snapped back onto
    /// the physical grid.
    fn transform_around(self, origin: (f64, f64), transform: NodeTransform, scale: f64) -> Self {
        let scale = sanitize_layout_scale(scale);
        let translate_x = transform.translate_x as f64 * scale;
        let translate_y = transform.translate_y as f64 * scale;
        let map_x = |value: ResolvedLayoutValue| {
            origin.0 + (value.0 as f64 - origin.0) * transform.scale_x as f64 + translate_x
        };
        let map_y = |value: ResolvedLayoutValue| {
            origin.1 + (value.0 as f64 - origin.1) * transform.scale_y as f64 + translate_y
        };
        let (left, right) = (map_x(self.x), map_x(self.right()));
        let (top, bottom) = (map_y(self.y), map_y(self.bottom()));
        let min_x = round_half_up(left.min(right));
        let min_y = round_half_up(top.min(bottom));
        let max_x = round_half_up(left.max(right));
        let max_y = round_half_up(top.max(bottom));
        Self::from_px(min_x, min_y, max_x - min_x, max_y - min_y)
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

pub(super) fn layout_node_with_scale(
    node: &DecorationNode,
    rect: LogicalRect,
    window_slot_size: Option<(i32, i32)>,
    scale: f64,
    root_extra_px: (i32, i32),
) -> Result<ComputedDecorationNode, DecorationLayoutError> {
    // The root's physical size derives from its logical size alone, so the
    // whole tree is position independent; the window's placement enters only
    // through the frame origin.
    let mut frame = LayoutFrame::new(rect.x as f64, rect.y as f64, scale);
    frame.root_extra_px = root_extra_px;
    let root_rect = ResolvedLogicalRect::from_px(
        0,
        0,
        round_half_up(rect.width as f64 * frame.scale) + root_extra_px.0,
        round_half_up(rect.height as f64 * frame.scale) + root_extra_px.1,
    );
    layout_node_resolved(node, root_rect, None, window_slot_size, frame, root_rect)
}

fn layout_node_resolved(
    node: &DecorationNode,
    resolved_rect: ResolvedLogicalRect,
    inherited_clip: Option<ResolvedDecorationClip>,
    window_slot_size: Option<(i32, i32)>,
    frame: LayoutFrame,
    containing_block: ResolvedLogicalRect,
) -> Result<ComputedDecorationNode, DecorationLayoutError> {
    let scale = frame.scale;
    let resolved_border_width = node.style.resolved_border_width(scale);
    let resolved_border_radius = node.style.resolved_border_radius(scale);
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
            resolved_rect,
            effective_clip,
            window_slot_size,
            frame,
            child_containing_block,
        )?,
        DecorationNodeKind::ShaderEffect(effect) => layout_box_children(
            node,
            content_rect,
            effect.direction,
            resolved_rect,
            effective_clip,
            window_slot_size,
            frame,
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
            resolved_rect,
            effective_clip,
            window_slot_size,
            frame,
            child_containing_block,
        )?,
        DecorationNodeKind::Popup(_) => layout_box_children(
            node,
            content_rect,
            LayoutDirection::Column,
            resolved_rect,
            effective_clip,
            window_slot_size,
            frame,
            child_containing_block,
        )?,
        _ if node.children.is_empty() => Vec::new(),
        _ => node
            .children
            .iter()
            .map(|child| {
                if let DecorationNodeKind::Popup(popup) = &child.kind {
                    return layout_popup(child, popup, resolved_rect, window_slot_size, frame);
                }
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
                    frame,
                    child_containing_block,
                )
            })
            .collect::<Result<Vec<_>, _>>()?,
    };

    let mut computed = ComputedDecorationNode {
        stable_id: node.stable_id.clone(),
        interaction: node.interaction.clone(),
        window_border_interaction: node.window_border_interaction,
        kind: node.kind.clone(),
        style: node.computed_style(),
        rect: frame.logical_rect(resolved_rect),
        resolved_rect,
        resolved_content_rect: content_rect,
        resolved_border_width,
        resolved_border_radius,
        effective_clip: effective_clip.map(|clip| clip.to_logical_clip(frame)),
        resolved_effective_clip: effective_clip,
        frame,
        transform_scale: (1.0, 1.0),
        children,
    };

    apply_node_transform(&mut computed);
    Ok(computed)
}

pub(super) fn reapply_tree_preserving_layout(
    computed: &mut ComputedDecorationNode,
    node: &DecorationNode,
    inherited_clip: Option<ResolvedDecorationClip>,
) {
    let frame = computed.frame;
    let scale = frame.scale;
    computed.stable_id = node.stable_id.clone();
    computed.interaction = node.interaction.clone();
    computed.window_border_interaction = node.window_border_interaction;
    computed.kind = node.kind.clone();
    computed.style = node.computed_style();
    let content_rect = computed
        .resolved_rect
        .inset(node.style.resolved_content_inset(scale));
    let effective_clip =
        effective_clip_for_node_resolved(node, inherited_clip, content_rect, scale);
    computed.rect = frame.logical_rect(computed.resolved_rect);
    computed.resolved_content_rect = content_rect;
    computed.resolved_border_width = node.style.resolved_border_width(scale);
    computed.resolved_border_radius = node.style.resolved_border_radius(scale);
    computed.effective_clip = effective_clip.map(|clip| clip.to_logical_clip(frame));
    computed.resolved_effective_clip = effective_clip;

    for (computed_child, node_child) in computed.children.iter_mut().zip(node.children.iter()) {
        let inherited_clip = if matches!(node_child.kind, DecorationNodeKind::Popup(_)) {
            None
        } else {
            computed.resolved_effective_clip
        };
        reapply_tree_preserving_layout(computed_child, node_child, inherited_clip);
    }
}

fn layout_box_children(
    node: &DecorationNode,
    content_rect: ResolvedLogicalRect,
    direction: LayoutDirection,
    anchor: ResolvedLogicalRect,
    effective_clip: Option<ResolvedDecorationClip>,
    window_slot_size: Option<(i32, i32)>,
    frame: LayoutFrame,
    containing_block: ResolvedLogicalRect,
) -> Result<Vec<ComputedDecorationNode>, DecorationLayoutError> {
    if node.children.is_empty() {
        return Ok(Vec::new());
    }
    let scale = frame.scale;

    let flow_children = node
        .children
        .iter()
        .enumerate()
        .filter(|(_, child)| !child.is_out_of_flow())
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
    let layout_debug_enabled = crate::env_flag!("SHOJI_GAP_LAYOUT_CHILD_DEBUG");
    let direction_name = match direction {
        LayoutDirection::Row => "row",
        LayoutDirection::Column => "column",
    };
    let parent_stable_id = node.stable_id.as_deref().unwrap_or("<none>");

    for (flow_position, ((index, child), main_size)) in flow_children
        .into_iter()
        .zip(allocated)
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
        );
        let cross_origin = margin_box_cross_origin + margin_cross_start;

        let child_rect = direction.rect_resolved(
            cursor + margin_main_start,
            cross_origin,
            child_main_size,
            cross_size,
        );
        if layout_debug_enabled {
            let (parent_cross_start, parent_cross_len, child_cross_start, child_cross_len) =
                match direction {
                    LayoutDirection::Row => (
                        content_rect.y.raw(),
                        content_rect.height.raw(),
                        child_rect.y.raw(),
                        child_rect.height.raw(),
                    ),
                    LayoutDirection::Column => (
                        content_rect.x.raw(),
                        content_rect.width.raw(),
                        child_rect.x.raw(),
                        child_rect.width.raw(),
                    ),
                };
            let child_center_twice_px = child_cross_start * 2 + child_cross_len;
            let parent_center_twice_px = parent_cross_start * 2 + parent_cross_len;
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
                DecorationNodeKind::Popup(_) => "popup",
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
                cursor_px = cursor.raw(),
                main_size_px = main_size.raw(),
                cross_available_px = cross_available.raw(),
                cross_size_px = cross_size.raw(),
                cross_origin_px = cross_origin.raw(),
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
            frame,
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
        if let DecorationNodeKind::Popup(popup) = &child.kind {
            children[index] = Some(layout_popup(child, popup, anchor, window_slot_size, frame)?);
        } else if child.style.is_absolute_positioned() {
            let child_rect =
                absolute_child_rect_resolved(child, containing_block, window_slot_size, scale);
            children[index] = Some(layout_node_resolved(
                child,
                child_rect,
                effective_clip,
                window_slot_size,
                frame,
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
    let snap = |value: f64| ResolvedLayoutValue::from_logical(value, scale);
    let left = offsets.left.map(snap);
    let right = offsets.right.map(snap);
    let top = offsets.top.map(snap);
    let bottom = offsets.bottom.map(snap);

    let auto_size = child.auto_size_resolved(window_slot_size, scale);
    let width = match (child.style.width, left, right) {
        (Some(width), _, _) => snap(width),
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
        (Some(height), _, _) => snap(height),
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

/// Lays out a `<Popup>` next to `anchor` (its parent's border box), sized by
/// its content and free of its ancestors' clips.
fn layout_popup(
    node: &DecorationNode,
    popup: &PopupNode,
    anchor: ResolvedLogicalRect,
    window_slot_size: Option<(i32, i32)>,
    frame: LayoutFrame,
) -> Result<ComputedDecorationNode, DecorationLayoutError> {
    let scale = frame.scale;
    let snap = |value: f64| ResolvedLayoutValue::from_logical(value, scale);
    let auto_size = node.auto_size_resolved(window_slot_size, scale);
    let clamp = |value: ResolvedLayoutValue, min: Option<f64>, max: Option<f64>| {
        let value = max.map(snap).map_or(value, |max| value.min(max));
        min.map(snap).map_or(value, |min| value.max(min))
    };
    let style = &node.style;
    let width = clamp(
        style
            .width
            .map(snap)
            .or(auto_size.map(|(width, _)| width))
            .unwrap_or(ResolvedLayoutValue::ZERO),
        style.min_width,
        style.max_width,
    );
    let height = clamp(
        style
            .height
            .map(snap)
            .or(auto_size.map(|(_, height)| height))
            .unwrap_or(ResolvedLayoutValue::ZERO),
        style.min_height,
        style.max_height,
    );
    let rect = popup::popup_rect(popup, anchor, (width, height), frame);
    layout_node_resolved(node, rect, None, window_slot_size, frame, rect)
}

fn apply_node_transform(node: &mut ComputedDecorationNode) {
    let Some(transform) = node.style.transform else {
        return;
    };
    if transform.is_identity() {
        return;
    }

    let origin = (
        node.resolved_rect.x.raw() as f64 + node.resolved_rect.width.raw() as f64 * 0.5,
        node.resolved_rect.y.raw() as f64 + node.resolved_rect.height.raw() as f64 * 0.5,
    );
    transform_subtree(node, origin, transform);
}

fn transform_subtree(
    node: &mut ComputedDecorationNode,
    origin: (f64, f64),
    transform: NodeTransform,
) {
    let frame = node.frame;
    node.resolved_rect = node
        .resolved_rect
        .transform_around(origin, transform, frame.scale);
    node.rect = frame.logical_rect(node.resolved_rect);
    node.resolved_content_rect =
        node.resolved_content_rect
            .transform_around(origin, transform, frame.scale);
    if let Some(clip) = &mut node.resolved_effective_clip {
        clip.rect = clip.rect.transform_around(origin, transform, frame.scale);
    }
    node.effective_clip = node
        .resolved_effective_clip
        .map(|clip| clip.to_logical_clip(frame));
    node.transform_scale = (
        node.transform_scale.0 * transform.scale_x.abs() as f64,
        node.transform_scale.1 * transform.scale_y.abs() as f64,
    );

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
            .map(|value| ResolvedLayoutValue::from_logical(value, scale))
            .unwrap_or_else(|| {
                self.auto_size_resolved(window_slot_size, scale)
                    .map(|(width, height)| match direction {
                        LayoutDirection::Row => width,
                        LayoutDirection::Column => height,
                    })
                    .unwrap_or(ResolvedLayoutValue::ZERO)
            });

        self.style.clamp_main_resolved(direction, fallback, scale)
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
            .map(|value| ResolvedLayoutValue::from_logical(value, scale))
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

    fn flex_grow_for_layout(&self) -> f32 {
        self.style.flex_grow.unwrap_or({
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

    fn intrinsic_size_resolved(
        &self,
        window_slot_size: Option<(i32, i32)>,
        scale: f64,
    ) -> Option<(ResolvedLayoutValue, ResolvedLayoutValue)> {
        match &self.kind {
            DecorationNodeKind::Label(label) => {
                let font_size = self.style.font_size.unwrap_or(13.0).max(1.0) as f32;
                let line_height = self
                    .style
                    .line_height
                    .map(|value| value as f32)
                    .unwrap_or(font_size + 4.0)
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
                    raster_scale: 1.0,
                };
                let (width, height) = measure_label_intrinsic(&spec);
                Some((
                    ResolvedLayoutValue::from_logical(width as f64, scale),
                    ResolvedLayoutValue::from_logical(height as f64, scale),
                ))
            }
            // The client buffer covers `round(size · scale)` physical pixels.
            DecorationNodeKind::WindowSlot => window_slot_size.map(|(width, height)| {
                (
                    ResolvedLayoutValue::from_logical(width as f64, scale),
                    ResolvedLayoutValue::from_logical(height as f64, scale),
                )
            }),
            _ => None,
        }
    }

    fn auto_size_resolved(
        &self,
        window_slot_size: Option<(i32, i32)>,
        scale: f64,
    ) -> Option<(ResolvedLayoutValue, ResolvedLayoutValue)> {
        self.intrinsic_size_resolved(window_slot_size, scale)
            .or_else(|| self.content_based_size_resolved(window_slot_size, scale))
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
            DecorationNodeKind::Popup(_) => Some(self.stack_content_size_resolved(
                LayoutDirection::Column,
                window_slot_size,
                scale,
            )),
            _ => None,
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
            .filter(|child| !child.is_out_of_flow())
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
            .filter(|child| !child.is_out_of_flow())
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

    /// The widest side of the border (the single width used where a uniform
    /// border is assumed, e.g. the inner radius of a clip).
    pub(crate) fn resolved_border_width(&self, scale: f64) -> ResolvedLayoutValue {
        let edges = self.resolved_border_edges(scale);
        edges.top.max(edges.right).max(edges.bottom).max(edges.left)
    }

    pub(crate) fn resolved_border_radius(&self, scale: f64) -> ResolvedLayoutValue {
        ResolvedLayoutValue::from_logical(self.border_radius.unwrap_or(0.0).max(0.0), scale)
    }

    /// The border of one side: `borderTop` etc. override `border`.
    pub(crate) fn border_sides(&self) -> [Option<BorderStyle>; 4] {
        [
            self.border_top.or(self.border),
            self.border_right.or(self.border),
            self.border_bottom.or(self.border),
            self.border_left.or(self.border),
        ]
    }

    pub(crate) fn has_border(&self) -> bool {
        self.border_sides()
            .iter()
            .any(|side| side.is_some_and(|border| border.width > 0.0))
    }

    /// Per-side border widths on the physical grid.
    pub(crate) fn resolved_border_edges(&self, scale: f64) -> ResolvedLayoutEdges {
        let [top, right, bottom, left] = self.border_sides().map(|side| {
            side.map(|border| ResolvedLayoutValue::border_width(border.width, scale))
                .unwrap_or(ResolvedLayoutValue::ZERO)
        });
        ResolvedLayoutEdges {
            top,
            right,
            bottom,
            left,
        }
    }

    fn resolved_content_inset(&self, scale: f64) -> ResolvedLayoutEdges {
        let border = self.resolved_border_edges(scale);
        let padding = ResolvedLayoutEdges::from_edges(self.padding, scale);
        ResolvedLayoutEdges {
            top: padding.top + border.top,
            right: padding.right + border.right,
            bottom: padding.bottom + border.bottom,
            left: padding.left + border.left,
        }
    }

    /// Resolved padding on the physical grid.
    pub(crate) fn resolved_padding(&self, scale: f64) -> ResolvedLayoutEdges {
        ResolvedLayoutEdges::from_edges(self.padding, scale)
    }

    fn resolved_margin(&self, scale: f64) -> ResolvedLayoutEdges {
        ResolvedLayoutEdges::from_edges(self.margin, scale)
    }

    fn resolved_gap(&self, scale: f64) -> ResolvedLayoutValue {
        ResolvedLayoutValue::from_logical(self.gap.unwrap_or(0.0).max(0.0), scale)
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
        // `open` only toggles visibility: a closed popup is laid out too.
        (DecorationNodeKind::Popup(left), DecorationNodeKind::Popup(right)) => {
            PopupNode { open: true, ..*left } == PopupNode { open: true, ..*right }
        }
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

fn clamp_size_resolved(
    value: ResolvedLayoutValue,
    min: Option<f64>,
    max: Option<f64>,
    scale: f64,
) -> ResolvedLayoutValue {
    let mut value = ResolvedLayoutValue::from_raw(value.raw().max(0));
    if let Some(min) = min {
        value = value.max(ResolvedLayoutValue::from_logical(min.max(0.0), scale));
    }
    if let Some(max) = max {
        value = value.min(ResolvedLayoutValue::from_logical(max.max(0.0), scale));
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

    fn cross_origin_for_child_resolved(
        self,
        rect: ResolvedLogicalRect,
        align: Option<AlignItems>,
        child_cross_size: ResolvedLayoutValue,
    ) -> ResolvedLayoutValue {
        let (start, len) = match self {
            LayoutDirection::Row => (rect.y, rect.height),
            LayoutDirection::Column => (rect.x, rect.width),
        };
        match align.unwrap_or(AlignItems::Stretch) {
            AlignItems::Center => ResolvedLayoutValue::from_raw(
                start.raw() + (len.raw() - child_cross_size.raw()).max(0) / 2,
            ),
            AlignItems::End => start + len - child_cross_size,
            AlignItems::Start | AlignItems::Stretch => start,
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
    radius: Option<f64>,
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

fn find_button_action(node: &ComputedDecorationNode, point: LayoutPoint) -> Option<WindowAction> {
    if node.style.visible == Some(false) || !node.style.pointer_events_enabled() {
        return None;
    }
    if node
        .resolved_effective_clip
        .is_some_and(|clip| !clip.rect.contains_point(point))
    {
        return None;
    }

    for child in paint_ordered_children(node)
        .into_iter()
        .filter(|child| !matches!(child.kind, DecorationNodeKind::Popup(_)))
    {
        if let Some(action) = find_button_action(child, point) {
            return Some(action);
        }
    }

    match &node.kind {
        DecorationNodeKind::Button(button) if node.resolved_rect.contains_point(point) => {
            Some(button.action.clone())
        }
        _ => None,
    }
}

fn find_interaction_target(
    node: &ComputedDecorationNode,
    point: LayoutPoint,
) -> Option<DecorationInteractionTarget> {
    if node.style.visible == Some(false) || !node.style.pointer_events_enabled() {
        return None;
    }
    if node
        .resolved_effective_clip
        .is_some_and(|clip| !clip.rect.contains_point(point))
    {
        return None;
    }

    for child in paint_ordered_children(node)
        .into_iter()
        .filter(|child| !matches!(child.kind, DecorationNodeKind::Popup(_)))
    {
        if let Some(target) = find_interaction_target(child, point) {
            return Some(target);
        }
    }

    if node.resolved_rect.contains_point(point) && node.interaction.has_any() {
        let node_id = node.stable_id.clone()?;
        return Some(DecorationInteractionTarget {
            node_id,
            handlers: node.interaction.clone(),
        });
    }

    None
}

/// `find_interaction_target` without the nodes that contain the client slot.
fn has_interaction_target_beside_client(node: &ComputedDecorationNode, point: LayoutPoint) -> bool {
    if node.style.visible == Some(false) || !node.style.pointer_events_enabled() {
        return false;
    }
    if node
        .resolved_effective_clip
        .is_some_and(|clip| !clip.rect.contains_point(point))
    {
        return false;
    }
    if node
        .children
        .iter()
        .filter(|child| !matches!(child.kind, DecorationNodeKind::Popup(_)))
        .any(|child| has_interaction_target_beside_client(child, point))
    {
        return true;
    }
    node.resolved_rect.contains_point(point)
        && node.interaction.has_any()
        && node.stable_id.is_some()
        && node.resolved_window_slot_rect().is_none()
}

fn hit_test_resize_edges(
    rect: ResolvedLogicalRect,
    edge_width: i32,
    corner_width: i32,
    point: LayoutPoint,
) -> Option<ResizeEdges> {
    let edge_width = edge_width.max(0) as f64;
    let corner_width = (corner_width as f64).max(edge_width).max(0.0);
    if edge_width == 0.0 || !rect.contains_point(point) {
        return None;
    }

    let left = rect.x.0 as f64;
    let top = rect.y.0 as f64;
    let right = rect.right().0 as f64;
    let bottom = rect.bottom().0 as f64;
    let on_left = point.x < left + edge_width;
    let on_right = point.x >= right - edge_width;
    let on_top = point.y < top + edge_width;
    let on_bottom = point.y >= bottom - edge_width;
    let near_left_corner = point.x < left + corner_width;
    let near_right_corner = point.x >= right - corner_width;
    let near_top_corner = point.y < top + corner_width;
    let near_bottom_corner = point.y >= bottom - corner_width;

    let mut edges = ResizeEdges::empty();
    if on_top {
        edges |= ResizeEdges::TOP;
        if near_left_corner {
            edges |= ResizeEdges::LEFT;
        } else if near_right_corner {
            edges |= ResizeEdges::RIGHT;
        }
    }
    if on_bottom {
        edges |= ResizeEdges::BOTTOM;
        if near_left_corner {
            edges |= ResizeEdges::LEFT;
        } else if near_right_corner {
            edges |= ResizeEdges::RIGHT;
        }
    }
    if on_left {
        edges |= ResizeEdges::LEFT;
        if near_top_corner {
            edges |= ResizeEdges::TOP;
        } else if near_bottom_corner {
            edges |= ResizeEdges::BOTTOM;
        }
    }
    if on_right {
        edges |= ResizeEdges::RIGHT;
        if near_top_corner {
            edges |= ResizeEdges::TOP;
        } else if near_bottom_corner {
            edges |= ResizeEdges::BOTTOM;
        }
    }

    (!edges.is_empty()).then_some(edges)
}

fn effective_clip_for_node_resolved(
    node: &DecorationNode,
    inherited_clip: Option<ResolvedDecorationClip>,
    content_rect: ResolvedLogicalRect,
    scale: f64,
) -> Option<ResolvedDecorationClip> {
    let node_clip = node_clips_children(node).then(|| ResolvedDecorationClip {
        rect: content_rect,
        radius: (node.style.resolved_border_radius(scale)
            - node.style.resolved_border_width(scale))
        .max(ResolvedLayoutValue::ZERO),
    });

    match (inherited_clip, node_clip) {
        (Some(parent), Some(current)) => intersect_resolved_decoration_clips(parent, current),
        (Some(parent), None) => Some(parent),
        (None, Some(current)) => Some(current),
        (None, None) => None,
    }
}

fn node_clips_children(node: &DecorationNode) -> bool {
    // A container border defines the visible inner edge for its descendants.
    // `overflow: visible` is the explicit escape hatch. WindowSlot is filtered
    // separately when the client-surface clip is derived, because it is a leaf
    // placement marker rather than an SSD container.
    node.style.clips_children()
        || (node.style.has_border() && !matches!(node.style.overflow, Some(Overflow::Visible)))
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
            width: 2.0,
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
                width: 2.0,
                color: Color::WHITE,
            }),
            ..Default::default()
        })
        .with_children(vec![
            DecorationNode::new(DecorationNodeKind::WindowBorder)
                .with_style(DecorationStyle {
                    border: Some(BorderStyle {
                        width: 2.0,
                        color: Color::WHITE,
                    }),
                    border_radius: Some(20.0),
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
                .raw(),
            18
        );
    }

    #[test]
    fn bordered_box_clips_children_by_default() {
        let root = DecorationNode::new(DecorationNodeKind::Box(BoxNode {
            direction: LayoutDirection::Column,
        }))
        .with_style(DecorationStyle {
            border: Some(BorderStyle {
                width: 2.0,
                color: Color::WHITE,
            }),
            border_radius: Some(20.0),
            ..Default::default()
        })
        .with_children(vec![
            DecorationNode::new(DecorationNodeKind::Box(BoxNode::default()))
                .with_children(vec![DecorationNode::new(DecorationNodeKind::WindowSlot)]),
        ]);

        let layout = DecorationTree::new(root)
            .layout(LogicalRect::new(0, 0, 100, 50))
            .expect("layout should succeed");

        assert_eq!(
            layout.root.children[0]
                .resolved_effective_clip
                .expect("border clip should propagate to children")
                .radius
                .raw(),
            18
        );
    }

    #[test]
    fn overflow_hidden_box_clips_children_with_rounded_radius() {
        let root = DecorationNode::new(DecorationNodeKind::Box(BoxNode {
            direction: LayoutDirection::Column,
        }))
        .with_style(DecorationStyle {
            overflow: Some(Overflow::Hidden),
            border: Some(BorderStyle {
                width: 2.0,
                color: Color::WHITE,
            }),
            border_radius: Some(20.0),
            ..Default::default()
        })
        .with_children(vec![
            DecorationNode::new(DecorationNodeKind::Box(BoxNode::default()))
                .with_children(vec![DecorationNode::new(DecorationNodeKind::WindowSlot)]),
        ]);

        let layout = DecorationTree::new(root)
            .layout(LogicalRect::new(0, 0, 100, 50))
            .expect("layout should succeed");
        let clip = layout.root.children[0]
            .resolved_effective_clip
            .expect("child should inherit rounded clip");

        assert_eq!(clip.radius.raw(), 18);
    }

    #[test]
    fn overflow_hidden_button_clips_children_with_rounded_radius() {
        let root = DecorationNode::new(DecorationNodeKind::Button(ButtonNode {
            action: WindowAction::Close,
        }))
        .with_style(DecorationStyle {
            overflow: Some(Overflow::Hidden),
            border: Some(BorderStyle {
                width: 1.0,
                color: Color::WHITE,
            }),
            border_radius: Some(12.0),
            ..Default::default()
        })
        .with_children(vec![DecorationNode::new(DecorationNodeKind::WindowSlot)]);

        let layout = DecorationTree::new(root)
            .layout(LogicalRect::new(0, 0, 100, 50))
            .expect("layout should succeed");
        let clip = layout.root.children[0]
            .resolved_effective_clip
            .expect("button child should inherit rounded clip");

        assert_eq!(clip.radius.raw(), 11);
    }

    #[test]
    fn column_box_allocates_remaining_space_to_window_slot() {
        let titlebar = DecorationNode::new(DecorationNodeKind::Box(BoxNode {
            direction: LayoutDirection::Row,
        }))
        .with_style(DecorationStyle {
            height: Some(28.0),
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
            width: Some(100.0),
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
            width: Some(20.0),
            ..Default::default()
        });

        let root = DecorationNode::new(DecorationNodeKind::Box(BoxNode {
            direction: LayoutDirection::Row,
        }))
        .with_style(DecorationStyle {
            gap: Some(4.0),
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
            width: Some(4.0),
            height: Some(4.0),
            ..Default::default()
        });
        let button = DecorationNode::new(DecorationNodeKind::Button(ButtonNode {
            action: WindowAction::Close,
        }))
        .with_style(DecorationStyle {
            width: Some(16.0),
            height: Some(16.0),
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
            width: Some(4.0),
            height: Some(4.0),
            ..Default::default()
        });
        let button = DecorationNode::new(DecorationNodeKind::Button(ButtonNode {
            action: WindowAction::Close,
        }))
        .with_style(DecorationStyle {
            width: Some(16.0),
            height: Some(16.0),
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
                width: Some(10.0),
                height: Some(10.0),
                ..Default::default()
            })
        };
        let toolbar = DecorationNode::new(DecorationNodeKind::Box(BoxNode {
            direction: LayoutDirection::Row,
        }))
        .with_style(DecorationStyle {
            width: Some(100.0),
            height: Some(10.0),
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
            height: Some(30.0),
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
                    width: 2.0,
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
                capture_padding: 0,
                invalidate: EffectInvalidationPolicy::Always,
                pipeline: Vec::new(),
                alpha: EffectAlphaMode::Opaque,
            },
        }))
        .with_style(DecorationStyle {
            padding: Edges {
                top: 6.0,
                right: 6.0,
                bottom: 6.0,
                left: 6.0,
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
            height: Some(30.0),
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
            height: Some(30.0),
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
                    width: 2.0,
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
                capture_padding: 0,
                invalidate: EffectInvalidationPolicy::Always,
                pipeline: Vec::new(),
                alpha: EffectAlphaMode::Opaque,
            },
        }))
        .with_style(DecorationStyle {
            padding: Edges {
                top: 6.0,
                right: 6.0,
                bottom: 6.0,
                left: 6.0,
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
                    top: Some(0.0),
                    right: Some(0.0),
                    bottom: Some(0.0),
                    left: Some(0.0),
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
                    height: Some(20.0),
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
                    width: Some(10.0),
                    height: Some(10.0),
                    margin: Edges {
                        left: 32.0,
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
                        width: Some(10.0),
                        height: Some(10.0),
                        inset: PositionOffsets {
                            left: Some(5.0),
                            ..Default::default()
                        },
                        margin: Edges {
                            left: 7.0,
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
                    left: Some(4.0),
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        let right = DecorationNode::new(DecorationNodeKind::Box(BoxNode::default())).with_style(
            DecorationStyle {
                position: Some(StylePosition::Absolute),
                inset: PositionOffsets {
                    left: Some(8.0),
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
                    width: Some(100.0),
                    height: Some(20.0),
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
                    width: Some(100.0),
                    height: Some(20.0),
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
            width: Some(100.0),
            height: Some(20.0),
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
            width: Some(100.0),
            height: Some(20.0),
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
                    width: Some(100.0),
                    height: Some(20.0),
                    ..Default::default()
                }),
                DecorationNode::new(DecorationNodeKind::Button(ButtonNode {
                    action: WindowAction::Close,
                }))
                .with_style(DecorationStyle {
                    position: Some(StylePosition::Absolute),
                    z_index: Some(10),
                    width: Some(100.0),
                    height: Some(20.0),
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
                    width: Some(20.0),
                    height: Some(10.0),
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
                        width: Some(20.0),
                        height: Some(20.0),
                        inset: PositionOffsets {
                            top: Some(-10.0),
                            left: Some(-10.0),
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
            height: Some(24.0),
            color: Some(Color::BLACK),
            ..Default::default()
        });

        let root = DecorationNode::new(DecorationNodeKind::WindowBorder)
            .with_style(DecorationStyle {
                background: Some(Color::WHITE),
                border: Some(BorderStyle {
                    width: 2.0,
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
                if *rect == LogicalRect::new(0, 0, 100, 40) && *width == 2.0 && *color == Color::BLACK
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
                width: 1.0,
                color: Color::BLACK,
            }),
            ..Default::default()
        })
        .with_children(vec![
            DecorationNode::new(DecorationNodeKind::Box(BoxNode {
                direction: LayoutDirection::Column,
            }))
            .with_style(DecorationStyle {
                height: Some(4.0),
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
                    if *rect == LogicalRect::new(0, 0, 10, 10) && *width == 1.0 && *color == Color::BLACK
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
                width: Some(20.0),
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
                    height: Some(20.0),
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
                height: Some(20.0),
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
                    width: 4.0,
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
    fn hit_test_uses_window_border_resize_hit_area() {
        let root = DecorationNode::new(DecorationNodeKind::WindowBorder)
            .with_style(DecorationStyle {
                border: Some(BorderStyle {
                    width: 2.0,
                    color: Color::WHITE,
                }),
                ..Default::default()
            })
            .with_window_border_interaction(WindowBorderInteraction {
                resize_hit_area: Some(WindowResizeHitArea {
                    edge_width: Some(8),
                    corner_width: Some(14),
                }),
            })
            .with_children(vec![DecorationNode::new(DecorationNodeKind::WindowSlot)]);

        let layout = DecorationTree::new(root)
            .layout(LogicalRect::new(0, 0, 100, 60))
            .expect("layout should succeed");

        assert_eq!(
            layout.hit_test(LogicalPoint::new(6, 30)),
            DecorationHitTestResult::Resize(ResizeEdges::LEFT)
        );
        assert_eq!(
            layout.hit_test(LogicalPoint::new(10, 1)),
            DecorationHitTestResult::Resize(ResizeEdges::TOP_LEFT)
        );
        assert_eq!(
            layout.hit_test(LogicalPoint::new(20, 1)),
            DecorationHitTestResult::Resize(ResizeEdges::TOP)
        );
    }

    #[test]
    fn style_lengths_snap_to_the_physical_grid() {
        // 3 logical px at 1.5x is 4.5 physical px: ties round up to 5 px
        // (3.333 logical), never to a fractional pixel.
        assert_eq!(ResolvedLayoutValue::from_logical(3.0, 1.5).raw(), 5);
        assert_eq!(ResolvedLayoutValue::from_logical(1.75, 1.5).raw(), 3);
        assert_eq!(ResolvedLayoutValue::from_logical(1.6, 1.25).raw(), 2);
        // A thin but non-zero border stays visible as one physical pixel.
        assert_eq!(ResolvedLayoutValue::border_width(0.3, 1.25).raw(), 1);
        assert_eq!(ResolvedLayoutValue::border_width(0.0, 1.25).raw(), 0);
        assert_eq!(ResolvedLayoutValue::from_logical(0.3, 1.25).raw(), 0);
    }

    #[test]
    fn layout_preserves_subpixel_child_offsets_at_fractional_scale() {
        let root = DecorationNode::new(DecorationNodeKind::WindowBorder)
            .with_style(DecorationStyle {
                border: Some(BorderStyle {
                    width: 1.0,
                    color: Color::WHITE,
                }),
                padding: Edges {
                    top: 4.0,
                    right: 4.0,
                    bottom: 4.0,
                    left: 4.0,
                },
                ..Default::default()
            })
            .with_children(vec![
                DecorationNode::new(DecorationNodeKind::Box(BoxNode {
                    direction: LayoutDirection::Row,
                }))
                .with_style(DecorationStyle {
                    gap: Some(3.0),
                    ..Default::default()
                })
                .with_children(vec![
                    DecorationNode::new(DecorationNodeKind::Label(LabelNode { text: "A".into() }))
                        .with_style(DecorationStyle {
                            width: Some(11.0),
                            height: Some(20.0),
                            ..Default::default()
                        }),
                    DecorationNode::new(DecorationNodeKind::AppIcon).with_style(DecorationStyle {
                        width: Some(11.0),
                        height: Some(20.0),
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

        // gap 3 at 1.6x = 4.8 -> 5 physical px.
        assert_eq!(
            (icon.resolved_rect.x - label.resolved_rect.x - label.resolved_rect.width).raw(),
            5
        );
    }

    #[test]
    fn stretched_column_shrinks_auto_child_to_fit_fractional_height() {
        let top_border = DecorationNode::new(DecorationNodeKind::Box(BoxNode {
            direction: LayoutDirection::Row,
        }))
        .with_style(DecorationStyle {
            height: Some(2.0),
            background: Some(Color::BLACK),
            ..Default::default()
        });

        let bottom_border = DecorationNode::new(DecorationNodeKind::Box(BoxNode {
            direction: LayoutDirection::Row,
        }))
        .with_style(DecorationStyle {
            height: Some(2.0),
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
                height: Some(30.0),
                ..Default::default()
            }),
            DecorationNode::new(DecorationNodeKind::Box(BoxNode {
                direction: LayoutDirection::Row,
            }))
            .with_style(DecorationStyle {
                height: Some(30.0),
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
                    width: 2.0,
                    color: Color::WHITE,
                }),
                border_radius: Some(20.0),
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
                        width: Some(2.0),
                        ..Default::default()
                    }),
                    anchor_column,
                    DecorationNode::new(DecorationNodeKind::Box(BoxNode {
                        direction: LayoutDirection::Row,
                    }))
                    .with_style(DecorationStyle {
                        width: Some(2.0),
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
            gap: Some(3.0),
            ..Default::default()
        })
        .with_children(vec![
            DecorationNode::new(DecorationNodeKind::Label(LabelNode { text: "A".into() }))
                .with_style(DecorationStyle {
                    width: Some(11.0),
                    height: Some(20.0),
                    color: Some(Color::WHITE),
                    ..Default::default()
                }),
            DecorationNode::new(DecorationNodeKind::AppIcon).with_style(DecorationStyle {
                width: Some(11.0),
                height: Some(20.0),
                ..Default::default()
            }),
            DecorationNode::new(DecorationNodeKind::WindowSlot),
        ]);

        let updated = DecorationNode::new(DecorationNodeKind::Box(BoxNode {
            direction: LayoutDirection::Row,
        }))
        .with_style(DecorationStyle {
            gap: Some(3.0),
            background: Some(Color::BLACK),
            ..Default::default()
        })
        .with_children(vec![
            DecorationNode::new(DecorationNodeKind::Label(LabelNode { text: "A".into() }))
                .with_style(DecorationStyle {
                    width: Some(11.0),
                    height: Some(20.0),
                    color: Some(Color::BLACK),
                    ..Default::default()
                }),
            DecorationNode::new(DecorationNodeKind::AppIcon).with_style(DecorationStyle {
                width: Some(11.0),
                height: Some(20.0),
                ..Default::default()
            }),
            DecorationNode::new(DecorationNodeKind::WindowSlot),
        ]);

        let mut layout = DecorationTree::new(original)
            .layout_for_client_with_scale(LogicalRect::new(50, 40, 200, 120), 1.6)
            .expect("layout should succeed");
        reapply_tree_preserving_layout(&mut layout.root, &updated, None);

        let label = &layout.root.children[0];
        let icon = &layout.root.children[1];
        assert_eq!(
            (icon.resolved_rect.x - label.resolved_rect.x - label.resolved_rect.width).raw(),
            5
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
                    width: Some(18.0),
                    height: Some(18.0),
                    ..Default::default()
                }),
                DecorationNode::new(DecorationNodeKind::WindowSlot),
            ]),
        );

        let layout = tree
            .layout_for_client_with_scale(LogicalRect::new(0, 0, 100, 60), 1.6)
            .expect("layout should succeed");
        let button = &layout.root.children[0];

        // 18 at 1.6x = 28.8 -> 29 physical px.
        assert_eq!(button.resolved_rect.width.raw(), 29);
        assert_eq!(button.resolved_rect.height.raw(), 29);
    }

    #[test]
    fn flow_child_and_absolute_overlay_share_fractional_parent_edge() {
        let button = DecorationNode::new(DecorationNodeKind::Button(ButtonNode {
            action: WindowAction::Close,
        }))
        .with_style(DecorationStyle {
            width: Some(16.0),
            height: Some(16.0),
            border: Some(BorderStyle {
                width: 1.0,
                color: Color::WHITE,
            }),
            ..Default::default()
        });
        let image = DecorationNode::new(DecorationNodeKind::Image(ImageNode {
            src: "/tmp/icon.svg".into(),
            fit: ImageFit::Contain,
        }))
        .with_style(DecorationStyle {
            width: Some(16.0),
            height: Some(16.0),
            position: Some(StylePosition::Absolute),
            ..Default::default()
        });
        let overlay = DecorationNode::new(DecorationNodeKind::Box(BoxNode::default()))
            .with_style(DecorationStyle {
                width: Some(16.0),
                height: Some(16.0),
                position: Some(StylePosition::Relative),
                ..Default::default()
            })
            .with_children(vec![button, image]);
        let root = DecorationNode::new(DecorationNodeKind::Box(BoxNode {
            direction: LayoutDirection::Row,
        }))
        .with_style(DecorationStyle {
            border: Some(BorderStyle {
                width: 2.0,
                color: Color::WHITE,
            }),
            ..Default::default()
        })
        .with_children(vec![
            overlay,
            DecorationNode::new(DecorationNodeKind::WindowSlot),
        ]);

        let layout = DecorationTree::new(root)
            .layout_for_client_with_scale(LogicalRect::new(0, 0, 100, 30), 1.25)
            .expect("layout should succeed");
        let overlay = &layout.root.children[0];
        let button = &overlay.children[0];
        let image = &overlay.children[1];

        assert_eq!(button.resolved_rect.x, image.resolved_rect.x);
        assert_eq!(button.resolved_rect.y, image.resolved_rect.y);
    }

    #[test]
    fn titlebar_label_shrink_keeps_window_slot_aligned_at_small_width() {
        let close_button = DecorationNode::new(DecorationNodeKind::Box(BoxNode {
            direction: LayoutDirection::Row,
        }))
        .with_style(DecorationStyle {
            position: Some(StylePosition::Relative),
            ..Default::default()
        })
        .with_children(vec![
            DecorationNode::new(DecorationNodeKind::Button(ButtonNode {
                action: WindowAction::Close,
            }))
            .with_style(DecorationStyle {
                width: Some(16.0),
                height: Some(16.0),
                ..Default::default()
            }),
        ]);
        let titlebar = DecorationNode::new(DecorationNodeKind::ShaderEffect(ShaderEffectNode {
            shader: CompiledEffect {
                input: EffectInput::Backdrop,
                capture_padding: 0,
                invalidate: EffectInvalidationPolicy::Always,
                pipeline: Vec::new(),
                alpha: EffectAlphaMode::Opaque,
            },
            direction: LayoutDirection::Row,
        }))
        .with_style(DecorationStyle {
            height: Some(30.0),
            padding: Edges {
                left: 8.0,
                right: 8.0,
                ..Default::default()
            },
            gap: Some(8.0),
            align_items: Some(AlignItems::Center),
            ..Default::default()
        })
        .with_children(vec![
            DecorationNode::new(DecorationNodeKind::AppIcon).with_style(DecorationStyle {
                width: Some(16.0),
                height: Some(16.0),
                ..Default::default()
            }),
            DecorationNode::new(DecorationNodeKind::Label(LabelNode {
                text: "A very long title that should shrink before the chrome breaks".into(),
            }))
            .with_style(DecorationStyle {
                flex_grow: Some(1.0),
                flex_shrink: Some(1.0),
                min_width: Some(0.0),
                ..Default::default()
            }),
            close_button,
        ]);
        let root = DecorationNode::new(DecorationNodeKind::WindowBorder)
            .with_style(DecorationStyle {
                border: Some(BorderStyle {
                    width: 2.0,
                    color: Color::WHITE,
                }),
                border_radius: Some(10.0),
                ..Default::default()
            })
            .with_children(vec![
                DecorationNode::new(DecorationNodeKind::Box(BoxNode {
                    direction: LayoutDirection::Row,
                }))
                .with_children(vec![
                    DecorationNode::new(DecorationNodeKind::Box(BoxNode {
                        direction: LayoutDirection::Column,
                    }))
                    .with_children(vec![
                        titlebar,
                        DecorationNode::new(DecorationNodeKind::WindowSlot),
                    ]),
                ]),
            ]);

        let client_rect = LogicalRect::new(100, 80, 80, 60);
        let layout = DecorationTree::new(root)
            .layout_for_client_with_scale(client_rect, 1.25)
            .expect("layout should succeed");
        let slot = layout
            .window_slot_rect()
            .expect("window slot should be present");

        assert_eq!(slot, client_rect);
        assert_eq!(layout.root.rect.x, client_rect.x - 2);
        // Border 2 at 1.25x is 3 physical px per side, i.e. a constant 2
        // logical px inset; the one-pixel slack is absorbed by the client
        // sampling, not by a size-dependent inset.
        assert_eq!(layout.root.rect.width, client_rect.width + 4);
    }

    /// Every node rendered through the root frame must land exactly on the
    /// physical pixels the layout chose, wherever the window sits and at any
    /// fractional scale: layout and rendering share one pixel grid.
    #[test]
    fn rendered_node_edges_match_layout_pixels_at_fractional_scales() {
        let button = |action| {
            DecorationNode::new(DecorationNodeKind::Button(ButtonNode { action })).with_style(
                DecorationStyle {
                    width: Some(15.5),
                    height: Some(15.5),
                    border: Some(BorderStyle {
                        width: 0.75,
                        color: Color::WHITE,
                    }),
                    border_radius: Some(7.75),
                    ..Default::default()
                },
            )
        };
        let titlebar = DecorationNode::new(DecorationNodeKind::Box(BoxNode {
            direction: LayoutDirection::Row,
        }))
        .with_style(DecorationStyle {
            height: Some(29.5),
            padding: Edges::symmetric(7.25, 0.0),
            gap: Some(5.5),
            align_items: Some(AlignItems::Center),
            justify_content: Some(JustifyContent::SpaceBetween),
            ..Default::default()
        })
        .with_children(vec![
            DecorationNode::new(DecorationNodeKind::Label(LabelNode {
                text: "title".into(),
            }))
            .with_style(DecorationStyle {
                flex_grow: Some(1.0),
                ..Default::default()
            }),
            button(WindowAction::Minimize),
            button(WindowAction::Close),
        ]);
        let root = DecorationNode::new(DecorationNodeKind::WindowBorder)
            .with_style(DecorationStyle {
                border: Some(BorderStyle {
                    width: 1.5,
                    color: Color::WHITE,
                }),
                border_radius: Some(10.0),
                ..Default::default()
            })
            .with_children(vec![
                DecorationNode::new(DecorationNodeKind::Box(BoxNode {
                    direction: LayoutDirection::Column,
                }))
                .with_children(vec![titlebar, DecorationNode::new(DecorationNodeKind::WindowSlot)]),
            ]);
        let tree = DecorationTree::new(root);

        fn check(node: &ComputedDecorationNode, root: LogicalRect, scale: f64) {
            let scale_xy = smithay::utils::Scale::from((scale, scale));
            let output_geo = smithay::utils::Rectangle::new((0, 0).into(), (0, 0).into());
            let mapped = crate::backend::visual::relative_physical_rect_from_root_precise(
                node.frame.precise_rect(node.resolved_rect),
                root,
                Default::default(),
                output_geo,
                scale_xy,
            );
            assert_eq!(
                (mapped.loc.x, mapped.loc.y, mapped.size.w, mapped.size.h),
                (
                    node.resolved_rect.x.raw(),
                    node.resolved_rect.y.raw(),
                    node.resolved_rect.width.raw(),
                    node.resolved_rect.height.raw(),
                ),
                "node {:?} at scale {scale}",
                node.kind,
            );
            for child in &node.children {
                check(child, root, scale);
            }
        }

        for scale in [1.0, 1.25, 1.5, 1.6, 1.75, 1.8, 2.25] {
            for (x, y) in [(0, 0), (1, 3), (777, 401), (2561, 1439)] {
                let layout = tree
                    .layout_for_client_with_scale(LogicalRect::new(x, y, 641, 417), scale)
                    .expect("layout should succeed");
                check(&layout.root, layout.root.rect, scale);
            }
        }
    }

    /// Pointer hit testing uses the rendered pixel rects: at 1.5x a button
    /// whose layout edge falls between logical pixels is hit exactly up to
    /// its last rendered physical pixel and not beyond it.
    #[test]
    fn hit_test_follows_rendered_pixels_at_fractional_scale() {
        let tree = DecorationTree::new(
            DecorationNode::new(DecorationNodeKind::Box(BoxNode {
                direction: LayoutDirection::Row,
            }))
            .with_children(vec![
                DecorationNode::new(DecorationNodeKind::Button(ButtonNode {
                    action: WindowAction::Close,
                }))
                .with_style(DecorationStyle {
                    width: Some(15.5),
                    height: Some(20.0),
                    ..Default::default()
                }),
                DecorationNode::new(DecorationNodeKind::WindowSlot),
            ]),
        );
        let layout = tree
            .layout_with_scale(LogicalRect::new(100, 50, 200, 100), 1.5)
            .expect("layout should succeed");
        // 15.5 logical at 1.5x = 23.25 -> 23 physical px = 15.333 logical.
        let button_right = 100.0 + 23.0 / 1.5;
        assert_eq!(
            layout.hit_test_at(button_right - 0.01, 60.0),
            DecorationHitTestResult::Action(WindowAction::Close)
        );
        assert_eq!(
            layout.hit_test_at(button_right + 0.01, 60.0),
            DecorationHitTestResult::ClientArea
        );
    }

    #[test]
    fn framebuffer_backdrop_support_depends_on_inputs_not_pipeline_shape() {
        let backdrop = CompiledEffect {
            input: EffectInput::Backdrop,
            capture_padding: 0,
            invalidate: EffectInvalidationPolicy::Always,
            pipeline: vec![
                EffectStage::Noise(NoiseStage {
                    kind: NoiseKind::Salt,
                    amount: 0.1,
                }),
                EffectStage::Save("noisy".into()),
                EffectStage::Blend {
                    input: EffectInput::Named("noisy".into()),
                    mode: BlendMode::Screen,
                    alpha: 0.5,
                },
            ],
            alpha: EffectAlphaMode::Opaque,
        };
        assert!(backdrop.supports_framebuffer_backdrop());

        let xray = CompiledEffect {
            input: EffectInput::Backdrop,
            capture_padding: 0,
            invalidate: EffectInvalidationPolicy::Always,
            pipeline: vec![EffectStage::Unit(Box::new(CompiledEffect {
                input: EffectInput::XrayBackdrop,
                capture_padding: 0,
                invalidate: EffectInvalidationPolicy::Always,
                pipeline: Vec::new(),
                alpha: EffectAlphaMode::Opaque,
            }))],
            alpha: EffectAlphaMode::Opaque,
        };
        assert!(!xray.supports_framebuffer_backdrop());

        let window_source = CompiledEffect {
            input: EffectInput::Backdrop,
            capture_padding: 0,
            invalidate: EffectInvalidationPolicy::Always,
            pipeline: vec![EffectStage::Blend {
                input: EffectInput::WindowSource(WindowSourceInclude::Full),
                mode: BlendMode::Normal,
                alpha: 1.0,
            }],
            alpha: EffectAlphaMode::Opaque,
        };
        assert!(!window_source.supports_framebuffer_backdrop());

        let layer_mask = CompiledEffect {
            input: EffectInput::Backdrop,
            capture_padding: 0,
            invalidate: EffectInvalidationPolicy::Always,
            pipeline: vec![EffectStage::Shader(ShaderStage {
                shader: ShaderModule {
                    path: "mask.frag".into(),
                },
                uniforms: std::collections::BTreeMap::new(),
                textures: std::collections::BTreeMap::from([(
                    "layer_mask".into(),
                    EffectInput::LayerSource(WindowSourceInclude::Full),
                )]),
            })],
            alpha: EffectAlphaMode::Opaque,
        };
        assert!(layer_mask.uses_backdrop_input());
        assert!(layer_mask.uses_layer_source_input());
        assert!(!layer_mask.supports_framebuffer_backdrop());

        let popup_mask = CompiledEffect {
            input: EffectInput::Backdrop,
            capture_padding: 0,
            invalidate: EffectInvalidationPolicy::Always,
            pipeline: vec![EffectStage::Shader(ShaderStage {
                shader: ShaderModule {
                    path: "popup-mask.frag".into(),
                },
                uniforms: std::collections::BTreeMap::new(),
                textures: std::collections::BTreeMap::from([(
                    "popup_mask".into(),
                    EffectInput::PopupSource(WindowSourceInclude::Full),
                )]),
            })],
            alpha: EffectAlphaMode::Preserve,
        };
        assert!(!popup_mask.supports_framebuffer_backdrop());
        assert!(popup_mask.supports_popup_framebuffer_backdrop());
    }

    /// A bordered window whose title bar ends with a 20px button carrying a
    /// 60x16 tooltip popup.
    fn popup_tree(popup: PopupNode, with_popup: bool) -> DecorationTree {
        let mut button = DecorationNode::new(DecorationNodeKind::Button(ButtonNode {
            action: WindowAction::Maximize,
        }))
        .with_style(DecorationStyle {
            width: Some(20.0),
            height: Some(20.0),
            ..Default::default()
        });
        button.stable_id = Some("button".into());
        if with_popup {
            let mut tooltip = DecorationNode::new(DecorationNodeKind::Popup(popup)).with_children(
                vec![DecorationNode::new(DecorationNodeKind::Box(BoxNode::default())).with_style(
                    DecorationStyle {
                        width: Some(60.0),
                        height: Some(16.0),
                        background: Some(Color::rgba(0, 0, 0, 255)),
                        ..Default::default()
                    },
                )],
            );
            tooltip.stable_id = Some("tooltip".into());
            tooltip.children[0].stable_id = Some("tooltip-body".into());
            button.push_child(tooltip);
        }
        let titlebar = DecorationNode::new(DecorationNodeKind::Box(BoxNode {
            direction: LayoutDirection::Row,
        }))
        .with_style(DecorationStyle {
            height: Some(20.0),
            ..Default::default()
        })
        .with_children(vec![
            DecorationNode::new(DecorationNodeKind::Box(BoxNode::default())).with_style(
                DecorationStyle {
                    flex_grow: Some(1.0),
                    ..Default::default()
                },
            ),
            button,
        ]);
        DecorationTree::new(
            DecorationNode::new(DecorationNodeKind::WindowBorder)
                .with_style(DecorationStyle {
                    border: Some(BorderStyle {
                        width: 2.0,
                        color: Color::rgba(255, 255, 255, 255),
                    }),
                    border_radius: Some(8.0),
                    ..Default::default()
                })
                .with_children(vec![
                    DecorationNode::new(DecorationNodeKind::Box(BoxNode::default())).with_children(
                        vec![titlebar, DecorationNode::new(DecorationNodeKind::WindowSlot)],
                    ),
                ]),
        )
    }

    fn find_by_id<'a>(
        node: &'a ComputedDecorationNode,
        id: &str,
    ) -> Option<&'a ComputedDecorationNode> {
        if node.stable_id.as_deref() == Some(id) {
            return Some(node);
        }
        node.children.iter().find_map(|child| find_by_id(child, id))
    }

    #[test]
    fn popup_sits_below_its_anchor_outside_the_flow_and_the_clips() {
        set_popup_viewports(Vec::new());
        let popup = PopupNode {
            offset: 4.0,
            ..PopupNode::default()
        };
        let window = LogicalRect::new(100, 100, 300, 200);
        let layout = popup_tree(popup, true).layout(window).expect("layout");
        let without = popup_tree(popup, false).layout(window).expect("layout");

        let button = find_by_id(&layout.root, "button").unwrap();
        assert_eq!(button.rect, find_by_id(&without.root, "button").unwrap().rect);
        let tooltip = find_by_id(&layout.root, "tooltip").unwrap();
        assert_eq!(
            tooltip.rect,
            LogicalRect::new(button.rect.x - 20, button.rect.y + 20 + 4, 60, 16)
        );
        // It reaches past the window's right edge, unclipped by the border.
        assert!(tooltip.rect.x + tooltip.rect.width > window.x + window.width);
        assert!(tooltip.effective_clip.is_none());
        assert!(tooltip.children[0].effective_clip.is_none());
        // The window keeps its size and its input.
        assert_eq!(layout.bounds_rect(), without.bounds_rect());
        let inside_tooltip = LogicalPoint::new(tooltip.rect.x + 10, tooltip.rect.y + 8);
        assert_eq!(layout.hit_test(inside_tooltip), without.hit_test(inside_tooltip));
        assert_eq!(
            layout.hit_test(LogicalPoint::new(button.rect.x + 10, button.rect.y + 10)),
            DecorationHitTestResult::Action(WindowAction::Maximize)
        );

        // Its paint goes to the popup pass, unclipped; the rest stays in the
        // window's pass.
        let scopes = PopupScopes::of(&layout.root);
        let buffers = paint_buffers_for_layout(&layout);
        let (popup_buffers, window_buffers): (Vec<_>, Vec<_>) = buffers
            .iter()
            .partition(|buffer| scopes.layer_of(&buffer.stable_key) == Some(PopupLayer::Top));
        assert_eq!(popup_buffers.len(), 1);
        assert_eq!(popup_buffers[0].owner_node_id.as_deref(), Some("tooltip-body"));
        assert!(popup_buffers[0].paint.geometry.clip.is_none());
        assert!(popup_buffers[0].paint.geometry.rounded_clip.is_none());
        assert!(!window_buffers.is_empty());
    }

    #[test]
    fn closed_popup_is_neither_drawn_nor_in_a_scope() {
        let closed = PopupNode {
            open: false,
            ..PopupNode::default()
        };
        let layout = popup_tree(closed, true)
            .layout(LogicalRect::new(0, 0, 300, 200))
            .expect("layout");
        let tooltip = find_by_id(&layout.root, "tooltip").unwrap();
        assert_eq!(tooltip.style.visible, Some(false));
        assert!(PopupScopes::of(&layout.root).is_empty());
        assert!(
            paint_buffers_for_layout(&layout)
                .iter()
                .all(|buffer| buffer.owner_node_id.as_deref() != Some("tooltip-body"))
        );
        // Opening it does not need a relayout.
        assert!(
            popup_tree(closed, true)
                .root
                .layout_equivalent(&popup_tree(PopupNode::default(), true).root)
        );
    }

    #[test]
    fn popup_flips_above_when_the_output_ends_below_its_anchor() {
        // The output ends just below the title bar.
        set_popup_viewports(vec![LogicalRect::new(0, 0, 1000, 130)]);
        let popup = PopupNode {
            offset: 4.0,
            ..PopupNode::default()
        };
        let layout = popup_tree(popup, true)
            .layout(LogicalRect::new(100, 100, 300, 200))
            .expect("layout");
        set_popup_viewports(Vec::new());
        let button = find_by_id(&layout.root, "button").unwrap();
        let tooltip = find_by_id(&layout.root, "tooltip").unwrap();
        assert_eq!(tooltip.rect.y + tooltip.rect.height + 4, button.rect.y);
    }

    fn hoverable(mut node: DecorationNode, id: &str) -> DecorationNode {
        node.stable_id = Some(id.into());
        node.interaction.hover_change = Some(DecorationStateChangeHandler {
            true_handler: format!("{id}.true"),
            false_handler: format!("{id}.false"),
        });
        node
    }

    fn sized(kind: DecorationNodeKind, width: f64, height: f64) -> DecorationNode {
        DecorationNode::new(kind).with_style(DecorationStyle {
            width: Some(width),
            height: Some(height),
            ..Default::default()
        })
    }

    /// A title bar button ("anchor") with an `Auto` menu below it; the menu
    /// holds a button ("item") that has a submenu of its own.
    fn menu_tree(menu_open: bool) -> DecorationTree {
        let auto = |open: bool, placement: PopupPlacement, layer: PopupLayer| PopupNode {
            open,
            placement,
            layer,
            offset: 4.0,
            mode: PopupMode::Auto,
            ..PopupNode::default()
        };
        let mut submenu = DecorationNode::new(DecorationNodeKind::Popup(auto(
            true,
            PopupPlacement::Right,
            PopupLayer::Window,
        )))
        .with_children(vec![sized(DecorationNodeKind::Box(BoxNode::default()), 30.0, 30.0)]);
        submenu.stable_id = Some("submenu".into());
        let item = hoverable(
            sized(
                DecorationNodeKind::Button(ButtonNode {
                    action: WindowAction::Minimize,
                }),
                20.0,
                20.0,
            ),
            "item",
        )
        .with_children(vec![submenu]);
        let mut menu = DecorationNode::new(DecorationNodeKind::Popup(auto(
            menu_open,
            PopupPlacement::Bottom,
            PopupLayer::Top,
        )))
        .with_children(vec![
            sized(DecorationNodeKind::Box(BoxNode::default()), 80.0, 40.0).with_children(vec![item]),
        ]);
        menu.stable_id = Some("menu".into());
        let anchor = hoverable(
            sized(
                DecorationNodeKind::Button(ButtonNode {
                    action: WindowAction::Maximize,
                }),
                20.0,
                20.0,
            ),
            "anchor",
        )
        .with_children(vec![menu]);
        DecorationTree::new(
            DecorationNode::new(DecorationNodeKind::Box(BoxNode::default())).with_children(vec![
                DecorationNode::new(DecorationNodeKind::Box(BoxNode {
                    direction: LayoutDirection::Row,
                }))
                .with_style(DecorationStyle {
                    height: Some(20.0),
                    ..Default::default()
                })
                .with_children(vec![anchor]),
                DecorationNode::new(DecorationNodeKind::WindowSlot),
            ]),
        )
    }

    fn center(rect: LogicalRect) -> (f64, f64) {
        (
            rect.x as f64 + rect.width as f64 / 2.0,
            rect.y as f64 + rect.height as f64 / 2.0,
        )
    }

    #[test]
    fn only_content_placed_in_the_client_area_takes_input_there() {
        let absolute = |node: DecorationNode, left: f64| {
            let style = DecorationStyle {
                position: Some(StylePosition::Absolute),
                inset: PositionOffsets {
                    top: Some(10.0),
                    left: Some(left),
                    ..Default::default()
                },
                ..node.style.clone()
            };
            node.with_style(style)
        };
        let root = hoverable(
            DecorationNode::new(DecorationNodeKind::Box(BoxNode::default())).with_style(
                DecorationStyle {
                    position: Some(StylePosition::Relative),
                    ..Default::default()
                },
            ),
            "frame",
        )
        .with_children(vec![
            DecorationNode::new(DecorationNodeKind::WindowSlot),
            absolute(
                hoverable(sized(DecorationNodeKind::Box(BoxNode::default()), 30.0, 20.0), "chip"),
                10.0,
            ),
            absolute(
                sized(
                    DecorationNodeKind::Button(ButtonNode {
                        action: WindowAction::Close,
                    }),
                    20.0,
                    20.0,
                ),
                60.0,
            ),
        ]);
        let layout = DecorationTree::new(root)
            .layout(LogicalRect::new(100, 100, 200, 150))
            .expect("layout");
        let chip = find_by_id(&layout.root, "chip").unwrap().rect;
        let (x, y) = center(chip);
        assert!(layout.takes_input_over_client_at(x, y), "a node with handlers in the client area");
        assert!(layout.takes_input_over_client_at(170.0, 120.0), "a button in the client area");
        // The frame only wraps the client: it is hovered there, but a hole in
        // the client's input region still falls through.
        let ids = layout
            .interaction_targets_at_precise(250.0, 200.0)
            .into_iter()
            .map(|target| target.node_id)
            .collect::<Vec<_>>();
        assert_eq!(ids, ["frame"]);
        assert!(!layout.takes_input_over_client_at(250.0, 200.0));
    }

    #[test]
    fn interactive_popup_takes_input_and_keeps_its_anchor_hovered() {
        set_popup_viewports(Vec::new());
        let layout = menu_tree(true)
            .layout(LogicalRect::new(100, 100, 300, 200))
            .expect("layout");
        let anchor = find_by_id(&layout.root, "anchor").unwrap().rect;
        let menu = find_by_id(&layout.root, "menu").unwrap().rect;
        let item = find_by_id(&layout.root, "item").unwrap().rect;
        let ids = |(x, y): (f64, f64)| {
            layout
                .interaction_targets_at_precise(x, y)
                .into_iter()
                .map(|target| target.node_id)
                .collect::<Vec<_>>()
        };

        // A button inside the menu works; the menu's background swallows the
        // press instead of moving the window or reaching the client.
        let (x, y) = center(item);
        assert_eq!(
            layout.hit_test_at(x, y),
            DecorationHitTestResult::Action(WindowAction::Minimize)
        );
        let below_item = (menu.x as f64 + 70.0, menu.y as f64 + 35.0);
        assert_eq!(layout.hit_test_at(below_item.0, below_item.1), DecorationHitTestResult::Popup);
        // Inside the menu the anchor stays hovered (DOM `:hover`); outside,
        // only the innermost node is.
        assert_eq!(ids(center(item)), ["item", "anchor"]);
        assert_eq!(ids(below_item), ["anchor"]);
        assert_eq!(ids(center(anchor)), ["anchor"]);
    }

    #[test]
    fn popup_info_reports_areas_anchor_interest_and_nesting() {
        set_popup_viewports(Vec::new());
        let layout = menu_tree(true)
            .layout(LogicalRect::new(100, 100, 300, 200))
            .expect("layout");
        let anchor = find_by_id(&layout.root, "anchor").unwrap().rect;
        let submenu = find_by_id(&layout.root, "submenu").unwrap().rect;
        let popups = layout.popups();
        assert_eq!(popups.len(), 2);
        let (menu, nested) = (&popups[0], &popups[1]);
        assert_eq!(menu.node_id.as_deref(), Some("menu"));
        assert!(menu.enclosing.is_empty());
        assert_eq!(nested.enclosing, ["menu"]);
        // A nested popup is drawn (and hit) in its outer popup's layer.
        assert_eq!(nested.popup.layer, PopupLayer::Top);

        let (x, y) = center(submenu);
        assert!(menu.contains(x, y), "the menu covers its open submenu");
        assert!(!menu.anchor_contains(x, y));
        let (x, y) = center(anchor);
        assert!(menu.anchor_contains(x, y) && menu.has_interest(x, y));

        // Closed: no area of its own, interest only on the anchor.
        let layout = menu_tree(false)
            .layout(LogicalRect::new(100, 100, 300, 200))
            .expect("layout");
        let popups = layout.popups();
        assert_eq!(popups.len(), 1, "a closed popup hides what it nests");
        assert!(!popups[0].is_open_and_interactive());
        assert!(popups[0].has_interest(x, y));
    }
}
