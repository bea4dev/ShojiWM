//! Typed results every config runtime answers with, the static fallback
//! decoration, and the shared error type.

use super::window_model::{
    GestureSwipeEventSnapshot, ManagedWindowAnimationSnapshot, ManagedWindowState,
    PointerMoveEventSnapshot, WaylandLayerSnapshot, WaylandPopupSnapshot, WaylandWindowAction,
    WaylandWindowSnapshot, WindowActivateRequestEventSnapshot, WindowDecorationClientSnapshot,
    WindowDecorationDecisionSnapshot, WindowDecorationModeSnapshot,
    WindowDecorationPolicyContextSnapshot, WindowFullscreenRequestEventSnapshot,
    WindowMaximizeRequestEventSnapshot, WindowMinimizeRequestEventSnapshot,
    WindowMoveEventSnapshot, WindowResizeEventSnapshot,
};
use super::{
    DecorationBridgeError, DecorationLayoutError, DecorationNode, DecorationTree, EffectInput,
    EffectRegion, WindowEffectConfig, WindowTransform, decode_tree_json,
};
use crate::runtime_workspace::RuntimeWorkspaceActivateRequestSnapshot;

/// Typed call surface of the embedded TypeScript runtime and the static
/// fallback decoration.
///
/// The compositor itself no longer calls this: it talks to whatever config
/// runtime is active through `runtime_api::ConfigRuntime` messages, and
/// `runtime_ts::TypeScriptRuntime` maps those onto these methods.
pub trait DecorationEvaluator {
    fn evaluate_window(
        &self,
        window: &WaylandWindowSnapshot,
        now_ms: u64,
    ) -> Result<DecorationEvaluationResult, DecorationEvaluationError>;

    fn evaluate_window_preview(
        &self,
        window: &WaylandWindowSnapshot,
        now_ms: u64,
    ) -> Result<DecorationEvaluationResult, DecorationEvaluationError> {
        self.evaluate_window(window, now_ms)
    }

    fn window_decoration_policy(
        &self,
        _window: &WaylandWindowSnapshot,
        _context: &WindowDecorationPolicyContextSnapshot,
    ) -> Result<WindowDecorationDecisionSnapshot, DecorationEvaluationError> {
        Ok(WindowDecorationDecisionSnapshot {
            mode: WindowDecorationModeSnapshot::Server,
        })
    }

    /// The legacy KDE decoration manager's default mode for `client`, or
    /// `None` to leave it to the compositor (CSD).
    fn kde_default_decoration_mode(
        &self,
        _client: &WindowDecorationClientSnapshot,
    ) -> Result<Option<WindowDecorationModeSnapshot>, DecorationEvaluationError> {
        Ok(None)
    }

    fn evaluate_cached_window(
        &self,
        _window_id: &str,
        _window: Option<&WaylandWindowSnapshot>,
        _now_ms: u64,
        _force_full_reevaluation: bool,
    ) -> Result<DecorationCachedEvaluationResult, DecorationEvaluationError> {
        Err(DecorationEvaluationError::RuntimeProtocol(
            "cached window evaluation unsupported".into(),
        ))
    }

    fn scheduler_tick(
        &self,
        _now_ms: f64,
        _output: Option<&str>,
        _frame_outputs: &[String],
    ) -> Result<DecorationSchedulerTick, DecorationEvaluationError> {
        Ok(DecorationSchedulerTick::default())
    }

    fn window_closed(&self, _window_id: &str) -> Result<(), DecorationEvaluationError> {
        Ok(())
    }

    fn invoke_handler(
        &self,
        _window_id: &str,
        _handler_id: &str,
        _now_ms: u64,
    ) -> Result<DecorationHandlerInvocation, DecorationEvaluationError> {
        Ok(DecorationHandlerInvocation::default())
    }

    fn start_close(
        &self,
        _window_id: &str,
        _now_ms: u64,
    ) -> Result<DecorationHandlerInvocation, DecorationEvaluationError> {
        Ok(DecorationHandlerInvocation::default())
    }

    fn invoke_key_binding(
        &self,
        _binding_id: &str,
        _now_ms: u64,
    ) -> Result<DecorationKeyBindingInvocation, DecorationEvaluationError> {
        Ok(DecorationKeyBindingInvocation::default())
    }

    fn workspace_activate(
        &self,
        _event: &RuntimeWorkspaceActivateRequestSnapshot,
        _now_ms: u64,
    ) -> Result<DecorationHandlerInvocation, DecorationEvaluationError> {
        Ok(DecorationHandlerInvocation::default())
    }

    fn window_resize(
        &self,
        _window_id: &str,
        _event: &WindowResizeEventSnapshot,
        _now_ms: u64,
    ) -> Result<DecorationWindowResizeInvocation, DecorationEvaluationError> {
        Ok(DecorationWindowResizeInvocation::default())
    }

    fn window_move(
        &self,
        _window_id: &str,
        _event: &WindowMoveEventSnapshot,
        _now_ms: u64,
    ) -> Result<DecorationWindowMoveInvocation, DecorationEvaluationError> {
        Ok(DecorationWindowMoveInvocation::default())
    }

    fn window_maximize_request(
        &self,
        _snapshot: &WaylandWindowSnapshot,
        _event: &WindowMaximizeRequestEventSnapshot,
        _now_ms: u64,
    ) -> Result<DecorationWindowStateRequestInvocation, DecorationEvaluationError> {
        Ok(DecorationWindowStateRequestInvocation::default())
    }

    fn window_minimize_request(
        &self,
        _snapshot: &WaylandWindowSnapshot,
        _event: &WindowMinimizeRequestEventSnapshot,
        _now_ms: u64,
    ) -> Result<DecorationWindowStateRequestInvocation, DecorationEvaluationError> {
        Ok(DecorationWindowStateRequestInvocation::default())
    }

    fn window_fullscreen_request(
        &self,
        _snapshot: &WaylandWindowSnapshot,
        _event: &WindowFullscreenRequestEventSnapshot,
        _now_ms: u64,
    ) -> Result<DecorationWindowStateRequestInvocation, DecorationEvaluationError> {
        Ok(DecorationWindowStateRequestInvocation::default())
    }

    fn window_activate_request(
        &self,
        _snapshot: &WaylandWindowSnapshot,
        _event: &WindowActivateRequestEventSnapshot,
        _now_ms: u64,
    ) -> Result<DecorationWindowStateRequestInvocation, DecorationEvaluationError> {
        Ok(DecorationWindowStateRequestInvocation::default())
    }

    fn pointer_move(
        &self,
        _event: &PointerMoveEventSnapshot,
        _now_ms: u64,
    ) -> Result<DecorationPointerMoveAsyncInvocation, DecorationEvaluationError> {
        Ok(DecorationPointerMoveAsyncInvocation::default())
    }

    fn pointer_move_async(&self, _event: PointerMoveEventSnapshot, _now_ms: u64) {}

    fn gesture_swipe(
        &self,
        _event: &GestureSwipeEventSnapshot,
        _now_ms: u64,
    ) -> Result<DecorationGestureSwipeAsyncInvocation, DecorationEvaluationError> {
        Ok(DecorationGestureSwipeAsyncInvocation::default())
    }

    fn gesture_swipe_async(&self, _event: GestureSwipeEventSnapshot, _now_ms: u64) {}

    fn evaluate_layer_effects(
        &self,
        _output_name: &str,
        _layers: &[WaylandLayerSnapshot],
        _now_ms: u64,
    ) -> Result<LayerEffectEvaluationResult, DecorationEvaluationError> {
        Ok(LayerEffectEvaluationResult::default())
    }

    fn evaluate_popup_effects(
        &self,
        _output_name: &str,
        _popups: &[WaylandPopupSnapshot],
        _now_ms: u64,
    ) -> Result<PopupEffectEvaluationResult, DecorationEvaluationError> {
        Ok(PopupEffectEvaluationResult::default())
    }
}

#[derive(Debug, Clone)]
pub struct DecorationEvaluationResult {
    pub node: DecorationNode,
    pub transform: WindowTransform,
    pub managed_window: ManagedWindowState,
    pub window_effects: Option<WindowEffectConfig>,
    pub dirty_node_ids: Vec<String>,
    pub next_poll_in_ms: Option<u64>,
    /// Window actions (typically scheduleAnimation / cancelAnimation) queued
    /// by user handlers during this evaluation. Returned in-band so the
    /// compositor can apply them *before* sampling animations for the same
    /// refresh — fixing the one-frame flash at the static target position
    /// before open / first-commit animations kick in.
    pub actions: Vec<RuntimeWindowAction>,
}

#[derive(Debug, Clone)]
pub struct DecorationCachedEvaluationResult {
    pub node: Option<DecorationNode>,
    pub node_patches: Vec<crate::runtime_api::CompositionPatch>,
    pub transform: WindowTransform,
    pub managed_window: ManagedWindowState,
    pub window_effects: Option<WindowEffectConfig>,
    pub window_effect_uniform_only: bool,
    pub dirty_node_ids: Vec<String>,
    pub managed_window_only: bool,
    pub next_poll_in_ms: Option<u64>,
    /// See `DecorationEvaluationResult::actions`. Same role on the cached path.
    pub actions: Vec<RuntimeWindowAction>,
}

impl From<DecorationEvaluationResult> for DecorationCachedEvaluationResult {
    fn from(result: DecorationEvaluationResult) -> Self {
        Self {
            node: Some(result.node),
            node_patches: Vec::new(),
            transform: result.transform,
            managed_window: result.managed_window,
            window_effects: result.window_effects,
            window_effect_uniform_only: false,
            dirty_node_ids: result.dirty_node_ids,
            managed_window_only: false,
            next_poll_in_ms: result.next_poll_in_ms,
            actions: result.actions,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct DecorationSchedulerTick {
    pub dirty: bool,
    pub runtime_dirty: bool,
    pub dirty_window_ids: Vec<String>,
    pub dirty_managed_window_ids: Vec<String>,
    pub dirty_window_node_ids: std::collections::HashMap<String, Vec<String>>,
    pub dirty_layer_ids: Vec<String>,
    pub dirty_layer_node_ids: std::collections::HashMap<String, Vec<String>>,
    pub actions: Vec<RuntimeWindowAction>,
    pub next_poll_in_ms: Option<u64>,
}

#[derive(Debug, Clone, Default)]
pub struct DecorationHandlerInvocation {
    pub invoked: bool,
    /// Close-animation duration the config declared via
    /// `window.setCloseAnimationDuration(...)`. Only populated by
    /// `start_close` responses; the closing-snapshot watchdog derives its
    /// per-window finalize deadline from this.
    pub close_animation_duration_ms: Option<u64>,
    pub node: Option<DecorationNode>,
    pub transform: Option<WindowTransform>,
    pub managed_window: Option<ManagedWindowState>,
    pub window_effects: Option<WindowEffectConfig>,
    pub dirty_window_ids: Vec<String>,
    pub dirty_managed_window_ids: Vec<String>,
    pub dirty_window_node_ids: std::collections::HashMap<String, Vec<String>>,
    pub actions: Vec<RuntimeWindowAction>,
    pub next_poll_in_ms: Option<u64>,
}

#[derive(Debug, Clone, Default)]
pub struct DecorationKeyBindingInvocation {
    pub invoked: bool,
    pub dirty: bool,
    pub dirty_window_ids: Vec<String>,
    pub dirty_managed_window_ids: Vec<String>,
    pub dirty_window_node_ids: std::collections::HashMap<String, Vec<String>>,
    pub dirty_layer_node_ids: std::collections::HashMap<String, Vec<String>>,
    pub actions: Vec<RuntimeWindowAction>,
    pub next_poll_in_ms: Option<u64>,
}

#[derive(Debug, Clone, Default)]
pub struct DecorationWindowResizeInvocation {
    pub invoked: bool,
    pub dirty: bool,
    pub dirty_window_ids: Vec<String>,
    pub dirty_managed_window_ids: Vec<String>,
    pub dirty_window_node_ids: std::collections::HashMap<String, Vec<String>>,
    pub dirty_layer_node_ids: std::collections::HashMap<String, Vec<String>>,
    pub actions: Vec<RuntimeWindowAction>,
    pub next_poll_in_ms: Option<u64>,
}

#[derive(Debug, Clone, Default)]
pub struct DecorationWindowMoveInvocation {
    pub invoked: bool,
    pub dirty: bool,
    pub dirty_window_ids: Vec<String>,
    pub dirty_managed_window_ids: Vec<String>,
    pub dirty_window_node_ids: std::collections::HashMap<String, Vec<String>>,
    pub dirty_layer_node_ids: std::collections::HashMap<String, Vec<String>>,
    pub actions: Vec<RuntimeWindowAction>,
    pub next_poll_in_ms: Option<u64>,
}

#[derive(Debug, Clone, Default)]
pub struct DecorationWindowStateRequestInvocation {
    pub invoked: bool,
    pub dirty: bool,
    pub dirty_window_ids: Vec<String>,
    pub dirty_managed_window_ids: Vec<String>,
    pub dirty_window_node_ids: std::collections::HashMap<String, Vec<String>>,
    pub dirty_layer_node_ids: std::collections::HashMap<String, Vec<String>>,
    pub actions: Vec<RuntimeWindowAction>,
    pub next_poll_in_ms: Option<u64>,
}

#[derive(Debug, Clone, Default)]
pub struct DecorationPointerMoveAsyncInvocation {
    pub invoked: bool,
    pub dirty: bool,
    pub dirty_window_ids: Vec<String>,
    pub dirty_managed_window_ids: Vec<String>,
    pub dirty_window_node_ids: std::collections::HashMap<String, Vec<String>>,
    pub dirty_layer_node_ids: std::collections::HashMap<String, Vec<String>>,
    pub actions: Vec<RuntimeWindowAction>,
    pub next_poll_in_ms: Option<u64>,
}

pub type DecorationGestureSwipeAsyncInvocation = DecorationPointerMoveAsyncInvocation;

#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeEventConfigUpdate {
    #[serde(default)]
    pub pointer_move: bool,
    #[serde(default)]
    pub pointer_move_async: bool,
    #[serde(default)]
    pub gesture_swipe: bool,
    #[serde(default)]
    pub gesture_swipe_async: bool,
    /// Touchpad pinches (`RuntimeEvent::GesturePinch`).
    #[serde(default)]
    pub gesture_pinch_async: bool,
    /// Drag and drop events (`InputRequest::Dnd`).
    #[serde(default)]
    pub dnd: bool,
}

#[derive(Debug, Clone, Default)]
pub struct LayerEffectEvaluationResult {
    pub effects: Vec<RuntimeLayerEffectAssignment>,
    pub next_poll_in_ms: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct RuntimeLayerEffectAssignment {
    pub layer_id: String,
    pub effects: Option<WindowEffectConfig>,
}

#[derive(Debug, Clone, Default)]
pub struct PopupEffectEvaluationResult {
    pub effects: Vec<RuntimePopupEffectAssignment>,
    pub next_poll_in_ms: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct RuntimePopupEffectAssignment {
    pub popup_id: String,
    pub effects: Option<WindowEffectConfig>,
    /// `COMPOSITOR.rendering.surfacePolicy` result for this popup's surface.
    pub surface_policy: Option<super::SurfacePolicy>,
}

pub fn validate_popup_effect_config(
    effects: WindowEffectConfig,
) -> Result<WindowEffectConfig, DecorationBridgeError> {
    let is_popup_source =
        |slot: &super::WindowEffectSlot| matches!(slot.effect.input, EffectInput::PopupSource(_));
    // `behind` additionally accepts backdrop inputs that can be resolved from
    // the framebuffer at draw time. They may sample a pre-captured popup
    // source, but not xray/window/layer sources: popups render inline with
    // their parent's element stream, so there is no offline scene capture.
    if effects.behind.as_ref().is_some_and(|slot| {
        !is_popup_source(slot) && !slot.effect.supports_popup_framebuffer_backdrop()
    }) || effects
        .behind_root_surface
        .as_ref()
        .is_some_and(|slot| !is_popup_source(slot))
        || effects
            .in_front
            .as_ref()
            .is_some_and(|slot| !is_popup_source(slot))
        || effects
            .replace
            .as_ref()
            .is_some_and(|slot| !is_popup_source(slot))
        // Subsurfaces are split out of toplevel windows only.
        || effects.replace_subsurfaces.is_some()
        || effects.behind_subsurfaces.is_some()
        // Regions narrow layer backdrops only.
        || [
            &effects.behind,
            &effects.behind_root_surface,
            &effects.in_front,
            &effects.replace,
        ]
        .into_iter()
        .any(slot_narrowed_to_region)
    {
        return Err(DecorationBridgeError::InvalidEffectInput);
    }
    Ok(effects)
}

pub fn validate_layer_effect_config(
    effects: WindowEffectConfig,
) -> Result<WindowEffectConfig, DecorationBridgeError> {
    let is_layer_source =
        |slot: &super::WindowEffectSlot| matches!(slot.effect.input, EffectInput::LayerSource(_));
    if effects
        .behind
        .as_ref()
        .is_some_and(|slot| !is_layer_source(slot) && !slot.effect.is_backdrop())
        || effects
            .behind_root_surface
            .as_ref()
            .is_some_and(|slot| !is_layer_source(slot))
        || effects
            .in_front
            .as_ref()
            .is_some_and(|slot| !is_layer_source(slot))
        || effects
            .replace
            .as_ref()
            .is_some_and(|slot| !is_layer_source(slot))
        || effects.replace_subsurfaces.is_some()
        || effects.behind_subsurfaces.is_some()
        // Only a backdrop `behind` can be narrowed to a region: the layer itself is
        // still drawn whole, so every other slot has to cover the whole surface.
        || effects
            .behind
            .as_ref()
            .is_some_and(|slot| slot.region != EffectRegion::Surface && !slot.effect.is_backdrop())
        || [&effects.behind_root_surface, &effects.in_front, &effects.replace]
            .into_iter()
            .any(slot_narrowed_to_region)
    {
        return Err(DecorationBridgeError::InvalidEffectInput);
    }
    Ok(effects)
}

fn slot_narrowed_to_region(slot: &Option<super::WindowEffectSlot>) -> bool {
    slot.as_ref()
        .is_some_and(|slot| slot.region != EffectRegion::Surface)
}

#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub struct RuntimeWindowAction {
    #[serde(rename = "windowId")]
    pub window_id: String,
    pub action: WaylandWindowAction,
    #[serde(default)]
    pub animation: Option<ManagedWindowAnimationSnapshot>,
    #[serde(default)]
    pub channel: Option<String>,
}

/// Temporary Rust-side evaluator that mirrors the intended TS-level behavior:
///
/// - focused windows get a yellow border
/// - unfocused windows get a white border
/// - title is reflected into a label node
///
/// This exists only to establish the per-window reevaluation flow for milestone 3.
#[derive(Debug, Default, Clone, Copy)]
pub struct StaticDecorationEvaluator;

impl DecorationEvaluator for StaticDecorationEvaluator {
    fn evaluate_window(
        &self,
        window: &WaylandWindowSnapshot,
        _now_ms: u64,
    ) -> Result<DecorationEvaluationResult, DecorationEvaluationError> {
        let border_color = if window.is_focused {
            "#ffff00"
        } else {
            "#ffffff"
        };

        let json = format!(
            r##"{{
                "kind": "WindowBorder",
                "props": {{
                    "style": {{
                        "border": {{ "px": 1, "color": "{border_color}" }}
                    }}
                }},
                "children": [
                    {{
                        "kind": "Box",
                        "props": {{
                            "direction": "column"
                        }},
                        "children": [
                            {{
                                "kind": "Box",
                                "props": {{
                                    "direction": "row",
                                    "style": {{
                                        "height": 28,
                                        "paddingX": 8,
                                        "gap": 8
                                    }}
                                }},
                                "children": [
                                    {{
                                        "kind": "Label",
                                        "props": {{
                                            "text": {title:?}
                                        }},
                                        "children": []
                                    }},
                                    {{
                                        "kind": "Box",
                                        "props": {{
                                            "style": {{ "flexGrow": 1 }}
                                        }},
                                        "children": []
                                    }},
                                    {{
                                        "kind": "Button",
                                        "props": {{
                                            "onClick": "close"
                                        }},
                                        "children": []
                                    }}
                                ]
                            }},
                            {{
                                "kind": "Window",
                                "props": {{}},
                                "children": []
                            }}
                        ]
                    }}
                ]
            }}"##,
            title = window.title,
        );

        Ok(DecorationEvaluationResult {
            node: decode_tree_json(&json)?,
            transform: WindowTransform::default(),
            managed_window: ManagedWindowState::default(),
            window_effects: None,
            dirty_node_ids: Vec::new(),
            next_poll_in_ms: None,
            actions: Vec::new(),
        })
    }
}

pub fn evaluate_dynamic_decoration<E: DecorationEvaluator>(
    evaluator: &E,
    window: &WaylandWindowSnapshot,
    now_ms: u64,
) -> Result<DecorationTree, DecorationEvaluationError> {
    evaluator
        .evaluate_window(window, now_ms)
        .map(|result| DecorationTree::new(result.node))
}

#[derive(Debug, thiserror::Error)]
pub enum DecorationEvaluationError {
    #[error(transparent)]
    Bridge(#[from] DecorationBridgeError),
    #[error("failed to compute decoration layout: {0:?}")]
    Layout(DecorationLayoutError),
    #[error("failed to serialize window snapshot for evaluation: {0}")]
    SnapshotSerialization(String),
    #[error("failed to execute decoration runtime: {0}")]
    Io(#[from] std::io::Error),
    #[error("decoration runtime exited with status {status}: {stderr}")]
    RuntimeFailed { status: i32, stderr: String },
    #[error("decoration runtime returned invalid utf-8 output")]
    InvalidUtf8,
    #[error("decoration runtime returned invalid json: {0}")]
    InvalidResponse(String),
    #[error("decoration runtime protocol error: {0}")]
    RuntimeProtocol(String),
    #[error("{0} is not supported by this config runtime")]
    Unsupported(&'static str),
}
