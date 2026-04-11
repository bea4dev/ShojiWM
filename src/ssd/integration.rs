use smithay::{
    backend::renderer::element::solid::SolidColorBuffer,
    desktop::Window,
    utils::{Logical, Point, Rectangle},
};
use std::time::{Duration, Instant};
use tracing::{debug, info, trace, warn};

use crate::backend::rounded::RoundedElementState;
use crate::backend::visual::RectSnapMode;
use crate::backend::visual::{inverse_transform_point, transformed_root_rect};
use crate::backend::{
    icon::{CachedDecorationIcon, IconSpec},
    shader_effect::CachedShaderEffect,
    text::{CachedDecorationLabel, LabelSpec},
    visual::PreciseLogicalRect,
};
use crate::state::ShojiWM;

use super::{
    ComputedDecorationTree, DecorationEvaluationError, DecorationEvaluationResult,
    DecorationEvaluator, DecorationHandlerInvocation, DecorationHitTestResult,
    DecorationSchedulerTick, DecorationTree, LayerEffectEvaluationResult, LogicalPoint,
    LogicalRect, StaticDecorationEvaluator, WaylandLayerSnapshot, WaylandWindowSnapshot,
    WindowTransform, reapply_tree_preserving_layout,
};

fn clip_debug_enabled() -> bool {
    std::env::var_os("SHOJI_CLIP_DEBUG").is_some()
}

#[derive(Debug, Clone)]
pub struct WindowDecorationState {
    pub snapshot: WaylandWindowSnapshot,
    pub tree: DecorationTree,
    pub layout: ComputedDecorationTree,
    pub layout_scale: f64,
    pub client_rect: LogicalRect,
    pub visual_transform: WindowTransform,
    pub content_clip: Option<ContentClip>,
    pub buffers: Vec<CachedDecorationBuffer>,
    pub shader_buffers: Vec<CachedShaderEffect>,
    pub text_buffers: Vec<CachedDecorationLabel>,
    pub icon_buffers: Vec<CachedDecorationIcon>,
    pub rounded_cache: std::collections::HashMap<String, RoundedElementState>,
    pub shader_cache:
        std::collections::HashMap<String, crate::backend::shader_effect::ShaderEffectElementState>,
    pub backdrop_cache:
        std::collections::HashMap<String, crate::backend::shader_effect::CachedBackdropTexture>,
}

#[derive(Debug, Clone, Copy)]
pub struct ContentClip {
    // Reserved client slot geometry. This controls where the client is placed.
    pub rect: Rectangle<i32, Logical>,
    pub rect_precise: PreciseLogicalRect,
    // Ancestor clip mask geometry. This controls how the client is clipped.
    pub mask_rect: Rectangle<i32, Logical>,
    pub mask_rect_precise: PreciseLogicalRect,
    pub radius: i32,
    pub radius_precise: f32,
    pub corner_radii: [i32; 4],
    pub corner_radii_precise: [f32; 4],
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
    pub rect_precise: Option<PreciseLogicalRect>,
    pub color: super::Color,
    pub buffer: SolidColorBuffer,
    pub radius: i32,
    pub radius_precise: Option<f32>,
    pub border_width: f32,
    pub hole_rect: Option<LogicalRect>,
    pub hole_rect_precise: Option<PreciseLogicalRect>,
    pub hole_radius: i32,
    pub hole_radius_precise: Option<f32>,
    pub shared_inner_hole: bool,
    pub clip_rect: Option<LogicalRect>,
    pub clip_radius: i32,
    pub clip_rect_precise: Option<PreciseLogicalRect>,
    pub clip_radius_precise: Option<f32>,
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
    fn decoration_layout_scale_for_window(&self, window: &Window) -> f64 {
        self.space
            .outputs_for_element(window)
            .into_iter()
            .map(|output| output.current_scale().fractional_scale())
            .fold(1.0f64, f64::max)
            .max(1.0)
    }

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
                let shared_edges = build_shared_edge_geometry_map(&decoration.layout);
                decoration.content_clip =
                    content_clip_for_layout(&decoration.tree, &decoration.layout, &shared_edges);
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

        // Always use the live (client-area) snapshot for the closing animation.
        // The complete_window_snapshot bakes decorations into the texture, so using it here
        // would cause decorations to appear twice (once in the texture, once from separate
        // decoration elements). Clean it up but don't use it.
        self.complete_window_snapshots.remove(window_id);
        let live_snapshot = self.live_window_snapshots.remove(window_id);
        let Some(live_snapshot) = live_snapshot else {
            return Ok(false);
        };

        self.sync_runtime_display_state();
        let invocation = self.decoration_evaluator.start_close(window_id, now_ms)?;
        self.consume_runtime_display_config(invocation.display_config.clone());
        self.consume_runtime_process_config(invocation.process_config.clone());
        if !invocation.process_actions.is_empty() {
            self.apply_runtime_process_actions(invocation.process_actions.clone());
        }
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
        // Always use space.element_location (via window_client_rect) as the source of truth for
        // the window's current position. decoration.layout.root.rect lags behind because it is
        // only updated inside refresh_window_decorations_for_output — which itself calls this
        // function to decide whether to process the window at all. Using the stale decoration
        // rect here creates a chicken-and-egg deadlock: a window that moves from eDP-1 to DP-4
        // keeps reporting "eDP-1" as its primary output, so the DP-4 refresh pass skips it, and
        // its decoration coordinates never get updated.
        let center = if let Some(client_rect) = self.window_client_rect(window) {
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
        let evaluation =
            self.decoration_evaluator
                .evaluate_layer_effects(output_name, &snapshots, now_ms)?;
        self.consume_runtime_display_config(evaluation.display_config.clone());
        self.consume_runtime_process_config(evaluation.process_config.clone());
        if !evaluation.process_actions.is_empty() {
            self.apply_runtime_process_actions(evaluation.process_actions.clone());
        }

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
                self.configured_layer_effects
                    .insert(assignment.layer_id, effect);
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
        let mut pending_process_config_updates = Vec::new();
        let mut pending_process_actions = Vec::new();
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
        self.window_decorations
            .retain(|window, _| windows.contains(window));
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
            let layout_scale = self.decoration_layout_scale_for_window(&window);
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
                let previous_root = self.window_decorations.get(&window).map(|cached| {
                    transformed_root_rect(cached.layout.root.rect, cached.visual_transform)
                });
                let now_ms = Duration::from(self.clock.now()).as_millis() as u64;
                let evaluation = match self.decoration_evaluator.evaluate_window(&snapshot, now_ms)
                {
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
                pending_process_config_updates.push(evaluation.process_config.clone());
                pending_process_actions.extend(evaluation.process_actions.clone());
                let tree = DecorationTree::new(evaluation.node);
                let layout = tree
                    .layout_for_client_with_scale(client_rect, layout_scale)
                    .map_err(super::DecorationEvaluationError::Layout)?;
                push_damage_pair(
                    &mut self.pending_decoration_damage,
                    previous_root,
                    transformed_root_rect(layout.root.rect, evaluation.transform),
                );
                let shared_edges = build_shared_edge_geometry_map(&layout);
                let content_clip = content_clip_for_layout(&tree, &layout, &shared_edges);
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
                log_decoration_refresh("rebuild", &snapshot, client_rect, &layout, &buffers);
                let caches = self
                    .window_decorations
                    .remove(&window)
                    .map(|cached| {
                        (
                            cached.rounded_cache,
                            cached.shader_cache,
                            cached.backdrop_cache,
                        )
                    })
                    .unwrap_or_default();
                let (rounded_cache, shader_cache, backdrop_cache) = caches;
                self.window_decorations.insert(
                    window,
                    WindowDecorationState {
                        snapshot,
                        tree,
                        layout,
                        layout_scale,
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
                    let evaluation = match self
                        .decoration_evaluator
                        .evaluate_window(&snapshot, now_ms)
                    {
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
                    pending_process_config_updates.push(evaluation.process_config.clone());
                    pending_process_actions.extend(evaluation.process_actions.clone());
                    cached.tree = DecorationTree::new(evaluation.node);
                    cached.layout = cached
                        .tree
                        .layout_for_client_with_scale(client_rect, layout_scale)
                        .map_err(super::DecorationEvaluationError::Layout)?;
                    cached.layout_scale = layout_scale;
                    push_damage_pair(
                        &mut self.pending_decoration_damage,
                        Some(previous_root),
                        transformed_root_rect(cached.layout.root.rect, evaluation.transform),
                    );
                    cached.client_rect = client_rect;
                    cached.snapshot = snapshot;
                    cached.visual_transform = evaluation.transform;
                    let shared_edges = build_shared_edge_geometry_map(&cached.layout);
                    cached.content_clip =
                        content_clip_for_layout(&cached.tree, &cached.layout, &shared_edges);
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
                        match self
                            .decoration_evaluator
                            .evaluate_cached_window(&snapshot.id, now_ms)
                        {
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
                                        StaticDecorationEvaluator
                                            .evaluate_window(&snapshot, now_ms)?
                                    }
                                }
                            }
                        }
                    };
                    pending_display_config_updates.push(evaluation.display_config.clone());
                    pending_process_config_updates.push(evaluation.process_config.clone());
                    pending_process_actions.extend(evaluation.process_actions.clone());
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
                            reapply_tree_preserving_layout(
                                &mut cached.layout.root,
                                &cached.tree.root,
                                None,
                                cached.layout_scale,
                            );
                            cached.layout.root.sync_root_bounds(cached.layout_scale);
                            let shared_edges = build_shared_edge_geometry_map(&cached.layout);
                            cached.content_clip = content_clip_for_layout(
                                &cached.tree,
                                &cached.layout,
                                &shared_edges,
                            );
                            let order_map = build_render_order_map(&cached.layout);
                            if dirty_node_ids.is_empty() {
                                cached.buffers = build_cached_buffers(&cached.layout, &order_map);
                                cached.shader_buffers =
                                    build_shader_buffers(&cached.layout, &order_map);
                                freeze_manual_shader_buffers(
                                    &previous_shader_buffers,
                                    &mut cached.shader_buffers,
                                );
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
                                    rebuild_partial_buffers(
                                        &cached.layout,
                                        &order_map,
                                        &dirty_node_ids,
                                    );
                                let mut merged_shader_buffers = merge_shader_buffers(
                                    &previous_shader_buffers,
                                    rebuilt_shader_buffers,
                                    &dirty_node_ids,
                                );
                                freeze_manual_shader_buffers(
                                    &previous_shader_buffers,
                                    &mut merged_shader_buffers,
                                );
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
                                .layout_for_client_with_scale(client_rect, layout_scale)
                                .map_err(super::DecorationEvaluationError::Layout)?;
                            cached.layout_scale = layout_scale;
                            let shared_edges = build_shared_edge_geometry_map(&cached.layout);
                            cached.content_clip = content_clip_for_layout(
                                &cached.tree,
                                &cached.layout,
                                &shared_edges,
                            );
                            let order_map = build_render_order_map(&cached.layout);
                            cached.buffers = build_cached_buffers(&cached.layout, &order_map);
                            cached.shader_buffers =
                                build_shader_buffers(&cached.layout, &order_map);
                            freeze_manual_shader_buffers(
                                &previous_shader_buffers,
                                &mut cached.shader_buffers,
                            );
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
                        self.pending_decoration_damage
                            .extend(runtime_dirty_node_damage_rects(
                                &previous_layout,
                                previous_transform,
                                &cached.layout,
                                cached.visual_transform,
                                &dirty_node_ids,
                            ));
                    } else {
                        self.pending_decoration_damage
                            .extend(runtime_dirty_damage_rects(
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
                let evaluation = self
                    .decoration_evaluator
                    .evaluate_cached_window(&window_id, now_ms)?;
                pending_display_config_updates.push(evaluation.display_config.clone());
                pending_process_config_updates.push(evaluation.process_config.clone());
                pending_process_actions.extend(evaluation.process_actions.clone());
                let next_tree = DecorationTree::new(evaluation.node);
                let dirty_node_ids = evaluation.dirty_node_ids;
                let tree_changed = next_tree != closing.decoration.tree;
                if !tree_changed {
                    closing.decoration.visual_transform = evaluation.transform;
                } else {
                    let layout_equivalent = closing
                        .decoration
                        .tree
                        .root
                        .layout_equivalent(&next_tree.root);
                    closing.decoration.tree = next_tree;
                    if layout_equivalent {
                        reapply_tree_preserving_layout(
                            &mut closing.decoration.layout.root,
                            &closing.decoration.tree.root,
                            None,
                            closing.decoration.layout_scale,
                        );
                        closing
                            .decoration
                            .layout
                            .root
                            .sync_root_bounds(closing.decoration.layout_scale);
                        let shared_edges =
                            build_shared_edge_geometry_map(&closing.decoration.layout);
                        closing.decoration.content_clip = content_clip_for_layout(
                            &closing.decoration.tree,
                            &closing.decoration.layout,
                            &shared_edges,
                        );
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
                            let (rebuilt_buffers, rebuilt_shader_buffers) = rebuild_partial_buffers(
                                &closing.decoration.layout,
                                &order_map,
                                &dirty_node_ids,
                            );
                            let mut merged_shader_buffers = merge_shader_buffers(
                                &previous_shader_buffers,
                                rebuilt_shader_buffers,
                                &dirty_node_ids,
                            );
                            freeze_manual_shader_buffers(
                                &previous_shader_buffers,
                                &mut merged_shader_buffers,
                            );
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
                            .layout_for_client_with_scale(
                                closing.decoration.client_rect,
                                closing.decoration.layout_scale,
                            )
                            .map_err(super::DecorationEvaluationError::Layout)?;
                        let shared_edges = build_shared_edge_geometry_map(&layout);
                        let content_clip = content_clip_for_layout(
                            &closing.decoration.tree,
                            &layout,
                            &shared_edges,
                        );
                        let order_map = build_render_order_map(&layout);
                        let buffers = build_cached_buffers(&layout, &order_map);
                        let shader_buffers = build_shader_buffers(&layout, &order_map);
                        let text_buffers = build_text_buffers(
                            &layout,
                            &order_map,
                            closing_raster_scale,
                            &mut self.text_rasterizer,
                        );
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
                        self.suggested_window_offset =
                            suggested_window_offset(&closing.decoration.layout);
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
                    self.pending_decoration_damage
                        .extend(runtime_dirty_node_damage_rects(
                            &previous_layout,
                            previous_transform,
                            &closing.decoration.layout,
                            closing.transform,
                            &dirty_node_ids,
                        ));
                } else {
                    self.pending_decoration_damage
                        .extend(runtime_dirty_damage_rects(
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
        }

        for update in pending_display_config_updates {
            self.consume_runtime_display_config(update);
        }
        for update in pending_process_config_updates {
            self.consume_runtime_process_config(update);
        }
        if !pending_process_actions.is_empty() {
            self.apply_runtime_process_actions(pending_process_actions);
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
    shared_edges: &std::collections::HashMap<String, SharedEdgeNodeGeometry>,
) -> Option<ContentClip> {
    slot_content_clip_for_node(&layout.root, None, None, shared_edges)
}

fn window_border_inner_clip_resolved(
    node: &super::ComputedDecorationNode,
) -> Option<crate::ssd::ResolvedDecorationClip> {
    if !matches!(node.kind, super::DecorationNodeKind::WindowBorder) {
        return None;
    }
    node.style.border?;
    Some(
        node.resolved_effective_clip
            .unwrap_or(crate::ssd::ResolvedDecorationClip {
                rect: node.resolved_content_rect,
                radius: (node.resolved_border_radius - node.resolved_border_width)
                    .max(crate::ssd::ResolvedLayoutValue::ZERO),
            }),
    )
}

fn window_border_inner_hole_rect(
    node: &super::ComputedDecorationNode,
    border_width: i32,
) -> LogicalRect {
    let fallback_inner_rect = node.rect.inset(super::Edges {
        top: border_width,
        right: border_width,
        bottom: border_width,
        left: border_width,
    });

    let inner_rect = window_border_inner_clip_resolved(node)
        .map(|clip| clip.rect.round_to_logical_rect())
        .unwrap_or_else(|| node.resolved_content_rect.round_to_logical_rect());
    if inner_rect.width <= 0 || inner_rect.height <= 0 {
        fallback_inner_rect
    } else {
        inner_rect
    }
}

fn precise_rect_from_resolved(rect: crate::ssd::ResolvedLogicalRect) -> PreciseLogicalRect {
    PreciseLogicalRect {
        x: rect.x.to_f32(),
        y: rect.y.to_f32(),
        width: rect.width.to_f32(),
        height: rect.height.to_f32(),
    }
}

fn precise_rect_from_logical(rect: LogicalRect) -> PreciseLogicalRect {
    PreciseLogicalRect {
        x: rect.x as f32,
        y: rect.y as f32,
        width: rect.width as f32,
        height: rect.height as f32,
    }
}

fn window_border_inner_clip_logical(
    node: &super::ComputedDecorationNode,
) -> Option<super::DecorationClip> {
    if !matches!(node.kind, super::DecorationNodeKind::WindowBorder) {
        return None;
    }
    node.style.border?;
    let border_width = node.resolved_border_width.round_to_i32().max(0);
    let rect = window_border_inner_hole_rect(node, border_width);
    if rect.width <= 0 || rect.height <= 0 {
        return None;
    }
    Some(super::DecorationClip {
        rect,
        radius: (node.resolved_border_radius - node.resolved_border_width)
            .round_to_i32()
            .max(0),
    })
}

fn normal_border_inner_rect(node: &super::ComputedDecorationNode) -> Option<LogicalRect> {
    node.style.border?;
    let border_width = node.resolved_border_width.round_to_i32().max(0);
    let rect = node.rect.inset(super::Edges::all(border_width));
    (rect.width > 0 && rect.height > 0).then_some(rect)
}

fn normal_border_inner_rect_precise(
    node: &super::ComputedDecorationNode,
) -> Option<PreciseLogicalRect> {
    node.style.border?;
    let border_width = node.resolved_border_width;
    let rect = node.resolved_rect.inset(super::ResolvedLayoutEdges {
        top: border_width,
        right: border_width,
        bottom: border_width,
        left: border_width,
    });
    (rect.width.raw() > 0 && rect.height.raw() > 0).then(|| precise_rect_from_resolved(rect))
}

fn slot_content_clip_for_node(
    node: &super::ComputedDecorationNode,
    nearest_border: Option<(i32, i32)>,
    nearest_rounded_mask: Option<crate::ssd::ResolvedDecorationClip>,
    shared_edges: &std::collections::HashMap<String, SharedEdgeNodeGeometry>,
) -> Option<ContentClip> {
    let next_border = if matches!(node.kind, super::DecorationNodeKind::WindowBorder) {
        node.style
            .border
            .map(|border| {
                (
                    border.width.max(0),
                    node.style.border_radius.unwrap_or(0).max(0),
                )
            })
            .or(nearest_border)
    } else {
        nearest_border
    };
    let next_rounded_mask = window_border_inner_clip_resolved(node)
        .filter(|clip| clip.radius.raw() > 0)
        .or(nearest_rounded_mask);

    if matches!(node.kind, super::DecorationNodeKind::WindowSlot) {
        let (_border_width, _border_radius) = next_border.unwrap_or((0, 0));
        let inherited_clip =
            node.resolved_effective_clip
                .unwrap_or(crate::ssd::ResolvedDecorationClip {
                    rect: node.resolved_rect,
                    radius: (node.resolved_border_radius - node.resolved_border_width)
                        .max(crate::ssd::ResolvedLayoutValue::ZERO),
                });
        let slot_rect = node.resolved_rect;
        let slot_rect_precise = precise_rect_from_resolved(slot_rect);
        let mask = next_rounded_mask.unwrap_or(inherited_clip);
        let mask_rect_precise = precise_rect_from_resolved(mask.rect);
        let corner_radii_precise = if mask.radius.raw() > 0 {
            [mask.radius.to_f32().max(0.0); 4]
        } else {
            [0.0; 4]
        };
        let corner_radii = corner_radii_precise.map(|radius| radius.round().max(0.0) as i32);
        return Some(ContentClip {
            rect: Rectangle::new(
                Point::from((slot_rect.x.round_to_i32(), slot_rect.y.round_to_i32())),
                (
                    slot_rect.width.round_to_i32(),
                    slot_rect.height.round_to_i32(),
                )
                    .into(),
            ),
            rect_precise: slot_rect_precise,
            mask_rect: Rectangle::new(
                Point::from((mask.rect.x.round_to_i32(), mask.rect.y.round_to_i32())),
                (
                    mask.rect.width.round_to_i32(),
                    mask.rect.height.round_to_i32(),
                )
                    .into(),
            ),
            mask_rect_precise,
            // Client surfaces should stay rectangular inside the reserved slot.
            // The surrounding WindowBorder descendants use the rounded mask;
            // the client content itself should not inherit that corner radius.
            radius: 0,
            radius_precise: 0.0,
            corner_radii,
            corner_radii_precise,
            snap_mode: RectSnapMode::SharedEdges,
        });
    }

    node.children.iter().find_map(|child| {
        slot_content_clip_for_node(child, next_border, next_rounded_mask, shared_edges)
    })
}

impl DecorationTree {
    /// Compute a layout where the `WindowSlot` matches the provided client rect.
    pub fn layout_for_client(
        &self,
        client_rect: LogicalRect,
    ) -> Result<ComputedDecorationTree, super::DecorationLayoutError> {
        self.layout_for_client_with_scale(client_rect, 1.0)
    }

    pub fn layout_for_client_with_scale(
        &self,
        client_rect: LogicalRect,
        scale: f64,
    ) -> Result<ComputedDecorationTree, super::DecorationLayoutError> {
        let initial = self.layout_with_window_slot_size(
            LogicalRect::new(0, 0, client_rect.width, client_rect.height),
            Some((client_rect.width, client_rect.height)),
            scale,
        )?;
        let slot = initial
            .window_slot_rect()
            .ok_or(super::DecorationLayoutError::MissingComputedWindowSlot)?;
        let initial_bounds = initial.bounds_rect();

        let extra_left = slot.x - initial_bounds.x;
        let extra_top = slot.y - initial_bounds.y;
        let extra_right = (initial_bounds.x + initial_bounds.width) - (slot.x + slot.width);
        let extra_bottom = (initial_bounds.y + initial_bounds.height) - (slot.y + slot.height);

        let desired = self.layout_with_window_slot_size(
            LogicalRect::new(
                0,
                0,
                client_rect.width + extra_left + extra_right,
                client_rect.height + extra_top + extra_bottom,
            ),
            Some((client_rect.width, client_rect.height)),
            scale,
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
        scale: f64,
    ) -> Result<ComputedDecorationTree, super::DecorationLayoutError> {
        self.validate()?;

        let mut root =
            super::layout_node_with_scale(&self.root, bounds, None, window_slot_size, scale)?;
        root.sync_root_bounds(scale);
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
            resolved_rect: crate::ssd::ResolvedLogicalRect {
                x: self.resolved_rect.x + crate::ssd::ResolvedLayoutValue::from_i32(dx),
                y: self.resolved_rect.y + crate::ssd::ResolvedLayoutValue::from_i32(dy),
                width: self.resolved_rect.width,
                height: self.resolved_rect.height,
            },
            resolved_content_rect: crate::ssd::ResolvedLogicalRect {
                x: self.resolved_content_rect.x + crate::ssd::ResolvedLayoutValue::from_i32(dx),
                y: self.resolved_content_rect.y + crate::ssd::ResolvedLayoutValue::from_i32(dy),
                width: self.resolved_content_rect.width,
                height: self.resolved_content_rect.height,
            },
            resolved_border_width: self.resolved_border_width,
            resolved_border_radius: self.resolved_border_radius,
            effective_clip: self.effective_clip.map(|clip| super::DecorationClip {
                rect: LogicalRect::new(
                    clip.rect.x + dx,
                    clip.rect.y + dy,
                    clip.rect.width,
                    clip.rect.height,
                ),
                radius: clip.radius,
            }),
            resolved_effective_clip: self.resolved_effective_clip.map(|clip| {
                crate::ssd::ResolvedDecorationClip {
                    rect: crate::ssd::ResolvedLogicalRect {
                        x: clip.rect.x + crate::ssd::ResolvedLayoutValue::from_i32(dx),
                        y: clip.rect.y + crate::ssd::ResolvedLayoutValue::from_i32(dy),
                        width: clip.rect.width,
                        height: clip.rect.height,
                    },
                    radius: clip.radius,
                }
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
    let shared_edges = build_shared_edge_geometry_map(layout);
    let (buffers, _) = build_cached_buffers_and_shaders(layout, order_map, None, &shared_edges);
    buffers
}

fn build_shader_buffers(
    layout: &ComputedDecorationTree,
    order_map: &std::collections::HashMap<String, usize>,
) -> Vec<CachedShaderEffect> {
    let shared_edges = build_shared_edge_geometry_map(layout);
    let (_, buffers) = build_cached_buffers_and_shaders(layout, order_map, None, &shared_edges);
    buffers
}

fn build_cached_buffers_and_shaders(
    layout: &ComputedDecorationTree,
    order_map: &std::collections::HashMap<String, usize>,
    dirty_node_ids: Option<&std::collections::HashSet<&str>>,
    shared_edges: &std::collections::HashMap<String, SharedEdgeNodeGeometry>,
) -> (Vec<CachedDecorationBuffer>, Vec<CachedShaderEffect>) {
    let mut buffers = Vec::new();
    let mut shader_buffers = Vec::new();
    collect_cached_buffers(
        &layout.root,
        "root".to_string(),
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        order_map,
        dirty_node_ids,
        shared_edges,
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
    let shared_edges = build_shared_edge_geometry_map(layout);
    let mut buffers = Vec::new();
    collect_text_buffers(
        &layout.root,
        "root".into(),
        order_map,
        None,
        &shared_edges,
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
    let shared_edges = build_shared_edge_geometry_map(layout);
    let mut buffers = Vec::new();
    collect_icon_buffers(
        &layout.root,
        "root".into(),
        order_map,
        None,
        &shared_edges,
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
    let shared_edges = build_shared_edge_geometry_map(layout);
    build_cached_buffers_and_shaders(layout, order_map, Some(&dirty_node_ids), &shared_edges)
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
    let shared_edges = build_shared_edge_geometry_map(layout);
    let mut buffers = Vec::new();
    collect_text_buffers(
        &layout.root,
        "root".into(),
        order_map,
        Some(&dirty_node_ids),
        &shared_edges,
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
    let shared_edges = build_shared_edge_geometry_map(layout);
    let mut buffers = Vec::new();
    collect_icon_buffers(
        &layout.root,
        "root".into(),
        order_map,
        Some(&dirty_node_ids),
        &shared_edges,
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

    if let Some(background) = node
        .style
        .background
        .map(|color| color.with_opacity(node.style.opacity))
    {
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
    ancestor_resolved_clip: Option<crate::ssd::ResolvedDecorationClip>,
    ancestor_clip_rect_precise: Option<PreciseLogicalRect>,
    ancestor_clip_radius_precise: Option<f32>,
    nearest_rounded_clip: Option<super::DecorationClip>,
    nearest_rounded_clip_rect_precise: Option<PreciseLogicalRect>,
    nearest_rounded_clip_radius_precise: Option<f32>,
    order_map: &std::collections::HashMap<String, usize>,
    dirty_node_ids: Option<&std::collections::HashSet<&str>>,
    shared_edges: &std::collections::HashMap<String, SharedEdgeNodeGeometry>,
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
    let shared_geometry = node
        .stable_id
        .as_deref()
        .and_then(|stable_id| shared_edges.get(stable_id))
        .copied();

    let node_radius = node.resolved_border_radius.round_to_i32().max(0);
    let current_clip_rect = ancestor_clip.map(|clip| clip.rect);
    let current_clip_radius = ancestor_clip.map(|clip| clip.radius).unwrap_or(0);
    let current_clip_rect_precise = ancestor_clip_rect_precise
        .or_else(|| ancestor_resolved_clip.map(|clip| precise_rect_from_resolved(clip.rect)));
    let current_clip_radius_precise = ancestor_clip_radius_precise
        .or_else(|| ancestor_resolved_clip.map(|clip| clip.radius.to_f32().max(0.0)));
    let border_fit = node.style.effective_border_fit(&node.kind);
    let fit_children = matches!(border_fit, super::BorderFit::FitChildren);
    let window_border_inner_clip_resolved = if fit_children {
        window_border_inner_clip_resolved(node)
    } else {
        None
    };
    let window_border_inner_clip_precise = if fit_children {
        window_border_inner_clip_resolved.map(|clip| precise_rect_from_resolved(clip.rect))
    } else {
        normal_border_inner_rect_precise(node)
    };
    let window_border_inner_radius_precise = if fit_children {
        window_border_inner_clip_resolved.map(|clip| clip.radius.to_f32().max(0.0))
    } else {
        Some(
            (node.resolved_border_radius - node.resolved_border_width)
                .to_f32()
                .max(0.0),
        )
    };
    let window_border_inner_clip = if fit_children {
        window_border_inner_clip_logical(node)
    } else {
        normal_border_inner_rect(node).map(|rect| super::DecorationClip {
            rect,
            radius: (node.resolved_border_radius - node.resolved_border_width)
                .round_to_i32()
                .max(0),
        })
    };
    let child_clip = window_border_inner_clip.or(node.effective_clip);
    let child_resolved_clip = window_border_inner_clip_resolved.or(node.resolved_effective_clip);
    let child_clip_rect_precise = if fit_children {
        shared_geometry
            .map(|geometry| geometry.content_rect_precise)
            .or(window_border_inner_clip_precise)
            .or_else(|| child_resolved_clip.map(|clip| precise_rect_from_resolved(clip.rect)))
    } else {
        shared_geometry
            .and_then(|geometry| geometry.clip_rect_precise)
            .or_else(|| child_resolved_clip.map(|clip| precise_rect_from_resolved(clip.rect)))
    };
    let child_clip_radius_precise = if fit_children {
        window_border_inner_radius_precise
            .or_else(|| child_resolved_clip.map(|clip| clip.radius.to_f32().max(0.0)))
    } else {
        child_resolved_clip.map(|clip| clip.radius.to_f32().max(0.0))
    };
    let effective_clip_rect = if current_clip_radius_precise.unwrap_or(0.0) > 0.0 {
        current_clip_rect
    } else {
        nearest_rounded_clip.map(|clip| clip.rect)
    };
    let effective_clip_radius = if current_clip_radius_precise.unwrap_or(0.0) > 0.0 {
        current_clip_radius
    } else {
        nearest_rounded_clip.map(|clip| clip.radius).unwrap_or(0)
    };
    let effective_clip_rect_precise = if current_clip_radius_precise.unwrap_or(0.0) > 0.0 {
        current_clip_rect_precise
    } else {
        nearest_rounded_clip_rect_precise
    };
    let effective_clip_radius_precise = if current_clip_radius_precise.unwrap_or(0.0) > 0.0 {
        current_clip_radius_precise
    } else {
        nearest_rounded_clip_radius_precise
    };
    let next_nearest_rounded_clip = if child_clip_radius_precise.unwrap_or(0.0) > 0.0 {
        child_clip
    } else {
        nearest_rounded_clip
    };
    let next_nearest_rounded_clip_rect_precise = if child_clip_radius_precise.unwrap_or(0.0) > 0.0 {
        child_clip_rect_precise
    } else {
        nearest_rounded_clip_rect_precise
    };
    let next_nearest_rounded_clip_radius_precise = if child_clip_radius_precise.unwrap_or(0.0) > 0.0
    {
        child_clip_radius_precise
    } else {
        nearest_rounded_clip_radius_precise
    };
    if clip_debug_enabled() {
        trace!(
            stable_id = node.stable_id.as_deref().unwrap_or("<none>"),
            kind = node_kind_name(&node.kind),
            current_clip_rect = ?current_clip_rect,
            current_clip_rect_precise = ?current_clip_rect_precise,
            current_clip_radius = current_clip_radius,
            current_clip_radius_precise = ?current_clip_radius_precise,
            child_clip_rect = ?child_clip.map(|clip| clip.rect),
            child_clip_rect_precise = ?child_clip_rect_precise,
            child_clip_radius = child_clip.map(|clip| clip.radius),
            child_clip_radius_precise = ?child_clip_radius_precise,
            effective_clip_rect = ?effective_clip_rect,
            effective_clip_rect_precise = ?effective_clip_rect_precise,
            effective_clip_radius = effective_clip_radius,
            effective_clip_radius_precise = ?effective_clip_radius_precise,
            fit_children,
            include_node,
            "clip propagation at cached buffer collection"
        );
    }
    let window_border_inner_rect = window_border_inner_clip.map(|clip| clip.rect).or_else(|| {
        node.style.border.and_then(|_border| {
            matches!(node.kind, super::DecorationNodeKind::WindowBorder).then(|| {
                window_border_inner_hole_rect(
                    node,
                    node.resolved_border_width.round_to_i32().max(0),
                )
            })
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
                        let current_order = *order_map
                            .get(&format!("{path}:border"))
                            .unwrap_or(&usize::MAX);
                        buffers.push(CachedDecorationBuffer {
                            owner_node_id: node.stable_id.clone(),
                            stable_key: format!("{path}:border"),
                            order: current_order,
                            rect: node.rect,
                            rect_precise: Some(
                                shared_geometry
                                    .map(|geometry| geometry.rect_precise)
                                    .unwrap_or_else(|| {
                                        precise_rect_from_resolved(node.resolved_rect)
                                    }),
                            ),
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
                            radius_precise: (!node.children.is_empty())
                                .then(|| node.resolved_border_radius.to_f32().max(0.0)),
                            border_width: node.resolved_border_width.to_f32().max(0.0),
                            hole_rect: window_border_inner_rect,
                            hole_rect_precise: shared_geometry
                                .map(|geometry| geometry.content_rect_precise)
                                .or(window_border_inner_clip_precise)
                                .or_else(|| {
                                    (fit_children && !node.children.is_empty()).then(|| {
                                        precise_rect_from_resolved(
                                            window_border_inner_clip_resolved
                                                .map(|clip| clip.rect)
                                                .unwrap_or(node.resolved_content_rect),
                                        )
                                    })
                                }),
                            hole_radius: window_border_inner_clip
                                .map(|clip| clip.radius.max(0))
                                .unwrap_or_else(|| {
                                    (node.resolved_border_radius - node.resolved_border_width)
                                        .round_to_i32()
                                        .max(0)
                                }),
                            hole_radius_precise: window_border_inner_radius_precise.or_else(|| {
                                (fit_children && !node.children.is_empty()).then(|| {
                                    (node.resolved_border_radius - node.resolved_border_width)
                                        .to_f32()
                                        .max(0.0)
                                })
                            }),
                            shared_inner_hole: !node.children.is_empty() && fit_children,
                            clip_rect: effective_clip_rect,
                            clip_radius: effective_clip_radius,
                            clip_rect_precise: effective_clip_rect_precise,
                            clip_radius_precise: effective_clip_radius_precise,
                            source_kind: node_kind_name(&node.kind),
                        });
                    }
                }

                if let super::DecorationNodeKind::ShaderEffect(effect) = &node.kind {
                    let current_order = *order_map
                        .get(&format!("{path}:shader"))
                        .unwrap_or(&usize::MAX);
                    shader_buffers.push(CachedShaderEffect {
                        owner_node_id: node.stable_id.clone(),
                        stable_key: format!("{path}:shader"),
                        order: current_order,
                        rect: node.rect,
                        rect_precise: Some(
                            shared_geometry
                                .map(|geometry| geometry.rect_precise)
                                .unwrap_or_else(|| precise_rect_from_resolved(node.resolved_rect)),
                        ),
                        shader: effect.shader.clone(),
                        clip_rect: effective_clip_rect,
                        clip_radius: effective_clip_radius,
                        clip_rect_precise: effective_clip_rect_precise,
                        clip_radius_precise: effective_clip_radius_precise,
                    });
                }

                if let Some(background) = node
                    .style
                    .background
                    .map(|color| color.with_opacity(node.style.opacity))
                {
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
                                    Some(precise_rect_from_resolved(node.resolved_rect)),
                                    background,
                                    node.stable_id.clone(),
                                    node_radius,
                                    Some(node.resolved_border_radius.to_f32().max(0.0)),
                                    0.0,
                                    Some(inner_rect),
                                    window_border_inner_clip
                                        .map(|clip| clip.radius.max(0))
                                        .unwrap_or_else(|| {
                                            (node.resolved_border_radius
                                                - node.resolved_border_width)
                                                .round_to_i32()
                                                .max(0)
                                        }),
                                    window_border_inner_clip
                                        .map(|clip| precise_rect_from_logical(clip.rect)),
                                    window_border_inner_clip.map(|clip| clip.radius.max(0) as f32),
                                    effective_clip_rect_precise,
                                    effective_clip_radius_precise,
                                    None,
                                    0,
                                );
                            } else {
                                push_cached_fill(
                                    buffers,
                                    *order_map
                                        .get(&format!("{path}:fill"))
                                        .unwrap_or(&usize::MAX),
                                    format!("{path}:fill"),
                                    node.rect,
                                    Some(precise_rect_from_resolved(node.resolved_rect)),
                                    background,
                                    node.stable_id.clone(),
                                    node_radius,
                                    Some(node.resolved_border_radius.to_f32().max(0.0)),
                                    0.0,
                                    None,
                                    0,
                                    None,
                                    None,
                                    effective_clip_rect_precise,
                                    effective_clip_radius_precise,
                                    None,
                                    0,
                                );
                            }
                        } else {
                            push_cached_fill(
                                buffers,
                                *order_map
                                    .get(&format!("{path}:fill"))
                                    .unwrap_or(&usize::MAX),
                                format!("{path}:fill"),
                                node.rect,
                                Some(precise_rect_from_resolved(node.resolved_rect)),
                                background,
                                node.stable_id.clone(),
                                node_radius,
                                Some(node.resolved_border_radius.to_f32().max(0.0)),
                                0.0,
                                None,
                                0,
                                None,
                                None,
                                effective_clip_rect_precise,
                                effective_clip_radius_precise,
                                effective_clip_rect,
                                effective_clip_radius,
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
                    child_resolved_clip,
                    child_clip_rect_precise,
                    child_clip_radius_precise,
                    next_nearest_rounded_clip,
                    next_nearest_rounded_clip_rect_precise,
                    next_nearest_rounded_clip_radius_precise,
                    order_map,
                    dirty_node_ids,
                    shared_edges,
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
    shared_edges: &std::collections::HashMap<String, SharedEdgeNodeGeometry>,
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
            shared_edges,
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
    let shared_geometry = node
        .stable_id
        .as_deref()
        .and_then(|stable_id| shared_edges.get(stable_id))
        .copied();
    let color = node.style.color.unwrap_or(super::Color::WHITE);
    if color.a == 0 {
        return;
    }

    let spec = LabelSpec {
        rect: node.rect,
        rect_precise: Some(
            shared_geometry
                .map(|geometry| geometry.rect_precise)
                .unwrap_or_else(|| precise_rect_from_resolved(node.resolved_rect)),
        ),
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
        buffer.stable_key = format!("{path}:label");
        buffer.order = *order_map
            .get(&format!("{path}:label"))
            .unwrap_or(&usize::MAX);
        buffer.rect_precise = Some(
            shared_geometry
                .map(|geometry| geometry.rect_precise)
                .unwrap_or_else(|| precise_rect_from_resolved(node.resolved_rect)),
        );
        buffer.clip_rect = node.effective_clip.map(|clip| clip.rect);
        buffer.clip_radius = node.effective_clip.map(|clip| clip.radius).unwrap_or(0);
        buffer.clip_rect_precise = shared_geometry
            .and_then(|geometry| geometry.clip_rect_precise)
            .or_else(|| {
                node.resolved_effective_clip
                    .map(|clip| precise_rect_from_resolved(clip.rect))
            });
        buffer.clip_radius_precise = node
            .resolved_effective_clip
            .map(|clip| clip.radius.to_f32().max(0.0));
        buffers.push(buffer);
    }
}

fn collect_icon_buffers(
    node: &super::ComputedDecorationNode,
    path: String,
    order_map: &std::collections::HashMap<String, usize>,
    dirty_node_ids: Option<&std::collections::HashSet<&str>>,
    shared_edges: &std::collections::HashMap<String, SharedEdgeNodeGeometry>,
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
            shared_edges,
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
    let shared_geometry = node
        .stable_id
        .as_deref()
        .and_then(|stable_id| shared_edges.get(stable_id))
        .copied();

    let spec = IconSpec {
        rect: node.rect,
        rect_precise: Some(
            shared_geometry
                .map(|geometry| geometry.rect_precise)
                .unwrap_or_else(|| precise_rect_from_resolved(node.resolved_rect)),
        ),
        icon: snapshot.icon.clone(),
        app_id: snapshot.app_id.clone(),
        raster_scale,
    };

    if let Some(buffer) = rasterizer.render_icon(&spec) {
        let mut buffer = buffer;
        buffer.owner_node_id = node.stable_id.clone();
        buffer.stable_key = format!("{path}:icon");
        buffer.order = *order_map
            .get(&format!("{path}:icon"))
            .unwrap_or(&usize::MAX);
        buffer.rect_precise = Some(
            shared_geometry
                .map(|geometry| geometry.rect_precise)
                .unwrap_or_else(|| precise_rect_from_resolved(node.resolved_rect)),
        );
        buffer.clip_rect = node.effective_clip.map(|clip| clip.rect);
        buffer.clip_radius = node.effective_clip.map(|clip| clip.radius).unwrap_or(0);
        buffer.clip_rect_precise = shared_geometry
            .and_then(|geometry| geometry.clip_rect_precise)
            .or_else(|| {
                node.resolved_effective_clip
                    .map(|clip| precise_rect_from_resolved(clip.rect))
            });
        buffer.clip_radius_precise = node
            .resolved_effective_clip
            .map(|clip| clip.radius.to_f32().max(0.0));
        buffers.push(buffer);
    }
}

fn push_cached_fill(
    buffers: &mut Vec<CachedDecorationBuffer>,
    order: usize,
    stable_key: String,
    rect: LogicalRect,
    rect_precise: Option<PreciseLogicalRect>,
    color: super::Color,
    owner_node_id: Option<String>,
    radius: i32,
    radius_precise: Option<f32>,
    border_width: f32,
    hole_rect: Option<LogicalRect>,
    hole_radius: i32,
    hole_rect_precise: Option<PreciseLogicalRect>,
    hole_radius_precise: Option<f32>,
    clip_rect_precise: Option<PreciseLogicalRect>,
    clip_radius_precise: Option<f32>,
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
        rect_precise: rect_precise.or_else(|| Some(precise_rect_from_logical(rect))),
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
        radius_precise,
        border_width,
        hole_rect,
        hole_rect_precise,
        hole_radius,
        hole_radius_precise,
        shared_inner_hole: false,
        clip_rect,
        clip_radius,
        clip_rect_precise,
        clip_radius_precise,
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

fn gap_debug_layout_enabled() -> bool {
    std::env::var_os("SHOJI_GAP_LAYOUT_DEBUG").is_some()
        || std::env::var_os("SHOJI_GAP_DEBUG").is_some()
}

fn resolved_rect_right(rect: crate::ssd::ResolvedLogicalRect) -> f32 {
    rect.x.to_f32() + rect.width.to_f32()
}

fn resolved_rect_bottom(rect: crate::ssd::ResolvedLogicalRect) -> f32 {
    rect.y.to_f32() + rect.height.to_f32()
}

fn format_resolved_rect(rect: crate::ssd::ResolvedLogicalRect) -> String {
    format!(
        "x={:.3}, y={:.3}, w={:.3}, h={:.3}, right={:.3}, bottom={:.3}",
        rect.x.to_f32(),
        rect.y.to_f32(),
        rect.width.to_f32(),
        rect.height.to_f32(),
        resolved_rect_right(rect),
        resolved_rect_bottom(rect),
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct SharedEdgeId(u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SharedEdgeAxis {
    Horizontal,
    Vertical,
}

#[derive(Debug, Clone, Copy)]
struct SharedEdgeSpec {
    axis: SharedEdgeAxis,
    logical_raw: i32,
}

#[derive(Debug, Clone, Copy)]
struct SharedEdgeRect {
    left: SharedEdgeId,
    top: SharedEdgeId,
    right: SharedEdgeId,
    bottom: SharedEdgeId,
}

#[derive(Debug, Clone)]
struct SharedEdgeTreeNode {
    stable_id: String,
    kind: &'static str,
    rect: SharedEdgeRect,
    content_rect: SharedEdgeRect,
    clip_rect: Option<SharedEdgeRect>,
    children: Vec<SharedEdgeTreeNode>,
}

#[derive(Debug, Clone)]
struct SharedEdgeTree {
    edges: Vec<SharedEdgeSpec>,
    root: SharedEdgeTreeNode,
}

#[derive(Debug, Clone, Copy)]
struct SharedEdgeNodeGeometry {
    rect_precise: PreciseLogicalRect,
    content_rect_precise: PreciseLogicalRect,
    clip_rect_precise: Option<PreciseLogicalRect>,
}

#[derive(Debug, Clone, Copy)]
struct SharedEdgeNodeRefs {
    rect: SharedEdgeRect,
    content: SharedEdgeRect,
}

#[derive(Debug, Clone, Copy, Default)]
struct SharedEdgeBuildContext {
    parent: Option<SharedEdgeNodeRefs>,
    previous_sibling: Option<SharedEdgeNodeRefs>,
}

#[derive(Default)]
struct SharedEdgeBuilder {
    next_id: u32,
    edges: Vec<SharedEdgeSpec>,
}

impl SharedEdgeBuilder {
    fn edge(
        &mut self,
        axis: SharedEdgeAxis,
        logical_raw: i32,
        candidates: &[SharedEdgeId],
    ) -> SharedEdgeId {
        if let Some(existing) = candidates.iter().copied().find(|candidate| {
            let spec = self.spec(*candidate);
            spec.axis == axis && spec.logical_raw == logical_raw
        }) {
            return existing;
        }

        let id = SharedEdgeId(self.next_id);
        self.next_id += 1;
        self.edges.push(SharedEdgeSpec { axis, logical_raw });
        id
    }

    fn spec(&self, id: SharedEdgeId) -> SharedEdgeSpec {
        self.edges[id.0 as usize]
    }

    fn rect_from_resolved(
        &mut self,
        rect: crate::ssd::ResolvedLogicalRect,
        context: SharedEdgeBuildContext,
    ) -> SharedEdgeRect {
        let mut left_candidates = Vec::new();
        let mut top_candidates = Vec::new();
        let mut right_candidates = Vec::new();
        let mut bottom_candidates = Vec::new();

        if let Some(parent) = context.parent {
            left_candidates.extend([parent.rect.left, parent.content.left]);
            top_candidates.extend([parent.rect.top, parent.content.top]);
            right_candidates.extend([parent.rect.right, parent.content.right]);
            bottom_candidates.extend([parent.rect.bottom, parent.content.bottom]);
        }

        if let Some(previous) = context.previous_sibling {
            left_candidates.extend([
                previous.rect.left,
                previous.rect.right,
                previous.content.left,
                previous.content.right,
            ]);
            top_candidates.extend([
                previous.rect.top,
                previous.rect.bottom,
                previous.content.top,
                previous.content.bottom,
            ]);
            right_candidates.extend([
                previous.rect.left,
                previous.rect.right,
                previous.content.left,
                previous.content.right,
            ]);
            bottom_candidates.extend([
                previous.rect.top,
                previous.rect.bottom,
                previous.content.top,
                previous.content.bottom,
            ]);
        }

        SharedEdgeRect {
            left: self.edge(SharedEdgeAxis::Vertical, rect.x.raw(), &left_candidates),
            top: self.edge(SharedEdgeAxis::Horizontal, rect.y.raw(), &top_candidates),
            right: self.edge(
                SharedEdgeAxis::Vertical,
                (rect.x + rect.width).raw(),
                &right_candidates,
            ),
            bottom: self.edge(
                SharedEdgeAxis::Horizontal,
                (rect.y + rect.height).raw(),
                &bottom_candidates,
            ),
        }
    }

    fn inner_rect_from_resolved(
        &mut self,
        rect: crate::ssd::ResolvedLogicalRect,
        context: SharedEdgeBuildContext,
        rect_refs: SharedEdgeRect,
    ) -> SharedEdgeRect {
        let mut left_candidates = vec![rect_refs.left];
        let mut top_candidates = vec![rect_refs.top];
        let mut right_candidates = vec![rect_refs.right];
        let mut bottom_candidates = vec![rect_refs.bottom];

        if let Some(parent) = context.parent {
            left_candidates.extend([parent.rect.left, parent.content.left]);
            top_candidates.extend([parent.rect.top, parent.content.top]);
            right_candidates.extend([parent.rect.right, parent.content.right]);
            bottom_candidates.extend([parent.rect.bottom, parent.content.bottom]);
        }

        if let Some(previous) = context.previous_sibling {
            left_candidates.extend([
                previous.rect.left,
                previous.rect.right,
                previous.content.left,
                previous.content.right,
            ]);
            top_candidates.extend([
                previous.rect.top,
                previous.rect.bottom,
                previous.content.top,
                previous.content.bottom,
            ]);
            right_candidates.extend([
                previous.rect.left,
                previous.rect.right,
                previous.content.left,
                previous.content.right,
            ]);
            bottom_candidates.extend([
                previous.rect.top,
                previous.rect.bottom,
                previous.content.top,
                previous.content.bottom,
            ]);
        }

        SharedEdgeRect {
            left: self.edge(SharedEdgeAxis::Vertical, rect.x.raw(), &left_candidates),
            top: self.edge(SharedEdgeAxis::Horizontal, rect.y.raw(), &top_candidates),
            right: self.edge(
                SharedEdgeAxis::Vertical,
                (rect.x + rect.width).raw(),
                &right_candidates,
            ),
            bottom: self.edge(
                SharedEdgeAxis::Horizontal,
                (rect.y + rect.height).raw(),
                &bottom_candidates,
            ),
        }
    }

    fn build_node(
        &mut self,
        node: &super::ComputedDecorationNode,
        context: SharedEdgeBuildContext,
    ) -> SharedEdgeTreeNode {
        let rect = self.rect_from_resolved(node.resolved_rect, context);
        let content_rect = self.inner_rect_from_resolved(node.resolved_content_rect, context, rect);
        let clip_rect = node
            .resolved_effective_clip
            .map(|clip| self.inner_rect_from_resolved(clip.rect, context, content_rect));
        let refs = SharedEdgeNodeRefs {
            rect,
            content: content_rect,
        };
        let mut previous_sibling = None;
        let mut children = Vec::with_capacity(node.children.len());
        for child in &node.children {
            let child_context = SharedEdgeBuildContext {
                parent: Some(refs),
                previous_sibling,
            };
            let child_node = self.build_node(child, child_context);
            previous_sibling = Some(SharedEdgeNodeRefs {
                rect: child_node.rect,
                content: child_node.content_rect,
            });
            children.push(child_node);
        }

        SharedEdgeTreeNode {
            stable_id: node.stable_id.as_deref().unwrap_or("<none>").to_string(),
            kind: node_kind_name(&node.kind),
            rect,
            content_rect,
            clip_rect,
            children,
        }
    }
}

fn build_shared_edge_tree(layout: &ComputedDecorationTree) -> SharedEdgeTree {
    let mut builder = SharedEdgeBuilder::default();
    let root = builder.build_node(&layout.root, SharedEdgeBuildContext::default());
    SharedEdgeTree {
        edges: builder.edges,
        root,
    }
}

fn precise_rect_from_shared_edge_rect(
    tree: &SharedEdgeTree,
    rect: SharedEdgeRect,
) -> PreciseLogicalRect {
    let left = tree.edges[rect.left.0 as usize].logical_raw as f32
        / crate::ssd::RESOLVED_LAYOUT_SUBPIXELS as f32;
    let top = tree.edges[rect.top.0 as usize].logical_raw as f32
        / crate::ssd::RESOLVED_LAYOUT_SUBPIXELS as f32;
    let right = tree.edges[rect.right.0 as usize].logical_raw as f32
        / crate::ssd::RESOLVED_LAYOUT_SUBPIXELS as f32;
    let bottom = tree.edges[rect.bottom.0 as usize].logical_raw as f32
        / crate::ssd::RESOLVED_LAYOUT_SUBPIXELS as f32;

    PreciseLogicalRect {
        x: left,
        y: top,
        width: (right - left).max(0.0),
        height: (bottom - top).max(0.0),
    }
}

fn collect_shared_edge_node_geometry(
    tree: &SharedEdgeTree,
    node: &SharedEdgeTreeNode,
    out: &mut std::collections::HashMap<String, SharedEdgeNodeGeometry>,
) {
    if node.stable_id != "<none>" {
        out.insert(
            node.stable_id.clone(),
            SharedEdgeNodeGeometry {
                rect_precise: precise_rect_from_shared_edge_rect(tree, node.rect),
                content_rect_precise: precise_rect_from_shared_edge_rect(tree, node.content_rect),
                clip_rect_precise: node
                    .clip_rect
                    .map(|rect| precise_rect_from_shared_edge_rect(tree, rect)),
            },
        );
    }

    for child in &node.children {
        collect_shared_edge_node_geometry(tree, child, out);
    }
}

fn build_shared_edge_geometry_map(
    layout: &ComputedDecorationTree,
) -> std::collections::HashMap<String, SharedEdgeNodeGeometry> {
    let tree = build_shared_edge_tree(layout);
    let mut map = std::collections::HashMap::new();
    collect_shared_edge_node_geometry(&tree, &tree.root, &mut map);
    map
}

fn format_shared_edge_id(id: SharedEdgeId) -> String {
    format!("e{}", id.0)
}

fn format_shared_edge_rect(rect: SharedEdgeRect) -> String {
    format!(
        "L={}, T={}, R={}, B={}",
        format_shared_edge_id(rect.left),
        format_shared_edge_id(rect.top),
        format_shared_edge_id(rect.right),
        format_shared_edge_id(rect.bottom),
    )
}

fn format_shared_edge_rect_values(rect: SharedEdgeRect, tree: &SharedEdgeTree) -> String {
    let left = tree.edges[rect.left.0 as usize].logical_raw as f32
        / crate::ssd::RESOLVED_LAYOUT_SUBPIXELS as f32;
    let top = tree.edges[rect.top.0 as usize].logical_raw as f32
        / crate::ssd::RESOLVED_LAYOUT_SUBPIXELS as f32;
    let right = tree.edges[rect.right.0 as usize].logical_raw as f32
        / crate::ssd::RESOLVED_LAYOUT_SUBPIXELS as f32;
    let bottom = tree.edges[rect.bottom.0 as usize].logical_raw as f32
        / crate::ssd::RESOLVED_LAYOUT_SUBPIXELS as f32;
    format!("left={left:.3}, top={top:.3}, right={right:.3}, bottom={bottom:.3}")
}

fn log_gap_shared_edge_tree(snapshot: &WaylandWindowSnapshot, layout: &ComputedDecorationTree) {
    let tree = build_shared_edge_tree(layout);
    let slot_node = find_shared_edge_node_by_kind(&tree.root, "window-slot");

    info!(
        window_id = snapshot.id,
        title = snapshot.title,
        edge_count = tree.edges.len(),
        root_rect_edges = %format_shared_edge_rect(tree.root.rect),
        root_content_edges = %format_shared_edge_rect(tree.root.content_rect),
        slot_rect_edges = slot_node.map(|node| format_shared_edge_rect(node.rect)),
        slot_content_edges = slot_node.map(|node| format_shared_edge_rect(node.content_rect)),
        "gap shared edge summary"
    );

    log_gap_shared_edge_node(snapshot, &tree, &tree.root, None, 0);
}

fn find_shared_edge_node_by_kind<'a>(
    node: &'a SharedEdgeTreeNode,
    kind: &str,
) -> Option<&'a SharedEdgeTreeNode> {
    if node.kind == kind {
        return Some(node);
    }

    node.children
        .iter()
        .find_map(|child| find_shared_edge_node_by_kind(child, kind))
}

fn log_gap_shared_edge_node(
    snapshot: &WaylandWindowSnapshot,
    tree: &SharedEdgeTree,
    node: &SharedEdgeTreeNode,
    parent: Option<&SharedEdgeTreeNode>,
    depth: usize,
) {
    info!(
        window_id = snapshot.id,
        depth,
        kind = node.kind,
        stable_id = node.stable_id,
        rect_edges = %format_shared_edge_rect(node.rect),
        rect_edge_values = %format_shared_edge_rect_values(node.rect, tree),
        content_edges = %format_shared_edge_rect(node.content_rect),
        content_edge_values = %format_shared_edge_rect_values(node.content_rect, tree),
        clip_edges = node.clip_rect.map(format_shared_edge_rect),
        clip_edge_values = node
            .clip_rect
            .map(|rect| format_shared_edge_rect_values(rect, tree)),
        shares_parent_content_left = parent
            .map(|parent| node.rect.left == parent.content_rect.left),
        shares_parent_content_top = parent
            .map(|parent| node.rect.top == parent.content_rect.top),
        shares_parent_content_right = parent
            .map(|parent| node.rect.right == parent.content_rect.right),
        shares_parent_content_bottom = parent
            .map(|parent| node.rect.bottom == parent.content_rect.bottom),
        rect_shares_own_content_left = node.rect.left == node.content_rect.left,
        rect_shares_own_content_top = node.rect.top == node.content_rect.top,
        rect_shares_own_content_right = node.rect.right == node.content_rect.right,
        rect_shares_own_content_bottom = node.rect.bottom == node.content_rect.bottom,
        "gap shared edge node"
    );

    for child in &node.children {
        log_gap_shared_edge_node(snapshot, tree, child, Some(node), depth + 1);
    }
}

fn log_gap_layout_tree(
    snapshot: &WaylandWindowSnapshot,
    client_rect: LogicalRect,
    layout: &ComputedDecorationTree,
) {
    let Some(slot_rect) = layout.window_slot_rect() else {
        return;
    };
    let Some(slot_resolved_rect) = layout.root.resolved_window_slot_rect() else {
        return;
    };

    let root = &layout.root;
    let root_content_rect = root.resolved_content_rect.round_to_logical_rect();
    info!(
        window_id = snapshot.id,
        title = snapshot.title,
        client_rect = %format_rect(client_rect),
        root_rect = %format_rect(root.rect),
        root_rect_resolved = %format_resolved_rect(root.resolved_rect),
        root_content_rect = %format_rect(root_content_rect),
        root_content_rect_resolved = %format_resolved_rect(root.resolved_content_rect),
        slot_rect = %format_rect(slot_rect),
        slot_rect_resolved = %format_resolved_rect(slot_resolved_rect),
        logical_slot_right_delta_vs_client = (slot_rect.x + slot_rect.width) - (client_rect.x + client_rect.width),
        logical_slot_bottom_delta_vs_client = (slot_rect.y + slot_rect.height) - (client_rect.y + client_rect.height),
        resolved_slot_right_delta_vs_client = resolved_rect_right(slot_resolved_rect)
            - (client_rect.x + client_rect.width) as f32,
        resolved_slot_bottom_delta_vs_client = resolved_rect_bottom(slot_resolved_rect)
            - (client_rect.y + client_rect.height) as f32,
        logical_root_content_right_delta_vs_slot = (root_content_rect.x + root_content_rect.width)
            - (slot_rect.x + slot_rect.width),
        logical_root_content_bottom_delta_vs_slot = (root_content_rect.y + root_content_rect.height)
            - (slot_rect.y + slot_rect.height),
        resolved_root_content_right_delta_vs_slot = resolved_rect_right(root.resolved_content_rect)
            - resolved_rect_right(slot_resolved_rect),
        resolved_root_content_bottom_delta_vs_slot = resolved_rect_bottom(root.resolved_content_rect)
            - resolved_rect_bottom(slot_resolved_rect),
        "gap layout summary"
    );

    log_gap_shared_edge_tree(snapshot, layout);
    log_gap_layout_node(snapshot, root, None, 0);
}

fn log_gap_layout_node(
    snapshot: &WaylandWindowSnapshot,
    node: &super::ComputedDecorationNode,
    parent: Option<&super::ComputedDecorationNode>,
    depth: usize,
) {
    let rect_right = node.rect.x + node.rect.width;
    let rect_bottom = node.rect.y + node.rect.height;
    let content_rect = node.resolved_content_rect.round_to_logical_rect();
    let content_right = content_rect.x + content_rect.width;
    let content_bottom = content_rect.y + content_rect.height;

    let parent_outer_left_delta =
        parent.map(|parent| node.resolved_rect.x.to_f32() - parent.resolved_rect.x.to_f32());
    let parent_content_left_delta = parent
        .map(|parent| node.resolved_rect.x.to_f32() - parent.resolved_content_rect.x.to_f32());
    let parent_outer_top_delta =
        parent.map(|parent| node.resolved_rect.y.to_f32() - parent.resolved_rect.y.to_f32());
    let parent_content_top_delta = parent
        .map(|parent| node.resolved_rect.y.to_f32() - parent.resolved_content_rect.y.to_f32());
    let parent_outer_right_delta =
        parent.map(|parent| (parent.rect.x + parent.rect.width) - rect_right);
    let parent_content_right_delta = parent.map(|parent| {
        let parent_content_rect = parent.resolved_content_rect.round_to_logical_rect();
        (parent_content_rect.x + parent_content_rect.width) - rect_right
    });
    let parent_outer_right_delta_resolved = parent.map(|parent| {
        resolved_rect_right(parent.resolved_rect) - resolved_rect_right(node.resolved_rect)
    });
    let parent_content_right_delta_resolved = parent.map(|parent| {
        resolved_rect_right(parent.resolved_content_rect) - resolved_rect_right(node.resolved_rect)
    });

    let child_union = (!node.children.is_empty()).then(|| {
        let mut min_x = f32::INFINITY;
        let mut min_y = f32::INFINITY;
        let mut max_right = f32::NEG_INFINITY;
        let mut max_bottom = f32::NEG_INFINITY;
        for child in &node.children {
            min_x = min_x.min(child.resolved_rect.x.to_f32());
            min_y = min_y.min(child.resolved_rect.y.to_f32());
            max_right = max_right.max(resolved_rect_right(child.resolved_rect));
            max_bottom = max_bottom.max(resolved_rect_bottom(child.resolved_rect));
        }
        (min_x, min_y, max_right, max_bottom)
    });

    info!(
        window_id = snapshot.id,
        depth,
        kind = node_kind_name(&node.kind),
        stable_id = node.stable_id.as_deref().unwrap_or("<none>"),
        rect = %format_rect(node.rect),
        rect_resolved = %format_resolved_rect(node.resolved_rect),
        rect_right,
        rect_bottom,
        content_rect = %format_rect(content_rect),
        content_rect_resolved = %format_resolved_rect(node.resolved_content_rect),
        content_right,
        content_bottom,
        border_width_logical = node.resolved_border_width.round_to_i32(),
        border_width_resolved = node.resolved_border_width.to_f32(),
        border_radius_logical = node.resolved_border_radius.round_to_i32(),
        border_radius_resolved = node.resolved_border_radius.to_f32(),
        parent_kind = parent
            .map(|parent| node_kind_name(&parent.kind))
            .unwrap_or("<root>"),
        parent_outer_left_delta,
        parent_content_left_delta,
        parent_outer_top_delta,
        parent_content_top_delta,
        parent_outer_right_delta,
        parent_content_right_delta,
        parent_outer_right_delta_resolved,
        parent_content_right_delta_resolved,
        child_union_left_resolved = child_union.map(|union| union.0),
        child_union_top_resolved = child_union.map(|union| union.1),
        child_union_right_resolved = child_union.map(|union| union.2),
        child_union_bottom_resolved = child_union.map(|union| union.3),
        child_union_right_delta_from_outer = child_union
            .map(|union| resolved_rect_right(node.resolved_rect) - union.2),
        child_union_right_delta_from_content = child_union
            .map(|union| resolved_rect_right(node.resolved_content_rect) - union.2),
        child_union_bottom_delta_from_outer = child_union
            .map(|union| resolved_rect_bottom(node.resolved_rect) - union.3),
        child_union_bottom_delta_from_content = child_union
            .map(|union| resolved_rect_bottom(node.resolved_content_rect) - union.3),
        "gap layout node"
    );

    for child in &node.children {
        log_gap_layout_node(snapshot, child, Some(node), depth + 1);
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

    if gap_debug_layout_enabled() {
        log_gap_layout_tree(snapshot, client_rect, layout);
    }

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
    format!(
        "x={}, y={}, w={}, h={}",
        rect.x, rect.y, rect.width, rect.height
    )
}

fn format_color(color: super::Color) -> String {
    format!("rgba({}, {}, {}, {})", color.r, color.g, color.b, color.a)
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
        previous_shader_buffers.iter().map(|item| {
            (
                item.stable_key.clone(),
                (item.rect, format!("{:?}", item.shader)),
            )
        }),
        next_shader_buffers.iter().map(|item| {
            (
                item.stable_key.clone(),
                (item.rect, format!("{:?}", item.shader)),
            )
        }),
        &mut damage,
    );
    collect_keyed_rect_damage(
        previous_text_buffers.iter().map(|item| {
            (
                format!(
                    "text:{}:{}:{}:{}:{}:{}",
                    item.order,
                    item.rect.x,
                    item.rect.y,
                    item.rect.width,
                    item.rect.height,
                    item.text
                ),
                (item.rect, format!("{:?}", item.color)),
            )
        }),
        next_text_buffers.iter().map(|item| {
            (
                format!(
                    "text:{}:{}:{}:{}:{}:{}",
                    item.order,
                    item.rect.x,
                    item.rect.y,
                    item.rect.width,
                    item.rect.height,
                    item.text
                ),
                (item.rect, format!("{:?}", item.color)),
            )
        }),
        &mut damage,
    );
    collect_keyed_rect_damage(
        previous_icon_buffers.iter().map(|item| {
            (
                format!(
                    "icon:{}:{}:{}:{}:{}",
                    item.order, item.rect.x, item.rect.y, item.rect.width, item.rect.height
                ),
                (item.rect, String::new()),
            )
        }),
        next_icon_buffers.iter().map(|item| {
            (
                format!(
                    "icon:{}:{}:{}:{}:{}",
                    item.order, item.rect.x, item.rect.y, item.rect.width, item.rect.height
                ),
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
            crate::ssd::EffectInvalidationPolicy::Manual {
                dirty_when: false,
                ..
            }
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
) where
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
        BorderStyle, BoxNode, Color, DecorationNode, DecorationNodeKind, DecorationStyle, Edges,
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

    #[test]
    fn content_clip_matches_window_slot_not_border_inner() {
        let tree = DecorationTree::new(
            DecorationNode::new(DecorationNodeKind::WindowBorder)
                .with_style(DecorationStyle {
                    border: Some(BorderStyle {
                        width: 2,
                        color: Color::WHITE,
                    }),
                    border_radius: Some(18),
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
            .layout_for_client_with_scale(LogicalRect::new(50, 100, 800, 600), 1.25)
            .expect("layout should succeed");
        let slot = layout.window_slot_rect().expect("slot should exist");
        let shared_edges = build_shared_edge_geometry_map(&layout);
        let clip = content_clip_for_layout(&tree, &layout, &shared_edges)
            .expect("content clip should exist");

        assert_eq!(clip.rect.loc.x, slot.x);
        assert_eq!(clip.rect.loc.y, slot.y);
        assert_eq!(clip.rect.size.w, slot.width);
        assert_eq!(clip.rect.size.h, slot.height);
        assert_eq!(clip.radius, 0);
        assert_eq!(clip.radius_precise, 0.0);
        assert!(clip.corner_radii.iter().all(|radius| *radius > 0));
        assert!(clip.corner_radii_precise.iter().all(|radius| *radius > 0.0));
    }

    #[test]
    fn content_clip_keeps_all_shared_corners_when_slot_fills_inner_mask() {
        let tree = DecorationTree::new(
            DecorationNode::new(DecorationNodeKind::WindowBorder)
                .with_style(DecorationStyle {
                    border: Some(BorderStyle {
                        width: 2,
                        color: Color::WHITE,
                    }),
                    border_radius: Some(18),
                    ..Default::default()
                })
                .with_children(vec![DecorationNode::new(DecorationNodeKind::WindowSlot)]),
        );

        let layout = tree
            .layout_for_client_with_scale(LogicalRect::new(50, 100, 800, 600), 1.25)
            .expect("layout should succeed");
        let shared_edges = build_shared_edge_geometry_map(&layout);
        let clip = content_clip_for_layout(&tree, &layout, &shared_edges)
            .expect("content clip should exist");

        assert!(clip.corner_radii.iter().all(|radius| *radius > 0));
        assert!(clip.corner_radii_precise.iter().all(|radius| *radius > 0.0));
    }

    #[test]
    fn content_clip_separates_slot_rect_from_ancestor_mask_through_padding_box() {
        let tree = DecorationTree::new(
            DecorationNode::new(DecorationNodeKind::WindowBorder)
                .with_style(DecorationStyle {
                    border: Some(BorderStyle {
                        width: 2,
                        color: Color::WHITE,
                    }),
                    border_radius: Some(18),
                    ..Default::default()
                })
                .with_children(vec![
                    DecorationNode::new(DecorationNodeKind::Box(BoxNode {
                        direction: LayoutDirection::Column,
                    }))
                    .with_style(DecorationStyle {
                        padding: Edges::all(5),
                        ..Default::default()
                    })
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
            .layout_for_client_with_scale(LogicalRect::new(50, 100, 800, 600), 1.25)
            .expect("layout should succeed");
        let slot = layout.window_slot_rect().expect("slot should exist");
        let shared_edges = build_shared_edge_geometry_map(&layout);
        let clip = content_clip_for_layout(&tree, &layout, &shared_edges)
            .expect("content clip should exist");

        assert_eq!(clip.rect.loc.x, slot.x);
        assert_eq!(clip.rect.loc.y, slot.y);
        assert_eq!(clip.rect.size.w, slot.width);
        assert_eq!(clip.rect.size.h, slot.height);
        assert!(clip.mask_rect.loc.x < clip.rect.loc.x);
        assert!(clip.mask_rect.loc.y < clip.rect.loc.y);
        assert!(clip.mask_rect.size.w > clip.rect.size.w);
        assert!(clip.mask_rect.size.h > clip.rect.size.h);
        assert!(clip.corner_radii_precise[2] > 0.0);
        assert!(clip.corner_radii_precise[3] > 0.0);
    }

    #[test]
    fn slot_content_clip_uses_nearest_rounded_mask_radius() {
        let rounded_mask = crate::ssd::ResolvedDecorationClip {
            rect: crate::ssd::ResolvedLogicalRect {
                x: crate::ssd::ResolvedLayoutValue::from_raw(100),
                y: crate::ssd::ResolvedLayoutValue::from_raw(200),
                width: crate::ssd::ResolvedLayoutValue::from_raw(1000),
                height: crate::ssd::ResolvedLayoutValue::from_raw(800),
            },
            radius: crate::ssd::ResolvedLayoutValue::from_i32(18),
        };
        let radius = rounded_mask.radius.to_f32();

        assert_eq!([radius; 4], [18.0, 18.0, 18.0, 18.0]);
    }

    #[test]
    fn shared_edge_tree_reuses_parent_content_edges_for_window_border() {
        let tree = DecorationTree::new(
            DecorationNode::new(DecorationNodeKind::WindowBorder)
                .with_style(DecorationStyle {
                    border: Some(BorderStyle {
                        width: 2,
                        color: Color::WHITE,
                    }),
                    ..Default::default()
                })
                .with_children(vec![
                    DecorationNode::new(DecorationNodeKind::Box(BoxNode {
                        direction: LayoutDirection::Column,
                    }))
                    .with_children(vec![DecorationNode::new(DecorationNodeKind::WindowSlot)]),
                ]),
        );

        let layout = tree
            .layout_for_client_with_scale(LogicalRect::new(50, 100, 800, 600), 1.75)
            .expect("layout should succeed");
        let edge_tree = build_shared_edge_tree(&layout);
        let window_border = &edge_tree.root.children[0];

        assert_eq!(edge_tree.root.content_rect.left, window_border.rect.left);
        assert_eq!(edge_tree.root.content_rect.top, window_border.rect.top);
        assert_eq!(edge_tree.root.content_rect.right, window_border.rect.right);
        assert_eq!(
            edge_tree.root.content_rect.bottom,
            window_border.rect.bottom
        );
    }

    #[test]
    fn shared_edge_tree_reuses_box_edges_when_content_matches_rect() {
        let tree = DecorationTree::new(
            DecorationNode::new(DecorationNodeKind::WindowBorder)
                .with_style(DecorationStyle {
                    border: Some(BorderStyle {
                        width: 2,
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
            .layout_for_client_with_scale(LogicalRect::new(50, 100, 800, 600), 1.75)
            .expect("layout should succeed");
        let edge_tree = build_shared_edge_tree(&layout);
        let inner_box = &edge_tree.root.children[0];
        let window_slot = &inner_box.children[1];

        assert_eq!(inner_box.rect.left, inner_box.content_rect.left);
        assert_eq!(inner_box.rect.top, inner_box.content_rect.top);
        assert_eq!(inner_box.rect.right, inner_box.content_rect.right);
        assert_eq!(inner_box.rect.bottom, inner_box.content_rect.bottom);

        assert_eq!(inner_box.content_rect.left, window_slot.rect.left);
        assert_eq!(inner_box.content_rect.right, window_slot.rect.right);
    }

    #[test]
    fn shared_edge_tree_reuses_adjacent_row_sibling_boundary() {
        let tree = DecorationTree::new(
            DecorationNode::new(DecorationNodeKind::WindowBorder)
                .with_style(DecorationStyle {
                    border: Some(BorderStyle {
                        width: 2,
                        color: Color::WHITE,
                    }),
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
                        .with_style(DecorationStyle {
                            width: Some(120),
                            ..Default::default()
                        }),
                        DecorationNode::new(DecorationNodeKind::WindowSlot),
                    ]),
                ]),
        );

        let layout = tree
            .layout_for_client_with_scale(LogicalRect::new(50, 100, 800, 600), 1.75)
            .expect("layout should succeed");
        let edge_tree = build_shared_edge_tree(&layout);
        let row_box = &edge_tree.root.children[0];
        let left_box = &row_box.children[0];
        let window_slot = &row_box.children[1];

        assert_eq!(left_box.rect.right, window_slot.rect.left);
        assert_eq!(left_box.rect.top, window_slot.rect.top);
        assert_eq!(left_box.rect.bottom, window_slot.rect.bottom);
    }

    #[test]
    fn shared_edge_tree_reuses_adjacent_column_sibling_boundary() {
        let tree = DecorationTree::new(
            DecorationNode::new(DecorationNodeKind::WindowBorder)
                .with_style(DecorationStyle {
                    border: Some(BorderStyle {
                        width: 2,
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
                            height: Some(48),
                            ..Default::default()
                        }),
                        DecorationNode::new(DecorationNodeKind::WindowSlot),
                    ]),
                ]),
        );

        let layout = tree
            .layout_for_client_with_scale(LogicalRect::new(50, 100, 800, 600), 1.75)
            .expect("layout should succeed");
        let edge_tree = build_shared_edge_tree(&layout);
        let column_box = &edge_tree.root.children[0];
        let header_box = &column_box.children[0];
        let window_slot = &column_box.children[1];

        assert_eq!(header_box.rect.bottom, window_slot.rect.top);
        assert_eq!(header_box.rect.left, window_slot.rect.left);
        assert_eq!(header_box.rect.right, window_slot.rect.right);
    }
}
