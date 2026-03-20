use smithay::{
    backend::renderer::element::solid::SolidColorBuffer,
    desktop::Window,
    utils::{Logical, Point, Rectangle},
};
use std::time::{Duration, Instant};
use tracing::{debug, trace, warn};

use crate::state::ShojiWM;
use crate::backend::{
    icon::{CachedDecorationIcon, IconSpec},
    text::{CachedDecorationLabel, LabelSpec},
};
use crate::backend::visual::{inverse_transform_point, transformed_root_rect};
use crate::backend::rounded::RoundedElementState;

use super::{
    ComputedDecorationTree, DecorationEvaluationError, DecorationEvaluationResult, DecorationEvaluator,
    DecorationHitTestResult, DecorationSchedulerTick, DecorationTree, LogicalPoint, LogicalRect,
    StaticDecorationEvaluator, WaylandWindowSnapshot, WindowTransform,
};

#[derive(Debug, Clone)]
pub struct WindowDecorationState {
    pub snapshot: WaylandWindowSnapshot,
    pub tree: DecorationTree,
    pub layout: ComputedDecorationTree,
    pub client_rect: LogicalRect,
    pub visual_transform: WindowTransform,
    pub content_clip: Option<ContentClip>,
    pub buffers: Vec<CachedDecorationBuffer>,
    pub text_buffers: Vec<CachedDecorationLabel>,
    pub icon_buffers: Vec<CachedDecorationIcon>,
    pub rounded_cache: std::collections::HashMap<String, RoundedElementState>,
}

#[derive(Debug, Clone, Copy)]
pub struct ContentClip {
    pub rect: Rectangle<i32, Logical>,
    pub radius: i32,
}

impl WindowDecorationState {
    pub fn hit_test(&self, point: Point<f64, Logical>) -> DecorationHitTestResult {
        let logical = LogicalPoint::new(point.x.floor() as i32, point.y.floor() as i32);
        self.layout.hit_test(logical)
    }
}

#[derive(Debug, Clone)]
pub enum DecorationRuntimeEvaluator {
    Static(super::StaticDecorationEvaluator),
    Node(super::NodeDecorationEvaluator),
}

#[derive(Debug, Clone)]
pub struct CachedDecorationBuffer {
    pub stable_key: String,
    pub rect: LogicalRect,
    pub color: super::Color,
    pub buffer: SolidColorBuffer,
    pub radius: i32,
    pub border_width: i32,
    pub clip_rect: Option<LogicalRect>,
    pub clip_radius: i32,
    pub source_kind: &'static str,
}

impl Default for DecorationRuntimeEvaluator {
    fn default() -> Self {
        Self::Static(super::StaticDecorationEvaluator)
    }
}

impl DecorationEvaluator for DecorationRuntimeEvaluator {
    fn evaluate_window(
        &self,
        window: &WaylandWindowSnapshot,
    ) -> Result<DecorationEvaluationResult, DecorationEvaluationError> {
        match self {
            Self::Static(evaluator) => evaluator.evaluate_window(window),
            Self::Node(evaluator) => evaluator.evaluate_window(window),
        }
    }

    fn scheduler_tick(
        &self,
        now_ms: u64,
    ) -> Result<DecorationSchedulerTick, DecorationEvaluationError> {
        match self {
            Self::Static(_) => Ok(DecorationSchedulerTick::default()),
            Self::Node(evaluator) => evaluator.scheduler_tick(now_ms),
        }
    }

    fn evaluate_cached_window(
        &self,
        window_id: &str,
    ) -> Result<DecorationEvaluationResult, DecorationEvaluationError> {
        match self {
            Self::Static(_) => Err(DecorationEvaluationError::RuntimeProtocol(
                "cached window evaluation unsupported for static evaluator".into(),
            )),
            Self::Node(evaluator) => evaluator.evaluate_cached_window(window_id),
        }
    }

    fn window_closed(&self, window_id: &str) -> Result<(), DecorationEvaluationError> {
        match self {
            Self::Static(_) => Ok(()),
            Self::Node(evaluator) => evaluator.window_closed(window_id),
        }
    }

    fn invoke_handler(
        &self,
        window_id: &str,
        handler_id: &str,
        now_ms: u64,
    ) -> Result<super::DecorationHandlerInvocation, DecorationEvaluationError> {
        match self {
            Self::Static(_) => Ok(super::DecorationHandlerInvocation::default()),
            Self::Node(evaluator) => evaluator.invoke_handler(window_id, handler_id, now_ms),
        }
    }

    fn start_close(
        &self,
        window_id: &str,
        now_ms: u64,
    ) -> Result<super::DecorationHandlerInvocation, DecorationEvaluationError> {
        match self {
            Self::Static(_) => Ok(super::DecorationHandlerInvocation::default()),
            Self::Node(evaluator) => evaluator.start_close(window_id, now_ms),
        }
    }
}

impl ShojiWM {
    pub fn promote_window_to_closing_snapshot(
        &mut self,
        window_id: &str,
        decoration: &WindowDecorationState,
        now_ms: u64,
    ) -> Result<bool, DecorationEvaluationError> {
        if self.closing_window_snapshots.contains_key(window_id) {
            return Ok(true);
        }

        let live_snapshot = self
            .complete_window_snapshots
            .remove(window_id)
            .or_else(|| self.live_window_snapshots.remove(window_id));
        let Some(live_snapshot) = live_snapshot else {
            return Ok(false);
        };

        let invocation = self.decoration_evaluator.start_close(window_id, now_ms)?;
        if !invocation.invoked {
            self.live_window_snapshots
                .insert(window_id.to_string(), live_snapshot);
            return Ok(false);
        }

        self.closing_window_snapshots.insert(
            window_id.to_string(),
            crate::backend::snapshot::ClosingWindowSnapshot {
                window_id: window_id.to_string(),
                live: live_snapshot,
                decoration: decoration.clone(),
                transform: invocation.transform.unwrap_or(decoration.visual_transform),
            },
        );
        self.runtime_dirty_window_ids
            .extend(invocation.dirty_window_ids.into_iter());
        self.runtime_scheduler_enabled = invocation.next_poll_in_ms.is_some();
        self.apply_runtime_window_actions(invocation.actions);
        self.schedule_redraw();

        Ok(true)
    }

    pub fn suggested_window_location(
        &self,
        snapshot: &WaylandWindowSnapshot,
    ) -> Result<(i32, i32), DecorationEvaluationError> {
        if let Some((left_extent, top_extent)) = self.suggested_window_offset {
            let location = if let Some(output) = self.space.outputs().next() {
                if let Some(output_geo) = self.space.output_geometry(output) {
                    (
                        output_geo.loc.x + left_extent,
                        output_geo.loc.y + top_extent,
                    )
                } else {
                    (left_extent, top_extent)
                }
            } else {
                (left_extent, top_extent)
            };

            debug!(
                window_id = snapshot.id,
                title = snapshot.title,
                app_id = snapshot.app_id,
                suggested_x = location.0,
                suggested_y = location.1,
                "computed suggested client location from cached offsets"
            );

            return Ok(location);
        }

        let evaluation = StaticDecorationEvaluator.evaluate_window(snapshot)?;
        let tree = DecorationTree::new(evaluation.node);
        let layout = tree
            .layout_for_client(LogicalRect::new(0, 0, 0, 0))
            .map_err(super::DecorationEvaluationError::Layout)?;

        let root = layout.root.rect;
        let slot = layout
            .window_slot_rect()
            .ok_or(super::DecorationEvaluationError::Layout(
                super::DecorationLayoutError::MissingComputedWindowSlot,
            ))?;

        let left_extent = (slot.x - root.x).max(0);
        let top_extent = (slot.y - root.y).max(0);

        let location = if let Some(output) = self.space.outputs().next() {
            if let Some(output_geo) = self.space.output_geometry(output) {
                (
                    output_geo.loc.x + left_extent,
                    output_geo.loc.y + top_extent,
                )
            } else {
                (left_extent, top_extent)
            }
        } else {
            (left_extent, top_extent)
        };

        debug!(
            window_id = snapshot.id,
            title = snapshot.title,
            app_id = snapshot.app_id,
            root_rect = %format_rect(root),
            slot_rect = %format_rect(slot),
            suggested_x = location.0,
            suggested_y = location.1,
            "computed suggested client location for new window"
        );
        Ok(location)
    }

    pub fn refresh_window_decorations(&mut self) -> Result<(), DecorationEvaluationError> {
        let refresh_started_at = Instant::now();
        let force_runtime_reevaluate = self.runtime_poll_dirty;
        let force_async_asset_refresh = self.async_asset_dirty;
        let windows: Vec<Window> = self.space.elements().cloned().collect();
        let window_count = windows.len();
        let mut rebuilt = 0usize;
        let mut relayout = 0usize;
        let now_ms = Duration::from(self.clock.now()).as_millis() as u64;
        let removed_windows = self
            .window_decorations
            .iter()
            .filter(|(window, _)| !windows.contains(window))
            .map(|(_, decoration)| {
                (
                    decoration.snapshot.id.clone(),
                    decoration.layout.root.rect,
                    decoration.visual_transform,
                    decoration.clone(),
                )
            })
            .collect::<Vec<_>>();
        for (window_id, root_rect, _previous_transform, decoration) in &removed_windows {
            if self.closing_window_snapshots.contains_key(window_id) {
                continue;
            }

            if !self.promote_window_to_closing_snapshot(window_id, decoration, now_ms)? {
                self.decoration_evaluator.window_closed(window_id)?;
                self.runtime_dirty_window_ids.remove(window_id);
                self.snapshot_dirty_window_ids.remove(window_id);
                self.pending_decoration_damage.push(*root_rect);
            }
        }
        self.window_decorations.retain(|window, _| windows.contains(window));

        for window in windows {
            let client_rect = match self.window_client_rect(&window) {
                Some(rect) => rect,
                None => continue,
            };
            let snapshot = self.snapshot_window(&window);

            let needs_tree = force_runtime_reevaluate
                || self.runtime_dirty_window_ids.contains(&snapshot.id)
                || self
                .window_decorations
                .get(&window)
                .map(|cached| cached.snapshot != snapshot)
                .unwrap_or(true);

            if needs_tree {
                let started_at = Instant::now();
                let previous_root = self
                    .window_decorations
                    .get(&window)
                    .map(|cached| transformed_root_rect(cached.layout.root.rect, cached.visual_transform));
                let evaluation = match self.decoration_evaluator.evaluate_window(&snapshot) {
                    Ok(evaluation) => evaluation,
                    Err(error) => {
                        warn!(
                            window_id = snapshot.id,
                            title = snapshot.title,
                            app_id = snapshot.app_id,
                            ?error,
                            "decoration runtime evaluation failed, falling back to static decoration"
                        );
                        StaticDecorationEvaluator.evaluate_window(&snapshot)?
                    }
                };
                let tree = DecorationTree::new(evaluation.node);
                let layout = tree
                    .layout_for_client(client_rect)
                    .map_err(super::DecorationEvaluationError::Layout)?;
                push_damage_pair(
                    &mut self.pending_decoration_damage,
                    previous_root,
                    transformed_root_rect(layout.root.rect, evaluation.transform),
                );
                let content_clip = content_clip_for_layout(&tree, &layout);
                let buffers = build_cached_buffers(&layout);
                let text_buffers = build_text_buffers(&layout, &mut self.text_rasterizer);
                let icon_buffers = build_icon_buffers(&layout, &snapshot, &mut self.icon_rasterizer);
                self.suggested_window_offset = suggested_window_offset(&layout);
                rebuilt += 1;
                debug!(
                    window_id = snapshot.id,
                    title = snapshot.title,
                    text_buffer_count = text_buffers.len(),
                    elapsed_ms = started_at.elapsed().as_secs_f64() * 1000.0,
                    "rebuilt window decoration tree"
                );
                log_decoration_refresh(
                    "rebuild",
                    &snapshot,
                    client_rect,
                    &layout,
                    &buffers,
                );
                let rounded_cache = self
                    .window_decorations
                    .remove(&window)
                    .map(|cached| cached.rounded_cache)
                    .unwrap_or_default();
                self.window_decorations.insert(
                    window,
                    WindowDecorationState {
                        snapshot,
                        tree,
                        layout,
                        client_rect,
                        visual_transform: evaluation.transform,
                        content_clip,
                        buffers,
                        text_buffers,
                        icon_buffers,
                        rounded_cache,
                    },
                );
                self.schedule_redraw();
                self.runtime_scheduler_enabled = evaluation.next_poll_in_ms.is_some();
            } else if let Some(cached) = self.window_decorations.get_mut(&window) {
                if cached.client_rect != client_rect {
                    let started_at = Instant::now();
                    let previous_root =
                        transformed_root_rect(cached.layout.root.rect, cached.visual_transform);
                    let evaluation = match self.decoration_evaluator.evaluate_window(&snapshot) {
                        Ok(evaluation) => evaluation,
                        Err(error) => {
                            warn!(
                                window_id = snapshot.id,
                                title = snapshot.title,
                                app_id = snapshot.app_id,
                                ?error,
                                "decoration runtime evaluation failed during relayout, falling back to static decoration"
                            );
                            StaticDecorationEvaluator.evaluate_window(&snapshot)?
                        }
                    };
                    cached.tree = DecorationTree::new(evaluation.node);
                    cached.layout = cached
                        .tree
                        .layout_for_client(client_rect)
                        .map_err(super::DecorationEvaluationError::Layout)?;
                    push_damage_pair(
                        &mut self.pending_decoration_damage,
                        Some(previous_root),
                        transformed_root_rect(cached.layout.root.rect, evaluation.transform),
                    );
                    cached.client_rect = client_rect;
                    cached.snapshot = snapshot;
                    cached.visual_transform = evaluation.transform;
                    cached.content_clip = content_clip_for_layout(&cached.tree, &cached.layout);
                    cached.buffers = build_cached_buffers(&cached.layout);
                    cached.text_buffers =
                        build_text_buffers(&cached.layout, &mut self.text_rasterizer);
                    cached.icon_buffers =
                        build_icon_buffers(&cached.layout, &cached.snapshot, &mut self.icon_rasterizer);
                    self.suggested_window_offset = suggested_window_offset(&cached.layout);
                    relayout += 1;
                    debug!(
                        window_id = cached.snapshot.id,
                        title = cached.snapshot.title,
                        text_buffer_count = cached.text_buffers.len(),
                        elapsed_ms = started_at.elapsed().as_secs_f64() * 1000.0,
                        "recomputed window decoration layout"
                    );
                    log_decoration_refresh(
                        "relayout",
                        &cached.snapshot,
                        client_rect,
                        &cached.layout,
                        &cached.buffers,
                    );
                    self.schedule_redraw();
                    self.runtime_scheduler_enabled = evaluation.next_poll_in_ms.is_some();
                } else if force_runtime_reevaluate
                    || self.runtime_dirty_window_ids.contains(&snapshot.id)
                {
                    let previous_root =
                        transformed_root_rect(cached.layout.root.rect, cached.visual_transform);
                    let evaluation = match self.decoration_evaluator.evaluate_window(&snapshot) {
                        Ok(evaluation) => evaluation,
                        Err(error) => {
                            warn!(
                                window_id = snapshot.id,
                                title = snapshot.title,
                                app_id = snapshot.app_id,
                                ?error,
                                "decoration runtime evaluation failed during transform update, falling back to static decoration"
                            );
                            StaticDecorationEvaluator.evaluate_window(&snapshot)?
                        }
                    };

                    if cached.visual_transform != evaluation.transform {
                        push_damage_pair(
                            &mut self.pending_decoration_damage,
                            Some(previous_root),
                            transformed_root_rect(cached.layout.root.rect, evaluation.transform),
                        );
                        cached.visual_transform = evaluation.transform;
                        cached.snapshot = snapshot;
                        self.schedule_redraw();
                    }
                    self.runtime_scheduler_enabled = evaluation.next_poll_in_ms.is_some();
                } else if force_async_asset_refresh {
                    cached.text_buffers =
                        build_text_buffers(&cached.layout, &mut self.text_rasterizer);
                    cached.icon_buffers =
                        build_icon_buffers(&cached.layout, &cached.snapshot, &mut self.icon_rasterizer);
                }
            }
        }

        let closing_dirty_ids = self
            .runtime_dirty_window_ids
            .iter()
            .filter(|window_id| self.closing_window_snapshots.contains_key(*window_id))
            .cloned()
            .collect::<Vec<_>>();
        for window_id in closing_dirty_ids {
            if let Some(closing) = self.closing_window_snapshots.get_mut(&window_id) {
                let previous_root =
                    transformed_root_rect(closing.decoration.layout.root.rect, closing.transform);
                let evaluation = self.decoration_evaluator.evaluate_cached_window(&window_id)?;
                let tree = DecorationTree::new(evaluation.node);
                let layout = tree
                    .layout_for_client(closing.decoration.client_rect)
                    .map_err(super::DecorationEvaluationError::Layout)?;
                let content_clip = content_clip_for_layout(&tree, &layout);
                let buffers = build_cached_buffers(&layout);
                let text_buffers = build_text_buffers(&layout, &mut self.text_rasterizer);
                let icon_buffers =
                    build_icon_buffers(&layout, &closing.decoration.snapshot, &mut self.icon_rasterizer);
                closing.decoration.tree = tree;
                closing.decoration.layout = layout;
                closing.decoration.content_clip = content_clip;
                closing.decoration.buffers = buffers;
                closing.decoration.text_buffers = text_buffers;
                closing.decoration.icon_buffers = icon_buffers;
                self.suggested_window_offset = suggested_window_offset(&closing.decoration.layout);
                closing.decoration.visual_transform = evaluation.transform;
                closing.transform = evaluation.transform;
                push_damage_pair(
                    &mut self.pending_decoration_damage,
                    Some(previous_root),
                    transformed_root_rect(closing.decoration.layout.root.rect, closing.transform),
                );
                self.runtime_scheduler_enabled = evaluation.next_poll_in_ms.is_some();
                self.schedule_redraw();
            }
        }

        if force_async_asset_refresh {
            for closing in self.closing_window_snapshots.values_mut() {
                closing.decoration.text_buffers =
                    build_text_buffers(&closing.decoration.layout, &mut self.text_rasterizer);
                closing.decoration.icon_buffers = build_icon_buffers(
                    &closing.decoration.layout,
                    &closing.decoration.snapshot,
                    &mut self.icon_rasterizer,
                );
            }
        }

        trace!(
            window_count,
            rebuilt,
            relayout,
            elapsed_ms = refresh_started_at.elapsed().as_secs_f64() * 1000.0,
            "refresh_window_decorations finished"
        );
        self.runtime_poll_dirty = false;
        self.async_asset_dirty = false;
        self.runtime_dirty_window_ids.clear();

        Ok(())
    }

    pub fn decoration_under(
        &self,
        point: Point<f64, Logical>,
    ) -> Option<(Window, DecorationHitTestResult)> {
        self.space.elements().rev().find_map(|window| {
            let decoration = self.window_decorations.get(window)?;
            let logical_point = LogicalPoint::new(point.x.floor() as i32, point.y.floor() as i32);
            let transformed_root =
                transformed_root_rect(decoration.layout.root.rect, decoration.visual_transform);
            transformed_root.contains(logical_point).then(|| {
                let local_point = inverse_transform_point(
                    point,
                    decoration.layout.root.rect,
                    decoration.visual_transform,
                );
                (window.clone(), decoration.hit_test(local_point))
            })
        })
    }

    fn window_client_rect(&self, window: &Window) -> Option<LogicalRect> {
        let loc = self.space.element_location(window)?;
        let geometry = window.geometry();
        if geometry.size.w <= 0 || geometry.size.h <= 0 {
            return None;
        }
        Some(LogicalRect::new(
            loc.x + geometry.loc.x,
            loc.y + geometry.loc.y,
            geometry.size.w,
            geometry.size.h,
        ))
    }
}

fn content_clip_for_layout(
    tree: &DecorationTree,
    layout: &ComputedDecorationTree,
) -> Option<ContentClip> {
    let border = tree.root.style.border?;
    if !matches!(tree.root.kind, super::DecorationNodeKind::WindowBorder) {
        return None;
    }

    let inner_rect = layout.root.rect.inset(super::Edges {
        top: border.width.max(0),
        right: border.width.max(0),
        bottom: border.width.max(0),
        left: border.width.max(0),
    });
    Some(ContentClip {
        rect: Rectangle::new(
            Point::from((inner_rect.x, inner_rect.y)),
            (inner_rect.width, inner_rect.height).into(),
        ),
        radius: (tree.root.style.border_radius.unwrap_or(0) - border.width.max(0)).max(0),
    })
}

impl DecorationTree {
    /// Compute a layout where the `WindowSlot` matches the provided client rect.
    pub fn layout_for_client(
        &self,
        client_rect: LogicalRect,
    ) -> Result<ComputedDecorationTree, super::DecorationLayoutError> {
        let initial = self.layout(LogicalRect::new(0, 0, client_rect.width, client_rect.height))?;
        let slot = initial
            .window_slot_rect()
            .ok_or(super::DecorationLayoutError::MissingComputedWindowSlot)?;

        let extra_width = initial.root.rect.width - slot.width;
        let extra_height = initial.root.rect.height - slot.height;

        let desired = self.layout(LogicalRect::new(
            0,
            0,
            client_rect.width + extra_width,
            client_rect.height + extra_height,
        ))?;

        let desired_slot = desired
            .window_slot_rect()
            .ok_or(super::DecorationLayoutError::MissingComputedWindowSlot)?;

        let translated = desired.translated(
            client_rect.x - desired_slot.x,
            client_rect.y - desired_slot.y,
        );

        debug!(
            client_rect = %format_rect(client_rect),
            initial_root = %format_rect(initial.root.rect),
            initial_slot = %format_rect(slot),
            extra_width,
            extra_height,
            desired_root = %format_rect(desired.root.rect),
            desired_slot = %format_rect(desired_slot),
            translated_root = %format_rect(translated.root.rect),
            translated_slot = %format_rect(
                translated
                    .window_slot_rect()
                    .ok_or(super::DecorationLayoutError::MissingComputedWindowSlot)?
            ),
            "computed decoration layout for client rect"
        );

        Ok(translated)
    }
}

impl ComputedDecorationTree {
    pub fn translated(&self, dx: i32, dy: i32) -> Self {
        Self {
            root: self.root.translated(dx, dy),
        }
    }
}

impl super::ComputedDecorationNode {
    fn translated(&self, dx: i32, dy: i32) -> Self {
        Self {
            kind: self.kind.clone(),
            style: self.style.clone(),
            rect: LogicalRect::new(
                self.rect.x + dx,
                self.rect.y + dy,
                self.rect.width,
                self.rect.height,
            ),
            children: self
                .children
                .iter()
                .map(|child| child.translated(dx, dy))
                .collect(),
        }
    }
}

fn build_cached_buffers(layout: &ComputedDecorationTree) -> Vec<CachedDecorationBuffer> {
    let mut buffers = Vec::new();
    collect_cached_buffers(&layout.root, "root".to_string(), None, &mut buffers);
    buffers
}

fn suggested_window_offset(layout: &ComputedDecorationTree) -> Option<(i32, i32)> {
    let root = layout.root.rect;
    let slot = layout.window_slot_rect()?;
    Some(((slot.x - root.x).max(0), (slot.y - root.y).max(0)))
}

fn build_text_buffers(
    layout: &ComputedDecorationTree,
    rasterizer: &mut crate::backend::text::TextRasterizer,
) -> Vec<CachedDecorationLabel> {
    let mut buffers = Vec::new();
    collect_text_buffers(&layout.root, rasterizer, &mut buffers);
    buffers
}

fn build_icon_buffers(
    layout: &ComputedDecorationTree,
    snapshot: &WaylandWindowSnapshot,
    rasterizer: &mut crate::backend::icon::IconRasterizer,
) -> Vec<CachedDecorationIcon> {
    let mut buffers = Vec::new();
    collect_icon_buffers(&layout.root, snapshot, rasterizer, &mut buffers);
    buffers
}

fn collect_cached_buffers(
    node: &super::ComputedDecorationNode,
    path: String,
    inherited_clip: Option<(LogicalRect, i32)>,
    buffers: &mut Vec<CachedDecorationBuffer>,
) {
    if node.style.visible == Some(false) {
        return;
    }

    let node_radius = node.style.border_radius.unwrap_or(0).max(0);
    let child_clip = if let Some(border) = node.style.border {
        if matches!(node.kind, super::DecorationNodeKind::WindowBorder) {
            let inner_rect = node.rect.inset(super::Edges {
                top: border.width.max(0),
                right: border.width.max(0),
                bottom: border.width.max(0),
                left: border.width.max(0),
            });
            Some((inner_rect, (node_radius - border.width.max(0)).max(0)))
        } else {
            inherited_clip
        }
    } else {
        inherited_clip
    };

    match &node.kind {
        super::DecorationNodeKind::Label(_)
        | super::DecorationNodeKind::AppIcon
        | super::DecorationNodeKind::WindowSlot => {}
        _ => {
            if let Some(border) = node.style.border {
                let color = border.color.with_opacity(node.style.opacity);
                if color.a > 0 && border.width > 0 {
                    buffers.push(CachedDecorationBuffer {
                        stable_key: format!("{path}:border"),
                        rect: node.rect,
                        color,
                        buffer: SolidColorBuffer::new(
                            (node.rect.width.max(1), node.rect.height.max(1)),
                            [
                                color.r as f32 / 255.0,
                                color.g as f32 / 255.0,
                                color.b as f32 / 255.0,
                                color.a as f32 / 255.0,
                            ],
                        ),
                        radius: node_radius,
                        border_width: border.width.max(0),
                        clip_rect: inherited_clip.map(|(rect, _)| rect),
                        clip_radius: inherited_clip.map(|(_, radius)| radius).unwrap_or(0),
                        source_kind: node_kind_name(&node.kind),
                    });
                }
            }

            for (index, child) in node.children.iter().rev().enumerate() {
                collect_cached_buffers(child, format!("{path}/child-{index}"), child_clip, buffers);
            }

            if let Some(background) = node.style.background.map(|color| color.with_opacity(node.style.opacity)) {
                if background.a > 0 {
                    if matches!(node.kind, super::DecorationNodeKind::WindowBorder) {
                        if let Some((inner_rect, _)) = child_clip {
                            push_fill_rects_around_hole(
                                buffers,
                                &path,
                                node.rect,
                                inner_rect,
                                background,
                                node_radius,
                            );
                        } else {
                            push_cached_fill(
                                buffers,
                                format!("{path}:fill"),
                                node.rect,
                                background,
                                node_radius,
                                0,
                                None,
                                0,
                            );
                        }
                    } else {
                        push_cached_fill(
                            buffers,
                            format!("{path}:fill"),
                            node.rect,
                            background,
                            node_radius,
                            0,
                            inherited_clip.map(|(rect, _)| rect),
                            inherited_clip.map(|(_, radius)| radius).unwrap_or(0),
                        );
                    }
                }
            }
        }
    }
}

fn collect_text_buffers(
    node: &super::ComputedDecorationNode,
    rasterizer: &mut crate::backend::text::TextRasterizer,
    buffers: &mut Vec<CachedDecorationLabel>,
) {
    if node.style.visible == Some(false) {
        return;
    }

    for child in node.children.iter().rev() {
        collect_text_buffers(child, rasterizer, buffers);
    }

    let super::DecorationNodeKind::Label(label) = &node.kind else {
        return;
    };
    let color = node.style.color.unwrap_or(super::Color::WHITE);
    if color.a == 0 {
        return;
    }

    let spec = LabelSpec {
        rect: node.rect,
        text: label.text.clone(),
        color: color.with_opacity(node.style.opacity),
        font_size: node.style.font_size.unwrap_or(13),
        font_weight: node.style.font_weight.clone(),
        font_family: node.style.font_family.clone(),
        text_align: node.style.text_align.clone(),
        line_height: node.style.line_height,
    };

    if let Some(buffer) = rasterizer.render_label(&spec) {
        buffers.push(buffer);
    }
}

fn collect_icon_buffers(
    node: &super::ComputedDecorationNode,
    snapshot: &WaylandWindowSnapshot,
    rasterizer: &mut crate::backend::icon::IconRasterizer,
    buffers: &mut Vec<CachedDecorationIcon>,
) {
    if node.style.visible == Some(false) {
        return;
    }

    for child in node.children.iter().rev() {
        collect_icon_buffers(child, snapshot, rasterizer, buffers);
    }

    let super::DecorationNodeKind::AppIcon = &node.kind else {
        return;
    };

    let spec = IconSpec {
        rect: node.rect,
        icon: snapshot.icon.clone(),
        app_id: snapshot.app_id.clone(),
    };

    if let Some(buffer) = rasterizer.render_icon(&spec) {
        buffers.push(buffer);
    }
}

fn push_fill_rects_around_hole(
    buffers: &mut Vec<CachedDecorationBuffer>,
    path: &str,
    rect: LogicalRect,
    hole: LogicalRect,
    color: super::Color,
    radius: i32,
) {
    let top_height = (hole.y - rect.y).max(0);
    let bottom_y = hole.y + hole.height;
    let bottom_height = (rect.y + rect.height - bottom_y).max(0);
    let left_width = (hole.x - rect.x).max(0);
    let right_x = hole.x + hole.width;
    let right_width = (rect.x + rect.width - right_x).max(0);

    let candidates = [
        ("fill-top", LogicalRect::new(rect.x, rect.y, rect.width, top_height)),
        ("fill-bottom", LogicalRect::new(rect.x, bottom_y, rect.width, bottom_height)),
        ("fill-left", LogicalRect::new(rect.x, hole.y, left_width, hole.height)),
        ("fill-right", LogicalRect::new(right_x, hole.y, right_width, hole.height)),
    ];

    for (suffix, candidate) in candidates {
        push_cached_fill(buffers, format!("{path}:{suffix}"), candidate, color, radius, 0, None, 0);
    }
}

fn push_cached_fill(
    buffers: &mut Vec<CachedDecorationBuffer>,
    stable_key: String,
    rect: LogicalRect,
    color: super::Color,
    radius: i32,
    border_width: i32,
    clip_rect: Option<LogicalRect>,
    clip_radius: i32,
) {
    if rect.width <= 0 || rect.height <= 0 || color.a == 0 {
        return;
    }

    buffers.push(CachedDecorationBuffer {
        stable_key,
        rect,
        color,
        buffer: SolidColorBuffer::new(
            (rect.width.max(1), rect.height.max(1)),
            [
                color.r as f32 / 255.0,
                color.g as f32 / 255.0,
                color.b as f32 / 255.0,
                color.a as f32 / 255.0,
            ],
        ),
        radius,
        border_width,
        clip_rect,
        clip_radius,
        source_kind: "fill",
    });
}

fn node_kind_name(kind: &super::DecorationNodeKind) -> &'static str {
    match kind {
        super::DecorationNodeKind::Box(_) => "box",
        super::DecorationNodeKind::Label(_) => "label",
        super::DecorationNodeKind::Button(_) => "button",
        super::DecorationNodeKind::AppIcon => "app-icon",
        super::DecorationNodeKind::WindowBorder => "window-border",
        super::DecorationNodeKind::WindowSlot => "window-slot",
    }
}

fn log_decoration_refresh(
    reason: &str,
    snapshot: &WaylandWindowSnapshot,
    client_rect: LogicalRect,
    layout: &ComputedDecorationTree,
    buffers: &[CachedDecorationBuffer],
) {
    let slot_rect = layout.window_slot_rect();
    let root_rect = layout.root.rect;

    debug!(
        reason,
        window_id = snapshot.id,
        title = snapshot.title,
        app_id = snapshot.app_id,
        focused = snapshot.is_focused,
        client_rect = %format_rect(client_rect),
        root_rect = %format_rect(root_rect),
        slot_rect = slot_rect
            .map(format_rect)
            .unwrap_or_else(|| "<missing>".to_string()),
        root_to_client_left = client_rect.x - root_rect.x,
        root_to_client_top = client_rect.y - root_rect.y,
        client_to_root_right = (root_rect.x + root_rect.width) - (client_rect.x + client_rect.width),
        client_to_root_bottom = (root_rect.y + root_rect.height) - (client_rect.y + client_rect.height),
        buffer_count = buffers.len(),
        "updated window decoration layout"
    );

    for (index, buffer) in buffers.iter().enumerate() {
        trace!(
            reason,
            window_id = snapshot.id,
            index,
            rect = %format_rect(buffer.rect),
            color = %format_color(buffer.color),
            radius = buffer.radius,
            border_width = buffer.border_width,
            clip_rect = buffer
                .clip_rect
                .map(format_rect)
                .unwrap_or_else(|| "<none>".to_string()),
            source_kind = buffer.source_kind,
            "cached decoration buffer"
        );
    }
}

fn format_rect(rect: LogicalRect) -> String {
    format!("x={}, y={}, w={}, h={}", rect.x, rect.y, rect.width, rect.height)
}

fn format_color(color: super::Color) -> String {
    format!(
        "rgba({}, {}, {}, {})",
        color.r, color.g, color.b, color.a
    )
}

fn push_damage_pair(
    damage: &mut Vec<LogicalRect>,
    old_rect: Option<LogicalRect>,
    new_rect: LogicalRect,
) {
    if let Some(old_rect) = old_rect {
        if old_rect != new_rect {
            damage.push(old_rect);
        }
    }
    damage.push(new_rect);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ssd::{
        BorderStyle, BoxNode, Color, DecorationNode, DecorationNodeKind, DecorationStyle,
        LayoutDirection,
    };

    #[test]
    fn layout_for_client_aligns_window_slot_with_client_rect() {
        let tree = DecorationTree::new(
            DecorationNode::new(DecorationNodeKind::WindowBorder)
                .with_style(DecorationStyle {
                    border: Some(BorderStyle {
                        width: 1,
                        color: Color::WHITE,
                    }),
                    ..Default::default()
                })
                .with_children(vec![
                    DecorationNode::new(DecorationNodeKind::Box(BoxNode {
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
                        DecorationNode::new(DecorationNodeKind::WindowSlot),
                    ]),
                ]),
        );

        let layout = tree
            .layout_for_client(LogicalRect::new(50, 100, 800, 600))
            .expect("layout should succeed");

        assert_eq!(
            layout.window_slot_rect(),
            Some(LogicalRect::new(50, 100, 800, 600))
        );
    }
}
