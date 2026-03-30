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
    shader_effect::CachedShaderEffect,
    text::{CachedDecorationLabel, LabelSpec},
};
use crate::backend::visual::RectSnapMode;
use crate::backend::visual::{inverse_transform_point, transformed_root_rect};
use crate::backend::rounded::RoundedElementState;

use super::{
    ComputedDecorationTree, DecorationEvaluationError, DecorationEvaluationResult,
    DecorationEvaluator, DecorationHandlerInvocation, DecorationHitTestResult,
    DecorationSchedulerTick, DecorationTree, LayerEffectEvaluationResult, LogicalPoint,
    LogicalRect, StaticDecorationEvaluator, WaylandLayerSnapshot, WaylandWindowSnapshot,
    WindowTransform,
    reapply_tree_preserving_layout,
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
    pub shader_buffers: Vec<CachedShaderEffect>,
    pub text_buffers: Vec<CachedDecorationLabel>,
    pub icon_buffers: Vec<CachedDecorationIcon>,
    pub rounded_cache: std::collections::HashMap<String, RoundedElementState>,
    pub shader_cache: std::collections::HashMap<String, crate::backend::shader_effect::ShaderEffectElementState>,
    pub backdrop_cache: std::collections::HashMap<String, crate::backend::shader_effect::CachedBackdropTexture>,
}

#[derive(Debug, Clone, Copy)]
pub struct ContentClip {
    pub rect: Rectangle<i32, Logical>,
    pub radius: i32,
    pub snap_mode: RectSnapMode,
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
    pub owner_node_id: Option<String>,
    pub stable_key: String,
    pub order: usize,
    pub rect: LogicalRect,
    pub color: super::Color,
    pub buffer: SolidColorBuffer,
    pub radius: i32,
    pub border_width: i32,
    pub hole_rect: Option<LogicalRect>,
    pub hole_radius: i32,
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
        now_ms: u64,
    ) -> Result<DecorationEvaluationResult, DecorationEvaluationError> {
        match self {
            Self::Static(evaluator) => evaluator.evaluate_window(window, now_ms),
            Self::Node(evaluator) => evaluator.evaluate_window(window, now_ms),
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
        now_ms: u64,
    ) -> Result<DecorationEvaluationResult, DecorationEvaluationError> {
        match self {
            Self::Static(_) => Err(DecorationEvaluationError::RuntimeProtocol(
                "cached window evaluation unsupported for static evaluator".into(),
            )),
            Self::Node(evaluator) => evaluator.evaluate_cached_window(window_id, now_ms),
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

    fn evaluate_layer_effects(
        &self,
        output_name: &str,
        layers: &[WaylandLayerSnapshot],
        now_ms: u64,
    ) -> Result<LayerEffectEvaluationResult, DecorationEvaluationError> {
        match self {
            Self::Static(_) => Ok(LayerEffectEvaluationResult::default()),
            Self::Node(evaluator) => evaluator.evaluate_layer_effects(output_name, layers, now_ms),
        }
    }
}

impl DecorationRuntimeEvaluator {
    pub fn sync_display_state(
        &self,
        display_state: std::collections::BTreeMap<String, super::WaylandOutputSnapshot>,
    ) {
        if let Self::Node(evaluator) = self {
            evaluator.set_display_state(display_state);
        }
    }
}

impl ShojiWM {
    fn decoration_raster_scale_for_window(&self, window: &Window) -> i32 {
        self.space
            .outputs_for_element(window)
            .into_iter()
            .map(|output| output.current_scale().fractional_scale().ceil() as i32)
            .max()
            .unwrap_or(1)
            .max(1)
    }

    fn decoration_raster_scale_for_rect(&self, rect: LogicalRect) -> i32 {
        let logical = smithay::utils::Rectangle::new(
            smithay::utils::Point::from((rect.x, rect.y)),
            (rect.width, rect.height).into(),
        );
        self.space
            .outputs()
            .filter_map(|output| {
                let geometry = self.space.output_geometry(output)?;
                logical
                    .intersection(geometry)
                    .map(|_| output.current_scale().fractional_scale().ceil() as i32)
            })
            .max()
            .unwrap_or(1)
            .max(1)
    }

    pub fn apply_runtime_handler_invocation(
        &mut self,
        window: &Window,
        invocation: &DecorationHandlerInvocation,
    ) {
        let raster_scale = self.decoration_raster_scale_for_window(window);
        let Some(decoration) = self.window_decorations.get_mut(window) else {
            return;
        };

        let previous_root =
            transformed_root_rect(decoration.layout.root.rect, decoration.visual_transform);

        if let Some(node) = invocation.node.clone() {
            decoration.tree = crate::ssd::DecorationTree::new(node);
            if let Ok(layout) = decoration.tree.layout_for_client(decoration.client_rect) {
                decoration.layout = layout;
                decoration.content_clip =
                    content_clip_for_layout(&decoration.tree, &decoration.layout);
                let order_map = build_render_order_map(&decoration.layout);
                decoration.buffers = build_cached_buffers(&decoration.layout, &order_map);
                decoration.shader_buffers = build_shader_buffers(&decoration.layout, &order_map);
                decoration.text_buffers = build_text_buffers(
                    &decoration.layout,
                    &order_map,
                    raster_scale,
                    &mut self.text_rasterizer,
                );
                decoration.icon_buffers = build_icon_buffers(
                    &decoration.layout,
                    &order_map,
                    raster_scale,
                    &decoration.snapshot,
                    &mut self.icon_rasterizer,
                );
                self.suggested_window_offset = suggested_window_offset(&decoration.layout);
            }
        }

        if let Some(transform) = invocation.transform {
            decoration.visual_transform = transform;
        }

        let next_root =
            transformed_root_rect(decoration.layout.root.rect, decoration.visual_transform);
        push_damage_pair(
            &mut self.pending_decoration_damage,
            Some(previous_root),
            next_root,
        );
        self.schedule_redraw();
    }

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

        self.sync_runtime_display_state();
        let invocation = self.decoration_evaluator.start_close(window_id, now_ms)?;
        self.consume_runtime_display_config(invocation.display_config.clone());
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
        let pointer_location = self
            .seat
            .get_pointer()
            .map(|pointer| pointer.current_location().to_i32_round());
        let preferred_output_geometry = pointer_location
            .and_then(|pointer_location| {
                self.space
                    .outputs()
                    .filter_map(|output| self.space.output_geometry(output))
                    .find(|geometry| geometry.contains(pointer_location))
            })
            .or_else(|| {
                self.space
                    .outputs()
                    .filter_map(|output| self.space.output_geometry(output))
                    .min_by_key(|geometry| (geometry.loc.x, geometry.loc.y))
            });

        if let Some((left_extent, top_extent)) = self.suggested_window_offset {
            let location = if let Some(output_geo) = preferred_output_geometry {
                (
                    output_geo.loc.x + left_extent,
                    output_geo.loc.y + top_extent,
                )
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

        let now_ms = Duration::from(self.clock.now()).as_millis() as u64;
        let evaluation = StaticDecorationEvaluator.evaluate_window(snapshot, now_ms)?;
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

        let location = if let Some(output_geo) = preferred_output_geometry {
            (
                output_geo.loc.x + left_extent,
                output_geo.loc.y + top_extent,
            )
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

    fn primary_output_name_for_window(&self, window: &Window) -> Option<String> {
        let center = if let Some(decoration) = self.window_decorations.get(window) {
            let root =
                transformed_root_rect(decoration.layout.root.rect, decoration.visual_transform);
            Point::from((root.x + root.width / 2, root.y + root.height / 2))
        } else if let Some(client_rect) = self.window_client_rect(window) {
            Point::from((
                client_rect.x + client_rect.width / 2,
                client_rect.y + client_rect.height / 2,
            ))
        } else {
            return self
                .space
                .outputs_for_element(window)
                .first()
                .map(|output| output.name());
        };

        self.space
            .outputs()
            .find(|output| {
                self.space
                    .output_geometry(output)
                    .is_some_and(|geometry| geometry.contains(center))
            })
            .map(|output| output.name())
            .or_else(|| {
                self.space
                    .outputs_for_element(window)
                    .first()
                    .map(|output| output.name())
            })
    }

    pub fn refresh_window_decorations(&mut self) -> Result<(), DecorationEvaluationError> {
        self.refresh_window_decorations_for_output(None)
    }

    pub fn refresh_layer_effects_for_output(
        &mut self,
        output_name: &str,
    ) -> Result<(), DecorationEvaluationError> {
        let snapshots = self.snapshot_layers();
        let output_layer_ids = snapshots
            .iter()
            .filter(|snapshot| snapshot.output_name == output_name)
            .map(|snapshot| snapshot.id.clone())
            .collect::<std::collections::HashSet<_>>();
        let now_ms = Duration::from(self.clock.now()).as_millis() as u64;
        self.sync_runtime_display_state();
        let evaluation = self
            .decoration_evaluator
            .evaluate_layer_effects(output_name, &snapshots, now_ms)?;
        self.consume_runtime_display_config(evaluation.display_config.clone());

        self.runtime_scheduler_enabled = evaluation.next_poll_in_ms.is_some();
        if evaluation.next_poll_in_ms == Some(0) {
            self.runtime_animation_outputs
                .insert(output_name.to_string());
        } else {
            self.runtime_animation_outputs.remove(output_name);
        }
        for layer_id in output_layer_ids {
            self.configured_layer_effects.remove(&layer_id);
        }
        for assignment in evaluation.effects {
            if let Some(effect) = assignment.effect {
                self.configured_layer_effects.insert(assignment.layer_id, effect);
            }
        }

        Ok(())
    }

    pub fn refresh_window_decorations_for_output(
        &mut self,
        target_output_name: Option<&str>,
    ) -> Result<(), DecorationEvaluationError> {
        let refresh_started_at = Instant::now();
        let force_runtime_reevaluate = self.runtime_poll_dirty;
        let force_output_animation_reevaluate = target_output_name
            .is_some_and(|output_name| self.runtime_animation_outputs.contains(output_name));
        let force_async_asset_refresh = self.async_asset_dirty;
        let mut pending_display_config_updates = Vec::new();
        self.sync_runtime_display_state();
        let windows: Vec<Window> = self.space.elements().cloned().collect();
        let window_count = windows.len();
        let mut rebuilt = 0usize;
        let mut relayout = 0usize;
        let mut animation_active_for_target = false;
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
                self.windows_ready_for_decoration.remove(window_id);
                self.runtime_dirty_window_ids.remove(window_id);
                self.snapshot_dirty_window_ids.remove(window_id);
                self.pending_decoration_damage.push(*root_rect);
            }
        }
        self.window_decorations.retain(|window, _| windows.contains(window));
        self.window_primary_output_names
            .retain(|window, _| windows.contains(window));

        for window in windows {
            let primary_output_name = self.primary_output_name_for_window(&window);
            if let Some(target_output_name) = target_output_name {
                if primary_output_name.as_deref() != Some(target_output_name) {
                    continue;
                }
            }
            if let Some(primary_output_name) = primary_output_name {
                self.window_primary_output_names
                    .insert(window.clone(), primary_output_name);
            }
            let client_rect = match self.window_client_rect(&window) {
                Some(rect) => rect,
                None => continue,
            };
            let snapshot = self.snapshot_window(&window);
            let window_raster_scale = self.decoration_raster_scale_for_window(&window);
            let had_cached_decoration = self.window_decorations.contains_key(&window);
            let runtime_state_changed = self
                .window_decorations
                .get(&window)
                .map(|cached| window_snapshot_requires_runtime_refresh(&cached.snapshot, &snapshot))
                .unwrap_or(false);
            let snapshot_changed = self
                .window_decorations
                .get(&window)
                .map(|cached| window_snapshot_requires_rebuild(&cached.snapshot, &snapshot))
                .unwrap_or(true);

            let runtime_dirty = force_runtime_reevaluate
                || force_output_animation_reevaluate
                || runtime_state_changed
                || self.runtime_dirty_window_ids.contains(&snapshot.id);
            if !had_cached_decoration || snapshot_changed {
                let started_at = Instant::now();
                let previous_root = self
                    .window_decorations
                    .get(&window)
                    .map(|cached| transformed_root_rect(cached.layout.root.rect, cached.visual_transform));
                let now_ms = Duration::from(self.clock.now()).as_millis() as u64;
                let evaluation = match self.decoration_evaluator.evaluate_window(&snapshot, now_ms) {
                    Ok(evaluation) => evaluation,
                    Err(error) => {
                        warn!(
                            window_id = snapshot.id,
                            title = snapshot.title,
                            app_id = snapshot.app_id,
                            ?error,
                            "decoration runtime evaluation failed, falling back to static decoration"
                        );
                        StaticDecorationEvaluator.evaluate_window(&snapshot, now_ms)?
                    }
                };
                pending_display_config_updates.push(evaluation.display_config.clone());
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
                let order_map = build_render_order_map(&layout);
                let buffers = build_cached_buffers(&layout, &order_map);
                let mut shader_buffers = build_shader_buffers(&layout, &order_map);
                let text_buffers = build_text_buffers(
                    &layout,
                    &order_map,
                    window_raster_scale,
                    &mut self.text_rasterizer,
                );
                let icon_buffers = build_icon_buffers(
                    &layout,
                    &order_map,
                    window_raster_scale,
                    &snapshot,
                    &mut self.icon_rasterizer,
                );
                if let Some(previous) = self.window_decorations.get(&window) {
                    freeze_manual_shader_buffers(&previous.shader_buffers, &mut shader_buffers);
                }
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
                let caches = self
                    .window_decorations
                    .remove(&window)
                    .map(|cached| (cached.rounded_cache, cached.shader_cache, cached.backdrop_cache))
                    .unwrap_or_default();
                let (rounded_cache, shader_cache, backdrop_cache) = caches;
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
                        shader_buffers,
                        text_buffers,
                        icon_buffers,
                        rounded_cache,
                        shader_cache,
                        backdrop_cache,
                    },
                );
                self.schedule_redraw();
                self.runtime_scheduler_enabled = evaluation.next_poll_in_ms.is_some();
                animation_active_for_target |= evaluation.next_poll_in_ms == Some(0);
            } else if let Some(cached) = self.window_decorations.get_mut(&window) {
                if cached.client_rect != client_rect {
                    let started_at = Instant::now();
                    let previous_root =
                        transformed_root_rect(cached.layout.root.rect, cached.visual_transform);
                    let now_ms = Duration::from(self.clock.now()).as_millis() as u64;
                    let evaluation = match self.decoration_evaluator.evaluate_window(&snapshot, now_ms) {
                        Ok(evaluation) => evaluation,
                        Err(error) => {
                            warn!(
                                window_id = snapshot.id,
                                title = snapshot.title,
                                app_id = snapshot.app_id,
                                ?error,
                                "decoration runtime evaluation failed during relayout, falling back to static decoration"
                            );
                            StaticDecorationEvaluator.evaluate_window(&snapshot, now_ms)?
                        }
                    };
                    pending_display_config_updates.push(evaluation.display_config.clone());
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
                    let order_map = build_render_order_map(&cached.layout);
                    cached.buffers = build_cached_buffers(&cached.layout, &order_map);
                    cached.shader_buffers = build_shader_buffers(&cached.layout, &order_map);
                    cached.text_buffers = build_text_buffers(
                        &cached.layout,
                        &order_map,
                        window_raster_scale,
                        &mut self.text_rasterizer,
                    );
                    cached.icon_buffers = build_icon_buffers(
                        &cached.layout,
                        &order_map,
                        window_raster_scale,
                        &cached.snapshot,
                        &mut self.icon_rasterizer,
                    );
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
                    animation_active_for_target |= evaluation.next_poll_in_ms == Some(0);
                } else if runtime_dirty {
                    let started_at = Instant::now();
                    let previous_root =
                        transformed_root_rect(cached.layout.root.rect, cached.visual_transform);
                    let previous_transform = cached.visual_transform;
                    let previous_layout = cached.layout.clone();
                    let previous_buffers = cached.buffers.clone();
                    let previous_shader_buffers = cached.shader_buffers.clone();
                    let previous_text_buffers = cached.text_buffers.clone();
                    let previous_icon_buffers = cached.icon_buffers.clone();
                    let now_ms = Duration::from(self.clock.now()).as_millis() as u64;
                    let evaluation = if runtime_state_changed {
                        match self.decoration_evaluator.evaluate_window(&snapshot, now_ms) {
                            Ok(evaluation) => evaluation,
                            Err(error) => {
                                warn!(
                                    window_id = snapshot.id,
                                    title = snapshot.title,
                                    app_id = snapshot.app_id,
                                    ?error,
                                    "decoration runtime evaluation failed during runtime state update, falling back to static decoration"
                                );
                                StaticDecorationEvaluator.evaluate_window(&snapshot, now_ms)?
                            }
                        }
                    } else {
                        match self.decoration_evaluator.evaluate_cached_window(&snapshot.id, now_ms) {
                            Ok(evaluation) => evaluation,
                            Err(error) => {
                                warn!(
                                    window_id = snapshot.id,
                                    title = snapshot.title,
                                    app_id = snapshot.app_id,
                                    ?error,
                                    "cached decoration runtime evaluation failed during transform update, falling back to full evaluation"
                                );
                                match self.decoration_evaluator.evaluate_window(&snapshot, now_ms) {
                                    Ok(evaluation) => evaluation,
                                    Err(error) => {
                                        warn!(
                                            window_id = snapshot.id,
                                            title = snapshot.title,
                                            app_id = snapshot.app_id,
                                            ?error,
                                            "decoration runtime evaluation failed during transform update, falling back to static decoration"
                                        );
                                        StaticDecorationEvaluator.evaluate_window(&snapshot, now_ms)?
                                    }
                                }
                            }
                        }
                    };
                    pending_display_config_updates.push(evaluation.display_config.clone());
                    let next_tree = DecorationTree::new(evaluation.node);
                    let next_transform = evaluation.transform;
                    let dirty_node_ids = evaluation.dirty_node_ids;
                    let tree_changed = next_tree != cached.tree;
                    cached.snapshot = snapshot;

                    if !tree_changed {
                        cached.visual_transform = next_transform;
                    } else {
                        let layout_equivalent = cached.tree.root.layout_equivalent(&next_tree.root);
                        cached.tree = next_tree;
                        if layout_equivalent {
                            reapply_tree_preserving_layout(&mut cached.layout.root, &cached.tree.root, None);
                            cached.layout.root.rect = cached.layout.root.bounds_rect();
                            cached.content_clip = content_clip_for_layout(&cached.tree, &cached.layout);
                            let order_map = build_render_order_map(&cached.layout);
                            if dirty_node_ids.is_empty() {
                                cached.buffers = build_cached_buffers(&cached.layout, &order_map);
                                cached.shader_buffers = build_shader_buffers(&cached.layout, &order_map);
                                freeze_manual_shader_buffers(&previous_shader_buffers, &mut cached.shader_buffers);
                                cached.text_buffers = build_text_buffers(
                                    &cached.layout,
                                    &order_map,
                                    window_raster_scale,
                                    &mut self.text_rasterizer,
                                );
                                cached.icon_buffers = build_icon_buffers(
                                    &cached.layout,
                                    &order_map,
                                    window_raster_scale,
                                    &cached.snapshot,
                                    &mut self.icon_rasterizer,
                                );
                            } else {
                                let (rebuilt_buffers, rebuilt_shader_buffers) =
                                    rebuild_partial_buffers(&cached.layout, &order_map, &dirty_node_ids);
                                let mut merged_shader_buffers = merge_shader_buffers(
                                    &previous_shader_buffers,
                                    rebuilt_shader_buffers,
                                    &dirty_node_ids,
                                );
                                freeze_manual_shader_buffers(&previous_shader_buffers, &mut merged_shader_buffers);
                                cached.buffers = merge_cached_buffers(
                                    &previous_buffers,
                                    rebuilt_buffers,
                                    &dirty_node_ids,
                                );
                                cached.shader_buffers = merged_shader_buffers;
                                cached.text_buffers = merge_text_buffers(
                                    &previous_text_buffers,
                                    rebuild_partial_text_buffers(
                                        &cached.layout,
                                        &order_map,
                                        &dirty_node_ids,
                                        window_raster_scale,
                                        &mut self.text_rasterizer,
                                    ),
                                    &dirty_node_ids,
                                );
                                cached.icon_buffers = merge_icon_buffers(
                                    &previous_icon_buffers,
                                    rebuild_partial_icon_buffers(
                                        &cached.layout,
                                        &order_map,
                                        &dirty_node_ids,
                                        window_raster_scale,
                                        &cached.snapshot,
                                        &mut self.icon_rasterizer,
                                    ),
                                    &dirty_node_ids,
                                );
                            }
                        } else {
                            cached.layout = cached
                                .tree
                                .layout_for_client(client_rect)
                                .map_err(super::DecorationEvaluationError::Layout)?;
                            cached.content_clip = content_clip_for_layout(&cached.tree, &cached.layout);
                            let order_map = build_render_order_map(&cached.layout);
                            cached.buffers = build_cached_buffers(&cached.layout, &order_map);
                            cached.shader_buffers = build_shader_buffers(&cached.layout, &order_map);
                            freeze_manual_shader_buffers(&previous_shader_buffers, &mut cached.shader_buffers);
                            cached.text_buffers = build_text_buffers(
                                &cached.layout,
                                &order_map,
                                window_raster_scale,
                                &mut self.text_rasterizer,
                            );
                            cached.icon_buffers = build_icon_buffers(
                                &cached.layout,
                                &order_map,
                                window_raster_scale,
                                &cached.snapshot,
                                &mut self.icon_rasterizer,
                            );
                            self.suggested_window_offset = suggested_window_offset(&cached.layout);
                        }
                        cached.visual_transform = next_transform;
                    }
                    let next_root =
                        transformed_root_rect(cached.layout.root.rect, cached.visual_transform);
                    if previous_transform != cached.visual_transform || previous_root != next_root {
                        push_damage_pair(
                            &mut self.pending_decoration_damage,
                            Some(previous_root),
                            next_root,
                        );
                    } else if !dirty_node_ids.is_empty() {
                        self.pending_decoration_damage.extend(runtime_dirty_node_damage_rects(
                            &previous_layout,
                            previous_transform,
                            &cached.layout,
                            cached.visual_transform,
                            &dirty_node_ids,
                        ));
                    } else {
                        self.pending_decoration_damage.extend(runtime_dirty_damage_rects(
                            &previous_buffers,
                            &cached.buffers,
                            &previous_shader_buffers,
                            &cached.shader_buffers,
                            &previous_text_buffers,
                            &cached.text_buffers,
                            &previous_icon_buffers,
                            &cached.icon_buffers,
                        ));
                    }
                    debug!(
                        window_id = cached.snapshot.id,
                        title = cached.snapshot.title,
                        text_buffer_count = cached.text_buffers.len(),
                        elapsed_ms = started_at.elapsed().as_secs_f64() * 1000.0,
                        "recomputed window decoration tree from runtime dirty state"
                    );
                    if force_async_asset_refresh {
                        let order_map = build_render_order_map(&cached.layout);
                        cached.text_buffers = build_text_buffers(
                            &cached.layout,
                            &order_map,
                            window_raster_scale,
                            &mut self.text_rasterizer,
                        );
                        cached.icon_buffers = build_icon_buffers(
                            &cached.layout,
                            &order_map,
                            window_raster_scale,
                            &cached.snapshot,
                            &mut self.icon_rasterizer,
                        );
                    }
                    log_decoration_refresh(
                        "runtime-dirty",
                        &cached.snapshot,
                        client_rect,
                        &cached.layout,
                        &cached.buffers,
                    );
                    self.schedule_redraw();
                    self.runtime_scheduler_enabled = evaluation.next_poll_in_ms.is_some();
                    animation_active_for_target |= evaluation.next_poll_in_ms == Some(0);
                } else if force_async_asset_refresh {
                    let order_map = build_render_order_map(&cached.layout);
                    cached.text_buffers = build_text_buffers(
                        &cached.layout,
                        &order_map,
                        window_raster_scale,
                        &mut self.text_rasterizer,
                    );
                    cached.icon_buffers = build_icon_buffers(
                        &cached.layout,
                        &order_map,
                        window_raster_scale,
                        &cached.snapshot,
                        &mut self.icon_rasterizer,
                    );
                }
            }
        }

        let closing_dirty_ids = self
            .closing_window_snapshots
            .keys()
            .filter(|window_id| {
                force_output_animation_reevaluate
                    || self.runtime_dirty_window_ids.contains(*window_id)
            })
            .cloned()
            .collect::<Vec<_>>();
        for window_id in closing_dirty_ids {
            let closing_raster_scale = self
                .closing_window_snapshots
                .get(&window_id)
                .map(|closing| self.decoration_raster_scale_for_rect(closing.live.rect))
                .unwrap_or(1);
            if let Some(closing) = self.closing_window_snapshots.get_mut(&window_id) {
                let previous_root =
                    transformed_root_rect(closing.decoration.layout.root.rect, closing.transform);
                let previous_layout = closing.decoration.layout.clone();
                let previous_transform = closing.transform;
                let previous_buffers = closing.decoration.buffers.clone();
                let previous_shader_buffers = closing.decoration.shader_buffers.clone();
                let previous_text_buffers = closing.decoration.text_buffers.clone();
                let previous_icon_buffers = closing.decoration.icon_buffers.clone();
                let now_ms = Duration::from(self.clock.now()).as_millis() as u64;
                let evaluation = self.decoration_evaluator.evaluate_cached_window(&window_id, now_ms)?;
                pending_display_config_updates.push(evaluation.display_config.clone());
                let next_tree = DecorationTree::new(evaluation.node);
                let dirty_node_ids = evaluation.dirty_node_ids;
                let tree_changed = next_tree != closing.decoration.tree;
                if !tree_changed {
                    closing.decoration.visual_transform = evaluation.transform;
                } else {
                    let layout_equivalent =
                        closing.decoration.tree.root.layout_equivalent(&next_tree.root);
                    closing.decoration.tree = next_tree;
                    if layout_equivalent {
                        reapply_tree_preserving_layout(
                            &mut closing.decoration.layout.root,
                            &closing.decoration.tree.root,
                            None,
                        );
                        closing.decoration.layout.root.rect = closing.decoration.layout.root.bounds_rect();
                        closing.decoration.content_clip =
                            content_clip_for_layout(&closing.decoration.tree, &closing.decoration.layout);
                        let order_map = build_render_order_map(&closing.decoration.layout);
                        if dirty_node_ids.is_empty() {
                            closing.decoration.buffers =
                                build_cached_buffers(&closing.decoration.layout, &order_map);
                            closing.decoration.shader_buffers =
                                build_shader_buffers(&closing.decoration.layout, &order_map);
                            closing.decoration.text_buffers = build_text_buffers(
                                &closing.decoration.layout,
                                &order_map,
                                closing_raster_scale,
                                &mut self.text_rasterizer,
                            );
                            closing.decoration.icon_buffers = build_icon_buffers(
                                &closing.decoration.layout,
                                &order_map,
                                closing_raster_scale,
                                &closing.decoration.snapshot,
                                &mut self.icon_rasterizer,
                            );
                        } else {
                            let (rebuilt_buffers, rebuilt_shader_buffers) =
                                rebuild_partial_buffers(&closing.decoration.layout, &order_map, &dirty_node_ids);
                            let mut merged_shader_buffers = merge_shader_buffers(
                                &previous_shader_buffers,
                                rebuilt_shader_buffers,
                                &dirty_node_ids,
                            );
                            freeze_manual_shader_buffers(&previous_shader_buffers, &mut merged_shader_buffers);
                            closing.decoration.buffers = merge_cached_buffers(
                                &previous_buffers,
                                rebuilt_buffers,
                                &dirty_node_ids,
                            );
                            closing.decoration.shader_buffers = merged_shader_buffers;
                            closing.decoration.text_buffers = merge_text_buffers(
                                &previous_text_buffers,
                                rebuild_partial_text_buffers(
                                    &closing.decoration.layout,
                                    &order_map,
                                    &dirty_node_ids,
                                    closing_raster_scale,
                                    &mut self.text_rasterizer,
                                ),
                                &dirty_node_ids,
                            );
                            closing.decoration.icon_buffers = merge_icon_buffers(
                                &previous_icon_buffers,
                                rebuild_partial_icon_buffers(
                                    &closing.decoration.layout,
                                    &order_map,
                                    &dirty_node_ids,
                                    closing_raster_scale,
                                    &closing.decoration.snapshot,
                                    &mut self.icon_rasterizer,
                                ),
                                &dirty_node_ids,
                            );
                        }
                    } else {
                        let layout = closing
                            .decoration
                            .tree
                            .layout_for_client(closing.decoration.client_rect)
                            .map_err(super::DecorationEvaluationError::Layout)?;
                        let content_clip = content_clip_for_layout(&closing.decoration.tree, &layout);
                        let order_map = build_render_order_map(&layout);
                        let buffers = build_cached_buffers(&layout, &order_map);
                        let shader_buffers = build_shader_buffers(&layout, &order_map);
                        let text_buffers =
                            build_text_buffers(&layout, &order_map, closing_raster_scale, &mut self.text_rasterizer);
                        let icon_buffers = build_icon_buffers(
                            &layout,
                            &order_map,
                            closing_raster_scale,
                            &closing.decoration.snapshot,
                            &mut self.icon_rasterizer,
                        );
                        closing.decoration.layout = layout;
                        closing.decoration.content_clip = content_clip;
                        closing.decoration.buffers = buffers;
                        closing.decoration.shader_buffers = shader_buffers;
                        closing.decoration.text_buffers = text_buffers;
                        closing.decoration.icon_buffers = icon_buffers;
                        self.suggested_window_offset = suggested_window_offset(&closing.decoration.layout);
                    }
                    closing.decoration.visual_transform = evaluation.transform;
                }
                closing.decoration.visual_transform = evaluation.transform;
                closing.transform = evaluation.transform;
                let next_root =
                    transformed_root_rect(closing.decoration.layout.root.rect, closing.transform);
                if previous_transform != closing.transform || previous_root != next_root {
                    push_damage_pair(
                        &mut self.pending_decoration_damage,
                        Some(previous_root),
                        next_root,
                    );
                } else if !dirty_node_ids.is_empty() {
                    self.pending_decoration_damage.extend(runtime_dirty_node_damage_rects(
                        &previous_layout,
                        previous_transform,
                        &closing.decoration.layout,
                        closing.transform,
                        &dirty_node_ids,
                    ));
                } else {
                    self.pending_decoration_damage.extend(runtime_dirty_damage_rects(
                        &previous_buffers,
                        &closing.decoration.buffers,
                        &previous_shader_buffers,
                        &closing.decoration.shader_buffers,
                        &previous_text_buffers,
                        &closing.decoration.text_buffers,
                        &previous_icon_buffers,
                        &closing.decoration.icon_buffers,
                    ));
                }
                if force_async_asset_refresh {
                    let order_map = build_render_order_map(&closing.decoration.layout);
                    closing.decoration.text_buffers = build_text_buffers(
                        &closing.decoration.layout,
                        &order_map,
                        closing_raster_scale,
                        &mut self.text_rasterizer,
                    );
                    closing.decoration.icon_buffers = build_icon_buffers(
                        &closing.decoration.layout,
                        &order_map,
                        closing_raster_scale,
                        &closing.decoration.snapshot,
                        &mut self.icon_rasterizer,
                    );
                }
                self.runtime_scheduler_enabled = evaluation.next_poll_in_ms.is_some();
                self.schedule_redraw();
                animation_active_for_target |= evaluation.next_poll_in_ms == Some(0);
            }
        }

        if let Some(output_name) = target_output_name {
            if animation_active_for_target {
                self.runtime_animation_outputs
                    .insert(output_name.to_string());
            } else {
                self.runtime_animation_outputs.remove(output_name);
            }
        }

        if force_async_asset_refresh {
            let closing_scales = self
                .closing_window_snapshots
                .iter()
                .map(|(window_id, closing)| {
                    (
                        window_id.clone(),
                        self.decoration_raster_scale_for_rect(closing.live.rect),
                    )
                })
                .collect::<std::collections::HashMap<_, _>>();
            for (window_id, closing) in self.closing_window_snapshots.iter_mut() {
                let closing_raster_scale = *closing_scales.get(window_id).unwrap_or(&1);
                let order_map = build_render_order_map(&closing.decoration.layout);
                closing.decoration.buffers =
                    build_cached_buffers(&closing.decoration.layout, &order_map);
                closing.decoration.shader_buffers =
                    build_shader_buffers(&closing.decoration.layout, &order_map);
                closing.decoration.text_buffers =
                    build_text_buffers(
                        &closing.decoration.layout,
                        &order_map,
                        closing_raster_scale,
                        &mut self.text_rasterizer,
                    );
                closing.decoration.icon_buffers = build_icon_buffers(
                    &closing.decoration.layout,
                    &order_map,
                    closing_raster_scale,
                    &closing.decoration.snapshot,
                    &mut self.icon_rasterizer,
                );
            }
        }

        for update in pending_display_config_updates {
            self.consume_runtime_display_config(update);
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
    _tree: &DecorationTree,
    layout: &ComputedDecorationTree,
) -> Option<ContentClip> {
    slot_content_clip_for_node(&layout.root, None)
}

fn window_border_inner_hole_rect(
    node: &super::ComputedDecorationNode,
    border_width: i32,
) -> LogicalRect {
    let inner_rect = node.rect.inset(super::Edges {
        top: border_width,
        right: border_width,
        bottom: border_width,
        left: border_width,
    });

    let Some(slot_rect) = node.window_slot_rect() else {
        return inner_rect;
    };

    let left = slot_rect.x.max(inner_rect.x);
    let right = (slot_rect.x + slot_rect.width).min(inner_rect.x + inner_rect.width);
    if right <= left {
        return inner_rect;
    }

    LogicalRect::new(left, inner_rect.y, right - left, inner_rect.height)
}

fn slot_content_clip_for_node(
    node: &super::ComputedDecorationNode,
    nearest_border: Option<(i32, i32)>,
) -> Option<ContentClip> {
    let next_border = if matches!(node.kind, super::DecorationNodeKind::WindowBorder) {
        node.style.border.map(|border| {
            (
                border.width.max(0),
                node.style.border_radius.unwrap_or(0).max(0),
            )
        }).or(nearest_border)
    } else {
        nearest_border
    };

    if matches!(node.kind, super::DecorationNodeKind::WindowSlot) {
        let (border_width, border_radius) = next_border.unwrap_or((0, 0));
        return Some(ContentClip {
            rect: Rectangle::new(
                Point::from((node.rect.x, node.rect.y)),
                (node.rect.width, node.rect.height).into(),
            ),
            radius: (border_radius - border_width).max(0),
            snap_mode: RectSnapMode::OriginAndSize,
        });
    }

    node.children
        .iter()
        .find_map(|child| slot_content_clip_for_node(child, next_border))
}

impl DecorationTree {
    /// Compute a layout where the `WindowSlot` matches the provided client rect.
    pub fn layout_for_client(
        &self,
        client_rect: LogicalRect,
    ) -> Result<ComputedDecorationTree, super::DecorationLayoutError> {
        let initial = self.layout_with_window_slot_size(
            LogicalRect::new(0, 0, client_rect.width, client_rect.height),
            Some((client_rect.width, client_rect.height)),
        )?;
        let slot = initial
            .window_slot_rect()
            .ok_or(super::DecorationLayoutError::MissingComputedWindowSlot)?;
        let initial_bounds = initial.bounds_rect();

        let extra_left = slot.x - initial_bounds.x;
        let extra_top = slot.y - initial_bounds.y;
        let extra_right =
            (initial_bounds.x + initial_bounds.width) - (slot.x + slot.width);
        let extra_bottom =
            (initial_bounds.y + initial_bounds.height) - (slot.y + slot.height);

        let desired = self.layout_with_window_slot_size(
            LogicalRect::new(
                0,
                0,
                client_rect.width + extra_left + extra_right,
                client_rect.height + extra_top + extra_bottom,
            ),
            Some((client_rect.width, client_rect.height)),
        )?;

        let desired_slot = desired
            .window_slot_rect()
            .ok_or(super::DecorationLayoutError::MissingComputedWindowSlot)?;
        let translated = desired.translated(
            client_rect.x - desired_slot.x,
            client_rect.y - desired_slot.y,
        );
        Ok(translated)
    }

    fn layout_with_window_slot_size(
        &self,
        bounds: LogicalRect,
        window_slot_size: Option<(i32, i32)>,
    ) -> Result<ComputedDecorationTree, super::DecorationLayoutError> {
        self.validate()?;

        let mut root = super::layout_node(&self.root, bounds, None, window_slot_size)?;
        root.rect = root.bounds_rect();
        if root.window_slot_rect().is_none() {
            return Err(super::DecorationLayoutError::MissingComputedWindowSlot);
        }

        Ok(ComputedDecorationTree { root })
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
            stable_id: self.stable_id.clone(),
            kind: self.kind.clone(),
            style: self.style.clone(),
            rect: LogicalRect::new(
                self.rect.x + dx,
                self.rect.y + dy,
                self.rect.width,
                self.rect.height,
            ),
            effective_clip: self.effective_clip.map(|clip| super::DecorationClip {
                rect: LogicalRect::new(
                    clip.rect.x + dx,
                    clip.rect.y + dy,
                    clip.rect.width,
                    clip.rect.height,
                ),
                radius: clip.radius,
            }),
            children: self
                .children
                .iter()
                .map(|child| child.translated(dx, dy))
                .collect(),
        }
    }
}

fn build_cached_buffers(
    layout: &ComputedDecorationTree,
    order_map: &std::collections::HashMap<String, usize>,
) -> Vec<CachedDecorationBuffer> {
    let (buffers, _) = build_cached_buffers_and_shaders(layout, order_map, None);
    buffers
}

fn build_shader_buffers(
    layout: &ComputedDecorationTree,
    order_map: &std::collections::HashMap<String, usize>,
) -> Vec<CachedShaderEffect> {
    let (_, buffers) = build_cached_buffers_and_shaders(layout, order_map, None);
    buffers
}

fn build_cached_buffers_and_shaders(
    layout: &ComputedDecorationTree,
    order_map: &std::collections::HashMap<String, usize>,
    dirty_node_ids: Option<&std::collections::HashSet<&str>>,
) -> (Vec<CachedDecorationBuffer>, Vec<CachedShaderEffect>) {
    let mut buffers = Vec::new();
    let mut shader_buffers = Vec::new();
    collect_cached_buffers(
        &layout.root,
        "root".to_string(),
        None,
        order_map,
        dirty_node_ids,
        &mut buffers,
        &mut shader_buffers,
    );
    (buffers, shader_buffers)
}

fn suggested_window_offset(layout: &ComputedDecorationTree) -> Option<(i32, i32)> {
    let root = layout.root.rect;
    let slot = layout.window_slot_rect()?;
    Some(((slot.x - root.x).max(0), (slot.y - root.y).max(0)))
}

fn build_text_buffers(
    layout: &ComputedDecorationTree,
    order_map: &std::collections::HashMap<String, usize>,
    raster_scale: i32,
    rasterizer: &mut crate::backend::text::TextRasterizer,
) -> Vec<CachedDecorationLabel> {
    let mut buffers = Vec::new();
    collect_text_buffers(
        &layout.root,
        "root".into(),
        order_map,
        None,
        raster_scale,
        rasterizer,
        &mut buffers,
    );
    buffers
}

fn build_icon_buffers(
    layout: &ComputedDecorationTree,
    order_map: &std::collections::HashMap<String, usize>,
    raster_scale: i32,
    snapshot: &WaylandWindowSnapshot,
    rasterizer: &mut crate::backend::icon::IconRasterizer,
) -> Vec<CachedDecorationIcon> {
    let mut buffers = Vec::new();
    collect_icon_buffers(
        &layout.root,
        "root".into(),
        order_map,
        None,
        raster_scale,
        snapshot,
        rasterizer,
        &mut buffers,
    );
    buffers
}

fn build_render_order_map(
    layout: &ComputedDecorationTree,
) -> std::collections::HashMap<String, usize> {
    let mut map = std::collections::HashMap::new();
    let mut order = 0usize;
    collect_render_orders(&layout.root, "root".into(), &mut order, &mut map);
    map
}

fn rebuild_partial_buffers(
    layout: &ComputedDecorationTree,
    order_map: &std::collections::HashMap<String, usize>,
    dirty_node_ids: &[String],
) -> (Vec<CachedDecorationBuffer>, Vec<CachedShaderEffect>) {
    let dirty_node_ids = dirty_node_ids
        .iter()
        .map(String::as_str)
        .collect::<std::collections::HashSet<_>>();
    build_cached_buffers_and_shaders(layout, order_map, Some(&dirty_node_ids))
}

fn rebuild_partial_text_buffers(
    layout: &ComputedDecorationTree,
    order_map: &std::collections::HashMap<String, usize>,
    dirty_node_ids: &[String],
    raster_scale: i32,
    rasterizer: &mut crate::backend::text::TextRasterizer,
) -> Vec<CachedDecorationLabel> {
    let dirty_node_ids = dirty_node_ids
        .iter()
        .map(String::as_str)
        .collect::<std::collections::HashSet<_>>();
    let mut buffers = Vec::new();
    collect_text_buffers(
        &layout.root,
        "root".into(),
        order_map,
        Some(&dirty_node_ids),
        raster_scale,
        rasterizer,
        &mut buffers,
    );
    buffers
}

fn rebuild_partial_icon_buffers(
    layout: &ComputedDecorationTree,
    order_map: &std::collections::HashMap<String, usize>,
    dirty_node_ids: &[String],
    raster_scale: i32,
    snapshot: &WaylandWindowSnapshot,
    rasterizer: &mut crate::backend::icon::IconRasterizer,
) -> Vec<CachedDecorationIcon> {
    let dirty_node_ids = dirty_node_ids
        .iter()
        .map(String::as_str)
        .collect::<std::collections::HashSet<_>>();
    let mut buffers = Vec::new();
    collect_icon_buffers(
        &layout.root,
        "root".into(),
        order_map,
        Some(&dirty_node_ids),
        raster_scale,
        snapshot,
        rasterizer,
        &mut buffers,
    );
    buffers
}

fn merge_cached_buffers(
    previous: &[CachedDecorationBuffer],
    rebuilt: Vec<CachedDecorationBuffer>,
    dirty_node_ids: &[String],
) -> Vec<CachedDecorationBuffer> {
    let dirty_node_ids = dirty_node_ids
        .iter()
        .map(String::as_str)
        .collect::<std::collections::HashSet<_>>();
    let mut merged = previous
        .iter()
        .filter(|item| {
            item.owner_node_id
                .as_deref()
                .is_none_or(|node_id| !dirty_node_ids.contains(node_id))
        })
        .cloned()
        .collect::<Vec<_>>();
    merged.extend(rebuilt);
    merged.sort_by_key(|item| item.order);
    merged
}

fn merge_shader_buffers(
    previous: &[CachedShaderEffect],
    rebuilt: Vec<CachedShaderEffect>,
    dirty_node_ids: &[String],
) -> Vec<CachedShaderEffect> {
    let dirty_node_ids = dirty_node_ids
        .iter()
        .map(String::as_str)
        .collect::<std::collections::HashSet<_>>();
    let mut merged = previous
        .iter()
        .filter(|item| {
            item.owner_node_id
                .as_deref()
                .is_none_or(|node_id| !dirty_node_ids.contains(node_id))
        })
        .cloned()
        .collect::<Vec<_>>();
    merged.extend(rebuilt);
    merged.sort_by_key(|item| item.order);
    merged
}

fn merge_text_buffers(
    previous: &[CachedDecorationLabel],
    rebuilt: Vec<CachedDecorationLabel>,
    dirty_node_ids: &[String],
) -> Vec<CachedDecorationLabel> {
    let dirty_node_ids = dirty_node_ids
        .iter()
        .map(String::as_str)
        .collect::<std::collections::HashSet<_>>();
    let mut merged = previous
        .iter()
        .filter(|item| {
            item.owner_node_id
                .as_deref()
                .is_none_or(|node_id| !dirty_node_ids.contains(node_id))
        })
        .cloned()
        .collect::<Vec<_>>();
    merged.extend(rebuilt);
    merged.sort_by_key(|item| item.order);
    merged
}

fn merge_icon_buffers(
    previous: &[CachedDecorationIcon],
    rebuilt: Vec<CachedDecorationIcon>,
    dirty_node_ids: &[String],
) -> Vec<CachedDecorationIcon> {
    let dirty_node_ids = dirty_node_ids
        .iter()
        .map(String::as_str)
        .collect::<std::collections::HashSet<_>>();
    let mut merged = previous
        .iter()
        .filter(|item| {
            item.owner_node_id
                .as_deref()
                .is_none_or(|node_id| !dirty_node_ids.contains(node_id))
        })
        .cloned()
        .collect::<Vec<_>>();
    merged.extend(rebuilt);
    merged.sort_by_key(|item| item.order);
    merged
}

fn collect_render_orders(
    node: &super::ComputedDecorationNode,
    path: String,
    order: &mut usize,
    map: &mut std::collections::HashMap<String, usize>,
) {
    if node.style.visible == Some(false) {
        return;
    }

    match &node.kind {
        super::DecorationNodeKind::Label(_) => {
            map.insert(format!("{path}:label"), *order);
            *order += 1;
            return;
        }
        super::DecorationNodeKind::AppIcon => {
            map.insert(format!("{path}:icon"), *order);
            *order += 1;
            return;
        }
        super::DecorationNodeKind::WindowSlot => return,
        _ => {}
    }

    for (index, child) in node.children.iter().rev().enumerate() {
        collect_render_orders(child, format!("{path}/child-{index}"), order, map);
    }

    if let Some(border) = node.style.border {
        let color = border.color.with_opacity(node.style.opacity);
        if color.a > 0 && border.width > 0 {
            map.insert(format!("{path}:border"), *order);
            *order += 1;
        }
    }

    if let super::DecorationNodeKind::ShaderEffect(_) = &node.kind {
        map.insert(format!("{path}:shader"), *order);
        *order += 1;
    }

    if let Some(background) = node.style.background.map(|color| color.with_opacity(node.style.opacity)) {
        if background.a > 0 {
            if matches!(node.kind, super::DecorationNodeKind::WindowBorder) {
                map.insert(format!("{path}:fill-top"), *order);
                *order += 1;
                map.insert(format!("{path}:fill-bottom"), *order);
                *order += 1;
                map.insert(format!("{path}:fill-left"), *order);
                *order += 1;
                map.insert(format!("{path}:fill-right"), *order);
                *order += 1;
            } else {
                map.insert(format!("{path}:fill"), *order);
                *order += 1;
            }
        }
    }
}

fn collect_cached_buffers(
    node: &super::ComputedDecorationNode,
    path: String,
    ancestor_clip: Option<super::DecorationClip>,
    order_map: &std::collections::HashMap<String, usize>,
    dirty_node_ids: Option<&std::collections::HashSet<&str>>,
    buffers: &mut Vec<CachedDecorationBuffer>,
    shader_buffers: &mut Vec<CachedShaderEffect>,
) {
    if node.style.visible == Some(false) {
        return;
    }

    let include_node = dirty_node_ids.is_none_or(|dirty_node_ids| {
        node.stable_id
            .as_deref()
            .is_some_and(|stable_id| dirty_node_ids.contains(stable_id))
    });

    let node_radius = node.style.border_radius.unwrap_or(0).max(0);
    let current_clip_rect = ancestor_clip.map(|clip| clip.rect);
    let current_clip_radius = ancestor_clip.map(|clip| clip.radius).unwrap_or(0);
    let child_clip = node.effective_clip;
    let window_border_inner_rect = node.style.border.and_then(|border| {
        matches!(node.kind, super::DecorationNodeKind::WindowBorder).then(|| {
            window_border_inner_hole_rect(node, border.width.max(0))
        })
    });

    match &node.kind {
        super::DecorationNodeKind::Label(_)
        | super::DecorationNodeKind::AppIcon
        | super::DecorationNodeKind::WindowSlot => {}
        _ => {
            if include_node {
                if let Some(border) = node.style.border {
                    let color = border.color.with_opacity(node.style.opacity);
                    if color.a > 0 && border.width > 0 {
                        let current_order =
                            *order_map.get(&format!("{path}:border")).unwrap_or(&usize::MAX);
                        buffers.push(CachedDecorationBuffer {
                            owner_node_id: node.stable_id.clone(),
                            stable_key: format!("{path}:border"),
                            order: current_order,
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
                            hole_rect: Some(window_border_inner_hole_rect(
                                node,
                                border.width.max(0),
                            )),
                            hole_radius: (node_radius - border.width.max(0)).max(0),
                            clip_rect: current_clip_rect,
                            clip_radius: current_clip_radius,
                            source_kind: node_kind_name(&node.kind),
                        });
                    }
                }

                if let super::DecorationNodeKind::ShaderEffect(effect) = &node.kind {
                    let current_order =
                        *order_map.get(&format!("{path}:shader")).unwrap_or(&usize::MAX);
                    shader_buffers.push(CachedShaderEffect {
                        owner_node_id: node.stable_id.clone(),
                        stable_key: format!("{path}:shader"),
                        order: current_order,
                        rect: node.rect,
                        shader: effect.shader.clone(),
                        clip_rect: current_clip_rect,
                        clip_radius: current_clip_radius,
                    });
                }

                if let Some(background) = node.style.background.map(|color| color.with_opacity(node.style.opacity)) {
                    if background.a > 0 {
                        if matches!(node.kind, super::DecorationNodeKind::WindowBorder) {
                            if let Some(inner_rect) = window_border_inner_rect {
                                push_cached_fill(
                                    buffers,
                                    *order_map
                                        .get(&format!("{path}:fill-top"))
                                        .unwrap_or(&usize::MAX),
                                    format!("{path}:fill-top"),
                                    node.rect,
                                    background,
                                    node.stable_id.clone(),
                                    node_radius,
                                    0,
                                    Some(inner_rect),
                                    node
                                        .style
                                        .border
                                        .map(|border| {
                                            (node_radius - border.width.max(0)).max(0)
                                        })
                                        .unwrap_or(node_radius),
                                    None,
                                    0,
                                );
                            } else {
                                push_cached_fill(
                                    buffers,
                                    *order_map.get(&format!("{path}:fill")).unwrap_or(&usize::MAX),
                                    format!("{path}:fill"),
                                    node.rect,
                                    background,
                                    node.stable_id.clone(),
                                    node_radius,
                                    0,
                                    None,
                                    0,
                                    None,
                                    0,
                                );
                            }
                        } else {
                            push_cached_fill(
                                buffers,
                                *order_map.get(&format!("{path}:fill")).unwrap_or(&usize::MAX),
                                format!("{path}:fill"),
                                node.rect,
                                background,
                                node.stable_id.clone(),
                                node_radius,
                                0,
                                None,
                                0,
                                current_clip_rect,
                                current_clip_radius,
                            );
                        }
                    }
                }
            }

            for (index, child) in node.children.iter().rev().enumerate() {
                collect_cached_buffers(
                    child,
                    format!("{path}/child-{index}"),
                    child_clip,
                    order_map,
                    dirty_node_ids,
                    buffers,
                    shader_buffers,
                );
            }
        }
    }
}

fn collect_text_buffers(
    node: &super::ComputedDecorationNode,
    path: String,
    order_map: &std::collections::HashMap<String, usize>,
    dirty_node_ids: Option<&std::collections::HashSet<&str>>,
    raster_scale: i32,
    rasterizer: &mut crate::backend::text::TextRasterizer,
    buffers: &mut Vec<CachedDecorationLabel>,
) {
    if node.style.visible == Some(false) {
        return;
    }

    for (index, child) in node.children.iter().rev().enumerate() {
        collect_text_buffers(
            child,
            format!("{path}/child-{index}"),
            order_map,
            dirty_node_ids,
            raster_scale,
            rasterizer,
            buffers,
        );
    }

    if dirty_node_ids.is_some_and(|dirty_node_ids| {
        !node
            .stable_id
            .as_deref()
            .is_some_and(|stable_id| dirty_node_ids.contains(stable_id))
    }) {
        return;
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
        raster_scale,
    };

    if let Some(buffer) = rasterizer.render_label(&spec) {
        let mut buffer = buffer;
        buffer.owner_node_id = node.stable_id.clone();
        buffer.order = *order_map.get(&format!("{path}:label")).unwrap_or(&usize::MAX);
        buffer.clip_rect = node.effective_clip.map(|clip| clip.rect);
        buffer.clip_radius = node.effective_clip.map(|clip| clip.radius).unwrap_or(0);
        buffers.push(buffer);
    }
}

fn collect_icon_buffers(
    node: &super::ComputedDecorationNode,
    path: String,
    order_map: &std::collections::HashMap<String, usize>,
    dirty_node_ids: Option<&std::collections::HashSet<&str>>,
    raster_scale: i32,
    snapshot: &WaylandWindowSnapshot,
    rasterizer: &mut crate::backend::icon::IconRasterizer,
    buffers: &mut Vec<CachedDecorationIcon>,
) {
    if node.style.visible == Some(false) {
        return;
    }

    for (index, child) in node.children.iter().rev().enumerate() {
        collect_icon_buffers(
            child,
            format!("{path}/child-{index}"),
            order_map,
            dirty_node_ids,
            raster_scale,
            snapshot,
            rasterizer,
            buffers,
        );
    }

    if dirty_node_ids.is_some_and(|dirty_node_ids| {
        !node
            .stable_id
            .as_deref()
            .is_some_and(|stable_id| dirty_node_ids.contains(stable_id))
    }) {
        return;
    }

    let super::DecorationNodeKind::AppIcon = &node.kind else {
        return;
    };

    let spec = IconSpec {
        rect: node.rect,
        icon: snapshot.icon.clone(),
        app_id: snapshot.app_id.clone(),
        raster_scale,
    };

    if let Some(buffer) = rasterizer.render_icon(&spec) {
        let mut buffer = buffer;
        buffer.owner_node_id = node.stable_id.clone();
        buffer.order = *order_map.get(&format!("{path}:icon")).unwrap_or(&usize::MAX);
        buffer.clip_rect = node.effective_clip.map(|clip| clip.rect);
        buffer.clip_radius = node.effective_clip.map(|clip| clip.radius).unwrap_or(0);
        buffers.push(buffer);
    }
}

fn push_cached_fill(
    buffers: &mut Vec<CachedDecorationBuffer>,
    order: usize,
    stable_key: String,
    rect: LogicalRect,
    color: super::Color,
    owner_node_id: Option<String>,
    radius: i32,
    border_width: i32,
    hole_rect: Option<LogicalRect>,
    hole_radius: i32,
    clip_rect: Option<LogicalRect>,
    clip_radius: i32,
) {
    if rect.width <= 0 || rect.height <= 0 || color.a == 0 {
        return;
    }

    buffers.push(CachedDecorationBuffer {
        owner_node_id,
        stable_key,
        order,
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
        hole_rect,
        hole_radius,
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
        super::DecorationNodeKind::ShaderEffect(_) => "shader-effect",
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
            hole_rect = buffer
                .hole_rect
                .map(format_rect)
                .unwrap_or_else(|| "<none>".to_string()),
            hole_radius = buffer.hole_radius,
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

fn window_snapshot_requires_rebuild(
    previous: &WaylandWindowSnapshot,
    next: &WaylandWindowSnapshot,
) -> bool {
    previous.id != next.id
        || previous.title != next.title
        || previous.app_id != next.app_id
        || previous.position != next.position
        || previous.is_floating != next.is_floating
        || previous.is_maximized != next.is_maximized
        || previous.is_fullscreen != next.is_fullscreen
        || previous.is_xwayland != next.is_xwayland
        || previous.icon != next.icon
}

fn window_snapshot_requires_runtime_refresh(
    previous: &WaylandWindowSnapshot,
    next: &WaylandWindowSnapshot,
) -> bool {
    previous.is_focused != next.is_focused || previous.interaction != next.interaction
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

fn runtime_dirty_damage_rects(
    previous_buffers: &[CachedDecorationBuffer],
    next_buffers: &[CachedDecorationBuffer],
    previous_shader_buffers: &[CachedShaderEffect],
    next_shader_buffers: &[CachedShaderEffect],
    previous_text_buffers: &[CachedDecorationLabel],
    next_text_buffers: &[CachedDecorationLabel],
    previous_icon_buffers: &[CachedDecorationIcon],
    next_icon_buffers: &[CachedDecorationIcon],
) -> Vec<LogicalRect> {
    let mut damage = Vec::new();

    collect_keyed_rect_damage(
        previous_buffers.iter().map(|item| {
            (
                item.stable_key.clone(),
                (
                    item.rect,
                    format!(
                        "{:?}:{:?}:{}:{}:{:?}:{}:{:?}:{}",
                        item.color,
                        item.source_kind,
                        item.radius,
                        item.border_width,
                        item.hole_rect,
                        item.hole_radius,
                        item.clip_rect,
                        item.clip_radius
                    ),
                ),
            )
        }),
        next_buffers.iter().map(|item| {
            (
                item.stable_key.clone(),
                (
                    item.rect,
                    format!(
                        "{:?}:{:?}:{}:{}:{:?}:{}:{:?}:{}",
                        item.color,
                        item.source_kind,
                        item.radius,
                        item.border_width,
                        item.hole_rect,
                        item.hole_radius,
                        item.clip_rect,
                        item.clip_radius
                    ),
                ),
            )
        }),
        &mut damage,
    );
    collect_keyed_rect_damage(
        previous_shader_buffers
            .iter()
            .map(|item| (item.stable_key.clone(), (item.rect, format!("{:?}", item.shader)))),
        next_shader_buffers
            .iter()
            .map(|item| (item.stable_key.clone(), (item.rect, format!("{:?}", item.shader)))),
        &mut damage,
    );
    collect_keyed_rect_damage(
        previous_text_buffers.iter().map(|item| {
            (
                format!(
                    "text:{}:{}:{}:{}:{}:{}",
                    item.order, item.rect.x, item.rect.y, item.rect.width, item.rect.height, item.text
                ),
                (item.rect, format!("{:?}", item.color)),
            )
        }),
        next_text_buffers.iter().map(|item| {
            (
                format!(
                    "text:{}:{}:{}:{}:{}:{}",
                    item.order, item.rect.x, item.rect.y, item.rect.width, item.rect.height, item.text
                ),
                (item.rect, format!("{:?}", item.color)),
            )
        }),
        &mut damage,
    );
    collect_keyed_rect_damage(
        previous_icon_buffers.iter().map(|item| {
            (
                format!("icon:{}:{}:{}:{}:{}", item.order, item.rect.x, item.rect.y, item.rect.width, item.rect.height),
                (item.rect, String::new()),
            )
        }),
        next_icon_buffers.iter().map(|item| {
            (
                format!("icon:{}:{}:{}:{}:{}", item.order, item.rect.x, item.rect.y, item.rect.width, item.rect.height),
                (item.rect, String::new()),
            )
        }),
        &mut damage,
    );

    damage
}

fn runtime_dirty_node_damage_rects(
    previous_layout: &ComputedDecorationTree,
    previous_transform: WindowTransform,
    next_layout: &ComputedDecorationTree,
    next_transform: WindowTransform,
    dirty_node_ids: &[String],
) -> Vec<LogicalRect> {
    let node_id_set = dirty_node_ids
        .iter()
        .map(String::as_str)
        .collect::<std::collections::HashSet<_>>();
    let mut previous_rects = Vec::new();
    let mut next_rects = Vec::new();
    previous_layout
        .root
        .rects_for_stable_ids(&node_id_set, &mut previous_rects);
    next_layout
        .root
        .rects_for_stable_ids(&node_id_set, &mut next_rects);

    let mut damage = Vec::new();
    for rect in previous_rects {
        damage.push(transformed_root_rect(rect, previous_transform));
    }
    for rect in next_rects {
        damage.push(transformed_root_rect(rect, next_transform));
    }
    damage
}

fn freeze_manual_shader_buffers(
    previous_shader_buffers: &[CachedShaderEffect],
    next_shader_buffers: &mut [CachedShaderEffect],
) {
    let previous_by_key = previous_shader_buffers
        .iter()
        .map(|item| (item.stable_key.as_str(), item))
        .collect::<std::collections::HashMap<_, _>>();

    for next in next_shader_buffers.iter_mut() {
        let Some(previous) = previous_by_key.get(next.stable_key.as_str()) else {
            continue;
        };
        if matches!(
            next.shader.invalidate_policy(),
            crate::ssd::EffectInvalidationPolicy::Manual { dirty_when: false, .. }
        ) {
            let invalidate = next.shader.invalidate.clone();
            next.shader = previous.shader.clone();
            next.shader.invalidate = invalidate;
        }
    }
}

fn collect_keyed_rect_damage<K>(
    previous: impl IntoIterator<Item = (K, (LogicalRect, String))>,
    next: impl IntoIterator<Item = (K, (LogicalRect, String))>,
    damage: &mut Vec<LogicalRect>,
)
where
    K: Eq + std::hash::Hash + Clone,
{
    let previous_map: std::collections::HashMap<K, (LogicalRect, String)> =
        previous.into_iter().collect();
    let next_map: std::collections::HashMap<K, (LogicalRect, String)> = next.into_iter().collect();

    for (key, (old_rect, old_sig)) in &previous_map {
        match next_map.get(key) {
            Some((new_rect, new_sig)) if new_rect == old_rect && new_sig == old_sig => {}
            Some((new_rect, _)) => {
                damage.push(*old_rect);
                damage.push(*new_rect);
            }
            None => damage.push(*old_rect),
        }
    }

    for (key, (new_rect, _)) in &next_map {
        if !previous_map.contains_key(key) {
            damage.push(*new_rect);
        }
    }
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
