use smithay::{
    backend::renderer::element::solid::SolidColorBuffer,
    desktop::Window,
    utils::{Logical, Point},
};
use std::time::Instant;
use tracing::{debug, trace};

use crate::state::ShojiWM;

use super::{
    ComputedDecorationTree, DecorationEvaluationError, DecorationEvaluator,
    DecorationHitTestResult, DecorationTree, LogicalPoint, LogicalRect, WaylandWindowSnapshot,
    evaluate_dynamic_decoration,
};

#[derive(Debug, Clone)]
pub struct WindowDecorationState {
    pub snapshot: WaylandWindowSnapshot,
    pub tree: DecorationTree,
    pub layout: ComputedDecorationTree,
    pub client_rect: LogicalRect,
    pub buffers: Vec<CachedDecorationBuffer>,
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
    pub rect: LogicalRect,
    pub color: super::Color,
    pub buffer: SolidColorBuffer,
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
    ) -> Result<super::DecorationNode, DecorationEvaluationError> {
        match self {
            Self::Static(evaluator) => evaluator.evaluate_window(window),
            Self::Node(evaluator) => evaluator.evaluate_window(window),
        }
    }
}

impl ShojiWM {
    pub fn suggested_window_location(
        &self,
        snapshot: &WaylandWindowSnapshot,
    ) -> Result<(i32, i32), DecorationEvaluationError> {
        let tree = evaluate_dynamic_decoration(&self.decoration_evaluator, snapshot)?;
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
        let windows: Vec<Window> = self.space.elements().cloned().collect();
        let window_count = windows.len();
        let mut rebuilt = 0usize;
        let mut relayout = 0usize;

        self.window_decorations.retain(|window, _| windows.contains(window));

        for window in windows {
            let client_rect = match self.window_client_rect(&window) {
                Some(rect) => rect,
                None => continue,
            };
            let snapshot = self.snapshot_window(&window);

            let needs_tree = self
                .window_decorations
                .get(&window)
                .map(|cached| cached.snapshot != snapshot)
                .unwrap_or(true);

            if needs_tree {
                let started_at = Instant::now();
                let tree = evaluate_dynamic_decoration(&self.decoration_evaluator, &snapshot)?;
                let layout = tree
                    .layout_for_client(client_rect)
                    .map_err(super::DecorationEvaluationError::Layout)?;
                let buffers = build_cached_buffers(&layout);
                rebuilt += 1;
                debug!(
                    window_id = snapshot.id,
                    title = snapshot.title,
                    elapsed_ms = started_at.elapsed().as_secs_f64() * 1000.0,
                    "rebuilt window decoration tree"
                );
                log_decoration_refresh("rebuild", &snapshot, client_rect, &layout, &buffers);
                self.window_decorations.insert(
                    window,
                    WindowDecorationState {
                        snapshot,
                        tree,
                        layout,
                        client_rect,
                        buffers,
                    },
                );
            } else if let Some(cached) = self.window_decorations.get_mut(&window) {
                if cached.client_rect != client_rect {
                    let started_at = Instant::now();
                    cached.layout = cached
                        .tree
                        .layout_for_client(client_rect)
                        .map_err(super::DecorationEvaluationError::Layout)?;
                    cached.client_rect = client_rect;
                    cached.snapshot = snapshot;
                    cached.buffers = build_cached_buffers(&cached.layout);
                    relayout += 1;
                    debug!(
                        window_id = cached.snapshot.id,
                        title = cached.snapshot.title,
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
                }
            }
        }

        debug!(
            window_count,
            rebuilt,
            relayout,
            elapsed_ms = refresh_started_at.elapsed().as_secs_f64() * 1000.0,
            "refresh_window_decorations finished"
        );

        Ok(())
    }

    pub fn decoration_under(
        &self,
        point: Point<f64, Logical>,
    ) -> Option<(Window, DecorationHitTestResult)> {
        self.space
            .elements()
            .rev()
            .find_map(|window| {
                let decoration = self.window_decorations.get(window)?;
                let hit = decoration.hit_test(point);
                (!matches!(hit, DecorationHitTestResult::Outside | DecorationHitTestResult::ClientArea))
                    .then_some((window.clone(), hit))
            })
    }

    fn window_client_rect(&self, window: &Window) -> Option<LogicalRect> {
        let loc = self.space.element_location(window)?;
        let geometry = window.geometry();
        Some(LogicalRect::new(
            loc.x,
            loc.y,
            geometry.size.w,
            geometry.size.h,
        ))
    }
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
    for primitive in layout.render_primitives() {
        match primitive {
            super::DecorationRenderPrimitive::FillRect { rect, color, .. } => {
                buffers.push(CachedDecorationBuffer {
                    rect,
                    color,
                    buffer: SolidColorBuffer::new(
                        (rect.width, rect.height),
                        [
                            color.r as f32 / 255.0,
                            color.g as f32 / 255.0,
                            color.b as f32 / 255.0,
                            color.a as f32 / 255.0,
                        ],
                    ),
                });
            }
            super::DecorationRenderPrimitive::BorderRect { rect, width, color, .. } => {
                let width = width.max(0);
                if width == 0 {
                    continue;
                }

                let parts = [
                    LogicalRect::new(rect.x, rect.y, rect.width, width),
                    LogicalRect::new(rect.x, rect.y + rect.height - width, rect.width, width),
                    LogicalRect::new(rect.x, rect.y, width, rect.height),
                    LogicalRect::new(rect.x + rect.width - width, rect.y, width, rect.height),
                ];

                for part in parts {
                    if part.width > 0 && part.height > 0 {
                        buffers.push(CachedDecorationBuffer {
                            rect: part,
                            color,
                            buffer: SolidColorBuffer::new(
                                (part.width, part.height),
                                [
                                    color.r as f32 / 255.0,
                                    color.g as f32 / 255.0,
                                    color.b as f32 / 255.0,
                                    color.a as f32 / 255.0,
                                ],
                            ),
                        });
                    }
                }
            }
            super::DecorationRenderPrimitive::Label { .. }
            | super::DecorationRenderPrimitive::AppIcon { .. }
            | super::DecorationRenderPrimitive::WindowSlot { .. } => {}
        }
    }
    buffers
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
