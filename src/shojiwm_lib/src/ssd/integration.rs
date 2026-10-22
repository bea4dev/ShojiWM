use bumpalo::Bump;
use hashbrown::{DefaultHashBuilder, HashMap as BumpHashMap};
use smithay::{
    desktop::Window,
    reexports::wayland_protocols::xdg::shell::server::xdg_toplevel,
    utils::{Logical, Point, Rectangle, Size},
};
use std::{
    collections::BTreeMap,
    hash::{Hash, Hasher},
    time::{Duration, Instant},
};
use tracing::{debug, info, trace, warn};

use crate::backend::paint::PaintElementState;
use crate::backend::visual::RectSnapMode;
use crate::backend::visual::{inverse_transform_point, transformed_root_rect};

/// Margin added on top of `closeAnimationDuration × 2` when deriving a
/// closing snapshot's watchdog deadline; absorbs scheduler jitter around the
/// TS-side `closePoll` that normally delivers `finalizeClose`.
const CLOSING_SNAPSHOT_DEADLINE_MARGIN_MS: u64 = 1_000;
/// Watchdog deadline for closing snapshots whose close-animation duration is
/// unknown (config declared none, or the isolate died before answering).
/// Deliberately generous: with a known duration the per-window deadline
/// applies instead, so this only bounds genuinely broken closes.
const CLOSING_SNAPSHOT_FALLBACK_DEADLINE_MS: u64 = 30_000;
use crate::backend::{
    icon::{CachedDecorationIcon, IconSpec},
    shader_effect::CachedShaderEffect,
    text::{CachedDecorationLabel, LabelSpec},
    visual::PreciseLogicalRect,
};
use crate::state::{ActiveManagedWindowAnimation, ShojiWM};

use super::{
    ComputedDecorationTree, DecorationEvaluationError, DecorationEvaluator,
    DecorationHandlerInvocation, DecorationHitTestResult, DecorationNode, DecorationTree,
    LogicalPoint, LogicalRect, StaticDecorationEvaluator, WaylandLayerSnapshot,
    WaylandPopupSnapshot, WaylandWindowAction, WaylandWindowSnapshot, WindowEffectConfig,
    WindowPositionSnapshot, WindowTransform, reapply_tree_preserving_layout,
    window_model::{
        ManagedWindowAnimationEasingSnapshot, ManagedWindowAnimationMode,
        ManagedWindowAnimationSnapshot, ManagedWindowPointAnimationSnapshot,
        ManagedWindowPointSnapshot, ManagedWindowRectAnimationSnapshot, ManagedWindowRectSnapshot,
        ManagedWindowScalarAnimationSnapshot, ManagedWindowState,
    },
};
use crate::runtime_api::{
    CompositionPatch, OVERLAY_STAGE_INDEX, PAINT_STAGE_INDEX, SHADER_INPUT_STAGE_INDEX,
};

#[derive(Debug, Clone, Copy)]
struct CachedTreeUpdate {
    changed: bool,
    layout_equivalent: bool,
}

#[derive(Debug, Default)]
struct ShaderUniformFastUpdate {
    tree_changed: bool,
    rendered_changed: bool,
    damage_rects: Vec<LogicalRect>,
}

fn is_shader_uniform_only_update(
    full: &Option<DecorationNode>,
    patches: &[CompositionPatch],
) -> bool {
    full.is_none()
        && !patches.is_empty()
        && patches
            .iter()
            .all(|patch| matches!(patch, CompositionPatch::ShaderUniform { .. }))
}

/// The uniforms of a node's `paint` / `overlay` shader addressed by a
/// patch `stage_index`, or `None` for effect pipeline stages.
fn paint_slot_for_stage(stage_index: usize) -> Option<super::paint::PaintSlot> {
    match stage_index {
        PAINT_STAGE_INDEX => Some(super::paint::PaintSlot::Paint),
        OVERLAY_STAGE_INDEX => Some(super::paint::PaintSlot::Overlay),
        _ => None,
    }
}

fn paint_shader_mut(
    style: &mut super::DecorationStyle,
    slot: super::paint::PaintSlot,
) -> Option<&mut super::PaintShader> {
    match slot {
        super::paint::PaintSlot::Paint => style.paint.as_mut(),
        _ => style.overlay.as_mut(),
    }
}

fn validate_paint_uniform(
    style: &super::DecorationStyle,
    slot: super::paint::PaintSlot,
    node_id: &str,
    name: &str,
    value: &super::ShaderUniformValue,
) -> Result<(), DecorationEvaluationError> {
    let shader = match slot {
        super::paint::PaintSlot::Paint => style.paint.as_ref(),
        _ => style.overlay.as_ref(),
    };
    let current = shader.and_then(|shader| shader.uniforms.get(name));
    match current {
        Some(current) if current.shape_matches(value) => Ok(()),
        _ => Err(DecorationEvaluationError::RuntimeProtocol(format!(
            "paint uniform patch target is missing: {node_id}.{name}"
        ))),
    }
}

fn apply_shader_uniform_fast_update(
    tree: &mut DecorationTree,
    layout: &mut ComputedDecorationTree,
    buffers: &mut [CachedDecorationBuffer],
    shader_buffers: &mut [CachedShaderEffect],
    patches: &[CompositionPatch],
) -> Result<ShaderUniformFastUpdate, DecorationEvaluationError> {
    for patch in patches {
        let CompositionPatch::ShaderUniform {
            node_id,
            stage_index,
            name,
            value,
        } = patch
        else {
            return Err(DecorationEvaluationError::RuntimeProtocol(
                "non-uniform patch reached the shader uniform fast path".into(),
            ));
        };

        let Some(tree_node) = find_decoration_node(&tree.root, node_id) else {
            return Err(DecorationEvaluationError::RuntimeProtocol(format!(
                "cached composition patch target is missing: {node_id}"
            )));
        };
        let Some(computed_node) = find_computed_decoration_node(&layout.root, node_id) else {
            return Err(DecorationEvaluationError::RuntimeProtocol(format!(
                "computed shader uniform patch target is missing: {node_id}"
            )));
        };

        if let Some(slot) = paint_slot_for_stage(*stage_index) {
            validate_paint_uniform(&tree_node.style, slot, node_id, name, value)?;
            validate_paint_uniform(&computed_node.style, slot, node_id, name, value)?;
            continue;
        }

        let super::DecorationNodeKind::ShaderEffect(tree_effect) = &tree_node.kind else {
            return Err(DecorationEvaluationError::RuntimeProtocol(format!(
                "shader uniform patch target is not ShaderEffect: {node_id}"
            )));
        };
        validate_shader_uniform(&tree_effect.shader, node_id, *stage_index, name)?;

        let super::DecorationNodeKind::ShaderEffect(computed_effect) = &computed_node.kind else {
            return Err(DecorationEvaluationError::RuntimeProtocol(format!(
                "computed shader uniform patch target is not ShaderEffect: {node_id}"
            )));
        };
        validate_shader_uniform(&computed_effect.shader, node_id, *stage_index, name)?;

        for buffer in shader_buffers
            .iter()
            .filter(|buffer| buffer.owner_node_id.as_deref() == Some(node_id.as_str()))
        {
            validate_shader_uniform(&buffer.shader, node_id, *stage_index, name)?;
        }
    }

    let mut update = ShaderUniformFastUpdate::default();
    for patch in patches {
        let CompositionPatch::ShaderUniform {
            node_id,
            stage_index,
            name,
            value,
        } = patch
        else {
            unreachable!("uniform-only update was validated above");
        };

        if let Some(slot) = paint_slot_for_stage(*stage_index) {
            let set = |style: &mut super::DecorationStyle| -> bool {
                let shader = paint_shader_mut(style, slot).expect("paint uniform was validated");
                let current = shader
                    .uniforms
                    .get_mut(name)
                    .expect("paint uniform was validated");
                if current == value {
                    return false;
                }
                current.clone_from(value);
                true
            };
            let tree_node = find_decoration_node_mut(&mut tree.root, node_id)
                .expect("uniform patch tree target was validated");
            update.tree_changed |= set(&mut tree_node.style);
            let computed_node = find_computed_decoration_node_mut(&mut layout.root, node_id)
                .expect("uniform patch computed target was validated");
            set(&mut computed_node.style);

            let suffix = format!(":{}", slot.key());
            for buffer in buffers.iter_mut().filter(|buffer| {
                buffer.owner_node_id.as_deref() == Some(node_id.as_str())
                    && buffer.stable_key.ends_with(&suffix)
            }) {
                let Some(current) = buffer.paint.uniforms.get_mut(name) else {
                    continue;
                };
                if current != value {
                    current.clone_from(value);
                    update.rendered_changed = true;
                    update.damage_rects.push(buffer.rect);
                }
            }
            continue;
        }

        let tree_node = find_decoration_node_mut(&mut tree.root, node_id)
            .expect("uniform patch tree target was validated");
        let super::DecorationNodeKind::ShaderEffect(tree_effect) = &mut tree_node.kind else {
            unreachable!("uniform patch tree target kind was validated");
        };
        update.tree_changed |=
            set_shader_uniform(&mut tree_effect.shader, *stage_index, name, value);

        let computed_node = find_computed_decoration_node_mut(&mut layout.root, node_id)
            .expect("uniform patch computed target was validated");
        let super::DecorationNodeKind::ShaderEffect(computed_effect) = &mut computed_node.kind
        else {
            unreachable!("uniform patch computed target kind was validated");
        };
        set_shader_uniform(&mut computed_effect.shader, *stage_index, name, value);

        let freeze_rendered_shader = matches!(
            computed_effect.shader.invalidate_policy(),
            crate::ssd::EffectInvalidationPolicy::Manual {
                dirty_when: false,
                ..
            }
        );
        if freeze_rendered_shader {
            continue;
        }

        for buffer in shader_buffers
            .iter_mut()
            .filter(|buffer| buffer.owner_node_id.as_deref() == Some(node_id.as_str()))
        {
            if set_shader_uniform(&mut buffer.shader, *stage_index, name, value) {
                update.rendered_changed = true;
                update.damage_rects.push(buffer.rect);
            }
        }
    }

    update
        .damage_rects
        .sort_unstable_by_key(|rect| (rect.x, rect.y, rect.width, rect.height));
    update.damage_rects.dedup();
    Ok(update)
}

fn validate_shader_uniform(
    effect: &super::CompiledEffect,
    node_id: &str,
    stage_index: usize,
    name: &str,
) -> Result<(), DecorationEvaluationError> {
    let stage = if stage_index == SHADER_INPUT_STAGE_INDEX {
        match &effect.input {
            super::EffectInput::Shader(stage) => Some(stage),
            _ => None,
        }
    } else {
        match effect.pipeline.get(stage_index) {
            Some(super::EffectStage::Shader(stage)) => Some(stage),
            _ => None,
        }
    };
    let Some(stage) = stage else {
        return Err(DecorationEvaluationError::RuntimeProtocol(format!(
            "shader uniform patch stage is missing: {node_id}[{stage_index}]"
        )));
    };
    if !stage.uniforms.contains_key(name) {
        return Err(DecorationEvaluationError::RuntimeProtocol(format!(
            "shader uniform patch target is missing: {node_id}.{name}"
        )));
    }
    Ok(())
}

fn set_shader_uniform(
    effect: &mut super::CompiledEffect,
    stage_index: usize,
    name: &str,
    value: &super::ShaderUniformValue,
) -> bool {
    let stage = if stage_index == SHADER_INPUT_STAGE_INDEX {
        match &mut effect.input {
            super::EffectInput::Shader(stage) => Some(stage),
            _ => None,
        }
    } else {
        match effect.pipeline.get_mut(stage_index) {
            Some(super::EffectStage::Shader(stage)) => Some(stage),
            _ => None,
        }
    };
    let Some(stage) = stage else {
        unreachable!("shader uniform stage was validated");
    };
    let current = stage
        .uniforms
        .get_mut(name)
        .expect("shader uniform was validated");
    if !current.shape_matches(value) {
        unreachable!("shader uniform shape was validated");
    }
    if current == value {
        return false;
    }
    current.clone_from(value);
    true
}

fn apply_cached_tree_update(
    tree: &mut DecorationTree,
    full: Option<DecorationNode>,
    patches: Vec<CompositionPatch>,
) -> Result<CachedTreeUpdate, DecorationEvaluationError> {
    match (full, patches.is_empty()) {
        (Some(next_root), true) => {
            let changed = tree.root != next_root;
            let layout_equivalent = !changed || tree.root.layout_equivalent(&next_root);
            if changed {
                tree.root = next_root;
            }
            Ok(CachedTreeUpdate {
                changed,
                layout_equivalent,
            })
        }
        (Some(_), false) => Err(DecorationEvaluationError::RuntimeProtocol(
            "cached composition returned both a full tree and patches".into(),
        )),
        (None, true) => Err(DecorationEvaluationError::RuntimeProtocol(
            "cached composition returned neither a full tree nor patches".into(),
        )),
        (None, false) => {
            let mut changed = false;
            let mut layout_equivalent = true;
            for patch in patches {
                let node_id = patch.node_id().to_owned();
                let Some(target) = find_decoration_node_mut(&mut tree.root, &node_id) else {
                    return Err(DecorationEvaluationError::RuntimeProtocol(format!(
                        "cached composition patch target is missing: {}",
                        node_id
                    )));
                };
                match patch {
                    CompositionPatch::ReplaceNode { node, .. } => {
                        if *target == node {
                            continue;
                        }
                        layout_equivalent &= target.layout_equivalent(&node);
                        *target = node;
                        changed = true;
                    }
                    CompositionPatch::ReplaceProps { node, .. } => {
                        // Compared and swapped without the children, which stay.
                        let children = std::mem::take(&mut target.children);
                        if *target != node {
                            layout_equivalent &= target.layout_equivalent(&node);
                            *target = node;
                            changed = true;
                        }
                        target.children = children;
                    }
                    CompositionPatch::ShaderUniform {
                        stage_index,
                        name,
                        value,
                        ..
                    } => {
                        // A paint / overlay shader's uniform, on any node kind. These arrive
                        // next to node patches when a turn changed both.
                        if let Some(slot) = paint_slot_for_stage(stage_index) {
                            validate_paint_uniform(&target.style, slot, &node_id, &name, &value)?;
                            let shader = paint_shader_mut(&mut target.style, slot)
                                .expect("paint uniform was validated");
                            let current = shader
                                .uniforms
                                .get_mut(&name)
                                .expect("paint uniform was validated");
                            if *current != value {
                                *current = value;
                                changed = true;
                            }
                            continue;
                        }
                        let super::DecorationNodeKind::ShaderEffect(effect) = &mut target.kind
                        else {
                            return Err(DecorationEvaluationError::RuntimeProtocol(format!(
                                "shader uniform patch target is not ShaderEffect: {node_id}"
                            )));
                        };
                        validate_shader_uniform(&effect.shader, &node_id, stage_index, &name)?;
                        if set_shader_uniform(&mut effect.shader, stage_index, &name, &value) {
                            changed = true;
                        }
                    }
                }
            }
            Ok(CachedTreeUpdate {
                changed,
                layout_equivalent,
            })
        }
    }
}

fn find_decoration_node<'a>(
    node: &'a DecorationNode,
    stable_id: &str,
) -> Option<&'a DecorationNode> {
    if node.stable_id.as_deref() == Some(stable_id) {
        return Some(node);
    }
    node.children
        .iter()
        .find_map(|child| find_decoration_node(child, stable_id))
}

fn find_decoration_node_mut<'a>(
    node: &'a mut DecorationNode,
    stable_id: &str,
) -> Option<&'a mut DecorationNode> {
    if node.stable_id.as_deref() == Some(stable_id) {
        return Some(node);
    }
    node.children
        .iter_mut()
        .find_map(|child| find_decoration_node_mut(child, stable_id))
}

fn find_computed_decoration_node<'a>(
    node: &'a super::ComputedDecorationNode,
    stable_id: &str,
) -> Option<&'a super::ComputedDecorationNode> {
    if node.stable_id.as_deref() == Some(stable_id) {
        return Some(node);
    }
    node.children
        .iter()
        .find_map(|child| find_computed_decoration_node(child, stable_id))
}

fn find_computed_decoration_node_mut<'a>(
    node: &'a mut super::ComputedDecorationNode,
    stable_id: &str,
) -> Option<&'a mut super::ComputedDecorationNode> {
    if node.stable_id.as_deref() == Some(stable_id) {
        return Some(node);
    }
    node.children
        .iter_mut()
        .find_map(|child| find_computed_decoration_node_mut(child, stable_id))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EffectEvaluationCacheEntry {
    signature: u64,
    animating: bool,
}

type BumpNodeGeometryMap<'a> = BumpHashMap<&'a str, NodeGeometry, DefaultHashBuilder, &'a Bump>;

trait NodeGeometryLookup {
    fn node_geometry(&self, stable_id: &str) -> Option<NodeGeometry>;
}

impl NodeGeometryLookup for std::collections::HashMap<String, NodeGeometry> {
    fn node_geometry(&self, stable_id: &str) -> Option<NodeGeometry> {
        self.get(stable_id).copied()
    }
}

impl NodeGeometryLookup for BumpNodeGeometryMap<'_> {
    fn node_geometry(&self, stable_id: &str) -> Option<NodeGeometry> {
        self.get(stable_id).copied()
    }
}

fn handler_debug_enabled() -> bool {
    use std::sync::OnceLock;

    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var_os("SHOJI_SSD_HANDLER_DEBUG")
            .is_some_and(|value| value != "0" && !value.is_empty())
    })
}

fn animation_timing_debug_enabled() -> bool {
    use std::sync::OnceLock;

    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var_os("SHOJI_ANIMATION_TIMING_DEBUG")
            .is_some_and(|value| value != "0" && !value.is_empty())
    })
}

fn managed_rect_debug_enabled() -> bool {
    use std::sync::OnceLock;

    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var_os("SHOJI_MANAGED_RECT_DEBUG")
            .is_some_and(|value| value != "0" && !value.is_empty())
    })
}

fn managed_rect_path_debug_enabled() -> bool {
    use std::sync::OnceLock;

    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var_os("SHOJI_MANAGED_RECT_PATH_DEBUG")
            .is_some_and(|value| value != "0" && !value.is_empty())
    })
}

fn runtime_dirty_debug_enabled() -> bool {
    use std::sync::OnceLock;

    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var_os("SHOJI_RUNTIME_DIRTY_DEBUG")
            .or_else(|| std::env::var_os("SHOJI_SSD_SUPPRESSION_DEBUG"))
            .is_some_and(|value| value != "0" && !value.is_empty())
    })
}

fn minimize_debug_enabled() -> bool {
    use std::sync::OnceLock;

    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var_os("SHOJI_MINIMIZE_DEBUG")
            .is_some_and(|value| value != "0" && !value.is_empty())
    })
}

/// `SHOJI_ANIMATION_DEBUG=1` traces every schedule / cancel / per-frame advance
/// of a managed-window animation. Useful for diagnosing animation "ghost"
/// states (window ends up offscreen, hit-test missing, etc.).
fn managed_animation_debug_enabled() -> bool {
    use std::sync::OnceLock;

    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var_os("SHOJI_ANIMATION_DEBUG")
            .is_some_and(|value| value != "0" && !value.is_empty())
    })
}

fn hot_reload_debug_enabled() -> bool {
    use std::sync::OnceLock;

    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var_os("SHOJI_HOT_RELOAD_DEBUG")
            .is_some_and(|value| value != "0" && !value.is_empty())
    })
}

fn label_debug_enabled() -> bool {
    use std::sync::OnceLock;

    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var_os("SHOJI_LABEL_DEBUG").is_some_and(|value| value != "0" && !value.is_empty())
    })
}

#[derive(Default)]
struct ManagedRectPathStats {
    last_log: Option<Instant>,
    full_rebuild: usize,
    refresh_position_translate: usize,
    refresh_size_relayout: usize,
    runtime_dirty: usize,
    runtime_managed_only: usize,
    apply_noop: usize,
    apply_position_fast: usize,
    apply_size_fast: usize,
    apply_position: usize,
    apply_size: usize,
    apply_configure_only: usize,
}

enum ManagedRectPathEvent {
    FullRebuild,
    RefreshPositionTranslate,
    RefreshSizeRelayout,
    RuntimeDirty,
    RuntimeManagedOnly,
    ApplyNoop,
    ApplyPositionFast,
    ApplySizeFast,
    ApplyPosition,
    ApplySize,
    ApplyConfigureOnly,
}

fn record_managed_rect_path_event(event: ManagedRectPathEvent) {
    if !managed_rect_path_debug_enabled() {
        return;
    }

    use std::sync::{Mutex, OnceLock};

    static STATS: OnceLock<Mutex<ManagedRectPathStats>> = OnceLock::new();
    let stats = STATS.get_or_init(|| Mutex::new(ManagedRectPathStats::default()));
    let Ok(mut stats) = stats.lock() else {
        return;
    };

    match event {
        ManagedRectPathEvent::FullRebuild => stats.full_rebuild += 1,
        ManagedRectPathEvent::RefreshPositionTranslate => stats.refresh_position_translate += 1,
        ManagedRectPathEvent::RefreshSizeRelayout => stats.refresh_size_relayout += 1,
        ManagedRectPathEvent::RuntimeDirty => stats.runtime_dirty += 1,
        ManagedRectPathEvent::RuntimeManagedOnly => stats.runtime_managed_only += 1,
        ManagedRectPathEvent::ApplyNoop => stats.apply_noop += 1,
        ManagedRectPathEvent::ApplyPositionFast => stats.apply_position_fast += 1,
        ManagedRectPathEvent::ApplySizeFast => stats.apply_size_fast += 1,
        ManagedRectPathEvent::ApplyPosition => stats.apply_position += 1,
        ManagedRectPathEvent::ApplySize => stats.apply_size += 1,
        ManagedRectPathEvent::ApplyConfigureOnly => stats.apply_configure_only += 1,
    }

    let now = Instant::now();
    let last_log = *stats.last_log.get_or_insert(now);
    if now.duration_since(last_log) < Duration::from_secs(1) {
        return;
    }

    info!(
        full_rebuild = stats.full_rebuild,
        refresh_position_translate = stats.refresh_position_translate,
        refresh_size_relayout = stats.refresh_size_relayout,
        runtime_dirty = stats.runtime_dirty,
        runtime_managed_only = stats.runtime_managed_only,
        apply_noop = stats.apply_noop,
        apply_position_fast = stats.apply_position_fast,
        apply_size_fast = stats.apply_size_fast,
        apply_position = stats.apply_position,
        apply_size = stats.apply_size,
        apply_configure_only = stats.apply_configure_only,
        "managed rect path stats"
    );

    *stats = ManagedRectPathStats {
        last_log: Some(now),
        ..ManagedRectPathStats::default()
    };
}

fn animation_spike_threshold_ms() -> f64 {
    use std::sync::OnceLock;

    static THRESHOLD_MS: OnceLock<f64> = OnceLock::new();
    *THRESHOLD_MS.get_or_init(|| {
        std::env::var("SHOJI_ANIMATION_SPIKE_THRESHOLD_MS")
            .ok()
            .and_then(|value| value.parse::<f64>().ok())
            .filter(|value| *value > 0.0)
            .unwrap_or(12.0)
    })
}

fn animation_gap_debug_enabled() -> bool {
    use std::sync::OnceLock;

    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var_os("SHOJI_ANIMATION_GAP_DEBUG")
            .is_some_and(|value| value != "0" && !value.is_empty())
    })
}

fn log_animation_output_activity(
    output_name: &str,
    closing_active_count: usize,
    animation_active_for_target: bool,
) {
    if !animation_gap_debug_enabled() {
        return;
    }

    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};

    static STATE: OnceLock<Mutex<HashMap<String, (bool, usize)>>> = OnceLock::new();
    let state = STATE.get_or_init(|| Mutex::new(HashMap::new()));
    let Ok(mut guard) = state.lock() else {
        return;
    };
    let previous = guard.insert(
        output_name.to_string(),
        (animation_active_for_target, closing_active_count),
    );
    if previous != Some((animation_active_for_target, closing_active_count)) {
        info!(
            output_name,
            previous_animation_active = previous.map(|value| value.0),
            animation_active_for_target,
            previous_closing_active_count = previous.map(|value| value.1),
            closing_active_count,
            "animation gap: output activity transition"
        );
    }
}

fn log_animation_window_refresh_timing(
    phase: &'static str,
    snapshot: &WaylandWindowSnapshot,
    elapsed_ms: f64,
    evaluate_ms: f64,
    layout_ms: f64,
    clip_ms: f64,
    order_ms: f64,
    buffers_ms: f64,
    shader_ms: f64,
    text_ms: f64,
    icon_ms: f64,
    finalize_ms: f64,
    dirty_node_count: usize,
    tree_changed: Option<bool>,
    layout_equivalent: Option<bool>,
) {
    if !animation_timing_debug_enabled() || elapsed_ms < animation_spike_threshold_ms() {
        return;
    }

    warn!(
        phase,
        window_id = snapshot.id,
        title = snapshot.title,
        app_id = snapshot.app_id,
        elapsed_ms,
        evaluate_ms,
        layout_ms,
        clip_ms,
        order_ms,
        buffers_ms,
        shader_ms,
        text_ms,
        icon_ms,
        finalize_ms,
        dirty_node_count,
        tree_changed,
        layout_equivalent,
        "animation timing: decoration window spike"
    );
}

#[derive(Debug, Clone)]
pub struct WindowDecorationState {
    pub snapshot: WaylandWindowSnapshot,
    pub tree: DecorationTree,
    pub layout: ComputedDecorationTree,
    pub layout_scale: f64,
    pub client_rect: LogicalRect,
    /// `client_rect` is the materialised result of
    /// `managed_client_rect_for_state(tree, managed_window, _, layout_scale)`.
    /// It's coherent with the cached state immediately after rebuild/relayout,
    /// but the `runtime_dirty` branch can swap `tree`/`managed_window`/
    /// `layout_scale` *without* rerunning the probe loop, so the materialised
    /// rect lags by one tick. This flag records that lag so the per-refresh
    /// diff check can skip the probe loop entirely while the cache is
    /// coherent — which is the steady-state hot path (~25% CPU during
    /// ufo-test).
    pub client_rect_potentially_stale: bool,
    /// Sub-logical-pixel remainders of the TS-declared managed rect edges
    /// (`edge - round(edge)`, each in (-0.5, 0.5]). The integer layout/Space/
    /// configure pipeline drops these fractions; rendering adds them back when
    /// computing the root physical origin and frame size so rect animations
    /// move and resize at physical-pixel granularity instead of logical.
    pub root_subpixel_offset: crate::backend::visual::RootSubpixelEdges,
    /// Last animated transform produced by `advance_managed_window_animations`,
    /// **or** the static value when no animation is active. This is the value
    /// rendering reads — it changes per frame while an animation is in flight.
    pub visual_transform: WindowTransform,
    /// Last animated managed-window state. Same lifetime semantics as
    /// `visual_transform` — replaced per frame during animations, then frozen
    /// at the final sample until the next TS evaluation arrives.
    pub managed_window: super::ManagedWindowState,
    /// True while Rust-side managed-window animation is actively driving this
    /// window. Hidden workspaces are represented as `idle` at rest, but a
    /// workspace-switch animation must still be renderable while it is bringing
    /// an idle window back on screen.
    pub managed_window_animation_active: bool,
    /// The client size (width, height) most recently delivered via xdg /
    /// X11 configure. Tracked separately from `client_rect` because we want
    /// to keep the client configured at the animation's final target size for
    /// the entire animation — sending the animated intermediate size each
    /// frame would make the client buffer chase a moving target and the
    /// compositor would scale the lagging buffer up to fit, producing the
    /// "buffer stretched" visual. By pinning the configure size to the
    /// animation's composed final target we only need to send one configure
    /// for the resize, while the visual rect still animates smoothly (the
    /// client buffer is rendered at its committed size and our viewporter /
    /// SSD layout handles the rest).
    pub last_configured_client_size: Option<(i32, i32)>,
    /// Composition-declared transform from the most recent TS evaluation,
    /// **without** any animation deltas applied. `advance_managed_window_animations`
    /// resets `visual_transform` from this each frame before sampling the
    /// active animations, so additive animations (mode = add / sub / multiply)
    /// don't compound from one frame to the next.
    pub static_visual_transform: WindowTransform,
    /// Composition-declared managed-window state from the most recent TS
    /// evaluation. Same role as `static_visual_transform` — anchors animation
    /// sampling so each frame starts from the composition's intent rather than
    /// from yesterday's animated result.
    pub static_managed_window: super::ManagedWindowState,
    pub window_effects: Option<super::WindowEffectConfig>,
    pub content_clip: Option<ContentClip>,
    pub buffers: Vec<CachedDecorationBuffer>,
    pub shader_buffers: Vec<CachedShaderEffect>,
    pub text_buffers: Vec<CachedDecorationLabel>,
    pub icon_buffers: Vec<CachedDecorationIcon>,
    pub paint_cache: std::collections::HashMap<String, PaintElementState>,
    pub shader_cache:
        std::collections::HashMap<String, crate::backend::shader_effect::ShaderEffectElementState>,
    pub backdrop_cache:
        std::collections::HashMap<String, crate::backend::shader_effect::CachedBackdropTexture>,
    pub window_effect_cache:
        std::collections::HashMap<String, crate::backend::shader_effect::WindowEffectElementState>,
}

#[derive(Debug, Clone, Copy)]
pub struct ContentClip {
    // Reserved client slot geometry. This is the xdg window-geometry origin
    // used to place the client surface tree, independently of whether the
    // surface is clipped.
    pub rect: Rectangle<i32, Logical>,
    pub rect_precise: PreciseLogicalRect,
    // True only when an ancestor SSD explicitly clips its children. A bare
    // WindowSlot is placement metadata and must not crop client-owned CSD,
    // shadows, or resize margins outside xdg_surface.window_geometry.
    pub clips_surface: bool,
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
    /// The window's shape for a window effect's `EffectContext`: the frame
    /// of the `<WindowBorder>` (or the root), or — for an effect over the
    /// root surface only — the client area inside its border.
    pub fn effect_frame_shape(
        &self,
        root_surface_only: bool,
    ) -> crate::backend::shader_effect::NodeEffectShape {
        fn find_border(
            node: &super::ComputedDecorationNode,
        ) -> Option<&super::ComputedDecorationNode> {
            if matches!(node.kind, super::DecorationNodeKind::WindowBorder) {
                return Some(node);
            }
            node.children.iter().find_map(find_border)
        }
        let node = find_border(&self.layout.root).unwrap_or(&self.layout.root);
        let geometry = super::paint::node_geometry(node, Default::default());
        crate::backend::shader_effect::NodeEffectShape {
            layout_scale: node.frame.scale,
            radius: if root_surface_only {
                geometry.inner_radius
            } else {
                geometry.radius
            },
            border: if root_surface_only {
                [0; 4]
            } else {
                geometry.border
            },
            clip: None,
        }
    }

    pub fn hit_test(&self, point: Point<f64, Logical>) -> DecorationHitTestResult {
        self.layout.hit_test_at(point.x, point.y)
    }

    /// Whether global `pos` lies in the `<WindowBorder>` resize band outside
    /// the frame (see `DecorationLayout::resize_hit_outset`).
    pub fn resize_band_contains(&self, pos: Point<f64, Logical>) -> bool {
        if self.layout.resize_hit_outset() <= 0.0 {
            return false;
        }
        let local = inverse_transform_point(pos, self.layout.root.rect, self.visual_transform);
        let root = self.layout.root.rect;
        let inside_root = local.x >= root.x as f64
            && local.y >= root.y as f64
            && local.x < (root.x + root.width) as f64
            && local.y < (root.y + root.height) as f64;
        !inside_root && matches!(self.hit_test(local), DecorationHitTestResult::Resize(_))
    }

    /// The open `<Popup>`s, which the backends draw in their own pass.
    pub fn popup_scopes(&self) -> super::PopupScopes {
        super::PopupScopes::of(&self.layout.root)
    }

    /// Whether the tree has a `<Popup>`, open or not.
    pub fn has_popup(&self) -> bool {
        fn any(node: &super::ComputedDecorationNode) -> bool {
            matches!(node.kind, super::DecorationNodeKind::Popup(_))
                || node.children.iter().any(any)
        }
        any(&self.layout.root)
    }

    /// Whether an open `Auto` / `Manual` popup takes pointer input.
    pub fn has_open_interactive_popup(&self) -> bool {
        fn any(node: &super::ComputedDecorationNode) -> bool {
            if node.style.visible == Some(false) {
                return false;
            }
            matches!(&node.kind, super::DecorationNodeKind::Popup(popup) if popup.mode.is_interactive())
                || node.children.iter().any(any)
        }
        any(&self.layout.root)
    }

    pub fn managed_window_allows_render(&self) -> bool {
        !self.managed_window.managed
            || (self.managed_window.visible
                && (!self.managed_window.idle || self.managed_window_animation_active))
    }

    pub fn managed_window_allows_render_on_output(&self, output_name: &str) -> bool {
        self.managed_window_allows_render()
            && self
                .managed_window
                .visible_outputs
                .as_ref()
                .is_none_or(|outputs| outputs.iter().any(|output| output == output_name))
    }

    /// Whether the config hides the window on `output_name` (not shown, idle,
    /// or kept off that output), regardless of an animation that may still
    /// keep it rendering for a moment.
    pub fn managed_window_hidden_on_output(&self, output_name: &str) -> bool {
        self.managed_window.managed
            && (!self.managed_window.visible
                || self.managed_window.idle
                || self
                    .managed_window
                    .visible_outputs
                    .as_ref()
                    .is_some_and(|outputs| !outputs.iter().any(|output| output == output_name)))
    }

    pub fn managed_window_allows_input(&self) -> bool {
        self.managed_window_allows_render() && self.managed_window.interactive
    }

    pub fn managed_window_allows_input_on_output(&self, output_name: &str) -> bool {
        self.managed_window_allows_render_on_output(output_name) && self.managed_window.interactive
    }
}

/// One paint item of a decoration node (shadow, background, border or user
/// paint shader), see `ssd::paint`.
#[derive(Debug, Clone)]
pub struct CachedDecorationBuffer {
    pub owner_node_id: Option<String>,
    pub stable_key: String,
    pub order: usize,
    /// Logical bounds of everything the item draws, for culling and damage.
    pub rect: LogicalRect,
    pub source_kind: &'static str,
    pub paint: super::paint::PaintItem,
}

impl ShojiWM {
    fn decoration_layout_scale_for_window(&self, window: &Window) -> f64 {
        let visible_outputs = self.visible_outputs_for_window(window);
        let rect = self
            .window_decorations
            .get(window)
            .map(|decoration| decoration.layout.root.rect);
        self.layout_scale_for_rect_with_visible(rect, visible_outputs.as_deref())
    }

    fn decoration_layout_scale_for_rect(&self, rect: LogicalRect) -> f64 {
        self.layout_scale_for_rect_with_visible(Some(rect), None)
    }

    fn decoration_raster_scale_for_window(&self, window: &Window) -> f64 {
        let visible_outputs = self.visible_outputs_for_window(window);
        let rect = self
            .window_decorations
            .get(window)
            .map(|decoration| decoration.layout.root.rect);
        self.raster_scale_for_rect_with_visible(rect, visible_outputs.as_deref())
    }

    fn decoration_raster_scale_for_rect(&self, rect: LogicalRect) -> f64 {
        self.raster_scale_for_rect_with_visible(Some(rect), None)
    }

    fn visible_outputs_for_window(&self, window: &Window) -> Option<Vec<String>> {
        self.window_decorations
            .get(window)
            .and_then(|decoration| decoration.managed_window.visible_outputs.clone())
    }

    /// Scale selection that honours the window's `visibleOutputs`.
    ///
    /// Workspace scroll positions windows far outside the active viewport;
    /// without a filter, `space.outputs()` happily reports any output whose
    /// logical geometry happens to intersect that scrolled-out rect, and the
    /// computed scale flips to that unrelated monitor's value mid-scroll. The
    /// SSD layout / raster scale then mutates, which forces a full layout +
    /// buffer rebuild every frame the rect crosses a monitor boundary. On
    /// multi-monitor setups with mixed fractional scales this shows up as
    /// visible scroll frame drops even on high-end hardware.
    ///
    /// Strategy:
    /// 1. If the caller provided `visible_outputs`, restrict candidates to
    ///    that set; otherwise fall back to every output (the `_for_rect`
    ///    flavour preserves the previous behaviour for callers that don't
    ///    have a window context).
    /// 2. Prefer the max fractional scale among candidates whose geometry
    ///    intersects `rect`. This is the visually-correct value the moment
    ///    the window is on-screen.
    /// 3. If none intersect (scrolled completely off-screen of every visible
    ///    output, or `rect` is None), fall back to the max scale across the
    ///    candidate set itself. This keeps the SSD layout / raster scale
    ///    pinned to the workspace's "home" output rather than swinging to
    ///    whichever other monitor the rect happens to drift into.
    fn layout_scale_for_rect_with_visible(
        &self,
        rect: Option<LogicalRect>,
        visible_outputs: Option<&[String]>,
    ) -> f64 {
        self.fold_candidate_scales(rect, visible_outputs, 1.0, f64::max)
    }

    fn raster_scale_for_rect_with_visible(
        &self,
        rect: Option<LogicalRect>,
        visible_outputs: Option<&[String]>,
    ) -> f64 {
        // Decoration text and icons are rasterized at the layout scale itself,
        // so their buffers cover exactly the physical pixels the layout gave
        // them instead of being drawn at `ceil(scale)` and resampled down.
        self.layout_scale_for_rect_with_visible(rect, visible_outputs)
    }

    fn fold_candidate_scales<T, F>(
        &self,
        rect: Option<LogicalRect>,
        visible_outputs: Option<&[String]>,
        initial: T,
        mut combine: F,
    ) -> T
    where
        T: Copy,
        F: FnMut(T, f64) -> T,
    {
        let candidates: Vec<_> = self
            .space
            .outputs()
            .filter(|output| match visible_outputs {
                Some(allowed) => allowed.iter().any(|name| name == &output.name()),
                None => true,
            })
            .collect();

        let mut matched_any_intersection = false;
        let mut intersecting_acc = initial;
        if let Some(rect) = rect {
            let logical = smithay::utils::Rectangle::new(
                smithay::utils::Point::from((rect.x, rect.y)),
                (rect.width, rect.height).into(),
            );
            for output in &candidates {
                let Some(geometry) = self.space.output_geometry(output) else {
                    continue;
                };
                if logical.intersection(geometry).is_none() {
                    continue;
                }
                matched_any_intersection = true;
                intersecting_acc =
                    combine(intersecting_acc, output.current_scale().fractional_scale());
            }
        }
        if matched_any_intersection {
            return intersecting_acc;
        }

        let mut fallback_acc = initial;
        for output in &candidates {
            fallback_acc = combine(fallback_acc, output.current_scale().fractional_scale());
        }
        fallback_acc
    }

    /// Put the space element where the laid-out client rect says it is.
    ///
    /// The apply loop only relocates the element when the desired rect
    /// differs from the current layout, so any path that rewrites the layout
    /// without going through it leaves the element where the last frame that
    /// *did* go through put it. That happens at the end of a rect animation
    /// whose completion re-evaluates the window straight from the static rect
    /// (see the close-opacity fix): the last animated frame — an overshoot or
    /// a half-pixel rounding of the eased value — mapped the element at, say,
    /// y = 1 or y = −34, the completion wrote y = 0 into the layout, and the
    /// following frames were no-ops. The client surface then rendered offset
    /// from its own decoration by that residue, and for a fullscreen window
    /// the residue alone kept the fast path off, so the bars stayed visible
    /// over a game that believed it was fullscreen. Cheap enough to run on
    /// every no-op frame: one geometry read and one point compare.
    fn sync_space_location_to_client_rect(&mut self, window: &Window, client_rect: LogicalRect) {
        let geometry = window.geometry();
        let location = Point::from((
            client_rect.x - geometry.loc.x,
            client_rect.y - geometry.loc.y,
        ));
        if self.space.element_location(window) != Some(location) {
            record_managed_rect_path_event(ManagedRectPathEvent::ApplyPositionFast);
            self.space.relocate_element(window, location);
            self.schedule_redraw();
        }
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
        let previous_text_buffers = decoration.text_buffers.clone();

        if let Some(node) = invocation.node.clone() {
            decoration.tree = crate::ssd::DecorationTree::new(node);
            if let Ok(layout) = decoration.tree.layout_for_client_with_subpixel(
                decoration.client_rect,
                decoration.layout_scale,
                decoration.root_subpixel_offset,
            ) {
                decoration.layout = layout;
                let node_geometry = build_node_geometry_map(&decoration.layout);
                decoration.content_clip =
                    content_clip_for_layout(&decoration.tree, &decoration.layout, &node_geometry);
                let order_map = build_render_order_map(&decoration.layout);
                decoration.buffers = build_cached_buffers(&decoration.layout, &order_map);
                decoration.shader_buffers = build_shader_buffers(&decoration.layout, &order_map);
                decoration.text_buffers = build_text_buffers_with_fallback(
                    &decoration.layout,
                    &order_map,
                    raster_scale,
                    &mut self.text_rasterizer,
                    &previous_text_buffers,
                );
                decoration.icon_buffers = build_icon_buffers(
                    &decoration.layout,
                    &order_map,
                    raster_scale,
                    &decoration.snapshot,
                    &mut self.icon_rasterizer,
                );
                self.suggested_window_offset = suggested_window_offset(&decoration.layout);
                if handler_debug_enabled() {
                    log_decoration_refresh(
                        "runtime-handler",
                        &decoration.snapshot,
                        decoration.client_rect,
                        &decoration.layout,
                        &decoration.buffers,
                    );
                }
            } else if handler_debug_enabled() {
                warn!(
                    window_id = decoration.snapshot.id,
                    title = decoration.snapshot.title,
                    client_rect = %format_rect(decoration.client_rect),
                    layout_scale = decoration.layout_scale,
                    "runtime handler decoration relayout failed"
                );
            }
        }

        if let Some(transform) = invocation.transform {
            decoration.visual_transform = transform;
            decoration.static_visual_transform = transform;
        }
        if let Some(managed_window) = &invocation.managed_window {
            decoration.managed_window = managed_window.clone();
            decoration.static_managed_window = managed_window.clone();
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

    pub fn invoke_window_resize_event(
        &mut self,
        window_id: &str,
        event: &super::WindowResizeEventSnapshot,
        now_ms: u64,
    ) -> bool {
        self.sync_runtime_display_state();
        let invocation = match self.config_runtime.window_resize(window_id, event, now_ms) {
            Ok(invocation) => invocation,
            Err(error) => {
                warn!(window_id, ?error, "runtime window resize event failed");
                return false;
            }
        };

        self.drain_runtime_host_messages();

        if invocation.dirty {
            self.runtime_poll_dirty = true;
            self.mark_runtime_dirty_windows(
                invocation.dirty_window_ids,
                invocation.dirty_managed_window_ids,
                &invocation.dirty_window_node_ids,
            );
            self.request_tty_maintenance("runtime-window-resize-dirty");
            self.schedule_redraw();
        }
        if !invocation.actions.is_empty() {
            self.request_tty_maintenance("runtime-window-resize-actions");
            self.apply_runtime_window_actions(invocation.actions);
            self.schedule_redraw();
        }
        self.runtime_scheduler_enabled = invocation.next_poll_in_ms.is_some();
        if invocation.next_poll_in_ms == Some(0) {
            self.request_tty_maintenance("runtime-window-resize-animation");
            self.schedule_redraw();
        }

        invocation.invoked
    }

    pub fn invoke_window_move_event(
        &mut self,
        window_id: &str,
        event: &super::WindowMoveEventSnapshot,
        now_ms: u64,
    ) -> bool {
        self.sync_runtime_display_state();
        let invocation = match self.config_runtime.window_move(window_id, event, now_ms) {
            Ok(invocation) => invocation,
            Err(error) => {
                warn!(window_id, ?error, "runtime window move event failed");
                return false;
            }
        };

        self.drain_runtime_host_messages();

        if invocation.dirty {
            self.runtime_poll_dirty = true;
            self.mark_runtime_dirty_windows(
                invocation.dirty_window_ids,
                invocation.dirty_managed_window_ids,
                &invocation.dirty_window_node_ids,
            );
            self.request_tty_maintenance("runtime-window-move-dirty");
            self.schedule_redraw();
        }
        if !invocation.actions.is_empty() {
            self.request_tty_maintenance("runtime-window-move-actions");
            self.apply_runtime_window_actions(invocation.actions);
            self.schedule_redraw();
        }
        self.runtime_scheduler_enabled = invocation.next_poll_in_ms.is_some();
        if invocation.next_poll_in_ms == Some(0) {
            self.request_tty_maintenance("runtime-window-move-animation");
            self.schedule_redraw();
        }

        invocation.invoked
    }

    pub fn invoke_window_maximize_request_event(
        &mut self,
        snapshot: &WaylandWindowSnapshot,
        event: &super::WindowMaximizeRequestEventSnapshot,
        now_ms: u64,
    ) -> bool {
        self.sync_runtime_display_state();
        let invocation = match self
            .config_runtime
            .window_maximize_request(snapshot, event, now_ms)
        {
            Ok(invocation) => invocation,
            Err(error) => {
                warn!(
                    window_id = %snapshot.id,
                    ?error,
                    "runtime window maximize request event failed"
                );
                return false;
            }
        };
        self.handle_window_state_request_invocation("runtime-window-maximize-request", invocation)
    }

    pub fn invoke_window_minimize_request_event(
        &mut self,
        snapshot: &WaylandWindowSnapshot,
        event: &super::WindowMinimizeRequestEventSnapshot,
        now_ms: u64,
    ) -> bool {
        self.sync_runtime_display_state();
        let invocation = match self
            .config_runtime
            .window_minimize_request(snapshot, event, now_ms)
        {
            Ok(invocation) => invocation,
            Err(error) => {
                warn!(
                    window_id = %snapshot.id,
                    ?error,
                    "runtime window minimize request event failed"
                );
                return false;
            }
        };
        self.handle_window_state_request_invocation("runtime-window-minimize-request", invocation)
    }

    pub fn invoke_window_fullscreen_request_event(
        &mut self,
        snapshot: &WaylandWindowSnapshot,
        event: &super::WindowFullscreenRequestEventSnapshot,
        now_ms: u64,
    ) -> bool {
        self.sync_runtime_display_state();
        let invocation = match self
            .config_runtime
            .window_fullscreen_request(snapshot, event, now_ms)
        {
            Ok(invocation) => invocation,
            Err(error) => {
                warn!(
                    window_id = %snapshot.id,
                    ?error,
                    "runtime window fullscreen request event failed"
                );
                return false;
            }
        };
        self.handle_window_state_request_invocation("runtime-window-fullscreen-request", invocation)
    }

    pub fn invoke_window_activate_request_event(
        &mut self,
        snapshot: &WaylandWindowSnapshot,
        event: &super::WindowActivateRequestEventSnapshot,
        now_ms: u64,
    ) -> bool {
        self.sync_runtime_display_state();
        let invocation = match self
            .config_runtime
            .window_activate_request(snapshot, event, now_ms)
        {
            Ok(invocation) => invocation,
            Err(error) => {
                warn!(
                    window_id = %snapshot.id,
                    ?error,
                    "runtime window activate request event failed"
                );
                return false;
            }
        };
        self.handle_window_state_request_invocation("runtime-window-activate-request", invocation)
    }

    fn handle_window_state_request_invocation(
        &mut self,
        reason: &'static str,
        invocation: super::evaluator::DecorationWindowStateRequestInvocation,
    ) -> bool {
        self.drain_runtime_host_messages();

        if invocation.dirty {
            self.runtime_poll_dirty = true;
            self.mark_runtime_dirty_windows(
                invocation.dirty_window_ids,
                invocation.dirty_managed_window_ids,
                &invocation.dirty_window_node_ids,
            );
            self.request_tty_maintenance(reason);
            self.schedule_redraw();
        }
        if !invocation.actions.is_empty() {
            self.request_tty_maintenance(reason);
            self.apply_runtime_window_actions(invocation.actions);
            self.schedule_redraw();
        }
        self.runtime_scheduler_enabled = invocation.next_poll_in_ms.is_some();
        if invocation.next_poll_in_ms == Some(0) {
            self.request_tty_maintenance(reason);
            self.schedule_redraw();
        }

        invocation.invoked
    }

    pub fn managed_resize_initial_rect(
        &self,
        window: &Window,
        fallback: smithay::utils::Rectangle<i32, smithay::utils::Logical>,
    ) -> smithay::utils::Rectangle<i32, smithay::utils::Logical> {
        self.window_decorations
            .get(window)
            .filter(|decoration| decoration.managed_window.managed)
            .map(|decoration| {
                // The rect the config set, unless a rect animation is moving the window: then
                // the layout root is where it is drawn. The root can also be transient: a press
                // that focuses the window re-renders its chrome (e.g. a focus-dependent border)
                // and, for a moment, lays the previous tree out around the new client rect, which
                // reported the start rect inset by the border.
                match decoration.managed_window.rect {
                    Some(rect)
                        if !self
                            .managed_window_animations
                            .contains_key(&decoration.snapshot.id) =>
                    {
                        managed_rect_snapshot_to_logical_rect(rect)
                    }
                    _ => decoration.layout.root.rect,
                }
            })
            .map(|rect| {
                smithay::utils::Rectangle::new(
                    (rect.x, rect.y).into(),
                    (rect.width, rect.height).into(),
                )
            })
            .unwrap_or(fallback)
    }

    /// Force-finalize every closing snapshot whose per-window deadline has
    /// passed (see `ClosingWindowSnapshot::finalize_deadline_ms`). Normal
    /// closes never reach this — a firing watchdog means the TS finalize
    /// handshake was lost, so it warns.
    fn finalize_closing_snapshots_past_deadline(&mut self, now_ms: u64) {
        let stale_closing_window_ids: Vec<String> = self
            .closing_window_snapshots
            .iter()
            .filter(|(_, closing)| now_ms >= closing.finalize_deadline_ms)
            .map(|(window_id, _)| window_id.clone())
            .collect();
        if stale_closing_window_ids.is_empty() {
            return;
        }
        if let Some(closing) = stale_closing_window_ids
            .first()
            .and_then(|window_id| self.closing_window_snapshots.get(window_id))
        {
            warn!(
                window_ids = ?stale_closing_window_ids,
                stalled_for_ms = now_ms.saturating_sub(closing.promoted_at_ms),
                "closing snapshot watchdog: force-finalizing stalled close animation(s)"
            );
        }
        self.force_finalize_closing_snapshots(stale_closing_window_ids);
    }

    /// Deterministically finalize all closing snapshots right now. Used when
    /// the TS isolate is about to be replaced (config hot reload): the new
    /// isolate has no `closePoll` timers or per-window close state for
    /// windows that were mid-close, so their `finalizeClose` would never
    /// arrive and the snapshots' `GlesTexture`s would only be reclaimed by
    /// the watchdog deadline. Cutting the animations short at the reload
    /// boundary is both deterministic and visually unobjectionable — the
    /// whole scene re-evaluates anyway.
    pub fn finalize_all_closing_snapshots(&mut self, reason: &'static str) {
        let window_ids: Vec<String> = self.closing_window_snapshots.keys().cloned().collect();
        if window_ids.is_empty() {
            return;
        }
        info!(
            window_ids = ?window_ids,
            reason, "finalizing all closing snapshots"
        );
        self.force_finalize_closing_snapshots(window_ids);
    }

    fn force_finalize_closing_snapshots(&mut self, window_ids: Vec<String>) {
        for window_id in &window_ids {
            if let Some(closing) = self.closing_window_snapshots.get(window_id) {
                let stale_root = transformed_root_rect(
                    closing.decoration.layout.root.rect,
                    closing.decoration.visual_transform,
                );
                self.pending_decoration_damage.push(stale_root);
            }
        }
        self.apply_runtime_window_actions(
            window_ids
                .into_iter()
                .map(|window_id| crate::ssd::RuntimeWindowAction {
                    window_id,
                    action: crate::ssd::WaylandWindowAction::FinalizeClose,
                    animation: None,
                    channel: None,
                })
                .collect(),
        );
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
        self.complete_window_snapshot_trackers.remove(window_id);
        let live_snapshot = self.live_window_snapshots.remove(window_id);
        let Some(mut live_snapshot) = live_snapshot else {
            self.live_window_snapshot_trackers.remove(window_id);
            return Ok(false);
        };
        crate::backend::snapshot::retarget_snapshot_rect(
            &mut live_snapshot,
            decoration.client_rect,
        );

        self.sync_runtime_display_state();
        let invocation = self.config_runtime.start_close(window_id, now_ms)?;
        self.drain_runtime_host_messages();
        if !invocation.invoked {
            self.live_window_snapshots
                .insert(window_id.to_string(), live_snapshot);
            // Other windows' changes came with it.
            self.apply_runtime_handler_mutation(invocation);
            return Ok(false);
        }
        self.live_window_snapshot_trackers.remove(window_id);

        // Derive the watchdog deadline from the close-animation duration the
        // config declared (`window.setCloseAnimationDuration`). ×2 plus a
        // margin tolerates scheduler jitter around the TS-side `closePoll`;
        // a missing/zero duration falls back to a generous constant so an
        // isolate that died before answering still gets reclaimed.
        let close_duration_ms = invocation.close_animation_duration_ms.unwrap_or(0);
        let finalize_deadline_ms = if close_duration_ms > 0 {
            now_ms
                .saturating_add(close_duration_ms.saturating_mul(2))
                .saturating_add(CLOSING_SNAPSHOT_DEADLINE_MARGIN_MS)
        } else {
            now_ms.saturating_add(CLOSING_SNAPSHOT_FALLBACK_DEADLINE_MS)
        };
        debug!(
            window_id,
            close_duration_ms,
            deadline_in_ms = finalize_deadline_ms.saturating_sub(now_ms),
            "promoted window to closing snapshot"
        );
        self.closing_window_snapshots.insert(
            window_id.to_string(),
            crate::backend::snapshot::ClosingWindowSnapshot {
                window_id: window_id.to_string(),
                live: live_snapshot,
                decoration: {
                    let mut decoration = decoration.clone();
                    // Closing snapshots render their client content from the
                    // integer `live.rect`; keep the decoration frame on the
                    // same integer grid so the two cannot drift apart.
                    decoration.root_subpixel_offset = Default::default();
                    decoration
                },
                transform: invocation.transform.unwrap_or(decoration.visual_transform),
                promoted_at_ms: now_ms,
                finalize_deadline_ms,
                native_animation_completed: false,
            },
        );
        self.mark_runtime_dirty_windows(
            invocation.dirty_window_ids,
            invocation.dirty_managed_window_ids,
            &invocation.dirty_window_node_ids,
        );
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
            .map(|pointer| pointer.current_location().to_i32_floor());
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

    pub fn initial_managed_window_client_rect(
        &mut self,
        snapshot: &WaylandWindowSnapshot,
    ) -> Result<Option<LogicalRect>, DecorationEvaluationError> {
        self.sync_runtime_display_state();
        let now_ms = Duration::from(self.clock.now()).as_millis() as u64;
        // Initial configure needs the TS-managed rect before the window's first commit.
        // This uses a preconfigure runtime evaluation; the runtime keeps onOpen-created
        // window state but reanchors animations when the first real evaluation arrives.
        let mut evaluation = self
            .config_runtime
            .evaluate_window_preview(snapshot, now_ms)?;

        self.drain_runtime_host_messages();
        // Apply window actions queued during onOpen (e.g. window.focus(),
        // scheduleAnimation). Without this, anything onOpen pushes — most
        // notably `window.focus()` — gets dropped on the floor, since the
        // preconfigure path is the only one that surfaces those side effects
        // for newly-mapped windows. The caller (xdg_shell.rs new_toplevel)
        // has already mapped the window into `self.space`, so action lookup
        // (`space.elements().find(...)`) succeeds.
        if !evaluation.actions.is_empty() {
            let actions = std::mem::take(&mut evaluation.actions);
            for action in actions
                .iter()
                .filter(|action| matches!(action.action, WaylandWindowAction::Focus))
            {
                self.pending_initial_focus_window_ids
                    .insert(action.window_id.clone());
            }
            self.apply_runtime_window_actions(actions);
        }
        self.runtime_scheduler_enabled = evaluation.next_poll_in_ms.is_some();

        let managed = evaluation.managed_window;
        if !managed.managed {
            return Ok(None);
        }

        let Some(desired_root) = managed.rect else {
            return Ok(None);
        };
        let desired_root = managed_rect_snapshot_to_logical_rect(desired_root);
        if desired_root.width <= 0 || desired_root.height <= 0 {
            return Ok(None);
        }

        let tree = DecorationTree::new(evaluation.node);
        let layout_scale = self.decoration_layout_scale_for_rect(desired_root);
        managed_client_rect_for_root(&tree, desired_root, layout_scale).map(Some)
    }

    pub(crate) fn primary_output_name_for_window(&self, window: &Window) -> Option<String> {
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
            .or_else(|| {
                // A window pushed fully outside every output (e.g. a floating
                // window on a scrolled tiled workspace) must still belong to
                // some output's refresh pass: returning None makes every
                // refresh_window_decorations_for_output call skip it, freezing
                // its decoration layout at the last on-screen position while
                // the space location keeps moving — visible as a window stuck
                // at the screen edge. Attribute it to the nearest output.
                self.nearest_output_to_point(center)
                    .map(|output| output.name())
            })
    }

    /// The output whose geometry is closest to `point` (the output containing
    /// it when one does). Used to keep windows that sit entirely outside every
    /// output attributed to *some* output instead of to none.
    pub(crate) fn nearest_output_to_point(
        &self,
        point: Point<i32, Logical>,
    ) -> Option<smithay::output::Output> {
        self.space
            .outputs()
            .min_by_key(|output| {
                self.space
                    .output_geometry(output)
                    .map_or(i64::MAX, |geometry| {
                        let dx = i64::from(
                            (geometry.loc.x - point.x)
                                .max(point.x - (geometry.loc.x + geometry.size.w))
                                .max(0),
                        );
                        let dy = i64::from(
                            (geometry.loc.y - point.y)
                                .max(point.y - (geometry.loc.y + geometry.size.h))
                                .max(0),
                        );
                        dx * dx + dy * dy
                    })
            })
            .cloned()
    }

    pub fn refresh_window_decorations(&mut self) -> Result<(), DecorationEvaluationError> {
        self.refresh_window_decorations_for_output(None)
    }

    /// Split the action list returned by an in-band evaluation: schedule /
    /// cancel animation actions are applied **immediately** (so the upcoming
    /// `advance_managed_window_animations` already sees them), the rest are
    /// returned to be deferred to the standard end-of-refresh action sweep.
    /// This is the fix for the "open animation flashes static target for one
    /// frame" bug — without immediate application, scheduleAnimation actions
    /// would sit in `pending_window_actions` until after rendering the static
    /// frame.
    fn apply_pre_advance_animation_actions(
        &mut self,
        actions: Vec<crate::ssd::RuntimeWindowAction>,
    ) -> Vec<crate::ssd::RuntimeWindowAction> {
        let mut deferred = Vec::with_capacity(actions.len());
        for action in actions {
            if hot_reload_debug_enabled() || minimize_debug_enabled() {
                let cached_state = self.window_decorations.iter().find_map(|(_, decoration)| {
                    (decoration.snapshot.id == action.window_id).then_some({
                        (
                            decoration.managed_window.idle,
                            decoration.managed_window.visible,
                            decoration.managed_window.interactive,
                            decoration.managed_window_animation_active,
                            decoration.visual_transform.opacity,
                            decoration.static_managed_window.idle,
                            decoration.static_managed_window.visible,
                            decoration.static_visual_transform.opacity,
                        )
                    })
                });
                info!(
                    window_id = %action.window_id,
                    action = ?action.action,
                    channel = ?action.channel,
                    animation_channel = ?action.animation.as_ref().map(|animation| animation.channel.as_str()),
                    rect = ?action.animation.as_ref().and_then(|animation| animation.rect.as_ref()),
                    opacity = ?action.animation.as_ref().and_then(|animation| animation.opacity.as_ref()),
                    runtime_dirty = self.runtime_dirty_window_ids.contains(&action.window_id),
                    runtime_managed_only = self.runtime_managed_only_window_ids.contains(&action.window_id),
                    cached_state = ?cached_state,
                    "runtime action debug: pre-advance window action"
                );
            }
            match action.action {
                crate::ssd::WaylandWindowAction::ScheduleAnimation => {
                    if let Some(animation) = action.animation {
                        self.schedule_managed_window_animation(action.window_id, animation);
                    }
                }
                crate::ssd::WaylandWindowAction::CancelAnimation => {
                    self.cancel_managed_window_animation(
                        &action.window_id,
                        action.channel.as_deref(),
                    );
                }
                _ => deferred.push(action),
            }
        }
        deferred
    }

    pub fn schedule_managed_window_animation(
        &mut self,
        window_id: String,
        mut animation: ManagedWindowAnimationSnapshot,
    ) {
        self.managed_window_animation_sequence =
            self.managed_window_animation_sequence.wrapping_add(1);
        let channel = animation.channel.clone();
        let started_at_ms = Duration::from(self.clock.now()).as_secs_f64() * 1000.0;
        let had_any_existing = self
            .managed_window_animations
            .get(&window_id)
            .is_some_and(|channels| !channels.is_empty());
        if managed_animation_debug_enabled()
            || hot_reload_debug_enabled()
            || minimize_debug_enabled()
        {
            let had_existing = self
                .managed_window_animations
                .get(&window_id)
                .and_then(|channels| channels.get(&channel))
                .is_some();
            info!(
                window_id = %window_id,
                channel = %channel,
                started_at_ms,
                had_any_existing,
                had_existing,
                rect = ?animation.rect,
                offset = ?animation.offset,
                opacity = ?animation.opacity,
                runtime_dirty = self.runtime_dirty_window_ids.contains(&window_id),
                runtime_managed_only = self.runtime_managed_only_window_ids.contains(&window_id),
                "managed animation: schedule"
            );
        }

        // NOTE: previously we (a) reset `decoration.managed_window` and
        // `decoration.visual_transform` to their static values when the window
        // had no in-flight animations and (b) eagerly set
        // `managed_window_animation_active = true`. Together they opened a
        // race window: the reset moves the window back to its in-viewport
        // static rect with opacity = 1.0, and the eager flag bypasses the
        // `idle` filter in `managed_window_allows_render`. If a render fires
        // before the immediate advance+apply below has had a chance to push
        // the rect off-screen / drop opacity to 0, a now-hidden workspace's
        // window appears at full opacity in its static position for one
        // frame. Both ops are redundant — `advance_managed_window_animations`
        // already seeds from `static_managed_window`, applies the freshly
        // inserted animation's progress-0 sample, and sets the active flag
        // exactly when the animation begins driving the decoration. Skipping
        // the eager pair eliminates the flash without losing any cleanup.

        // Smooth handoff: when overriding an in-flight animation in the same
        // channel, TS-provided `from` values reflect the *declarative target*
        // (state.set() value), not the current visual position. If we used
        // them as-is the visual would snap from "mid-lerp" to "previous target"
        // before continuing toward the new target. Instead, sample the existing
        // animation at "now" and use those samples as `from`. This produces a
        // continuous lerp from where the user actually sees the window.
        if let Some(existing) = self
            .managed_window_animations
            .get(&window_id)
            .and_then(|channels| channels.get(&channel))
        {
            let (existing_progress, _) = managed_animation_progress(existing, started_at_ms);
            if let (Some(new_rect), Some(existing_rect)) =
                (animation.rect.as_mut(), existing.animation.rect.as_ref())
            {
                let sampled =
                    sample_rect_animation(existing_rect, existing_progress, existing_rect.from);
                new_rect.from = Some(sampled);
            }
            if let (Some(new_offset), Some(existing_offset)) = (
                animation.offset.as_mut(),
                existing.animation.offset.as_ref(),
            ) {
                let sampled = sample_point_animation(existing_offset, existing_progress);
                new_offset.from = Some(sampled);
            }
            if let (Some(new_opacity), Some(existing_opacity)) = (
                animation.opacity.as_mut(),
                existing.animation.opacity.as_ref(),
            ) {
                let sampled = sample_scalar_animation(
                    existing_opacity,
                    existing_progress,
                    existing_opacity.from.unwrap_or(existing_opacity.to),
                );
                new_opacity.from = Some(sampled);
            }
        }

        let inserted_window_id = window_id.clone();
        // Snapshot the pre-insert decoration state so the diagnostic below
        // can show exactly what would have been rendered if a render had
        // fired in the gap.
        let pre_decoration = self.window_decorations.iter().find_map(|(_, d)| {
            (d.snapshot.id == inserted_window_id).then_some({
                (
                    d.managed_window.rect,
                    d.managed_window.idle,
                    d.managed_window.visible,
                    d.managed_window_animation_active,
                    d.layout.root.rect,
                    d.visual_transform.opacity,
                    d.static_managed_window.idle,
                    d.static_managed_window.visible,
                    d.static_visual_transform.opacity,
                )
            })
        });
        self.managed_window_animations
            .entry(window_id)
            .or_default()
            .insert(
                channel.clone(),
                ActiveManagedWindowAnimation {
                    sequence: self.managed_window_animation_sequence,
                    started_at_ms,
                    animation,
                },
            );
        let mut dirty_rect_window_ids = self.advance_managed_window_animations(started_at_ms);
        dirty_rect_window_ids.insert(inserted_window_id.clone());
        self.apply_managed_window_rects(&dirty_rect_window_ids, true);
        if (managed_animation_debug_enabled()
            || hot_reload_debug_enabled()
            || minimize_debug_enabled())
            && let Some(post) = self.window_decorations.iter().find_map(|(_, d)| {
                (d.snapshot.id == inserted_window_id).then(|| {
                    (
                        d.managed_window.rect,
                        d.managed_window.idle,
                        d.managed_window.visible,
                        d.managed_window_animation_active,
                        d.layout.root.rect,
                        d.visual_transform.opacity,
                        d.static_managed_window.idle,
                        d.static_managed_window.visible,
                        d.static_visual_transform.opacity,
                        d.managed_window_allows_render(),
                    )
                })
            })
        {
            info!(
                window_id = %inserted_window_id,
                channel = %channel,
                pre = ?pre_decoration,
                post = ?post,
                runtime_dirty = self.runtime_dirty_window_ids.contains(&inserted_window_id),
                runtime_managed_only = self.runtime_managed_only_window_ids.contains(&inserted_window_id),
                "managed animation: schedule pre/post state"
            );
        }
        self.schedule_redraw();
        self.request_tty_maintenance("managed-window-animation-scheduled");
    }

    /// Drops a cancelled channel's last opacity/offset sample from a closing
    /// snapshot whose other channels keep running. Closing snapshots only take
    /// a new transform from channels that sample one (see
    /// `advance_managed_window_animations`), so without this the cancelled
    /// sample would stay. A surviving opacity/offset channel resamples on the
    /// next advance.
    fn reset_closing_transform_to_static(&mut self, window_id: &str) {
        let Some(closing) = self.closing_window_snapshots.get_mut(window_id) else {
            return;
        };
        let previous_root = transformed_root_rect(
            closing.decoration.layout.root.rect,
            closing.decoration.visual_transform,
        );
        let previous_transform = closing.decoration.visual_transform;
        closing.decoration.visual_transform = closing.decoration.static_visual_transform;
        closing.transform = closing.decoration.static_visual_transform;
        let next_root = transformed_root_rect(
            closing.decoration.layout.root.rect,
            closing.decoration.visual_transform,
        );
        if previous_transform != closing.decoration.visual_transform || previous_root != next_root {
            push_damage_pair(
                &mut self.pending_decoration_damage,
                Some(previous_root),
                next_root,
            );
        }
    }

    fn reset_managed_window_animation_state_to_static(&mut self, window_id: &str) {
        let mut reset_live = false;
        for (_, decoration) in self.window_decorations.iter_mut() {
            if decoration.snapshot.id != window_id {
                continue;
            }

            let previous_root =
                transformed_root_rect(decoration.layout.root.rect, decoration.visual_transform);
            let previous_transform = decoration.visual_transform;
            decoration.managed_window = decoration.static_managed_window.clone();
            decoration.visual_transform = decoration.static_visual_transform;
            let next_root =
                transformed_root_rect(decoration.layout.root.rect, decoration.visual_transform);
            if previous_transform != decoration.visual_transform || previous_root != next_root {
                push_damage_pair(
                    &mut self.pending_decoration_damage,
                    Some(previous_root),
                    next_root,
                );
            }
            reset_live = true;
            break;
        }

        let mut reset_closing = false;
        if let Some(closing) = self.closing_window_snapshots.get_mut(window_id) {
            let previous_root = transformed_root_rect(
                closing.decoration.layout.root.rect,
                closing.decoration.visual_transform,
            );
            let previous_transform = closing.decoration.visual_transform;
            closing.decoration.managed_window = closing.decoration.static_managed_window.clone();
            closing.decoration.visual_transform = closing.decoration.static_visual_transform;
            closing.transform = closing.decoration.static_visual_transform;
            // This path deliberately reverts to static state (hot reload /
            // explicit cancel), so drop the completed-animation freeze too.
            closing.native_animation_completed = false;
            let next_root = transformed_root_rect(
                closing.decoration.layout.root.rect,
                closing.decoration.visual_transform,
            );
            if previous_transform != closing.decoration.visual_transform
                || previous_root != next_root
            {
                push_damage_pair(
                    &mut self.pending_decoration_damage,
                    Some(previous_root),
                    next_root,
                );
            }
            reset_closing = true;
        }

        if hot_reload_debug_enabled() {
            info!(
                window_id,
                reset_live, reset_closing, "hot reload: reset managed animation state to static"
            );
        }
    }

    fn set_managed_window_animation_active(&mut self, window_id: &str, active: bool) {
        for (_, decoration) in self.window_decorations.iter_mut() {
            if decoration.snapshot.id == window_id {
                decoration.managed_window_animation_active = active;
                break;
            }
        }

        if let Some(closing) = self.closing_window_snapshots.get_mut(window_id) {
            closing.decoration.managed_window_animation_active = active;
        }
    }

    pub fn cancel_managed_window_animation(&mut self, window_id: &str, channel: Option<&str>) {
        let should_log_cancel = managed_animation_debug_enabled()
            || hot_reload_debug_enabled()
            || managed_rect_debug_enabled();
        if should_log_cancel {
            info!(
                window_id,
                channel = ?channel,
                before_channels = ?self
                    .managed_window_animations
                    .get(window_id)
                    .map(|channels| channels.keys().cloned().collect::<Vec<_>>()),
                "managed animation: cancel"
            );
        }
        let mut cancelled_transform_channel = false;
        if let Some(channel) = channel {
            if let Some(channels) = self.managed_window_animations.get_mut(window_id) {
                cancelled_transform_channel = channels.remove(channel).is_some_and(|active| {
                    active.animation.offset.is_some() || active.animation.opacity.is_some()
                });
                if channels.is_empty() {
                    self.managed_window_animations.remove(window_id);
                }
            }
        } else {
            self.managed_window_animations.remove(window_id);
        }
        let active = self
            .managed_window_animations
            .get(window_id)
            .is_some_and(|channels| !channels.is_empty());
        self.set_managed_window_animation_active(window_id, active);
        if !active {
            self.reset_managed_window_animation_state_to_static(window_id);
        } else if cancelled_transform_channel {
            self.reset_closing_transform_to_static(window_id);
        }
        if should_log_cancel {
            info!(
                window_id,
                channel = ?channel,
                active,
                after_channels = ?self
                    .managed_window_animations
                    .get(window_id)
                    .map(|channels| channels.keys().cloned().collect::<Vec<_>>()),
                reset_to_static = !active,
                "managed animation: cancel applied"
            );
        }
        self.schedule_redraw();
        self.request_tty_maintenance("managed-window-animation-cancelled");
    }

    fn advance_managed_window_animations(
        &mut self,
        now_ms: f64,
    ) -> std::collections::HashSet<String> {
        let mut dirty_rect_window_ids = std::collections::HashSet::new();
        if self.managed_window_animations.is_empty() {
            return dirty_rect_window_ids;
        }

        let window_ids = self
            .managed_window_animations
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        let mut active_any = false;

        for window_id in window_ids {
            let Some(channels) = self.managed_window_animations.get(&window_id) else {
                continue;
            };
            let channel_values = channels.values().cloned().collect::<Vec<_>>();
            if channel_values.is_empty() {
                continue;
            }

            let mut completed_channels = Vec::new();
            // Sort by mode priority first, then scheduling sequence. Override is
            // the "base layer" — it must run before Add / Sub / Multiply so the
            // additive modes apply their delta on top of the override's result
            // rather than the other way round. Within the same priority, the
            // scheduling order (newer last) decides who wins / stacks.
            let mut channel_values = channel_values;
            channel_values.sort_by_key(|animation| {
                let mode_priority = animation_mode_priority(&animation.animation);
                (mode_priority, animation.sequence)
            });

            // Always seed from the *static* composition state (not last frame's
            // animated result). Without this reset, additive/multiplicative
            // animations (mode = add / sub / multiply) would compound their
            // delta into the base every frame — the source of the workspace-
            // switch "runaway offset" bug. Override-mode animations don't care
            // because they replace the field outright, but resetting is cheap
            // and uniform.
            let Some(base_managed_window) = self
                .window_decorations
                .iter()
                .find_map(|(_, decoration)| {
                    (decoration.snapshot.id == window_id)
                        .then(|| decoration.static_managed_window.clone())
                })
                .or_else(|| {
                    self.closing_window_snapshots
                        .get(&window_id)
                        .map(|closing| closing.decoration.static_managed_window.clone())
                })
            else {
                self.managed_window_animations.remove(&window_id);
                continue;
            };

            let mut next_managed_window = base_managed_window.clone();
            let mut rect_changed = false;
            let mut transform_changed = false;

            for active in &channel_values {
                let (progress, running) = managed_animation_progress(active, now_ms);
                active_any |= running;
                if !running {
                    completed_channels.push(active.animation.channel.clone());
                }

                if let Some(rect_animation) = &active.animation.rect {
                    let value =
                        sample_rect_animation(rect_animation, progress, next_managed_window.rect);
                    apply_rect_animation_value(
                        &mut next_managed_window,
                        value,
                        rect_animation.mode,
                    );
                    rect_changed = true;
                }

                if let Some(offset_animation) = &active.animation.offset {
                    let value = sample_point_animation(offset_animation, progress);
                    apply_offset_animation_value(
                        &mut next_managed_window,
                        value,
                        offset_animation.mode,
                    );
                    transform_changed = true;
                }

                if let Some(opacity_animation) = &active.animation.opacity {
                    let value = sample_scalar_animation(
                        opacity_animation,
                        progress,
                        next_managed_window.transform.opacity as f64,
                    );
                    apply_opacity_animation_value(
                        &mut next_managed_window,
                        value,
                        opacity_animation.mode,
                    );
                    transform_changed = true;
                }
            }

            if let Some(channels) = self.managed_window_animations.get_mut(&window_id) {
                for channel in completed_channels {
                    channels.remove(&channel);
                }
                if channels.is_empty() {
                    self.managed_window_animations.remove(&window_id);
                }
            }
            let animation_still_active = self
                .managed_window_animations
                .get(&window_id)
                .is_some_and(|channels| !channels.is_empty());

            // Rect animations for closing snapshots do not have a live
            // `WindowDecorationState` entry anymore, so the live-window branch
            // below cannot mark them dirty. Mark the id here at the animation
            // level; `apply_managed_window_rects` has a closing-snapshot pass
            // that consumes the same dirty id set.
            if rect_changed {
                dirty_rect_window_ids.insert(window_id.clone());
            }

            for (_, decoration) in self.window_decorations.iter_mut() {
                if decoration.snapshot.id != window_id {
                    continue;
                }
                let previous_root =
                    transformed_root_rect(decoration.layout.root.rect, decoration.visual_transform);
                let previous_transform = decoration.visual_transform;
                decoration.managed_window = next_managed_window.clone();
                decoration.managed_window_animation_active = animation_still_active;
                // Publish the static-seeded transform even when only rect channels remain;
                // a cancelled opacity/offset channel must not leave its last sample behind.
                decoration.visual_transform = next_managed_window.transform;
                if rect_changed {
                    dirty_rect_window_ids.insert(window_id.clone());
                }
                let next_root =
                    transformed_root_rect(decoration.layout.root.rect, decoration.visual_transform);
                if previous_transform != decoration.visual_transform || previous_root != next_root {
                    push_damage_pair(
                        &mut self.pending_decoration_damage,
                        Some(previous_root),
                        next_root,
                    );
                }
                if managed_animation_debug_enabled() {
                    let active_channels = self
                        .managed_window_animations
                        .get(&window_id)
                        .map(|channels| {
                            channels
                                .values()
                                .map(|active| active.animation.channel.as_str())
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default();
                    info!(
                        window_id = %window_id,
                        now_ms,
                        active_channels = ?active_channels,
                        static_rect = ?decoration.static_managed_window.rect,
                        result_rect = ?decoration.managed_window.rect,
                        result_translate_x = decoration.visual_transform.translate_x,
                        result_translate_y = decoration.visual_transform.translate_y,
                        result_opacity = decoration.visual_transform.opacity,
                        rect_changed,
                        transform_changed,
                        "managed animation: advance frame result"
                    );
                }
                break;
            }

            if let Some(closing) = self.closing_window_snapshots.get_mut(&window_id) {
                let previous_root = transformed_root_rect(
                    closing.decoration.layout.root.rect,
                    closing.decoration.visual_transform,
                );
                let previous_transform = closing.decoration.visual_transform;
                closing.decoration.managed_window = next_managed_window.clone();
                closing.decoration.managed_window_animation_active = animation_still_active;
                // Unlike live windows, a closing snapshot keeps the last opacity/offset
                // sample once those channels finish: the static transform is the TS
                // pre-close state (opacity 1), and adopting it while a rect channel is
                // still running would flash the window back in. Cancelled channels are
                // reverted in `cancel_managed_window_animation` instead.
                if transform_changed {
                    closing.decoration.visual_transform = next_managed_window.transform;
                    closing.transform = next_managed_window.transform;
                }
                let next_root = transformed_root_rect(
                    closing.decoration.layout.root.rect,
                    closing.decoration.visual_transform,
                );
                if previous_transform != closing.decoration.visual_transform
                    || previous_root != next_root
                {
                    push_damage_pair(
                        &mut self.pending_decoration_damage,
                        Some(previous_root),
                        next_root,
                    );
                }

                // The `FinalizeClose` action (which drops this snapshot's
                // `GlesTexture` and frees its VRAM) is only ever enqueued
                // from the JS/TS-gated closing pass below, which only
                // re-evaluates a closing window when it is present in
                // `runtime_dirty_window_ids`. A close animation driven
                // purely by this native managed-window-animation system
                // (`WaylandWindowAction::ScheduleAnimation`, as used by
                // `scheduleCloseAnimation` in the default config) never
                // itself touches `runtime_dirty_window_ids` once the JS
                // side's one-shot `start_close` dirty flag is consumed on
                // the first frame — so without this, a closing window whose
                // fade is entirely native-driven never gets re-evaluated,
                // `FinalizeClose` never fires, and its texture leaks for
                // the lifetime of the compositor. Re-mark it dirty the
                // moment its native animation finishes so the existing
                // finalize check actually runs again on the next frame.
                if !animation_still_active {
                    // The animation's final state (opacity 0 / end offset) is
                    // now the authoritative end state of this close. Freeze it
                    // so the re-evaluation triggered below cannot overwrite
                    // `closing.transform` with the TS-side *static* transform
                    // (opacity 1) — doing so flashed the closing window back
                    // to full opacity for the frame(s) until `FinalizeClose`.
                    closing.native_animation_completed = true;
                    if managed_animation_debug_enabled() {
                        info!(
                            window_id = %window_id,
                            final_opacity = closing.transform.opacity,
                            "managed animation: closing snapshot animation completed; freezing animated state"
                        );
                    }
                    self.runtime_dirty_window_ids.insert(window_id.clone());
                }
            }
        }

        if active_any {
            self.schedule_redraw();
            self.request_tty_maintenance("managed-window-animation-active");
        }

        dirty_rect_window_ids
    }

    pub fn refresh_layer_effects_for_output(
        &mut self,
        output_name: &str,
    ) -> Result<(), DecorationEvaluationError> {
        timescope::scope!("ssd refresh_layer_effects_for_output");
        let refresh_started_at = Instant::now();
        let snapshot_started_at = Instant::now();
        let snapshots = {
            timescope::scope!("ssd layer snapshot");
            self.snapshot_layers()
        };
        let snapshot_elapsed_ms = snapshot_started_at.elapsed().as_secs_f64() * 1000.0;
        let output_layer_ids = snapshots
            .iter()
            .filter(|snapshot| snapshot.output_name == output_name)
            .map(|snapshot| snapshot.id.clone())
            .collect::<std::collections::HashSet<_>>();
        let live_layer_ids = snapshots
            .iter()
            .map(|snapshot| snapshot.id.clone())
            .collect::<std::collections::HashSet<_>>();
        let now_ms = Duration::from(self.clock.now()).as_millis() as u64;
        let signature = layer_effect_evaluation_signature(output_name, &snapshots);
        let force_evaluate = self
            .layer_effect_evaluation_cache
            .get(output_name)
            .is_none_or(|cache| cache.signature != signature || cache.animating);
        if !force_evaluate {
            retain_effect_assignments_for_live_ids(
                &mut self.configured_layer_effects,
                &live_layer_ids,
            );
            retain_effect_texture_cache_for_live_ids(&mut self.layer_effect_cache, &live_layer_ids);
            retain_effect_texture_cache_for_live_ids(
                &mut self.layer_framebuffer_effect_states,
                &live_layer_ids,
            );
            crate::backend::shader_effect::retain_backdrop_cache_for_live_layers(
                &mut self.layer_backdrop_cache,
                &live_layer_ids,
            );
            return Ok(());
        }
        let sync_started_at = Instant::now();
        self.sync_runtime_display_state();
        let sync_elapsed_ms = sync_started_at.elapsed().as_secs_f64() * 1000.0;
        let evaluate_started_at = Instant::now();
        let evaluation = {
            timescope::scope!("ssd layer effect evaluate");
            self.config_runtime
                .evaluate_layer_effects(output_name, &snapshots, now_ms)?
        };
        let evaluate_elapsed_ms = evaluate_started_at.elapsed().as_secs_f64() * 1000.0;
        let apply_started_at = Instant::now();
        self.drain_runtime_host_messages();

        self.runtime_scheduler_enabled = evaluation.next_poll_in_ms.is_some();
        if evaluation.next_poll_in_ms == Some(0) {
            self.runtime_animation_outputs
                .insert(output_name.to_string());
        } else {
            self.runtime_animation_outputs.remove(output_name);
        }
        let output_layer_count = output_layer_ids.len();
        for layer_id in &output_layer_ids {
            self.configured_layer_effects.remove(layer_id);
        }
        let effect_count = evaluation.effects.len();
        let next_poll_in_ms = evaluation.next_poll_in_ms;
        self.layer_effect_evaluation_cache.insert(
            output_name.to_string(),
            EffectEvaluationCacheEntry {
                signature,
                animating: next_poll_in_ms == Some(0),
            },
        );
        for assignment in evaluation.effects {
            if let Some(effects) = assignment.effects {
                self.configured_layer_effects
                    .insert(assignment.layer_id, effects);
            }
        }
        retain_effect_assignments_for_live_ids(&mut self.configured_layer_effects, &live_layer_ids);
        let live_layer_prefixes = snapshots
            .iter()
            .map(|snapshot| format!("{}@", snapshot.id))
            .collect::<Vec<_>>();
        self.layer_effect_cache.retain(|key, _| {
            live_layer_prefixes
                .iter()
                .any(|prefix| key.starts_with(prefix))
        });
        retain_effect_texture_cache_for_live_ids(
            &mut self.layer_framebuffer_effect_states,
            &live_layer_ids,
        );
        crate::backend::shader_effect::retain_backdrop_cache_for_live_layers(
            &mut self.layer_backdrop_cache,
            &live_layer_ids,
        );
        let apply_elapsed_ms = apply_started_at.elapsed().as_secs_f64() * 1000.0;
        let elapsed_ms = refresh_started_at.elapsed().as_secs_f64() * 1000.0;

        if animation_timing_debug_enabled()
            && (elapsed_ms >= animation_spike_threshold_ms()
                || evaluate_elapsed_ms >= animation_spike_threshold_ms())
        {
            warn!(
                output_name,
                layer_snapshot_count = snapshots.len(),
                output_layer_count,
                effect_count,
                snapshot_elapsed_ms,
                sync_elapsed_ms,
                evaluate_elapsed_ms,
                apply_elapsed_ms,
                elapsed_ms,
                next_poll_in_ms,
                "animation timing: layer effects spike"
            );
        }

        Ok(())
    }

    /// Re-evaluate `COMPOSITOR.effect.popup` assignments for all popups on
    /// the given output. Mirrors `refresh_layer_effects_for_output`: called
    /// once per rendered frame, with the runtime returning the full effect set
    /// for the currently mapped popups.
    pub fn refresh_popup_effects_for_output(
        &mut self,
        output_name: &str,
    ) -> Result<(), DecorationEvaluationError> {
        let snapshots = self.snapshot_popups();
        let output_popup_ids = snapshots
            .iter()
            .filter(|snapshot| snapshot.output_name == output_name)
            .map(|snapshot| snapshot.id.clone())
            .collect::<std::collections::HashSet<_>>();
        let live_popup_ids = snapshots
            .iter()
            .map(|snapshot| snapshot.id.clone())
            .collect::<std::collections::HashSet<_>>();
        let now_ms = Duration::from(self.clock.now()).as_millis() as u64;
        let signature = popup_effect_evaluation_signature(output_name, &snapshots);
        let force_evaluate = self
            .popup_effect_evaluation_cache
            .get(output_name)
            .is_none_or(|cache| cache.signature != signature || cache.animating);
        if !force_evaluate {
            retain_effect_assignments_for_live_ids(
                &mut self.configured_popup_effects,
                &live_popup_ids,
            );
            retain_effect_texture_cache_for_live_ids(&mut self.popup_effect_cache, &live_popup_ids);
            crate::backend::shader_effect::retain_shared_effect_pipeline_caches_for_live_popups(
                &live_popup_ids,
            );
            return Ok(());
        }
        self.sync_runtime_display_state();
        let evaluation =
            self.config_runtime
                .evaluate_popup_effects(output_name, &snapshots, now_ms)?;
        self.drain_runtime_host_messages();

        for popup_id in &output_popup_ids {
            self.configured_popup_effects.remove(popup_id);
            self.configured_popup_surface_policies.remove(popup_id);
        }
        for assignment in evaluation.effects {
            // Surface policies are independent of effect assignments: a popup
            // can have `opaqueRegion: "ignore"` with no effect configured.
            if let Some(policy) = assignment.surface_policy {
                self.configured_popup_surface_policies
                    .insert(assignment.popup_id.clone(), policy);
            }
            if let Some(effects) = assignment.effects {
                self.configured_popup_effects
                    .insert(assignment.popup_id, effects);
            }
        }
        retain_effect_assignments_for_live_ids(&mut self.configured_popup_effects, &live_popup_ids);
        self.configured_popup_surface_policies
            .retain(|id, _| live_popup_ids.contains(id));
        self.popup_effect_evaluation_cache.insert(
            output_name.to_string(),
            EffectEvaluationCacheEntry {
                signature,
                animating: evaluation.next_poll_in_ms == Some(0),
            },
        );
        // Drop element-state cache entries for popups that no longer exist.
        let live_popup_prefixes = snapshots
            .iter()
            .map(|snapshot| format!("{}@", snapshot.id))
            .collect::<Vec<_>>();
        self.popup_effect_cache.retain(|key, _| {
            live_popup_prefixes
                .iter()
                .any(|prefix| key.starts_with(prefix))
        });
        self.popup_framebuffer_effect_states.retain(|key, _| {
            live_popup_prefixes
                .iter()
                .any(|prefix| key.starts_with(prefix))
        });
        crate::backend::shader_effect::retain_shared_effect_pipeline_caches_for_live_popups(
            &live_popup_ids,
        );

        Ok(())
    }

    /// The runtime-side end of a window that closed WITHOUT a closing animation: tell the
    /// config it is gone and drop every per-id set that would otherwise keep electing,
    /// deferring or dirtying a window that no longer exists.
    ///
    /// Two callers. The decoration sweep reaches it for a window it still holds a decoration
    /// for but can no longer find in the space. The close handlers reach it directly, and
    /// have to: they prune that decoration entry themselves (to stop the VRAM leak that
    /// e4befcf fixed), which also hides the window from the sweep — so a toplevel destroyed
    /// before it ever painted, or any X11 window, used to vanish from the compositor while
    /// the config went on believing it existed, focused. The maximise button then acted on a
    /// ghost.
    pub fn close_window_in_runtime(
        &mut self,
        window_id: &str,
        damage: Option<LogicalRect>,
    ) -> Result<(), DecorationEvaluationError> {
        // Local bookkeeping first: it must go whatever the runtime answers, or a failed
        // request leaves the id in these sets for the process lifetime.
        self.windows_ready_for_decoration.remove(window_id);
        // Kept in step with `windows_ready_for_decoration`. A window destroyed before its
        // first paint otherwise leaves its id here for the process lifetime, which
        // permanently defeats the `is_empty()` fast path in
        // `should_defer_initial_keyboard_focus`.
        self.pending_initial_focus_window_ids.remove(window_id);
        self.runtime_dirty_window_ids.remove(window_id);
        self.runtime_managed_only_window_ids.remove(window_id);
        self.snapshot_dirty_window_ids.remove(window_id);
        self.live_window_snapshots.remove(window_id);
        self.live_window_snapshot_trackers.remove(window_id);
        if let Some(rect) = damage {
            self.pending_decoration_damage.push(rect);
        }
        self.config_runtime.window_closed(window_id)
    }

    pub fn refresh_window_decorations_for_output(
        &mut self,
        target_output_name: Option<&str>,
    ) -> Result<(), DecorationEvaluationError> {
        timescope::scope!("ssd refresh_window_decorations_for_output");
        let refresh_started_at = Instant::now();
        let spike_threshold_ms = animation_spike_threshold_ms();
        let force_runtime_reevaluate =
            self.runtime_poll_dirty && self.runtime_dirty_window_ids.is_empty();
        let force_output_animation_reevaluate = target_output_name
            .is_some_and(|output_name| self.runtime_animation_outputs.contains(output_name));
        let force_async_asset_refresh = self.async_asset_dirty;
        let mut pending_window_actions = Vec::new();
        // Window actions returned in-band from evaluations that need to take
        // effect *before* `advance_managed_window_animations` runs this frame
        // (scheduleAnimation / cancelAnimation). Collected during the windows
        // pass because most evaluation sites hold a `&mut self.window_decorations`
        // borrow which prevents calling `self.apply_pre_advance_animation_actions`
        // inline. We drain this list after the windows pass, before `advance`.
        let mut pre_advance_actions: Vec<crate::ssd::RuntimeWindowAction> = Vec::new();
        let mut pending_finalize_close_damage = Vec::new();
        {
            timescope::scope!("ssd sync runtime display state");
            self.sync_runtime_display_state();
        }
        super::set_popup_viewports(
            self.space
                .outputs()
                .filter_map(|output| self.space.output_geometry(output))
                .map(|geometry| {
                    LogicalRect::new(
                        geometry.loc.x,
                        geometry.loc.y,
                        geometry.size.w,
                        geometry.size.h,
                    )
                })
                .collect(),
        );
        let windows: Vec<Window> = {
            timescope::scope!("ssd collect windows");
            self.space.elements().cloned().collect()
        };
        let live_window_ids = {
            timescope::scope!("ssd collect live window ids");
            windows
                .iter()
                .map(|window| self.snapshot_window(window).id)
                .collect::<std::collections::HashSet<_>>()
        };
        let window_count = windows.len();
        let mut rebuilt = 0usize;
        let mut relayout = 0usize;
        let mut runtime_dirty_updates = 0usize;
        let mut promoted_closing = 0usize;
        let mut closing_runtime_updates = 0usize;
        let mut animation_active_for_target = false;
        let mut processed_runtime_dirty_window_ids = std::collections::HashSet::new();
        self.runtime_dirty_marked_during_refresh = Some(Default::default());
        let mut managed_rect_apply_window_ids = std::collections::HashSet::new();
        let now_ms = Duration::from(self.clock.now()).as_millis() as u64;
        let closing_active_count = self.closing_window_snapshots.len();

        // Watchdog: `closing_window_snapshots` entries are normally freed via
        // `WaylandWindowAction::FinalizeClose`, driven by the TS runtime's
        // close handshake (`closePoll` armed in `startClose`). That chain
        // breaks when the isolate is replaced mid-animation or the config
        // throws during the close; the entry — and its `GlesTexture` (real
        // VRAM) — would then sit in the map for the lifetime of the
        // compositor. Each entry carries a per-window `finalize_deadline_ms`
        // derived from the close-animation duration the config itself
        // declared (see `promote_window_to_closing_snapshot`), so anything
        // past its deadline is stuck, not still animating; force it closed
        // rather than let VRAM grow with every window ever closed. A firing
        // watchdog always indicates a bug in the finalize chain — hence warn.
        self.finalize_closing_snapshots_past_deadline(now_ms);

        let removed_windows_started_at = Instant::now();
        let removed_windows = {
            timescope::scope!("ssd collect removed windows");
            self.window_decorations
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
                .collect::<Vec<_>>()
        };
        {
            timescope::scope!("ssd removed windows pass");
            for (window_id, root_rect, _previous_transform, decoration) in &removed_windows {
                if self.closing_window_snapshots.contains_key(window_id) {
                    continue;
                }

                if !self.promote_window_to_closing_snapshot(window_id, decoration, now_ms)? {
                    self.close_window_in_runtime(window_id, Some(*root_rect))?;
                } else {
                    promoted_closing = promoted_closing.saturating_add(1);
                }
            }
        }
        {
            timescope::scope!("ssd retain live windows");
            self.window_decorations
                .retain(|window, _| windows.contains(window));
            self.window_primary_output_names
                .retain(|window, _| windows.contains(window));
        }
        let removed_windows_elapsed_ms =
            removed_windows_started_at.elapsed().as_secs_f64() * 1000.0;

        let windows_pass_started_at = Instant::now();
        {
            timescope::scope!("ssd windows pass");
            for window in windows {
                timescope::scope!("ssd window refresh");
                let primary_output_name = {
                    timescope::scope!("ssd window primary output");
                    self.primary_output_name_for_window(&window)
                };
                let snapshot = {
                    timescope::scope!("ssd window snapshot");
                    self.snapshot_window(&window)
                };
                let snapshot_id = snapshot.id.clone();
                let window_was_runtime_dirty = {
                    timescope::scope!("ssd window dirty lookup");
                    self.runtime_dirty_window_ids.contains(&snapshot_id)
                };
                let minimize_debug = minimize_debug_enabled();
                if (runtime_dirty_debug_enabled() || minimize_debug) && window_was_runtime_dirty {
                    let cached_snapshot = self
                        .window_decorations
                        .get(&window)
                        .map(|cached| &cached.snapshot);
                    let cached_state = self.window_decorations.get(&window).map(|cached| {
                        (
                            cached.managed_window.idle,
                            cached.managed_window.visible,
                            cached.managed_window.interactive,
                            cached.managed_window_animation_active,
                            cached.visual_transform.opacity,
                            cached.static_managed_window.idle,
                            cached.static_managed_window.visible,
                            cached.static_visual_transform.opacity,
                        )
                    });
                    info!(
                        window_id = %snapshot_id,
                        title = %snapshot.title,
                        cached_title = ?cached_snapshot.map(|snapshot| snapshot.title.as_str()),
                        app_id = ?snapshot.app_id,
                        cached_app_id = ?cached_snapshot.and_then(|snapshot| snapshot.app_id.as_deref()),
                        runtime_managed_only = self.runtime_managed_only_window_ids.contains(&snapshot_id),
                        target_output = ?target_output_name,
                        cached_state = ?cached_state,
                        "runtime dirty debug: refresh candidate"
                    );
                }
                let should_process = {
                    timescope::scope!("ssd window process filter");
                    should_process_window_for_refresh(
                        primary_output_name.as_deref(),
                        target_output_name,
                        force_async_asset_refresh,
                        force_output_animation_reevaluate,
                        force_runtime_reevaluate,
                        window_was_runtime_dirty,
                    )
                };
                if !should_process {
                    if (runtime_dirty_debug_enabled() || minimize_debug) && window_was_runtime_dirty
                    {
                        info!(
                            window_id = %snapshot_id,
                            title = %snapshot.title,
                            primary_output = ?primary_output_name,
                            target_output = ?target_output_name,
                            "runtime dirty debug: refresh candidate skipped"
                        );
                    }
                    continue;
                }
                if let Some(primary_output_name) = primary_output_name {
                    self.window_primary_output_names
                        .insert(window.clone(), primary_output_name);
                }
                let (client_rect, client_rect_source) = {
                    timescope::scope!("ssd window client rect");
                    match self.window_client_rect(&window) {
                        Some(rect) => (rect, "live"),
                        None => {
                            let cached_client_rect = self
                                .window_decorations
                                .get(&window)
                                .map(|cached| cached.client_rect);
                            if window_was_runtime_dirty
                                || force_runtime_reevaluate
                                || force_output_animation_reevaluate
                            {
                                if let Some(rect) = cached_client_rect {
                                    if runtime_dirty_debug_enabled() || minimize_debug {
                                        info!(
                                            window_id = %snapshot_id,
                                            title = %snapshot.title,
                                            cached_client_rect = ?rect,
                                            window_was_runtime_dirty,
                                            force_runtime_reevaluate,
                                            force_output_animation_reevaluate,
                                            "runtime dirty debug: using cached client rect"
                                        );
                                    }
                                    (rect, "cached")
                                } else {
                                    if runtime_dirty_debug_enabled() || minimize_debug {
                                        info!(
                                            window_id = %snapshot_id,
                                            title = %snapshot.title,
                                            window_was_runtime_dirty,
                                            force_runtime_reevaluate,
                                            force_output_animation_reevaluate,
                                            "runtime dirty debug: skipped missing live and cached client rect"
                                        );
                                    }
                                    continue;
                                }
                            } else {
                                if runtime_dirty_debug_enabled() || minimize_debug {
                                    info!(
                                        window_id = %snapshot_id,
                                        title = %snapshot.title,
                                        window_was_runtime_dirty,
                                        force_runtime_reevaluate,
                                        force_output_animation_reevaluate,
                                        "runtime dirty debug: skipped missing live client rect"
                                    );
                                }
                                continue;
                            }
                        }
                    }
                };
                let layout_scale = {
                    timescope::scope!("ssd window layout scale");
                    self.decoration_layout_scale_for_window(&window)
                };
                let window_raster_scale = {
                    timescope::scope!("ssd window raster scale");
                    self.decoration_raster_scale_for_window(&window)
                };
                let cached_effective_client_rect = {
                    timescope::scope!("ssd window effective client rect");
                    self.window_decorations
                        .get(&window)
                        .map(|cached| {
                            // Fast path: when the cache is coherent
                            // (`client_rect_potentially_stale == false`),
                            // `managed_client_rect_for_state(cached.tree,
                            // cached.managed_window, _, cached.layout_scale)` is
                            // *by construction* equal to `cached.client_rect` — the
                            // function's result for managed windows depends only on
                            // `(tree, managed_window.rect, scale)`, all of which are
                            // exactly the inputs the cached rect was derived from,
                            // and for unmanaged windows the function returns the
                            // fallback (which `snapshot_changed` would have caught
                            // separately). Skipping it avoids the up-to-4-iteration
                            // probe-layout loop per window per redraw, which was
                            // the dominant CPU cost during heavy client commits
                            // (ufo-test at 4K@120Hz: ~25% of CPU in the SSD layout
                            // path).
                            // A rect-driven window's result does not read the
                            // fallback either, so it holds after a move too.
                            if !cached.client_rect_potentially_stale
                                && (cached.snapshot.position == snapshot.position
                                    || managed_client_rect_ignores_fallback(&cached.managed_window))
                            {
                                return Ok(cached.client_rect);
                            }
                            if let Some(rect) = managed_client_rect_for_move(cached) {
                                return Ok(rect);
                            }
                            managed_client_rect_for_state(
                                &cached.tree,
                                &cached.managed_window,
                                client_rect,
                                cached.layout_scale,
                            )
                        })
                        .transpose()?
                        .unwrap_or(client_rect)
                };
                // The probe above ran against the cached tree, managed rect and
                // scale. Once it agrees with the cached rect and no Rust-side
                // animation is moving the window, the cache is coherent again;
                // otherwise a window whose animation ended without a relayout
                // (an opacity fade, a rect animation landing on the cached
                // rect) would run the probe loop on every redraw for good.
                if let Some(cached) = self.window_decorations.get_mut(&window) {
                    if client_rect_probe_restores_coherence(
                        cached,
                        layout_scale,
                        cached_effective_client_rect,
                    ) {
                        cached.client_rect_potentially_stale = false;
                    }
                }
                let had_cached_decoration = {
                    timescope::scope!("ssd window cache lookup");
                    self.window_decorations.contains_key(&window)
                };
                let (runtime_state_changed, snapshot_changed) = {
                    timescope::scope!("ssd window snapshot diff");
                    let runtime_state_changed = self
                        .window_decorations
                        .get(&window)
                        .map(|cached| {
                            window_snapshot_requires_runtime_refresh(&cached.snapshot, &snapshot)
                        })
                        .unwrap_or(false);
                    let snapshot_changed = self
                        .window_decorations
                        .get(&window)
                        .map(|cached| window_snapshot_requires_rebuild(&cached.snapshot, &snapshot))
                        .unwrap_or(true);
                    (runtime_state_changed, snapshot_changed)
                };

                // Not `force_output_animation_reevaluate`: a runtime animation steps
                // through signals, so the scheduler tick reports the windows it moves.
                // Forcing every window of the output too re-evaluated all of them on
                // each frame of any one window's animation (a focus fade), and kept
                // moving windows off the translate fast path below.
                let runtime_dirty =
                    force_runtime_reevaluate || runtime_state_changed || window_was_runtime_dirty;
                let force_full_cached_reevaluation = force_runtime_reevaluate
                    || (window_was_runtime_dirty
                        && !self.runtime_managed_only_window_ids.contains(&snapshot_id)
                        && !self.runtime_node_only_window_ids.contains(&snapshot_id));
                if (runtime_dirty_debug_enabled() || minimize_debug)
                    && (window_was_runtime_dirty || runtime_dirty)
                {
                    info!(
                        window_id = %snapshot_id,
                        title = %snapshot.title,
                        client_rect_source,
                        had_cached_decoration,
                        snapshot_changed,
                        runtime_dirty,
                        runtime_state_changed,
                        window_was_runtime_dirty,
                        cached_effective_client_rect = ?cached_effective_client_rect,
                        "runtime dirty debug: refresh branch decision"
                    );
                }
                if !had_cached_decoration || snapshot_changed {
                    let started_at = Instant::now();
                    let previous_root = self.window_decorations.get(&window).map(|cached| {
                        transformed_root_rect(cached.layout.root.rect, cached.visual_transform)
                    });
                    let evaluate_started_at = Instant::now();
                    let mut evaluation = {
                        timescope::scope!("ssd window evaluate");
                        match self.config_runtime.evaluate_window(&snapshot, now_ms) {
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
                        }
                    };
                    let evaluate_ms = evaluate_started_at.elapsed().as_secs_f64() * 1000.0;
                    pre_advance_actions.extend(std::mem::take(&mut evaluation.actions));
                    let tree = DecorationTree::new(evaluation.node);
                    let previous_animation_state =
                        self.window_decorations.get(&window).and_then(|cached| {
                            cached.managed_window_animation_active.then(|| {
                                (
                                    cached.managed_window.clone(),
                                    cached.visual_transform,
                                    cached.last_configured_client_size,
                                )
                            })
                        });
                    let layout_managed_window = previous_animation_state
                        .as_ref()
                        .map(|(managed_window, _, _)| managed_window)
                        .unwrap_or(&evaluation.managed_window);
                    let layout_client_rect = managed_client_rect_for_state(
                        &tree,
                        layout_managed_window,
                        client_rect,
                        layout_scale,
                    )?;
                    let layout_started_at = Instant::now();
                    let layout_subpixel = self
                        .window_decorations
                        .get(&window)
                        .map(|cached| cached.root_subpixel_offset)
                        .unwrap_or_default();
                    let layout = {
                        timescope::scope!("ssd window layout");
                        tree.layout_for_client_with_subpixel(
                            layout_client_rect,
                            layout_scale,
                            layout_subpixel,
                        )
                        .map_err(super::DecorationEvaluationError::Layout)?
                    };
                    let layout_ms = layout_started_at.elapsed().as_secs_f64() * 1000.0;
                    push_damage_pair(
                        &mut self.pending_decoration_damage,
                        previous_root,
                        transformed_root_rect(layout.root.rect, evaluation.transform),
                    );
                    let previous_text_buffers = self
                        .window_decorations
                        .get(&window)
                        .map(|cached| cached.text_buffers.clone())
                        .unwrap_or_default();
                    let clip_started_at = Instant::now();
                    let content_clip = {
                        timescope::scope!("ssd window clip");
                        let node_geometry = build_node_geometry_map(&layout);
                        content_clip_for_layout(&tree, &layout, &node_geometry)
                    };
                    let clip_ms = clip_started_at.elapsed().as_secs_f64() * 1000.0;
                    let order_started_at = Instant::now();
                    let order_map = {
                        timescope::scope!("ssd window render order");
                        build_render_order_map(&layout)
                    };
                    let order_ms = order_started_at.elapsed().as_secs_f64() * 1000.0;
                    let buffers_started_at = Instant::now();
                    let buffers = {
                        timescope::scope!("ssd window cached buffers");
                        build_cached_buffers(&layout, &order_map)
                    };
                    let buffers_ms = buffers_started_at.elapsed().as_secs_f64() * 1000.0;
                    let shader_started_at = Instant::now();
                    let mut shader_buffers = {
                        timescope::scope!("ssd window shader buffers");
                        build_shader_buffers(&layout, &order_map)
                    };
                    let shader_ms = shader_started_at.elapsed().as_secs_f64() * 1000.0;
                    let text_started_at = Instant::now();
                    let text_buffers = {
                        timescope::scope!("ssd window text buffers");
                        build_text_buffers_with_fallback(
                            &layout,
                            &order_map,
                            window_raster_scale,
                            &mut self.text_rasterizer,
                            &previous_text_buffers,
                        )
                    };
                    let text_ms = text_started_at.elapsed().as_secs_f64() * 1000.0;
                    let icon_started_at = Instant::now();
                    let icon_buffers = {
                        timescope::scope!("ssd window icon buffers");
                        build_icon_buffers(
                            &layout,
                            &order_map,
                            window_raster_scale,
                            &snapshot,
                            &mut self.icon_rasterizer,
                        )
                    };
                    let icon_ms = icon_started_at.elapsed().as_secs_f64() * 1000.0;
                    let finalize_started_at = Instant::now();
                    {
                        timescope::scope!("ssd window finalize rebuild");
                        if let Some(previous) = self.window_decorations.get(&window) {
                            freeze_manual_shader_buffers(
                                &previous.shader_buffers,
                                &mut shader_buffers,
                            );
                        }
                        self.suggested_window_offset = suggested_window_offset(&layout);
                    }
                    let finalize_ms = finalize_started_at.elapsed().as_secs_f64() * 1000.0;
                    rebuilt += 1;
                    record_managed_rect_path_event(ManagedRectPathEvent::FullRebuild);
                    if evaluation.managed_window.managed && evaluation.managed_window.rect.is_some()
                    {
                        managed_rect_apply_window_ids.insert(snapshot.id.clone());
                    }
                    let elapsed_ms = started_at.elapsed().as_secs_f64() * 1000.0;
                    debug!(
                        window_id = snapshot.id,
                        title = snapshot.title,
                        text_buffer_count = text_buffers.len(),
                        elapsed_ms,
                        "rebuilt window decoration tree"
                    );
                    log_animation_window_refresh_timing(
                        "rebuild",
                        &snapshot,
                        elapsed_ms,
                        evaluate_ms,
                        layout_ms,
                        clip_ms,
                        order_ms,
                        buffers_ms,
                        shader_ms,
                        text_ms,
                        icon_ms,
                        finalize_ms,
                        0,
                        None,
                        None,
                    );
                    log_decoration_refresh(
                        "rebuild",
                        &snapshot,
                        layout_client_rect,
                        &layout,
                        &buffers,
                    );
                    let caches = self
                        .window_decorations
                        .remove(&window)
                        .map(|cached| {
                            (
                                cached.paint_cache,
                                cached.shader_cache,
                                cached.backdrop_cache,
                                cached.window_effect_cache,
                            )
                        })
                        .unwrap_or_default();
                    let (paint_cache, shader_cache, backdrop_cache, window_effect_cache) = caches;
                    let static_transform = evaluation.transform;
                    let static_managed = evaluation.managed_window.clone();
                    let (
                        visual_transform,
                        managed_window,
                        managed_window_animation_active,
                        last_configured_client_size,
                    ) = previous_animation_state
                        .map(
                            |(managed_window, visual_transform, last_configured_client_size)| {
                                (
                                    visual_transform,
                                    managed_window,
                                    true,
                                    last_configured_client_size,
                                )
                            },
                        )
                        .unwrap_or((static_transform, evaluation.managed_window, false, None));
                    self.window_decorations.insert(
                        window,
                        WindowDecorationState {
                            snapshot,
                            tree,
                            layout,
                            layout_scale,
                            client_rect: layout_client_rect,
                            client_rect_potentially_stale: false,
                            root_subpixel_offset: managed_rect_subpixel_offset(&managed_window),
                            visual_transform,
                            managed_window,
                            managed_window_animation_active,
                            last_configured_client_size,
                            static_visual_transform: static_transform,
                            static_managed_window: static_managed,
                            window_effects: evaluation.window_effects,
                            content_clip,
                            buffers,
                            shader_buffers,
                            text_buffers,
                            icon_buffers,
                            paint_cache,
                            shader_cache,
                            backdrop_cache,
                            window_effect_cache,
                        },
                    );
                    self.schedule_redraw();
                    self.runtime_scheduler_enabled = evaluation.next_poll_in_ms.is_some();
                    animation_active_for_target |= evaluation.next_poll_in_ms == Some(0);
                } else if let Some(cached) = self.window_decorations.get_mut(&window) {
                    if cached.client_rect != cached_effective_client_rect
                        && !runtime_dirty
                        && !force_async_asset_refresh
                        && cached.client_rect.width == cached_effective_client_rect.width
                        && cached.client_rect.height == cached_effective_client_rect.height
                    {
                        let previous_root =
                            transformed_root_rect(cached.layout.root.rect, cached.visual_transform);
                        let dx = cached_effective_client_rect.x - cached.client_rect.x;
                        let dy = cached_effective_client_rect.y - cached.client_rect.y;
                        translate_cached_decoration_position(
                            cached,
                            dx,
                            dy,
                            cached_effective_client_rect,
                        );
                        cached.snapshot = snapshot;
                        let next_root =
                            transformed_root_rect(cached.layout.root.rect, cached.visual_transform);
                        push_damage_pair(
                            &mut self.pending_decoration_damage,
                            Some(previous_root),
                            next_root,
                        );
                        self.schedule_redraw();
                        record_managed_rect_path_event(
                            ManagedRectPathEvent::RefreshPositionTranslate,
                        );
                    } else if cached.client_rect != cached_effective_client_rect
                        && !runtime_dirty
                        && !force_async_asset_refresh
                    {
                        let started_at = Instant::now();
                        let finalize_ms = 0.0;
                        let previous_root =
                            transformed_root_rect(cached.layout.root.rect, cached.visual_transform);
                        let layout_started_at = Instant::now();
                        cached.layout = {
                            timescope::scope!("ssd window layout");
                            cached
                                .tree
                                .layout_for_client_with_subpixel(
                                    cached_effective_client_rect,
                                    layout_scale,
                                    cached.root_subpixel_offset,
                                )
                                .map_err(super::DecorationEvaluationError::Layout)?
                        };
                        let layout_ms = layout_started_at.elapsed().as_secs_f64() * 1000.0;
                        cached.layout_scale = layout_scale;
                        push_damage_pair(
                            &mut self.pending_decoration_damage,
                            Some(previous_root),
                            transformed_root_rect(cached.layout.root.rect, cached.visual_transform),
                        );
                        cached.client_rect = cached_effective_client_rect;
                        cached.client_rect_potentially_stale = false;
                        cached.snapshot = snapshot;
                        let clip_started_at = Instant::now();
                        {
                            timescope::scope!("ssd window clip");
                            let node_geometry = build_node_geometry_map(&cached.layout);
                            cached.content_clip = content_clip_for_layout(
                                &cached.tree,
                                &cached.layout,
                                &node_geometry,
                            );
                        }
                        let clip_ms = clip_started_at.elapsed().as_secs_f64() * 1000.0;
                        let order_started_at = Instant::now();
                        let order_map = {
                            timescope::scope!("ssd window render order");
                            build_render_order_map(&cached.layout)
                        };
                        let order_ms = order_started_at.elapsed().as_secs_f64() * 1000.0;
                        let buffers_started_at = Instant::now();
                        cached.buffers = {
                            timescope::scope!("ssd window cached buffers");
                            build_cached_buffers(&cached.layout, &order_map)
                        };
                        let buffers_ms = buffers_started_at.elapsed().as_secs_f64() * 1000.0;
                        let shader_started_at = Instant::now();
                        cached.shader_buffers = {
                            timescope::scope!("ssd window shader buffers");
                            build_shader_buffers(&cached.layout, &order_map)
                        };
                        let shader_ms = shader_started_at.elapsed().as_secs_f64() * 1000.0;
                        let text_started_at = Instant::now();
                        let previous_text_buffers = cached.text_buffers.clone();
                        cached.text_buffers = {
                            timescope::scope!("ssd window text buffers");
                            build_text_buffers_with_fallback(
                                &cached.layout,
                                &order_map,
                                window_raster_scale,
                                &mut self.text_rasterizer,
                                &previous_text_buffers,
                            )
                        };
                        let text_ms = text_started_at.elapsed().as_secs_f64() * 1000.0;
                        let icon_started_at = Instant::now();
                        cached.icon_buffers = {
                            timescope::scope!("ssd window icon buffers");
                            build_icon_buffers(
                                &cached.layout,
                                &order_map,
                                window_raster_scale,
                                &cached.snapshot,
                                &mut self.icon_rasterizer,
                            )
                        };
                        let icon_ms = icon_started_at.elapsed().as_secs_f64() * 1000.0;
                        self.suggested_window_offset = suggested_window_offset(&cached.layout);
                        relayout += 1;
                        let elapsed_ms = started_at.elapsed().as_secs_f64() * 1000.0;
                        debug!(
                            window_id = cached.snapshot.id,
                            title = cached.snapshot.title,
                            text_buffer_count = cached.text_buffers.len(),
                            elapsed_ms,
                            "recomputed window decoration layout"
                        );
                        log_animation_window_refresh_timing(
                            "relayout",
                            &cached.snapshot,
                            elapsed_ms,
                            0.0,
                            layout_ms,
                            clip_ms,
                            order_ms,
                            buffers_ms,
                            shader_ms,
                            text_ms,
                            icon_ms,
                            finalize_ms,
                            0,
                            None,
                            None,
                        );
                        log_decoration_refresh(
                            "relayout",
                            &cached.snapshot,
                            cached_effective_client_rect,
                            &cached.layout,
                            &cached.buffers,
                        );
                        self.schedule_redraw();
                        record_managed_rect_path_event(ManagedRectPathEvent::RefreshSizeRelayout);
                    } else if runtime_dirty {
                        let started_at = Instant::now();
                        let previous_root =
                            transformed_root_rect(cached.layout.root.rect, cached.visual_transform);
                        if runtime_dirty_debug_enabled() || minimize_debug {
                            info!(
                                window_id = %snapshot_id,
                                title = %snapshot.title,
                                cached_title = %cached.snapshot.title,
                                runtime_state_changed,
                                window_was_runtime_dirty,
                                force_runtime_reevaluate,
                                force_full_cached_reevaluation,
                                force_output_animation_reevaluate,
                                force_async_asset_refresh,
                                client_rect_source,
                                cached_dynamic_idle = cached.managed_window.idle,
                                cached_dynamic_visible = cached.managed_window.visible,
                                cached_dynamic_interactive = cached.managed_window.interactive,
                                cached_animation_active = cached.managed_window_animation_active,
                                cached_dynamic_opacity = cached.visual_transform.opacity,
                                cached_static_idle = cached.static_managed_window.idle,
                                cached_static_visible = cached.static_managed_window.visible,
                                cached_static_opacity = cached.static_visual_transform.opacity,
                                "runtime dirty debug: evaluating cached window"
                            );
                        }
                        let evaluate_started_at = Instant::now();
                        let mut evaluation = {
                            timescope::scope!("ssd window runtime evaluate");
                            if runtime_state_changed && !force_full_cached_reevaluation {
                                match self.config_runtime.evaluate_window(&snapshot, now_ms) {
                                    Ok(evaluation) => evaluation.into(),
                                    Err(error) => {
                                        warn!(
                                            window_id = snapshot.id,
                                            title = snapshot.title,
                                            app_id = snapshot.app_id,
                                            ?error,
                                            "decoration runtime evaluation failed during runtime state update, falling back to static decoration"
                                        );
                                        StaticDecorationEvaluator
                                            .evaluate_window(&snapshot, now_ms)?
                                            .into()
                                    }
                                }
                            } else {
                                match self.config_runtime.evaluate_cached_window(
                                    &snapshot.id,
                                    (runtime_state_changed || force_full_cached_reevaluation)
                                        .then_some(&snapshot),
                                    now_ms,
                                    force_full_cached_reevaluation,
                                ) {
                                    Ok(evaluation) => evaluation,
                                    Err(error) => {
                                        warn!(
                                            window_id = snapshot.id,
                                            title = snapshot.title,
                                            app_id = snapshot.app_id,
                                            ?error,
                                            "cached decoration runtime evaluation failed during transform update, re-seeding the runtime cache"
                                        );
                                        // Retry through the *cached* entry point with the
                                        // snapshot attached rather than `evaluate_window`.
                                        // Both run a full composition evaluation, but only
                                        // this one repopulates the runtime's
                                        // `cacheByWindowId`: the `evaluate` handler merely
                                        // reads that map, so a plain full evaluation leaves
                                        // the cache missing and every later frame lands
                                        // here again. That is what turns a single spurious
                                        // `windowClosed` for a still-live window into a
                                        // permanent per-frame re-evaluation — pointer
                                        // motion stutters and the decoration stays stale
                                        // until the window really closes. Passing the
                                        // snapshot takes the runtime's recreate branch,
                                        // which rebuilds the entry and re-emits focus and
                                        // first-commit, so the next frame is cached again.
                                        match self.config_runtime.evaluate_cached_window(
                                            &snapshot.id,
                                            Some(&snapshot),
                                            now_ms,
                                            true,
                                        ) {
                                            Ok(evaluation) => evaluation,
                                            Err(error) => {
                                                warn!(
                                                    window_id = snapshot.id,
                                                    title = snapshot.title,
                                                    app_id = snapshot.app_id,
                                                    ?error,
                                                    "decoration runtime cache re-seed failed during transform update, falling back to static decoration"
                                                );
                                                StaticDecorationEvaluator
                                                    .evaluate_window(&snapshot, now_ms)?
                                                    .into()
                                            }
                                        }
                                    }
                                }
                            }
                        };
                        let evaluate_ms = evaluate_started_at.elapsed().as_secs_f64() * 1000.0;
                        if runtime_dirty_debug_enabled() || minimize_debug {
                            info!(
                                window_id = %snapshot_id,
                                title = %snapshot.title,
                                managed_window_only = evaluation.managed_window_only,
                                dirty_node_ids = ?evaluation.dirty_node_ids,
                                action_count = evaluation.actions.len(),
                                action_kinds = ?evaluation
                                    .actions
                                    .iter()
                                    .map(|action| (&action.action, action.channel.as_deref(), action.animation.as_ref().map(|animation| animation.channel.as_str())))
                                    .collect::<Vec<_>>(),
                                next_idle = evaluation.managed_window.idle,
                                next_visible = evaluation.managed_window.visible,
                                next_interactive = evaluation.managed_window.interactive,
                                next_transform_opacity = evaluation.transform.opacity,
                                next_poll_in_ms = ?evaluation.next_poll_in_ms,
                                "runtime dirty debug: cached evaluation result"
                            );
                        }
                        pre_advance_actions.extend(std::mem::take(&mut evaluation.actions));
                        if evaluation.managed_window_only {
                            if force_async_asset_refresh {
                                // Text and icons rasterize asynchronously, and the
                                // refresh after they arrive is the only one that picks
                                // them up. A managed-window-only update (a window
                                // animating open, say) must not skip it, or the labels
                                // and icons stay blank until something relayouts the
                                // window.
                                let order_map = build_render_order_map(&cached.layout);
                                let previous_text_buffers = cached.text_buffers.clone();
                                cached.text_buffers = build_text_buffers_with_fallback(
                                    &cached.layout,
                                    &order_map,
                                    window_raster_scale,
                                    &mut self.text_rasterizer,
                                    &previous_text_buffers,
                                );
                                cached.icon_buffers = build_icon_buffers(
                                    &cached.layout,
                                    &order_map,
                                    window_raster_scale,
                                    &cached.snapshot,
                                    &mut self.icon_rasterizer,
                                );
                            }
                            if runtime_dirty_debug_enabled() {
                                info!(
                                    window_id = %snapshot_id,
                                    title = %snapshot.title,
                                    cached_title = %cached.snapshot.title,
                                    text_buffers = ?label_debug_enabled().then(|| summarize_text_buffers(&cached.text_buffers)),
                                    "runtime dirty debug: managed-window-only result"
                                );
                            }

                            let has_active_animation = cached.managed_window_animation_active;

                            let next_managed_window = evaluation.managed_window;
                            let next_transform = evaluation.transform;
                            let previous_dynamic_rect = cached.managed_window.rect;
                            let previous_static_rect = cached.static_managed_window.rect;
                            let previous_transform = cached.visual_transform;
                            let previous_static_transform = cached.static_visual_transform;

                            cached.snapshot = snapshot;
                            cached.static_managed_window = next_managed_window.clone();
                            cached.static_visual_transform = next_transform;
                            cached.window_effects = evaluation.window_effects;
                            if managed_rect_debug_enabled() {
                                info!(
                                    window_id = %snapshot_id,
                                    title = %cached.snapshot.title,
                                    has_active_animation,
                                    previous_dynamic_rect = ?previous_dynamic_rect,
                                    previous_static_rect = ?previous_static_rect,
                                    next_static_rect = ?cached.static_managed_window.rect,
                                    dynamic_rect_after_static_update = ?cached.managed_window.rect,
                                    previous_transform_translate_x = previous_transform.translate_x,
                                    previous_transform_translate_y = previous_transform.translate_y,
                                    previous_transform_scale_x = previous_transform.scale_x,
                                    previous_transform_scale_y = previous_transform.scale_y,
                                    previous_transform_opacity = previous_transform.opacity,
                                    previous_static_transform_translate_x = previous_static_transform.translate_x,
                                    previous_static_transform_translate_y = previous_static_transform.translate_y,
                                    next_static_transform_translate_x = cached.static_visual_transform.translate_x,
                                    next_static_transform_translate_y = cached.static_visual_transform.translate_y,
                                    dynamic_transform_translate_x = cached.visual_transform.translate_x,
                                    dynamic_transform_translate_y = cached.visual_transform.translate_y,
                                    "managed rect debug: managed-only state update"
                                );
                            }

                            if has_active_animation {
                                // 重要:
                                // 現在の cached.managed_window / cached.visual_transform は
                                // animation が生成した「今フレームの見た目」なので潰さない。
                                //
                                // refresh の最後で advance_managed_window_animations(now_ms) が走り、
                                // static_managed_window / static_visual_transform から
                                // 正しい animated state を再計算する。
                                cached.client_rect_potentially_stale = true;

                                runtime_dirty_updates = runtime_dirty_updates.saturating_add(1);
                                record_managed_rect_path_event(
                                    ManagedRectPathEvent::RuntimeManagedOnly,
                                );
                                managed_rect_apply_window_ids.insert(snapshot_id.clone());

                                self.schedule_redraw();
                                self.runtime_scheduler_enabled =
                                    evaluation.next_poll_in_ms.is_some();
                                animation_active_for_target |=
                                    evaluation.next_poll_in_ms == Some(0);
                                processed_runtime_dirty_window_ids.insert(snapshot_id);
                                continue;
                            }

                            // animation が無い場合だけ dynamic state も更新する
                            cached.managed_window = next_managed_window;
                            cached.visual_transform = next_transform;
                            if managed_rect_debug_enabled() {
                                info!(
                                    window_id = %snapshot_id,
                                    title = %cached.snapshot.title,
                                    dynamic_rect_after_update = ?cached.managed_window.rect,
                                    dynamic_transform_translate_x = cached.visual_transform.translate_x,
                                    dynamic_transform_translate_y = cached.visual_transform.translate_y,
                                    dynamic_transform_scale_x = cached.visual_transform.scale_x,
                                    dynamic_transform_scale_y = cached.visual_transform.scale_y,
                                    dynamic_transform_opacity = cached.visual_transform.opacity,
                                    "managed rect debug: managed-only dynamic committed"
                                );
                            }

                            let next_root = transformed_root_rect(
                                cached.layout.root.rect,
                                cached.visual_transform,
                            );
                            push_damage_pair(
                                &mut self.pending_decoration_damage,
                                Some(previous_root),
                                next_root,
                            );
                            let elapsed_ms = started_at.elapsed().as_secs_f64() * 1000.0;
                            log_animation_window_refresh_timing(
                                "managed-window-only",
                                &cached.snapshot,
                                elapsed_ms,
                                evaluate_ms,
                                0.0,
                                0.0,
                                0.0,
                                0.0,
                                0.0,
                                0.0,
                                0.0,
                                0.0,
                                0,
                                Some(false),
                                Some(true),
                            );
                            runtime_dirty_updates = runtime_dirty_updates.saturating_add(1);
                            record_managed_rect_path_event(
                                ManagedRectPathEvent::RuntimeManagedOnly,
                            );
                            managed_rect_apply_window_ids.insert(snapshot_id.clone());
                            cached.client_rect_potentially_stale = false;
                            self.schedule_redraw();
                            self.runtime_scheduler_enabled = evaluation.next_poll_in_ms.is_some();
                            animation_active_for_target |= evaluation.next_poll_in_ms == Some(0);
                            processed_runtime_dirty_window_ids.insert(snapshot_id);
                            continue;
                        }
                        let composition_uniform_only = is_shader_uniform_only_update(
                            &evaluation.node,
                            &evaluation.node_patches,
                        );
                        let composition_unchanged =
                            evaluation.node.is_none() && evaluation.node_patches.is_empty();
                        let shader_uniform_fast_path = !force_async_asset_refresh
                            && (composition_uniform_only || evaluation.window_effect_uniform_only)
                            && (composition_uniform_only || composition_unchanged)
                            && evaluation.transform == cached.static_visual_transform
                            && evaluation.managed_window == cached.static_managed_window
                            && (evaluation.window_effect_uniform_only
                                || evaluation.window_effects == cached.window_effects);
                        if shader_uniform_fast_path {
                            let update = {
                                timescope::scope!("ssd window shader uniform fast update");
                                apply_shader_uniform_fast_update(
                                    &mut cached.tree,
                                    &mut cached.layout,
                                    &mut cached.buffers,
                                    &mut cached.shader_buffers,
                                    &evaluation.node_patches,
                                )?
                            };
                            cached.snapshot = snapshot;
                            if evaluation.window_effect_uniform_only {
                                cached.window_effects = evaluation.window_effects;
                            }
                            let rendered_changed =
                                update.rendered_changed || evaluation.window_effect_uniform_only;
                            if rendered_changed {
                                self.pending_decoration_damage.extend(
                                    update.damage_rects.into_iter().map(|rect| {
                                        transformed_root_rect(rect, cached.visual_transform)
                                    }),
                                );
                            }
                            let elapsed_ms = started_at.elapsed().as_secs_f64() * 1000.0;
                            record_managed_rect_path_event(ManagedRectPathEvent::RuntimeDirty);
                            log_animation_window_refresh_timing(
                                "shader-uniform-fast",
                                &cached.snapshot,
                                elapsed_ms,
                                evaluate_ms,
                                0.0,
                                0.0,
                                0.0,
                                0.0,
                                0.0,
                                0.0,
                                0.0,
                                0.0,
                                evaluation.dirty_node_ids.len(),
                                Some(update.tree_changed),
                                Some(true),
                            );
                            runtime_dirty_updates = runtime_dirty_updates.saturating_add(1);
                            self.runtime_scheduler_enabled = evaluation.next_poll_in_ms.is_some();
                            animation_active_for_target |= evaluation.next_poll_in_ms == Some(0);
                            processed_runtime_dirty_window_ids.insert(snapshot_id);
                            if rendered_changed {
                                self.schedule_redraw();
                            }
                            continue;
                        }
                        let previous_transform = cached.visual_transform;
                        let previous_layout = cached.layout.clone();
                        let previous_buffers = cached.buffers.clone();
                        let previous_shader_buffers = cached.shader_buffers.clone();
                        let previous_text_buffers = cached.text_buffers.clone();
                        let previous_icon_buffers = cached.icon_buffers.clone();
                        let rebuild_started_at = Instant::now();
                        let next_transform = evaluation.transform;
                        let next_managed_window = evaluation.managed_window;
                        let next_window_effects = evaluation.window_effects;
                        let mut client_rect_potentially_stale = cached
                            .client_rect_potentially_stale
                            || next_managed_window != cached.static_managed_window
                            || layout_scale != cached.layout_scale;
                        let dirty_node_ids = evaluation.dirty_node_ids;
                        let label_debug = label_debug_enabled();
                        let cached_label_summary =
                            label_debug.then(|| summarize_tree_labels(&cached.tree));
                        let tree_update = {
                            timescope::scope!("ssd window apply tree update");
                            apply_cached_tree_update(
                                &mut cached.tree,
                                evaluation.node.take(),
                                std::mem::take(&mut evaluation.node_patches),
                            )?
                        };
                        let next_label_summary =
                            label_debug.then(|| summarize_tree_labels(&cached.tree));
                        let previous_text_summary =
                            label_debug.then(|| summarize_text_buffers(&previous_text_buffers));
                        if runtime_dirty_debug_enabled() {
                            info!(
                                window_id = %snapshot_id,
                                title = %snapshot.title,
                                cached_title = %cached.snapshot.title,
                                tree_changed = tree_update.changed,
                                dirty_node_count = dirty_node_ids.len(),
                                dirty_node_ids = ?dirty_node_ids,
                                previous_text_count = previous_text_buffers.len(),
                                cached_labels = ?cached_label_summary,
                                next_labels = ?next_label_summary,
                                previous_text_buffers = ?previous_text_summary,
                                "runtime dirty debug: tree result"
                            );
                        }
                        let mut layout_equivalent_state = None;
                        cached.snapshot = snapshot;
                        cached.static_managed_window = next_managed_window.clone();

                        let has_active_animation = cached.managed_window_animation_active;

                        if !has_active_animation {
                            cached.managed_window = next_managed_window;
                        }
                        cached.window_effects = next_window_effects;

                        if !tree_update.changed {
                            if !has_active_animation {
                                cached.visual_transform = next_transform;
                            }
                            cached.static_visual_transform = next_transform;
                        } else {
                            let layout_equivalent = tree_update.layout_equivalent;
                            layout_equivalent_state = Some(layout_equivalent);
                            if layout_equivalent {
                                {
                                    timescope::scope!("ssd window reapply tree layout");
                                    reapply_tree_preserving_layout(
                                        &mut cached.layout.root,
                                        &cached.tree.root,
                                        None,
                                    );
                                    cached.layout.root.sync_root_bounds();
                                }
                                {
                                    timescope::scope!("ssd window clip");
                                    let node_geometry = build_node_geometry_map(&cached.layout);
                                    cached.content_clip = content_clip_for_layout(
                                        &cached.tree,
                                        &cached.layout,
                                        &node_geometry,
                                    );
                                }
                                let order_map = {
                                    timescope::scope!("ssd window render order");
                                    build_render_order_map(&cached.layout)
                                };
                                if dirty_node_ids.is_empty() {
                                    cached.buffers = {
                                        timescope::scope!("ssd window cached buffers");
                                        build_cached_buffers(&cached.layout, &order_map)
                                    };
                                    cached.shader_buffers = {
                                        timescope::scope!("ssd window shader buffers");
                                        build_shader_buffers(&cached.layout, &order_map)
                                    };
                                    {
                                        timescope::scope!("ssd window freeze shader buffers");
                                        freeze_manual_shader_buffers(
                                            &previous_shader_buffers,
                                            &mut cached.shader_buffers,
                                        );
                                    }
                                    cached.text_buffers = {
                                        timescope::scope!("ssd window text buffers");
                                        build_text_buffers_with_fallback(
                                            &cached.layout,
                                            &order_map,
                                            window_raster_scale,
                                            &mut self.text_rasterizer,
                                            &previous_text_buffers,
                                        )
                                    };
                                    cached.icon_buffers = {
                                        timescope::scope!("ssd window icon buffers");
                                        build_icon_buffers(
                                            &cached.layout,
                                            &order_map,
                                            window_raster_scale,
                                            &cached.snapshot,
                                            &mut self.icon_rasterizer,
                                        )
                                    };
                                } else {
                                    let (rebuilt_buffers, rebuilt_shader_buffers) = {
                                        timescope::scope!("ssd window partial buffers");
                                        rebuild_partial_buffers(
                                            &cached.layout,
                                            &order_map,
                                            &dirty_node_ids,
                                        )
                                    };
                                    let mut merged_shader_buffers = {
                                        timescope::scope!("ssd window merge shader buffers");
                                        merge_shader_buffers(
                                            &previous_shader_buffers,
                                            rebuilt_shader_buffers,
                                            &dirty_node_ids,
                                            &order_map,
                                        )
                                    };
                                    {
                                        timescope::scope!("ssd window freeze shader buffers");
                                        freeze_manual_shader_buffers(
                                            &previous_shader_buffers,
                                            &mut merged_shader_buffers,
                                        );
                                    }
                                    cached.buffers = {
                                        timescope::scope!("ssd window merge cached buffers");
                                        merge_cached_buffers(
                                            &previous_buffers,
                                            rebuilt_buffers,
                                            &dirty_node_ids,
                                            &order_map,
                                        )
                                    };
                                    cached.shader_buffers = merged_shader_buffers;
                                    cached.text_buffers = {
                                        timescope::scope!("ssd window partial text buffers");
                                        merge_text_buffers(
                                            &previous_text_buffers,
                                            rebuild_partial_text_buffers_with_fallback(
                                                &cached.layout,
                                                &order_map,
                                                &dirty_node_ids,
                                                window_raster_scale,
                                                &mut self.text_rasterizer,
                                                &previous_text_buffers,
                                            ),
                                            &dirty_node_ids,
                                            &order_map,
                                        )
                                    };
                                    cached.icon_buffers = {
                                        timescope::scope!("ssd window partial icon buffers");
                                        merge_icon_buffers(
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
                                            &order_map,
                                        )
                                    };
                                }
                            } else {
                                let layout_client_rect = {
                                    timescope::scope!("ssd window effective client rect");
                                    managed_client_rect_for_state(
                                        &cached.tree,
                                        &cached.managed_window,
                                        client_rect,
                                        layout_scale,
                                    )?
                                };
                                cached.layout = {
                                    timescope::scope!("ssd window layout");
                                    cached
                                        .tree
                                        .layout_for_client_with_subpixel(
                                            layout_client_rect,
                                            layout_scale,
                                            cached.root_subpixel_offset,
                                        )
                                        .map_err(super::DecorationEvaluationError::Layout)?
                                };
                                cached.layout_scale = layout_scale;
                                cached.client_rect = layout_client_rect;
                                client_rect_potentially_stale = has_active_animation;
                                {
                                    timescope::scope!("ssd window clip");
                                    let node_geometry = build_node_geometry_map(&cached.layout);
                                    cached.content_clip = content_clip_for_layout(
                                        &cached.tree,
                                        &cached.layout,
                                        &node_geometry,
                                    );
                                }
                                let order_map = {
                                    timescope::scope!("ssd window render order");
                                    build_render_order_map(&cached.layout)
                                };
                                cached.buffers = {
                                    timescope::scope!("ssd window cached buffers");
                                    build_cached_buffers(&cached.layout, &order_map)
                                };
                                cached.shader_buffers = {
                                    timescope::scope!("ssd window shader buffers");
                                    build_shader_buffers(&cached.layout, &order_map)
                                };
                                {
                                    timescope::scope!("ssd window freeze shader buffers");
                                    freeze_manual_shader_buffers(
                                        &previous_shader_buffers,
                                        &mut cached.shader_buffers,
                                    );
                                }
                                cached.text_buffers = {
                                    timescope::scope!("ssd window text buffers");
                                    build_text_buffers_with_fallback(
                                        &cached.layout,
                                        &order_map,
                                        window_raster_scale,
                                        &mut self.text_rasterizer,
                                        &previous_text_buffers,
                                    )
                                };
                                cached.icon_buffers = {
                                    timescope::scope!("ssd window icon buffers");
                                    build_icon_buffers(
                                        &cached.layout,
                                        &order_map,
                                        window_raster_scale,
                                        &cached.snapshot,
                                        &mut self.icon_rasterizer,
                                    )
                                };
                                {
                                    timescope::scope!("ssd window suggested offset");
                                    self.suggested_window_offset =
                                        suggested_window_offset(&cached.layout);
                                }
                            }
                            if !has_active_animation {
                                cached.visual_transform = next_transform;
                            }
                            cached.static_visual_transform = next_transform;
                        }
                        let rebuild_ms = rebuild_started_at.elapsed().as_secs_f64() * 1000.0;
                        let finalize_started_at = Instant::now();
                        {
                            timescope::scope!("ssd window runtime dirty damage");
                            let next_root = transformed_root_rect(
                                cached.layout.root.rect,
                                cached.visual_transform,
                            );
                            if previous_transform != cached.visual_transform
                                || previous_root != next_root
                            {
                                push_damage_pair(
                                    &mut self.pending_decoration_damage,
                                    Some(previous_root),
                                    next_root,
                                );
                            } else if !dirty_node_ids.is_empty() {
                                self.pending_decoration_damage.extend(
                                    runtime_dirty_node_damage_rects(
                                        &previous_layout,
                                        previous_transform,
                                        &cached.layout,
                                        cached.visual_transform,
                                        &dirty_node_ids,
                                    ),
                                );
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
                        }
                        let finalize_ms = finalize_started_at.elapsed().as_secs_f64() * 1000.0;
                        let elapsed_ms = started_at.elapsed().as_secs_f64() * 1000.0;
                        debug!(
                            window_id = cached.snapshot.id,
                            title = cached.snapshot.title,
                            text_buffer_count = cached.text_buffers.len(),
                            elapsed_ms,
                            "recomputed window decoration tree from runtime dirty state"
                        );
                        record_managed_rect_path_event(ManagedRectPathEvent::RuntimeDirty);
                        managed_rect_apply_window_ids.insert(snapshot_id.clone());
                        log_animation_window_refresh_timing(
                            "runtime-dirty",
                            &cached.snapshot,
                            elapsed_ms,
                            evaluate_ms,
                            rebuild_ms,
                            0.0,
                            0.0,
                            0.0,
                            0.0,
                            0.0,
                            0.0,
                            finalize_ms,
                            dirty_node_ids.len(),
                            Some(tree_update.changed),
                            layout_equivalent_state,
                        );
                        if force_async_asset_refresh {
                            let order_map = build_render_order_map(&cached.layout);
                            let previous_text_buffers = cached.text_buffers.clone();
                            cached.text_buffers = build_text_buffers_with_fallback(
                                &cached.layout,
                                &order_map,
                                window_raster_scale,
                                &mut self.text_rasterizer,
                                &previous_text_buffers,
                            );
                            cached.icon_buffers = build_icon_buffers(
                                &cached.layout,
                                &order_map,
                                window_raster_scale,
                                &cached.snapshot,
                                &mut self.icon_rasterizer,
                            );
                        }
                        if label_debug_enabled() {
                            info!(
                                window_id = %cached.snapshot.id,
                                title = %cached.snapshot.title,
                                text_buffers = ?summarize_text_buffers(&cached.text_buffers),
                                "label debug: runtime dirty final text buffers"
                            );
                        }
                        log_decoration_refresh(
                            "runtime-dirty",
                            &cached.snapshot,
                            client_rect,
                            &cached.layout,
                            &cached.buffers,
                        );
                        runtime_dirty_updates = runtime_dirty_updates.saturating_add(1);
                        // Uniform, color, and other layout-equivalent changes do
                        // not affect the materialised client rect. Only keep the
                        // cache stale when one of its actual geometry inputs
                        // changed without a relayout above.
                        cached.client_rect_potentially_stale = client_rect_potentially_stale;
                        self.schedule_redraw();
                        self.runtime_scheduler_enabled = evaluation.next_poll_in_ms.is_some();
                        animation_active_for_target |= evaluation.next_poll_in_ms == Some(0);
                    } else if force_async_asset_refresh {
                        let order_map = build_render_order_map(&cached.layout);
                        let previous_text_buffers = cached.text_buffers.clone();
                        cached.text_buffers = build_text_buffers_with_fallback(
                            &cached.layout,
                            &order_map,
                            window_raster_scale,
                            &mut self.text_rasterizer,
                            &previous_text_buffers,
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
                if window_was_runtime_dirty {
                    processed_runtime_dirty_window_ids.insert(snapshot_id);
                }
            }
        }
        let windows_pass_elapsed_ms = windows_pass_started_at.elapsed().as_secs_f64() * 1000.0;
        if !pre_advance_actions.is_empty() {
            let deferred = {
                timescope::scope!("ssd apply pre-advance actions");
                self.apply_pre_advance_animation_actions(std::mem::take(&mut pre_advance_actions))
            };
            pending_window_actions.extend(deferred);
        }
        managed_rect_apply_window_ids.extend({
            timescope::scope!("ssd advance window animations");
            // The frame's presentation time when a render asked for this refresh; the
            // wall clock otherwise. Never before the wall clock: an animation started
            // since the frame was predicted must not sample before its own start.
            let wall_ms = Duration::from(self.clock.now()).as_secs_f64() * 1000.0;
            let animation_now_ms = self
                .animation_frame_time_ms
                .map_or(wall_ms, |frame_ms| frame_ms.max(wall_ms));
            self.advance_managed_window_animations(animation_now_ms)
        });
        if managed_rect_debug_enabled() {
            let mut apply_ids = managed_rect_apply_window_ids
                .iter()
                .cloned()
                .collect::<Vec<_>>();
            apply_ids.sort();
            info!(
                ?apply_ids,
                count = apply_ids.len(),
                target_output = ?target_output_name,
                force_runtime_reevaluate,
                runtime_dirty_window_ids_count = self.runtime_dirty_window_ids.len(),
                runtime_managed_only_window_ids_count = self.runtime_managed_only_window_ids.len(),
                "managed rect debug: refresh apply batch"
            );
        }
        {
            timescope::scope!("ssd apply managed window rects");
            self.apply_managed_window_rects(&managed_rect_apply_window_ids, false);
        }

        let closing_pass_started_at = Instant::now();
        let closing_dirty_ids = {
            timescope::scope!("ssd collect closing dirty ids");
            self.closing_window_snapshots
                .keys()
                .filter(|window_id| {
                    force_output_animation_reevaluate
                        || self.runtime_dirty_window_ids.contains(*window_id)
                })
                .cloned()
                .collect::<Vec<_>>()
        };
        {
            timescope::scope!("ssd closing pass");
            for window_id in closing_dirty_ids {
                let force_full_cached_reevaluation =
                    self.runtime_dirty_window_ids.contains(&window_id)
                        && !self.runtime_node_only_window_ids.contains(&window_id);
                let closing_raster_scale = self
                    .closing_window_snapshots
                    .get(&window_id)
                    .map(|closing| self.decoration_raster_scale_for_rect(closing.live.rect))
                    .unwrap_or(1.0);
                if let Some(closing) = self.closing_window_snapshots.get_mut(&window_id) {
                    let previous_root = transformed_root_rect(
                        closing.decoration.layout.root.rect,
                        closing.transform,
                    );
                    let previous_layout = closing.decoration.layout.clone();
                    let previous_transform = closing.transform;
                    let previous_buffers = closing.decoration.buffers.clone();
                    let previous_shader_buffers = closing.decoration.shader_buffers.clone();
                    let previous_text_buffers = closing.decoration.text_buffers.clone();
                    let previous_icon_buffers = closing.decoration.icon_buffers.clone();
                    let mut evaluation = {
                        timescope::scope!("ssd closing runtime evaluate");
                        self.config_runtime.evaluate_cached_window(
                            &window_id,
                            None,
                            now_ms,
                            force_full_cached_reevaluation,
                        )?
                    };
                    pre_advance_actions.extend(std::mem::take(&mut evaluation.actions));
                    if evaluation.managed_window_only {
                        // Once a native close animation has completed, its
                        // final state is frozen; adopting the TS static
                        // transform here would flash the window back to
                        // opacity 1 (see `native_animation_completed`).
                        let preserve_animated_state =
                            closing.decoration.managed_window_animation_active
                                || closing.native_animation_completed;
                        let next_managed_window = evaluation.managed_window;
                        let next_transform = evaluation.transform;

                        closing.decoration.static_managed_window = next_managed_window.clone();
                        closing.decoration.static_visual_transform = next_transform;
                        closing.decoration.window_effects = evaluation.window_effects;
                        if !preserve_animated_state {
                            closing.decoration.managed_window = next_managed_window;
                            closing.decoration.visual_transform = next_transform;
                            closing.transform = next_transform;
                        }
                        if closing.decoration.managed_window.managed
                            && let Some(desired_root) = closing.decoration.managed_window.rect
                        {
                            let desired_root = managed_rect_snapshot_to_logical_rect(desired_root);
                            if desired_root.width > 0 && desired_root.height > 0 {
                                let desired_client = managed_client_rect_for_root(
                                    &closing.decoration.tree,
                                    desired_root,
                                    closing.decoration.layout_scale,
                                )?;
                                let position_changed = desired_client.x
                                    != closing.decoration.client_rect.x
                                    || desired_client.y != closing.decoration.client_rect.y;
                                let size_changed = desired_client.width
                                    != closing.decoration.client_rect.width
                                    || desired_client.height
                                        != closing.decoration.client_rect.height;
                                if size_changed {
                                    let layout = closing
                                        .decoration
                                        .tree
                                        .layout_for_client_with_scale(
                                            desired_client,
                                            closing.decoration.layout_scale,
                                        )
                                        .map_err(super::DecorationEvaluationError::Layout)?;
                                    let node_geometry = build_node_geometry_map(&layout);
                                    let content_clip = content_clip_for_layout(
                                        &closing.decoration.tree,
                                        &layout,
                                        &node_geometry,
                                    );
                                    let order_map = build_render_order_map(&layout);
                                    closing.decoration.layout = layout;
                                    closing.decoration.content_clip = content_clip;
                                    closing.decoration.client_rect = desired_client;
                                    closing.decoration.snapshot.position =
                                        WindowPositionSnapshot::from(desired_client);
                                    closing.decoration.buffers = build_cached_buffers(
                                        &closing.decoration.layout,
                                        &order_map,
                                    );
                                    closing.decoration.shader_buffers = build_shader_buffers(
                                        &closing.decoration.layout,
                                        &order_map,
                                    );
                                    freeze_manual_shader_buffers(
                                        &previous_shader_buffers,
                                        &mut closing.decoration.shader_buffers,
                                    );
                                    closing.decoration.text_buffers =
                                        build_text_buffers_with_fallback(
                                            &closing.decoration.layout,
                                            &order_map,
                                            closing_raster_scale,
                                            &mut self.text_rasterizer,
                                            &previous_text_buffers,
                                        );
                                    closing.decoration.icon_buffers = build_icon_buffers(
                                        &closing.decoration.layout,
                                        &order_map,
                                        closing_raster_scale,
                                        &closing.decoration.snapshot,
                                        &mut self.icon_rasterizer,
                                    );
                                    closing.live.rect = desired_client;
                                } else if position_changed {
                                    let dx = desired_client.x - closing.decoration.client_rect.x;
                                    let dy = desired_client.y - closing.decoration.client_rect.y;
                                    translate_cached_decoration_position(
                                        &mut closing.decoration,
                                        dx,
                                        dy,
                                        desired_client,
                                    );
                                    closing.live.rect = desired_client;
                                }
                            }
                        }
                        if !preserve_animated_state {
                            closing.transform = next_transform;
                        }
                        let next_root = transformed_root_rect(
                            closing.decoration.layout.root.rect,
                            closing.transform,
                        );
                        push_damage_pair(
                            &mut self.pending_decoration_damage,
                            Some(previous_root),
                            next_root,
                        );
                        if evaluation.next_poll_in_ms.is_none()
                            && closing.transform.opacity <= 0.001
                        {
                            pending_finalize_close_damage.push(next_root);
                            pending_window_actions.push(crate::ssd::RuntimeWindowAction {
                                window_id: window_id.clone(),
                                action: crate::ssd::WaylandWindowAction::FinalizeClose,
                                animation: None,
                                channel: None,
                            });
                        }
                        self.runtime_scheduler_enabled = evaluation.next_poll_in_ms.is_some();
                        self.schedule_redraw();
                        closing_runtime_updates = closing_runtime_updates.saturating_add(1);
                        animation_active_for_target |= evaluation.next_poll_in_ms == Some(0);
                        processed_runtime_dirty_window_ids.insert(window_id);
                        continue;
                    }
                    let tree_update = apply_cached_tree_update(
                        &mut closing.decoration.tree,
                        evaluation.node.take(),
                        std::mem::take(&mut evaluation.node_patches),
                    )?;
                    // See the managed_window_only branch above: a completed
                    // native close animation freezes the animated state.
                    let preserve_animated_state =
                        closing.decoration.managed_window_animation_active
                            || closing.native_animation_completed;
                    let next_managed_window = evaluation.managed_window;
                    let next_transform = evaluation.transform;
                    closing.decoration.static_managed_window = next_managed_window.clone();
                    closing.decoration.window_effects = evaluation.window_effects;
                    if !preserve_animated_state {
                        closing.decoration.managed_window = next_managed_window;
                    }
                    let dirty_node_ids = evaluation.dirty_node_ids;
                    if !tree_update.changed {
                        if !preserve_animated_state {
                            closing.decoration.visual_transform = next_transform;
                        }
                        closing.decoration.static_visual_transform = next_transform;
                    } else {
                        let layout_equivalent = tree_update.layout_equivalent;
                        if layout_equivalent {
                            reapply_tree_preserving_layout(
                                &mut closing.decoration.layout.root,
                                &closing.decoration.tree.root,
                                None,
                            );
                            closing.decoration.layout.root.sync_root_bounds();
                            let node_geometry = build_node_geometry_map(&closing.decoration.layout);
                            closing.decoration.content_clip = content_clip_for_layout(
                                &closing.decoration.tree,
                                &closing.decoration.layout,
                                &node_geometry,
                            );
                            let order_map = build_render_order_map(&closing.decoration.layout);
                            if dirty_node_ids.is_empty() {
                                closing.decoration.buffers =
                                    build_cached_buffers(&closing.decoration.layout, &order_map);
                                closing.decoration.shader_buffers =
                                    build_shader_buffers(&closing.decoration.layout, &order_map);
                                closing.decoration.text_buffers = build_text_buffers_with_fallback(
                                    &closing.decoration.layout,
                                    &order_map,
                                    closing_raster_scale,
                                    &mut self.text_rasterizer,
                                    &previous_text_buffers,
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
                                    rebuild_partial_buffers(
                                        &closing.decoration.layout,
                                        &order_map,
                                        &dirty_node_ids,
                                    );
                                let mut merged_shader_buffers = merge_shader_buffers(
                                    &previous_shader_buffers,
                                    rebuilt_shader_buffers,
                                    &dirty_node_ids,
                                    &order_map,
                                );
                                freeze_manual_shader_buffers(
                                    &previous_shader_buffers,
                                    &mut merged_shader_buffers,
                                );
                                closing.decoration.buffers = merge_cached_buffers(
                                    &previous_buffers,
                                    rebuilt_buffers,
                                    &dirty_node_ids,
                                    &order_map,
                                );
                                closing.decoration.shader_buffers = merged_shader_buffers;
                                closing.decoration.text_buffers = merge_text_buffers(
                                    &previous_text_buffers,
                                    rebuild_partial_text_buffers_with_fallback(
                                        &closing.decoration.layout,
                                        &order_map,
                                        &dirty_node_ids,
                                        closing_raster_scale,
                                        &mut self.text_rasterizer,
                                        &previous_text_buffers,
                                    ),
                                    &dirty_node_ids,
                                    &order_map,
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
                                    &order_map,
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
                            let node_geometry = build_node_geometry_map(&layout);
                            let content_clip = content_clip_for_layout(
                                &closing.decoration.tree,
                                &layout,
                                &node_geometry,
                            );
                            let order_map = build_render_order_map(&layout);
                            let buffers = build_cached_buffers(&layout, &order_map);
                            let shader_buffers = build_shader_buffers(&layout, &order_map);
                            let text_buffers = build_text_buffers_with_fallback(
                                &layout,
                                &order_map,
                                closing_raster_scale,
                                &mut self.text_rasterizer,
                                &previous_text_buffers,
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
                        if !preserve_animated_state {
                            closing.decoration.visual_transform = next_transform;
                        }
                        closing.decoration.static_visual_transform = next_transform;
                    }
                    if closing.decoration.managed_window.managed
                        && let Some(desired_root) = closing.decoration.managed_window.rect
                    {
                        let desired_root = managed_rect_snapshot_to_logical_rect(desired_root);
                        if desired_root.width > 0 && desired_root.height > 0 {
                            let desired_client = managed_client_rect_for_root(
                                &closing.decoration.tree,
                                desired_root,
                                closing.decoration.layout_scale,
                            )?;
                            let position_changed = desired_client.x
                                != closing.decoration.client_rect.x
                                || desired_client.y != closing.decoration.client_rect.y;
                            let size_changed = desired_client.width
                                != closing.decoration.client_rect.width
                                || desired_client.height != closing.decoration.client_rect.height;
                            if size_changed {
                                let layout = closing
                                    .decoration
                                    .tree
                                    .layout_for_client_with_scale(
                                        desired_client,
                                        closing.decoration.layout_scale,
                                    )
                                    .map_err(super::DecorationEvaluationError::Layout)?;
                                let node_geometry = build_node_geometry_map(&layout);
                                let content_clip = content_clip_for_layout(
                                    &closing.decoration.tree,
                                    &layout,
                                    &node_geometry,
                                );
                                let order_map = build_render_order_map(&layout);
                                closing.decoration.layout = layout;
                                closing.decoration.content_clip = content_clip;
                                closing.decoration.client_rect = desired_client;
                                closing.decoration.snapshot.position =
                                    WindowPositionSnapshot::from(desired_client);
                                closing.decoration.buffers =
                                    build_cached_buffers(&closing.decoration.layout, &order_map);
                                closing.decoration.shader_buffers =
                                    build_shader_buffers(&closing.decoration.layout, &order_map);
                                freeze_manual_shader_buffers(
                                    &previous_shader_buffers,
                                    &mut closing.decoration.shader_buffers,
                                );
                                closing.decoration.text_buffers = build_text_buffers_with_fallback(
                                    &closing.decoration.layout,
                                    &order_map,
                                    closing_raster_scale,
                                    &mut self.text_rasterizer,
                                    &previous_text_buffers,
                                );
                                closing.decoration.icon_buffers = build_icon_buffers(
                                    &closing.decoration.layout,
                                    &order_map,
                                    closing_raster_scale,
                                    &closing.decoration.snapshot,
                                    &mut self.icon_rasterizer,
                                );
                                closing.live.rect = desired_client;
                            } else if position_changed {
                                let dx = desired_client.x - closing.decoration.client_rect.x;
                                let dy = desired_client.y - closing.decoration.client_rect.y;
                                translate_cached_decoration_position(
                                    &mut closing.decoration,
                                    dx,
                                    dy,
                                    desired_client,
                                );
                                closing.live.rect = desired_client;
                            }
                        }
                    }
                    if !preserve_animated_state {
                        closing.decoration.visual_transform = next_transform;
                        closing.transform = next_transform;
                    }
                    closing.decoration.static_visual_transform = next_transform;
                    let next_root = transformed_root_rect(
                        closing.decoration.layout.root.rect,
                        closing.transform,
                    );
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
                        let previous_text_buffers = closing.decoration.text_buffers.clone();
                        closing.decoration.text_buffers = build_text_buffers_with_fallback(
                            &closing.decoration.layout,
                            &order_map,
                            closing_raster_scale,
                            &mut self.text_rasterizer,
                            &previous_text_buffers,
                        );
                        closing.decoration.icon_buffers = build_icon_buffers(
                            &closing.decoration.layout,
                            &order_map,
                            closing_raster_scale,
                            &closing.decoration.snapshot,
                            &mut self.icon_rasterizer,
                        );
                    }
                    if evaluation.next_poll_in_ms.is_none() && closing.transform.opacity <= 0.001 {
                        pending_finalize_close_damage.push(next_root);
                        pending_window_actions.push(crate::ssd::RuntimeWindowAction {
                            window_id: window_id.clone(),
                            action: crate::ssd::WaylandWindowAction::FinalizeClose,
                            animation: None,
                            channel: None,
                        });
                    }
                    self.runtime_scheduler_enabled = evaluation.next_poll_in_ms.is_some();
                    self.schedule_redraw();
                    closing_runtime_updates = closing_runtime_updates.saturating_add(1);
                    animation_active_for_target |= evaluation.next_poll_in_ms == Some(0);
                    processed_runtime_dirty_window_ids.insert(window_id);
                }
            }
        }
        let closing_pass_elapsed_ms = closing_pass_started_at.elapsed().as_secs_f64() * 1000.0;

        // Closing-pass evaluations drain TS actions into `pre_advance_actions`
        // too, but the pre-advance application above already ran before this
        // pass — without a second sweep those actions (e.g. a `finalizeClose`
        // pending in TS, or a follow-up scheduleAnimation) would be silently
        // dropped. Apply schedule/cancel immediately and defer the rest to the
        // end-of-refresh action sweep like everywhere else.
        if !pre_advance_actions.is_empty() {
            let deferred = {
                timescope::scope!("ssd apply closing-pass actions");
                self.apply_pre_advance_animation_actions(std::mem::take(&mut pre_advance_actions))
            };
            pending_window_actions.extend(deferred);
        }

        if let Some(output_name) = target_output_name {
            if animation_active_for_target {
                self.runtime_animation_outputs
                    .insert(output_name.to_string());
            } else {
                self.runtime_animation_outputs.remove(output_name);
            }
            log_animation_output_activity(
                output_name,
                closing_active_count,
                animation_active_for_target,
            );
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
                let closing_raster_scale = *closing_scales.get(window_id).unwrap_or(&1.0);
                let order_map = build_render_order_map(&closing.decoration.layout);
                closing.decoration.buffers =
                    build_cached_buffers(&closing.decoration.layout, &order_map);
                closing.decoration.shader_buffers =
                    build_shader_buffers(&closing.decoration.layout, &order_map);
                let previous_text_buffers = closing.decoration.text_buffers.clone();
                closing.decoration.text_buffers = build_text_buffers_with_fallback(
                    &closing.decoration.layout,
                    &order_map,
                    closing_raster_scale,
                    &mut self.text_rasterizer,
                    &previous_text_buffers,
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

        let apply_updates_started_at = Instant::now();
        self.drain_runtime_host_messages();
        if !pending_finalize_close_damage.is_empty() {
            self.pending_decoration_damage
                .extend(pending_finalize_close_damage);
        }
        if !pending_window_actions.is_empty() {
            self.apply_runtime_window_actions(pending_window_actions);
        }
        // Minimize/restore land as `managed_window.idle` transitions during the
        // evaluations above, not as focus changes — re-derive each toplevel's
        // xdg `Activated` state so those transitions emit configures (see
        // `sync_window_activated_states` for why clients depend on this).
        self.sync_window_activated_states();
        let apply_updates_elapsed_ms = apply_updates_started_at.elapsed().as_secs_f64() * 1000.0;
        let refresh_elapsed_ms = refresh_started_at.elapsed().as_secs_f64() * 1000.0;

        if animation_timing_debug_enabled()
            && (animation_active_for_target
                || closing_active_count > 0
                || promoted_closing > 0
                || rebuilt > 0
                || relayout > 0
                || runtime_dirty_updates > 0
                || closing_runtime_updates > 0
                || refresh_elapsed_ms >= spike_threshold_ms)
        {
            let target_output = target_output_name.unwrap_or("<all>");
            if refresh_elapsed_ms >= spike_threshold_ms {
                warn!(
                    target_output,
                    window_count,
                    closing_active_count,
                    promoted_closing,
                    rebuilt,
                    relayout,
                    runtime_dirty_updates,
                    closing_runtime_updates,
                    animation_active_for_target,
                    force_runtime_reevaluate,
                    force_output_animation_reevaluate,
                    force_async_asset_refresh,
                    removed_windows_elapsed_ms,
                    windows_pass_elapsed_ms,
                    closing_pass_elapsed_ms,
                    apply_updates_elapsed_ms,
                    elapsed_ms = refresh_elapsed_ms,
                    spike_threshold_ms,
                    "animation timing: decoration refresh spike"
                );
            } else {
                info!(
                    target_output,
                    window_count,
                    closing_active_count,
                    promoted_closing,
                    rebuilt,
                    relayout,
                    runtime_dirty_updates,
                    closing_runtime_updates,
                    animation_active_for_target,
                    force_runtime_reevaluate,
                    force_output_animation_reevaluate,
                    force_async_asset_refresh,
                    removed_windows_elapsed_ms,
                    windows_pass_elapsed_ms,
                    closing_pass_elapsed_ms,
                    apply_updates_elapsed_ms,
                    elapsed_ms = refresh_elapsed_ms,
                    spike_threshold_ms,
                    "animation timing: decoration refresh"
                );
            }
        }

        trace!(
            window_count,
            rebuilt,
            relayout,
            elapsed_ms = refresh_elapsed_ms,
            "refresh_window_decorations finished"
        );
        let marked_during_refresh = self
            .runtime_dirty_marked_during_refresh
            .take()
            .unwrap_or_default();
        for window_id in processed_runtime_dirty_window_ids {
            // Marked again after it was processed: refresh it next time.
            if marked_during_refresh.contains(&window_id) {
                continue;
            }
            self.runtime_dirty_window_ids.remove(&window_id);
            self.runtime_managed_only_window_ids.remove(&window_id);
            self.runtime_node_only_window_ids.remove(&window_id);
        }
        self.runtime_dirty_window_ids.retain(|window_id| {
            live_window_ids.contains(window_id)
                || self.closing_window_snapshots.contains_key(window_id)
        });
        self.runtime_managed_only_window_ids.retain(|window_id| {
            live_window_ids.contains(window_id)
                || self.closing_window_snapshots.contains_key(window_id)
        });
        self.runtime_node_only_window_ids.retain(|window_id| {
            live_window_ids.contains(window_id)
                || self.closing_window_snapshots.contains_key(window_id)
        });
        self.pending_xdg_state_configure_window_ids
            .retain(|window_id| live_window_ids.contains(window_id));
        self.runtime_poll_dirty = !self.runtime_dirty_window_ids.is_empty();
        self.async_asset_dirty = false;
        self.sync_taskbar_window_states();
        self.sync_popup_dismissals();

        Ok(())
    }

    pub fn decoration_under(
        &self,
        point: Point<f64, Logical>,
    ) -> Option<(Window, DecorationHitTestResult)> {
        // Popups are hit only while their window is untransformed, so the
        // point needs no inverse transform.
        if let Some(window) = self.ssd_popup_window_under(point) {
            let hit = self.window_decorations.get(&window)?.hit_test(point);
            return Some((window, hit));
        }
        let output_name = self.output_name_at_point(point);
        self.windows_top_to_bottom().into_iter().find_map(|window| {
            let decoration = self.window_decorations.get(window)?;
            if !output_name.as_deref().map_or_else(
                || decoration.managed_window_allows_input(),
                |output| decoration.managed_window_allows_input_on_output(output),
            ) {
                return None;
            }
            let logical_point = LogicalPoint::new(point.x.floor() as i32, point.y.floor() as i32);
            let transformed_root =
                transformed_root_rect(decoration.layout.root.rect, decoration.visual_transform);
            let inside = transformed_root.contains(logical_point)
                && self.client_input_region_accepts(window, Some(decoration), point);
            if !inside && !decoration.resize_band_contains(point) {
                return None;
            }
            let local_point = inverse_transform_point(
                point,
                decoration.layout.root.rect,
                decoration.visual_transform,
            );
            Some((window.clone(), decoration.hit_test(local_point)))
        })
    }

    pub fn decoration_interaction_target_under(
        &self,
        point: Point<f64, Logical>,
    ) -> Option<(Window, super::DecorationInteractionTarget)> {
        let (window, targets) = self.decoration_interaction_targets_under(point)?;
        targets.into_iter().next().map(|target| (window, target))
    }

    /// Like `decoration_interaction_target_under`, with the whole hover chain
    /// when the pointer is inside an interactive `<Popup>` (innermost first).
    pub fn decoration_interaction_targets_under(
        &self,
        point: Point<f64, Logical>,
    ) -> Option<(Window, Vec<super::DecorationInteractionTarget>)> {
        if let Some(window) = self.ssd_popup_window_under(point) {
            let decoration = self.window_decorations.get(&window)?;
            let targets = decoration
                .layout
                .interaction_targets_at_precise(point.x, point.y);
            return Some((window, targets));
        }
        let output_name = self.output_name_at_point(point);
        self.windows_top_to_bottom().into_iter().find_map(|window| {
            let decoration = self.window_decorations.get(window)?;
            if !output_name.as_deref().map_or_else(
                || decoration.managed_window_allows_input(),
                |output| decoration.managed_window_allows_input_on_output(output),
            ) {
                return None;
            }
            let logical_point = LogicalPoint::new(point.x.floor() as i32, point.y.floor() as i32);
            let transformed_root =
                transformed_root_rect(decoration.layout.root.rect, decoration.visual_transform);
            if !transformed_root.contains(logical_point)
                || !self.client_input_region_accepts(window, Some(decoration), point)
            {
                // The resize band ends the search: nothing below it is hovered.
                return decoration
                    .resize_band_contains(point)
                    .then(|| (window.clone(), Vec::new()));
            }
            let local_point = inverse_transform_point(
                point,
                decoration.layout.root.rect,
                decoration.visual_transform,
            );
            let targets = decoration
                .layout
                .interaction_targets_at_precise(local_point.x, local_point.y);
            (!targets.is_empty()).then(|| (window.clone(), targets))
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

    /// `defer_state_configures`: the caller is the animation-schedule path,
    /// which runs *inside* the TS handler that requested a fullscreen or
    /// maximize state change — before the composition it also changed has
    /// been re-evaluated. The client size derived here still uses the old
    /// chrome insets (a fullscreen request computed 1596x966 for a 1600x1000
    /// output; the unfullscreen computed 1604x1034 for a windowed client), and
    /// the corrected configure followed a moment later. SDL2 games (Unity,
    /// Source) memorise whatever size they were last configured at while
    /// windowed and XResizeWindow back to it on their next mode switch, so the
    /// stale first size came back as a 1604x1034 X window and the fullscreen
    /// state machine never settled. Leaving the state transition's configure
    /// to the refresh pass — which runs after the re-evaluation and still
    /// sees `pending_xdg_state_configure_window_ids` — sends exactly one
    /// configure with the final size and state.
    fn apply_managed_window_rects(
        &mut self,
        dirty_window_ids: &std::collections::HashSet<String>,
        defer_state_configures: bool,
    ) {
        timescope::scope!("ssd apply managed window rects body");
        if managed_rect_debug_enabled() {
            let mut dirty_ids = dirty_window_ids.iter().cloned().collect::<Vec<_>>();
            dirty_ids.sort();
            let mut configured_ids = self
                .pending_xdg_state_configure_window_ids
                .iter()
                .cloned()
                .collect::<Vec<_>>();
            configured_ids.sort();
            let mut cached_ids = self
                .window_decorations
                .values()
                .map(|decoration| decoration.snapshot.id.clone())
                .collect::<Vec<_>>();
            cached_ids.sort();
            info!(
                ?dirty_ids,
                ?configured_ids,
                ?cached_ids,
                "managed rect debug: apply start"
            );
        }
        let windows = {
            timescope::scope!("ssd apply managed collect candidates");
            self.window_decorations
                .iter()
                .filter_map(|(window, decoration)| {
                    let managed = &decoration.managed_window;
                    if !managed.managed {
                        return None;
                    }
                    if !dirty_window_ids.contains(&decoration.snapshot.id)
                        && !self
                            .pending_xdg_state_configure_window_ids
                            .contains(&decoration.snapshot.id)
                    {
                        return None;
                    }
                    Some(window.clone())
                })
                .collect::<Vec<_>>()
        };

        if managed_rect_debug_enabled() {
            let mut candidate_ids = windows
                .iter()
                .filter_map(|window| {
                    self.window_decorations
                        .get(window)
                        .map(|decoration| decoration.snapshot.id.clone())
                })
                .collect::<Vec<_>>();
            candidate_ids.sort();
            info!(
                ?candidate_ids,
                count = candidate_ids.len(),
                "managed rect debug: apply candidates"
            );
        }

        for window in windows {
            timescope::scope!("ssd apply managed window");
            let Some((
                force_rect_size,
                tiled,
                needs_xdg_state_configure,
                desired_root_raw,
                desired_root,
                current_root,
                current_client,
                window_id,
                last_configured_client_size,
                rect_override_target_raw,
            )) = ({
                timescope::scope!("ssd apply managed window state");
                let Some(decoration) = self.window_decorations.get(&window) else {
                    continue;
                };
                let managed = &decoration.managed_window;
                let Some(desired_root_raw) = managed.rect else {
                    if managed_rect_debug_enabled() {
                        info!(
                            window_id = %decoration.snapshot.id,
                            title = %decoration.snapshot.title,
                            managed = managed.managed,
                            "managed rect debug: apply skip missing desired rect"
                        );
                    }
                    continue;
                };
                let window_id = decoration.snapshot.id.clone();
                let static_managed_window = decoration.static_managed_window.clone();
                let rect_override_target_raw = self
                    .managed_window_animations
                    .get(&window_id)
                    .and_then(|channels| {
                        final_override_rect_animation_target(&static_managed_window, channels)
                    });
                // Snap the animated rect to the physical pixel grid, anchored
                // at the animation's final target. Windows animating with
                // identical deltas and timing (workspace scroll / strip
                // reflow) then move in identical physical-pixel steps, so
                // their relative positions stay rigid — independently rounding
                // a shared fractional delta per window makes neighbouring gaps
                // oscillate by ±1 physical px. Anchoring at the target (not
                // the start) makes the final frame land exactly on the static
                // rect, avoiding a settle jump when the animation completes.
                let desired_root_raw = match rect_override_target_raw {
                    Some(target) => snap_rect_animation_to_physical_grid(
                        desired_root_raw,
                        target,
                        decoration.layout_scale,
                    ),
                    None => desired_root_raw,
                };
                let desired_root = managed_rect_snapshot_to_logical_rect(desired_root_raw);
                let current_root = decoration.layout.root.rect;
                let current_client = decoration.client_rect;
                let last_configured_client_size = decoration.last_configured_client_size;
                Some((
                    managed.force_rect_size,
                    managed.tiled,
                    self.pending_xdg_state_configure_window_ids
                        .contains(&window_id),
                    desired_root_raw,
                    desired_root,
                    current_root,
                    current_client,
                    window_id,
                    last_configured_client_size,
                    rect_override_target_raw,
                ))
            })
            else {
                continue;
            };

            // The integer pipeline below may conclude "noop" while the
            // fractional origin remainder still moved (sub-logical-pixel
            // animation steps). Rendering anchors the root physical origin on
            // this fraction, so persist it and repaint even when every integer
            // rect comparison says nothing changed.
            {
                let desired_subpixel = managed_rect_snapshot_subpixel_edges(desired_root_raw);
                let subpixel_changed =
                    self.window_decorations
                        .get_mut(&window)
                        .is_some_and(|decoration| {
                            let changed = decoration.root_subpixel_offset != desired_subpixel;
                            decoration.root_subpixel_offset = desired_subpixel;
                            changed
                        });
                if subpixel_changed {
                    // The fractional shift moves rendering by at most one
                    // physical pixel; damage one logical pixel beyond both
                    // integer roots to cover it.
                    self.pending_decoration_damage.push(LogicalRect::new(
                        current_root.x - 1,
                        current_root.y - 1,
                        current_root.width + 2,
                        current_root.height + 2,
                    ));
                    self.pending_decoration_damage.push(LogicalRect::new(
                        desired_root.x - 1,
                        desired_root.y - 1,
                        desired_root.width + 2,
                        desired_root.height + 2,
                    ));
                    self.window_scene_generation = self.window_scene_generation.wrapping_add(1);
                    self.schedule_redraw();
                }
            }

            // A sub-pixel change of the root *size* moves the root's far edge
            // by a physical pixel without changing any integer rect: relayout
            // so the decoration follows (the client slot absorbs it).
            let subpixel_size_changed = force_rect_size
                && self
                    .window_decorations
                    .get(&window)
                    .is_some_and(|decoration| {
                        decoration.layout.root.frame.root_extra_px
                            != root_subpixel_extra_px(
                                current_root.width,
                                current_root.height,
                                decoration.root_subpixel_offset,
                                decoration.layout_scale,
                            )
                    });

            // When an Override rect animation is in flight we want the client
            // configured at its **final** target size — not the animated
            // intermediate — so its buffer arrives at the right resolution
            // exactly once and the visual scaling is handled by viewporter /
            // SSD layout instead of by stretching a lagging buffer. The
            // visual rect (relocate, SSD layout) still uses the animated
            // `desired_client`; only the size we hand the client deviates.
            let active_rect_override_target =
                rect_override_target_raw.map(managed_rect_snapshot_to_logical_rect);

            let tiled_state_changed = {
                timescope::scope!("ssd apply managed tiled state");
                window.toplevel().is_some_and(|toplevel| {
                    toplevel.with_pending_state(|state| {
                        let tiled_states = [
                            xdg_toplevel::State::TiledLeft,
                            xdg_toplevel::State::TiledRight,
                            xdg_toplevel::State::TiledTop,
                            xdg_toplevel::State::TiledBottom,
                        ];
                        let was_tiled = tiled_states
                            .iter()
                            .all(|state_name| state.states.contains(*state_name));
                        for state_name in tiled_states {
                            if tiled {
                                state.states.set(state_name);
                            } else {
                                state.states.unset(state_name);
                            }
                        }
                        was_tiled != tiled
                    })
                })
            };

            let configure_client_size = {
                timescope::scope!("ssd apply managed configure size");
                if let Some(final_root) = active_rect_override_target {
                    let final_client = managed_client_rect_from_current_insets(
                        current_root,
                        current_client,
                        final_root,
                    );
                    (final_client.width, final_client.height)
                } else {
                    let desired_client = managed_client_rect_from_current_insets(
                        current_root,
                        current_client,
                        desired_root,
                    );
                    (desired_client.width, desired_client.height)
                }
            };
            let configure_size_changed = last_configured_client_size != Some(configure_client_size);
            let should_configure =
                configure_size_changed || needs_xdg_state_configure || tiled_state_changed;

            if desired_root == current_root && !should_configure && !subpixel_size_changed {
                record_managed_rect_path_event(ManagedRectPathEvent::ApplyNoop);
                if managed_rect_debug_enabled() {
                    info!(
                        window_id,
                        desired_root = %format_rect(desired_root),
                        current_root = %format_rect(current_root),
                        needs_xdg_state_configure,
                        configure_size_changed,
                        "managed rect debug: apply noop root"
                    );
                }
                self.sync_space_location_to_client_rect(&window, current_client);
                continue;
            }

            let desired_client = {
                timescope::scope!("ssd apply managed desired client");
                let root_size_changed = desired_root.width != current_root.width
                    || desired_root.height != current_root.height;
                // Without forceRectSize the client keeps its own size (e.g.
                // swappy settles on a size other than the managed rect), so
                // only the origin follows the rect. Treating the mismatch as a
                // size change would skip the position update below, and the
                // window would move only when the client commits.
                if root_size_changed && force_rect_size {
                    record_managed_rect_path_event(ManagedRectPathEvent::ApplySizeFast);
                    managed_client_rect_from_current_insets(
                        current_root,
                        current_client,
                        desired_root,
                    )
                } else {
                    let dx = desired_root.x - current_root.x;
                    let dy = desired_root.y - current_root.y;
                    if dx != 0 || dy != 0 {
                        record_managed_rect_path_event(ManagedRectPathEvent::ApplyPositionFast);
                    }
                    LogicalRect::new(
                        current_client.x + dx,
                        current_client.y + dy,
                        current_client.width,
                        current_client.height,
                    )
                }
            };

            if desired_client == current_client && !should_configure && !subpixel_size_changed {
                record_managed_rect_path_event(ManagedRectPathEvent::ApplyNoop);
                if managed_rect_debug_enabled() {
                    info!(
                        window_id,
                        desired_root = %format_rect(desired_root),
                        current_root = %format_rect(current_root),
                        desired_client = %format_rect(desired_client),
                        current_client = %format_rect(current_client),
                        needs_xdg_state_configure,
                        configure_size_changed,
                        "managed rect debug: apply noop client"
                    );
                }
                self.sync_space_location_to_client_rect(&window, current_client);
                continue;
            }

            let position_changed =
                desired_client.x != current_client.x || desired_client.y != current_client.y;
            let size_changed = desired_client.width != current_client.width
                || desired_client.height != current_client.height;
            let dx = desired_client.x - current_client.x;
            let dy = desired_client.y - current_client.y;

            if size_changed {
                record_managed_rect_path_event(ManagedRectPathEvent::ApplySize);
            } else if position_changed {
                record_managed_rect_path_event(ManagedRectPathEvent::ApplyPosition);
            } else {
                record_managed_rect_path_event(ManagedRectPathEvent::ApplyConfigureOnly);
            }

            if managed_rect_debug_enabled() {
                info!(
                    window_id,
                    raw_desired_root_x = desired_root_raw.x,
                    raw_desired_root_y = desired_root_raw.y,
                    raw_desired_root_width = desired_root_raw.width,
                    raw_desired_root_height = desired_root_raw.height,
                    raw_desired_root_right = desired_root_raw.x + desired_root_raw.width,
                    raw_desired_root_bottom = desired_root_raw.y + desired_root_raw.height,
                    desired_root = %format_rect(desired_root),
                    desired_root_right = desired_root.x + desired_root.width,
                    desired_root_bottom = desired_root.y + desired_root.height,
                    current_root = %format_rect(current_root),
                    current_root_right = current_root.x + current_root.width,
                    current_root_bottom = current_root.y + current_root.height,
                    current_client = %format_rect(current_client),
                    current_client_right = current_client.x + current_client.width,
                    current_client_bottom = current_client.y + current_client.height,
                    desired_client = %format_rect(desired_client),
                    desired_client_right = desired_client.x + desired_client.width,
                    desired_client_bottom = desired_client.y + desired_client.height,
                    dx,
                    dy,
                    position_changed,
                    size_changed,
                    "managed rect debug: apply"
                );
            }

            {
                timescope::scope!("ssd apply managed relocate");
                let geometry = window.geometry();
                let next_location = Point::from((
                    desired_client.x - geometry.loc.x,
                    desired_client.y - geometry.loc.y,
                ));
                if self.space.element_location(&window) != Some(next_location) {
                    self.space.relocate_element(&window, next_location);
                }
            }

            // Only push a configure when the size actually changes from what
            // the client was last told. `needs_xdg_state_configure` still
            // forces one through for non-size state updates (maximize, etc.).
            let send_configure =
                should_configure && !(defer_state_configures && needs_xdg_state_configure);
            if should_configure && !send_configure {
                // Stage the size anyway so a state-only configure sent by the
                // request finisher (when no refresh follows) carries it.
                if let Some(toplevel) = window.toplevel() {
                    toplevel.with_pending_state(|state| {
                        state.size = Some(Size::from(configure_client_size));
                    });
                }
                record_managed_rect_path_event(ManagedRectPathEvent::ApplyConfigureOnly);
            }
            if send_configure {
                timescope::scope!("ssd apply managed configure client");
                if let Some(toplevel) = window.toplevel() {
                    toplevel.with_pending_state(|state| {
                        state.size = Some(Size::from(configure_client_size));
                    });
                    toplevel.send_pending_configure();
                    self.pending_xdg_state_configure_window_ids
                        .remove(&window_id);
                    if let Some(decoration) = self.window_decorations.get_mut(&window) {
                        decoration.last_configured_client_size = Some(configure_client_size);
                    }
                } else if let Some(x11) = window.x11_surface() {
                    if configure_size_changed {
                        let placed = Rectangle::<i32, Logical>::new(
                            Point::from((desired_client.x, desired_client.y)),
                            Size::from(configure_client_size),
                        );
                        if let Err(error) = x11.configure(Some(placed)) {
                            warn!(
                                ?error,
                                window_id, "failed to configure managed X11 window rect"
                            );
                        }
                        if let Some(decoration) = self.window_decorations.get_mut(&window) {
                            decoration.last_configured_client_size = Some(configure_client_size);
                        }
                    }
                    self.pending_xdg_state_configure_window_ids
                        .remove(&window_id);
                }
            }
            if size_changed || subpixel_size_changed {
                timescope::scope!("ssd apply managed size rebuild");
                let window_raster_scale = self.decoration_raster_scale_for_window(&window);
                if force_rect_size
                    && let Some(decoration) = self.window_decorations.get_mut(&window)
                {
                    let previous_shader_buffers = decoration.shader_buffers.clone();
                    let previous_text_buffers = decoration.text_buffers.clone();
                    let layout = {
                        timescope::scope!("ssd apply managed size layout");
                        decoration
                            .tree
                            .layout_for_client_with_subpixel(
                                desired_client,
                                decoration.layout_scale,
                                decoration.root_subpixel_offset,
                            )
                            .map_err(super::DecorationEvaluationError::Layout)
                            .ok()
                    };
                    if let Some(layout) = layout {
                        let (content_clip, buffers, shader_buffers, text_buffers, icon_buffers) = {
                            timescope::scope!("ssd apply managed size buffers");
                            let arena = Bump::new();
                            let node_geometry = build_node_geometry_map_in(&layout, &arena);
                            let content_clip =
                                content_clip_for_layout(&decoration.tree, &layout, &node_geometry);
                            let order_map = build_render_order_map(&layout);
                            let (buffers, mut shader_buffers) = build_cached_buffers_and_shaders(
                                &layout,
                                &order_map,
                                None,
                                &node_geometry,
                            );
                            freeze_manual_shader_buffers(
                                &previous_shader_buffers,
                                &mut shader_buffers,
                            );
                            let text_buffers = {
                                timescope::scope!("ssd apply managed size text buffers");
                                if text_buffers_need_raster_for_layout(
                                    &layout,
                                    &node_geometry,
                                    &previous_text_buffers,
                                    window_raster_scale,
                                ) {
                                    build_text_buffers_with_node_geometry(
                                        &layout,
                                        &order_map,
                                        &node_geometry,
                                        window_raster_scale,
                                        &mut self.text_rasterizer,
                                        &previous_text_buffers,
                                    )
                                } else {
                                    retarget_text_buffers_with_node_geometry(
                                        &layout,
                                        &order_map,
                                        &node_geometry,
                                        &previous_text_buffers,
                                    )
                                }
                            };
                            let icon_buffers = {
                                timescope::scope!("ssd apply managed size icon buffers");
                                retarget_icon_buffers_with_node_geometry(
                                    &layout,
                                    &order_map,
                                    &node_geometry,
                                    &decoration.snapshot,
                                    &decoration.icon_buffers,
                                )
                            };
                            (
                                content_clip,
                                buffers,
                                shader_buffers,
                                text_buffers,
                                icon_buffers,
                            )
                        };
                        decoration.layout = layout;
                        decoration.content_clip = content_clip;
                        decoration.client_rect = desired_client;
                        decoration.snapshot.position = WindowPositionSnapshot::from(desired_client);
                        decoration.buffers = buffers;
                        decoration.shader_buffers = shader_buffers;
                        decoration.text_buffers = text_buffers;
                        decoration.icon_buffers = icon_buffers;
                    }
                }
            } else if position_changed {
                timescope::scope!("ssd apply managed position update");
                if let Some(decoration) = self.window_decorations.get_mut(&window) {
                    translate_cached_decoration_position(decoration, dx, dy, desired_client);
                }
            }

            self.pending_decoration_damage.push(current_root);
            self.pending_decoration_damage.push(LogicalRect::new(
                desired_root.x,
                desired_root.y,
                desired_root.width,
                desired_root.height,
            ));
            if size_changed || subpixel_size_changed {
                self.snapshot_dirty_window_ids.insert(window_id);
            }
            self.window_scene_generation = self.window_scene_generation.wrapping_add(1);
            self.schedule_redraw();
        }

        // Closing snapshots are not present in `window_decorations`, but
        // managed rect animations can still target them after `startClose`.
        // Apply those animated rects to the frozen client snapshot and the
        // cloned decoration cache so close animations can move/resize the
        // whole closing window, not just opacity/transform it.
        self.apply_managed_window_rects_to_closing_snapshots(dirty_window_ids);

        // Do not refresh pointer focus synchronously from apply_managed_window_rects.
        // This can be called from inside pointer grab motion handling.
        /*
        if applied_any_rect {
            let now_msec = std::time::Duration::from(self.clock.now()).as_millis() as u32;
            self.refresh_pointer_focus(now_msec);
        }*/
    }

    fn apply_managed_window_rects_to_closing_snapshots(
        &mut self,
        dirty_window_ids: &std::collections::HashSet<String>,
    ) {
        let closing_ids = self
            .closing_window_snapshots
            .keys()
            .filter(|window_id| dirty_window_ids.contains(*window_id))
            .cloned()
            .collect::<Vec<_>>();

        for window_id in closing_ids {
            let closing_raster_scale = self
                .closing_window_snapshots
                .get(&window_id)
                .map(|closing| self.decoration_raster_scale_for_rect(closing.live.rect))
                .unwrap_or(1.0);

            let Some(closing) = self.closing_window_snapshots.get_mut(&window_id) else {
                continue;
            };
            let managed = &closing.decoration.managed_window;
            if !managed.managed {
                continue;
            }
            let Some(desired_root_raw) = managed.rect else {
                continue;
            };
            let desired_root = managed_rect_snapshot_to_logical_rect(desired_root_raw);
            if desired_root.width <= 0 || desired_root.height <= 0 {
                continue;
            }

            let current_root = closing.decoration.layout.root.rect;
            let current_client = closing.decoration.client_rect;
            if desired_root == current_root {
                continue;
            }

            let desired_client = if desired_root.width != current_root.width
                || desired_root.height != current_root.height
            {
                managed_client_rect_from_current_insets(current_root, current_client, desired_root)
            } else {
                let dx = desired_root.x - current_root.x;
                let dy = desired_root.y - current_root.y;
                LogicalRect::new(
                    current_client.x + dx,
                    current_client.y + dy,
                    current_client.width,
                    current_client.height,
                )
            };
            if desired_client == current_client {
                continue;
            }

            let previous_root =
                transformed_root_rect(closing.decoration.layout.root.rect, closing.transform);
            let position_changed =
                desired_client.x != current_client.x || desired_client.y != current_client.y;
            let size_changed = desired_client.width != current_client.width
                || desired_client.height != current_client.height;

            if size_changed {
                let previous_shader_buffers = closing.decoration.shader_buffers.clone();
                let previous_text_buffers = closing.decoration.text_buffers.clone();
                let layout = match closing
                    .decoration
                    .tree
                    .layout_for_client_with_scale(desired_client, closing.decoration.layout_scale)
                {
                    Ok(layout) => layout,
                    Err(error) => {
                        warn!(
                            ?error,
                            window_id = %window_id,
                            desired_client = %format_rect(desired_client),
                            "failed to apply animated managed rect to closing snapshot"
                        );
                        continue;
                    }
                };
                let node_geometry = build_node_geometry_map(&layout);
                let content_clip =
                    content_clip_for_layout(&closing.decoration.tree, &layout, &node_geometry);
                let order_map = build_render_order_map(&layout);
                let mut shader_buffers = build_shader_buffers(&layout, &order_map);
                freeze_manual_shader_buffers(&previous_shader_buffers, &mut shader_buffers);
                let text_buffers = build_text_buffers_with_fallback(
                    &layout,
                    &order_map,
                    closing_raster_scale,
                    &mut self.text_rasterizer,
                    &previous_text_buffers,
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
                closing.decoration.client_rect = desired_client;
                closing.decoration.snapshot.position = WindowPositionSnapshot::from(desired_client);
                closing.decoration.buffers =
                    build_cached_buffers(&closing.decoration.layout, &order_map);
                closing.decoration.shader_buffers = shader_buffers;
                closing.decoration.text_buffers = text_buffers;
                closing.decoration.icon_buffers = icon_buffers;
                closing.live.rect = desired_client;
            } else if position_changed {
                let dx = desired_client.x - current_client.x;
                let dy = desired_client.y - current_client.y;
                translate_cached_decoration_position(
                    &mut closing.decoration,
                    dx,
                    dy,
                    desired_client,
                );
                closing.live.rect = desired_client;
            }

            let next_root =
                transformed_root_rect(closing.decoration.layout.root.rect, closing.transform);
            push_damage_pair(
                &mut self.pending_decoration_damage,
                Some(previous_root),
                next_root,
            );
            self.window_scene_generation = self.window_scene_generation.wrapping_add(1);
            self.schedule_redraw();

            if managed_rect_debug_enabled() {
                info!(
                    window_id = %window_id,
                    desired_root = %format_rect(desired_root),
                    desired_client = %format_rect(desired_client),
                    position_changed,
                    size_changed,
                    "managed rect debug: applied closing snapshot rect"
                );
            }
        }
    }
}

fn layer_effect_evaluation_signature(output_name: &str, snapshots: &[WaylandLayerSnapshot]) -> u64 {
    effect_evaluation_signature(output_name, snapshots, |snapshot| snapshot.id.as_str())
}

fn popup_effect_evaluation_signature(output_name: &str, snapshots: &[WaylandPopupSnapshot]) -> u64 {
    effect_evaluation_signature(output_name, snapshots, |snapshot| snapshot.id.as_str())
}

fn effect_evaluation_signature<T, F>(output_name: &str, snapshots: &[T], id: F) -> u64
where
    T: Hash,
    F: Fn(&T) -> &str,
{
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    let mut sorted_snapshots = snapshots.iter().collect::<Vec<_>>();
    sorted_snapshots.sort_by(|left, right| id(left).cmp(id(right)));

    output_name.hash(&mut hasher);
    sorted_snapshots.len().hash(&mut hasher);
    for snapshot in sorted_snapshots {
        snapshot.hash(&mut hasher);
    }
    hasher.finish()
}

fn retain_effect_assignments_for_live_ids(
    assignments: &mut std::collections::HashMap<String, WindowEffectConfig>,
    live_ids: &std::collections::HashSet<String>,
) {
    assignments.retain(|id, _| live_ids.contains(id));
}

fn retain_effect_texture_cache_for_live_ids<T>(
    cache: &mut std::collections::HashMap<String, T>,
    live_ids: &std::collections::HashSet<String>,
) {
    cache.retain(|key, _| {
        live_ids.iter().any(|id| {
            key.len() > id.len()
                && key.as_bytes().get(id.len()) == Some(&b'@')
                && key.starts_with(id)
        })
    });
}

fn content_clip_for_layout(
    _tree: &DecorationTree,
    layout: &ComputedDecorationTree,
    node_geometry: &impl NodeGeometryLookup,
) -> Option<ContentClip> {
    let mut clip = slot_content_clip_for_node(&layout.root, None, None, node_geometry)?;
    if let Some(slot) = layout.window_slot_rect() {
        clip.rect = Rectangle::new(
            Point::from((slot.x, slot.y)),
            (slot.width, slot.height).into(),
        );
    }
    Some(clip)
}

/// Snap an animated rect's edges to the physical pixel grid (multiples of
/// 1/scale), anchored at the animation's final target edges. See the call
/// site in `apply_managed_window_rects` for why this keeps concurrently
/// animating windows pixel-rigid relative to each other.
fn snap_rect_animation_to_physical_grid(
    raw: ManagedWindowRectSnapshot,
    target: ManagedWindowRectSnapshot,
    scale: f64,
) -> ManagedWindowRectSnapshot {
    if !scale.is_finite() || scale <= 0.0 {
        return raw;
    }
    let snap = |value: f64, anchor: f64| anchor - ((anchor - value) * scale).round() / scale;
    let left = snap(raw.x, target.x);
    let top = snap(raw.y, target.y);
    let right = snap(raw.x + raw.width, target.x + target.width);
    let bottom = snap(raw.y + raw.height, target.y + target.height);
    ManagedWindowRectSnapshot {
        x: left,
        y: top,
        width: right - left,
        height: bottom - top,
    }
}

/// Fractional edge remainders that `managed_rect_snapshot_to_logical_rect`
/// discards. Derivable from the managed rect alone because the quantized
/// edges are plain `round()` of the raw edges.
fn managed_rect_subpixel_offset(
    managed: &super::ManagedWindowState,
) -> crate::backend::visual::RootSubpixelEdges {
    managed
        .rect
        .map_or(Default::default(), managed_rect_snapshot_subpixel_edges)
}

fn managed_rect_snapshot_subpixel_edges(
    rect: ManagedWindowRectSnapshot,
) -> crate::backend::visual::RootSubpixelEdges {
    let right = rect.x + rect.width;
    let bottom = rect.y + rect.height;
    crate::backend::visual::RootSubpixelEdges {
        left: rect.x - rect.x.round(),
        top: rect.y - rect.y.round(),
        right: right - right.round(),
        bottom: bottom - bottom.round(),
    }
}

fn managed_rect_snapshot_to_logical_rect(rect: ManagedWindowRectSnapshot) -> LogicalRect {
    // Preserve shared/opposite edges when quantizing TS-provided floating rects.
    // Rounding x/y/width/height independently makes `round(x) + round(width)`
    // differ from `round(x + width)`, which shows up as a 1px wobble during
    // top/left anchored resizes and rect animations.
    let left = rect.x.round() as i32;
    let top = rect.y.round() as i32;
    let right = (rect.x + rect.width).round() as i32;
    let bottom = (rect.y + rect.height).round() as i32;

    LogicalRect::new(left, top, right - left, bottom - top)
}

fn managed_animation_progress(active: &ActiveManagedWindowAnimation, now_ms: f64) -> (f64, bool) {
    let duration = active
        .animation
        .rect
        .as_ref()
        .map(|animation| animation.duration)
        .into_iter()
        .chain(
            active
                .animation
                .offset
                .as_ref()
                .map(|animation| animation.duration),
        )
        .chain(
            active
                .animation
                .opacity
                .as_ref()
                .map(|animation| animation.duration),
        )
        .max()
        .unwrap_or(1)
        .max(1);
    let elapsed = (now_ms - active.started_at_ms).max(0.0);
    let raw = (elapsed / duration as f64).clamp(0.0, 1.0);
    let eased = sample_easing(
        active
            .animation
            .rect
            .as_ref()
            .map(|animation| animation.easing)
            .or_else(|| {
                active
                    .animation
                    .offset
                    .as_ref()
                    .map(|animation| animation.easing)
            })
            .or_else(|| {
                active
                    .animation
                    .opacity
                    .as_ref()
                    .map(|animation| animation.easing)
            })
            .unwrap_or_default(),
        raw,
    );
    (eased, elapsed < duration as f64)
}

fn sample_rect_animation(
    animation: &ManagedWindowRectAnimationSnapshot,
    progress: f64,
    fallback: Option<ManagedWindowRectSnapshot>,
) -> ManagedWindowRectSnapshot {
    let from = animation.from.or(fallback).unwrap_or(animation.to);
    ManagedWindowRectSnapshot {
        x: lerp(from.x, animation.to.x, progress),
        y: lerp(from.y, animation.to.y, progress),
        width: lerp(from.width, animation.to.width, progress),
        height: lerp(from.height, animation.to.height, progress),
    }
}

fn sample_point_animation(
    animation: &ManagedWindowPointAnimationSnapshot,
    progress: f64,
) -> ManagedWindowPointSnapshot {
    let from = animation
        .from
        .unwrap_or(ManagedWindowPointSnapshot { x: 0.0, y: 0.0 });
    ManagedWindowPointSnapshot {
        x: lerp(from.x, animation.to.x, progress),
        y: lerp(from.y, animation.to.y, progress),
    }
}

fn sample_scalar_animation(
    animation: &ManagedWindowScalarAnimationSnapshot,
    progress: f64,
    fallback: f64,
) -> f64 {
    let from = animation.from.unwrap_or(fallback);
    lerp(from, animation.to, progress)
}

fn apply_rect_animation_value(
    managed: &mut ManagedWindowState,
    value: ManagedWindowRectSnapshot,
    mode: ManagedWindowAnimationMode,
) {
    let base = managed.rect.unwrap_or(value);
    managed.rect = Some(match mode {
        ManagedWindowAnimationMode::Override | ManagedWindowAnimationMode::Multiply => value,
        ManagedWindowAnimationMode::Add => ManagedWindowRectSnapshot {
            x: base.x + value.x,
            y: base.y + value.y,
            width: base.width + value.width,
            height: base.height + value.height,
        },
        ManagedWindowAnimationMode::Sub => ManagedWindowRectSnapshot {
            x: base.x - value.x,
            y: base.y - value.y,
            width: base.width - value.width,
            height: base.height - value.height,
        },
    });
}

fn apply_offset_animation_value(
    managed: &mut ManagedWindowState,
    value: ManagedWindowPointSnapshot,
    mode: ManagedWindowAnimationMode,
) {
    match mode {
        ManagedWindowAnimationMode::Override => {
            managed.transform.translate_x = value.x;
            managed.transform.translate_y = value.y;
        }
        ManagedWindowAnimationMode::Add | ManagedWindowAnimationMode::Multiply => {
            managed.transform.translate_x += value.x;
            managed.transform.translate_y += value.y;
        }
        ManagedWindowAnimationMode::Sub => {
            managed.transform.translate_x -= value.x;
            managed.transform.translate_y -= value.y;
        }
    }
}

fn apply_opacity_animation_value(
    managed: &mut ManagedWindowState,
    value: f64,
    mode: ManagedWindowAnimationMode,
) {
    let next = match mode {
        ManagedWindowAnimationMode::Override => value,
        ManagedWindowAnimationMode::Add => managed.transform.opacity as f64 + value,
        ManagedWindowAnimationMode::Sub => managed.transform.opacity as f64 - value,
        ManagedWindowAnimationMode::Multiply => managed.transform.opacity as f64 * value,
    };
    managed.transform.opacity = next.clamp(0.0, 1.0) as f32;
}

fn sample_easing(easing: ManagedWindowAnimationEasingSnapshot, progress: f64) -> f64 {
    match easing {
        ManagedWindowAnimationEasingSnapshot::Linear => progress,
        ManagedWindowAnimationEasingSnapshot::CubicBezier { x1, y1, x2, y2 } => {
            sample_cubic_bezier(x1, y1, x2, y2, progress)
        }
    }
}

fn sample_cubic_bezier(x1: f64, y1: f64, x2: f64, y2: f64, progress: f64) -> f64 {
    if progress <= 0.0 {
        return 0.0;
    }
    if progress >= 1.0 {
        return 1.0;
    }

    let cx = 3.0 * x1;
    let bx = 3.0 * (x2 - x1) - cx;
    let ax = 1.0 - cx - bx;

    let cy = 3.0 * y1;
    let by = 3.0 * (y2 - y1) - cy;
    let ay = 1.0 - cy - by;

    let sample_x = |t: f64| ((ax * t + bx) * t + cx) * t;
    let sample_y = |t: f64| ((ay * t + by) * t + cy) * t;
    let sample_dx = |t: f64| (3.0 * ax * t + 2.0 * bx) * t + cx;

    let mut t = progress;

    for _ in 0..8 {
        let estimate = sample_x(t) - progress;
        if estimate.abs() < 1e-6 {
            return sample_y(t);
        }

        let derivative = sample_dx(t);
        if derivative.abs() < 1e-6 {
            break;
        }

        t -= estimate / derivative;
    }

    let mut lower = 0.0;
    let mut upper = 1.0;
    t = progress;

    for _ in 0..12 {
        let estimate = sample_x(t);
        if (estimate - progress).abs() < 1e-7 {
            break;
        }

        if progress > estimate {
            lower = t;
        } else {
            upper = t;
        }

        t = (upper + lower) * 0.5;
    }

    sample_y(t)
}

fn lerp(from: f64, to: f64, progress: f64) -> f64 {
    from + (to - from) * progress
}

/// Composition priority for animations within a single window. Override
/// channels run first (priority 0) so they set the *base* for the frame, then
/// additive / subtractive / multiplicative channels (priority 1) compose
/// their delta on top of that base. Tie-broken by `sequence` so newer
/// animations within the same priority bucket override older ones.
fn animation_mode_priority(animation: &ManagedWindowAnimationSnapshot) -> u8 {
    let is_override = animation
        .rect
        .as_ref()
        .map(|r| matches!(r.mode, ManagedWindowAnimationMode::Override))
        .or_else(|| {
            animation
                .offset
                .as_ref()
                .map(|o| matches!(o.mode, ManagedWindowAnimationMode::Override))
        })
        .or_else(|| {
            animation
                .opacity
                .as_ref()
                .map(|o| matches!(o.mode, ManagedWindowAnimationMode::Override))
        })
        .unwrap_or(false);
    if is_override { 0 } else { 1 }
}

fn final_override_rect_animation_target(
    static_managed_window: &ManagedWindowState,
    channels: &BTreeMap<String, ActiveManagedWindowAnimation>,
) -> Option<ManagedWindowRectSnapshot> {
    if !channels.values().any(|active| {
        active
            .animation
            .rect
            .as_ref()
            .is_some_and(|rect| rect.mode == ManagedWindowAnimationMode::Override)
    }) {
        return None;
    }

    let mut animations = channels.values().collect::<Vec<_>>();
    animations.sort_by_key(|active| (animation_mode_priority(&active.animation), active.sequence));

    let mut final_managed_window = static_managed_window.clone();
    for active in animations {
        let Some(rect_animation) = active.animation.rect.as_ref() else {
            continue;
        };
        let rect = sample_rect_animation(rect_animation, 1.0, final_managed_window.rect);
        apply_rect_animation_value(&mut final_managed_window, rect, rect_animation.mode);
    }

    final_managed_window.rect
}

fn managed_client_rect_for_state(
    tree: &DecorationTree,
    managed: &super::ManagedWindowState,
    fallback_client_rect: LogicalRect,
    scale: f64,
) -> Result<LogicalRect, DecorationEvaluationError> {
    timescope::scope!("ssd managed client rect for state");
    if !(managed.managed && managed.force_rect_size) {
        return Ok(fallback_client_rect);
    }

    let Some(desired_root) = managed.rect else {
        return Ok(fallback_client_rect);
    };
    let desired_root = managed_rect_snapshot_to_logical_rect(desired_root);
    if desired_root.width <= 0 || desired_root.height <= 0 {
        return Ok(fallback_client_rect);
    }

    managed_client_rect_for_root(tree, desired_root, scale)
}

/// Whether a probe of `managed_client_rect_for_state` that just ran against
/// the cached tree, managed rect and scale lets the refresh trust
/// `cached.client_rect` again (see `client_rect_potentially_stale`).
fn client_rect_probe_restores_coherence(
    cached: &WindowDecorationState,
    layout_scale: f64,
    probed_client_rect: LogicalRect,
) -> bool {
    client_rect_probe_agrees(
        cached.client_rect_potentially_stale,
        cached.managed_window_animation_active,
        cached.layout_scale,
        layout_scale,
        cached.client_rect,
        probed_client_rect,
    )
}

fn client_rect_probe_agrees(
    stale: bool,
    animation_active: bool,
    cached_scale: f64,
    layout_scale: f64,
    cached_client_rect: LogicalRect,
    probed_client_rect: LogicalRect,
) -> bool {
    // A running animation moves the managed rect again before the next
    // refresh; a scale change relayouts first.
    stale
        && !animation_active
        && cached_scale == layout_scale
        && cached_client_rect == probed_client_rect
}

/// Whether `managed_client_rect_for_state` derives the rect from the
/// managed rect alone, without reading its fallback client rect.
fn managed_client_rect_ignores_fallback(managed: &super::ManagedWindowState) -> bool {
    managed.managed
        && managed.force_rect_size
        && managed.rect.is_some_and(|rect| {
            let rect = managed_rect_snapshot_to_logical_rect(rect);
            rect.width > 0 && rect.height > 0
        })
}

/// `managed_client_rect_for_state` for a managed rect that only moved: at the size the
/// cached layout was computed for, the probe lands on that layout's insets, so they are
/// taken from it instead of probing. A Rust rect animation moving every tile of a
/// scrolled workspace otherwise ran the probe loop for each of them on every frame.
fn managed_client_rect_for_move(cached: &WindowDecorationState) -> Option<LogicalRect> {
    if !managed_client_rect_ignores_fallback(&cached.managed_window) {
        return None;
    }
    let desired_root = managed_rect_snapshot_to_logical_rect(cached.managed_window.rect?);
    let current_root = cached.layout.root.rect;
    (desired_root.width == current_root.width && desired_root.height == current_root.height).then(
        || managed_client_rect_from_current_insets(current_root, cached.client_rect, desired_root),
    )
}

fn managed_client_rect_from_current_insets(
    current_root: LogicalRect,
    current_client: LogicalRect,
    desired_root: LogicalRect,
) -> LogicalRect {
    let left = current_client.x - current_root.x;
    let top = current_client.y - current_root.y;
    let right = (current_root.x + current_root.width) - (current_client.x + current_client.width);
    let bottom =
        (current_root.y + current_root.height) - (current_client.y + current_client.height);

    LogicalRect::new(
        desired_root.x + left,
        desired_root.y + top,
        (desired_root.width - left - right).max(1),
        (desired_root.height - top - bottom).max(1),
    )
}

fn managed_client_rect_for_root(
    tree: &DecorationTree,
    desired_root: LogicalRect,
    scale: f64,
) -> Result<LogicalRect, DecorationEvaluationError> {
    timescope::scope!("ssd managed client rect for root");
    let mut client_width = desired_root.width.max(1);
    let mut client_height = desired_root.height.max(1);

    for _ in 0..4 {
        timescope::scope!("ssd managed client rect probe iteration");
        let probe_layout = {
            timescope::scope!("ssd managed client rect probe layout");
            tree.layout_for_client_with_scale(
                LogicalRect::new(0, 0, client_width, client_height),
                scale,
            )
            .map_err(super::DecorationEvaluationError::Layout)?
        };
        let node_geometry = {
            timescope::scope!("ssd managed client rect probe node geometry");
            build_node_geometry_map(&probe_layout)
        };
        let content_clip = {
            timescope::scope!("ssd managed client rect probe content clip");
            let Some(content_clip) = content_clip_for_layout(tree, &probe_layout, &node_geometry)
            else {
                return Ok(desired_root);
            };
            content_clip
        };

        let (left, top, next_width, next_height) = {
            timescope::scope!("ssd managed client rect probe insets");
            let left = content_clip.rect.loc.x - probe_layout.root.rect.x;
            let top = content_clip.rect.loc.y - probe_layout.root.rect.y;
            let right = (probe_layout.root.rect.x + probe_layout.root.rect.width)
                - (content_clip.rect.loc.x + content_clip.rect.size.w);
            let bottom = (probe_layout.root.rect.y + probe_layout.root.rect.height)
                - (content_clip.rect.loc.y + content_clip.rect.size.h);
            let next_width = (desired_root.width - left - right).max(1);
            let next_height = (desired_root.height - top - bottom).max(1);
            (left, top, next_width, next_height)
        };

        if next_width == client_width && next_height == client_height {
            return Ok(LogicalRect::new(
                desired_root.x + left,
                desired_root.y + top,
                client_width,
                client_height,
            ));
        }

        client_width = next_width;
        client_height = next_height;
    }

    let final_layout = {
        timescope::scope!("ssd managed client rect final layout");
        tree.layout_for_client_with_scale(
            LogicalRect::new(0, 0, client_width, client_height),
            scale,
        )
        .map_err(super::DecorationEvaluationError::Layout)?
    };
    let node_geometry = {
        timescope::scope!("ssd managed client rect final node geometry");
        build_node_geometry_map(&final_layout)
    };
    let content_clip = {
        timescope::scope!("ssd managed client rect final content clip");
        let Some(content_clip) = content_clip_for_layout(tree, &final_layout, &node_geometry)
        else {
            return Ok(desired_root);
        };
        content_clip
    };
    let (left, top) = {
        timescope::scope!("ssd managed client rect final insets");
        (
            content_clip.rect.loc.x - final_layout.root.rect.x,
            content_clip.rect.loc.y - final_layout.root.rect.y,
        )
    };

    Ok(LogicalRect::new(
        desired_root.x + left,
        desired_root.y + top,
        client_width,
        client_height,
    ))
}

fn fit_children_inner_clip_resolved(
    node: &super::ComputedDecorationNode,
) -> Option<crate::ssd::ResolvedDecorationClip> {
    if !matches!(
        node.style.effective_border_fit(&node.kind),
        super::BorderFit::FitChildren
    ) {
        return None;
    }
    if !node.style.has_border() {
        return None;
    }
    Some(
        node.resolved_effective_clip
            .unwrap_or(crate::ssd::ResolvedDecorationClip {
                rect: node.resolved_content_rect,
                radius: (node.resolved_border_radius - node.resolved_border_width)
                    .max(crate::ssd::ResolvedLayoutValue::ZERO),
            }),
    )
}

fn precise_rect_from_logical(rect: LogicalRect) -> PreciseLogicalRect {
    PreciseLogicalRect {
        x: rect.x as f32,
        y: rect.y as f32,
        width: rect.width as f32,
        height: rect.height as f32,
    }
}

fn node_child_mask_resolved(
    node: &super::ComputedDecorationNode,
) -> Option<crate::ssd::ResolvedDecorationClip> {
    fit_children_inner_clip_resolved(node).or(node.resolved_effective_clip)
}

fn slot_content_clip_for_node(
    node: &super::ComputedDecorationNode,
    nearest_border: Option<(i32, i32)>,
    nearest_mask: Option<crate::ssd::ResolvedDecorationClip>,
    _node_geometry: &impl NodeGeometryLookup,
) -> Option<ContentClip> {
    let next_border = if matches!(node.kind, super::DecorationNodeKind::WindowBorder) {
        node.style
            .border
            .map(|_| {
                (
                    node.frame.logical_len_rounded(node.resolved_border_width),
                    node.frame.logical_len_rounded(node.resolved_border_radius),
                )
            })
            .or(nearest_border)
    } else {
        nearest_border
    };
    // WindowSlot is only the placement anchor for the client surface. Its own
    // style must never manufacture a clip: clipping belongs to an ancestor
    // SSD container and reaches the slot through `nearest_mask`.
    let next_mask = if matches!(node.kind, super::DecorationNodeKind::WindowSlot) {
        nearest_mask
    } else {
        node_child_mask_resolved(node).or(nearest_mask)
    };

    if matches!(node.kind, super::DecorationNodeKind::WindowSlot) {
        let (_border_width, _border_radius) = next_border.unwrap_or((0, 0));
        let inherited_clip =
            node.resolved_effective_clip
                .unwrap_or(crate::ssd::ResolvedDecorationClip {
                    rect: node.resolved_rect,
                    radius: (node.resolved_border_radius - node.resolved_border_width)
                        .max(crate::ssd::ResolvedLayoutValue::ZERO),
                });
        let frame = node.frame;
        let slot_rect = node.resolved_rect;
        let slot_rect_precise = frame.precise_rect(slot_rect);
        let clips_surface = next_mask.is_some();
        let mask = next_mask.unwrap_or(inherited_clip);
        let mask_rect_precise = frame.precise_rect(mask.rect);
        let corner_radii_precise = if mask.radius.raw() > 0 {
            [frame.logical_len(mask.radius).max(0.0); 4]
        } else {
            [0.0; 4]
        };
        let corner_radii = corner_radii_precise.map(|radius| radius.round().max(0.0) as i32);
        let slot_logical = frame.logical_rect(slot_rect);
        let mask_logical = frame.logical_rect(mask.rect);
        return Some(ContentClip {
            rect: Rectangle::new(
                Point::from((slot_logical.x, slot_logical.y)),
                (slot_logical.width, slot_logical.height).into(),
            ),
            rect_precise: slot_rect_precise,
            clips_surface,
            mask_rect: Rectangle::new(
                Point::from((mask_logical.x, mask_logical.y)),
                (mask_logical.width, mask_logical.height).into(),
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

    node.children
        .iter()
        .find_map(|child| slot_content_clip_for_node(child, next_border, next_mask, _node_geometry))
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
        self.layout_for_client_with_subpixel(client_rect, scale, Default::default())
    }

    /// `layout_for_client_with_scale` for a root whose managed rect has
    /// sub-logical-pixel edges (`subpixel`): the root's physical size becomes
    /// `round((width + right - left) · scale)`, so a rect animation resizes the
    /// window in physical-pixel steps. The client slot absorbs the difference;
    /// the logical rects keep the integer size.
    pub(crate) fn layout_for_client_with_subpixel(
        &self,
        client_rect: LogicalRect,
        scale: f64,
        subpixel: crate::backend::visual::RootSubpixelEdges,
    ) -> Result<ComputedDecorationTree, super::DecorationLayoutError> {
        let initial = self.layout_with_window_slot_size(
            LogicalRect::new(0, 0, client_rect.width, client_rect.height),
            Some((client_rect.width, client_rect.height)),
            scale,
            (0, 0),
        )?;
        let slot = initial
            .window_slot_rect()
            .ok_or(super::DecorationLayoutError::MissingComputedWindowSlot)?;
        let initial_bounds = initial.root.rect;

        let extra_left = slot.x - initial_bounds.x;
        let extra_top = slot.y - initial_bounds.y;
        let extra_right = (initial_bounds.x + initial_bounds.width) - (slot.x + slot.width);
        let extra_bottom = (initial_bounds.y + initial_bounds.height) - (slot.y + slot.height);

        let root_width = client_rect.width + extra_left + extra_right;
        let root_height = client_rect.height + extra_top + extra_bottom;
        let desired = self.layout_with_window_slot_size(
            LogicalRect::new(0, 0, root_width, root_height),
            Some((client_rect.width, client_rect.height)),
            scale,
            root_subpixel_extra_px(root_width, root_height, subpixel, scale),
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
        root_extra_px: (i32, i32),
    ) -> Result<ComputedDecorationTree, super::DecorationLayoutError> {
        self.validate()?;

        let mut root = super::layout_node_with_scale(
            &self.root,
            bounds,
            window_slot_size,
            scale,
            root_extra_px,
        )?;
        root.sync_root_bounds();
        if root.window_slot_rect().is_none() {
            return Err(super::DecorationLayoutError::MissingComputedWindowSlot);
        }

        Ok(ComputedDecorationTree { root })
    }
}

/// Physical pixels a root of integer logical size `width × height` gains from
/// its managed rect's sub-logical-pixel edges.
fn root_subpixel_extra_px(
    width: i32,
    height: i32,
    subpixel: crate::backend::visual::RootSubpixelEdges,
    scale: f64,
) -> (i32, i32) {
    let scale = scale.abs().max(0.0001);
    let extra = |len: i32, start: f64, end: f64| {
        super::round_half_up((len as f64 + end - start) * scale)
            - super::round_half_up(len as f64 * scale)
    };
    (
        extra(width, subpixel.left, subpixel.right),
        extra(height, subpixel.top, subpixel.bottom),
    )
}

impl ComputedDecorationTree {
    pub fn translated(&self, dx: i32, dy: i32) -> Self {
        Self {
            root: self.root.translated(dx, dy),
        }
    }
}

impl super::ComputedDecorationNode {
    /// Moves the node by whole logical pixels. The physical-pixel layout is
    /// root-local, so only the frame origin and the logical views shift.
    fn translated(&self, dx: i32, dy: i32) -> Self {
        Self {
            stable_id: self.stable_id.clone(),
            interaction: self.interaction.clone(),
            window_border_interaction: self.window_border_interaction,
            kind: self.kind.clone(),
            style: self.style.clone(),
            rect: LogicalRect::new(
                self.rect.x + dx,
                self.rect.y + dy,
                self.rect.width,
                self.rect.height,
            ),
            resolved_rect: self.resolved_rect,
            resolved_content_rect: self.resolved_content_rect,
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
            resolved_effective_clip: self.resolved_effective_clip,
            frame: self.frame.translated(dx as f64, dy as f64),
            transform_scale: self.transform_scale,
            children: self
                .children
                .iter()
                .map(|child| child.translated(dx, dy))
                .collect(),
        }
    }
}

fn translate_cached_decoration_position(
    decoration: &mut WindowDecorationState,
    dx: i32,
    dy: i32,
    client_rect: LogicalRect,
) {
    decoration.layout = decoration.layout.translated(dx, dy);
    decoration.client_rect = client_rect;
    decoration.snapshot.position = WindowPositionSnapshot::from(client_rect);
    decoration.content_clip = decoration
        .content_clip
        .map(|clip| translate_content_clip(clip, dx, dy));

    // Paint geometry is root-local; only the logical bounds move.
    for buffer in &mut decoration.buffers {
        buffer.rect = translate_logical_rect(buffer.rect, dx, dy);
    }

    for buffer in &mut decoration.shader_buffers {
        buffer.rect = translate_logical_rect(buffer.rect, dx, dy);
        buffer.rect_precise = buffer
            .rect_precise
            .map(|rect| translate_precise_rect(rect, dx, dy));
        buffer.clip_rect = buffer
            .clip_rect
            .map(|rect| translate_logical_rect(rect, dx, dy));
        buffer.clip_rect_precise = buffer
            .clip_rect_precise
            .map(|rect| translate_precise_rect(rect, dx, dy));
    }

    for buffer in &mut decoration.text_buffers {
        buffer.rect = translate_logical_rect(buffer.rect, dx, dy);
        buffer.rect_precise = buffer
            .rect_precise
            .map(|rect| translate_precise_rect(rect, dx, dy));
        buffer.clip_rect = buffer
            .clip_rect
            .map(|rect| translate_logical_rect(rect, dx, dy));
        buffer.clip_rect_precise = buffer
            .clip_rect_precise
            .map(|rect| translate_precise_rect(rect, dx, dy));
    }

    for buffer in &mut decoration.icon_buffers {
        buffer.rect = translate_logical_rect(buffer.rect, dx, dy);
        buffer.rect_precise = buffer
            .rect_precise
            .map(|rect| translate_precise_rect(rect, dx, dy));
        buffer.clip_rect = buffer
            .clip_rect
            .map(|rect| translate_logical_rect(rect, dx, dy));
        buffer.clip_rect_precise = buffer
            .clip_rect_precise
            .map(|rect| translate_precise_rect(rect, dx, dy));
    }
}

fn translate_content_clip(clip: ContentClip, dx: i32, dy: i32) -> ContentClip {
    ContentClip {
        rect: translate_smithay_rect(clip.rect, dx, dy),
        rect_precise: translate_precise_rect(clip.rect_precise, dx, dy),
        clips_surface: clip.clips_surface,
        mask_rect: translate_smithay_rect(clip.mask_rect, dx, dy),
        mask_rect_precise: translate_precise_rect(clip.mask_rect_precise, dx, dy),
        ..clip
    }
}

fn translate_smithay_rect(
    rect: Rectangle<i32, Logical>,
    dx: i32,
    dy: i32,
) -> Rectangle<i32, Logical> {
    Rectangle::new(Point::from((rect.loc.x + dx, rect.loc.y + dy)), rect.size)
}

fn translate_logical_rect(rect: LogicalRect, dx: i32, dy: i32) -> LogicalRect {
    LogicalRect::new(rect.x + dx, rect.y + dy, rect.width, rect.height)
}

fn translate_precise_rect(rect: PreciseLogicalRect, dx: i32, dy: i32) -> PreciseLogicalRect {
    PreciseLogicalRect {
        x: rect.x + dx as f32,
        y: rect.y + dy as f32,
        width: rect.width,
        height: rect.height,
    }
}

/// The paint items of a laid-out tree, in render order (front first).
#[cfg(test)]
pub(crate) fn paint_buffers_for_layout(
    layout: &ComputedDecorationTree,
) -> Vec<CachedDecorationBuffer> {
    let order_map = build_render_order_map(layout);
    let mut buffers = build_cached_buffers(layout, &order_map);
    buffers.sort_by_key(|buffer| buffer.order);
    buffers
}

fn build_cached_buffers(
    layout: &ComputedDecorationTree,
    order_map: &std::collections::HashMap<String, usize>,
) -> Vec<CachedDecorationBuffer> {
    let node_geometry = build_node_geometry_map(layout);
    let (buffers, _) = build_cached_buffers_and_shaders(layout, order_map, None, &node_geometry);
    buffers
}

fn build_shader_buffers(
    layout: &ComputedDecorationTree,
    order_map: &std::collections::HashMap<String, usize>,
) -> Vec<CachedShaderEffect> {
    let node_geometry = build_node_geometry_map(layout);
    let (_, buffers) = build_cached_buffers_and_shaders(layout, order_map, None, &node_geometry);
    buffers
}

fn build_cached_buffers_and_shaders(
    layout: &ComputedDecorationTree,
    order_map: &std::collections::HashMap<String, usize>,
    dirty_node_ids: Option<&std::collections::HashSet<&str>>,
    node_geometry: &impl NodeGeometryLookup,
) -> (Vec<CachedDecorationBuffer>, Vec<CachedShaderEffect>) {
    let mut buffers = Vec::new();
    let mut shader_buffers = Vec::new();
    collect_cached_buffers(
        &layout.root,
        "root".to_string(),
        super::paint::PaintClipState::default(),
        false,
        order_map,
        dirty_node_ids,
        node_geometry,
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

fn build_text_buffers_with_fallback(
    layout: &ComputedDecorationTree,
    order_map: &std::collections::HashMap<String, usize>,
    raster_scale: f64,
    rasterizer: &mut crate::backend::text::TextRasterizer,
    previous: &[CachedDecorationLabel],
) -> Vec<CachedDecorationLabel> {
    let node_geometry = build_node_geometry_map(layout);
    build_text_buffers_with_node_geometry(
        layout,
        order_map,
        &node_geometry,
        raster_scale,
        rasterizer,
        previous,
    )
}

fn build_text_buffers_with_node_geometry(
    layout: &ComputedDecorationTree,
    order_map: &std::collections::HashMap<String, usize>,
    node_geometry: &impl NodeGeometryLookup,
    raster_scale: f64,
    rasterizer: &mut crate::backend::text::TextRasterizer,
    previous: &[CachedDecorationLabel],
) -> Vec<CachedDecorationLabel> {
    let mut buffers = Vec::new();
    collect_text_buffers(
        &layout.root,
        "root".into(),
        order_map,
        None,
        node_geometry,
        raster_scale,
        rasterizer,
        previous,
        &mut buffers,
    );
    buffers
}

fn build_icon_buffers(
    layout: &ComputedDecorationTree,
    order_map: &std::collections::HashMap<String, usize>,
    raster_scale: f64,
    snapshot: &WaylandWindowSnapshot,
    rasterizer: &mut crate::backend::icon::IconRasterizer,
) -> Vec<CachedDecorationIcon> {
    let node_geometry = build_node_geometry_map(layout);
    build_icon_buffers_with_node_geometry(
        layout,
        order_map,
        &node_geometry,
        raster_scale,
        snapshot.into(),
        rasterizer,
    )
}

/// Where `<AppIcon>` takes its picture from.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct IconSource<'a> {
    pub icon: Option<&'a super::WindowIconSnapshot>,
    pub app_id: Option<&'a str>,
}

impl<'a> From<&'a WaylandWindowSnapshot> for IconSource<'a> {
    fn from(snapshot: &'a WaylandWindowSnapshot) -> Self {
        Self {
            icon: snapshot.icon.as_ref(),
            app_id: snapshot.app_id.as_deref(),
        }
    }
}

/// A decoration tree drawn for no window (a drag view, see `crate::dnd`),
/// laid out at one scale, with the buffers to draw it.
#[derive(Debug)]
pub(crate) struct StandaloneDecoration {
    pub layout: ComputedDecorationTree,
    pub buffers: Vec<CachedDecorationBuffer>,
    /// `<ShaderEffect>` nodes.
    pub shader_buffers: Vec<CachedShaderEffect>,
    pub shader_cache:
        std::collections::HashMap<String, crate::backend::shader_effect::ShaderEffectElementState>,
    pub text_buffers: Vec<CachedDecorationLabel>,
    pub icon_buffers: Vec<CachedDecorationIcon>,
    pub paint_cache: std::collections::HashMap<String, PaintElementState>,
}

impl StandaloneDecoration {
    /// Lay `tree` out at `scale` and rasterize its labels and images.
    /// `previous` lends its paint element states (stable element ids, so an
    /// unchanged paint item is not redrawn) and its labels (shown until a new
    /// label is rasterized).
    pub(crate) fn build(
        tree: &DecorationTree,
        slot_size: Option<(i32, i32)>,
        scale: f64,
        source: IconSource<'_>,
        text_rasterizer: &mut crate::backend::text::TextRasterizer,
        icon_rasterizer: &mut crate::backend::icon::IconRasterizer,
        previous: Option<Self>,
    ) -> Result<Self, super::DecorationLayoutError> {
        let layout = tree.layout_standalone(slot_size, scale)?;
        let order_map = build_render_order_map(&layout);
        let buffers = build_cached_buffers(&layout, &order_map);
        let shader_buffers = build_shader_buffers(&layout, &order_map);
        let text_buffers = build_text_buffers_with_fallback(
            &layout,
            &order_map,
            scale,
            text_rasterizer,
            previous
                .as_ref()
                .map(|previous| previous.text_buffers.as_slice())
                .unwrap_or_default(),
        );
        let node_geometry = build_node_geometry_map(&layout);
        let icon_buffers = build_icon_buffers_with_node_geometry(
            &layout,
            &order_map,
            &node_geometry,
            scale,
            source,
            icon_rasterizer,
        );
        let (paint_cache, shader_cache) = previous
            .map(|previous| (previous.paint_cache, previous.shader_cache))
            .unwrap_or_default();
        Ok(Self {
            layout,
            buffers,
            shader_buffers,
            shader_cache,
            text_buffers,
            icon_buffers,
            paint_cache,
        })
    }
}

fn build_icon_buffers_with_node_geometry(
    layout: &ComputedDecorationTree,
    order_map: &std::collections::HashMap<String, usize>,
    node_geometry: &impl NodeGeometryLookup,
    raster_scale: f64,
    source: IconSource<'_>,
    rasterizer: &mut crate::backend::icon::IconRasterizer,
) -> Vec<CachedDecorationIcon> {
    let mut buffers = Vec::new();
    collect_icon_buffers(
        &layout.root,
        "root".into(),
        order_map,
        None,
        node_geometry,
        raster_scale,
        source,
        rasterizer,
        &mut buffers,
    );
    buffers
}

fn retarget_text_buffers_with_node_geometry(
    layout: &ComputedDecorationTree,
    order_map: &std::collections::HashMap<String, usize>,
    node_geometry: &impl NodeGeometryLookup,
    previous: &[CachedDecorationLabel],
) -> Vec<CachedDecorationLabel> {
    let mut buffers = Vec::new();
    collect_retargeted_text_buffers(
        &layout.root,
        "root".into(),
        order_map,
        node_geometry,
        previous,
        &mut buffers,
    );
    buffers
}

fn collect_retargeted_text_buffers(
    node: &super::ComputedDecorationNode,
    path: String,
    order_map: &std::collections::HashMap<String, usize>,
    node_geometry: &impl NodeGeometryLookup,
    previous: &[CachedDecorationLabel],
    buffers: &mut Vec<CachedDecorationLabel>,
) {
    if node.style.visible == Some(false) {
        return;
    }

    for_each_paint_ordered_child(node, |index, child| {
        collect_retargeted_text_buffers(
            child,
            format!("{path}/child-{index}"),
            order_map,
            node_geometry,
            previous,
            buffers,
        );
    });

    let super::DecorationNodeKind::Label(label) = &node.kind else {
        return;
    };

    let stable_key = format!("{path}:label");
    let Some(mut buffer) = fallback_text_buffer(previous, node.stable_id.as_deref(), &stable_key)
    else {
        return;
    };
    let shared_geometry = node
        .stable_id
        .as_deref()
        .and_then(|stable_id| node_geometry.node_geometry(stable_id));
    let rect_precise = shared_geometry
        .map(|geometry| geometry.rect_precise)
        .unwrap_or_else(|| node.frame.precise_rect(node.resolved_rect));
    let clip_rect_precise = shared_geometry
        .and_then(|geometry| geometry.clip_rect_precise)
        .or_else(|| {
            node.resolved_effective_clip
                .map(|clip| node.frame.precise_rect(clip.rect))
        });
    let clip_radius_precise = node
        .resolved_effective_clip
        .map(|clip| node.frame.logical_len(clip.radius).max(0.0));

    buffer.owner_node_id = node.stable_id.clone();
    buffer.stable_key = stable_key.clone();
    buffer.order = *order_map.get(&stable_key).unwrap_or(&usize::MAX);
    buffer.rect = node.rect;
    buffer.rect_precise = Some(rect_precise);
    buffer.clip_rect = node.effective_clip.map(|clip| clip.rect);
    buffer.clip_radius = node.effective_clip.map(|clip| clip.radius).unwrap_or(0);
    buffer.clip_rect_precise = clip_rect_precise;
    buffer.clip_radius_precise = clip_radius_precise;
    buffer.text = label.text.clone();
    buffer.color = node
        .style
        .color
        .unwrap_or(super::Color::WHITE)
        .with_opacity(node.style.opacity);
    buffers.push(buffer);
}

fn retarget_icon_buffers_with_node_geometry(
    layout: &ComputedDecorationTree,
    order_map: &std::collections::HashMap<String, usize>,
    node_geometry: &impl NodeGeometryLookup,
    snapshot: &WaylandWindowSnapshot,
    previous: &[CachedDecorationIcon],
) -> Vec<CachedDecorationIcon> {
    let mut buffers = Vec::new();
    collect_retargeted_icon_buffers(
        &layout.root,
        "root".into(),
        order_map,
        node_geometry,
        snapshot,
        previous,
        &mut buffers,
    );
    buffers
}

fn collect_retargeted_icon_buffers(
    node: &super::ComputedDecorationNode,
    path: String,
    order_map: &std::collections::HashMap<String, usize>,
    node_geometry: &impl NodeGeometryLookup,
    snapshot: &WaylandWindowSnapshot,
    previous: &[CachedDecorationIcon],
    buffers: &mut Vec<CachedDecorationIcon>,
) {
    if node.style.visible == Some(false) {
        return;
    }

    for_each_paint_ordered_child(node, |index, child| {
        collect_retargeted_icon_buffers(
            child,
            format!("{path}/child-{index}"),
            order_map,
            node_geometry,
            snapshot,
            previous,
            buffers,
        );
    });

    if !matches!(
        node.kind,
        super::DecorationNodeKind::AppIcon(_) | super::DecorationNodeKind::Image(_)
    ) {
        return;
    }

    let stable_key = format!("{path}:icon");
    let Some(mut buffer) = fallback_icon_buffer(previous, node.stable_id.as_deref(), &stable_key)
    else {
        return;
    };
    let shared_geometry = node
        .stable_id
        .as_deref()
        .and_then(|stable_id| node_geometry.node_geometry(stable_id));
    let rect_precise = shared_geometry
        .map(|geometry| geometry.rect_precise)
        .unwrap_or_else(|| node.frame.precise_rect(node.resolved_rect));
    let clip_rect_precise = shared_geometry
        .and_then(|geometry| geometry.clip_rect_precise)
        .or_else(|| {
            node.resolved_effective_clip
                .map(|clip| node.frame.precise_rect(clip.rect))
        });
    let clip_radius_precise = node
        .resolved_effective_clip
        .map(|clip| node.frame.logical_len(clip.radius).max(0.0));

    buffer.owner_node_id = node.stable_id.clone();
    buffer.stable_key = stable_key.clone();
    buffer.order = *order_map.get(&stable_key).unwrap_or(&usize::MAX);
    buffer.rect = node.rect;
    buffer.rect_precise = Some(rect_precise);
    buffer.clip_rect = node.effective_clip.map(|clip| clip.rect);
    buffer.clip_radius = node.effective_clip.map(|clip| clip.radius).unwrap_or(0);
    buffer.clip_rect_precise = clip_rect_precise;
    buffer.clip_radius_precise = clip_radius_precise;

    if matches!(&node.kind, super::DecorationNodeKind::AppIcon(icon) if icon.fallback.is_none())
        && snapshot.icon.is_none()
        && snapshot.app_id.is_none()
    {
        return;
    }
    buffers.push(buffer);
}

fn fallback_icon_buffer(
    previous: &[CachedDecorationIcon],
    owner_node_id: Option<&str>,
    stable_key: &str,
) -> Option<CachedDecorationIcon> {
    if let Some(owner_node_id) = owner_node_id
        && let Some(buffer) = previous
            .iter()
            .find(|buffer| buffer.owner_node_id.as_deref() == Some(owner_node_id))
    {
        return Some(buffer.clone());
    }

    previous
        .iter()
        .find(|buffer| buffer.stable_key == stable_key)
        .cloned()
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
    let node_geometry = build_node_geometry_map(layout);
    build_cached_buffers_and_shaders(layout, order_map, Some(&dirty_node_ids), &node_geometry)
}

fn rebuild_partial_text_buffers_with_fallback(
    layout: &ComputedDecorationTree,
    order_map: &std::collections::HashMap<String, usize>,
    dirty_node_ids: &[String],
    raster_scale: f64,
    rasterizer: &mut crate::backend::text::TextRasterizer,
    previous: &[CachedDecorationLabel],
) -> Vec<CachedDecorationLabel> {
    let dirty_node_ids = dirty_node_ids
        .iter()
        .map(String::as_str)
        .collect::<std::collections::HashSet<_>>();
    let node_geometry = build_node_geometry_map(layout);
    let mut buffers = Vec::new();
    collect_text_buffers(
        &layout.root,
        "root".into(),
        order_map,
        Some(&dirty_node_ids),
        &node_geometry,
        raster_scale,
        rasterizer,
        previous,
        &mut buffers,
    );
    buffers
}

fn rebuild_partial_icon_buffers(
    layout: &ComputedDecorationTree,
    order_map: &std::collections::HashMap<String, usize>,
    dirty_node_ids: &[String],
    raster_scale: f64,
    snapshot: &WaylandWindowSnapshot,
    rasterizer: &mut crate::backend::icon::IconRasterizer,
) -> Vec<CachedDecorationIcon> {
    let dirty_node_ids = dirty_node_ids
        .iter()
        .map(String::as_str)
        .collect::<std::collections::HashSet<_>>();
    let node_geometry = build_node_geometry_map(layout);
    let mut buffers = Vec::new();
    collect_icon_buffers(
        &layout.root,
        "root".into(),
        order_map,
        Some(&dirty_node_ids),
        &node_geometry,
        raster_scale,
        snapshot.into(),
        rasterizer,
        &mut buffers,
    );
    buffers
}

fn merge_cached_buffers(
    previous: &[CachedDecorationBuffer],
    rebuilt: Vec<CachedDecorationBuffer>,
    dirty_node_ids: &[String],
    order_map: &std::collections::HashMap<String, usize>,
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
                .is_none_or(|node_id| !node_id_matches_dirty_scope(node_id, &dirty_node_ids))
        })
        // The kept items get the current paint order: a node shown or hidden
        // elsewhere (an opening popup) renumbers the ones after it.
        .map(|item| {
            let mut item = item.clone();
            if let Some(order) = order_map.get(&item.stable_key) {
                item.order = *order;
            }
            item
        })
        .collect::<Vec<_>>();
    merged.extend(rebuilt);
    merged.sort_by_key(|item| item.order);
    merged
}

fn merge_shader_buffers(
    previous: &[CachedShaderEffect],
    rebuilt: Vec<CachedShaderEffect>,
    dirty_node_ids: &[String],
    order_map: &std::collections::HashMap<String, usize>,
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
                .is_none_or(|node_id| !node_id_matches_dirty_scope(node_id, &dirty_node_ids))
        })
        // The kept items get the current paint order: a node shown or hidden
        // elsewhere (an opening popup) renumbers the ones after it.
        .map(|item| {
            let mut item = item.clone();
            if let Some(order) = order_map.get(&item.stable_key) {
                item.order = *order;
            }
            item
        })
        .collect::<Vec<_>>();
    merged.extend(rebuilt);
    merged.sort_by_key(|item| item.order);
    merged
}

fn merge_text_buffers(
    previous: &[CachedDecorationLabel],
    rebuilt: Vec<CachedDecorationLabel>,
    dirty_node_ids: &[String],
    order_map: &std::collections::HashMap<String, usize>,
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
                .is_none_or(|node_id| !node_id_matches_dirty_scope(node_id, &dirty_node_ids))
        })
        // The kept items get the current paint order: a node shown or hidden
        // elsewhere (an opening popup) renumbers the ones after it.
        .map(|item| {
            let mut item = item.clone();
            if let Some(order) = order_map.get(&item.stable_key) {
                item.order = *order;
            }
            item
        })
        .collect::<Vec<_>>();
    merged.extend(rebuilt);
    merged.sort_by_key(|item| item.order);
    merged
}

fn merge_icon_buffers(
    previous: &[CachedDecorationIcon],
    rebuilt: Vec<CachedDecorationIcon>,
    dirty_node_ids: &[String],
    order_map: &std::collections::HashMap<String, usize>,
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
                .is_none_or(|node_id| !node_id_matches_dirty_scope(node_id, &dirty_node_ids))
        })
        // The kept items get the current paint order: a node shown or hidden
        // elsewhere (an opening popup) renumbers the ones after it.
        .map(|item| {
            let mut item = item.clone();
            if let Some(order) = order_map.get(&item.stable_key) {
                item.order = *order;
            }
            item
        })
        .collect::<Vec<_>>();
    merged.extend(rebuilt);
    merged.sort_by_key(|item| item.order);
    merged
}

/// Node ids are paths: the TypeScript runtime joins segments with `.`
/// (`root.Box#title`), the Rust SDK with `/` (`w1/0/2`).
fn is_descendant_node_id(node_id: &str, ancestor_id: &str) -> bool {
    node_id.len() > ancestor_id.len()
        && node_id.starts_with(ancestor_id)
        && matches!(node_id.as_bytes()[ancestor_id.len()], b'.' | b'/')
}

fn node_id_matches_dirty_scope(
    node_id: &str,
    dirty_node_ids: &std::collections::HashSet<&str>,
) -> bool {
    dirty_node_ids
        .iter()
        .any(|dirty_id| node_id == *dirty_id || is_descendant_node_id(node_id, dirty_id))
}

fn paint_ordered_children(
    node: &super::ComputedDecorationNode,
) -> Vec<(usize, &super::ComputedDecorationNode)> {
    let mut children = node.children.iter().enumerate().collect::<Vec<_>>();
    children.sort_by(|(left_index, left), (right_index, right)| {
        right
            .style
            .z_index
            .unwrap_or(0)
            .cmp(&left.style.z_index.unwrap_or(0))
            .then_with(|| right_index.cmp(left_index))
    });
    children
}

fn for_each_paint_ordered_child(
    node: &super::ComputedDecorationNode,
    mut f: impl FnMut(usize, &super::ComputedDecorationNode),
) {
    if node
        .children
        .iter()
        .all(|child| child.style.z_index.unwrap_or(0) == 0)
    {
        for (index, child) in node.children.iter().enumerate().rev() {
            f(index, child);
        }
        return;
    }

    for (index, child) in paint_ordered_children(node) {
        f(index, child);
    }
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
    if matches!(node.kind, super::DecorationNodeKind::WindowSlot) {
        return;
    }

    let mut push = |key: String, order: &mut usize| {
        map.insert(format!("{path}:{key}"), *order);
        *order += 1;
    };

    if super::paint::has_overlay(node) {
        push(super::paint::PaintSlot::Overlay.key(), order);
    }

    match &node.kind {
        super::DecorationNodeKind::Label(_) => push("label".into(), order),
        super::DecorationNodeKind::AppIcon(_) | super::DecorationNodeKind::Image(_) => {
            push("icon".into(), order)
        }
        _ => {}
    }

    for_each_paint_ordered_child(node, |index, child| {
        collect_render_orders(child, format!("{path}/child-{index}"), order, map);
    });

    let mut push = |key: String, order: &mut usize| {
        map.insert(format!("{path}:{key}"), *order);
        *order += 1;
    };
    let slots = super::paint::back_slots(node);
    let effect_index = matches!(node.kind, super::DecorationNodeKind::ShaderEffect(_))
        .then(|| super::paint::effect_slot_index(node, &slots));
    for (index, slot) in slots.iter().enumerate() {
        if effect_index == Some(index) {
            push("shader".into(), order);
        }
        push(slot.key(), order);
    }
    if effect_index == Some(slots.len()) {
        push("shader".into(), order);
    }
}

fn collect_cached_buffers(
    node: &super::ComputedDecorationNode,
    path: String,
    clips: super::paint::PaintClipState,
    in_popup: bool,
    order_map: &std::collections::HashMap<String, usize>,
    dirty_node_ids: Option<&std::collections::HashSet<&str>>,
    node_geometry: &impl NodeGeometryLookup,
    buffers: &mut Vec<CachedDecorationBuffer>,
    shader_buffers: &mut Vec<CachedShaderEffect>,
) {
    if node.style.visible == Some(false) {
        return;
    }
    if matches!(node.kind, super::DecorationNodeKind::WindowSlot) {
        return;
    }
    // A popup is drawn outside the window: its ancestors' clips do not apply,
    // and its pass has no effect pipeline (backdrops would sample the window
    // stream it is not part of), so `<ShaderEffect>`s inside it draw only
    // their paint and children.
    let is_popup = matches!(node.kind, super::DecorationNodeKind::Popup(_));
    let clips = if is_popup {
        super::paint::PaintClipState::default()
    } else {
        clips
    };
    let in_popup = in_popup || is_popup;

    let include_node = dirty_node_ids.is_none_or(|dirty_node_ids| {
        node.stable_id
            .as_deref()
            .is_some_and(|stable_id| node_id_matches_dirty_scope(stable_id, dirty_node_ids))
    });

    if include_node {
        let geometry = super::paint::node_geometry(node, clips);
        let mut slots = super::paint::back_slots(node);
        if super::paint::has_overlay(node) {
            slots.push(super::paint::PaintSlot::Overlay);
        }
        for slot in slots {
            let stable_key = format!("{path}:{}", slot.key());
            let Some(item) = super::paint::paint_item(node, slot, geometry) else {
                continue;
            };
            buffers.push(CachedDecorationBuffer {
                owner_node_id: node.stable_id.clone(),
                order: *order_map.get(&stable_key).unwrap_or(&usize::MAX),
                rect: super::paint::item_logical_rect(node.frame, &item),
                stable_key,
                source_kind: slot.source_kind(),
                paint: item,
            });
        }

        if let super::DecorationNodeKind::ShaderEffect(effect) = &node.kind
            && !in_popup
        {
            // Effects keep their established clip rule: the inherited clip
            // when it is rounded, otherwise the nearest rounded ancestor.
            let effect_clip = clips
                .clip
                .filter(|clip| clip.radius.raw() > 0)
                .or(clips.nearest_rounded);
            shader_buffers.push(CachedShaderEffect {
                owner_node_id: node.stable_id.clone(),
                stable_key: format!("{path}:shader"),
                order: *order_map
                    .get(&format!("{path}:shader"))
                    .unwrap_or(&usize::MAX),
                rect: node.rect,
                rect_precise: Some(
                    node.stable_id
                        .as_deref()
                        .and_then(|stable_id| node_geometry.node_geometry(stable_id))
                        .map(|geometry| geometry.rect_precise)
                        .unwrap_or_else(|| node.frame.precise_rect(node.resolved_rect)),
                ),
                shader: effect.shader.clone(),
                clip_rect: effect_clip.map(|clip| node.frame.logical_rect(clip.rect)),
                clip_radius: effect_clip
                    .map(|clip| node.frame.logical_len_rounded(clip.radius).max(0))
                    .unwrap_or(0),
                clip_rect_precise: effect_clip.map(|clip| node.frame.precise_rect(clip.rect)),
                clip_radius_precise: effect_clip
                    .map(|clip| node.frame.logical_len(clip.radius).max(0.0)),
                node_shape: {
                    let geometry = super::paint::node_geometry(node, clips);
                    let node_rect = geometry.node;
                    crate::backend::shader_effect::NodeEffectShape {
                        layout_scale: node.frame.scale,
                        radius: geometry.radius,
                        border: geometry.border,
                        clip: effect_clip.map(|clip| {
                            let rect = super::paint::PxRect::from_resolved(clip.rect);
                            let radius = clip.radius.raw().max(0);
                            (
                                [rect.x - node_rect.x, rect.y - node_rect.y, rect.w, rect.h],
                                [radius; 4],
                            )
                        }),
                    }
                },
            });
        }
    }

    let child_clips = clips.for_children(node);
    for_each_paint_ordered_child(node, |index, child| {
        collect_cached_buffers(
            child,
            format!("{path}/child-{index}"),
            child_clips,
            in_popup,
            order_map,
            dirty_node_ids,
            node_geometry,
            buffers,
            shader_buffers,
        );
    });
}

fn collect_text_buffers(
    node: &super::ComputedDecorationNode,
    path: String,
    order_map: &std::collections::HashMap<String, usize>,
    dirty_node_ids: Option<&std::collections::HashSet<&str>>,
    node_geometry: &impl NodeGeometryLookup,
    raster_scale: f64,
    rasterizer: &mut crate::backend::text::TextRasterizer,
    previous: &[CachedDecorationLabel],
    buffers: &mut Vec<CachedDecorationLabel>,
) {
    if node.style.visible == Some(false) {
        return;
    }

    for_each_paint_ordered_child(node, |index, child| {
        collect_text_buffers(
            child,
            format!("{path}/child-{index}"),
            order_map,
            dirty_node_ids,
            node_geometry,
            raster_scale,
            rasterizer,
            previous,
            buffers,
        );
    });

    if dirty_node_ids.is_some_and(|dirty_node_ids| {
        !node
            .stable_id
            .as_deref()
            .is_some_and(|stable_id| node_id_matches_dirty_scope(stable_id, dirty_node_ids))
    }) {
        return;
    }

    let super::DecorationNodeKind::Label(label) = &node.kind else {
        return;
    };
    let shared_geometry = node
        .stable_id
        .as_deref()
        .and_then(|stable_id| node_geometry.node_geometry(stable_id));
    let color = node.style.color.unwrap_or(super::Color::WHITE);
    if color.a == 0 {
        return;
    }

    let spec = LabelSpec {
        rect: node.rect,
        rect_precise: Some(
            shared_geometry
                .map(|geometry| geometry.rect_precise)
                .unwrap_or_else(|| node.frame.precise_rect(node.resolved_rect)),
        ),
        text: label.text.clone(),
        color: color.with_opacity(node.style.opacity),
        font_size: node
            .style
            .font_size
            .unwrap_or(crate::ssd::DEFAULT_LABEL_FONT_SIZE) as f32,
        font_weight: node.style.font_weight.clone(),
        font_family: node.style.font_family.clone(),
        text_align: node.style.text_align.clone(),
        text_overflow: node.style.text_overflow.clone(),
        line_height: node.style.line_height.map(|value| value as f32),
        raster_scale,
    };

    let stable_key = format!("{path}:label");
    let order = *order_map.get(&stable_key).unwrap_or(&usize::MAX);
    let current_rect_precise = shared_geometry
        .map(|geometry| geometry.rect_precise)
        .unwrap_or_else(|| node.frame.precise_rect(node.resolved_rect));
    let current_clip_rect_precise = shared_geometry
        .and_then(|geometry| geometry.clip_rect_precise)
        .or_else(|| {
            node.resolved_effective_clip
                .map(|clip| node.frame.precise_rect(clip.rect))
        });
    let current_clip_radius_precise = node
        .resolved_effective_clip
        .map(|clip| node.frame.logical_len(clip.radius).max(0.0));

    if label_debug_enabled() {
        info!(
            path,
            stable_key = %stable_key,
            owner_node_id = ?node.stable_id,
            text = %label_preview(&spec.text),
            rect = %format_rect(spec.rect),
            rect_precise = ?spec.rect_precise,
            resolved_rect = %format_resolved_rect(node.resolved_rect),
            clip_rect = ?node.effective_clip.map(|clip| clip.rect),
            clip_rect_precise = ?current_clip_rect_precise,
            raster_scale,
            dirty_scoped = dirty_node_ids.is_some(),
            "label debug: collect label"
        );
    }

    if let Some(buffer) = rasterizer.render_label(&spec) {
        let mut buffer = buffer;
        buffer.owner_node_id = node.stable_id.clone();
        buffer.stable_key = stable_key;
        buffer.order = order;
        buffer.rect_precise = Some(current_rect_precise);
        buffer.clip_rect = node.effective_clip.map(|clip| clip.rect);
        buffer.clip_radius = node.effective_clip.map(|clip| clip.radius).unwrap_or(0);
        buffer.clip_rect_precise = current_clip_rect_precise;
        buffer.clip_radius_precise = current_clip_radius_precise;
        if label_debug_enabled() {
            info!(
                stable_key = %buffer.stable_key,
                owner_node_id = ?buffer.owner_node_id,
                text = %label_preview(&buffer.text),
                rect = %format_rect(buffer.rect),
                rect_precise = ?buffer.rect_precise,
                clip_rect = ?buffer.clip_rect,
                clip_rect_precise = ?buffer.clip_rect_precise,
                order = buffer.order,
                "label debug: rendered label buffer"
            );
        }
        buffers.push(buffer);
    } else if let Some(mut buffer) =
        fallback_text_buffer(previous, node.stable_id.as_deref(), &stable_key)
    {
        // Text rasterization is asynchronous. Keep the previous texture visible until the new
        // spec is ready, otherwise changing a title/label produces a one-frame blank flash.
        // The texture keeps the size it was rasterized at: a label sized by its text (a
        // window title) changes size together with the text, and drawing the old texture
        // into the new rect would stretch the old text for the frames until the new one
        // is ready.
        let (rect, rect_precise) = fallback_label_rect(
            &buffer,
            spec.rect,
            current_rect_precise,
            spec.text_align.as_deref(),
        );
        buffer.owner_node_id = node.stable_id.clone();
        buffer.stable_key = stable_key;
        buffer.order = order;
        buffer.rect = rect;
        buffer.rect_precise = Some(rect_precise);
        buffer.clip_rect = node.effective_clip.map(|clip| clip.rect);
        buffer.clip_radius = node.effective_clip.map(|clip| clip.radius).unwrap_or(0);
        buffer.clip_rect_precise = current_clip_rect_precise;
        buffer.clip_radius_precise = current_clip_radius_precise;
        if rect_precise.width > current_rect_precise.width + 0.01
            || rect_precise.height > current_rect_precise.height + 0.01
        {
            // An old texture larger than the new label stays inside the label's box.
            let clip = intersect_precise_rect(
                current_clip_rect_precise.unwrap_or(current_rect_precise),
                current_rect_precise,
            );
            buffer.clip_rect = None;
            buffer.clip_rect_precise = Some(clip);
            if current_clip_rect_precise.is_none() {
                buffer.clip_radius = 0;
                buffer.clip_radius_precise = None;
            }
        }
        buffer.color = spec.color;
        if label_debug_enabled() {
            info!(
                stable_key = %buffer.stable_key,
                owner_node_id = ?buffer.owner_node_id,
                previous_text = %label_preview(&buffer.text),
                requested_text = %label_preview(&spec.text),
                rect = %format_rect(buffer.rect),
                rect_precise = ?buffer.rect_precise,
                clip_rect = ?buffer.clip_rect,
                clip_rect_precise = ?buffer.clip_rect_precise,
                order = buffer.order,
                "label debug: fallback label buffer"
            );
        }
        buffers.push(buffer);
    } else if label_debug_enabled() {
        info!(
            path,
            stable_key = %stable_key,
            owner_node_id = ?node.stable_id,
            text = %label_preview(&spec.text),
            previous_count = previous.len(),
            "label debug: label buffer unavailable"
        );
    }
}

/// Where a previous label texture is drawn while the label's new text rasterizes: at its own
/// (old) size, placed in the new rect the way the text is aligned in it.
fn fallback_label_rect(
    previous: &CachedDecorationLabel,
    rect: LogicalRect,
    rect_precise: PreciseLogicalRect,
    text_align: Option<&str>,
) -> (LogicalRect, PreciseLogicalRect) {
    let old = previous
        .rect_precise
        .unwrap_or_else(|| precise_rect_from_logical(previous.rect));
    let free_x = rect_precise.width - old.width;
    let dx = match text_align {
        Some("center") => free_x * 0.5,
        Some("end") => free_x,
        _ => 0.0,
    };
    // Text is centered vertically in its texture.
    let dy = (rect_precise.height - old.height) * 0.5;
    let placed = PreciseLogicalRect {
        x: rect_precise.x + dx,
        y: rect_precise.y + dy,
        width: old.width,
        height: old.height,
    };
    let placed_logical = LogicalRect::new(
        rect.x + dx.round() as i32,
        rect.y + dy.round() as i32,
        previous.rect.width,
        previous.rect.height,
    );
    (placed_logical, placed)
}

fn intersect_precise_rect(a: PreciseLogicalRect, b: PreciseLogicalRect) -> PreciseLogicalRect {
    let x = a.x.max(b.x);
    let y = a.y.max(b.y);
    let right = (a.x + a.width).min(b.x + b.width);
    let bottom = (a.y + a.height).min(b.y + b.height);
    PreciseLogicalRect {
        x,
        y,
        width: (right - x).max(0.0),
        height: (bottom - y).max(0.0),
    }
}

fn fallback_text_buffer(
    previous: &[CachedDecorationLabel],
    owner_node_id: Option<&str>,
    stable_key: &str,
) -> Option<CachedDecorationLabel> {
    if let Some(owner_node_id) = owner_node_id
        && let Some(buffer) = previous
            .iter()
            .find(|buffer| buffer.owner_node_id.as_deref() == Some(owner_node_id))
    {
        return Some(buffer.clone());
    }

    previous
        .iter()
        .find(|buffer| buffer.stable_key == stable_key)
        .cloned()
}

fn text_buffers_need_raster_for_layout(
    layout: &ComputedDecorationTree,
    node_geometry: &impl NodeGeometryLookup,
    previous: &[CachedDecorationLabel],
    raster_scale: f64,
) -> bool {
    text_buffers_need_raster_for_node(
        &layout.root,
        "root".into(),
        node_geometry,
        previous,
        raster_scale,
    )
}

fn text_buffers_need_raster_for_node(
    node: &super::ComputedDecorationNode,
    path: String,
    node_geometry: &impl NodeGeometryLookup,
    previous: &[CachedDecorationLabel],
    raster_scale: f64,
) -> bool {
    if node.style.visible == Some(false) {
        return false;
    }

    let children_need_raster = node.children.iter().enumerate().any(|(index, child)| {
        text_buffers_need_raster_for_node(
            child,
            format!("{path}/child-{index}"),
            node_geometry,
            previous,
            raster_scale,
        )
    });
    if children_need_raster {
        return true;
    }

    let super::DecorationNodeKind::Label(label) = &node.kind else {
        return false;
    };

    let stable_key = format!("{path}:label");
    let previous_buffer = fallback_text_buffer(previous, node.stable_id.as_deref(), &stable_key);
    let color = node
        .style
        .color
        .unwrap_or(super::Color::WHITE)
        .with_opacity(node.style.opacity);
    let expects_no_buffer =
        label.text.is_empty() || node.rect.width <= 0 || node.rect.height <= 0 || color.a == 0;
    if expects_no_buffer {
        return previous_buffer.is_some();
    }

    let Some(previous_buffer) = previous_buffer else {
        return true;
    };

    let shared_geometry = node
        .stable_id
        .as_deref()
        .and_then(|stable_id| node_geometry.node_geometry(stable_id));
    let rect_precise = shared_geometry
        .map(|geometry| geometry.rect_precise)
        .unwrap_or_else(|| node.frame.precise_rect(node.resolved_rect));
    let previous_rect_precise = previous_buffer
        .rendered_rect_precise
        .unwrap_or_else(|| precise_rect_from_logical(previous_buffer.rendered_rect));

    previous_buffer.text != label.text
        || previous_buffer.color != color
        || previous_buffer.raster_scale != raster_scale
        || previous_buffer.rendered_rect.width != node.rect.width
        || previous_buffer.rendered_rect.height != node.rect.height
        || (previous_rect_precise.width - rect_precise.width).abs() > 0.001
        || (previous_rect_precise.height - rect_precise.height).abs() > 0.001
}

fn collect_icon_buffers(
    node: &super::ComputedDecorationNode,
    path: String,
    order_map: &std::collections::HashMap<String, usize>,
    dirty_node_ids: Option<&std::collections::HashSet<&str>>,
    node_geometry: &impl NodeGeometryLookup,
    raster_scale: f64,
    source: IconSource<'_>,
    rasterizer: &mut crate::backend::icon::IconRasterizer,
    buffers: &mut Vec<CachedDecorationIcon>,
) {
    if node.style.visible == Some(false) {
        return;
    }

    for_each_paint_ordered_child(node, |index, child| {
        collect_icon_buffers(
            child,
            format!("{path}/child-{index}"),
            order_map,
            dirty_node_ids,
            node_geometry,
            raster_scale,
            source,
            rasterizer,
            buffers,
        );
    });

    if dirty_node_ids.is_some_and(|dirty_node_ids| {
        !node
            .stable_id
            .as_deref()
            .is_some_and(|stable_id| node_id_matches_dirty_scope(stable_id, dirty_node_ids))
    }) {
        return;
    }

    let (asset_path, image_fit, fallback_asset_path) = match &node.kind {
        super::DecorationNodeKind::AppIcon(icon) => (None, None, icon.fallback.clone()),
        super::DecorationNodeKind::Image(image) => (Some(image.src.clone()), Some(image.fit), None),
        _ => return,
    };
    let shared_geometry = node
        .stable_id
        .as_deref()
        .and_then(|stable_id| node_geometry.node_geometry(stable_id));

    let spec = IconSpec {
        rect: node.rect,
        rect_precise: Some(
            shared_geometry
                .map(|geometry| geometry.rect_precise)
                .unwrap_or_else(|| node.frame.precise_rect(node.resolved_rect)),
        ),
        icon: source.icon.cloned(),
        app_id: source.app_id.map(str::to_owned),
        asset_path,
        image_fit,
        fallback_asset_path,
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
                .unwrap_or_else(|| node.frame.precise_rect(node.resolved_rect)),
        );
        buffer.clip_rect = node.effective_clip.map(|clip| clip.rect);
        buffer.clip_radius = node.effective_clip.map(|clip| clip.radius).unwrap_or(0);
        buffer.clip_rect_precise = shared_geometry
            .and_then(|geometry| geometry.clip_rect_precise)
            .or_else(|| {
                node.resolved_effective_clip
                    .map(|clip| node.frame.precise_rect(clip.rect))
            });
        buffer.clip_radius_precise = node
            .resolved_effective_clip
            .map(|clip| node.frame.logical_len(clip.radius).max(0.0));
        buffers.push(buffer);
    }
}

fn node_kind_name(kind: &super::DecorationNodeKind) -> &'static str {
    match kind {
        super::DecorationNodeKind::Box(_) => "box",
        super::DecorationNodeKind::Label(_) => "label",
        super::DecorationNodeKind::Button(_) => "button",
        super::DecorationNodeKind::AppIcon(_) => "app-icon",
        super::DecorationNodeKind::Image(_) => "image",
        super::DecorationNodeKind::ShaderEffect(_) => "shader-effect",
        super::DecorationNodeKind::WindowBorder => "window-border",
        super::DecorationNodeKind::WindowSlot => "window-slot",
        super::DecorationNodeKind::Popup(_) => "popup",
    }
}

fn summarize_tree_labels(tree: &DecorationTree) -> Vec<String> {
    let mut labels = Vec::new();
    collect_tree_label_summary(&tree.root, "root", &mut labels);
    labels
}

fn collect_tree_label_summary(node: &super::DecorationNode, path: &str, labels: &mut Vec<String>) {
    if let super::DecorationNodeKind::Label(label) = &node.kind {
        labels.push(format!(
            "{path} id={:?} text={} style={:?}",
            node.stable_id,
            label_preview(&label.text),
            node.style,
        ));
    }

    for (index, child) in node.children.iter().enumerate() {
        collect_tree_label_summary(child, &format!("{path}/child-{index}"), labels);
    }
}

fn summarize_text_buffers(buffers: &[CachedDecorationLabel]) -> Vec<String> {
    buffers
        .iter()
        .map(|buffer| {
            format!(
                "key={} owner={:?} text={} rect={} precise={:?} clip={:?} clip_precise={:?} order={}",
                buffer.stable_key,
                buffer.owner_node_id,
                label_preview(&buffer.text),
                format_rect(buffer.rect),
                buffer.rect_precise,
                buffer.clip_rect,
                buffer.clip_rect_precise,
                buffer.order
            )
        })
        .collect()
}

fn label_preview(text: &str) -> String {
    const MAX_CHARS: usize = 80;
    let mut preview = text.chars().take(MAX_CHARS).collect::<String>();
    if text.chars().count() > MAX_CHARS {
        preview.push('…');
    }
    preview
}

fn gap_debug_layout_enabled() -> bool {
    crate::env_flag!("SHOJI_GAP_LAYOUT_DEBUG") || crate::env_flag!("SHOJI_GAP_DEBUG")
}

fn format_resolved_rect(rect: crate::ssd::ResolvedLogicalRect) -> String {
    format!(
        "px x={}, y={}, w={}, h={}, right={}, bottom={}",
        rect.x.raw(),
        rect.y.raw(),
        rect.width.raw(),
        rect.height.raw(),
        rect.right().raw(),
        rect.bottom().raw(),
    )
}

/// Global logical geometry of a laid-out node, derived exactly from its
/// physical-pixel layout (`origin + px / scale`), keyed by stable id for the
/// buffer builders.
#[derive(Debug, Clone, Copy)]
struct NodeGeometry {
    rect_precise: PreciseLogicalRect,
    clip_rect_precise: Option<PreciseLogicalRect>,
}

impl NodeGeometry {
    fn for_node(node: &super::ComputedDecorationNode) -> Self {
        let frame = node.frame;
        Self {
            rect_precise: frame.precise_rect(node.resolved_rect),
            clip_rect_precise: node
                .resolved_effective_clip
                .map(|clip| frame.precise_rect(clip.rect)),
        }
    }
}

fn collect_node_geometry<'a>(
    node: &'a super::ComputedDecorationNode,
    insert: &mut impl FnMut(&'a str, NodeGeometry),
) {
    if let Some(stable_id) = node.stable_id.as_deref() {
        insert(stable_id, NodeGeometry::for_node(node));
    }
    for child in &node.children {
        collect_node_geometry(child, insert);
    }
}

fn build_node_geometry_map(
    layout: &ComputedDecorationTree,
) -> std::collections::HashMap<String, NodeGeometry> {
    let mut map = std::collections::HashMap::new();
    collect_node_geometry(&layout.root, &mut |id, geometry| {
        map.insert(id.to_string(), geometry);
    });
    map
}

fn build_node_geometry_map_in<'a>(
    layout: &'a ComputedDecorationTree,
    arena: &'a Bump,
) -> BumpNodeGeometryMap<'a> {
    let mut map = BumpNodeGeometryMap::new_in(arena);
    collect_node_geometry(&layout.root, &mut |id, geometry| {
        map.insert(id, geometry);
    });
    map
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
    info!(
        window_id = snapshot.id,
        title = snapshot.title,
        scale = root.frame.scale,
        client_rect = %format_rect(client_rect),
        root_rect = %format_rect(root.rect),
        root_rect_resolved = %format_resolved_rect(root.resolved_rect),
        root_content_rect_resolved = %format_resolved_rect(root.resolved_content_rect),
        slot_rect = %format_rect(slot_rect),
        slot_rect_resolved = %format_resolved_rect(slot_resolved_rect),
        logical_slot_right_delta_vs_client = (slot_rect.x + slot_rect.width) - (client_rect.x + client_rect.width),
        logical_slot_bottom_delta_vs_client = (slot_rect.y + slot_rect.height) - (client_rect.y + client_rect.height),
        "gap layout summary"
    );

    log_gap_layout_node(snapshot, root, None, 0);
}

fn log_gap_layout_node(
    snapshot: &WaylandWindowSnapshot,
    node: &super::ComputedDecorationNode,
    parent: Option<&super::ComputedDecorationNode>,
    depth: usize,
) {
    info!(
        window_id = snapshot.id,
        depth,
        kind = node_kind_name(&node.kind),
        stable_id = node.stable_id.as_deref().unwrap_or("<none>"),
        rect = %format_rect(node.rect),
        rect_resolved = %format_resolved_rect(node.resolved_rect),
        content_rect_resolved = %format_resolved_rect(node.resolved_content_rect),
        border_width_px = node.resolved_border_width.raw(),
        border_radius_px = node.resolved_border_radius.raw(),
        parent_kind = parent
            .map(|parent| node_kind_name(&parent.kind))
            .unwrap_or("<root>"),
        parent_content_left_delta_px = parent
            .map(|parent| node.resolved_rect.x.raw() - parent.resolved_content_rect.x.raw()),
        parent_content_right_delta_px = parent.map(|parent| {
            parent.resolved_content_rect.right().raw() - node.resolved_rect.right().raw()
        }),
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
            stable_key = %buffer.stable_key,
            paint = ?buffer.paint,
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

fn window_snapshot_requires_rebuild(
    previous: &WaylandWindowSnapshot,
    next: &WaylandWindowSnapshot,
) -> bool {
    previous.id != next.id
        || previous.title != next.title
        || previous.app_id != next.app_id
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
    previous.is_focused != next.is_focused
        || previous.interaction != next.interaction
        // The runtime steps the window's animations and polls on this output's frames;
        // a stale value (the first snapshot is taken before the window is mapped, with no
        // output) would leave them on wall-clock timer ticks.
        || previous.output_name != next.output_name
}

fn push_damage_pair(
    damage: &mut Vec<LogicalRect>,
    old_rect: Option<LogicalRect>,
    new_rect: LogicalRect,
) {
    if let Some(old_rect) = old_rect
        && old_rect != new_rect
    {
        damage.push(old_rect);
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
                (item.rect, format!("{:?}", item.paint)),
            )
        }),
        next_buffers.iter().map(|item| {
            (
                item.stable_key.clone(),
                (item.rect, format!("{:?}", item.paint)),
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
    collect_dirty_scope_rects(&previous_layout.root, &node_id_set, &mut previous_rects);
    collect_dirty_scope_rects(&next_layout.root, &node_id_set, &mut next_rects);

    let mut damage = Vec::new();
    for rect in previous_rects {
        damage.push(transformed_root_rect(rect, previous_transform));
    }
    for rect in next_rects {
        damage.push(transformed_root_rect(rect, next_transform));
    }
    damage
}

fn collect_dirty_scope_rects(
    node: &super::ComputedDecorationNode,
    dirty_node_ids: &std::collections::HashSet<&str>,
    rects: &mut Vec<LogicalRect>,
) {
    if node
        .stable_id
        .as_deref()
        .is_some_and(|stable_id| node_id_matches_dirty_scope(stable_id, dirty_node_ids))
    {
        rects.push(node.rect);
    }

    for child in &node.children {
        collect_dirty_scope_rects(child, dirty_node_ids, rects);
    }
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

fn should_process_window_for_refresh(
    primary_output_name: Option<&str>,
    target_output_name: Option<&str>,
    force_async_asset_refresh: bool,
    force_output_animation_reevaluate: bool,
    force_runtime_reevaluate: bool,
    window_was_runtime_dirty: bool,
) -> bool {
    if force_async_asset_refresh
        || force_output_animation_reevaluate
        || force_runtime_reevaluate
        || window_was_runtime_dirty
    {
        return true;
    }

    target_output_name
        .is_none_or(|target_output_name| primary_output_name == Some(target_output_name))
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
        LayoutDirection, Overflow, StylePosition,
    };

    #[test]
    fn shader_clip_coverage_stays_inside_border_ring_at_fractional_scale() {
        // Numeric replication of the live render math for the terminal-glass
        // ShaderEffect (direct child of a WindowBorder). The shader display
        // maps v_coords over its frame-mapped geometry, normalizes to "area"
        // units, and cuts with a rounded rect clip. That coverage must stay
        // inside the border ring's outer corner curve.
        let mut glass = DecorationNode::new(DecorationNodeKind::ShaderEffect(
            crate::ssd::ShaderEffectNode {
                direction: LayoutDirection::Column,
                shader: crate::ssd::CompiledEffect {
                    input: crate::ssd::EffectInput::Backdrop,
                    capture_padding: 24,
                    invalidate: crate::ssd::EffectInvalidationPolicy::Always,
                    pipeline: Vec::new(),
                    alpha: crate::ssd::EffectAlphaMode::Opaque,
                },
            },
        ))
        .with_children(vec![
            DecorationNode::new(DecorationNodeKind::Box(BoxNode {
                direction: LayoutDirection::Row,
            }))
            .with_style(DecorationStyle {
                height: Some(30.0),
                ..Default::default()
            }),
            DecorationNode::new(DecorationNodeKind::WindowSlot),
        ]);
        glass.stable_id = Some("glass".into());
        let mut root = DecorationNode::new(DecorationNodeKind::WindowBorder)
            .with_style(DecorationStyle {
                border: Some(BorderStyle {
                    width: 2.0,
                    color: Color::WHITE,
                }),
                border_radius: Some(10.0),
                ..Default::default()
            })
            .with_children(vec![glass]);
        root.stable_id = Some("root".into());
        let tree = DecorationTree::new(root);
        let scale = 1.8f64;
        let layout = tree
            .layout_for_client_with_scale(LogicalRect::new(103, 133, 400, 300), scale)
            .expect("layout should succeed");
        let arena = Bump::new();
        let shared = build_node_geometry_map_in(&layout, &arena);
        let order = build_render_order_map(&layout);
        let (_buffers, shaders) = build_cached_buffers_and_shaders(&layout, &order, None, &shared);
        let glass_cache = shaders
            .iter()
            .find(|shader| shader.stable_key.starts_with("root/child-0"))
            .expect("glass shader cache");
        let root_rect = layout.root.rect;
        let output_geo =
            smithay::utils::Rectangle::<i32, Logical>::new((0, 0).into(), (2133, 1200).into());
        let scale2 = smithay::utils::Scale::from((scale, scale));
        let subpixel = crate::backend::visual::RootSubpixelEdges::default();
        let rect_precise = glass_cache.rect_precise.expect("glass rect_precise");
        let geometry = crate::backend::visual::relative_physical_rect_from_root_precise(
            rect_precise,
            root_rect,
            subpixel,
            output_geo,
            scale2,
        );
        let clip_rect_precise = glass_cache.clip_rect_precise.expect("glass clip");
        let local_rect_w = glass_cache.rect.width;
        let local_rect_h = glass_cache.rect.height;
        let clip_area =
            crate::backend::visual::snapped_precise_logical_rect_in_root_frame_area_space(
                clip_rect_precise,
                rect_precise,
                local_rect_w,
                local_rect_h,
                root_rect,
                subpixel,
                output_geo,
                scale2,
            );
        let clip_radius = glass_cache
            .clip_radius_precise
            .unwrap_or(glass_cache.clip_radius as f32);
        eprintln!(
            "root={root_rect:?} glass rect={:?} rect_precise={rect_precise:?}",
            glass_cache.rect
        );
        eprintln!(
            "geometry(root-local px)={geometry:?} clip_area={clip_area:?} clip_radius={clip_radius}"
        );

        // Shader emulation (wrap_backdrop_shader_source): coverage at a
        // root-local physical point. Models the full GPU data flow: the
        // pipeline output texture carries capture padding, so smithay's
        // v_coords varying spans the sample-src subrect (uv_offset..uv_offset
        // + uv_scale), NOT [0,1] across the quad. The wrapper must normalize
        // v_coords back to quad-local uv before scaling by rect_size —
        // `normalize_uv: false` replicates the historical bug (raw v_coords)
        // and is asserted below to poke, pinning the failure mode.
        let render_scale = geometry.size.w.max(1) as f32 / local_rect_w.max(1) as f32;
        let padding_px =
            (glass_cache.shader.capture_padding.max(0) as f32 * render_scale.max(1.0)).ceil();
        let tex_w = geometry.size.w as f32 + 2.0 * padding_px;
        let tex_h = geometry.size.h as f32 + 2.0 * padding_px;
        let uv_offset = (padding_px / tex_w, padding_px / tex_h);
        let uv_scale = (
            geometry.size.w as f32 / tex_w,
            geometry.size.h as f32 / tex_h,
        );
        let shader_alpha = |px: f64, py: f64, normalize_uv: bool| -> f32 {
            let quad_u = (px - geometry.loc.x as f64) / geometry.size.w.max(1) as f64;
            let quad_v = (py - geometry.loc.y as f64) / geometry.size.h.max(1) as f64;
            if !(0.0..=1.0).contains(&quad_u) || !(0.0..=1.0).contains(&quad_v) {
                return 0.0; // outside the drawn quad
            }
            // smithay tex_matrix: varying = src-subrect uv at this quad point.
            let v_coords = (
                uv_offset.0 + quad_u as f32 * uv_scale.0,
                uv_offset.1 + quad_v as f32 * uv_scale.1,
            );
            let (ux, uy) = if normalize_uv {
                (
                    (v_coords.0 - uv_offset.0) / uv_scale.0,
                    (v_coords.1 - uv_offset.1) / uv_scale.1,
                )
            } else {
                v_coords
            };
            let cx = ux * local_rect_w as f32 - clip_area.x;
            let cy = uy * local_rect_h as f32 - clip_area.y;
            let half_w = clip_area.width * 0.5;
            let half_h = clip_area.height * 0.5;
            let p = (cx - half_w, cy - half_h);
            let r = clip_radius;
            let q = ((p.0.abs() - (half_w - r)), (p.1.abs() - (half_h - r)));
            let dist = q.0.max(q.1).min(0.0) + (q.0.max(0.0).hypot(q.1.max(0.0))) - r;
            let half_px = 0.5 / render_scale.max(1.0);
            1.0 - ((dist + half_px) / (2.0 * half_px)).clamp(0.0, 1.0)
        };

        // Border ring outer curve: corner circle at root-local physical
        // (r_out, r_out) with r_out = snapped outer radius.
        let r_out = ((10.0 * scale).round()) as f64; // 18 px
        let (ccx, ccy) = (r_out, r_out);
        let mut worst: Option<(f64, f32)> = None;
        let mut worst_raw: Option<(f64, f32)> = None;
        for step in 0..2000 {
            let angle = std::f64::consts::FRAC_PI_2 * (step as f64) / 1999.0 + std::f64::consts::PI;
            for extra in 0..30 {
                let d = r_out + 0.75 + (extra as f64) * 0.25;
                let px = ccx + angle.cos() * d;
                let py = ccy + angle.sin() * d;
                let alpha = shader_alpha(px, py, true);
                if alpha > 0.1 && worst.is_none_or(|(wd, _)| d > wd) {
                    worst = Some((d, alpha));
                }
                let raw_alpha = shader_alpha(px, py, false);
                if raw_alpha > 0.1 && worst_raw.is_none_or(|(wd, _)| d > wd) {
                    worst_raw = Some((d, raw_alpha));
                }
            }
        }
        if let Some((d, alpha)) = worst {
            panic!(
                "glass coverage pokes outside the ring: up to {:.2}px beyond the outer curve (alpha {alpha:.2})",
                d - r_out
            );
        }
        // Sanity check of the diagnosis: with the capture padding present, the
        // raw-v_coords formula dilates the clip and must poke past the ring.
        // If this stops failing the emulation no longer models the padding.
        let (raw_d, _) = worst_raw.expect("raw v_coords formula should poke outside the ring");
        eprintln!(
            "raw v_coords formula pokes {:.2}px beyond the outer curve (expected)",
            raw_d - r_out
        );
    }

    #[test]
    fn window_border_child_clip_probe() {
        let mut titlebar = DecorationNode::new(DecorationNodeKind::ShaderEffect(
            crate::ssd::ShaderEffectNode {
                direction: LayoutDirection::Row,
                shader: crate::ssd::CompiledEffect {
                    input: crate::ssd::EffectInput::Backdrop,
                    capture_padding: 0,
                    invalidate: crate::ssd::EffectInvalidationPolicy::Always,
                    pipeline: Vec::new(),
                    alpha: crate::ssd::EffectAlphaMode::Opaque,
                },
            },
        ))
        .with_style(DecorationStyle {
            height: Some(30.0),
            background: Some(Color::BLACK),
            ..Default::default()
        });
        titlebar.stable_id = Some("titlebar".into());
        let mut inner = DecorationNode::new(DecorationNodeKind::Box(BoxNode {
            direction: LayoutDirection::Column,
        }))
        .with_children(vec![
            titlebar,
            DecorationNode::new(DecorationNodeKind::WindowSlot),
        ]);
        inner.stable_id = Some("inner".into());
        let mut root = DecorationNode::new(DecorationNodeKind::WindowBorder)
            .with_style(DecorationStyle {
                border: Some(BorderStyle {
                    width: 2.0,
                    color: Color::WHITE,
                }),
                border_radius: Some(10.0),
                ..Default::default()
            })
            .with_children(vec![inner]);
        root.stable_id = Some("root".into());
        let tree = DecorationTree::new(root);
        let layout = tree
            .layout_for_client_with_scale(LogicalRect::new(102, 132, 400, 300), 1.0)
            .expect("layout should succeed");
        eprintln!(
            "root rect={:?} content(resolved)={:?} effective_clip={:?}",
            layout.root.rect,
            layout
                .root
                .frame
                .logical_rect(layout.root.resolved_content_rect),
            layout.root.effective_clip,
        );
        let arena = Bump::new();
        let shared = build_node_geometry_map_in(&layout, &arena);
        let order = build_render_order_map(&layout);
        let (buffers, shaders) = build_cached_buffers_and_shaders(&layout, &order, None, &shared);
        for shader in &shaders {
            eprintln!(
                "shader {} rect={:?} rect_precise={:?} clip_rect_precise={:?} clip_radius_precise={:?}",
                shader.stable_key,
                shader.rect,
                shader.rect_precise,
                shader.clip_rect_precise,
                shader.clip_radius_precise,
            );
        }
        for buffer in &buffers {
            eprintln!(
                "buffer {} kind={} rect={:?} clip={:?} rounded_clip={:?}",
                buffer.stable_key,
                buffer.source_kind,
                buffer.rect,
                buffer.paint.geometry.clip,
                buffer.paint.geometry.rounded_clip,
            );
        }
    }

    /// 120 Hz frames are 8.33 ms apart; whole-millisecond sampling stepped a linear
    /// 250 ms animation by 3.2 % and 3.6 % of its distance in turn.
    /// A `<ShaderEffect>` inside a rounded `<WindowBorder>` reports its own
    /// shape plus the border's inner clip, in layout pixels.
    #[test]
    fn shader_effect_node_shape_carries_radius_border_and_clip() {
        let mut titlebar = DecorationNode::new(DecorationNodeKind::ShaderEffect(
            crate::ssd::ShaderEffectNode {
                direction: LayoutDirection::Row,
                shader: crate::ssd::CompiledEffect {
                    input: crate::ssd::EffectInput::Backdrop,
                    capture_padding: 0,
                    invalidate: crate::ssd::EffectInvalidationPolicy::Always,
                    pipeline: Vec::new(),
                    alpha: crate::ssd::EffectAlphaMode::Opaque,
                },
            },
        ))
        .with_style(DecorationStyle {
            height: Some(30.0),
            border_radius: Some(4.0),
            border_bottom: Some(BorderStyle {
                width: 1.0,
                color: Color::WHITE,
            }),
            ..Default::default()
        });
        titlebar.stable_id = Some("titlebar".into());
        let inner = DecorationNode::new(DecorationNodeKind::Box(BoxNode {
            direction: LayoutDirection::Column,
        }))
        .with_children(vec![
            titlebar,
            DecorationNode::new(DecorationNodeKind::WindowSlot),
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
            .with_children(vec![inner]);
        let layout = DecorationTree::new(root)
            .layout_for_client_with_scale(LogicalRect::new(100, 100, 400, 300), 1.5)
            .expect("layout should succeed");
        let order = build_render_order_map(&layout);
        let shared = build_node_geometry_map(&layout);
        let (_, shaders) = build_cached_buffers_and_shaders(&layout, &order, None, &shared);
        let shape = shaders[0].node_shape;

        assert_eq!(shape.layout_scale, 1.5);
        assert_eq!(shape.radius, [6; 4], "4 logical px at 1.5x");
        assert_eq!(shape.border, [0, 0, 2, 0], "bottom border only");
        let (clip_rect, clip_radius) = shape.clip.expect("inside the rounded border");
        // The border's inner edge: 3px border at 1.5x, radius (10 - 2) * 1.5.
        assert_eq!(clip_radius, [12; 4]);
        assert_eq!(
            &clip_rect[..2],
            &[0, 0],
            "the titlebar starts at the inner edge"
        );

        let frame = shape.effect_frame(None, 1.5);
        assert_eq!(frame.radius, [6.0; 4]);
        assert_eq!(frame.scale, 1.5);
        // On an output at another scale, pixels follow that output.
        assert_eq!(shape.effect_frame(None, 3.0).radius, [12.0; 4]);
    }

    #[test]
    fn managed_animation_progress_keeps_fractional_frame_times() {
        let animation = test_rect_animation(
            "default",
            1,
            ManagedWindowAnimationMode::Override,
            test_rect(0.0, 0.0, 100.0, 100.0),
            test_rect(250.0, 0.0, 100.0, 100.0),
        );
        let frame_ms = 1000.0 / 120.0;
        let steps = (1..=6)
            .map(|frame| {
                let before =
                    managed_animation_progress(&animation, (frame - 1) as f64 * frame_ms).0;
                let after = managed_animation_progress(&animation, frame as f64 * frame_ms).0;
                after - before
            })
            .collect::<Vec<_>>();
        for step in &steps {
            assert!(
                (step - frame_ms / 250.0).abs() < 1e-9,
                "uneven steps: {steps:?}"
            );
        }
    }

    fn test_rect(x: f64, y: f64, width: f64, height: f64) -> ManagedWindowRectSnapshot {
        ManagedWindowRectSnapshot {
            x,
            y,
            width,
            height,
        }
    }

    fn test_rect_animation(
        channel: &str,
        sequence: u64,
        mode: ManagedWindowAnimationMode,
        from: ManagedWindowRectSnapshot,
        to: ManagedWindowRectSnapshot,
    ) -> ActiveManagedWindowAnimation {
        ActiveManagedWindowAnimation {
            sequence,
            started_at_ms: 0.0,
            animation: ManagedWindowAnimationSnapshot {
                channel: channel.into(),
                rect: Some(ManagedWindowRectAnimationSnapshot {
                    from: Some(from),
                    to,
                    duration: 250,
                    easing: ManagedWindowAnimationEasingSnapshot::Linear,
                    mode,
                }),
                offset: None,
                opacity: None,
            },
        }
    }

    #[test]
    fn final_override_target_composes_the_animation_end_state() {
        let old_rect = test_rect(8.0, 8.0, 390.0, 984.0);
        let override_target = test_rect(8.0, 8.0, 522.666, 984.0);
        let mut static_managed_window = ManagedWindowState {
            rect: Some(old_rect),
            ..ManagedWindowState::default()
        };
        static_managed_window.managed = true;

        let mut channels = BTreeMap::new();
        channels.insert(
            "reflow".into(),
            test_rect_animation(
                "reflow",
                2,
                ManagedWindowAnimationMode::Override,
                old_rect,
                override_target,
            ),
        );
        channels.insert(
            "offset".into(),
            test_rect_animation(
                "offset",
                1,
                ManagedWindowAnimationMode::Add,
                test_rect(0.0, 0.0, 0.0, 0.0),
                test_rect(4.0, 2.0, 0.0, 0.0),
            ),
        );

        assert_eq!(
            final_override_rect_animation_target(&static_managed_window, &channels),
            Some(test_rect(12.0, 10.0, 522.666, 984.0))
        );
    }

    #[test]
    fn additive_only_animation_does_not_pin_client_configure_size() {
        let old_rect = test_rect(8.0, 8.0, 390.0, 984.0);
        let static_managed_window = ManagedWindowState {
            rect: Some(old_rect),
            ..ManagedWindowState::default()
        };
        let mut channels = BTreeMap::new();
        channels.insert(
            "offset".into(),
            test_rect_animation(
                "offset",
                1,
                ManagedWindowAnimationMode::Add,
                test_rect(0.0, 0.0, 0.0, 0.0),
                test_rect(4.0, 2.0, 0.0, 0.0),
            ),
        );

        assert_eq!(
            final_override_rect_animation_target(&static_managed_window, &channels),
            None
        );
    }

    /// Opening a popup renumbers the paint order of the nodes after it; the
    /// buffers a partial rebuild keeps must follow, or a kept label ends up
    /// behind a rebuilt background.
    #[test]
    fn partial_rebuild_renumbers_the_buffers_it_keeps() {
        let tree = |open: bool| {
            let mut background =
                DecorationNode::new(DecorationNodeKind::Box(super::super::BoxNode::default()))
                    .with_style(DecorationStyle {
                        height: Some(20.0),
                        background: Some(super::super::Color::rgba(0, 0, 0, 255)),
                        ..Default::default()
                    });
            background.stable_id = Some("root.background".into());
            let mut popup =
                DecorationNode::new(DecorationNodeKind::Popup(super::super::PopupNode {
                    open,
                    ..Default::default()
                }))
                .with_children(vec![
                    DecorationNode::new(DecorationNodeKind::Box(super::super::BoxNode::default()))
                        .with_style(DecorationStyle {
                            width: Some(10.0),
                            height: Some(10.0),
                            background: Some(super::super::Color::rgba(255, 0, 0, 255)),
                            ..Default::default()
                        }),
                ]);
            popup.stable_id = Some("root.popup".into());
            let mut root =
                DecorationNode::new(DecorationNodeKind::Box(super::super::BoxNode::default()))
                    .with_children(vec![
                        background,
                        DecorationNode::new(DecorationNodeKind::WindowSlot),
                        // Last child: painted first (front), so it shifts the rest.
                        popup,
                    ]);
            root.stable_id = Some("root".into());
            DecorationTree::new(root)
                .layout_for_client(LogicalRect::new(0, 0, 100, 100))
                .expect("layout")
        };

        let closed = tree(false);
        let closed_orders = build_render_order_map(&closed);
        let previous = build_cached_buffers(&closed, &closed_orders);
        let open = tree(true);
        let open_orders = build_render_order_map(&open);
        let dirty = vec!["root.popup".to_owned()];
        let (rebuilt, _) = rebuild_partial_buffers(&open, &open_orders, &dirty);
        let merged = merge_cached_buffers(&previous, rebuilt, &dirty, &open_orders);

        let background = merged
            .iter()
            .find(|buffer| buffer.owner_node_id.as_deref() == Some("root.background"))
            .expect("kept background buffer");
        assert_eq!(
            Some(&background.order),
            open_orders.get(&background.stable_key)
        );
        assert_ne!(
            closed_orders.get(&background.stable_key),
            open_orders.get(&background.stable_key),
            "the popup renumbers the background"
        );
    }

    #[test]
    fn dirty_scope_covers_descendants_in_both_id_styles() {
        let dirty = std::collections::HashSet::from(["root.Box#bar", "w1/0/2"]);
        assert!(node_id_matches_dirty_scope("root.Box#bar.Label", &dirty));
        assert!(node_id_matches_dirty_scope("w1/0/2/0", &dirty));
        assert!(node_id_matches_dirty_scope("w1/0/2", &dirty));
        assert!(!node_id_matches_dirty_scope("w1/0/21", &dirty));
        assert!(!node_id_matches_dirty_scope("root.Box#barista", &dirty));
    }

    #[test]
    fn cached_tree_patch_replaces_only_the_target_subtree() {
        let mut label = DecorationNode::new(DecorationNodeKind::Label(super::super::LabelNode {
            text: "before".into(),
        }));
        label.stable_id = Some("root.Label[0]".into());
        let mut slot = DecorationNode::new(DecorationNodeKind::WindowSlot);
        slot.stable_id = Some("root.WindowSlot[1]".into());
        let mut root = DecorationNode::new(DecorationNodeKind::Box(BoxNode {
            direction: LayoutDirection::Column,
        }))
        .with_children(vec![label, slot.clone()]);
        root.stable_id = Some("root".into());
        let mut tree = DecorationTree::new(root);

        let mut replacement =
            DecorationNode::new(DecorationNodeKind::Label(super::super::LabelNode {
                text: "after".into(),
            }));
        replacement.stable_id = Some("root.Label[0]".into());
        let update = apply_cached_tree_update(
            &mut tree,
            None,
            vec![CompositionPatch::ReplaceNode {
                node_id: "root.Label[0]".into(),
                node: replacement,
            }],
        )
        .expect("native subtree patch should apply");

        assert!(update.changed);
        assert_eq!(tree.root.children[1], slot);
        assert!(
            matches!(
                &tree.root.children[0].kind,
                DecorationNodeKind::Label(label) if label.text == "after"
            ),
            "patched label should replace the old subtree"
        );
    }

    #[test]
    fn cached_tree_patch_updates_shader_uniform_without_replacing_node() {
        let mut uniforms = std::collections::BTreeMap::new();
        uniforms.insert(
            "phase_01".into(),
            crate::ssd::ShaderUniformValue::Float(0.0),
        );
        let effect = crate::ssd::CompiledEffect {
            input: crate::ssd::EffectInput::Backdrop,
            capture_padding: 0,
            invalidate: crate::ssd::EffectInvalidationPolicy::Always,
            pipeline: vec![crate::ssd::EffectStage::Shader(crate::ssd::ShaderStage {
                shader: crate::ssd::ShaderModule {
                    path: "animated.frag".into(),
                },
                uniforms,
                textures: std::collections::BTreeMap::new(),
            })],
            alpha: crate::ssd::EffectAlphaMode::Opaque,
        };
        let mut root = DecorationNode::new(DecorationNodeKind::ShaderEffect(
            crate::ssd::ShaderEffectNode {
                direction: LayoutDirection::Column,
                shader: effect,
            },
        ));
        root.stable_id = Some("root".into());
        let mut tree = DecorationTree::new(root);

        let update = apply_cached_tree_update(
            &mut tree,
            None,
            vec![CompositionPatch::ShaderUniform {
                node_id: "root".into(),
                stage_index: 0,
                name: "phase_01".into(),
                value: crate::ssd::ShaderUniformValue::Float(0.5),
            }],
        )
        .expect("native shader uniform patch should apply");

        assert!(update.changed);
        assert!(update.layout_equivalent);
        let DecorationNodeKind::ShaderEffect(effect) = &tree.root.kind else {
            panic!("root should remain a ShaderEffect");
        };
        let crate::ssd::EffectStage::Shader(stage) = &effect.shader.pipeline[0] else {
            panic!("first stage should remain a shader stage");
        };
        assert_eq!(
            stage.uniforms.get("phase_01"),
            Some(&crate::ssd::ShaderUniformValue::Float(0.5))
        );
    }

    #[test]
    fn cached_tree_patch_updates_paint_uniform_next_to_node_patches() {
        let mut uniforms = std::collections::BTreeMap::new();
        uniforms.insert("hover".into(), crate::ssd::ShaderUniformValue::Float(0.0));
        let mut button = DecorationNode::new(DecorationNodeKind::Box(BoxNode {
            direction: LayoutDirection::Row,
        }));
        button.stable_id = Some("root.Button[0]".into());
        button.style.paint = Some(crate::ssd::PaintShader {
            shader: crate::ssd::ShaderModule {
                path: "button.frag".into(),
            },
            uniforms,
            outsets: Default::default(),
        });
        let mut label = DecorationNode::new(DecorationNodeKind::Label(super::super::LabelNode {
            text: "before".into(),
        }));
        label.stable_id = Some("root.Label[1]".into());
        let mut root = DecorationNode::new(DecorationNodeKind::Box(BoxNode {
            direction: LayoutDirection::Row,
        }))
        .with_children(vec![button, label]);
        root.stable_id = Some("root".into());
        let mut tree = DecorationTree::new(root);

        let mut replacement =
            DecorationNode::new(DecorationNodeKind::Label(super::super::LabelNode {
                text: "after".into(),
            }));
        replacement.stable_id = Some("root.Label[1]".into());
        let update = apply_cached_tree_update(
            &mut tree,
            None,
            vec![
                CompositionPatch::ReplaceNode {
                    node_id: "root.Label[1]".into(),
                    node: replacement,
                },
                CompositionPatch::ShaderUniform {
                    node_id: "root.Button[0]".into(),
                    stage_index: PAINT_STAGE_INDEX,
                    name: "hover".into(),
                    value: crate::ssd::ShaderUniformValue::Float(1.0),
                },
            ],
        )
        .expect("a paint uniform patch should apply next to node patches");

        assert!(update.changed);
        let paint = tree.root.children[0]
            .style
            .paint
            .as_ref()
            .expect("paint kept");
        assert_eq!(
            paint.uniforms.get("hover"),
            Some(&crate::ssd::ShaderUniformValue::Float(1.0))
        );
    }

    #[test]
    fn shader_uniform_update_targets_shader_input() {
        let mut uniforms = std::collections::BTreeMap::new();
        uniforms.insert(
            "phase_01".into(),
            crate::ssd::ShaderUniformValue::Float(0.0),
        );
        let effect = crate::ssd::CompiledEffect {
            input: crate::ssd::EffectInput::Shader(crate::ssd::ShaderStage {
                shader: crate::ssd::ShaderModule {
                    path: "animated.frag".into(),
                },
                uniforms,
                textures: std::collections::BTreeMap::new(),
            }),
            capture_padding: 0,
            invalidate: crate::ssd::EffectInvalidationPolicy::Always,
            pipeline: Vec::new(),
            alpha: crate::ssd::EffectAlphaMode::Opaque,
        };
        let mut root = DecorationNode::new(DecorationNodeKind::ShaderEffect(
            crate::ssd::ShaderEffectNode {
                direction: LayoutDirection::Column,
                shader: effect,
            },
        ));
        root.stable_id = Some("root".into());
        let mut tree = DecorationTree::new(root);

        let update = apply_cached_tree_update(
            &mut tree,
            None,
            vec![CompositionPatch::ShaderUniform {
                node_id: "root".into(),
                stage_index: SHADER_INPUT_STAGE_INDEX,
                name: "phase_01".into(),
                value: crate::ssd::ShaderUniformValue::Float(0.5),
            }],
        )
        .expect("shader input uniform patch should apply through the generic update path");

        assert!(update.changed);
        assert!(update.layout_equivalent);
        let DecorationNodeKind::ShaderEffect(effect) = &tree.root.kind else {
            panic!("root should remain a ShaderEffect");
        };
        let crate::ssd::EffectInput::Shader(input) = &effect.shader.input else {
            panic!("effect input should remain a shader input");
        };
        assert_eq!(
            input.uniforms.get("phase_01"),
            Some(&crate::ssd::ShaderUniformValue::Float(0.5))
        );
    }

    #[test]
    fn shader_uniform_fast_update_mutates_render_state_without_relayout() {
        let mut uniforms = std::collections::BTreeMap::new();
        uniforms.insert(
            "phase_01".into(),
            crate::ssd::ShaderUniformValue::Float(0.0),
        );
        let effect = crate::ssd::CompiledEffect {
            input: crate::ssd::EffectInput::Backdrop,
            capture_padding: 0,
            invalidate: crate::ssd::EffectInvalidationPolicy::Always,
            pipeline: vec![crate::ssd::EffectStage::Shader(crate::ssd::ShaderStage {
                shader: crate::ssd::ShaderModule {
                    path: "animated.frag".into(),
                },
                uniforms,
                textures: std::collections::BTreeMap::new(),
            })],
            alpha: crate::ssd::EffectAlphaMode::Opaque,
        };
        let mut shader = DecorationNode::new(DecorationNodeKind::ShaderEffect(
            crate::ssd::ShaderEffectNode {
                direction: LayoutDirection::Column,
                shader: effect,
            },
        ))
        .with_style(DecorationStyle {
            width: Some(64.0),
            height: Some(64.0),
            ..Default::default()
        });
        shader.stable_id = Some("root.ShaderEffect[0]".into());
        let mut slot = DecorationNode::new(DecorationNodeKind::WindowSlot);
        slot.stable_id = Some("root.WindowSlot[1]".into());
        let mut root = DecorationNode::new(DecorationNodeKind::Box(BoxNode {
            direction: LayoutDirection::Column,
        }))
        .with_children(vec![shader, slot]);
        root.stable_id = Some("root".into());
        let mut tree = DecorationTree::new(root);
        let mut layout = tree
            .layout_for_client(LogicalRect::new(10, 20, 800, 600))
            .expect("layout should succeed");
        let order_map = build_render_order_map(&layout);
        let mut shader_buffers = build_shader_buffers(&layout, &order_map);
        let layout_rect_before = layout.root.rect;

        let update = apply_shader_uniform_fast_update(
            &mut tree,
            &mut layout,
            &mut [],
            &mut shader_buffers,
            &[CompositionPatch::ShaderUniform {
                node_id: "root.ShaderEffect[0]".into(),
                stage_index: 0,
                name: "phase_01".into(),
                value: crate::ssd::ShaderUniformValue::Float(0.5),
            }],
        )
        .expect("native shader uniform fast update should apply");

        assert!(update.tree_changed);
        assert!(update.rendered_changed);
        assert_eq!(update.damage_rects.len(), 1);
        assert_eq!(layout.root.rect, layout_rect_before);
        let shader_value = |effect: &crate::ssd::CompiledEffect| {
            let crate::ssd::EffectStage::Shader(stage) = &effect.pipeline[0] else {
                panic!("first stage should remain a shader stage");
            };
            stage.uniforms.get("phase_01").cloned()
        };
        let DecorationNodeKind::ShaderEffect(tree_effect) = &tree.root.children[0].kind else {
            panic!("tree node should remain a ShaderEffect");
        };
        let DecorationNodeKind::ShaderEffect(layout_effect) = &layout.root.children[0].kind else {
            panic!("computed node should remain a ShaderEffect");
        };
        assert_eq!(
            shader_value(&tree_effect.shader),
            Some(crate::ssd::ShaderUniformValue::Float(0.5))
        );
        assert_eq!(
            shader_value(&layout_effect.shader),
            Some(crate::ssd::ShaderUniformValue::Float(0.5))
        );
        assert_eq!(
            shader_value(&shader_buffers[0].shader),
            Some(crate::ssd::ShaderUniformValue::Float(0.5))
        );
    }

    #[test]
    fn managed_rect_rounding_preserves_opposite_edges() {
        let rect = managed_rect_snapshot_to_logical_rect(ManagedWindowRectSnapshot {
            x: 10.4,
            y: 20.4,
            width: 99.4,
            height: 79.4,
        });

        assert_eq!(rect.x, 10);
        assert_eq!(rect.y, 20);
        assert_eq!(rect.x + rect.width, 110);
        assert_eq!(rect.y + rect.height, 100);
    }

    #[test]
    fn async_asset_refresh_processes_windows_outside_target_output() {
        assert!(should_process_window_for_refresh(
            Some("eDP-1"),
            Some("DP-4"),
            true,
            false,
            false,
            false,
        ));
        assert!(!should_process_window_for_refresh(
            Some("eDP-1"),
            Some("DP-4"),
            false,
            false,
            false,
            false,
        ));
        assert!(should_process_window_for_refresh(
            Some("eDP-1"),
            Some("DP-4"),
            false,
            true,
            false,
            false,
        ));
        assert!(should_process_window_for_refresh(
            Some("eDP-1"),
            Some("DP-4"),
            false,
            false,
            false,
            true,
        ));
        assert!(should_process_window_for_refresh(
            Some("eDP-1"),
            Some("DP-4"),
            false,
            false,
            true,
            false,
        ));
        assert!(should_process_window_for_refresh(
            Some("DP-4"),
            Some("DP-4"),
            false,
            false,
            false,
            false,
        ));
    }

    #[test]
    fn client_rect_probe_clears_staleness_once_animations_are_done() {
        let rect = LogicalRect::new(10, 40, 640, 480);
        let moved = LogicalRect::new(12, 40, 640, 480);
        // An opacity fade ended without a relayout: the probe agrees with the
        // cached rect, so the next refreshes can skip it.
        assert!(client_rect_probe_agrees(true, false, 1.5, 1.5, rect, rect));
        // Still animating, rescaled, or the rect really moved: keep probing.
        assert!(!client_rect_probe_agrees(true, true, 1.5, 1.5, rect, rect));
        assert!(!client_rect_probe_agrees(true, false, 1.0, 1.5, rect, rect));
        assert!(!client_rect_probe_agrees(
            true, false, 1.5, 1.5, rect, moved
        ));
        // Nothing to restore.
        assert!(!client_rect_probe_agrees(
            false, false, 1.5, 1.5, rect, rect
        ));
    }

    #[test]
    fn dirty_scope_matches_descendant_node_ids() {
        let dirty = ["root.Box[0]"]
            .into_iter()
            .collect::<std::collections::HashSet<_>>();

        assert!(node_id_matches_dirty_scope("root.Box[0]", &dirty));
        assert!(node_id_matches_dirty_scope(
            "root.Box[0].Button[1].Image[0]",
            &dirty
        ));
        assert!(!node_id_matches_dirty_scope("root.Box[1].Image[0]", &dirty));
        assert!(!node_id_matches_dirty_scope(
            "root.Box[0-extra].Image[0]",
            &dirty
        ));
    }

    #[test]
    fn layout_for_client_aligns_window_slot_with_client_rect() {
        let tree = DecorationTree::new(
            DecorationNode::new(DecorationNodeKind::WindowBorder)
                .with_style(DecorationStyle {
                    border: Some(BorderStyle {
                        width: 1.0,
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
                            height: Some(30.0),
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
    fn layout_for_client_does_not_expand_root_for_absolute_titlebar_overflow() {
        let titlebar_overlay = DecorationNode::new(DecorationNodeKind::Box(BoxNode {
            direction: LayoutDirection::Row,
        }))
        .with_style(DecorationStyle {
            position: Some(StylePosition::Relative),
            padding: Edges {
                left: 12.0,
                right: 12.0,
                ..Default::default()
            },
            ..Default::default()
        })
        .with_children(vec![
            DecorationNode::new(DecorationNodeKind::Box(BoxNode::default())).with_style(
                DecorationStyle {
                    width: Some(10.0),
                    height: Some(18.0),
                    margin: Edges {
                        left: 32.0,
                        ..Default::default()
                    },
                    ..Default::default()
                },
            ),
            DecorationNode::new(DecorationNodeKind::Box(BoxNode::default())).with_style(
                DecorationStyle {
                    position: Some(StylePosition::Absolute),
                    width: Some(96.0),
                    height: Some(18.0),
                    ..Default::default()
                },
            ),
        ]);
        let tree = DecorationTree::new(
            DecorationNode::new(DecorationNodeKind::WindowBorder)
                .with_style(DecorationStyle {
                    border: Some(BorderStyle {
                        width: 2.0,
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
                            height: Some(30.0),
                            padding: Edges {
                                left: 8.0,
                                right: 8.0,
                                ..Default::default()
                            },
                            gap: Some(8.0),
                            ..Default::default()
                        })
                        .with_children(vec![
                            DecorationNode::new(DecorationNodeKind::Box(BoxNode::default()))
                                .with_style(DecorationStyle {
                                    flex_grow: Some(1.0),
                                    ..Default::default()
                                }),
                            titlebar_overlay,
                        ]),
                        DecorationNode::new(DecorationNodeKind::WindowSlot),
                    ]),
                ]),
        );

        let client_rect = LogicalRect::new(50, 100, 200, 120);
        let layout = tree
            .layout_for_client(client_rect)
            .expect("layout should succeed");

        assert_eq!(layout.window_slot_rect(), Some(client_rect));
        assert_eq!(layout.root.rect.x, client_rect.x - 2);
        assert_eq!(
            layout.root.rect.x + layout.root.rect.width,
            client_rect.x + client_rect.width + 2
        );
        assert!(layout.bounds_rect().width > layout.root.rect.width);
    }

    #[test]
    fn bare_window_slot_does_not_clip_client_surface() {
        let tree = DecorationTree::new(DecorationNode::new(DecorationNodeKind::WindowSlot));
        let layout = tree
            .layout_for_client_with_scale(LogicalRect::new(50, 100, 800, 600), 1.25)
            .expect("layout should succeed");
        let node_geometry = build_node_geometry_map(&layout);
        let clip = content_clip_for_layout(&tree, &layout, &node_geometry)
            .expect("window slot placement should exist");

        assert!(!clip.clips_surface);
    }

    #[test]
    fn window_slot_style_cannot_clip_client_surface() {
        let tree = DecorationTree::new(
            DecorationNode::new(DecorationNodeKind::WindowSlot).with_style(DecorationStyle {
                overflow: Some(Overflow::Hidden),
                border: Some(BorderStyle {
                    width: 2.0,
                    color: Color::WHITE,
                }),
                border_radius: Some(12.0),
                ..Default::default()
            }),
        );
        let layout = tree
            .layout_for_client_with_scale(LogicalRect::new(50, 100, 800, 600), 1.25)
            .expect("layout should succeed");
        let node_geometry = build_node_geometry_map(&layout);
        let clip = content_clip_for_layout(&tree, &layout, &node_geometry)
            .expect("window slot placement should exist");

        assert!(!clip.clips_surface);
    }

    #[test]
    fn bordered_box_clips_client_surface_unless_overflow_is_visible() {
        let make_tree = |overflow| {
            DecorationTree::new(
                DecorationNode::new(DecorationNodeKind::Box(BoxNode::default()))
                    .with_style(DecorationStyle {
                        border: Some(BorderStyle {
                            width: 2.0,
                            color: Color::WHITE,
                        }),
                        border_radius: Some(12.0),
                        overflow,
                        ..Default::default()
                    })
                    .with_children(vec![DecorationNode::new(DecorationNodeKind::WindowSlot)]),
            )
        };

        for (overflow, expected) in [(None, true), (Some(Overflow::Visible), false)] {
            let tree = make_tree(overflow);
            let layout = tree
                .layout_for_client_with_scale(LogicalRect::new(50, 100, 800, 600), 1.25)
                .expect("layout should succeed");
            let node_geometry = build_node_geometry_map(&layout);
            let clip = content_clip_for_layout(&tree, &layout, &node_geometry)
                .expect("window slot placement should exist");
            assert_eq!(clip.clips_surface, expected);
        }
    }

    #[test]
    fn content_clip_matches_window_slot_not_border_inner() {
        let tree = DecorationTree::new(
            DecorationNode::new(DecorationNodeKind::WindowBorder)
                .with_style(DecorationStyle {
                    border: Some(BorderStyle {
                        width: 2.0,
                        color: Color::WHITE,
                    }),
                    border_radius: Some(18.0),
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
                            height: Some(30.0),
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
        let node_geometry = build_node_geometry_map(&layout);
        let clip = content_clip_for_layout(&tree, &layout, &node_geometry)
            .expect("content clip should exist");

        assert_eq!(clip.rect.loc.x, slot.x);
        assert_eq!(clip.rect.loc.y, slot.y);
        assert_eq!(clip.rect.size.w, slot.width);
        assert_eq!(clip.rect.size.h, slot.height);
        assert_eq!(clip.radius, 0);
        assert_eq!(clip.radius_precise, 0.0);
        assert!(clip.clips_surface);
        assert!(clip.corner_radii.iter().all(|radius| *radius > 0));
        assert!(clip.corner_radii_precise.iter().all(|radius| *radius > 0.0));
    }

    #[test]
    fn content_clip_keeps_all_shared_corners_when_slot_fills_inner_mask() {
        let tree = DecorationTree::new(
            DecorationNode::new(DecorationNodeKind::WindowBorder)
                .with_style(DecorationStyle {
                    border: Some(BorderStyle {
                        width: 2.0,
                        color: Color::WHITE,
                    }),
                    border_radius: Some(18.0),
                    ..Default::default()
                })
                .with_children(vec![DecorationNode::new(DecorationNodeKind::WindowSlot)]),
        );

        let layout = tree
            .layout_for_client_with_scale(LogicalRect::new(50, 100, 800, 600), 1.25)
            .expect("layout should succeed");
        let node_geometry = build_node_geometry_map(&layout);
        let clip = content_clip_for_layout(&tree, &layout, &node_geometry)
            .expect("content clip should exist");

        assert!(clip.clips_surface);
        assert!(clip.corner_radii.iter().all(|radius| *radius > 0));
        assert!(clip.corner_radii_precise.iter().all(|radius| *radius > 0.0));
    }

    #[test]
    fn content_clip_can_use_rounded_overflow_hidden_box_as_mask() {
        let tree = DecorationTree::new(
            DecorationNode::new(DecorationNodeKind::Box(BoxNode {
                direction: LayoutDirection::Column,
            }))
            .with_style(DecorationStyle {
                overflow: Some(Overflow::Hidden),
                border: Some(BorderStyle {
                    width: 2.0,
                    color: Color::WHITE,
                }),
                border_radius: Some(18.0),
                ..Default::default()
            })
            .with_children(vec![DecorationNode::new(DecorationNodeKind::WindowSlot)]),
        );

        let layout = tree
            .layout_for_client_with_scale(LogicalRect::new(50, 100, 800, 600), 1.25)
            .expect("layout should succeed");
        let node_geometry = build_node_geometry_map(&layout);
        let clip = content_clip_for_layout(&tree, &layout, &node_geometry)
            .expect("content clip should exist");

        assert!(clip.clips_surface);
        assert_eq!(clip.radius, 0);
        assert!(clip.corner_radii.iter().all(|radius| *radius > 0));
        assert!(clip.corner_radii_precise.iter().all(|radius| *radius > 0.0));
    }

    #[test]
    fn content_clip_separates_slot_rect_from_ancestor_mask_through_padding_box() {
        let tree = DecorationTree::new(
            DecorationNode::new(DecorationNodeKind::WindowBorder)
                .with_style(DecorationStyle {
                    border: Some(BorderStyle {
                        width: 2.0,
                        color: Color::WHITE,
                    }),
                    border_radius: Some(18.0),
                    ..Default::default()
                })
                .with_children(vec![
                    DecorationNode::new(DecorationNodeKind::Box(BoxNode {
                        direction: LayoutDirection::Column,
                    }))
                    .with_style(DecorationStyle {
                        padding: Edges::all(5.0),
                        ..Default::default()
                    })
                    .with_children(vec![
                        DecorationNode::new(DecorationNodeKind::Box(BoxNode {
                            direction: LayoutDirection::Row,
                        }))
                        .with_style(DecorationStyle {
                            height: Some(30.0),
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
        let node_geometry = build_node_geometry_map(&layout);
        let clip = content_clip_for_layout(&tree, &layout, &node_geometry)
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

    /// Tiled windows are sized root -> client (configure) -> root (layout of
    /// the committed client). Both directions must agree for every size, or
    /// the configure size flips by a pixel on every pass and the client keeps
    /// resizing.
    #[test]
    fn managed_root_and_client_sizes_round_trip_at_fractional_scales() {
        let tree = DecorationTree::new(
            DecorationNode::new(DecorationNodeKind::WindowBorder)
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
                        DecorationNode::new(DecorationNodeKind::WindowSlot),
                    ]),
                ]),
        );

        for scale in [1.0, 1.25, 1.5, 1.6, 1.75, 1.8, 2.0] {
            let mut insets = None;
            for width in 900..960 {
                for (x, y) in [(0, 0), (37, 11)] {
                    let root = LogicalRect::new(x, y, width, width - 7);
                    let client =
                        managed_client_rect_for_root(&tree, root, scale).expect("client rect");
                    let layout = tree
                        .layout_for_client_with_scale(client, scale)
                        .expect("layout");
                    assert_eq!(layout.root.rect, root, "scale {scale} root {root:?}");
                    assert_eq!(layout.window_slot_rect(), Some(client));
                    let current = (
                        client.x - root.x,
                        client.y - root.y,
                        root.width - client.width,
                        root.height - client.height,
                    );
                    assert_eq!(*insets.get_or_insert(current), current, "scale {scale}");
                }
            }
        }
    }

    /// A rect animation resizes in physical-pixel steps: the sub-logical-pixel
    /// part of the root size reaches the physical layout, while every logical
    /// rect (and with it the client configure size) stays on the integer size.
    #[test]
    fn sub_pixel_root_size_grows_the_physical_layout_only() {
        let tree = DecorationTree::new(
            DecorationNode::new(DecorationNodeKind::WindowBorder)
                .with_style(DecorationStyle {
                    border: Some(BorderStyle {
                        width: 2.0,
                        color: Color::WHITE,
                    }),
                    ..Default::default()
                })
                .with_children(vec![DecorationNode::new(DecorationNodeKind::WindowSlot)]),
        );
        let scale = 1.5;
        let client = LogicalRect::new(100, 100, 600, 400);
        let base = tree
            .layout_for_client_with_scale(client, scale)
            .expect("layout");
        let base_root_px = base.root.resolved_rect;
        let base_slot_px = base.root.resolved_window_slot_rect().expect("slot");

        for right in [0.2, 0.34, 0.49] {
            let subpixel = crate::backend::visual::RootSubpixelEdges {
                right,
                bottom: right,
                ..Default::default()
            };
            let layout = tree
                .layout_for_client_with_subpixel(client, scale, subpixel)
                .expect("layout");
            let root = &layout.root;
            let expected_width =
                crate::ssd::round_half_up((base.root.rect.width as f64 + right) * scale);
            assert_eq!(
                root.resolved_rect.width.raw(),
                expected_width,
                "right {right}"
            );
            let slot_px = root.resolved_window_slot_rect().expect("slot");
            assert_eq!(
                slot_px.width.raw() - base_slot_px.width.raw(),
                root.resolved_rect.width.raw() - base_root_px.width.raw(),
                "the slot absorbs the extra pixels"
            );
            assert_eq!(root.rect, base.root.rect);
            assert_eq!(layout.window_slot_rect(), Some(client));
        }
    }

    /// The drag path must keep the pointer's sub-logical-pixel motion.
    ///
    /// A move grab feeds `WindowMoveEventSnapshot::current_rect` to config
    /// code, which sets it as the managed rect; the fractional remainder then
    /// anchors the rendered root origin via `root_physical_origin_precise`.
    /// Rounding the drag delta to whole logical pixels (as the grab used to)
    /// pins the window to a 1.5 physical pixel grid at scale 1.5 — coarser
    /// than the cursor's own single physical pixel, so the window visibly
    /// steps and slips against the cursor it is stuck to.
    #[test]
    fn dragging_a_window_moves_it_one_physical_pixel_at_a_time() {
        let scale = 1.5f64;
        let output_geo =
            smithay::utils::Rectangle::<i32, Logical>::new((0, 0).into(), (2560, 1440).into());
        let output_scale = smithay::utils::Scale::from((scale, scale));
        let initial = LogicalRect::new(763, 349, 968, 813);

        // One physical pixel of pointer travel, expressed in logical units.
        let step = 1.0 / scale;
        let origin_after = |delta_steps: u32, round_delta_to_logical: bool| {
            let mut delta = step * delta_steps as f64;
            if round_delta_to_logical {
                delta = delta.round();
            }
            let dragged = ManagedWindowRectSnapshot {
                x: initial.x as f64 + delta,
                y: initial.y as f64,
                width: initial.width as f64,
                height: initial.height as f64,
            };
            crate::backend::visual::root_physical_origin_precise(
                managed_rect_snapshot_to_logical_rect(dragged),
                managed_rect_snapshot_subpixel_edges(dragged),
                output_geo,
                output_scale,
            )
        };

        let mut steps = Vec::new();
        let mut previous = origin_after(0, false).x;
        for index in 1..=12 {
            let current = origin_after(index, false).x;
            steps.push(current - previous);
            previous = current;
        }
        assert!(
            steps.iter().all(|step| *step == 1),
            "a one-physical-pixel drag step should move the window exactly one physical pixel, got {steps:?}"
        );

        // Same drag with the delta quantized to whole logical pixels: the
        // window stalls, then jumps two physical pixels at once.
        let mut rounded_steps = Vec::new();
        let mut previous = origin_after(0, true).x;
        for index in 1..=12 {
            let current = origin_after(index, true).x;
            rounded_steps.push(current - previous);
            previous = current;
        }
        assert!(
            rounded_steps.iter().any(|step| *step > 1),
            "the rounded-delta baseline should still show multi-pixel jumps, got {rounded_steps:?}"
        );
    }
}
