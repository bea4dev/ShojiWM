use std::{
    path::PathBuf,
    sync::{
        Arc, Condvar, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use tracing::{debug, info, warn};

use super::embedded_runtime::{
    EmbeddedRuntime, EmbeddedRuntimeResponse, NativeCachedResponse, NativeCompositionPatch,
    NativeCompositionRequest, NativeCompositionUpdate, NativeEffectRequest, NativeEffectUpdate,
    NativeInteractionRequest, NativeInteractionResponse, NativeSchedulerRequest,
    NativeSchedulerResponse,
};
use super::window_model::{
    GestureSwipeEventSnapshot, GestureSwipePhaseSnapshot, ManagedWindowAnimationSnapshot,
    ManagedWindowState, PointerMoveEventSnapshot, WaylandLayerSnapshot, WaylandOutputSnapshot,
    WaylandPopupSnapshot, WaylandWindowAction, WaylandWindowSnapshot,
    WindowActivateRequestEventSnapshot, WindowDecorationDecisionSnapshot,
    WindowDecorationModeSnapshot, WindowDecorationPolicyContextSnapshot,
    WindowFullscreenRequestEventSnapshot, WindowMaximizeRequestEventSnapshot,
    WindowMinimizeRequestEventSnapshot, WindowMoveEventSnapshot, WindowResizeEventSnapshot,
};
use super::{
    BackgroundEffectConfig, DecorationBridgeError, DecorationLayoutError, DecorationNode,
    DecorationTree, EffectInput, WindowEffectConfig, WindowTransform, decode_tree_json,
};
use crate::{
    activation_environment::{RuntimeEnvUpdates, apply_runtime_env_updates},
    config::RuntimeDisplayConfigUpdate,
    runtime_debug::RuntimeDebugConfigUpdate,
    runtime_input::{RuntimeInputConfigUpdate, RuntimeInputDeviceSnapshot},
    runtime_key_binding::RuntimeKeyBindingConfigUpdate,
    runtime_pointer::RuntimePointerConfigUpdate,
    runtime_process::{RuntimeProcessAction, RuntimeProcessConfigUpdate},
    runtime_workspace::{RuntimeWorkspaceActivateRequestSnapshot, RuntimeWorkspaceConfigUpdate},
};
use smithay::reexports::calloop::channel::Sender as CalloopSender;

fn managed_rect_debug_enabled() -> bool {
    std::env::var_os("SHOJI_MANAGED_RECT_DEBUG")
        .is_some_and(|value| value != "0" && !value.is_empty())
}

/// Dynamic decoration evaluation boundary.
///
/// This trait represents the hand-off point to the embedded TypeScript runtime. It allows
/// ShojiWM to build and validate window-aware decoration trees while keeping the dynamic
/// evaluation contract explicit.
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
        _now_ms: u64,
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
    pub display_config: Option<RuntimeDisplayConfigUpdate>,
    pub workspace_config: Option<RuntimeWorkspaceConfigUpdate>,
    pub key_binding_config: Option<RuntimeKeyBindingConfigUpdate>,
    pub pointer_config: Option<RuntimePointerConfigUpdate>,
    pub input_config: Option<RuntimeInputConfigUpdate>,
    pub event_config: Option<RuntimeEventConfigUpdate>,
    pub process_config: Option<RuntimeProcessConfigUpdate>,
    pub process_actions: Vec<RuntimeProcessAction>,
}

#[derive(Debug, Clone)]
pub struct DecorationCachedEvaluationResult {
    pub node: Option<DecorationNode>,
    pub node_patches: Vec<NativeCompositionPatch>,
    pub transform: WindowTransform,
    pub managed_window: ManagedWindowState,
    pub window_effects: Option<WindowEffectConfig>,
    pub window_effect_uniform_only: bool,
    pub dirty_node_ids: Vec<String>,
    pub managed_window_only: bool,
    pub next_poll_in_ms: Option<u64>,
    /// See `DecorationEvaluationResult::actions`. Same role on the cached path.
    pub actions: Vec<RuntimeWindowAction>,
    pub display_config: Option<RuntimeDisplayConfigUpdate>,
    pub workspace_config: Option<RuntimeWorkspaceConfigUpdate>,
    pub key_binding_config: Option<RuntimeKeyBindingConfigUpdate>,
    pub pointer_config: Option<RuntimePointerConfigUpdate>,
    pub input_config: Option<RuntimeInputConfigUpdate>,
    pub event_config: Option<RuntimeEventConfigUpdate>,
    pub process_config: Option<RuntimeProcessConfigUpdate>,
    pub process_actions: Vec<RuntimeProcessAction>,
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
            display_config: result.display_config,
            workspace_config: result.workspace_config,
            key_binding_config: result.key_binding_config,
            pointer_config: result.pointer_config,
            input_config: result.input_config,
            event_config: result.event_config,
            process_config: result.process_config,
            process_actions: result.process_actions,
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
    pub display_config: Option<RuntimeDisplayConfigUpdate>,
    pub workspace_config: Option<RuntimeWorkspaceConfigUpdate>,
    pub key_binding_config: Option<RuntimeKeyBindingConfigUpdate>,
    pub pointer_config: Option<RuntimePointerConfigUpdate>,
    pub input_config: Option<RuntimeInputConfigUpdate>,
    pub event_config: Option<RuntimeEventConfigUpdate>,
    pub process_config: Option<RuntimeProcessConfigUpdate>,
    pub process_actions: Vec<RuntimeProcessAction>,
    pub debug_config: Option<RuntimeDebugConfigUpdate>,
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
    pub display_config: Option<RuntimeDisplayConfigUpdate>,
    pub workspace_config: Option<RuntimeWorkspaceConfigUpdate>,
    pub key_binding_config: Option<RuntimeKeyBindingConfigUpdate>,
    pub pointer_config: Option<RuntimePointerConfigUpdate>,
    pub input_config: Option<RuntimeInputConfigUpdate>,
    pub event_config: Option<RuntimeEventConfigUpdate>,
    pub process_config: Option<RuntimeProcessConfigUpdate>,
    pub process_actions: Vec<RuntimeProcessAction>,
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
    pub display_config: Option<RuntimeDisplayConfigUpdate>,
    pub workspace_config: Option<RuntimeWorkspaceConfigUpdate>,
    pub key_binding_config: Option<RuntimeKeyBindingConfigUpdate>,
    pub pointer_config: Option<RuntimePointerConfigUpdate>,
    pub input_config: Option<RuntimeInputConfigUpdate>,
    pub event_config: Option<RuntimeEventConfigUpdate>,
    pub process_config: Option<RuntimeProcessConfigUpdate>,
    pub process_actions: Vec<RuntimeProcessAction>,
    pub debug_config: Option<RuntimeDebugConfigUpdate>,
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
    pub display_config: Option<RuntimeDisplayConfigUpdate>,
    pub workspace_config: Option<RuntimeWorkspaceConfigUpdate>,
    pub key_binding_config: Option<RuntimeKeyBindingConfigUpdate>,
    pub pointer_config: Option<RuntimePointerConfigUpdate>,
    pub input_config: Option<RuntimeInputConfigUpdate>,
    pub event_config: Option<RuntimeEventConfigUpdate>,
    pub process_config: Option<RuntimeProcessConfigUpdate>,
    pub process_actions: Vec<RuntimeProcessAction>,
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
    pub display_config: Option<RuntimeDisplayConfigUpdate>,
    pub workspace_config: Option<RuntimeWorkspaceConfigUpdate>,
    pub key_binding_config: Option<RuntimeKeyBindingConfigUpdate>,
    pub pointer_config: Option<RuntimePointerConfigUpdate>,
    pub input_config: Option<RuntimeInputConfigUpdate>,
    pub event_config: Option<RuntimeEventConfigUpdate>,
    pub process_config: Option<RuntimeProcessConfigUpdate>,
    pub process_actions: Vec<RuntimeProcessAction>,
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
    pub display_config: Option<RuntimeDisplayConfigUpdate>,
    pub workspace_config: Option<RuntimeWorkspaceConfigUpdate>,
    pub key_binding_config: Option<RuntimeKeyBindingConfigUpdate>,
    pub pointer_config: Option<RuntimePointerConfigUpdate>,
    pub input_config: Option<RuntimeInputConfigUpdate>,
    pub event_config: Option<RuntimeEventConfigUpdate>,
    pub process_config: Option<RuntimeProcessConfigUpdate>,
    pub process_actions: Vec<RuntimeProcessAction>,
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
    pub display_config: Option<RuntimeDisplayConfigUpdate>,
    pub workspace_config: Option<RuntimeWorkspaceConfigUpdate>,
    pub key_binding_config: Option<RuntimeKeyBindingConfigUpdate>,
    pub pointer_config: Option<RuntimePointerConfigUpdate>,
    pub input_config: Option<RuntimeInputConfigUpdate>,
    pub event_config: Option<RuntimeEventConfigUpdate>,
    pub process_config: Option<RuntimeProcessConfigUpdate>,
    pub process_actions: Vec<RuntimeProcessAction>,
}

pub type DecorationGestureSwipeAsyncInvocation = DecorationPointerMoveAsyncInvocation;

#[derive(Debug, Clone)]
pub enum DecorationRuntimeAsyncInvocation {
    PointerMove(DecorationPointerMoveAsyncInvocation),
    GestureSwipe(DecorationGestureSwipeAsyncInvocation),
    CursorConfig(crate::cursor::RuntimeCursorConfigUpdate),
}

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
}

#[derive(Debug, Clone, Default)]
pub struct LayerEffectEvaluationResult {
    pub effects: Vec<RuntimeLayerEffectAssignment>,
    pub next_poll_in_ms: Option<u64>,
    pub display_config: Option<RuntimeDisplayConfigUpdate>,
    pub workspace_config: Option<RuntimeWorkspaceConfigUpdate>,
    pub key_binding_config: Option<RuntimeKeyBindingConfigUpdate>,
    pub pointer_config: Option<RuntimePointerConfigUpdate>,
    pub input_config: Option<RuntimeInputConfigUpdate>,
    pub event_config: Option<RuntimeEventConfigUpdate>,
    pub process_config: Option<RuntimeProcessConfigUpdate>,
    pub process_actions: Vec<RuntimeProcessAction>,
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
    pub display_config: Option<RuntimeDisplayConfigUpdate>,
    pub workspace_config: Option<RuntimeWorkspaceConfigUpdate>,
    pub key_binding_config: Option<RuntimeKeyBindingConfigUpdate>,
    pub pointer_config: Option<RuntimePointerConfigUpdate>,
    pub input_config: Option<RuntimeInputConfigUpdate>,
    pub event_config: Option<RuntimeEventConfigUpdate>,
    pub process_config: Option<RuntimeProcessConfigUpdate>,
    pub process_actions: Vec<RuntimeProcessAction>,
}

#[derive(Debug, Clone)]
pub struct RuntimePopupEffectAssignment {
    pub popup_id: String,
    pub effects: Option<WindowEffectConfig>,
    /// `COMPOSITOR.rendering.surfacePolicy` result for this popup's surface.
    pub surface_policy: Option<super::SurfacePolicy>,
}

fn validate_popup_effect_config(
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
    {
        return Err(DecorationBridgeError::InvalidEffectInput);
    }
    Ok(effects)
}

fn validate_layer_effect_config(
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
    {
        return Err(DecorationBridgeError::InvalidEffectInput);
    }
    Ok(effects)
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
            display_config: None,
            workspace_config: None,
            key_binding_config: None,
            pointer_config: None,
            input_config: None,
            event_config: None,
            process_config: None,
            process_actions: Vec::new(),
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
}

pub struct EmbeddedDecorationEvaluator {
    script_path: PathBuf,
    config_path: PathBuf,
    working_dir: Option<PathBuf>,
    runtime: Arc<Mutex<Option<EmbeddedDecorationRuntime>>>,
    display_state: Arc<Mutex<std::collections::BTreeMap<String, WaylandOutputSnapshot>>>,
    input_state: Arc<Mutex<std::collections::BTreeMap<String, RuntimeInputDeviceSnapshot>>>,
    runtime_state_generation: Arc<AtomicU64>,
    pointer_move_async: Arc<PointerMoveAsyncDispatcher>,
    async_event_sender: Arc<Mutex<Option<CalloopSender<DecorationRuntimeAsyncInvocation>>>>,
}

#[derive(Debug)]
enum RuntimeAsyncWork {
    PointerMove {
        event: PointerMoveEventSnapshot,
        now_ms: u64,
    },
    GestureSwipe {
        event: GestureSwipeEventSnapshot,
        now_ms: u64,
    },
}

#[derive(Debug, Default)]
struct PointerMoveAsyncDispatcher {
    pending: Mutex<Option<RuntimeAsyncWork>>,
    pending_changed: Condvar,
    worker_started: AtomicBool,
    // The worker now outlives every reload, so it has to be told when the runtime
    // cell is empty: false from the head of `lifecycle_disable` until a
    // `lifecycle_enable` succeeds.
    runtime_dispatchable: AtomicBool,
    // Bumped per reload so an invocation produced by the outgoing isolate is
    // dropped instead of clobbering the freshly loaded config.
    epoch: AtomicU64,
    shutdown: AtomicBool,
}

struct EmbeddedDecorationRuntime {
    child: EmbeddedRuntime,
    next_request_id: u64,
    stderr_log: Arc<Mutex<String>>,
    async_event_sender: Arc<Mutex<Option<CalloopSender<DecorationRuntimeAsyncInvocation>>>>,
    last_sent_runtime_state_generation: u64,
}

#[derive(serde::Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum RuntimeRequest<'a> {
    DrainPreload {
        #[serde(rename = "requestId")]
        request_id: u64,
    },
    WindowDecorationPolicy {
        #[serde(rename = "requestId")]
        request_id: u64,
        snapshot: &'a WaylandWindowSnapshot,
        context: &'a WindowDecorationPolicyContextSnapshot,
        #[serde(rename = "displayState")]
        display_state: &'a std::collections::BTreeMap<String, WaylandOutputSnapshot>,
        #[serde(rename = "inputState")]
        input_state: &'a std::collections::BTreeMap<String, RuntimeInputDeviceSnapshot>,
    },
    WindowClosed {
        #[serde(rename = "requestId")]
        request_id: u64,
        #[serde(rename = "windowId")]
        window_id: &'a str,
        #[serde(rename = "displayState")]
        display_state: &'a std::collections::BTreeMap<String, WaylandOutputSnapshot>,
        #[serde(rename = "inputState")]
        input_state: &'a std::collections::BTreeMap<String, RuntimeInputDeviceSnapshot>,
    },
    InvokeHandler {
        #[serde(rename = "requestId")]
        request_id: u64,
        #[serde(rename = "windowId")]
        window_id: &'a str,
        #[serde(rename = "handlerId")]
        handler_id: &'a str,
        #[serde(rename = "nowMs")]
        now_ms: u64,
        #[serde(rename = "displayState")]
        display_state: &'a std::collections::BTreeMap<String, WaylandOutputSnapshot>,
        #[serde(rename = "inputState")]
        input_state: &'a std::collections::BTreeMap<String, RuntimeInputDeviceSnapshot>,
    },
    InvokeKeyBinding {
        #[serde(rename = "requestId")]
        request_id: u64,
        #[serde(rename = "bindingId")]
        binding_id: &'a str,
        #[serde(rename = "nowMs")]
        now_ms: u64,
        #[serde(rename = "displayState")]
        display_state: &'a std::collections::BTreeMap<String, WaylandOutputSnapshot>,
        #[serde(rename = "inputState")]
        input_state: &'a std::collections::BTreeMap<String, RuntimeInputDeviceSnapshot>,
    },
    WorkspaceActivate {
        #[serde(rename = "requestId")]
        request_id: u64,
        #[serde(rename = "workspaceId")]
        workspace_id: &'a str,
        #[serde(rename = "groupId")]
        #[serde(skip_serializing_if = "Option::is_none")]
        group_id: Option<&'a str>,
        #[serde(rename = "nowMs")]
        now_ms: u64,
        #[serde(rename = "displayState")]
        display_state: &'a std::collections::BTreeMap<String, WaylandOutputSnapshot>,
        #[serde(rename = "inputState")]
        input_state: &'a std::collections::BTreeMap<String, RuntimeInputDeviceSnapshot>,
    },
    WindowMaximizeRequest {
        #[serde(rename = "requestId")]
        request_id: u64,
        #[serde(rename = "windowId")]
        window_id: &'a str,
        snapshot: &'a WaylandWindowSnapshot,
        event: &'a WindowMaximizeRequestEventSnapshot,
        #[serde(rename = "nowMs")]
        now_ms: u64,
        #[serde(rename = "displayState")]
        display_state: &'a std::collections::BTreeMap<String, WaylandOutputSnapshot>,
        #[serde(rename = "inputState")]
        input_state: &'a std::collections::BTreeMap<String, RuntimeInputDeviceSnapshot>,
    },
    WindowMinimizeRequest {
        #[serde(rename = "requestId")]
        request_id: u64,
        #[serde(rename = "windowId")]
        window_id: &'a str,
        snapshot: &'a WaylandWindowSnapshot,
        event: &'a WindowMinimizeRequestEventSnapshot,
        #[serde(rename = "nowMs")]
        now_ms: u64,
        #[serde(rename = "displayState")]
        display_state: &'a std::collections::BTreeMap<String, WaylandOutputSnapshot>,
        #[serde(rename = "inputState")]
        input_state: &'a std::collections::BTreeMap<String, RuntimeInputDeviceSnapshot>,
    },
    WindowFullscreenRequest {
        #[serde(rename = "requestId")]
        request_id: u64,
        #[serde(rename = "windowId")]
        window_id: &'a str,
        snapshot: &'a WaylandWindowSnapshot,
        event: &'a WindowFullscreenRequestEventSnapshot,
        #[serde(rename = "nowMs")]
        now_ms: u64,
        #[serde(rename = "displayState")]
        display_state: &'a std::collections::BTreeMap<String, WaylandOutputSnapshot>,
        #[serde(rename = "inputState")]
        input_state: &'a std::collections::BTreeMap<String, RuntimeInputDeviceSnapshot>,
    },
    WindowActivateRequest {
        #[serde(rename = "requestId")]
        request_id: u64,
        #[serde(rename = "windowId")]
        window_id: &'a str,
        snapshot: &'a WaylandWindowSnapshot,
        event: &'a WindowActivateRequestEventSnapshot,
        #[serde(rename = "nowMs")]
        now_ms: u64,
        #[serde(rename = "displayState")]
        display_state: &'a std::collections::BTreeMap<String, WaylandOutputSnapshot>,
        #[serde(rename = "inputState")]
        input_state: &'a std::collections::BTreeMap<String, RuntimeInputDeviceSnapshot>,
    },
    StartClose {
        #[serde(rename = "requestId")]
        request_id: u64,
        #[serde(rename = "windowId")]
        window_id: &'a str,
        #[serde(rename = "nowMs")]
        now_ms: u64,
        #[serde(rename = "displayState")]
        display_state: &'a std::collections::BTreeMap<String, WaylandOutputSnapshot>,
        #[serde(rename = "inputState")]
        input_state: &'a std::collections::BTreeMap<String, RuntimeInputDeviceSnapshot>,
    },
    LifecycleEnable {
        #[serde(rename = "requestId")]
        request_id: u64,
        reason: &'a str,
        #[serde(skip_serializing_if = "Option::is_none")]
        state: Option<&'a serde_json::Value>,
        environment: &'a std::collections::BTreeMap<String, String>,
        #[serde(rename = "displayState")]
        display_state: &'a std::collections::BTreeMap<String, WaylandOutputSnapshot>,
        #[serde(rename = "inputState")]
        input_state: &'a std::collections::BTreeMap<String, RuntimeInputDeviceSnapshot>,
    },
    LifecycleDisable {
        #[serde(rename = "requestId")]
        request_id: u64,
        reason: &'a str,
        #[serde(rename = "displayState")]
        display_state: &'a std::collections::BTreeMap<String, WaylandOutputSnapshot>,
        #[serde(rename = "inputState")]
        input_state: &'a std::collections::BTreeMap<String, RuntimeInputDeviceSnapshot>,
    },
}

#[derive(serde::Deserialize)]
struct RuntimeEvaluateResponse {
    #[serde(rename = "requestId")]
    request_id: u64,
    kind: String,
    ok: bool,
    transform: Option<WindowTransform>,
    #[serde(rename = "managedWindow")]
    managed_window: Option<ManagedWindowState>,
    #[serde(rename = "dirtyNodeIds")]
    dirty_node_ids: Option<Vec<String>>,
    #[serde(rename = "managedWindowOnly")]
    managed_window_only: Option<bool>,
    #[serde(rename = "nextPollInMs")]
    next_poll_in_ms: Option<u64>,
    actions: Option<Vec<RuntimeWindowAction>>,
    #[serde(rename = "displayConfig")]
    display_config: Option<RuntimeDisplayConfigUpdate>,
    #[serde(rename = "workspaceConfig")]
    workspace_config: Option<RuntimeWorkspaceConfigUpdate>,
    #[serde(rename = "keyBindingConfig")]
    key_binding_config: Option<RuntimeKeyBindingConfigUpdate>,
    #[serde(rename = "pointerConfig")]
    pointer_config: Option<RuntimePointerConfigUpdate>,
    #[serde(rename = "inputConfig")]
    input_config: Option<RuntimeInputConfigUpdate>,
    #[serde(rename = "eventConfig")]
    event_config: Option<RuntimeEventConfigUpdate>,
    #[serde(rename = "processConfig")]
    process_config: Option<RuntimeProcessConfigUpdate>,
    #[serde(rename = "processActions")]
    process_actions: Option<Vec<RuntimeProcessAction>>,
    error: Option<String>,
}

#[derive(serde::Deserialize)]
struct RuntimeWindowDecorationPolicyResponse {
    #[serde(rename = "requestId")]
    request_id: u64,
    kind: String,
    ok: bool,
    decision: Option<WindowDecorationDecisionSnapshot>,
    error: Option<String>,
}

#[derive(serde::Deserialize)]
struct RuntimeDrainPreloadResponse {
    #[serde(rename = "requestId")]
    request_id: u64,
    kind: String,
    ok: bool,
    error: Option<String>,
}

#[derive(serde::Deserialize)]
struct RuntimeSchedulerResponse {
    #[serde(rename = "requestId")]
    request_id: u64,
    kind: String,
    ok: bool,
    dirty: Option<bool>,
    #[serde(rename = "runtimeDirty")]
    runtime_dirty: Option<bool>,
    #[serde(rename = "dirtyWindowIds")]
    dirty_window_ids: Option<Vec<String>>,
    #[serde(rename = "dirtyManagedWindowIds")]
    dirty_managed_window_ids: Option<Vec<String>>,
    #[serde(rename = "dirtyWindowNodeIds")]
    dirty_window_node_ids: Option<std::collections::HashMap<String, Vec<String>>>,
    #[serde(rename = "dirtyLayerIds")]
    dirty_layer_ids: Option<Vec<String>>,
    #[serde(rename = "dirtyLayerNodeIds")]
    dirty_layer_node_ids: Option<std::collections::HashMap<String, Vec<String>>>,
    actions: Option<Vec<RuntimeWindowAction>>,
    #[serde(rename = "nextPollInMs")]
    next_poll_in_ms: Option<u64>,
    #[serde(rename = "displayConfig")]
    display_config: Option<RuntimeDisplayConfigUpdate>,
    #[serde(rename = "workspaceConfig")]
    workspace_config: Option<RuntimeWorkspaceConfigUpdate>,
    #[serde(rename = "keyBindingConfig")]
    key_binding_config: Option<RuntimeKeyBindingConfigUpdate>,
    #[serde(rename = "pointerConfig")]
    pointer_config: Option<RuntimePointerConfigUpdate>,
    #[serde(rename = "inputConfig")]
    input_config: Option<RuntimeInputConfigUpdate>,
    #[serde(rename = "eventConfig")]
    event_config: Option<RuntimeEventConfigUpdate>,
    #[serde(rename = "processConfig")]
    process_config: Option<RuntimeProcessConfigUpdate>,
    #[serde(rename = "processActions")]
    process_actions: Option<Vec<RuntimeProcessAction>>,
    #[serde(rename = "debugConfig")]
    debug_config: Option<RuntimeDebugConfigUpdate>,
    error: Option<String>,
}

fn runtime_scheduler_response_from_native(
    response: NativeSchedulerResponse,
) -> RuntimeSchedulerResponse {
    RuntimeSchedulerResponse {
        request_id: response.request_id,
        kind: "schedulerTick".into(),
        ok: true,
        dirty: Some(response.dirty),
        runtime_dirty: Some(response.runtime_dirty),
        dirty_window_ids: Some(response.dirty_window_ids),
        dirty_managed_window_ids: Some(response.dirty_managed_window_ids),
        dirty_window_node_ids: Some(response.dirty_window_node_ids),
        dirty_layer_ids: Some(response.dirty_layer_ids),
        dirty_layer_node_ids: Some(response.dirty_layer_node_ids),
        actions: None,
        next_poll_in_ms: response.next_poll_in_ms,
        display_config: None,
        workspace_config: None,
        key_binding_config: None,
        pointer_config: None,
        input_config: None,
        event_config: None,
        process_config: None,
        process_actions: None,
        debug_config: None,
        error: None,
    }
}

fn runtime_evaluate_response_from_native(
    response: NativeCachedResponse,
) -> RuntimeEvaluateResponse {
    RuntimeEvaluateResponse {
        request_id: response.request_id,
        kind: "evaluateCached".into(),
        ok: true,
        transform: Some(response.transform),
        managed_window: Some(response.managed_window),
        dirty_node_ids: Some(response.dirty_node_ids),
        managed_window_only: Some(response.managed_window_only),
        next_poll_in_ms: response.next_poll_in_ms,
        actions: None,
        display_config: None,
        workspace_config: None,
        key_binding_config: None,
        pointer_config: None,
        input_config: None,
        event_config: None,
        process_config: None,
        process_actions: None,
        error: None,
    }
}

#[derive(serde::Deserialize)]
struct RuntimeClosedResponse {
    #[serde(rename = "requestId")]
    request_id: u64,
    kind: String,
    ok: bool,
    #[serde(rename = "displayConfig")]
    _display_config: Option<RuntimeDisplayConfigUpdate>,
    #[serde(rename = "workspaceConfig")]
    _workspace_config: Option<RuntimeWorkspaceConfigUpdate>,
    #[serde(rename = "keyBindingConfig")]
    _key_binding_config: Option<RuntimeKeyBindingConfigUpdate>,
    #[serde(rename = "pointerConfig")]
    _pointer_config: Option<RuntimePointerConfigUpdate>,
    #[serde(rename = "inputConfig")]
    _input_config: Option<RuntimeInputConfigUpdate>,
    #[serde(rename = "eventConfig")]
    _event_config: Option<RuntimeEventConfigUpdate>,
    #[serde(rename = "processConfig")]
    _process_config: Option<RuntimeProcessConfigUpdate>,
    #[serde(rename = "processActions")]
    _process_actions: Option<Vec<RuntimeProcessAction>>,
    error: Option<String>,
}

#[derive(serde::Deserialize)]
struct RuntimeInvokeHandlerResponse {
    #[serde(rename = "requestId")]
    request_id: u64,
    kind: String,
    ok: bool,
    invoked: Option<bool>,
    serialized: Option<serde_json::Value>,
    transform: Option<WindowTransform>,
    #[serde(rename = "managedWindow")]
    managed_window: Option<ManagedWindowState>,
    #[serde(rename = "dirtyWindowIds")]
    dirty_window_ids: Option<Vec<String>>,
    #[serde(rename = "dirtyManagedWindowIds")]
    dirty_managed_window_ids: Option<Vec<String>>,
    #[serde(rename = "dirtyWindowNodeIds")]
    dirty_window_node_ids: Option<std::collections::HashMap<String, Vec<String>>>,
    actions: Option<Vec<RuntimeWindowAction>>,
    #[serde(rename = "nextPollInMs")]
    next_poll_in_ms: Option<u64>,
    #[serde(rename = "displayConfig")]
    display_config: Option<RuntimeDisplayConfigUpdate>,
    #[serde(rename = "workspaceConfig")]
    workspace_config: Option<RuntimeWorkspaceConfigUpdate>,
    #[serde(rename = "keyBindingConfig")]
    key_binding_config: Option<RuntimeKeyBindingConfigUpdate>,
    #[serde(rename = "pointerConfig")]
    pointer_config: Option<RuntimePointerConfigUpdate>,
    #[serde(rename = "inputConfig")]
    input_config: Option<RuntimeInputConfigUpdate>,
    #[serde(rename = "eventConfig")]
    event_config: Option<RuntimeEventConfigUpdate>,
    #[serde(rename = "processConfig")]
    process_config: Option<RuntimeProcessConfigUpdate>,
    #[serde(rename = "processActions")]
    process_actions: Option<Vec<RuntimeProcessAction>>,
    error: Option<String>,
}

#[derive(serde::Deserialize)]
struct RuntimeStartCloseResponse {
    #[serde(rename = "requestId")]
    request_id: u64,
    kind: String,
    ok: bool,
    invoked: Option<bool>,
    #[serde(rename = "closeAnimationDurationMs")]
    close_animation_duration_ms: Option<u64>,
    serialized: Option<serde_json::Value>,
    transform: Option<WindowTransform>,
    #[serde(rename = "managedWindow")]
    managed_window: Option<ManagedWindowState>,
    #[serde(rename = "dirtyWindowIds")]
    dirty_window_ids: Option<Vec<String>>,
    #[serde(rename = "dirtyManagedWindowIds")]
    dirty_managed_window_ids: Option<Vec<String>>,
    #[serde(rename = "dirtyWindowNodeIds")]
    dirty_window_node_ids: Option<std::collections::HashMap<String, Vec<String>>>,
    actions: Option<Vec<RuntimeWindowAction>>,
    #[serde(rename = "nextPollInMs")]
    next_poll_in_ms: Option<u64>,
    #[serde(rename = "displayConfig")]
    display_config: Option<RuntimeDisplayConfigUpdate>,
    #[serde(rename = "workspaceConfig")]
    workspace_config: Option<RuntimeWorkspaceConfigUpdate>,
    #[serde(rename = "keyBindingConfig")]
    key_binding_config: Option<RuntimeKeyBindingConfigUpdate>,
    #[serde(rename = "pointerConfig")]
    pointer_config: Option<RuntimePointerConfigUpdate>,
    #[serde(rename = "inputConfig")]
    input_config: Option<RuntimeInputConfigUpdate>,
    #[serde(rename = "eventConfig")]
    event_config: Option<RuntimeEventConfigUpdate>,
    #[serde(rename = "processConfig")]
    process_config: Option<RuntimeProcessConfigUpdate>,
    #[serde(rename = "processActions")]
    process_actions: Option<Vec<RuntimeProcessAction>>,
    error: Option<String>,
}

#[derive(serde::Deserialize)]
struct RuntimeEffectConfigResponse {
    #[serde(rename = "requestId")]
    request_id: u64,
    kind: String,
    ok: bool,
    #[serde(rename = "displayConfig")]
    _display_config: Option<RuntimeDisplayConfigUpdate>,
    #[serde(rename = "workspaceConfig")]
    _workspace_config: Option<RuntimeWorkspaceConfigUpdate>,
    #[serde(rename = "keyBindingConfig")]
    _key_binding_config: Option<RuntimeKeyBindingConfigUpdate>,
    #[serde(rename = "pointerConfig")]
    _pointer_config: Option<RuntimePointerConfigUpdate>,
    #[serde(rename = "inputConfig")]
    _input_config: Option<RuntimeInputConfigUpdate>,
    #[serde(rename = "processConfig")]
    _process_config: Option<RuntimeProcessConfigUpdate>,
    #[serde(rename = "processActions")]
    _process_actions: Option<Vec<RuntimeProcessAction>>,
    error: Option<String>,
}

#[derive(serde::Deserialize)]
struct RuntimePopupEffectsResponse {
    #[serde(rename = "requestId")]
    request_id: u64,
    kind: String,
    ok: bool,
    #[serde(rename = "nextPollInMs")]
    next_poll_in_ms: Option<u64>,
    #[serde(rename = "displayConfig")]
    display_config: Option<RuntimeDisplayConfigUpdate>,
    #[serde(rename = "workspaceConfig")]
    workspace_config: Option<RuntimeWorkspaceConfigUpdate>,
    #[serde(rename = "keyBindingConfig")]
    key_binding_config: Option<RuntimeKeyBindingConfigUpdate>,
    #[serde(rename = "pointerConfig")]
    pointer_config: Option<RuntimePointerConfigUpdate>,
    #[serde(rename = "inputConfig")]
    input_config: Option<RuntimeInputConfigUpdate>,
    #[serde(rename = "eventConfig")]
    event_config: Option<RuntimeEventConfigUpdate>,
    #[serde(rename = "processConfig")]
    process_config: Option<RuntimeProcessConfigUpdate>,
    #[serde(rename = "processActions")]
    process_actions: Option<Vec<RuntimeProcessAction>>,
    error: Option<String>,
}

#[derive(serde::Deserialize)]
struct RuntimeLayerEffectsResponse {
    #[serde(rename = "requestId")]
    request_id: u64,
    kind: String,
    ok: bool,
    #[serde(rename = "nextPollInMs")]
    next_poll_in_ms: Option<u64>,
    #[serde(rename = "displayConfig")]
    display_config: Option<RuntimeDisplayConfigUpdate>,
    #[serde(rename = "workspaceConfig")]
    workspace_config: Option<RuntimeWorkspaceConfigUpdate>,
    #[serde(rename = "keyBindingConfig")]
    key_binding_config: Option<RuntimeKeyBindingConfigUpdate>,
    #[serde(rename = "pointerConfig")]
    pointer_config: Option<RuntimePointerConfigUpdate>,
    #[serde(rename = "inputConfig")]
    input_config: Option<RuntimeInputConfigUpdate>,
    #[serde(rename = "eventConfig")]
    event_config: Option<RuntimeEventConfigUpdate>,
    #[serde(rename = "processConfig")]
    process_config: Option<RuntimeProcessConfigUpdate>,
    #[serde(rename = "processActions")]
    process_actions: Option<Vec<RuntimeProcessAction>>,
    error: Option<String>,
}

#[derive(serde::Deserialize)]
struct RuntimeLifecycleEnableResponse {
    #[serde(rename = "requestId")]
    request_id: u64,
    kind: Option<String>,
    ok: bool,
    #[serde(rename = "displayConfig")]
    display_config: Option<RuntimeDisplayConfigUpdate>,
    #[serde(rename = "workspaceConfig")]
    workspace_config: Option<RuntimeWorkspaceConfigUpdate>,
    #[serde(rename = "keyBindingConfig")]
    key_binding_config: Option<RuntimeKeyBindingConfigUpdate>,
    #[serde(rename = "pointerConfig")]
    pointer_config: Option<RuntimePointerConfigUpdate>,
    #[serde(rename = "inputConfig")]
    input_config: Option<RuntimeInputConfigUpdate>,
    #[serde(rename = "eventConfig")]
    event_config: Option<RuntimeEventConfigUpdate>,
    #[serde(rename = "processConfig")]
    process_config: Option<RuntimeProcessConfigUpdate>,
    #[serde(rename = "processActions")]
    process_actions: Option<Vec<RuntimeProcessAction>>,
    error: Option<String>,
}

#[derive(serde::Deserialize)]
struct RuntimeLifecycleDisableResponse {
    #[serde(rename = "requestId")]
    request_id: u64,
    kind: Option<String>,
    ok: bool,
    #[serde(default)]
    state: serde_json::Value,
    error: Option<String>,
}

#[derive(serde::Deserialize)]
struct RuntimeInvokeKeyBindingResponse {
    #[serde(rename = "requestId")]
    request_id: u64,
    kind: String,
    ok: bool,
    invoked: Option<bool>,
    dirty: Option<bool>,
    #[serde(rename = "dirtyWindowIds")]
    dirty_window_ids: Option<Vec<String>>,
    #[serde(rename = "dirtyManagedWindowIds")]
    dirty_managed_window_ids: Option<Vec<String>>,
    #[serde(rename = "dirtyWindowNodeIds")]
    dirty_window_node_ids: Option<std::collections::HashMap<String, Vec<String>>>,
    #[serde(rename = "dirtyLayerNodeIds")]
    dirty_layer_node_ids: Option<std::collections::HashMap<String, Vec<String>>>,
    actions: Option<Vec<RuntimeWindowAction>>,
    #[serde(rename = "nextPollInMs")]
    next_poll_in_ms: Option<u64>,
    #[serde(rename = "displayConfig")]
    display_config: Option<RuntimeDisplayConfigUpdate>,
    #[serde(rename = "workspaceConfig")]
    workspace_config: Option<RuntimeWorkspaceConfigUpdate>,
    #[serde(rename = "keyBindingConfig")]
    key_binding_config: Option<RuntimeKeyBindingConfigUpdate>,
    #[serde(rename = "pointerConfig")]
    pointer_config: Option<RuntimePointerConfigUpdate>,
    #[serde(rename = "inputConfig")]
    input_config: Option<RuntimeInputConfigUpdate>,
    #[serde(rename = "eventConfig")]
    event_config: Option<RuntimeEventConfigUpdate>,
    #[serde(rename = "processConfig")]
    process_config: Option<RuntimeProcessConfigUpdate>,
    #[serde(rename = "processActions")]
    process_actions: Option<Vec<RuntimeProcessAction>>,
    #[serde(rename = "debugConfig")]
    debug_config: Option<RuntimeDebugConfigUpdate>,
    error: Option<String>,
}

#[derive(serde::Deserialize)]
struct RuntimeWindowMoveResponse {
    #[serde(rename = "requestId")]
    request_id: u64,
    kind: String,
    ok: bool,
    invoked: Option<bool>,
    dirty: Option<bool>,
    #[serde(rename = "dirtyWindowIds")]
    dirty_window_ids: Option<Vec<String>>,
    #[serde(rename = "dirtyManagedWindowIds")]
    dirty_managed_window_ids: Option<Vec<String>>,
    #[serde(rename = "dirtyWindowNodeIds")]
    dirty_window_node_ids: Option<std::collections::HashMap<String, Vec<String>>>,
    #[serde(rename = "dirtyLayerNodeIds")]
    dirty_layer_node_ids: Option<std::collections::HashMap<String, Vec<String>>>,
    actions: Option<Vec<RuntimeWindowAction>>,
    #[serde(rename = "nextPollInMs")]
    next_poll_in_ms: Option<u64>,
    #[serde(rename = "displayConfig")]
    display_config: Option<RuntimeDisplayConfigUpdate>,
    #[serde(rename = "workspaceConfig")]
    workspace_config: Option<RuntimeWorkspaceConfigUpdate>,
    #[serde(rename = "keyBindingConfig")]
    key_binding_config: Option<RuntimeKeyBindingConfigUpdate>,
    #[serde(rename = "pointerConfig")]
    pointer_config: Option<RuntimePointerConfigUpdate>,
    #[serde(rename = "inputConfig")]
    input_config: Option<RuntimeInputConfigUpdate>,
    #[serde(rename = "eventConfig")]
    event_config: Option<RuntimeEventConfigUpdate>,
    #[serde(rename = "processConfig")]
    process_config: Option<RuntimeProcessConfigUpdate>,
    #[serde(rename = "processActions")]
    process_actions: Option<Vec<RuntimeProcessAction>>,
    error: Option<String>,
}

type RuntimeWindowStateRequestResponse = RuntimeWindowMoveResponse;

#[derive(serde::Deserialize)]
struct RuntimePointerMoveAsyncResponse {
    #[serde(rename = "requestId")]
    request_id: u64,
    kind: String,
    ok: bool,
    invoked: Option<bool>,
    dirty: Option<bool>,
    #[serde(rename = "dirtyWindowIds")]
    dirty_window_ids: Option<Vec<String>>,
    #[serde(rename = "dirtyManagedWindowIds")]
    dirty_managed_window_ids: Option<Vec<String>>,
    #[serde(rename = "dirtyWindowNodeIds")]
    dirty_window_node_ids: Option<std::collections::HashMap<String, Vec<String>>>,
    #[serde(rename = "dirtyLayerNodeIds")]
    dirty_layer_node_ids: Option<std::collections::HashMap<String, Vec<String>>>,
    actions: Option<Vec<RuntimeWindowAction>>,
    #[serde(rename = "nextPollInMs")]
    next_poll_in_ms: Option<u64>,
    #[serde(rename = "displayConfig")]
    display_config: Option<RuntimeDisplayConfigUpdate>,
    #[serde(rename = "workspaceConfig")]
    workspace_config: Option<RuntimeWorkspaceConfigUpdate>,
    #[serde(rename = "keyBindingConfig")]
    key_binding_config: Option<RuntimeKeyBindingConfigUpdate>,
    #[serde(rename = "pointerConfig")]
    pointer_config: Option<RuntimePointerConfigUpdate>,
    #[serde(rename = "inputConfig")]
    input_config: Option<RuntimeInputConfigUpdate>,
    #[serde(rename = "eventConfig")]
    event_config: Option<RuntimeEventConfigUpdate>,
    #[serde(rename = "processConfig")]
    process_config: Option<RuntimeProcessConfigUpdate>,
    #[serde(rename = "processActions")]
    process_actions: Option<Vec<RuntimeProcessAction>>,
    error: Option<String>,
}

type RuntimeGestureSwipeAsyncResponse = RuntimePointerMoveAsyncResponse;

fn runtime_interaction_response_from_native(
    response: NativeInteractionResponse,
) -> RuntimePointerMoveAsyncResponse {
    RuntimePointerMoveAsyncResponse {
        request_id: response.request_id,
        kind: response.kind.as_str().to_owned(),
        ok: true,
        invoked: Some(response.invoked),
        dirty: Some(response.dirty),
        dirty_window_ids: Some(response.dirty_window_ids),
        dirty_managed_window_ids: Some(response.dirty_managed_window_ids),
        dirty_window_node_ids: Some(response.dirty_window_node_ids),
        dirty_layer_node_ids: Some(response.dirty_layer_node_ids),
        actions: Some(response.actions),
        next_poll_in_ms: response.next_poll_in_ms,
        display_config: None,
        workspace_config: None,
        key_binding_config: None,
        pointer_config: None,
        input_config: None,
        event_config: None,
        process_config: None,
        process_actions: None,
        error: None,
    }
}

fn validate_interaction_response(
    response: &RuntimePointerMoveAsyncResponse,
    request_id: u64,
    expected_kind: &str,
) -> Result<(), DecorationEvaluationError> {
    if response.request_id != request_id {
        return Err(DecorationEvaluationError::RuntimeProtocol(format!(
            "mismatched response id: expected {request_id}, got {}",
            response.request_id
        )));
    }
    if response.kind != expected_kind {
        return Err(DecorationEvaluationError::RuntimeProtocol(format!(
            "mismatched response kind for {expected_kind}: {}",
            response.kind
        )));
    }
    if !response.ok {
        return Err(DecorationEvaluationError::RuntimeProtocol(
            response
                .error
                .clone()
                .unwrap_or_else(|| "runtime returned failure".into()),
        ));
    }
    Ok(())
}

fn interaction_invocation_from_response(
    response: RuntimePointerMoveAsyncResponse,
) -> DecorationPointerMoveAsyncInvocation {
    DecorationPointerMoveAsyncInvocation {
        invoked: response.invoked.unwrap_or(false),
        dirty: response.dirty.unwrap_or(false),
        dirty_window_ids: response.dirty_window_ids.unwrap_or_default(),
        dirty_managed_window_ids: response.dirty_managed_window_ids.unwrap_or_default(),
        dirty_window_node_ids: response.dirty_window_node_ids.unwrap_or_default(),
        dirty_layer_node_ids: response.dirty_layer_node_ids.unwrap_or_default(),
        actions: response.actions.unwrap_or_default(),
        next_poll_in_ms: response.next_poll_in_ms,
        display_config: response.display_config,
        workspace_config: response.workspace_config,
        key_binding_config: response.key_binding_config,
        pointer_config: response.pointer_config,
        input_config: response.input_config,
        event_config: response.event_config,
        process_config: response.process_config,
        process_actions: response.process_actions.unwrap_or_default(),
    }
}

fn runtime_failed_error(runtime: &mut EmbeddedDecorationRuntime) -> DecorationEvaluationError {
    let status = runtime
        .child
        .try_wait()
        .ok()
        .flatten()
        .and_then(|status| status.code())
        .unwrap_or(-1);
    let stderr = runtime
        .stderr_log
        .lock()
        .map(|stderr| stderr.clone())
        .unwrap_or_default();
    DecorationEvaluationError::RuntimeFailed { status, stderr }
}

impl std::fmt::Debug for EmbeddedDecorationEvaluator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EmbeddedDecorationEvaluator")
            .field("script_path", &self.script_path)
            .field("config_path", &self.config_path)
            .field("working_dir", &self.working_dir)
            .finish()
    }
}

impl EmbeddedDecorationEvaluator {
    pub fn for_workspace(config_path: impl Into<PathBuf>) -> Self {
        Self {
            script_path: PathBuf::from("tools/decoration-runtime.ts"),
            config_path: config_path.into(),
            working_dir: None,
            runtime: Arc::new(Mutex::new(None)),
            display_state: Arc::new(Mutex::new(std::collections::BTreeMap::new())),
            input_state: Arc::new(Mutex::new(std::collections::BTreeMap::new())),
            runtime_state_generation: Arc::new(AtomicU64::new(1)),
            pointer_move_async: Arc::new(PointerMoveAsyncDispatcher::default()),
            async_event_sender: Arc::new(Mutex::new(None)),
        }
    }

    pub fn for_paths(script_path: impl Into<PathBuf>, config_path: impl Into<PathBuf>) -> Self {
        Self {
            script_path: script_path.into(),
            config_path: config_path.into(),
            working_dir: None,
            runtime: Arc::new(Mutex::new(None)),
            display_state: Arc::new(Mutex::new(std::collections::BTreeMap::new())),
            input_state: Arc::new(Mutex::new(std::collections::BTreeMap::new())),
            runtime_state_generation: Arc::new(AtomicU64::new(1)),
            pointer_move_async: Arc::new(PointerMoveAsyncDispatcher::default()),
            async_event_sender: Arc::new(Mutex::new(None)),
        }
    }

    pub fn with_working_dir(mut self, working_dir: impl Into<PathBuf>) -> Self {
        self.working_dir = Some(working_dir.into());
        self
    }

    pub fn set_async_event_sender(&self, sender: CalloopSender<DecorationRuntimeAsyncInvocation>) {
        if let Ok(mut guard) = self.async_event_sender.lock() {
            *guard = Some(sender);
        }
    }

    pub fn set_display_state(
        &self,
        display_state: std::collections::BTreeMap<String, WaylandOutputSnapshot>,
    ) {
        if let Ok(mut guard) = self.display_state.lock()
            && *guard != display_state {
                *guard = display_state;
                self.runtime_state_generation
                    .fetch_add(1, Ordering::Release);
            }
    }

    pub fn set_input_state(
        &self,
        input_state: std::collections::BTreeMap<String, RuntimeInputDeviceSnapshot>,
    ) {
        if let Ok(mut guard) = self.input_state.lock()
            && *guard != input_state {
                *guard = input_state;
                self.runtime_state_generation
                    .fetch_add(1, Ordering::Release);
            }
    }

    /// Retire the current isolate and hand back an evaluator that shares this
    /// one's state.
    ///
    /// Every `Arc` is shared rather than reallocated so the pointer-move worker
    /// spawned by the first generation keeps serving later ones. Allocating a
    /// fresh dispatcher here used to strand that worker on a condvar nobody
    /// would notify again, leaking its evaluator clone — and with it an isolate,
    /// two threads and four fds — on every reload the pointer had armed.
    pub fn fresh_like(&self) -> Self {
        self.reset_runtime_for_reload();
        self.clone()
    }

    /// Drop the isolate in place, keeping the cell every generation shares.
    fn reset_runtime_for_reload(&self) {
        self.pointer_move_async
            .runtime_dispatchable
            .store(false, Ordering::Release);
        self.pointer_move_async.epoch.fetch_add(1, Ordering::AcqRel);
        if let Ok(mut pending) = self.pointer_move_async.pending.lock() {
            *pending = None;
        }

        let mut runtime_guard = match self.runtime.lock() {
            Ok(guard) => guard,
            // The cell outlives reloads now, so a poisoned mutex would too.
            // Reload used to heal it by allocating a new one.
            Err(poisoned) => {
                self.runtime.clear_poison();
                poisoned.into_inner()
            }
        };
        // `EmbeddedRuntime::drop` closes the request channel and joins the
        // runtime thread, so this is the isolate teardown.
        *runtime_guard = None;
    }

    /// Stop the shared pointer-move worker. The worker holds an evaluator clone
    /// and parks on the dispatcher's condvar, so nothing else can retire it.
    #[allow(dead_code)]
    pub fn shutdown(&self) {
        self.pointer_move_async
            .shutdown
            .store(true, Ordering::Release);
        self.pointer_move_async.pending_changed.notify_all();
    }

    pub fn preload(&self) -> Result<(), DecorationEvaluationError> {
        let mut runtime_guard = self.runtime.lock().map_err(|_| {
            DecorationEvaluationError::RuntimeProtocol("runtime mutex poisoned".into())
        })?;
        let runtime = self.ensure_runtime(&mut runtime_guard)?;
        let request_id = runtime.next_request_id;
        runtime.next_request_id += 1;
        let request = serde_json::to_string(&RuntimeRequest::DrainPreload { request_id })
            .map_err(|err| DecorationEvaluationError::SnapshotSerialization(err.to_string()))?;
        runtime.write_request(&request)?;

        let response: RuntimeDrainPreloadResponse =
            if let Some(response) = runtime.read_response()? {
                response
            } else {
                let status = runtime
                    .child
                    .try_wait()?
                    .and_then(|status| status.code())
                    .unwrap_or(-1);
                let stderr = runtime
                    .stderr_log
                    .lock()
                    .map(|stderr| stderr.clone())
                    .unwrap_or_default();
                *runtime_guard = None;
                return Err(DecorationEvaluationError::RuntimeFailed { status, stderr });
            };
        if response.request_id != request_id {
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeProtocol(format!(
                "mismatched response id: expected {request_id}, got {}",
                response.request_id
            )));
        }
        if !response.ok {
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeProtocol(
                response
                    .error
                    .unwrap_or_else(|| "runtime returned failure".into()),
            ));
        }
        if response.kind != "drainPreload" {
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeProtocol(format!(
                "mismatched response kind for drainPreload: {}",
                response.kind
            )));
        }
        Ok(())
    }

    pub fn lifecycle_enable(
        &self,
        reason: &str,
        state: Option<&serde_json::Value>,
    ) -> Result<DecorationHandlerInvocation, DecorationEvaluationError> {
        let mut runtime_guard = self.runtime.lock().map_err(|_| {
            DecorationEvaluationError::RuntimeProtocol("runtime mutex poisoned".into())
        })?;
        let runtime = self.ensure_runtime(&mut runtime_guard)?;
        let request_id = runtime.next_request_id;
        runtime.next_request_id += 1;
        let display_state = self
            .display_state
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default();
        let input_state = self
            .input_state
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default();
        let environment = runtime_environment_snapshot();

        let request = serde_json::to_string(&RuntimeRequest::LifecycleEnable {
            request_id,
            reason,
            state,
            environment: &environment,
            display_state: &display_state,
            input_state: &input_state,
        })
        .map_err(|err| DecorationEvaluationError::SnapshotSerialization(err.to_string()))?;
        runtime.write_request(&request)?;

        let response: RuntimeLifecycleEnableResponse =
            if let Some(response) = runtime.read_response()? {
                response
            } else {
                let status = runtime
                    .child
                    .try_wait()?
                    .and_then(|status| status.code())
                    .unwrap_or(-1);
                let stderr = runtime
                    .stderr_log
                    .lock()
                    .map(|stderr| stderr.clone())
                    .unwrap_or_default();
                *runtime_guard = None;
                return Err(DecorationEvaluationError::RuntimeFailed { status, stderr });
            };
        if response.request_id != request_id {
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeProtocol(format!(
                "mismatched response id: expected {request_id}, got {}",
                response.request_id
            )));
        }
        if !response.ok {
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeProtocol(
                response
                    .error
                    .unwrap_or_else(|| "runtime returned failure".into()),
            ));
        }
        if response.kind.as_deref() != Some("lifecycleEnable") {
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeProtocol(format!(
                "mismatched response kind for lifecycleEnable: {}",
                response.kind.as_deref().unwrap_or("<missing>")
            )));
        }

        // The shared worker may dispatch from here on.
        self.pointer_move_async
            .runtime_dispatchable
            .store(true, Ordering::Release);

        Ok(DecorationHandlerInvocation {
            invoked: true,
            display_config: response.display_config,
            workspace_config: response.workspace_config,
            key_binding_config: response.key_binding_config,
            pointer_config: response.pointer_config,
            input_config: response.input_config,
            event_config: response.event_config,
            process_config: response.process_config,
            process_actions: response.process_actions.unwrap_or_default(),
            ..DecorationHandlerInvocation::default()
        })
    }

    pub fn lifecycle_disable(
        &self,
        reason: &str,
    ) -> Result<serde_json::Value, DecorationEvaluationError> {
        // Park the shared worker before taking the lock it also contends for.
        self.pointer_move_async
            .runtime_dispatchable
            .store(false, Ordering::Release);
        let mut runtime_guard = self.runtime.lock().map_err(|_| {
            DecorationEvaluationError::RuntimeProtocol("runtime mutex poisoned".into())
        })?;
        let runtime = self.ensure_runtime(&mut runtime_guard)?;
        let request_id = runtime.next_request_id;
        runtime.next_request_id += 1;
        let display_state = self
            .display_state
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default();
        let input_state = self
            .input_state
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default();

        let request = serde_json::to_string(&RuntimeRequest::LifecycleDisable {
            request_id,
            reason,
            display_state: &display_state,
            input_state: &input_state,
        })
        .map_err(|err| DecorationEvaluationError::SnapshotSerialization(err.to_string()))?;
        runtime.write_request(&request)?;

        let response: RuntimeLifecycleDisableResponse =
            if let Some(response) = runtime.read_response()? {
                response
            } else {
                let status = runtime
                    .child
                    .try_wait()?
                    .and_then(|status| status.code())
                    .unwrap_or(-1);
                let stderr = runtime
                    .stderr_log
                    .lock()
                    .map(|stderr| stderr.clone())
                    .unwrap_or_default();
                *runtime_guard = None;
                return Err(DecorationEvaluationError::RuntimeFailed { status, stderr });
            };
        if response.request_id != request_id {
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeProtocol(format!(
                "mismatched response id: expected {request_id}, got {}",
                response.request_id
            )));
        }
        if !response.ok {
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeProtocol(
                response
                    .error
                    .unwrap_or_else(|| "runtime returned failure".into()),
            ));
        }
        if response.kind.as_deref() != Some("lifecycleDisable") {
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeProtocol(format!(
                "mismatched response kind for lifecycleDisable: {}",
                response.kind.as_deref().unwrap_or("<missing>")
            )));
        }

        Ok(response.state)
    }

    fn ensure_runtime<'a>(
        &'a self,
        runtime: &'a mut Option<EmbeddedDecorationRuntime>,
    ) -> Result<&'a mut EmbeddedDecorationRuntime, DecorationEvaluationError> {
        if runtime.is_none() {
            *runtime = Some(self.spawn_embedded_runtime()?);
        }

        runtime
            .as_mut()
            .ok_or_else(|| DecorationEvaluationError::RuntimeProtocol("runtime unavailable".into()))
    }

    fn spawn_embedded_runtime(
        &self,
    ) -> Result<EmbeddedDecorationRuntime, DecorationEvaluationError> {
        debug!("spawning embedded RustyScript decoration runtime");
        let child = EmbeddedRuntime::start(
            self.script_path.clone(),
            self.config_path.clone(),
            self.working_dir.clone(),
        )
        .map_err(DecorationEvaluationError::RuntimeProtocol)?;
        Ok(EmbeddedDecorationRuntime {
            child,
            next_request_id: 1,
            stderr_log: Arc::new(Mutex::new(String::new())),
            async_event_sender: Arc::clone(&self.async_event_sender),
            last_sent_runtime_state_generation: 0,
        })
    }

    pub fn background_effect_config(
        &self,
    ) -> Result<Option<BackgroundEffectConfig>, DecorationEvaluationError> {
        let mut runtime_guard = self.runtime.lock().map_err(|_| {
            DecorationEvaluationError::RuntimeProtocol("runtime mutex poisoned".into())
        })?;
        let runtime = self.ensure_runtime(&mut runtime_guard)?;
        let request_id = runtime.next_request_id;
        runtime.next_request_id += 1;
        let display_state = self
            .display_state
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default();
        let input_state = self
            .input_state
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default();

        runtime.write_effect_request(NativeEffectRequest::GetEffectConfig {
            request_id,
            display_state,
            input_state,
        })?;

        let response: RuntimeEffectConfigResponse =
            if let Some(response) = runtime.read_response()? {
                response
            } else {
                let status = runtime
                    .child
                    .try_wait()?
                    .and_then(|status| status.code())
                    .unwrap_or(-1);
                let stderr = runtime
                    .stderr_log
                    .lock()
                    .map(|stderr| stderr.clone())
                    .unwrap_or_default();
                *runtime_guard = None;
                return Err(DecorationEvaluationError::RuntimeFailed { status, stderr });
            };
        if response.request_id != request_id {
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeProtocol(format!(
                "mismatched response id: expected {request_id}, got {}",
                response.request_id
            )));
        }
        if response.kind != "getEffectConfig" {
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeProtocol(format!(
                "mismatched response kind for getEffectConfig: {}",
                response.kind
            )));
        }
        if !response.ok {
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeProtocol(
                response
                    .error
                    .unwrap_or_else(|| "runtime returned failure".into()),
            ));
        }

        match runtime
            .take_effect_update(request_id)?
            .map(|resolved| resolved.update)
        {
            Some(NativeEffectUpdate::Background(effect)) => Ok(effect),
            Some(_) => Err(DecorationEvaluationError::RuntimeProtocol(
                "mismatched native effect update for getEffectConfig".into(),
            )),
            None => Err(DecorationEvaluationError::RuntimeProtocol(
                "missing native background effect update".into(),
            )),
        }
    }

    fn enqueue_pointer_move_async(&self, event: PointerMoveEventSnapshot, now_ms: u64) {
        self.ensure_pointer_move_async_worker();
        if let Ok(mut pending) = self.pointer_move_async.pending.lock() {
            if matches!(
                pending.as_ref(),
                Some(RuntimeAsyncWork::GestureSwipe {
                    event: GestureSwipeEventSnapshot {
                        phase: GestureSwipePhaseSnapshot::End | GestureSwipePhaseSnapshot::Cancel,
                        ..
                    },
                    ..
                })
            ) {
                return;
            }
            *pending = Some(RuntimeAsyncWork::PointerMove { event, now_ms });
            self.pointer_move_async.pending_changed.notify_one();
        }
    }

    fn enqueue_gesture_swipe_async(&self, event: GestureSwipeEventSnapshot, now_ms: u64) {
        self.ensure_pointer_move_async_worker();
        if let Ok(mut pending) = self.pointer_move_async.pending.lock() {
            *pending = Some(RuntimeAsyncWork::GestureSwipe { event, now_ms });
            self.pointer_move_async.pending_changed.notify_one();
        }
    }

    fn ensure_pointer_move_async_worker(&self) {
        // The worker is now process-lifetime, so this only ever flips once.
        // Reading first keeps the steady state off a cacheline the worker owns.
        if self.pointer_move_async.worker_started.load(Ordering::Relaxed) {
            return;
        }
        if self
            .pointer_move_async
            .worker_started
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }

        let evaluator = self.clone();
        let spawn_result = std::thread::Builder::new()
            .name("shojiwm-pointer-move-async".into())
            .spawn(move || evaluator.run_pointer_move_async_worker());
        if let Err(error) = spawn_result {
            self.pointer_move_async
                .worker_started
                .store(false, Ordering::Release);
            warn!(?error, "failed to spawn pointer move async worker");
        }
    }

    fn run_pointer_move_async_worker(self) {
        loop {
            let work = {
                let mut pending = match self.pointer_move_async.pending.lock() {
                    Ok(pending) => pending,
                    Err(_) => return,
                };
                while pending.is_none() {
                    if self.pointer_move_async.shutdown.load(Ordering::Acquire) {
                        return;
                    }
                    pending = match self.pointer_move_async.pending_changed.wait(pending) {
                        Ok(pending) => pending,
                        Err(_) => return,
                    };
                }
                pending.take()
            };
            if self.pointer_move_async.shutdown.load(Ordering::Acquire) {
                return;
            }
            let Some(work) = work else {
                continue;
            };

            let epoch = self.pointer_move_async.epoch.load(Ordering::Acquire);
            let result = match work {
                RuntimeAsyncWork::PointerMove { event, now_ms } => self
                    .dispatch_pointer_move_async(&event, now_ms)
                    .map(|invocation| {
                        invocation.map(DecorationRuntimeAsyncInvocation::PointerMove)
                    }),
                RuntimeAsyncWork::GestureSwipe { event, now_ms } => self
                    .dispatch_gesture_swipe_async(&event, now_ms)
                    .map(|invocation| {
                        invocation.map(DecorationRuntimeAsyncInvocation::GestureSwipe)
                    }),
            };

            // A reload can land while the round trip is in flight. The result then
            // came from the retired isolate, and consuming it would overwrite the
            // config the new one just installed.
            if self.pointer_move_async.epoch.load(Ordering::Acquire) != epoch {
                continue;
            }

            match result {
                Ok(Some(invocation)) => {
                    if let Ok(sender_guard) = self.async_event_sender.lock()
                        && let Some(sender) = sender_guard.as_ref()
                    {
                        let _ = sender.send(invocation);
                    }
                }
                Ok(None) => {}
                Err(error) => {
                    debug!(?error, "failed to dispatch runtime async event");
                }
            }
        }
    }

    /// Display and input state only cross the bridge when they have actually
    /// changed; the runtime keeps the last copy it was sent, and an absent field
    /// is the reuse signal. Mirrors the gate the cached and scheduler paths use.
    /// The returned generation is recorded once the write succeeds.
    fn interaction_state_payload(
        &self,
        last_sent: u64,
    ) -> (
        Option<std::collections::BTreeMap<String, WaylandOutputSnapshot>>,
        Option<std::collections::BTreeMap<String, RuntimeInputDeviceSnapshot>>,
        u64,
    ) {
        let generation = self.runtime_state_generation.load(Ordering::Acquire);
        if last_sent == generation {
            return (None, None, generation);
        }
        let display_state = self
            .display_state
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default();
        let input_state = self
            .input_state
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default();
        (Some(display_state), Some(input_state), generation)
    }

    fn dispatch_pointer_move(
        &self,
        event: &PointerMoveEventSnapshot,
        now_ms: u64,
    ) -> Result<DecorationPointerMoveAsyncInvocation, DecorationEvaluationError> {
        let mut runtime_guard = self.runtime.lock().map_err(|_| {
            DecorationEvaluationError::RuntimeProtocol("runtime mutex poisoned".into())
        })?;
        let runtime = self.ensure_runtime(&mut runtime_guard)?;
        let request_id = runtime.next_request_id;
        runtime.next_request_id += 1;
        let (display_state, input_state, runtime_state_generation) =
            self.interaction_state_payload(runtime.last_sent_runtime_state_generation);

        runtime.write_interaction_request(NativeInteractionRequest::PointerMove {
            request_id,
            event: event.clone(),
            now_ms,
            display_state,
            input_state,
        })?;
        runtime.last_sent_runtime_state_generation = runtime_state_generation;
        let response = if let Some(response) = runtime.read_interaction_response()? {
            response
        } else {
            return Err(runtime_failed_error(runtime));
        };
        validate_interaction_response(&response, request_id, "pointerMove")?;
        Ok(interaction_invocation_from_response(response))
    }

    fn dispatch_gesture_swipe(
        &self,
        event: &GestureSwipeEventSnapshot,
        now_ms: u64,
    ) -> Result<DecorationGestureSwipeAsyncInvocation, DecorationEvaluationError> {
        let mut runtime_guard = self.runtime.lock().map_err(|_| {
            DecorationEvaluationError::RuntimeProtocol("runtime mutex poisoned".into())
        })?;
        let runtime = self.ensure_runtime(&mut runtime_guard)?;
        let request_id = runtime.next_request_id;
        runtime.next_request_id += 1;
        let (display_state, input_state, runtime_state_generation) =
            self.interaction_state_payload(runtime.last_sent_runtime_state_generation);

        runtime.write_interaction_request(NativeInteractionRequest::GestureSwipe {
            request_id,
            event: event.clone(),
            now_ms,
            display_state,
            input_state,
        })?;
        runtime.last_sent_runtime_state_generation = runtime_state_generation;
        let response = if let Some(response) = runtime.read_interaction_response()? {
            response
        } else {
            return Err(runtime_failed_error(runtime));
        };
        validate_interaction_response(&response, request_id, "gestureSwipe")?;
        Ok(interaction_invocation_from_response(response))
    }

    fn dispatch_pointer_move_async(
        &self,
        event: &PointerMoveEventSnapshot,
        now_ms: u64,
    ) -> Result<Option<DecorationPointerMoveAsyncInvocation>, DecorationEvaluationError> {
        if !self
            .pointer_move_async
            .runtime_dispatchable
            .load(Ordering::Acquire)
        {
            return Ok(None);
        }
        let Ok(mut runtime_guard) = self.runtime.try_lock() else {
            // Pointer motion is lossy by design. If the runtime is handling a synchronous
            // request, dropping this sample is better than blocking input delivery.
            return Ok(None);
        };
        // Never `ensure_runtime` here: the worker must not be what brings an
        // isolate into existence, or a sample landing mid-reload spawns one that
        // never received `lifecycleEnable`.
        let Some(runtime) = runtime_guard.as_mut() else {
            return Ok(None);
        };
        let request_id = runtime.next_request_id;
        runtime.next_request_id += 1;
        let (display_state, input_state, runtime_state_generation) =
            self.interaction_state_payload(runtime.last_sent_runtime_state_generation);

        runtime.write_interaction_request(NativeInteractionRequest::PointerMoveAsync {
            request_id,
            event: event.clone(),
            now_ms,
            display_state,
            input_state,
        })?;
        runtime.last_sent_runtime_state_generation = runtime_state_generation;

        let response: RuntimePointerMoveAsyncResponse =
            if let Some(response) = runtime.read_interaction_response()? {
                response
            } else {
                let status = runtime
                    .child
                    .try_wait()?
                    .and_then(|status| status.code())
                    .unwrap_or(-1);
                let stderr = runtime
                    .stderr_log
                    .lock()
                    .map(|stderr| stderr.clone())
                    .unwrap_or_default();
                *runtime_guard = None;
                return Err(DecorationEvaluationError::RuntimeFailed { status, stderr });
            };
        if response.request_id != request_id {
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeProtocol(format!(
                "mismatched response id: expected {request_id}, got {}",
                response.request_id
            )));
        }
        if response.kind != "pointerMoveAsync" {
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeProtocol(format!(
                "mismatched response kind for pointerMoveAsync: {}",
                response.kind
            )));
        }
        if !response.ok {
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeProtocol(
                response
                    .error
                    .unwrap_or_else(|| "runtime returned failure".into()),
            ));
        }

        Ok(Some(DecorationPointerMoveAsyncInvocation {
            invoked: response.invoked.unwrap_or(false),
            dirty: response.dirty.unwrap_or(false),
            dirty_window_ids: response.dirty_window_ids.unwrap_or_default(),
            dirty_managed_window_ids: response.dirty_managed_window_ids.unwrap_or_default(),
            dirty_window_node_ids: response.dirty_window_node_ids.unwrap_or_default(),
            dirty_layer_node_ids: response.dirty_layer_node_ids.unwrap_or_default(),
            actions: response.actions.unwrap_or_default(),
            next_poll_in_ms: response.next_poll_in_ms,
            display_config: response.display_config,
            workspace_config: response.workspace_config,
            key_binding_config: response.key_binding_config,
            pointer_config: response.pointer_config,
            input_config: response.input_config,
            event_config: response.event_config,
            process_config: response.process_config,
            process_actions: response.process_actions.unwrap_or_default(),
        }))
    }

    fn dispatch_gesture_swipe_async(
        &self,
        event: &GestureSwipeEventSnapshot,
        now_ms: u64,
    ) -> Result<Option<DecorationGestureSwipeAsyncInvocation>, DecorationEvaluationError> {
        if !self
            .pointer_move_async
            .runtime_dispatchable
            .load(Ordering::Acquire)
        {
            return Ok(None);
        }
        let Ok(mut runtime_guard) = self.runtime.try_lock() else {
            return Ok(None);
        };
        // See `dispatch_pointer_move_async`: the worker never spawns an isolate.
        let Some(runtime) = runtime_guard.as_mut() else {
            return Ok(None);
        };
        let request_id = runtime.next_request_id;
        runtime.next_request_id += 1;
        let (display_state, input_state, runtime_state_generation) =
            self.interaction_state_payload(runtime.last_sent_runtime_state_generation);

        runtime.write_interaction_request(NativeInteractionRequest::GestureSwipeAsync {
            request_id,
            event: event.clone(),
            now_ms,
            display_state,
            input_state,
        })?;
        runtime.last_sent_runtime_state_generation = runtime_state_generation;

        let response: RuntimeGestureSwipeAsyncResponse =
            if let Some(response) = runtime.read_interaction_response()? {
                response
            } else {
                let status = runtime
                    .child
                    .try_wait()?
                    .and_then(|status| status.code())
                    .unwrap_or(-1);
                let stderr = runtime
                    .stderr_log
                    .lock()
                    .map(|stderr| stderr.clone())
                    .unwrap_or_default();
                *runtime_guard = None;
                return Err(DecorationEvaluationError::RuntimeFailed { status, stderr });
            };
        if response.request_id != request_id {
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeProtocol(format!(
                "mismatched response id: expected {request_id}, got {}",
                response.request_id
            )));
        }
        if response.kind != "gestureSwipeAsync" {
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeProtocol(format!(
                "mismatched response kind for gestureSwipeAsync: {}",
                response.kind
            )));
        }
        if !response.ok {
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeProtocol(
                response
                    .error
                    .unwrap_or_else(|| "runtime returned failure".into()),
            ));
        }

        Ok(Some(DecorationGestureSwipeAsyncInvocation {
            invoked: response.invoked.unwrap_or(false),
            dirty: response.dirty.unwrap_or(false),
            dirty_window_ids: response.dirty_window_ids.unwrap_or_default(),
            dirty_managed_window_ids: response.dirty_managed_window_ids.unwrap_or_default(),
            dirty_window_node_ids: response.dirty_window_node_ids.unwrap_or_default(),
            dirty_layer_node_ids: response.dirty_layer_node_ids.unwrap_or_default(),
            actions: response.actions.unwrap_or_default(),
            next_poll_in_ms: response.next_poll_in_ms,
            display_config: response.display_config,
            workspace_config: response.workspace_config,
            key_binding_config: response.key_binding_config,
            pointer_config: response.pointer_config,
            input_config: response.input_config,
            event_config: response.event_config,
            process_config: response.process_config,
            process_actions: response.process_actions.unwrap_or_default(),
        }))
    }
}

impl Clone for EmbeddedDecorationEvaluator {
    fn clone(&self) -> Self {
        Self {
            script_path: self.script_path.clone(),
            config_path: self.config_path.clone(),
            working_dir: self.working_dir.clone(),
            runtime: Arc::clone(&self.runtime),
            display_state: Arc::clone(&self.display_state),
            input_state: Arc::clone(&self.input_state),
            runtime_state_generation: Arc::clone(&self.runtime_state_generation),
            pointer_move_async: Arc::clone(&self.pointer_move_async),
            async_event_sender: Arc::clone(&self.async_event_sender),
        }
    }
}

impl EmbeddedDecorationRuntime {
    fn write_request(&mut self, request: &str) -> Result<(), DecorationEvaluationError> {
        timescope::scope!("runtime write request");
        let bytes = request.as_bytes();
        let _ = u32::try_from(bytes.len()).map_err(|_| {
            DecorationEvaluationError::RuntimeProtocol("runtime request too large".into())
        })?;
        record_runtime_protocol_request(request, bytes.len());
        self.child
            .write_request(request)
            .map_err(DecorationEvaluationError::RuntimeProtocol)
    }

    fn write_composition_request(
        &mut self,
        request: NativeCompositionRequest,
    ) -> Result<(), DecorationEvaluationError> {
        timescope::scope!("runtime write composition request");
        self.child
            .write_composition_request(request)
            .map_err(DecorationEvaluationError::RuntimeProtocol)
    }

    fn write_effect_request(
        &mut self,
        request: NativeEffectRequest,
    ) -> Result<(), DecorationEvaluationError> {
        timescope::scope!("runtime write effect request");
        self.child
            .write_effect_request(request)
            .map_err(DecorationEvaluationError::RuntimeProtocol)
    }

    fn write_interaction_request(
        &mut self,
        request: NativeInteractionRequest,
    ) -> Result<(), DecorationEvaluationError> {
        timescope::scope!("runtime write interaction request");
        self.child
            .write_interaction_request(request)
            .map_err(DecorationEvaluationError::RuntimeProtocol)
    }

    fn write_scheduler_request(
        &mut self,
        request: NativeSchedulerRequest,
    ) -> Result<(), DecorationEvaluationError> {
        timescope::scope!("runtime write scheduler request");
        self.child
            .write_scheduler_request(request)
            .map_err(DecorationEvaluationError::RuntimeProtocol)
    }

    fn write_cached_fast_request(
        &mut self,
        request_id: u64,
        window_id: String,
        force_full_reevaluation: bool,
        now_ms: u64,
    ) -> Result<(), DecorationEvaluationError> {
        timescope::scope!("runtime write cached fast request");
        self.child
            .write_cached_fast_request(request_id, window_id, force_full_reevaluation, now_ms)
            .map_err(DecorationEvaluationError::RuntimeProtocol)
    }

    fn write_scheduler_fast_request(
        &mut self,
        request_id: u64,
        now_ms: u64,
    ) -> Result<(), DecorationEvaluationError> {
        timescope::scope!("runtime write scheduler fast request");
        self.child
            .write_scheduler_fast_request(request_id, now_ms)
            .map_err(DecorationEvaluationError::RuntimeProtocol)
    }

    fn take_composition_update(
        &self,
        request_id: u64,
    ) -> Result<Option<NativeCompositionUpdate>, DecorationEvaluationError> {
        timescope::scope!("runtime take composition update");
        self.child
            .take_composition_update(request_id)
            .map_err(DecorationEvaluationError::RuntimeProtocol)
    }

    fn take_effect_update(
        &self,
        request_id: u64,
    ) -> Result<
        Option<super::embedded_runtime::ResolvedNativeEffectUpdate>,
        DecorationEvaluationError,
    > {
        timescope::scope!("runtime take effect update");
        self.child
            .take_effect_update(request_id)
            .map_err(DecorationEvaluationError::RuntimeProtocol)
    }

    fn read_response<T: serde::de::DeserializeOwned>(
        &mut self,
    ) -> Result<Option<T>, DecorationEvaluationError> {
        timescope::scope!("runtime read response");
        let payload = {
            timescope::scope!("runtime read frame");
            self.child
                .read_response()
                .map_err(DecorationEvaluationError::RuntimeProtocol)?
        };
        let Some(response) = payload else {
            return Ok(None);
        };
        let EmbeddedRuntimeResponse::Json(payload) = response else {
            return Err(DecorationEvaluationError::RuntimeProtocol(
                "received native metadata for a JSON response".into(),
            ));
        };
        self.decode_json_response(payload)
    }

    fn read_scheduler_response(
        &mut self,
    ) -> Result<Option<RuntimeSchedulerResponse>, DecorationEvaluationError> {
        timescope::scope!("runtime read scheduler response");
        let response = self
            .child
            .read_response()
            .map_err(DecorationEvaluationError::RuntimeProtocol)?;
        match response {
            None => Ok(None),
            Some(EmbeddedRuntimeResponse::Scheduler(response)) => {
                Ok(Some(runtime_scheduler_response_from_native(response)))
            }
            Some(EmbeddedRuntimeResponse::Json(payload)) => self.decode_json_response(payload),
            Some(_) => Err(DecorationEvaluationError::RuntimeProtocol(
                "received mismatched native response for schedulerTick".into(),
            )),
        }
    }

    fn read_cached_response(
        &mut self,
    ) -> Result<Option<RuntimeEvaluateResponse>, DecorationEvaluationError> {
        timescope::scope!("runtime read cached response");
        let response = self
            .child
            .read_response()
            .map_err(DecorationEvaluationError::RuntimeProtocol)?;
        match response {
            None => Ok(None),
            Some(EmbeddedRuntimeResponse::Cached(response)) => {
                Ok(Some(runtime_evaluate_response_from_native(response)))
            }
            Some(EmbeddedRuntimeResponse::Json(payload)) => self.decode_json_response(payload),
            Some(_) => Err(DecorationEvaluationError::RuntimeProtocol(
                "received mismatched native response for evaluateCached".into(),
            )),
        }
    }

    fn read_interaction_response(
        &mut self,
    ) -> Result<Option<RuntimePointerMoveAsyncResponse>, DecorationEvaluationError> {
        timescope::scope!("runtime read interaction response");
        let response = self
            .child
            .read_response()
            .map_err(DecorationEvaluationError::RuntimeProtocol)?;
        match response {
            None => Ok(None),
            Some(EmbeddedRuntimeResponse::Interaction(response)) => {
                Ok(Some(runtime_interaction_response_from_native(response)))
            }
            Some(EmbeddedRuntimeResponse::Json(payload)) => self.decode_json_response(payload),
            Some(_) => Err(DecorationEvaluationError::RuntimeProtocol(
                "received mismatched native response for interaction event".into(),
            )),
        }
    }

    fn decode_json_response<T: serde::de::DeserializeOwned>(
        &self,
        payload: Vec<u8>,
    ) -> Result<Option<T>, DecorationEvaluationError> {
        let value: serde_json::Value = {
            timescope::scope!("runtime json parse value");
            serde_json::from_slice(&payload).map_err(|error| {
                DecorationEvaluationError::InvalidResponse(format!(
                    "{error}; payload={}",
                    String::from_utf8_lossy(&payload)
                ))
            })?
        };

        record_runtime_protocol_response(
            value
                .get("kind")
                .and_then(|kind| kind.as_str())
                .unwrap_or("<missing>"),
            payload.len(),
        );

        if let Some(env_updates) = value.get("envUpdates") {
            timescope::scope!("runtime env updates");
            let env_updates: RuntimeEnvUpdates = serde_json::from_value(env_updates.clone())
                .map_err(|error| {
                    DecorationEvaluationError::InvalidResponse(format!(
                        "invalid envUpdates: {error}; payload={}",
                        String::from_utf8_lossy(&payload)
                    ))
                })?;
            apply_runtime_env_updates(env_updates, "typescript-runtime");
        }

        if let Some(cursor_config) = value.get("cursorConfig") {
            timescope::scope!("runtime cursor config");
            let cursor_config: crate::cursor::RuntimeCursorConfigUpdate =
                serde_json::from_value(cursor_config.clone()).map_err(|error| {
                    DecorationEvaluationError::InvalidResponse(format!(
                        "invalid cursorConfig: {error}; payload={}",
                        String::from_utf8_lossy(&payload)
                    ))
                })?;
            if let Ok(sender) = self.async_event_sender.lock()
                && let Some(sender) = sender.as_ref()
            {
                sender
                    .send(DecorationRuntimeAsyncInvocation::CursorConfig(
                        cursor_config,
                    ))
                    .map_err(|error| {
                        DecorationEvaluationError::RuntimeProtocol(format!(
                            "failed to dispatch runtime cursor config: {error}"
                        ))
                    })?;
            }
        }

        {
            timescope::scope!("runtime deserialize response");
            serde_json::from_value(value).map(Some).map_err(|error| {
                DecorationEvaluationError::InvalidResponse(format!(
                    "{error}; payload={}",
                    String::from_utf8_lossy(&payload)
                ))
            })
        }
    }
}

fn take_native_window_effects(
    runtime: &EmbeddedDecorationRuntime,
    request_id: u64,
    expected_window_id: &str,
) -> Result<Option<WindowEffectConfig>, DecorationEvaluationError> {
    take_native_window_effect_update(runtime, request_id, expected_window_id)
        .map(|(effects, _)| effects)
}

fn take_native_window_effect_update(
    runtime: &EmbeddedDecorationRuntime,
    request_id: u64,
    expected_window_id: &str,
) -> Result<(Option<WindowEffectConfig>, bool), DecorationEvaluationError> {
    timescope::scope!("runtime take native window effect update");
    match runtime.take_effect_update(request_id)? {
        Some(resolved)
            if matches!(
                &resolved.update,
                NativeEffectUpdate::Window { window_id, .. } if expected_window_id == window_id
            ) =>
        {
            let NativeEffectUpdate::Window { effects, .. } = resolved.update else {
                unreachable!();
            };
            Ok((effects, resolved.uniform_only))
        }
        Some(_) => Err(DecorationEvaluationError::RuntimeProtocol(
            "mismatched native window effect update".into(),
        )),
        None => Err(DecorationEvaluationError::RuntimeProtocol(
            "missing native window effect update".into(),
        )),
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct RuntimeProtocolCounter {
    count: u64,
    bytes: u64,
}

#[derive(Debug)]
struct RuntimeProtocolStats {
    last_log_at: Instant,
    requests: std::collections::BTreeMap<String, RuntimeProtocolCounter>,
    responses: std::collections::BTreeMap<String, RuntimeProtocolCounter>,
    request_count: u64,
    response_count: u64,
    request_bytes: u64,
    response_bytes: u64,
}

impl RuntimeProtocolStats {
    fn new() -> Self {
        Self {
            last_log_at: Instant::now(),
            requests: std::collections::BTreeMap::new(),
            responses: std::collections::BTreeMap::new(),
            request_count: 0,
            response_count: 0,
            request_bytes: 0,
            response_bytes: 0,
        }
    }

    fn clear_interval(&mut self, now: Instant) {
        self.last_log_at = now;
        self.requests.clear();
        self.responses.clear();
        self.request_count = 0;
        self.response_count = 0;
        self.request_bytes = 0;
        self.response_bytes = 0;
    }
}

fn runtime_protocol_stats_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var_os("SHOJI_RUNTIME_PROTOCOL_STATS")
            .is_some_and(|value| value != "0" && value != "off" && !value.is_empty())
    })
}

fn record_runtime_protocol_request(payload: &str, bytes: usize) {
    if !runtime_protocol_stats_enabled() {
        return;
    }
    let kind = extract_json_kind(payload).unwrap_or("<missing>");
    record_runtime_protocol_message(RuntimeProtocolDirection::Request, kind, bytes);
}

fn record_runtime_protocol_response(kind: &str, bytes: usize) {
    if !runtime_protocol_stats_enabled() {
        return;
    }
    record_runtime_protocol_message(RuntimeProtocolDirection::Response, kind, bytes);
}

#[derive(Debug, Clone, Copy)]
enum RuntimeProtocolDirection {
    Request,
    Response,
}

fn record_runtime_protocol_message(direction: RuntimeProtocolDirection, kind: &str, bytes: usize) {
    static STATS: OnceLock<Mutex<RuntimeProtocolStats>> = OnceLock::new();
    let stats = STATS.get_or_init(|| Mutex::new(RuntimeProtocolStats::new()));
    let Ok(mut stats) = stats.lock() else {
        return;
    };

    let bytes = bytes as u64;
    match direction {
        RuntimeProtocolDirection::Request => {
            let counter = stats.requests.entry(kind.to_owned()).or_default();
            counter.count = counter.count.saturating_add(1);
            counter.bytes = counter.bytes.saturating_add(bytes);
            stats.request_count = stats.request_count.saturating_add(1);
            stats.request_bytes = stats.request_bytes.saturating_add(bytes);
        }
        RuntimeProtocolDirection::Response => {
            let counter = stats.responses.entry(kind.to_owned()).or_default();
            counter.count = counter.count.saturating_add(1);
            counter.bytes = counter.bytes.saturating_add(bytes);
            stats.response_count = stats.response_count.saturating_add(1);
            stats.response_bytes = stats.response_bytes.saturating_add(bytes);
        }
    }

    let now = Instant::now();
    let interval = now.duration_since(stats.last_log_at);
    if interval < Duration::from_secs(1) {
        return;
    }

    let interval_ms = interval.as_secs_f64() * 1000.0;
    let requests = summarize_runtime_protocol_counters(&stats.requests);
    let responses = summarize_runtime_protocol_counters(&stats.responses);
    info!(
        interval_ms,
        request_count = stats.request_count,
        response_count = stats.response_count,
        request_bytes = stats.request_bytes,
        response_bytes = stats.response_bytes,
        request_rate_hz = stats.request_count as f64 / interval.as_secs_f64(),
        response_rate_hz = stats.response_count as f64 / interval.as_secs_f64(),
        request_kib_per_s = stats.request_bytes as f64 / 1024.0 / interval.as_secs_f64(),
        response_kib_per_s = stats.response_bytes as f64 / 1024.0 / interval.as_secs_f64(),
        requests = ?requests,
        responses = ?responses,
        "runtime protocol stats"
    );
    stats.clear_interval(now);
}

fn summarize_runtime_protocol_counters(
    counters: &std::collections::BTreeMap<String, RuntimeProtocolCounter>,
) -> Vec<(String, u64, u64)> {
    let mut summary = counters
        .iter()
        .map(|(kind, counter)| (kind.clone(), counter.count, counter.bytes))
        .collect::<Vec<_>>();
    summary.sort_by(|left, right| {
        right
            .2
            .cmp(&left.2)
            .then_with(|| right.1.cmp(&left.1))
            .then_with(|| left.0.cmp(&right.0))
    });
    summary.truncate(12);
    summary
}

fn extract_json_kind(payload: &str) -> Option<&str> {
    let start = payload.find("\"kind\":\"")? + "\"kind\":\"".len();
    let rest = &payload[start..];
    let end = rest.find('"')?;
    Some(&rest[..end])
}

fn runtime_environment_snapshot() -> std::collections::BTreeMap<String, String> {
    [
        "WAYLAND_DISPLAY",
        "DISPLAY",
        "XDG_CURRENT_DESKTOP",
        "XDG_SESSION_DESKTOP",
        "DESKTOP_SESSION",
    ]
    .into_iter()
    .filter_map(|key| std::env::var(key).ok().map(|value| (key.to_owned(), value)))
    .collect()
}

impl Drop for EmbeddedDecorationRuntime {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
impl DecorationEvaluator for EmbeddedDecorationEvaluator {
    fn evaluate_window(
        &self,
        window: &WaylandWindowSnapshot,
        now_ms: u64,
    ) -> Result<DecorationEvaluationResult, DecorationEvaluationError> {
        timescope::scope!("runtime evaluate_window");
        let mut runtime_guard = {
            timescope::scope!("runtime lock");
            self.runtime.lock().map_err(|_| {
                DecorationEvaluationError::RuntimeProtocol("runtime mutex poisoned".into())
            })?
        };
        let runtime = {
            timescope::scope!("runtime ensure");
            self.ensure_runtime(&mut runtime_guard)?
        };
        let request_id = runtime.next_request_id;
        runtime.next_request_id += 1;
        let runtime_state_generation = self.runtime_state_generation.load(Ordering::Acquire);
        let (display_state, input_state) = {
            timescope::scope!("runtime clone state");
            let display_state = self
                .display_state
                .lock()
                .map(|guard| guard.clone())
                .unwrap_or_default();
            let input_state = self
                .input_state
                .lock()
                .map(|guard| guard.clone())
                .unwrap_or_default();
            (display_state, input_state)
        };

        {
            timescope::scope!("runtime evaluate_window write request");
            runtime.write_composition_request(NativeCompositionRequest::Evaluate {
                request_id,
                snapshot: window.clone(),
                now_ms,
                display_state,
                input_state,
            })?;
            runtime.last_sent_runtime_state_generation = runtime_state_generation;
        }

        let response: RuntimeEvaluateResponse = {
            timescope::scope!("runtime evaluate_window read response");
            if let Some(response) = runtime.read_response()? {
                response
            } else {
                let status = runtime
                    .child
                    .try_wait()?
                    .and_then(|status| status.code())
                    .unwrap_or(-1);
                let stderr = runtime
                    .stderr_log
                    .lock()
                    .map(|stderr| stderr.clone())
                    .unwrap_or_default();
                *runtime_guard = None;
                return Err(DecorationEvaluationError::RuntimeFailed { status, stderr });
            }
        };
        if response.request_id != request_id {
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeProtocol(format!(
                "mismatched response id: expected {request_id}, got {}",
                response.request_id
            )));
        }
        if response.kind != "evaluate" {
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeProtocol(format!(
                "mismatched response kind for evaluate: {}",
                response.kind
            )));
        }
        if !response.ok {
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeProtocol(
                response
                    .error
                    .unwrap_or_else(|| "runtime returned failure".into()),
            ));
        }

        let node = match runtime.take_composition_update(request_id)? {
            Some(NativeCompositionUpdate::Full { window_id, node }) if window_id == window.id => {
                node
            }
            Some(update) => {
                *runtime_guard = None;
                return Err(DecorationEvaluationError::RuntimeProtocol(format!(
                    "invalid native composition update for evaluate: window={}, expected={}",
                    update.window_id(),
                    window.id
                )));
            }
            None => {
                *runtime_guard = None;
                return Err(DecorationEvaluationError::RuntimeProtocol(
                    "missing native composition tree".into(),
                ));
            }
        };
        let window_effects = take_native_window_effects(runtime, request_id, &window.id)?;
        Ok(DecorationEvaluationResult {
            node,
            transform: response.transform.unwrap_or_default(),
            managed_window: response.managed_window.unwrap_or_default(),
            window_effects,
            dirty_node_ids: response.dirty_node_ids.unwrap_or_default(),
            next_poll_in_ms: response.next_poll_in_ms,
            actions: response.actions.unwrap_or_default(),
            display_config: response.display_config,
            workspace_config: response.workspace_config,
            key_binding_config: response.key_binding_config,
            pointer_config: response.pointer_config,
            input_config: response.input_config,
            event_config: response.event_config,
            process_config: response.process_config,
            process_actions: response.process_actions.unwrap_or_default(),
        })
    }

    fn evaluate_window_preview(
        &self,
        window: &WaylandWindowSnapshot,
        now_ms: u64,
    ) -> Result<DecorationEvaluationResult, DecorationEvaluationError> {
        timescope::scope!("runtime evaluate_window_preview");
        let mut runtime_guard = {
            timescope::scope!("runtime lock");
            self.runtime.lock().map_err(|_| {
                DecorationEvaluationError::RuntimeProtocol("runtime mutex poisoned".into())
            })?
        };
        let runtime = {
            timescope::scope!("runtime ensure");
            self.ensure_runtime(&mut runtime_guard)?
        };
        let request_id = runtime.next_request_id;
        runtime.next_request_id += 1;
        let runtime_state_generation = self.runtime_state_generation.load(Ordering::Acquire);
        let (display_state, input_state) = {
            timescope::scope!("runtime clone state");
            let display_state = self
                .display_state
                .lock()
                .map(|guard| guard.clone())
                .unwrap_or_default();
            let input_state = self
                .input_state
                .lock()
                .map(|guard| guard.clone())
                .unwrap_or_default();
            (display_state, input_state)
        };

        {
            timescope::scope!("runtime evaluate_window_preview write request");
            runtime.write_composition_request(NativeCompositionRequest::EvaluatePreview {
                request_id,
                snapshot: window.clone(),
                now_ms,
                display_state,
                input_state,
            })?;
            runtime.last_sent_runtime_state_generation = runtime_state_generation;
        }

        let response: RuntimeEvaluateResponse = {
            timescope::scope!("runtime evaluate_window_preview read response");
            if let Some(response) = runtime.read_response()? {
                response
            } else {
                let status = runtime
                    .child
                    .try_wait()?
                    .and_then(|status| status.code())
                    .unwrap_or(-1);
                let stderr = runtime
                    .stderr_log
                    .lock()
                    .map(|stderr| stderr.clone())
                    .unwrap_or_default();
                *runtime_guard = None;
                return Err(DecorationEvaluationError::RuntimeFailed { status, stderr });
            }
        };
        if response.request_id != request_id {
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeProtocol(format!(
                "mismatched response id: expected {request_id}, got {}",
                response.request_id
            )));
        }
        if response.kind != "evaluatePreview" {
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeProtocol(format!(
                "mismatched response kind for evaluatePreview: {}",
                response.kind
            )));
        }
        if !response.ok {
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeProtocol(
                response
                    .error
                    .unwrap_or_else(|| "runtime returned failure".into()),
            ));
        }

        let node = match runtime.take_composition_update(request_id)? {
            Some(NativeCompositionUpdate::Full { window_id, node }) if window_id == window.id => {
                node
            }
            Some(update) => {
                *runtime_guard = None;
                return Err(DecorationEvaluationError::RuntimeProtocol(format!(
                    "invalid native composition update for evaluatePreview: window={}, expected={}",
                    update.window_id(),
                    window.id
                )));
            }
            None => {
                *runtime_guard = None;
                return Err(DecorationEvaluationError::RuntimeProtocol(
                    "missing native composition preview tree".into(),
                ));
            }
        };
        Ok(DecorationEvaluationResult {
            node,
            transform: response.transform.unwrap_or_default(),
            managed_window: response.managed_window.unwrap_or_default(),
            window_effects: take_native_window_effects(runtime, request_id, &window.id)?,
            dirty_node_ids: response.dirty_node_ids.unwrap_or_default(),
            next_poll_in_ms: response.next_poll_in_ms,
            actions: response.actions.unwrap_or_default(),
            display_config: response.display_config,
            workspace_config: response.workspace_config,
            key_binding_config: response.key_binding_config,
            pointer_config: response.pointer_config,
            input_config: response.input_config,
            event_config: response.event_config,
            process_config: response.process_config,
            process_actions: response.process_actions.unwrap_or_default(),
        })
    }

    fn window_decoration_policy(
        &self,
        window: &WaylandWindowSnapshot,
        context: &WindowDecorationPolicyContextSnapshot,
    ) -> Result<WindowDecorationDecisionSnapshot, DecorationEvaluationError> {
        let mut runtime_guard = self.runtime.lock().map_err(|_| {
            DecorationEvaluationError::RuntimeProtocol("runtime mutex poisoned".into())
        })?;
        let runtime = self.ensure_runtime(&mut runtime_guard)?;
        let request_id = runtime.next_request_id;
        runtime.next_request_id += 1;
        let display_state = self
            .display_state
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default();
        let input_state = self
            .input_state
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default();
        let request = serde_json::to_string(&RuntimeRequest::WindowDecorationPolicy {
            request_id,
            snapshot: window,
            context,
            display_state: &display_state,
            input_state: &input_state,
        })
        .map_err(|err| DecorationEvaluationError::SnapshotSerialization(err.to_string()))?;
        runtime.write_request(&request)?;

        let response: RuntimeWindowDecorationPolicyResponse =
            if let Some(response) = runtime.read_response()? {
                response
            } else {
                let status = runtime
                    .child
                    .try_wait()?
                    .and_then(|status| status.code())
                    .unwrap_or(-1);
                let stderr = runtime
                    .stderr_log
                    .lock()
                    .map(|stderr| stderr.clone())
                    .unwrap_or_default();
                *runtime_guard = None;
                return Err(DecorationEvaluationError::RuntimeFailed { status, stderr });
            };
        if response.request_id != request_id || response.kind != "windowDecorationPolicy" {
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeProtocol(format!(
                "mismatched response for windowDecorationPolicy: id={}, kind={}",
                response.request_id, response.kind
            )));
        }
        if !response.ok {
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeProtocol(
                response
                    .error
                    .unwrap_or_else(|| "runtime returned failure".into()),
            ));
        }
        response.decision.ok_or_else(|| {
            DecorationEvaluationError::RuntimeProtocol("missing decoration decision".into())
        })
    }

    fn evaluate_cached_window(
        &self,
        window_id: &str,
        window: Option<&WaylandWindowSnapshot>,
        now_ms: u64,
        force_full_reevaluation: bool,
    ) -> Result<DecorationCachedEvaluationResult, DecorationEvaluationError> {
        timescope::scope!("runtime evaluate_cached_window");
        let mut runtime_guard = {
            timescope::scope!("runtime lock");
            self.runtime.lock().map_err(|_| {
                DecorationEvaluationError::RuntimeProtocol("runtime mutex poisoned".into())
            })?
        };
        let runtime = {
            timescope::scope!("runtime ensure");
            self.ensure_runtime(&mut runtime_guard)?
        };
        let request_id = runtime.next_request_id;
        runtime.next_request_id += 1;
        let runtime_state_generation = self.runtime_state_generation.load(Ordering::Acquire);
        let use_fast_request = window.is_none()
            && runtime.last_sent_runtime_state_generation == runtime_state_generation;

        {
            timescope::scope!("runtime evaluate_cached_window write request");
            if use_fast_request {
                runtime.write_cached_fast_request(
                    request_id,
                    window_id.to_owned(),
                    force_full_reevaluation,
                    now_ms,
                )?;
            } else {
                let (display_state, input_state) = {
                    timescope::scope!("runtime clone state");
                    let display_state = self
                        .display_state
                        .lock()
                        .map(|guard| guard.clone())
                        .unwrap_or_default();
                    let input_state = self
                        .input_state
                        .lock()
                        .map(|guard| guard.clone())
                        .unwrap_or_default();
                    (display_state, input_state)
                };
                runtime.write_composition_request(NativeCompositionRequest::EvaluateCached {
                    request_id,
                    window_id: window_id.to_owned(),
                    snapshot: window.cloned(),
                    force_full_reevaluation,
                    now_ms,
                    display_state,
                    input_state,
                })?;
                runtime.last_sent_runtime_state_generation = runtime_state_generation;
            }
        }

        let response: RuntimeEvaluateResponse = {
            timescope::scope!("runtime evaluate_cached_window read response");
            if let Some(response) = runtime.read_cached_response()? {
                response
            } else {
                let status = runtime
                    .child
                    .try_wait()?
                    .and_then(|status| status.code())
                    .unwrap_or(-1);
                let stderr = runtime
                    .stderr_log
                    .lock()
                    .map(|stderr| stderr.clone())
                    .unwrap_or_default();
                *runtime_guard = None;
                return Err(DecorationEvaluationError::RuntimeFailed { status, stderr });
            }
        };
        if response.request_id != request_id {
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeProtocol(format!(
                "mismatched response id: expected {request_id}, got {}",
                response.request_id
            )));
        }
        if response.kind != "evaluateCached" {
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeProtocol(format!(
                "mismatched response kind for evaluateCached: {}",
                response.kind
            )));
        }
        if !response.ok {
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeProtocol(
                response
                    .error
                    .unwrap_or_else(|| "runtime returned failure".into()),
            ));
        }

        let managed_window_only = response.managed_window_only.unwrap_or(false);
        let native_update = runtime.take_composition_update(request_id)?;
        let (node, node_patches) = match (managed_window_only, native_update) {
            (true, None) => (None, Vec::new()),
            (true, Some(_)) => {
                *runtime_guard = None;
                return Err(DecorationEvaluationError::RuntimeProtocol(
                    "managed-window-only evaluation unexpectedly returned a composition update"
                        .into(),
                ));
            }
            (
                false,
                Some(NativeCompositionUpdate::Full {
                    window_id: update_window_id,
                    node,
                }),
            ) if update_window_id == window_id => (Some(node), Vec::new()),
            (
                false,
                Some(NativeCompositionUpdate::Patches {
                    window_id: update_window_id,
                    patches,
                }),
            ) if update_window_id == window_id => (None, patches),
            (false, Some(update)) => {
                *runtime_guard = None;
                return Err(DecorationEvaluationError::RuntimeProtocol(format!(
                    "native cached composition window mismatch: got={}, expected={window_id}",
                    update.window_id()
                )));
            }
            (false, None) => {
                *runtime_guard = None;
                return Err(DecorationEvaluationError::RuntimeProtocol(
                    "missing native cached composition update".into(),
                ));
            }
        };
        let (window_effects, window_effect_uniform_only) =
            take_native_window_effect_update(runtime, request_id, window_id)?;
        Ok(DecorationCachedEvaluationResult {
            node,
            node_patches,
            transform: response.transform.unwrap_or_default(),
            managed_window: response.managed_window.unwrap_or_default(),
            window_effects,
            window_effect_uniform_only,
            dirty_node_ids: response.dirty_node_ids.unwrap_or_default(),
            managed_window_only,
            next_poll_in_ms: response.next_poll_in_ms,
            actions: response.actions.unwrap_or_default(),
            display_config: response.display_config,
            workspace_config: response.workspace_config,
            key_binding_config: response.key_binding_config,
            pointer_config: response.pointer_config,
            input_config: response.input_config,
            event_config: response.event_config,
            process_config: response.process_config,
            process_actions: response.process_actions.unwrap_or_default(),
        })
    }

    fn scheduler_tick(
        &self,
        now_ms: u64,
    ) -> Result<DecorationSchedulerTick, DecorationEvaluationError> {
        let mut runtime_guard = self.runtime.lock().map_err(|_| {
            DecorationEvaluationError::RuntimeProtocol("runtime mutex poisoned".into())
        })?;

        let Some(_) = runtime_guard.as_ref() else {
            return Ok(DecorationSchedulerTick::default());
        };

        let runtime = self.ensure_runtime(&mut runtime_guard)?;
        let request_id = runtime.next_request_id;
        runtime.next_request_id += 1;
        let runtime_state_generation = self.runtime_state_generation.load(Ordering::Acquire);
        if runtime.last_sent_runtime_state_generation == runtime_state_generation {
            runtime.write_scheduler_fast_request(request_id, now_ms)?;
        } else {
            let display_state = self
                .display_state
                .lock()
                .map(|guard| guard.clone())
                .unwrap_or_default();
            let input_state = self
                .input_state
                .lock()
                .map(|guard| guard.clone())
                .unwrap_or_default();

            runtime.write_scheduler_request(NativeSchedulerRequest {
                request_id,
                kind: "schedulerTick",
                now_ms,
                display_state,
                input_state,
            })?;
            runtime.last_sent_runtime_state_generation = runtime_state_generation;
        }

        let response: RuntimeSchedulerResponse =
            if let Some(response) = runtime.read_scheduler_response()? {
                response
            } else {
                let status = runtime
                    .child
                    .try_wait()?
                    .and_then(|status| status.code())
                    .unwrap_or(-1);
                let stderr = runtime
                    .stderr_log
                    .lock()
                    .map(|stderr| stderr.clone())
                    .unwrap_or_default();
                *runtime_guard = None;
                return Err(DecorationEvaluationError::RuntimeFailed { status, stderr });
            };
        if response.request_id != request_id {
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeProtocol(format!(
                "mismatched response id: expected {request_id}, got {}",
                response.request_id
            )));
        }
        if response.kind != "schedulerTick" {
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeProtocol(format!(
                "mismatched response kind for schedulerTick: {}",
                response.kind
            )));
        }
        if !response.ok {
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeProtocol(
                response
                    .error
                    .unwrap_or_else(|| "runtime returned failure".into()),
            ));
        }

        if managed_rect_debug_enabled() {
            info!(
                now_ms,
                dirty = response.dirty.unwrap_or(false),
                dirty_window_ids = ?response.dirty_window_ids,
                dirty_window_node_ids = ?response.dirty_window_node_ids,
                next_poll_in_ms = ?response.next_poll_in_ms,
                "managed rect debug: runtime scheduler tick"
            );
        }

        Ok(DecorationSchedulerTick {
            dirty: response.dirty.unwrap_or(false),
            runtime_dirty: response.runtime_dirty.unwrap_or(false),
            dirty_window_ids: response.dirty_window_ids.unwrap_or_default(),
            dirty_managed_window_ids: response.dirty_managed_window_ids.unwrap_or_default(),
            dirty_window_node_ids: response.dirty_window_node_ids.unwrap_or_default(),
            dirty_layer_ids: response.dirty_layer_ids.unwrap_or_default(),
            dirty_layer_node_ids: response.dirty_layer_node_ids.unwrap_or_default(),
            actions: response.actions.unwrap_or_default(),
            next_poll_in_ms: response.next_poll_in_ms,
            display_config: response.display_config,
            workspace_config: response.workspace_config,
            key_binding_config: response.key_binding_config,
            pointer_config: response.pointer_config,
            input_config: response.input_config,
            event_config: response.event_config,
            process_config: response.process_config,
            process_actions: response.process_actions.unwrap_or_default(),
            debug_config: response.debug_config,
        })
    }

    fn window_closed(&self, window_id: &str) -> Result<(), DecorationEvaluationError> {
        let mut runtime_guard = self.runtime.lock().map_err(|_| {
            DecorationEvaluationError::RuntimeProtocol("runtime mutex poisoned".into())
        })?;

        let Some(_) = runtime_guard.as_ref() else {
            return Ok(());
        };

        let runtime = self.ensure_runtime(&mut runtime_guard)?;
        let request_id = runtime.next_request_id;
        runtime.next_request_id += 1;
        let display_state = self
            .display_state
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default();
        let input_state = self
            .input_state
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default();

        let request = serde_json::to_string(&RuntimeRequest::WindowClosed {
            request_id,
            window_id,
            display_state: &display_state,
            input_state: &input_state,
        })
        .map_err(|err| DecorationEvaluationError::SnapshotSerialization(err.to_string()))?;
        runtime.write_request(&request)?;

        let response: RuntimeClosedResponse = if let Some(response) = runtime.read_response()? {
            response
        } else {
            let status = runtime
                .child
                .try_wait()?
                .and_then(|status| status.code())
                .unwrap_or(-1);
            let stderr = runtime
                .stderr_log
                .lock()
                .map(|stderr| stderr.clone())
                .unwrap_or_default();
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeFailed { status, stderr });
        };
        if response.request_id != request_id {
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeProtocol(format!(
                "mismatched response id: expected {request_id}, got {}",
                response.request_id
            )));
        }
        if response.kind != "windowClosed" {
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeProtocol(format!(
                "mismatched response kind for windowClosed: {}",
                response.kind
            )));
        }
        if !response.ok {
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeProtocol(
                response
                    .error
                    .unwrap_or_else(|| "runtime returned failure".into()),
            ));
        }

        Ok(())
    }

    fn invoke_handler(
        &self,
        window_id: &str,
        handler_id: &str,
        now_ms: u64,
    ) -> Result<DecorationHandlerInvocation, DecorationEvaluationError> {
        let mut runtime_guard = self.runtime.lock().map_err(|_| {
            DecorationEvaluationError::RuntimeProtocol("runtime mutex poisoned".into())
        })?;

        let Some(_) = runtime_guard.as_ref() else {
            return Ok(DecorationHandlerInvocation::default());
        };

        let runtime = self.ensure_runtime(&mut runtime_guard)?;
        let request_id = runtime.next_request_id;
        runtime.next_request_id += 1;
        let display_state = self
            .display_state
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default();
        let input_state = self
            .input_state
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default();

        let request = serde_json::to_string(&RuntimeRequest::InvokeHandler {
            request_id,
            window_id,
            handler_id,
            now_ms,
            display_state: &display_state,
            input_state: &input_state,
        })
        .map_err(|err| DecorationEvaluationError::SnapshotSerialization(err.to_string()))?;
        runtime.write_request(&request)?;

        let response: RuntimeInvokeHandlerResponse =
            if let Some(response) = runtime.read_response()? {
                response
            } else {
                let status = runtime
                    .child
                    .try_wait()?
                    .and_then(|status| status.code())
                    .unwrap_or(-1);
                let stderr = runtime
                    .stderr_log
                    .lock()
                    .map(|stderr| stderr.clone())
                    .unwrap_or_default();
                *runtime_guard = None;
                return Err(DecorationEvaluationError::RuntimeFailed { status, stderr });
            };
        if response.request_id != request_id {
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeProtocol(format!(
                "mismatched response id: expected {request_id}, got {}",
                response.request_id
            )));
        }
        if response.kind != "invokeHandler" {
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeProtocol(format!(
                "mismatched response kind for invokeHandler: {}",
                response.kind
            )));
        }
        if !response.ok {
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeProtocol(
                response
                    .error
                    .unwrap_or_else(|| "runtime returned failure".into()),
            ));
        }

        let node = if let Some(serialized) = response.serialized {
            let stdout = serde_json::to_string(&serialized)
                .map_err(|err| DecorationEvaluationError::InvalidResponse(err.to_string()))?;
            Some(decode_tree_json(stdout.trim()).map_err(DecorationEvaluationError::Bridge)?)
        } else {
            None
        };

        Ok(DecorationHandlerInvocation {
            close_animation_duration_ms: None,
            invoked: response.invoked.unwrap_or(false),
            node,
            transform: response.transform,
            managed_window: response.managed_window,
            window_effects: take_native_window_effects(runtime, request_id, window_id)?,
            dirty_window_ids: response.dirty_window_ids.unwrap_or_default(),
            dirty_managed_window_ids: response.dirty_managed_window_ids.unwrap_or_default(),
            dirty_window_node_ids: response.dirty_window_node_ids.unwrap_or_default(),
            actions: response.actions.unwrap_or_default(),
            next_poll_in_ms: response.next_poll_in_ms,
            display_config: response.display_config,
            workspace_config: response.workspace_config,
            key_binding_config: response.key_binding_config,
            pointer_config: response.pointer_config,
            input_config: response.input_config,
            event_config: response.event_config,
            process_config: response.process_config,
            process_actions: response.process_actions.unwrap_or_default(),
        })
    }

    fn invoke_key_binding(
        &self,
        binding_id: &str,
        now_ms: u64,
    ) -> Result<DecorationKeyBindingInvocation, DecorationEvaluationError> {
        let mut runtime_guard = self.runtime.lock().map_err(|_| {
            DecorationEvaluationError::RuntimeProtocol("runtime mutex poisoned".into())
        })?;

        let Some(_) = runtime_guard.as_ref() else {
            return Ok(DecorationKeyBindingInvocation::default());
        };

        let runtime = self.ensure_runtime(&mut runtime_guard)?;
        let request_id = runtime.next_request_id;
        runtime.next_request_id += 1;
        let display_state = self
            .display_state
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default();
        let input_state = self
            .input_state
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default();

        let request = serde_json::to_string(&RuntimeRequest::InvokeKeyBinding {
            request_id,
            binding_id,
            now_ms,
            display_state: &display_state,
            input_state: &input_state,
        })
        .map_err(|err| DecorationEvaluationError::SnapshotSerialization(err.to_string()))?;
        runtime.write_request(&request)?;

        let response: RuntimeInvokeKeyBindingResponse =
            if let Some(response) = runtime.read_response()? {
                response
            } else {
                let status = runtime
                    .child
                    .try_wait()?
                    .and_then(|status| status.code())
                    .unwrap_or(-1);
                let stderr = runtime
                    .stderr_log
                    .lock()
                    .map(|stderr| stderr.clone())
                    .unwrap_or_default();
                *runtime_guard = None;
                return Err(DecorationEvaluationError::RuntimeFailed { status, stderr });
            };
        if response.request_id != request_id {
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeProtocol(format!(
                "mismatched response id: expected {request_id}, got {}",
                response.request_id
            )));
        }
        if response.kind != "invokeKeyBinding" {
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeProtocol(format!(
                "mismatched response kind for invokeKeyBinding: {}",
                response.kind
            )));
        }
        if !response.ok {
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeProtocol(
                response
                    .error
                    .unwrap_or_else(|| "runtime returned failure".into()),
            ));
        }

        Ok(DecorationKeyBindingInvocation {
            invoked: response.invoked.unwrap_or(false),
            dirty: response.dirty.unwrap_or(false),
            dirty_window_ids: response.dirty_window_ids.unwrap_or_default(),
            dirty_managed_window_ids: response.dirty_managed_window_ids.unwrap_or_default(),
            dirty_window_node_ids: response.dirty_window_node_ids.unwrap_or_default(),
            dirty_layer_node_ids: response.dirty_layer_node_ids.unwrap_or_default(),
            actions: response.actions.unwrap_or_default(),
            next_poll_in_ms: response.next_poll_in_ms,
            display_config: response.display_config,
            workspace_config: response.workspace_config,
            key_binding_config: response.key_binding_config,
            pointer_config: response.pointer_config,
            input_config: response.input_config,
            event_config: response.event_config,
            process_config: response.process_config,
            process_actions: response.process_actions.unwrap_or_default(),
            debug_config: response.debug_config,
        })
    }

    fn workspace_activate(
        &self,
        event: &RuntimeWorkspaceActivateRequestSnapshot,
        now_ms: u64,
    ) -> Result<DecorationHandlerInvocation, DecorationEvaluationError> {
        let mut runtime_guard = self.runtime.lock().map_err(|_| {
            DecorationEvaluationError::RuntimeProtocol("runtime mutex poisoned".into())
        })?;

        let Some(_) = runtime_guard.as_ref() else {
            return Ok(DecorationHandlerInvocation::default());
        };

        let runtime = self.ensure_runtime(&mut runtime_guard)?;
        let request_id = runtime.next_request_id;
        runtime.next_request_id += 1;
        let display_state = self
            .display_state
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default();
        let input_state = self
            .input_state
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default();

        let request = serde_json::to_string(&RuntimeRequest::WorkspaceActivate {
            request_id,
            workspace_id: &event.workspace_id,
            group_id: event.group_id.as_deref(),
            now_ms,
            display_state: &display_state,
            input_state: &input_state,
        })
        .map_err(|err| DecorationEvaluationError::SnapshotSerialization(err.to_string()))?;
        runtime.write_request(&request)?;

        let response: RuntimeInvokeHandlerResponse =
            if let Some(response) = runtime.read_response()? {
                response
            } else {
                let status = runtime
                    .child
                    .try_wait()?
                    .and_then(|status| status.code())
                    .unwrap_or(-1);
                let stderr = runtime
                    .stderr_log
                    .lock()
                    .map(|stderr| stderr.clone())
                    .unwrap_or_default();
                *runtime_guard = None;
                return Err(DecorationEvaluationError::RuntimeFailed { status, stderr });
            };
        if response.request_id != request_id {
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeProtocol(format!(
                "mismatched response id: expected {request_id}, got {}",
                response.request_id
            )));
        }
        if response.kind != "invokeHandler" {
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeProtocol(format!(
                "mismatched response kind for workspaceActivate: {}",
                response.kind
            )));
        }
        if !response.ok {
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeProtocol(
                response
                    .error
                    .unwrap_or_else(|| "runtime returned failure".into()),
            ));
        }

        let node = if let Some(serialized) = response.serialized {
            let stdout = serde_json::to_string(&serialized)
                .map_err(|err| DecorationEvaluationError::InvalidResponse(err.to_string()))?;
            Some(decode_tree_json(stdout.trim()).map_err(DecorationEvaluationError::Bridge)?)
        } else {
            None
        };

        Ok(DecorationHandlerInvocation {
            close_animation_duration_ms: None,
            invoked: response.invoked.unwrap_or(false),
            node,
            transform: response.transform,
            managed_window: response.managed_window,
            window_effects: None,
            dirty_window_ids: response.dirty_window_ids.unwrap_or_default(),
            dirty_managed_window_ids: response.dirty_managed_window_ids.unwrap_or_default(),
            dirty_window_node_ids: response.dirty_window_node_ids.unwrap_or_default(),
            actions: response.actions.unwrap_or_default(),
            next_poll_in_ms: response.next_poll_in_ms,
            display_config: response.display_config,
            workspace_config: response.workspace_config,
            key_binding_config: response.key_binding_config,
            pointer_config: response.pointer_config,
            input_config: response.input_config,
            event_config: response.event_config,
            process_config: response.process_config,
            process_actions: response.process_actions.unwrap_or_default(),
        })
    }

    fn window_resize(
        &self,
        window_id: &str,
        event: &WindowResizeEventSnapshot,
        now_ms: u64,
    ) -> Result<DecorationWindowResizeInvocation, DecorationEvaluationError> {
        let mut runtime_guard = self.runtime.lock().map_err(|_| {
            DecorationEvaluationError::RuntimeProtocol("runtime mutex poisoned".into())
        })?;

        let Some(_) = runtime_guard.as_ref() else {
            return Ok(DecorationWindowResizeInvocation::default());
        };

        let runtime = self.ensure_runtime(&mut runtime_guard)?;
        let request_id = runtime.next_request_id;
        runtime.next_request_id += 1;
        let (display_state, input_state, runtime_state_generation) =
            self.interaction_state_payload(runtime.last_sent_runtime_state_generation);

        runtime.write_interaction_request(NativeInteractionRequest::WindowResize {
            request_id,
            window_id: window_id.to_owned(),
            event: event.clone(),
            now_ms,
            display_state,
            input_state,
        })?;
        runtime.last_sent_runtime_state_generation = runtime_state_generation;

        let response: RuntimePointerMoveAsyncResponse =
            if let Some(response) = runtime.read_interaction_response()? {
                response
            } else {
                let status = runtime
                    .child
                    .try_wait()?
                    .and_then(|status| status.code())
                    .unwrap_or(-1);
                let stderr = runtime
                    .stderr_log
                    .lock()
                    .map(|stderr| stderr.clone())
                    .unwrap_or_default();
                *runtime_guard = None;
                return Err(DecorationEvaluationError::RuntimeFailed { status, stderr });
            };
        if response.request_id != request_id {
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeProtocol(format!(
                "mismatched response id: expected {request_id}, got {}",
                response.request_id
            )));
        }
        if response.kind != "windowResize" {
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeProtocol(format!(
                "mismatched response kind for windowResize: {}",
                response.kind
            )));
        }
        if !response.ok {
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeProtocol(
                response
                    .error
                    .unwrap_or_else(|| "runtime returned failure".into()),
            ));
        }

        Ok(DecorationWindowResizeInvocation {
            invoked: response.invoked.unwrap_or(false),
            dirty: response.dirty.unwrap_or(false),
            dirty_window_ids: response.dirty_window_ids.unwrap_or_default(),
            dirty_managed_window_ids: response.dirty_managed_window_ids.unwrap_or_default(),
            dirty_window_node_ids: response.dirty_window_node_ids.unwrap_or_default(),
            dirty_layer_node_ids: response.dirty_layer_node_ids.unwrap_or_default(),
            actions: response.actions.unwrap_or_default(),
            next_poll_in_ms: response.next_poll_in_ms,
            display_config: response.display_config,
            workspace_config: response.workspace_config,
            key_binding_config: response.key_binding_config,
            pointer_config: response.pointer_config,
            input_config: response.input_config,
            event_config: response.event_config,
            process_config: response.process_config,
            process_actions: response.process_actions.unwrap_or_default(),
        })
    }

    fn window_move(
        &self,
        window_id: &str,
        event: &WindowMoveEventSnapshot,
        now_ms: u64,
    ) -> Result<DecorationWindowMoveInvocation, DecorationEvaluationError> {
        let mut runtime_guard = self.runtime.lock().map_err(|_| {
            DecorationEvaluationError::RuntimeProtocol("runtime mutex poisoned".into())
        })?;

        let Some(_) = runtime_guard.as_ref() else {
            return Ok(DecorationWindowMoveInvocation::default());
        };

        let runtime = self.ensure_runtime(&mut runtime_guard)?;
        let request_id = runtime.next_request_id;
        runtime.next_request_id += 1;
        let (display_state, input_state, runtime_state_generation) =
            self.interaction_state_payload(runtime.last_sent_runtime_state_generation);

        runtime.write_interaction_request(NativeInteractionRequest::WindowMove {
            request_id,
            window_id: window_id.to_owned(),
            event: event.clone(),
            now_ms,
            display_state,
            input_state,
        })?;
        runtime.last_sent_runtime_state_generation = runtime_state_generation;

        let response: RuntimePointerMoveAsyncResponse =
            if let Some(response) = runtime.read_interaction_response()? {
                response
            } else {
                let status = runtime
                    .child
                    .try_wait()?
                    .and_then(|status| status.code())
                    .unwrap_or(-1);
                let stderr = runtime
                    .stderr_log
                    .lock()
                    .map(|stderr| stderr.clone())
                    .unwrap_or_default();
                *runtime_guard = None;
                return Err(DecorationEvaluationError::RuntimeFailed { status, stderr });
            };
        if response.request_id != request_id {
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeProtocol(format!(
                "mismatched response id: expected {request_id}, got {}",
                response.request_id
            )));
        }
        if response.kind != "windowMove" {
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeProtocol(format!(
                "mismatched response kind for windowMove: {}",
                response.kind
            )));
        }
        if !response.ok {
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeProtocol(
                response
                    .error
                    .unwrap_or_else(|| "runtime returned failure".into()),
            ));
        }

        Ok(DecorationWindowMoveInvocation {
            invoked: response.invoked.unwrap_or(false),
            dirty: response.dirty.unwrap_or(false),
            dirty_window_ids: response.dirty_window_ids.unwrap_or_default(),
            dirty_managed_window_ids: response.dirty_managed_window_ids.unwrap_or_default(),
            dirty_window_node_ids: response.dirty_window_node_ids.unwrap_or_default(),
            dirty_layer_node_ids: response.dirty_layer_node_ids.unwrap_or_default(),
            actions: response.actions.unwrap_or_default(),
            next_poll_in_ms: response.next_poll_in_ms,
            display_config: response.display_config,
            workspace_config: response.workspace_config,
            key_binding_config: response.key_binding_config,
            pointer_config: response.pointer_config,
            input_config: response.input_config,
            event_config: response.event_config,
            process_config: response.process_config,
            process_actions: response.process_actions.unwrap_or_default(),
        })
    }

    fn window_maximize_request(
        &self,
        snapshot: &WaylandWindowSnapshot,
        event: &WindowMaximizeRequestEventSnapshot,
        now_ms: u64,
    ) -> Result<DecorationWindowStateRequestInvocation, DecorationEvaluationError> {
        let mut runtime_guard = self.runtime.lock().map_err(|_| {
            DecorationEvaluationError::RuntimeProtocol("runtime mutex poisoned".into())
        })?;

        let runtime = self.ensure_runtime(&mut runtime_guard)?;
        let request_id = runtime.next_request_id;
        runtime.next_request_id += 1;
        let display_state = self
            .display_state
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default();
        let input_state = self
            .input_state
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default();

        let request = serde_json::to_string(&RuntimeRequest::WindowMaximizeRequest {
            request_id,
            window_id: &snapshot.id,
            snapshot,
            event,
            now_ms,
            display_state: &display_state,
            input_state: &input_state,
        })
        .map_err(|err| DecorationEvaluationError::SnapshotSerialization(err.to_string()))?;
        runtime.write_request(&request)?;

        let response: RuntimeWindowStateRequestResponse =
            if let Some(response) = runtime.read_response()? {
                response
            } else {
                let status = runtime
                    .child
                    .try_wait()?
                    .and_then(|status| status.code())
                    .unwrap_or(-1);
                let stderr = runtime
                    .stderr_log
                    .lock()
                    .map(|stderr| stderr.clone())
                    .unwrap_or_default();
                *runtime_guard = None;
                return Err(DecorationEvaluationError::RuntimeFailed { status, stderr });
            };
        if response.request_id != request_id {
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeProtocol(format!(
                "mismatched response id: expected {request_id}, got {}",
                response.request_id
            )));
        }
        if response.kind != "windowMaximizeRequest" {
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeProtocol(format!(
                "mismatched response kind for windowMaximizeRequest: {}",
                response.kind
            )));
        }
        if !response.ok {
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeProtocol(
                response
                    .error
                    .unwrap_or_else(|| "runtime returned failure".into()),
            ));
        }

        Ok(DecorationWindowStateRequestInvocation {
            invoked: response.invoked.unwrap_or(false),
            dirty: response.dirty.unwrap_or(false),
            dirty_window_ids: response.dirty_window_ids.unwrap_or_default(),
            dirty_managed_window_ids: response.dirty_managed_window_ids.unwrap_or_default(),
            dirty_window_node_ids: response.dirty_window_node_ids.unwrap_or_default(),
            dirty_layer_node_ids: response.dirty_layer_node_ids.unwrap_or_default(),
            actions: response.actions.unwrap_or_default(),
            next_poll_in_ms: response.next_poll_in_ms,
            display_config: response.display_config,
            workspace_config: response.workspace_config,
            key_binding_config: response.key_binding_config,
            pointer_config: response.pointer_config,
            input_config: response.input_config,
            event_config: response.event_config,
            process_config: response.process_config,
            process_actions: response.process_actions.unwrap_or_default(),
        })
    }

    fn window_minimize_request(
        &self,
        snapshot: &WaylandWindowSnapshot,
        event: &WindowMinimizeRequestEventSnapshot,
        now_ms: u64,
    ) -> Result<DecorationWindowStateRequestInvocation, DecorationEvaluationError> {
        let mut runtime_guard = self.runtime.lock().map_err(|_| {
            DecorationEvaluationError::RuntimeProtocol("runtime mutex poisoned".into())
        })?;

        let runtime = self.ensure_runtime(&mut runtime_guard)?;
        let request_id = runtime.next_request_id;
        runtime.next_request_id += 1;
        let display_state = self
            .display_state
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default();
        let input_state = self
            .input_state
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default();

        let request = serde_json::to_string(&RuntimeRequest::WindowMinimizeRequest {
            request_id,
            window_id: &snapshot.id,
            snapshot,
            event,
            now_ms,
            display_state: &display_state,
            input_state: &input_state,
        })
        .map_err(|err| DecorationEvaluationError::SnapshotSerialization(err.to_string()))?;
        runtime.write_request(&request)?;

        let response: RuntimeWindowStateRequestResponse =
            if let Some(response) = runtime.read_response()? {
                response
            } else {
                let status = runtime
                    .child
                    .try_wait()?
                    .and_then(|status| status.code())
                    .unwrap_or(-1);
                let stderr = runtime
                    .stderr_log
                    .lock()
                    .map(|stderr| stderr.clone())
                    .unwrap_or_default();
                *runtime_guard = None;
                return Err(DecorationEvaluationError::RuntimeFailed { status, stderr });
            };
        if response.request_id != request_id {
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeProtocol(format!(
                "mismatched response id: expected {request_id}, got {}",
                response.request_id
            )));
        }
        if response.kind != "windowMinimizeRequest" {
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeProtocol(format!(
                "mismatched response kind for windowMinimizeRequest: {}",
                response.kind
            )));
        }
        if !response.ok {
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeProtocol(
                response
                    .error
                    .unwrap_or_else(|| "runtime returned failure".into()),
            ));
        }

        Ok(DecorationWindowStateRequestInvocation {
            invoked: response.invoked.unwrap_or(false),
            dirty: response.dirty.unwrap_or(false),
            dirty_window_ids: response.dirty_window_ids.unwrap_or_default(),
            dirty_managed_window_ids: response.dirty_managed_window_ids.unwrap_or_default(),
            dirty_window_node_ids: response.dirty_window_node_ids.unwrap_or_default(),
            dirty_layer_node_ids: response.dirty_layer_node_ids.unwrap_or_default(),
            actions: response.actions.unwrap_or_default(),
            next_poll_in_ms: response.next_poll_in_ms,
            display_config: response.display_config,
            workspace_config: response.workspace_config,
            key_binding_config: response.key_binding_config,
            pointer_config: response.pointer_config,
            input_config: response.input_config,
            event_config: response.event_config,
            process_config: response.process_config,
            process_actions: response.process_actions.unwrap_or_default(),
        })
    }

    fn window_fullscreen_request(
        &self,
        snapshot: &WaylandWindowSnapshot,
        event: &WindowFullscreenRequestEventSnapshot,
        now_ms: u64,
    ) -> Result<DecorationWindowStateRequestInvocation, DecorationEvaluationError> {
        let mut runtime_guard = self.runtime.lock().map_err(|_| {
            DecorationEvaluationError::RuntimeProtocol("runtime mutex poisoned".into())
        })?;

        let runtime = self.ensure_runtime(&mut runtime_guard)?;
        let request_id = runtime.next_request_id;
        runtime.next_request_id += 1;
        let display_state = self
            .display_state
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default();
        let input_state = self
            .input_state
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default();

        let request = serde_json::to_string(&RuntimeRequest::WindowFullscreenRequest {
            request_id,
            window_id: &snapshot.id,
            snapshot,
            event,
            now_ms,
            display_state: &display_state,
            input_state: &input_state,
        })
        .map_err(|err| DecorationEvaluationError::SnapshotSerialization(err.to_string()))?;
        runtime.write_request(&request)?;

        let response: RuntimeWindowStateRequestResponse =
            if let Some(response) = runtime.read_response()? {
                response
            } else {
                let status = runtime
                    .child
                    .try_wait()?
                    .and_then(|status| status.code())
                    .unwrap_or(-1);
                let stderr = runtime
                    .stderr_log
                    .lock()
                    .map(|stderr| stderr.clone())
                    .unwrap_or_default();
                *runtime_guard = None;
                return Err(DecorationEvaluationError::RuntimeFailed { status, stderr });
            };
        if response.request_id != request_id {
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeProtocol(format!(
                "mismatched response id: expected {request_id}, got {}",
                response.request_id
            )));
        }
        if response.kind != "windowFullscreenRequest" {
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeProtocol(format!(
                "mismatched response kind for windowFullscreenRequest: {}",
                response.kind
            )));
        }
        if !response.ok {
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeProtocol(
                response
                    .error
                    .unwrap_or_else(|| "runtime returned failure".into()),
            ));
        }

        Ok(DecorationWindowStateRequestInvocation {
            invoked: response.invoked.unwrap_or(false),
            dirty: response.dirty.unwrap_or(false),
            dirty_window_ids: response.dirty_window_ids.unwrap_or_default(),
            dirty_managed_window_ids: response.dirty_managed_window_ids.unwrap_or_default(),
            dirty_window_node_ids: response.dirty_window_node_ids.unwrap_or_default(),
            dirty_layer_node_ids: response.dirty_layer_node_ids.unwrap_or_default(),
            actions: response.actions.unwrap_or_default(),
            next_poll_in_ms: response.next_poll_in_ms,
            display_config: response.display_config,
            workspace_config: response.workspace_config,
            key_binding_config: response.key_binding_config,
            pointer_config: response.pointer_config,
            input_config: response.input_config,
            event_config: response.event_config,
            process_config: response.process_config,
            process_actions: response.process_actions.unwrap_or_default(),
        })
    }

    fn window_activate_request(
        &self,
        snapshot: &WaylandWindowSnapshot,
        event: &WindowActivateRequestEventSnapshot,
        now_ms: u64,
    ) -> Result<DecorationWindowStateRequestInvocation, DecorationEvaluationError> {
        let mut runtime_guard = self.runtime.lock().map_err(|_| {
            DecorationEvaluationError::RuntimeProtocol("runtime mutex poisoned".into())
        })?;

        let runtime = self.ensure_runtime(&mut runtime_guard)?;
        let request_id = runtime.next_request_id;
        runtime.next_request_id += 1;
        let display_state = self
            .display_state
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default();
        let input_state = self
            .input_state
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default();

        let request = serde_json::to_string(&RuntimeRequest::WindowActivateRequest {
            request_id,
            window_id: &snapshot.id,
            snapshot,
            event,
            now_ms,
            display_state: &display_state,
            input_state: &input_state,
        })
        .map_err(|err| DecorationEvaluationError::SnapshotSerialization(err.to_string()))?;
        runtime.write_request(&request)?;

        let response: RuntimeWindowStateRequestResponse =
            if let Some(response) = runtime.read_response()? {
                response
            } else {
                let status = runtime
                    .child
                    .try_wait()?
                    .and_then(|status| status.code())
                    .unwrap_or(-1);
                let stderr = runtime
                    .stderr_log
                    .lock()
                    .map(|stderr| stderr.clone())
                    .unwrap_or_default();
                *runtime_guard = None;
                return Err(DecorationEvaluationError::RuntimeFailed { status, stderr });
            };
        if response.request_id != request_id {
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeProtocol(format!(
                "mismatched response id: expected {request_id}, got {}",
                response.request_id
            )));
        }
        if response.kind != "windowActivateRequest" {
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeProtocol(format!(
                "mismatched response kind for windowActivateRequest: {}",
                response.kind
            )));
        }
        if !response.ok {
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeProtocol(
                response
                    .error
                    .unwrap_or_else(|| "runtime returned failure".into()),
            ));
        }

        Ok(DecorationWindowStateRequestInvocation {
            invoked: response.invoked.unwrap_or(false),
            dirty: response.dirty.unwrap_or(false),
            dirty_window_ids: response.dirty_window_ids.unwrap_or_default(),
            dirty_managed_window_ids: response.dirty_managed_window_ids.unwrap_or_default(),
            dirty_window_node_ids: response.dirty_window_node_ids.unwrap_or_default(),
            dirty_layer_node_ids: response.dirty_layer_node_ids.unwrap_or_default(),
            actions: response.actions.unwrap_or_default(),
            next_poll_in_ms: response.next_poll_in_ms,
            display_config: response.display_config,
            workspace_config: response.workspace_config,
            key_binding_config: response.key_binding_config,
            pointer_config: response.pointer_config,
            input_config: response.input_config,
            event_config: response.event_config,
            process_config: response.process_config,
            process_actions: response.process_actions.unwrap_or_default(),
        })
    }

    fn pointer_move(
        &self,
        event: &PointerMoveEventSnapshot,
        now_ms: u64,
    ) -> Result<DecorationPointerMoveAsyncInvocation, DecorationEvaluationError> {
        self.dispatch_pointer_move(event, now_ms)
    }

    fn pointer_move_async(&self, event: PointerMoveEventSnapshot, now_ms: u64) {
        self.enqueue_pointer_move_async(event, now_ms);
    }

    fn gesture_swipe(
        &self,
        event: &GestureSwipeEventSnapshot,
        now_ms: u64,
    ) -> Result<DecorationGestureSwipeAsyncInvocation, DecorationEvaluationError> {
        self.dispatch_gesture_swipe(event, now_ms)
    }

    fn gesture_swipe_async(&self, event: GestureSwipeEventSnapshot, now_ms: u64) {
        self.enqueue_gesture_swipe_async(event, now_ms);
    }

    fn start_close(
        &self,
        window_id: &str,
        now_ms: u64,
    ) -> Result<DecorationHandlerInvocation, DecorationEvaluationError> {
        let mut runtime_guard = self.runtime.lock().map_err(|_| {
            DecorationEvaluationError::RuntimeProtocol("runtime mutex poisoned".into())
        })?;

        let Some(_) = runtime_guard.as_ref() else {
            return Ok(DecorationHandlerInvocation::default());
        };

        let runtime = self.ensure_runtime(&mut runtime_guard)?;
        let request_id = runtime.next_request_id;
        runtime.next_request_id += 1;
        let display_state = self
            .display_state
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default();
        let input_state = self
            .input_state
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default();

        let request = serde_json::to_string(&RuntimeRequest::StartClose {
            request_id,
            window_id,
            now_ms,
            display_state: &display_state,
            input_state: &input_state,
        })
        .map_err(|err| DecorationEvaluationError::SnapshotSerialization(err.to_string()))?;
        runtime.write_request(&request)?;

        let response: RuntimeStartCloseResponse = if let Some(response) = runtime.read_response()? {
            response
        } else {
            let status = runtime
                .child
                .try_wait()?
                .and_then(|status| status.code())
                .unwrap_or(-1);
            let stderr = runtime
                .stderr_log
                .lock()
                .map(|stderr| stderr.clone())
                .unwrap_or_default();
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeFailed { status, stderr });
        };
        if response.request_id != request_id {
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeProtocol(format!(
                "mismatched response id: expected {request_id}, got {}",
                response.request_id
            )));
        }
        if response.kind != "startClose" {
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeProtocol(format!(
                "mismatched response kind for startClose: {}",
                response.kind
            )));
        }
        if !response.ok {
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeProtocol(
                response
                    .error
                    .unwrap_or_else(|| "runtime returned failure".into()),
            ));
        }

        let node = if let Some(serialized) = response.serialized {
            let stdout = serde_json::to_string(&serialized)
                .map_err(|err| DecorationEvaluationError::InvalidResponse(err.to_string()))?;
            Some(decode_tree_json(stdout.trim()).map_err(DecorationEvaluationError::Bridge)?)
        } else {
            None
        };

        Ok(DecorationHandlerInvocation {
            close_animation_duration_ms: response.close_animation_duration_ms,
            invoked: response.invoked.unwrap_or(false),
            node,
            transform: response.transform,
            managed_window: response.managed_window,
            window_effects: take_native_window_effects(runtime, request_id, window_id)?,
            dirty_window_ids: response.dirty_window_ids.unwrap_or_default(),
            dirty_managed_window_ids: response.dirty_managed_window_ids.unwrap_or_default(),
            dirty_window_node_ids: response.dirty_window_node_ids.unwrap_or_default(),
            actions: response.actions.unwrap_or_default(),
            next_poll_in_ms: response.next_poll_in_ms,
            display_config: response.display_config,
            workspace_config: response.workspace_config,
            key_binding_config: response.key_binding_config,
            pointer_config: response.pointer_config,
            input_config: response.input_config,
            event_config: response.event_config,
            process_config: response.process_config,
            process_actions: response.process_actions.unwrap_or_default(),
        })
    }

    fn evaluate_layer_effects(
        &self,
        output_name: &str,
        layers: &[WaylandLayerSnapshot],
        now_ms: u64,
    ) -> Result<LayerEffectEvaluationResult, DecorationEvaluationError> {
        let mut runtime_guard = self.runtime.lock().map_err(|_| {
            DecorationEvaluationError::RuntimeProtocol("runtime mutex poisoned".into())
        })?;
        let runtime = self.ensure_runtime(&mut runtime_guard)?;
        let request_id = runtime.next_request_id;
        runtime.next_request_id += 1;
        let display_state = self
            .display_state
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default();
        let input_state = self
            .input_state
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default();

        runtime.write_effect_request(NativeEffectRequest::EvaluateLayerEffects {
            request_id,
            output_name: output_name.to_owned(),
            layers: layers.to_vec(),
            now_ms,
            display_state,
            input_state,
        })?;

        let response: RuntimeLayerEffectsResponse =
            if let Some(response) = runtime.read_response()? {
                response
            } else {
                let status = runtime
                    .child
                    .try_wait()?
                    .and_then(|status| status.code())
                    .unwrap_or(-1);
                let stderr = runtime
                    .stderr_log
                    .lock()
                    .map(|stderr| stderr.clone())
                    .unwrap_or_default();
                *runtime_guard = None;
                return Err(DecorationEvaluationError::RuntimeFailed { status, stderr });
            };
        if response.request_id != request_id {
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeProtocol(format!(
                "mismatched response id: expected {request_id}, got {}",
                response.request_id
            )));
        }
        if response.kind != "evaluateLayerEffects" {
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeProtocol(format!(
                "mismatched response kind for evaluateLayerEffects: {}",
                response.kind
            )));
        }
        if !response.ok {
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeProtocol(
                response
                    .error
                    .unwrap_or_else(|| "runtime returned failure".into()),
            ));
        }

        let effects = match runtime
            .take_effect_update(request_id)?
            .map(|resolved| resolved.update)
        {
            Some(NativeEffectUpdate::Layers(assignments)) => assignments
                .into_iter()
                .map(|assignment| {
                    Ok(RuntimeLayerEffectAssignment {
                        layer_id: assignment.layer_id,
                        effects: assignment
                            .effects
                            .map(validate_layer_effect_config)
                            .transpose()?,
                    })
                })
                .collect::<Result<Vec<_>, DecorationBridgeError>>()
                .map_err(DecorationEvaluationError::Bridge)?,
            Some(_) => {
                return Err(DecorationEvaluationError::RuntimeProtocol(
                    "mismatched native layer effect update".into(),
                ));
            }
            None => {
                return Err(DecorationEvaluationError::RuntimeProtocol(
                    "missing native layer effect update".into(),
                ));
            }
        };

        Ok(LayerEffectEvaluationResult {
            effects,
            next_poll_in_ms: response.next_poll_in_ms,
            display_config: response.display_config,
            workspace_config: response.workspace_config,
            key_binding_config: response.key_binding_config,
            pointer_config: response.pointer_config,
            input_config: response.input_config,
            event_config: response.event_config,
            process_config: response.process_config,
            process_actions: response.process_actions.unwrap_or_default(),
        })
    }

    fn evaluate_popup_effects(
        &self,
        output_name: &str,
        popups: &[WaylandPopupSnapshot],
        now_ms: u64,
    ) -> Result<PopupEffectEvaluationResult, DecorationEvaluationError> {
        let mut runtime_guard = self.runtime.lock().map_err(|_| {
            DecorationEvaluationError::RuntimeProtocol("runtime mutex poisoned".into())
        })?;
        let runtime = self.ensure_runtime(&mut runtime_guard)?;
        let request_id = runtime.next_request_id;
        runtime.next_request_id += 1;
        let display_state = self
            .display_state
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default();
        let input_state = self
            .input_state
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default();

        runtime.write_effect_request(NativeEffectRequest::EvaluatePopupEffects {
            request_id,
            output_name: output_name.to_owned(),
            popups: popups.to_vec(),
            now_ms,
            display_state,
            input_state,
        })?;

        let response: RuntimePopupEffectsResponse =
            if let Some(response) = runtime.read_response()? {
                response
            } else {
                let status = runtime
                    .child
                    .try_wait()?
                    .and_then(|status| status.code())
                    .unwrap_or(-1);
                let stderr = runtime
                    .stderr_log
                    .lock()
                    .map(|stderr| stderr.clone())
                    .unwrap_or_default();
                *runtime_guard = None;
                return Err(DecorationEvaluationError::RuntimeFailed { status, stderr });
            };
        if response.request_id != request_id {
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeProtocol(format!(
                "mismatched response id: expected {request_id}, got {}",
                response.request_id
            )));
        }
        if response.kind != "evaluatePopupEffects" {
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeProtocol(format!(
                "mismatched response kind for evaluatePopupEffects: {}",
                response.kind
            )));
        }
        if !response.ok {
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeProtocol(
                response
                    .error
                    .unwrap_or_else(|| "runtime returned failure".into()),
            ));
        }

        let effects = match runtime
            .take_effect_update(request_id)?
            .map(|resolved| resolved.update)
        {
            Some(NativeEffectUpdate::Popups(assignments)) => assignments
                .into_iter()
                .map(|assignment| {
                    Ok(RuntimePopupEffectAssignment {
                        popup_id: assignment.popup_id,
                        effects: assignment
                            .effects
                            .map(validate_popup_effect_config)
                            .transpose()?,
                        surface_policy: assignment.surface_policy,
                    })
                })
                .collect::<Result<Vec<_>, DecorationBridgeError>>()
                .map_err(DecorationEvaluationError::Bridge)?,
            Some(_) => {
                return Err(DecorationEvaluationError::RuntimeProtocol(
                    "mismatched native popup effect update".into(),
                ));
            }
            None => {
                return Err(DecorationEvaluationError::RuntimeProtocol(
                    "missing native popup effect update".into(),
                ));
            }
        };

        Ok(PopupEffectEvaluationResult {
            effects,
            next_poll_in_ms: response.next_poll_in_ms,
            display_config: response.display_config,
            workspace_config: response.workspace_config,
            key_binding_config: response.key_binding_config,
            pointer_config: response.pointer_config,
            input_config: response.input_config,
            event_config: response.event_config,
            process_config: response.process_config,
            process_actions: response.process_actions.unwrap_or_default(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ssd::{
        BackdropBlur, CompiledEffect, DecorationNodeKind, EffectAlphaMode,
        EffectInvalidationPolicy, EffectOutsets, EffectStage, ShaderModule, ShaderStage,
        WindowEffectSlot, WindowSourceInclude,
        window_model::{WaylandWindowSnapshot, WindowPositionSnapshot},
    };
    use std::{
        io::{BufRead, BufReader, Write},
        os::unix::net::UnixStream,
    };

    fn make_window(is_focused: bool) -> WaylandWindowSnapshot {
        WaylandWindowSnapshot {
            id: "1".into(),
            title: "Kitty".into(),
            app_id: Some("kitty".into()),
            position: WindowPositionSnapshot {
                x: 0.0,
                y: 0.0,
                width: 800.0,
                height: 600.0,
            },
            rect: WindowPositionSnapshot {
                x: 0.0,
                y: 0.0,
                width: 800.0,
                height: 600.0,
            },
            is_focused,
            is_floating: true,
            is_maximized: false,
            is_fullscreen: false,
            is_xwayland: false,
            decoration: Default::default(),
            size_constraints: Default::default(),
            is_resizable: true,
            is_transient: false,
            parent_id: None,
            icon: None,
            interaction: crate::ssd::DecorationInteractionSnapshot::default(),
        }
    }

    #[test]
    fn evaluator_reflects_title_into_tree() {
        let tree = evaluate_dynamic_decoration(&StaticDecorationEvaluator, &make_window(false), 0)
            .expect("evaluation should succeed");

        let title_node = &tree.root.children[0].children[0].children[0];
        assert!(
            matches!(&title_node.kind, DecorationNodeKind::Label(label) if label.text == "Kitty")
        );
    }

    #[test]
    fn evaluator_changes_border_color_for_focused_window() {
        let focused =
            evaluate_dynamic_decoration(&StaticDecorationEvaluator, &make_window(true), 0)
                .expect("focused evaluation should succeed");
        let unfocused =
            evaluate_dynamic_decoration(&StaticDecorationEvaluator, &make_window(false), 0)
                .expect("unfocused evaluation should succeed");

        assert_ne!(focused.root.style.border, unfocused.root.style.border);
    }

    #[test]
    fn embedded_runtime_loads_tsx_config() {
        let repository_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .expect("repository root should exist");
        let test_dir =
            std::env::temp_dir().join(format!("shojiwm deno runtime #?-{}", std::process::id()));
        std::fs::create_dir_all(&test_dir).expect("test directory should be created");
        let config_path = test_dir.join("config.tsx");
        std::fs::write(
            &config_path,
            r#"
import { Box, COMPOSITOR } from "shoji_wm";

COMPOSITOR.window.composition = () => <Box />;
"#,
        )
        .expect("test config should be written");

        let evaluator = EmbeddedDecorationEvaluator::for_paths(
            repository_root.join("tools/decoration-runtime.ts"),
            &config_path,
        )
        .with_working_dir(&repository_root);
        let result = evaluator.lifecycle_enable("test", None);
        drop(evaluator);
        let _ = std::fs::remove_dir_all(&test_dir);

        result.expect("embedded runtime should load and evaluate a TSX config");
    }

    #[test]
    fn embedded_runtime_dispatches_interactions_through_native_bridge() {
        use crate::ssd::window_model::{
            GestureSwipeEventSnapshot, GestureSwipePhaseSnapshot, PointerHitTargetSnapshot,
            PointerModifierStateSnapshot, PointerMoveEventSnapshot, PointerMovePointSnapshot,
            WindowMoveEventSnapshot, WindowMovePhaseSnapshot, WindowMoveSourceSnapshot,
            WindowResizeEdgesSnapshot, WindowResizeEventSnapshot, WindowResizePhaseSnapshot,
            WindowResizePointSnapshot, WindowResizeSourceSnapshot,
        };

        let repository_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .expect("repository root should exist");
        let test_dir = std::env::temp_dir().join(format!(
            "shojiwm-deno-native-interactions-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&test_dir).expect("test directory should be created");
        let config_path = test_dir.join("config.tsx");
        std::fs::write(
            &config_path,
            r#"
import { Box, COMPOSITOR } from "shoji_wm";

COMPOSITOR.window.composition = () => <Box />;
COMPOSITOR.event.onPointerMove(() => {});
COMPOSITOR.event.onPointerMoveAsync(() => {});
COMPOSITOR.event.onGestureSwipe(() => {});
COMPOSITOR.event.onGestureSwipeAsync(() => {});
COMPOSITOR.event.onWindowMove(() => {});
COMPOSITOR.event.onWindowResize(() => {});
"#,
        )
        .expect("test config should be written");

        let evaluator = EmbeddedDecorationEvaluator::for_paths(
            repository_root.join("tools/decoration-runtime.ts"),
            &config_path,
        )
        .with_working_dir(&repository_root);
        let lifecycle = evaluator
            .lifecycle_enable("test", None)
            .expect("runtime should load interaction listeners");
        let event_config = lifecycle
            .event_config
            .expect("runtime should publish interaction listener configuration");
        assert!(event_config.pointer_move);
        assert!(event_config.pointer_move_async);
        assert!(event_config.gesture_swipe);
        assert!(event_config.gesture_swipe_async);

        let pointer = PointerMoveEventSnapshot {
            position: PointerMovePointSnapshot { x: 10.0, y: 20.0 },
            delta: PointerMovePointSnapshot { x: 1.0, y: -1.0 },
            target: PointerHitTargetSnapshot::None,
            output_name: Some("output-1".into()),
            modifiers: PointerModifierStateSnapshot {
                logo: false,
                alt: false,
                ctrl: false,
                shift: false,
            },
            timestamp: 1,
        };
        assert!(
            evaluator
                .pointer_move(&pointer, 1)
                .expect("native pointer event should complete")
                .invoked
        );
        assert!(
            evaluator
                .dispatch_pointer_move_async(&pointer, 1)
                .expect("native async pointer event should complete")
                .expect("runtime should be available")
                .invoked
        );

        let gesture = GestureSwipeEventSnapshot {
            phase: GestureSwipePhaseSnapshot::Update,
            fingers: 3,
            position: Some(pointer.position),
            delta_x: 2.0,
            delta_y: 3.0,
            total_x: 4.0,
            total_y: 5.0,
            velocity_x: 6.0,
            velocity_y: 7.0,
            output_name: Some("output-1".into()),
            device: None,
            timestamp: 2,
        };
        assert!(
            evaluator
                .gesture_swipe(&gesture, 2)
                .expect("native gesture event should complete")
                .invoked
        );
        assert!(
            evaluator
                .dispatch_gesture_swipe_async(&gesture, 2)
                .expect("native async gesture event should complete")
                .expect("runtime should be available")
                .invoked
        );

        let window = make_window(false);
        evaluator
            .evaluate_window(&window, 3)
            .expect("window cache should be initialized");
        let point = WindowResizePointSnapshot { x: 10.0, y: 20.0 };
        let modifiers = PointerModifierStateSnapshot {
            logo: true,
            alt: false,
            ctrl: false,
            shift: false,
        };
        let move_event = WindowMoveEventSnapshot {
            source: WindowMoveSourceSnapshot::Modifier,
            phase: WindowMovePhaseSnapshot::Update,
            start_pointer: point,
            current_pointer: WindowResizePointSnapshot { x: 30.0, y: 40.0 },
            delta: WindowResizePointSnapshot { x: 20.0, y: 20.0 },
            start_rect: window.rect,
            current_rect: window.rect,
            output_name: Some("output-1".into()),
            modifiers,
            timestamp: 3,
        };
        assert!(
            evaluator
                .window_move(&window.id, &move_event, 3)
                .expect("native window move should complete")
                .invoked
        );

        let resize_event = WindowResizeEventSnapshot {
            source: WindowResizeSourceSnapshot::Modifier,
            phase: WindowResizePhaseSnapshot::Update,
            edges: WindowResizeEdgesSnapshot {
                left: false,
                right: true,
                top: false,
                bottom: true,
            },
            start_pointer: point,
            current_pointer: WindowResizePointSnapshot { x: 30.0, y: 40.0 },
            delta: WindowResizePointSnapshot { x: 20.0, y: 20.0 },
            start_rect: window.rect,
            current_rect: window.rect,
            output_name: Some("output-1".into()),
            timestamp: 4,
        };
        assert!(
            evaluator
                .window_resize(&window.id, &resize_event, 4)
                .expect("native window resize should complete")
                .invoked
        );

        drop(evaluator);
        let _ = std::fs::remove_dir_all(&test_dir);
    }

    #[test]
    fn embedded_runtime_transfers_all_effect_configs_through_native_bridge() {
        let repository_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .expect("repository root should exist");
        let test_dir = std::env::temp_dir().join(format!(
            "shojiwm-deno-native-effects-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&test_dir).expect("test directory should be created");
        let config_path = test_dir.join("config.tsx");
        std::fs::write(
            &config_path,
            r#"
import {
  backdropSource,
  Box,
  compileEffect,
  compileLayerEffect,
  compilePopupEffect,
  compileWindowEffect,
  COMPOSITOR,
  layerSource,
  noise,
  popupSource,
  windowSource,
} from "shoji_wm";

COMPOSITOR.window.composition = () => <Box />;
COMPOSITOR.effect.background_effect = compileEffect({
  input: backdropSource(),
  pipeline: [noise()],
});
COMPOSITOR.effect.window = () => ({
  behind: compileWindowEffect({
    input: windowSource(),
    pipeline: [noise()],
  }),
});
COMPOSITOR.effect.layer = () => ({
  replace: compileLayerEffect({
    input: layerSource(),
    pipeline: [noise()],
  }),
});
COMPOSITOR.effect.popup = () => ({
  inFront: compilePopupEffect({
    input: popupSource(),
    pipeline: [noise()],
  }),
});
COMPOSITOR.rendering.surfacePolicy = () => ({ opaqueRegion: "ignore" });
"#,
        )
        .expect("test config should be written");

        let evaluator = EmbeddedDecorationEvaluator::for_paths(
            repository_root.join("tools/decoration-runtime.ts"),
            &config_path,
        )
        .with_working_dir(&test_dir);

        let background = evaluator
            .background_effect_config()
            .expect("native background effect should evaluate")
            .expect("background effect should be present");
        assert!(matches!(background.effect.input, EffectInput::Backdrop));

        let window = evaluator
            .evaluate_window(&make_window(false), 0)
            .expect("native window effect should evaluate");
        assert!(window.window_effects.is_some_and(|effects| {
            effects
                .behind
                .is_some_and(|slot| matches!(slot.effect.input, EffectInput::WindowSource(_)))
        }));
        let cached_window = evaluator
            .evaluate_cached_window(&make_window(false).id, None, 16, false)
            .expect("native cached response should support a non-null window effect");
        assert!(cached_window.window_effects.is_some_and(|effects| {
            effects
                .behind
                .is_some_and(|slot| matches!(slot.effect.input, EffectInput::WindowSource(_)))
        }));

        let layer = WaylandLayerSnapshot {
            id: "layer-1".into(),
            namespace: Some("test".into()),
            layer: crate::ssd::window_model::LayerKindSnapshot::Top,
            output_name: "output-1".into(),
            position: crate::ssd::window_model::LayerPositionSnapshot {
                x: 0,
                y: 0,
                width: 800,
                height: 32,
            },
            anchor: crate::ssd::window_model::LayerAnchorSnapshot {
                top: true,
                bottom: false,
                left: true,
                right: true,
            },
            exclusive_zone: crate::ssd::window_model::LayerExclusiveZoneSnapshot::Exclusive {
                size: 32,
            },
            exclusive_edge: Some(crate::ssd::window_model::LayerEdgeSnapshot::Top),
            margin: crate::ssd::window_model::LayerMarginSnapshot::default(),
            keyboard_interactivity: crate::ssd::window_model::KeyboardInteractivitySnapshot::None,
            desired_size: crate::ssd::window_model::LayerDesiredSizeSnapshot {
                width: 800,
                height: 32,
            },
        };
        let layers = evaluator
            .evaluate_layer_effects("output-1", &[layer], 0)
            .expect("native layer effect should evaluate");
        assert!(layers.effects.first().is_some_and(|assignment| {
            assignment.effects.as_ref().is_some_and(|effects| {
                effects
                    .replace
                    .as_ref()
                    .is_some_and(|slot| matches!(slot.effect.input, EffectInput::LayerSource(_)))
            })
        }));

        let popup = WaylandPopupSnapshot {
            id: "popup-1".into(),
            parent_id: "layer-1".into(),
            parent_kind: crate::ssd::window_model::PopupParentKindSnapshot::Layer,
            output_name: "output-1".into(),
            position: crate::ssd::window_model::LayerPositionSnapshot {
                x: 10,
                y: 10,
                width: 200,
                height: 100,
            },
        };
        let popups = evaluator
            .evaluate_popup_effects("output-1", &[popup], 0)
            .expect("native popup effect should evaluate");
        assert!(popups.effects.first().is_some_and(|assignment| {
            assignment.effects.as_ref().is_some_and(|effects| {
                effects
                    .in_front
                    .as_ref()
                    .is_some_and(|slot| matches!(slot.effect.input, EffectInput::PopupSource(_)))
            }) && assignment.surface_policy.is_some_and(|policy| {
                policy.opaque_region == crate::ssd::OpaqueRegionPolicy::Ignore
            })
        }));

        drop(evaluator);
        let _ = std::fs::remove_dir_all(&test_dir);
    }

    #[test]
    fn embedded_runtime_preloads_default_config() {
        let repository_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .expect("repository root should exist");
        let evaluator = EmbeddedDecorationEvaluator::for_paths(
            repository_root.join("tools/decoration-runtime.ts"),
            repository_root.join("packages/config/src/index.tsx"),
        )
        .with_working_dir(&repository_root);

        evaluator
            .preload()
            .expect("embedded runtime should preload the default config");
    }

    fn make_named_window(
        id: &str,
        app_id: &str,
        is_focused: bool,
        is_maximized: bool,
    ) -> WaylandWindowSnapshot {
        let mut snapshot = make_window(is_focused);
        snapshot.id = id.to_string();
        snapshot.title = format!("{app_id} window");
        snapshot.app_id = Some(app_id.to_string());
        snapshot.is_maximized = is_maximized;
        snapshot
    }

    fn real_config_evaluator() -> EmbeddedDecorationEvaluator {
        let repository_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .expect("repository root should exist");
        EmbeddedDecorationEvaluator::for_paths(
            repository_root.join("tools/decoration-runtime.ts"),
            repository_root.join("packages/config/src/index.tsx"),
        )
        .with_working_dir(&repository_root)
    }

    fn tiled_workspace_persisted_state() -> serde_json::Value {
        serde_json::json!({
            "config.hybrid-window-manager": {
                "currentMonitor": "TEST-1",
                "activeWorkspaceByMonitor": [["TEST-1", 1]],
                "workspaces": [{
                    "monitor": "TEST-1",
                    "index": 1,
                    "isTiled": true,
                    "activeWindowId": null,
                    "scrollOffset": 0,
                    "windows": [],
                }],
            },
        })
    }

    fn launch_scenario_z_indices(second_window_maximized: bool, tiled: bool) -> (i32, i32) {
        let evaluator = real_config_evaluator();
        let mut display_state = std::collections::BTreeMap::new();
        display_state.insert("TEST-1".to_string(), test_output_snapshot("TEST-1"));
        evaluator.set_display_state(display_state);
        if tiled {
            evaluator
                .lifecycle_enable("reload", Some(&tiled_workspace_persisted_state()))
                .expect("tiled lifecycle should succeed");
        } else {
            evaluator
                .lifecycle_enable("initial", None)
                .expect("initial lifecycle should succeed");
        }

        // First app opens unmaximized and takes focus.
        let editor = make_named_window("0xa", "org.gnome.TextEditor", false, false);
        evaluator
            .evaluate_window_preview(&editor, 0)
            .expect("editor preview should evaluate");
        let editor_focused = make_named_window("0xa", "org.gnome.TextEditor", true, false);
        evaluator
            .evaluate_window(&editor_focused, 100)
            .expect("editor evaluation should succeed");

        // Second app launches; when maximized it sends set_maximized before its
        // window joins any workspace (observed live: the maximize request is
        // dispatched before hybrid-initial-configure).
        let chrome = make_named_window("0xb", "google-chrome", false, second_window_maximized);
        if second_window_maximized {
            evaluator
                .window_maximize_request(
                    &chrome,
                    &crate::ssd::WindowMaximizeRequestEventSnapshot {
                        maximized: true,
                        source: crate::ssd::WindowStateRequestSourceSnapshot::ClientCsd,
                        timestamp: 150,
                    },
                    150,
                )
                .expect("maximize request should evaluate");
        }
        evaluator
            .evaluate_window_preview(&chrome, 200)
            .expect("chrome preview should evaluate");
        let chrome_focused =
            make_named_window("0xb", "google-chrome", true, second_window_maximized);
        let chrome_result = evaluator
            .evaluate_window(&chrome_focused, 300)
            .expect("chrome evaluation should succeed");

        let editor_unfocused = make_named_window("0xa", "org.gnome.TextEditor", false, false);
        let editor_result = evaluator
            .evaluate_window(&editor_unfocused, 400)
            .expect("editor re-evaluation should succeed");

        let chrome_z = chrome_result
            .managed_window
            .z_index
            .expect("chrome should have a z index");
        let editor_z = editor_result
            .managed_window
            .z_index
            .expect("editor should have a z index");
        (editor_z, chrome_z)
    }

    #[test]
    fn plain_second_window_launches_above_existing_window() {
        let (editor_z, chrome_z) = launch_scenario_z_indices(false, false);
        assert!(
            chrome_z > editor_z,
            "second (plain) window should stack above: editor={editor_z} chrome={chrome_z}"
        );
    }

    #[test]
    fn maximized_second_window_launches_above_existing_window() {
        let (editor_z, chrome_z) = launch_scenario_z_indices(true, false);
        assert!(
            chrome_z > editor_z,
            "second (maximized) window should stack above: editor={editor_z} chrome={chrome_z}"
        );
    }

    fn activate_toggle_fixture(
        focused_at_activate: bool,
        source: crate::ssd::WindowActivateRequestSourceSnapshot,
    ) -> Vec<RuntimeWindowAction> {
        let evaluator = real_config_evaluator();
        let mut display_state = std::collections::BTreeMap::new();
        display_state.insert("TEST-1".to_string(), test_output_snapshot("TEST-1"));
        evaluator.set_display_state(display_state);
        evaluator
            .lifecycle_enable("initial", None)
            .expect("initial lifecycle should succeed");

        let window = make_named_window("0xa", "kitty-float", false, false);
        evaluator
            .evaluate_window_preview(&window, 0)
            .expect("preview should evaluate");
        let focused = make_named_window("0xa", "kitty-float", true, false);
        evaluator
            .evaluate_window(&focused, 100)
            .expect("evaluation should succeed");
        let at_activate = make_named_window("0xa", "kitty-float", focused_at_activate, false);
        if !focused_at_activate {
            evaluator
                .evaluate_window(&at_activate, 150)
                .expect("defocus evaluation should succeed");
        }

        let invocation = evaluator
            .window_activate_request(
                &at_activate,
                &crate::ssd::WindowActivateRequestEventSnapshot {
                    source,
                    timestamp: 200,
                },
                200,
            )
            .expect("activate request should evaluate");
        invocation.actions
    }

    fn has_action(
        actions: &[RuntimeWindowAction],
        window_id: &str,
        expected: crate::ssd::WaylandWindowAction,
    ) -> bool {
        actions
            .iter()
            .any(|action| action.window_id == window_id && action.action == expected)
    }

    #[test]
    fn reactivating_focused_floating_window_requests_minimize() {
        let actions = activate_toggle_fixture(
            true,
            crate::ssd::WindowActivateRequestSourceSnapshot::Api,
        );
        assert!(
            has_action(&actions, "0xa", crate::ssd::WaylandWindowAction::Minimize),
            "dock activation of the focused floating window should minimize it: {actions:?}"
        );
    }

    /// A dock that focuses the app it just launched (noctalia's dock arms a
    /// pending-launch-focus and activates as soon as a matching toplevel
    /// appears) sends `activate` before the client has committed a single
    /// buffer — the foreign-toplevel handle exists from `xdg_toplevel`
    /// creation. Only preview evaluations have run at that point, so the
    /// window is focused but has never presented. Treating that hand-off as a
    /// taskbar re-click minimized apps straight into the taskbar on launch
    /// (issue #68), visible only for maximized windows because those skip the
    /// deferred initial layout and so already belong to a workspace.
    #[test]
    fn launch_focus_activation_before_first_commit_does_not_minimize() {
        let evaluator = real_config_evaluator();
        let mut display_state = std::collections::BTreeMap::new();
        display_state.insert("TEST-1".to_string(), test_output_snapshot("TEST-1"));
        evaluator.set_display_state(display_state);
        evaluator
            .lifecycle_enable("initial", None)
            .expect("initial lifecycle should succeed");

        // Maximized launch: preview evaluations only — no `evaluate_window`,
        // so `onFirstCommit` has not fired yet.
        let opening = make_named_window("0xb", "google-chrome", false, true);
        evaluator
            .evaluate_window_preview(&opening, 0)
            .expect("preview should evaluate");
        let opening_focused = make_named_window("0xb", "google-chrome", true, true);
        evaluator
            .evaluate_window_preview(&opening_focused, 100)
            .expect("focused preview should evaluate");

        let actions = evaluator
            .window_activate_request(
                &opening_focused,
                &crate::ssd::WindowActivateRequestEventSnapshot {
                    source: crate::ssd::WindowActivateRequestSourceSnapshot::Api,
                    timestamp: 150,
                },
                150,
            )
            .expect("activate request should evaluate")
            .actions;

        assert!(
            !has_action(&actions, "0xb", crate::ssd::WaylandWindowAction::Minimize),
            "a dock focusing the window it just launched must not minimize it: {actions:?}"
        );
    }

    #[test]
    fn activating_unfocused_floating_window_focuses_it() {
        let actions = activate_toggle_fixture(
            false,
            crate::ssd::WindowActivateRequestSourceSnapshot::Api,
        );
        assert!(
            !has_action(&actions, "0xa", crate::ssd::WaylandWindowAction::Minimize),
            "activating an unfocused window must not minimize it: {actions:?}"
        );
        assert!(
            has_action(&actions, "0xa", crate::ssd::WaylandWindowAction::Focus),
            "activating an unfocused window should focus it: {actions:?}"
        );
    }

    #[test]
    fn reactivating_toggled_minimized_window_restores_it() {
        // The noctalia regression: minimize via the dock toggle, then click
        // the icon again while the (hidden) window still holds keyboard
        // focus. The second activation must restore the window, not bounce
        // it back into minimized.
        let evaluator = real_config_evaluator();
        let mut display_state = std::collections::BTreeMap::new();
        display_state.insert("TEST-1".to_string(), test_output_snapshot("TEST-1"));
        evaluator.set_display_state(display_state);
        evaluator
            .lifecycle_enable("initial", None)
            .expect("initial lifecycle should succeed");

        let window = make_named_window("0xa", "kitty-float", false, false);
        evaluator
            .evaluate_window_preview(&window, 0)
            .expect("preview should evaluate");
        let focused = make_named_window("0xa", "kitty-float", true, false);
        evaluator
            .evaluate_window(&focused, 100)
            .expect("evaluation should succeed");

        let event = crate::ssd::WindowActivateRequestEventSnapshot {
            source: crate::ssd::WindowActivateRequestSourceSnapshot::Api,
            timestamp: 200,
        };
        let first = evaluator
            .window_activate_request(&focused, &event, 200)
            .expect("first activate should evaluate");
        assert!(
            has_action(&first.actions, "0xa", crate::ssd::WaylandWindowAction::Minimize),
            "first activation should toggle the focused window into minimize: {:?}",
            first.actions
        );

        // Mirror `apply_runtime_window_actions`: the queued `window.minimize()`
        // action round-trips through Rust as a minimize request, which is what
        // flips WINDOW_STATE_MINIMIZED on the TS side.
        evaluator
            .window_minimize_request(
                &focused,
                &crate::ssd::WindowMinimizeRequestEventSnapshot {
                    minimized: true,
                    source: crate::ssd::WindowStateRequestSourceSnapshot::Api,
                    timestamp: 250,
                },
                250,
            )
            .expect("minimize request should evaluate");

        // No focus change is delivered in between — the focused snapshot is
        // intentionally stale, mirroring the live race.
        let second = evaluator
            .window_activate_request(&focused, &event, 300)
            .expect("second activate should evaluate");
        assert!(
            !has_action(&second.actions, "0xa", crate::ssd::WaylandWindowAction::Minimize),
            "re-activating the minimized window must not re-minimize it: {:?}",
            second.actions
        );
        assert!(
            has_action(&second.actions, "0xa", crate::ssd::WaylandWindowAction::Focus),
            "re-activating the minimized window should restore and focus it: {:?}",
            second.actions
        );
    }

    /// The sfwbar regression: its taskbar click sends `unset_minimized` and
    /// `activate` as separate requests in one flush. The unset_minimized
    /// restores the window before the activate handler runs, so `wasMinimized`
    /// no longer shields the minimize-raise toggle — and since focus never
    /// leaves a minimized window, the toggle read the activate as a re-click
    /// of a visible focused window and bounced it straight back into
    /// minimized (a one-frame flash). A restore and an activate this close
    /// together are one gesture and must never toggle.
    #[test]
    fn restore_then_activate_in_one_gesture_does_not_reminimize() {
        let evaluator = real_config_evaluator();
        let mut display_state = std::collections::BTreeMap::new();
        display_state.insert("TEST-1".to_string(), test_output_snapshot("TEST-1"));
        evaluator.set_display_state(display_state);
        evaluator
            .lifecycle_enable("initial", None)
            .expect("initial lifecycle should succeed");

        let window = make_named_window("0xa", "kitty-float", false, false);
        evaluator
            .evaluate_window_preview(&window, 0)
            .expect("preview should evaluate");
        let focused = make_named_window("0xa", "kitty-float", true, false);
        evaluator
            .evaluate_window(&focused, 100)
            .expect("evaluation should succeed");

        // Minimize from the taskbar; the window keeps keyboard focus.
        evaluator
            .window_minimize_request(
                &focused,
                &crate::ssd::WindowMinimizeRequestEventSnapshot {
                    minimized: true,
                    source: crate::ssd::WindowStateRequestSourceSnapshot::Api,
                    timestamp: 200,
                },
                200,
            )
            .expect("minimize request should evaluate");

        // sfwbar's click: unset_minimized (twice, in fact), then activate.
        for timestamp in [300, 301] {
            evaluator
                .window_minimize_request(
                    &focused,
                    &crate::ssd::WindowMinimizeRequestEventSnapshot {
                        minimized: false,
                        source: crate::ssd::WindowStateRequestSourceSnapshot::Api,
                        timestamp,
                    },
                    timestamp,
                )
                .expect("restore request should evaluate");
        }
        let activate = evaluator
            .window_activate_request(
                &focused,
                &crate::ssd::WindowActivateRequestEventSnapshot {
                    source: crate::ssd::WindowActivateRequestSourceSnapshot::Api,
                    timestamp: 302,
                },
                302,
            )
            .expect("activate request should evaluate");

        assert!(
            !has_action(
                &activate.actions,
                "0xa",
                crate::ssd::WaylandWindowAction::Minimize
            ),
            "a restore+activate taskbar click must not re-minimize the window: {:?}",
            activate.actions
        );
        assert!(
            has_action(
                &activate.actions,
                "0xa",
                crate::ssd::WaylandWindowAction::Focus
            ),
            "the restored window should be focused: {:?}",
            activate.actions
        );
    }

    /// Super+Left/Right on a tiled workspace: when the focused tile sticks out
    /// of the viewport on the side the key is heading, the press pans the tile
    /// fully into view; only a fully-visible tile advances focus to the
    /// neighbor. Repro: resize the middle of three tiles wider than the
    /// screen — it ends left-aligned (the resize-end `scrollToWindow` flips a
    /// wider-than-viewport tile to its left edge), overflowing to the right —
    /// then press right: the old behavior jumped straight to the neighbor.
    #[test]
    fn focus_key_pans_overflowing_tile_into_view_before_advancing() {
        use crate::ssd::window_model::{
            WindowResizeEdgesSnapshot, WindowResizeEventSnapshot, WindowResizePhaseSnapshot,
            WindowResizePointSnapshot, WindowResizeSourceSnapshot,
        };

        let evaluator = real_config_evaluator();
        let mut display_state = std::collections::BTreeMap::new();
        display_state.insert("TEST-1".to_string(), test_output_snapshot("TEST-1"));
        evaluator.set_display_state(display_state);
        evaluator
            .lifecycle_enable("reload", Some(&tiled_workspace_persisted_state()))
            .expect("tiled lifecycle should succeed");

        // Open 0xa → 0xb → 0xc, delivering the unfocus of the previous window
        // before the next one takes focus — new tiles insert after the focused
        // window, so stale focus snapshots would scramble the tile order.
        let mut now = 0;
        let mut previous: Option<&str> = None;
        for id in ["0xa", "0xb", "0xc"] {
            let window = make_named_window(id, "kitty", false, false);
            evaluator
                .evaluate_window_preview(&window, now)
                .expect("preview should evaluate");
            if let Some(previous) = previous {
                let unfocused = make_named_window(previous, "kitty", false, false);
                evaluator
                    .evaluate_window(&unfocused, now + 25)
                    .expect("defocus evaluation should succeed");
            }
            let focused = make_named_window(id, "kitty", true, false);
            evaluator
                .evaluate_window(&focused, now + 50)
                .expect("evaluation should succeed");
            previous = Some(id);
            now += 100;
        }
        // Focus the middle tile.
        let unfocused_c = make_named_window("0xc", "kitty", false, false);
        evaluator
            .evaluate_window(&unfocused_c, now)
            .expect("defocus evaluation should succeed");
        let focused_b = make_named_window("0xb", "kitty", true, false);
        evaluator
            .evaluate_window(&focused_b, now + 50)
            .expect("focus evaluation should succeed");
        now += 100;

        // Interactively resize the middle tile wider than the 1920px viewport.
        // `resizeTile` right-aligns the tile afterwards, so it overflows the
        // viewport on the left.
        let rect = |width: f64| crate::ssd::window_model::WindowPositionSnapshot {
            x: 0.0,
            y: 0.0,
            width,
            height: 600.0,
        };
        for (phase, width) in [
            (WindowResizePhaseSnapshot::Start, 800.0),
            (WindowResizePhaseSnapshot::Update, 2400.0),
            (WindowResizePhaseSnapshot::End, 2400.0),
        ] {
            let resize = WindowResizeEventSnapshot {
                source: WindowResizeSourceSnapshot::Ssd,
                phase,
                edges: WindowResizeEdgesSnapshot {
                    left: false,
                    right: true,
                    top: false,
                    bottom: false,
                },
                start_pointer: WindowResizePointSnapshot { x: 800.0, y: 300.0 },
                current_pointer: WindowResizePointSnapshot {
                    x: width,
                    y: 300.0,
                },
                delta: WindowResizePointSnapshot {
                    x: width - 800.0,
                    y: 0.0,
                },
                start_rect: rect(800.0),
                current_rect: rect(width),
                output_name: Some("TEST-1".into()),
                timestamp: now,
            };
            evaluator
                .window_resize("0xb", &resize, now)
                .expect("resize should evaluate");
            now += 10;
        }

        // First press: the tile overflows right, so the key pans it into view
        // and focus must stay on the same window.
        let first = evaluator
            .invoke_key_binding("tile-focus-right-quick", now)
            .expect("first focus-right should evaluate");
        assert!(
            first.invoked,
            "tile-focus-right-quick should be a known binding"
        );
        assert!(
            !has_action(&first.actions, "0xc", crate::ssd::WaylandWindowAction::Focus),
            "an overflowing tile must be panned into view, not skipped: {:?}",
            first.actions
        );
        assert!(
            has_action(&first.actions, "0xb", crate::ssd::WaylandWindowAction::Focus),
            "the overflowing tile should keep focus while panning: {:?}",
            first.actions
        );

        // Second press: the tile's right edge is now flush with the viewport,
        // so focus advances to the neighbor.
        let second = evaluator
            .invoke_key_binding("tile-focus-right-quick", now + 100)
            .expect("second focus-right should evaluate");
        assert!(
            has_action(&second.actions, "0xc", crate::ssd::WaylandWindowAction::Focus),
            "a fully-visible tile should advance focus to the neighbor: {:?}",
            second.actions
        );
    }

    /// Maximized tiles are wider than the inset tile viewport by design
    /// (MAXIMIZED_WINDOW_PADDING 8 < TILE_MARGIN 12), so when centered they
    /// poke 4px past the viewport on both sides while being fully on screen.
    /// Measuring the focus-key overflow against the inset viewport burned the
    /// first key press on that invisible 4px pan — every focus move between
    /// maximized tiles needed two presses. Fully-visible tiles must advance
    /// on the first press.
    #[test]
    fn focus_key_advances_from_fully_visible_maximized_tile_on_first_press() {
        let evaluator = real_config_evaluator();
        let mut display_state = std::collections::BTreeMap::new();
        display_state.insert("TEST-1".to_string(), test_output_snapshot("TEST-1"));
        evaluator.set_display_state(display_state);
        evaluator
            .lifecycle_enable("reload", Some(&tiled_workspace_persisted_state()))
            .expect("tiled lifecycle should succeed");

        let mut now = 0;
        let mut previous: Option<&str> = None;
        for id in ["0xa", "0xb", "0xc"] {
            let window = make_named_window(id, "kitty", false, true);
            evaluator
                .evaluate_window_preview(&window, now)
                .expect("preview should evaluate");
            if let Some(previous) = previous {
                let unfocused = make_named_window(previous, "kitty", false, true);
                evaluator
                    .evaluate_window(&unfocused, now + 25)
                    .expect("defocus evaluation should succeed");
            }
            let focused = make_named_window(id, "kitty", true, true);
            evaluator
                .evaluate_window(&focused, now + 50)
                .expect("evaluation should succeed");
            previous = Some(id);
            now += 100;
        }

        // Focus sits on 0xc, centered by the maximized scrollToWindow branch.
        // Each left press must advance immediately: 0xc → 0xb → 0xa.
        let first = evaluator
            .invoke_key_binding("tile-focus-left-quick", now)
            .expect("first focus-left should evaluate");
        assert!(
            has_action(&first.actions, "0xb", crate::ssd::WaylandWindowAction::Focus),
            "a fully-visible maximized tile must advance on the first press: {:?}",
            first.actions
        );

        let second = evaluator
            .invoke_key_binding("tile-focus-left-quick", now + 100)
            .expect("second focus-left should evaluate");
        assert!(
            has_action(&second.actions, "0xa", crate::ssd::WaylandWindowAction::Focus),
            "every subsequent press must advance one tile as well: {:?}",
            second.actions
        );
    }

    #[test]
    fn xdg_activation_of_focused_window_does_not_minimize() {
        let actions = activate_toggle_fixture(
            true,
            crate::ssd::WindowActivateRequestSourceSnapshot::XdgActivation,
        );
        assert!(
            !has_action(&actions, "0xa", crate::ssd::WaylandWindowAction::Minimize),
            "xdg-activation must never trigger the minimize toggle: {actions:?}"
        );
    }

    #[test]
    fn plain_second_window_launches_above_existing_window_tiled() {
        let (editor_z, chrome_z) = launch_scenario_z_indices(false, true);
        assert!(
            chrome_z > editor_z,
            "second (plain, tiled ws) window should stack above: editor={editor_z} chrome={chrome_z}"
        );
    }

    #[test]
    fn maximized_second_window_launches_above_existing_window_tiled() {
        let (editor_z, chrome_z) = launch_scenario_z_indices(true, true);
        assert!(
            chrome_z > editor_z,
            "second (maximized, tiled ws) window should stack above: editor={editor_z} chrome={chrome_z}"
        );
    }

    #[test]
    fn embedded_runtime_reload_picks_up_submodule_key_bindings() {
        let repository_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .expect("repository root should exist");
        let test_dir = std::env::temp_dir().join(format!(
            "shojiwm-deno-submodule-reload-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&test_dir).expect("test directory should be created");
        let config_path = test_dir.join("config.tsx");
        std::fs::write(
            &config_path,
            r#"
import { COMPOSITOR, Label } from "shoji_wm";
import "./bindings.ts";
COMPOSITOR.window.composition = () => <Label text="x" />;
"#,
        )
        .expect("test config should be written");
        let bindings_path = test_dir.join("bindings.ts");
        let write_bindings = |extra_binding: bool| {
            let extra = if extra_binding {
                r#"COMPOSITOR.key.bind("second", "Super+Y", () => {});"#
            } else {
                ""
            };
            std::fs::write(
                &bindings_path,
                format!(
                    r#"
import {{ COMPOSITOR }} from "shoji_wm";
COMPOSITOR.key.bind("first", "Super+T", () => {{}});
{extra}
"#
                ),
            )
            .expect("test bindings module should be written");
        };
        let binding_ids = |update: &Option<RuntimeKeyBindingConfigUpdate>| -> Vec<String> {
            update
                .as_ref()
                .map(|update| update.entries.iter().map(|entry| entry.id.clone()).collect())
                .unwrap_or_default()
        };

        write_bindings(false);
        let evaluator = EmbeddedDecorationEvaluator::for_paths(
            repository_root.join("tools/decoration-runtime.ts"),
            &config_path,
        )
        .with_working_dir(&repository_root);
        let invocation = evaluator
            .lifecycle_enable("initial", None)
            .expect("initial lifecycle enable should succeed");
        assert_eq!(
            binding_ids(&invocation.key_binding_config),
            vec!["first".to_string()],
        );

        write_bindings(true);
        let persisted = evaluator
            .lifecycle_disable("reload")
            .expect("lifecycle disable should succeed");
        let reloaded = evaluator.fresh_like();
        let invocation = reloaded
            .lifecycle_enable("reload", Some(&persisted))
            .expect("reload lifecycle enable should succeed");
        assert_eq!(
            binding_ids(&invocation.key_binding_config),
            vec!["first".to_string(), "second".to_string()],
            "hot reload should pick up key bindings added in imported submodules"
        );

        drop(reloaded);
        drop(evaluator);
        let _ = std::fs::remove_dir_all(&test_dir);
    }

    #[test]
    fn embedded_runtime_reload_delivers_new_key_bindings() {
        let repository_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .expect("repository root should exist");
        let test_dir = std::env::temp_dir().join(format!(
            "shojiwm-deno-keybinding-reload-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&test_dir).expect("test directory should be created");
        let config_path = test_dir.join("config.tsx");
        let write_config = |extra_binding: bool| {
            let extra = if extra_binding {
                r#"COMPOSITOR.key.bind("second", "Super+Y", () => {});"#
            } else {
                ""
            };
            std::fs::write(
                &config_path,
                format!(
                    r#"
import {{ COMPOSITOR, Label }} from "shoji_wm";
COMPOSITOR.key.bind("first", "Super+T", () => {{}});
{extra}
COMPOSITOR.window.composition = () => <Label text="x" />;
"#
                ),
            )
            .expect("test config should be written");
        };
        let binding_ids = |update: &Option<RuntimeKeyBindingConfigUpdate>| -> Vec<String> {
            update
                .as_ref()
                .map(|update| update.entries.iter().map(|entry| entry.id.clone()).collect())
                .unwrap_or_default()
        };

        write_config(false);
        let evaluator = EmbeddedDecorationEvaluator::for_paths(
            repository_root.join("tools/decoration-runtime.ts"),
            &config_path,
        )
        .with_working_dir(&repository_root);
        let invocation = evaluator
            .lifecycle_enable("initial", None)
            .expect("initial lifecycle enable should succeed");
        assert_eq!(
            binding_ids(&invocation.key_binding_config),
            vec!["first".to_string()],
            "initial lifecycle should deliver the initial key bindings"
        );

        write_config(true);
        let persisted = evaluator
            .lifecycle_disable("reload")
            .expect("lifecycle disable should succeed");
        let reloaded = evaluator.fresh_like();
        let invocation = reloaded
            .lifecycle_enable("reload", Some(&persisted))
            .expect("reload lifecycle enable should succeed");
        assert_eq!(
            binding_ids(&invocation.key_binding_config),
            vec!["first".to_string(), "second".to_string()],
            "hot reload should deliver the updated key binding set"
        );

        drop(reloaded);
        drop(evaluator);
        let _ = std::fs::remove_dir_all(&test_dir);
    }

    #[test]
    fn embedded_runtime_fresh_instance_reloads_config() {
        let repository_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .expect("repository root should exist");
        let test_dir =
            std::env::temp_dir().join(format!("shojiwm-deno-reload-test-{}", std::process::id()));
        std::fs::create_dir_all(&test_dir).expect("test directory should be created");
        let config_path = test_dir.join("config.tsx");
        let write_config = |text: &str| {
            std::fs::write(
                &config_path,
                format!(
                    r#"
import {{ COMPOSITOR, Label }} from "shoji_wm";
COMPOSITOR.window.composition = () => <Label text={text:?} />;
"#
                ),
            )
            .expect("test config should be written");
        };
        let evaluate_text = |evaluator: &EmbeddedDecorationEvaluator| {
            let result = evaluator
                .evaluate_window(&make_window(false), 0)
                .expect("config should evaluate");
            match result.node.kind {
                DecorationNodeKind::Label(label) => label.text,
                other => panic!("expected label root, got {other:?}"),
            }
        };

        write_config("before");
        let evaluator = EmbeddedDecorationEvaluator::for_paths(
            repository_root.join("tools/decoration-runtime.ts"),
            &config_path,
        )
        .with_working_dir(&repository_root);
        assert_eq!(evaluate_text(&evaluator), "before");

        write_config("after");
        let reloaded = evaluator.fresh_like();
        assert_eq!(evaluate_text(&reloaded), "after");

        drop(reloaded);
        drop(evaluator);
        let _ = std::fs::remove_dir_all(&test_dir);
    }

    #[test]
    fn embedded_runtime_reseeds_multiple_windows_after_lifecycle_restore() {
        let repository_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .expect("repository root should exist");
        let test_dir = std::env::temp_dir().join(format!(
            "shojiwm-deno-lifecycle-reseed-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&test_dir).expect("test directory should be created");
        let config_path = test_dir.join("config.tsx");
        std::fs::write(
            &config_path,
            r#"
import { COMPOSITOR, Label } from "shoji_wm";

let restored = "missing";
COMPOSITOR.onEnable((event) => {
  if (event.isReloading) {
    restored = event.restore("test.state")?.value ?? "missing";
  }
});
COMPOSITOR.onDisable((event) => {
  if (event.isReloading) {
    event.persist("test.state", { value: "restored" });
  }
});
COMPOSITOR.window.composition = () => <Label text={restored} />;
"#,
        )
        .expect("test config should be written");

        let evaluator = EmbeddedDecorationEvaluator::for_paths(
            repository_root.join("tools/decoration-runtime.ts"),
            &config_path,
        )
        .with_working_dir(&repository_root);
        evaluator
            .lifecycle_enable("initial", None)
            .expect("initial lifecycle should enable");
        let persisted = evaluator
            .lifecycle_disable("reload")
            .expect("reload lifecycle should persist state");

        let reloaded = evaluator.fresh_like();
        reloaded
            .lifecycle_enable("reload", Some(&persisted))
            .expect("reload lifecycle should restore state");

        for index in 0..2 {
            let mut window = make_window(false);
            window.id = format!("reload-window-{index}");
            let result = reloaded
                .evaluate_cached_window(&window.id, Some(&window), 0, true)
                .expect("empty runtime cache should be re-seeded from the snapshot");
            let node = result
                .node
                .expect("forced cache re-seed should return a full tree");
            match node.kind {
                DecorationNodeKind::Label(label) => assert_eq!(label.text, "restored"),
                other => panic!("expected label root, got {other:?}"),
            }
        }

        drop(reloaded);
        drop(evaluator);
        let _ = std::fs::remove_dir_all(&test_dir);
    }

    #[test]
    fn embedded_runtime_returns_native_composition_patches_for_signal_updates() {
        let repository_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .expect("repository root should exist");
        let test_dir =
            std::env::temp_dir().join(format!("shojiwm-deno-patch-test-{}", std::process::id()));
        std::fs::create_dir_all(&test_dir).expect("test directory should be created");
        let config_path = test_dir.join("config.tsx");
        std::fs::write(
            &config_path,
            r#"
import {
  animationVariable,
  Box,
  ClientWindow,
  COMPOSITOR,
} from "shoji_wm";

const phase = animationVariable("native-patch-test");
COMPOSITOR.window.composition = (window) => {
  const opacity = window.animation.variable(phase);
  if (!window.animation.running(phase)) {
    window.animation.start(phase, {
      duration: 1000,
      from: 0,
      to: 1,
    });
  }
  return <Box style={{ opacity }}><ClientWindow /></Box>;
};
"#,
        )
        .expect("test config should be written");

        let evaluator = EmbeddedDecorationEvaluator::for_paths(
            repository_root.join("tools/decoration-runtime.ts"),
            &config_path,
        )
        .with_working_dir(&repository_root);
        let window = make_window(false);
        evaluator
            .evaluate_window(&window, 0)
            .expect("initial native composition should evaluate");
        let tick = evaluator
            .scheduler_tick(16)
            .expect("animation scheduler should advance");
        assert!(tick.dirty_window_ids.iter().any(|id| id == &window.id));

        let cached = evaluator
            .evaluate_cached_window(&window.id, None, 16, false)
            .expect("cached native composition should evaluate");
        assert!(
            cached.node.is_none(),
            "signal-only updates must not return a full tree; dirty ids: {:?}",
            cached.dirty_node_ids
        );
        assert!(
            !cached.node_patches.is_empty(),
            "signal-only updates must return native subtree patches"
        );
        assert!(cached.node_patches.iter().all(|patch| {
            patch
                .replacement_node()
                .is_none_or(|node| node.stable_id.as_deref() == Some(patch.node_id()))
        }));

        drop(evaluator);
        let _ = std::fs::remove_dir_all(&test_dir);
    }

    #[test]
    fn embedded_runtime_uses_direct_shader_uniform_patches_for_animation() {
        let repository_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .expect("repository root should exist");
        let test_dir = std::env::temp_dir().join(format!(
            "shojiwm-deno-uniform-patch-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&test_dir).expect("test directory should be created");
        std::fs::write(
            test_dir.join("animated.frag"),
            "#version 100\nprecision mediump float;\nvoid main() { gl_FragColor = vec4(1.0); }\n",
        )
        .expect("test shader should be written");
        let config_path = test_dir.join("config.tsx");
        std::fs::write(
            &config_path,
            r#"
import {
  animationVariable,
  backdropSource,
  ClientWindow,
  compileEffect,
  COMPOSITOR,
  loadShader,
  shaderStage,
  ShaderEffect,
  uniformArray,
} from "shoji_wm";

const phase = animationVariable("native-uniform-patch-test");
COMPOSITOR.window.composition = (window) => {
  const value = window.animation.variable(phase);
  if (!window.animation.running(phase)) {
    window.animation.start(phase, {
      duration: 1000,
      from: 0,
      to: 1,
    });
  }
  const effect = compileEffect({
    input: backdropSource(),
    pipeline: [
      shaderStage(loadShader("./animated.frag"), {
        uniforms: {
          phase_01: value,
          control_points: uniformArray.vec2([[value, 0], [1, value]]),
        },
      }),
    ],
  });
  return <ShaderEffect shader={effect}><ClientWindow /></ShaderEffect>;
};
"#,
        )
        .expect("test config should be written");

        let evaluator = EmbeddedDecorationEvaluator::for_paths(
            repository_root.join("tools/decoration-runtime.ts"),
            &config_path,
        )
        .with_working_dir(&test_dir);
        let window = make_window(false);
        evaluator
            .evaluate_window(&window, 0)
            .expect("initial native composition should evaluate");
        let tick = evaluator
            .scheduler_tick(16)
            .expect("animation scheduler should advance");
        assert!(
            tick.dirty_window_node_ids
                .get(&window.id)
                .is_some_and(|node_ids| !node_ids.is_empty()),
            "uniform-only animation must remain node-scoped"
        );
        let cached = evaluator
            .evaluate_cached_window(&window.id, None, 16, false)
            .expect("cached native composition should evaluate");

        assert!(!cached.node_patches.is_empty());
        assert!(cached.node_patches.iter().any(|patch| matches!(
            patch,
            NativeCompositionPatch::ShaderUniform {
                name,
                stage_index: 0,
                ..
            } if name == "phase_01"
        )));
        assert!(cached.node_patches.iter().any(|patch| matches!(
            patch,
            NativeCompositionPatch::ShaderUniform {
                name,
                value: super::super::ShaderUniformValue::Vec2Array(values),
                ..
            } if name == "control_points"
                && values.len() == 2
                && values[0][0] > 0.0
                && values[1][1] > 0.0
        )));

        drop(evaluator);
        let _ = std::fs::remove_dir_all(&test_dir);
    }

    #[test]
    fn embedded_runtime_uses_direct_effect_uniform_patches_for_animation() {
        let repository_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .expect("repository root should exist");
        let test_dir = std::env::temp_dir().join(format!(
            "shojiwm-deno-effect-uniform-patch-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&test_dir).expect("test directory should be created");
        std::fs::write(
            test_dir.join("animated.frag"),
            "#version 100\nprecision mediump float;\nvoid main() { gl_FragColor = vec4(1.0); }\n",
        )
        .expect("test shader should be written");
        let config_path = test_dir.join("config.tsx");
        std::fs::write(
            &config_path,
            r#"
import {
  animationVariable,
  Box,
  compileWindowEffect,
  COMPOSITOR,
  loadShader,
  shaderStage,
  uniformArray,
  windowSource,
} from "shoji_wm";

const phase = animationVariable("native-effect-uniform-patch-test");
COMPOSITOR.window.composition = (window) => {
  if (!window.animation.running(phase)) {
    window.animation.start(phase, {
      duration: 1000,
      from: 0,
      to: 1,
    });
  }
  return <Box />;
};
COMPOSITOR.effect.window = (window) => ({
  behind: compileWindowEffect({
    input: windowSource(),
    pipeline: [
      shaderStage(loadShader("./animated.frag"), {
        uniforms: {
          phase_01: uniformArray.float(
            window.animation.variable(phase)((value) => [value, 0.5]),
          ),
        },
      }),
    ],
  }),
});
"#,
        )
        .expect("test config should be written");

        let evaluator = EmbeddedDecorationEvaluator::for_paths(
            repository_root.join("tools/decoration-runtime.ts"),
            &config_path,
        )
        .with_working_dir(&test_dir);
        let window = make_window(false);
        evaluator
            .evaluate_window(&window, 0)
            .expect("initial native effect should evaluate");
        let patches_before = evaluator
            .runtime
            .lock()
            .expect("runtime lock should be available")
            .as_ref()
            .expect("runtime should be initialized")
            .child
            .effect_uniform_patch_count();

        evaluator
            .scheduler_tick(16)
            .expect("animation scheduler should advance");
        let cached = evaluator
            .evaluate_cached_window(&window.id, None, 16, false)
            .expect("cached native effect should evaluate");
        let patches_after = evaluator
            .runtime
            .lock()
            .expect("runtime lock should be available")
            .as_ref()
            .expect("runtime should remain initialized")
            .child
            .effect_uniform_patch_count();

        assert!(
            patches_after > patches_before,
            "effect animation must use the direct uniform slot path"
        );
        assert!(
            cached.window_effect_uniform_only,
            "cached effect animation must remain marked as uniform-only"
        );
        let phase = cached
            .window_effects
            .and_then(|effects| effects.behind)
            .and_then(|slot| slot.effect.pipeline.into_iter().next())
            .and_then(|stage| match stage {
                EffectStage::Shader(shader) => shader.uniforms.get("phase_01").cloned(),
                _ => None,
            });
        assert!(matches!(
            phase,
            Some(super::super::ShaderUniformValue::FloatArray(values))
                if values.len() == 2 && values[0] > 0.0 && values[1] == 0.5
        ));

        drop(evaluator);
        let _ = std::fs::remove_dir_all(&test_dir);
    }

    #[test]
    fn embedded_runtime_uses_configured_working_directory() {
        let repository_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .expect("repository root should exist");
        let test_dir =
            std::env::temp_dir().join(format!("shojiwm-deno-cwd-test-{}", std::process::id()));
        std::fs::create_dir_all(&test_dir).expect("test directory should be created");
        let config_path = test_dir.join("config.tsx");
        std::fs::write(
            &config_path,
            r#"
import { COMPOSITOR, Label } from "shoji_wm";
COMPOSITOR.window.composition = () => <Label text={Deno.cwd()} />;
"#,
        )
        .expect("test config should be written");

        let evaluator = EmbeddedDecorationEvaluator::for_paths(
            repository_root.join("tools/decoration-runtime.ts"),
            &config_path,
        )
        .with_working_dir(&test_dir);
        let result = evaluator
            .evaluate_window(&make_window(false), 0)
            .expect("config should evaluate");
        match result.node.kind {
            DecorationNodeKind::Label(label) => {
                assert_eq!(PathBuf::from(label.text), test_dir);
            }
            other => panic!("expected label root, got {other:?}"),
        }

        drop(evaluator);
        let _ = std::fs::remove_dir_all(&test_dir);
    }

    #[test]
    fn embedded_runtime_serves_deno_unix_ipc() {
        let repository_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .expect("repository root should exist");
        let test_dir =
            std::env::temp_dir().join(format!("shojiwm-deno-ipc-test-{}", std::process::id()));
        std::fs::create_dir_all(&test_dir).expect("test directory should be created");
        let config_path = test_dir.join("config.tsx");
        let socket_path = test_dir.join("ipc.sock");
        drop(
            std::os::unix::net::UnixListener::bind(&socket_path)
                .expect("stale Unix socket should be created"),
        );
        assert!(
            socket_path.exists(),
            "dropping a Unix listener should leave its socket path behind"
        );
        let socket_literal =
            serde_json::to_string(&socket_path.to_string_lossy()).expect("path should serialize");
        std::fs::write(
            &config_path,
            format!(
                r#"
import {{ Box, COMPOSITOR }} from "shoji_wm";
import {{ createIpcServer }} from "shoji_wm/ipc";

const ipc = createIpcServer({socket_literal});
ipc.handle("ping", () => "pong");
COMPOSITOR.onDisable(() => ipc.close());
COMPOSITOR.window.composition = () => <Box />;
"#
            ),
        )
        .expect("test config should be written");

        let evaluator = EmbeddedDecorationEvaluator::for_paths(
            repository_root.join("tools/decoration-runtime.ts"),
            &config_path,
        )
        .with_working_dir(&repository_root);
        evaluator
            .lifecycle_enable("test", None)
            .expect("embedded runtime should enable the IPC config");

        let mut socket =
            UnixStream::connect(&socket_path).expect("Deno Unix listener should accept clients");
        socket
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("read timeout should be configured");
        socket
            .write_all(b"{\"id\":1,\"method\":\"ping\"}\n")
            .expect("IPC request should be written");
        let mut response = String::new();
        BufReader::new(socket)
            .read_line(&mut response)
            .expect("IPC response should be read");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&response)
                .expect("IPC response should be JSON"),
            serde_json::json!({ "id": 1, "result": "pong" })
        );

        evaluator
            .lifecycle_disable("test")
            .expect("embedded runtime should disable the IPC config");
        assert!(
            !socket_path.exists(),
            "closing the Deno IPC server should remove its socket path"
        );
        drop(evaluator);
        let _ = std::fs::remove_dir_all(&test_dir);
    }

    // A reload builds a fresh isolate rather than reusing one, and cppgc
    // finalizers are not guaranteed to run before teardown, so a listener fd
    // could outlive the runtime that opened it. Super+Shift+R cannot be driven
    // from a test (input.rs intercepts it in the compositor), so cycle the
    // runtime directly and watch the process fd table.
    #[test]
    fn embedded_runtime_ipc_does_not_leak_fds_across_reloads() {
        fn open_fds() -> usize {
            std::fs::read_dir("/proc/self/fd")
                .map(|entries| entries.count())
                .unwrap_or(0)
        }

        let repository_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .expect("repository root should exist");
        // Keep an encoded-looking segment in the path so the runtime's manual
        // file URL conversion cannot accidentally decode it into a space.
        let test_dir = std::env::temp_dir()
            .join(format!("shojiwm-ipc%20reload-test-{}", std::process::id()));
        std::fs::create_dir_all(&test_dir).expect("test directory should be created");

        let mut baseline = 0usize;
        for cycle in 0..6 {
            let socket_path = test_dir.join(format!("ipc-{cycle}.sock"));
            let socket_literal = serde_json::to_string(&socket_path.to_string_lossy())
                .expect("path should serialize");
            let config_path = test_dir.join(format!("config-{cycle}.tsx"));
            std::fs::write(
                &config_path,
                format!(
                    r#"
import {{ Box, COMPOSITOR }} from "shoji_wm";
import {{ createIpcServer }} from "shoji_wm/ipc";

const ipc = createIpcServer({socket_literal});
ipc.handle("ping", () => "pong");
COMPOSITOR.window.composition = () => <Box />;
"#
                ),
            )
            .expect("test config should be written");

            let evaluator = EmbeddedDecorationEvaluator::for_paths(
                repository_root.join("tools/decoration-runtime.ts"),
                &config_path,
            )
            .with_working_dir(&repository_root);
            evaluator
                .lifecycle_enable("test", None)
                .expect("embedded runtime should enable the IPC config");

            let mut socket = UnixStream::connect(&socket_path)
                .expect("each reload cycle should serve its own socket");
            socket
                .set_read_timeout(Some(Duration::from_secs(2)))
                .expect("read timeout should be configured");
            socket
                .write_all(b"{\"id\":1,\"method\":\"ping\"}\n")
                .expect("IPC request should be written");
            let mut response = String::new();
            BufReader::new(socket)
                .read_line(&mut response)
                .expect("IPC response should be read");
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&response)
                    .expect("IPC response should be JSON"),
                serde_json::json!({ "id": 1, "result": "pong" })
            );

            evaluator
                .lifecycle_disable("test")
                .expect("embedded runtime should disable the IPC config");
            drop(evaluator);

            // Let the first couple of cycles settle before sampling, so
            // one-off allocations are not counted as growth.
            if cycle == 1 {
                baseline = open_fds();
            }
        }

        let after = open_fds();
        assert!(
            after <= baseline + 2,
            "IPC sockets leaked across reloads: {baseline} fds after cycle 1, {after} after cycle 5"
        );

        let _ = std::fs::remove_dir_all(&test_dir);
    }

    fn test_output_snapshot(name: &str) -> WaylandOutputSnapshot {
        use crate::ssd::window_model::{OutputModeSnapshot, OutputPositionSnapshot};
        WaylandOutputSnapshot {
            name: name.to_owned(),
            description: None,
            make: None,
            model: None,
            serial: None,
            connector: None,
            enabled: true,
            resolution: Some(OutputModeSnapshot {
                width: 1920,
                height: 1080,
                refresh_rate: 60.0,
            }),
            position: OutputPositionSnapshot { x: 0, y: 0 },
            scale: 1.0,
            available_modes: Vec::new(),
        }
    }

    // The interaction paths omit display and input state when it has not changed
    // since the runtime last received it. A gate that never opened would leave
    // the runtime on permanently stale outputs, so pin both directions.
    #[test]
    fn interaction_state_payload_elides_only_unchanged_state() {
        let evaluator = EmbeddedDecorationEvaluator::for_paths(
            PathBuf::from("decoration-runtime.ts"),
            PathBuf::from("config.tsx"),
        );

        // A freshly spawned runtime records generation 0 while the evaluator
        // starts at 1, so the first request after any spawn carries full state.
        let (display, input, first) = evaluator.interaction_state_payload(0);
        assert!(display.is_some(), "cold start must send display state");
        assert!(input.is_some(), "cold start must send input state");

        let (display, input, _) = evaluator.interaction_state_payload(first);
        assert!(
            display.is_none() && input.is_none(),
            "unchanged state must not cross the bridge"
        );

        evaluator.set_display_state(std::collections::BTreeMap::from([(
            "DP-1".to_owned(),
            test_output_snapshot("DP-1"),
        )]));
        let (display, input, second) = evaluator.interaction_state_payload(first);
        assert!(
            display.is_some() && input.is_some(),
            "a changed output must reopen the gate"
        );
        assert!(second > first, "a real change must bump the generation");

        // set_display_state only bumps on an actual difference, so re-setting the
        // same map must leave the gate shut.
        evaluator.set_display_state(std::collections::BTreeMap::from([(
            "DP-1".to_owned(),
            test_output_snapshot("DP-1"),
        )]));
        let (display, _, third) = evaluator.interaction_state_payload(second);
        assert!(
            display.is_none(),
            "identical state must not reopen the gate"
        );
        assert_eq!(third, second);
    }

    // End-to-end counterpart to the unit test above. The elided fields are
    // omitted by `skip_serializing_if`, so the runtime must see no key at all —
    // a present-but-undefined field would hit `"displayState" in request` and
    // wipe the cached outputs instead of reusing them.
    #[test]
    fn elided_interaction_state_keeps_the_runtime_outputs_cached() {
        use crate::ssd::{
            PointerHitTargetSnapshot, PointerModifierStateSnapshot, PointerMoveEventSnapshot,
            PointerMovePointSnapshot,
        };

        let repository_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .expect("repository root should exist");
        let test_dir = std::env::temp_dir().join(format!(
            "shojiwm-interaction-gate-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&test_dir).expect("test directory should be created");
        let socket_path = test_dir.join("gate.sock");
        let socket_literal =
            serde_json::to_string(&socket_path.to_string_lossy()).expect("path should serialize");
        let config_path = test_dir.join("config.tsx");
        std::fs::write(
            &config_path,
            format!(
                r#"
import {{ Box, COMPOSITOR }} from "shoji_wm";
import {{ createIpcServer }} from "shoji_wm/ipc";

const ipc = createIpcServer({socket_literal});
ipc.handle("outputs", () => COMPOSITOR.output.list);
COMPOSITOR.window.composition = () => <Box />;
COMPOSITOR.event.onPointerMove(() => {{}});
"#
            ),
        )
        .expect("test config should be written");

        let outputs_seen_by_runtime = || -> Vec<String> {
            let mut socket =
                UnixStream::connect(&socket_path).expect("IPC server should be listening");
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .expect("read timeout should be configured");
            socket
                .write_all(b"{\"id\":1,\"method\":\"outputs\"}\n")
                .expect("IPC request should be written");
            let mut response = String::new();
            BufReader::new(socket)
                .read_line(&mut response)
                .expect("IPC response should be read");
            let parsed: serde_json::Value =
                serde_json::from_str(&response).expect("IPC response should be JSON");
            parsed["result"]
                .as_array()
                .expect("outputs handler should return an array")
                .iter()
                .map(|name| name.as_str().unwrap_or_default().to_owned())
                .collect()
        };

        let evaluator = EmbeddedDecorationEvaluator::for_paths(
            repository_root.join("tools/decoration-runtime.ts"),
            &config_path,
        )
        .with_working_dir(&repository_root);
        evaluator
            .lifecycle_enable("initial", None)
            .expect("embedded runtime should enable the gate config");

        let pointer = PointerMoveEventSnapshot {
            position: PointerMovePointSnapshot { x: 1.0, y: 2.0 },
            delta: PointerMovePointSnapshot { x: 0.0, y: 0.0 },
            target: PointerHitTargetSnapshot::None,
            output_name: Some("DP-1".into()),
            modifiers: PointerModifierStateSnapshot {
                logo: false,
                alt: false,
                ctrl: false,
                shift: false,
            },
            timestamp: 1,
        };

        evaluator.set_display_state(std::collections::BTreeMap::from([(
            "DP-1".to_owned(),
            test_output_snapshot("DP-1"),
        )]));
        evaluator
            .pointer_move(&pointer, 1)
            .expect("pointer move should reach the runtime");
        assert_eq!(
            outputs_seen_by_runtime(),
            vec!["DP-1".to_string()],
            "a changed output must cross the bridge"
        );

        // Nothing changed, so this request omits both fields entirely.
        evaluator
            .pointer_move(&pointer, 2)
            .expect("second pointer move should reach the runtime");
        assert_eq!(
            outputs_seen_by_runtime(),
            vec!["DP-1".to_string()],
            "an omitted field must reuse the cache, not clear it"
        );

        evaluator.set_display_state(std::collections::BTreeMap::from([
            ("DP-1".to_owned(), test_output_snapshot("DP-1")),
            ("HDMI-1".to_owned(), test_output_snapshot("HDMI-1")),
        ]));
        evaluator
            .pointer_move(&pointer, 3)
            .expect("third pointer move should reach the runtime");
        let mut seen = outputs_seen_by_runtime();
        seen.sort();
        assert_eq!(
            seen,
            vec!["DP-1".to_string(), "HDMI-1".to_string()],
            "a later change must reopen the gate"
        );

        evaluator
            .lifecycle_disable("test")
            .expect("embedded runtime should disable");
        drop(evaluator);
        let _ = std::fs::remove_dir_all(&test_dir);
    }

    // A reload used to hand the new generation a freshly allocated dispatcher,
    // stranding the pointer-move worker on a condvar nobody would notify again.
    // That worker owns an evaluator clone, so every reload the pointer had armed
    // leaked a V8 isolate, two threads and four fds. Cycle the runtime with the
    // worker armed, which is the case the keyboard-only reload burst never hits.
    #[test]
    fn embedded_runtime_reload_reuses_the_pointer_move_worker() {
        use crate::ssd::{
            PointerHitTargetSnapshot, PointerModifierStateSnapshot, PointerMoveEventSnapshot,
            PointerMovePointSnapshot,
        };

        // The thread is named "shojiwm-pointer-move-async"; comm truncates to 15.
        fn pointer_workers() -> usize {
            std::fs::read_dir("/proc/self/task")
                .map(|entries| {
                    entries
                        .filter_map(|entry| entry.ok())
                        .filter(|entry| {
                            std::fs::read_to_string(entry.path().join("comm"))
                                .is_ok_and(|comm| comm.trim() == "shojiwm-pointer")
                        })
                        .count()
                })
                .unwrap_or(0)
        }

        fn settle_at(expected: usize) -> usize {
            for _ in 0..100 {
                if pointer_workers() == expected {
                    return expected;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            pointer_workers()
        }
        let repository_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .expect("repository root should exist");
        let test_dir = std::env::temp_dir().join(format!(
            "shojiwm-pointer-reload-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&test_dir).expect("test directory should be created");
        let config_path = test_dir.join("config.tsx");
        std::fs::write(
            &config_path,
            r#"
import { Box, COMPOSITOR } from "shoji_wm";

COMPOSITOR.window.composition = () => <Box />;
COMPOSITOR.event.onPointerMoveAsync(() => {});
"#,
        )
        .expect("test config should be written");

        let mut evaluator = EmbeddedDecorationEvaluator::for_paths(
            repository_root.join("tools/decoration-runtime.ts"),
            &config_path,
        )
        .with_working_dir(&repository_root);
        evaluator
            .lifecycle_enable("initial", None)
            .expect("embedded runtime should enable the pointer config");

        let pointer = PointerMoveEventSnapshot {
            position: PointerMovePointSnapshot { x: 10.0, y: 20.0 },
            delta: PointerMovePointSnapshot { x: 1.0, y: -1.0 },
            target: PointerHitTargetSnapshot::None,
            output_name: Some("output-1".into()),
            modifiers: PointerModifierStateSnapshot {
                logo: false,
                alt: false,
                ctrl: false,
                shift: false,
            },
            timestamp: 1,
        };

        // The worker only exists once a pointer sample has reached the evaluator.
        evaluator.enqueue_pointer_move_async(pointer.clone(), 1);
        assert_eq!(
            settle_at(1),
            1,
            "the first pointer sample should spawn exactly one worker"
        );

        for cycle in 0..4u64 {
            let persisted = evaluator
                .lifecycle_disable("reload")
                .expect("embedded runtime should disable for reload");

            let dispatcher = Arc::clone(&evaluator.pointer_move_async);
            let runtime_cell = Arc::clone(&evaluator.runtime);
            let reloaded = evaluator.fresh_like();
            assert!(
                Arc::ptr_eq(&dispatcher, &reloaded.pointer_move_async),
                "reload {cycle} should reuse the dispatcher instead of stranding the worker"
            );
            assert!(
                Arc::ptr_eq(&runtime_cell, &reloaded.runtime),
                "reload {cycle} should swap the runtime cell in place"
            );
            drop(dispatcher);
            drop(runtime_cell);

            reloaded
                .lifecycle_enable("reload", Some(&persisted))
                .expect("embedded runtime should re-enable after reload");
            evaluator = reloaded;

            evaluator.enqueue_pointer_move_async(pointer.clone(), cycle + 2);
            assert_eq!(
                settle_at(1),
                1,
                "reload {cycle} should not spawn a second pointer worker"
            );
            assert_eq!(
                Arc::strong_count(&evaluator.pointer_move_async),
                2,
                "reload {cycle} should leave only the evaluator and its worker holding the dispatcher"
            );
        }

        evaluator.shutdown();
        assert_eq!(settle_at(0), 0, "shutdown should retire the shared worker");
        drop(evaluator);
        let _ = std::fs::remove_dir_all(&test_dir);
    }

    #[test]
    fn decoration_policy_request_uses_runtime_wire_names() {
        let window = make_window(false);
        let context = WindowDecorationPolicyContextSnapshot {
            protocol: crate::ssd::WindowDecorationProtocolSnapshot::XdgDecorationV1,
            client_preference: Some(WindowDecorationModeSnapshot::Client),
            can_negotiate: true,
            reason: crate::ssd::WindowDecorationPolicyReasonSnapshot::ClientRequest,
        };
        let display_state = std::collections::BTreeMap::new();
        let input_state = std::collections::BTreeMap::new();
        let request = RuntimeRequest::WindowDecorationPolicy {
            request_id: 42,
            snapshot: &window,
            context: &context,
            display_state: &display_state,
            input_state: &input_state,
        };

        let value = serde_json::to_value(request).expect("request should serialize");
        assert_eq!(value["kind"], "windowDecorationPolicy");
        assert_eq!(value["requestId"], 42);
        assert_eq!(value["snapshot"]["decoration"]["configuredMode"], "server");
        assert_eq!(value["context"]["protocol"], "xdg-decoration-v1");
        assert_eq!(value["context"]["clientPreference"], "client");
        assert_eq!(value["context"]["canNegotiate"], true);
        assert_eq!(value["context"]["reason"], "clientRequest");
    }

    #[test]
    fn popup_backdrop_mask_can_sample_popup_source() {
        let effect = CompiledEffect {
            input: EffectInput::Backdrop,
            capture_padding: 0,
            invalidate: EffectInvalidationPolicy::Always,
            pipeline: vec![
                EffectStage::DualKawaseBlur(BackdropBlur {
                    radius: 4,
                    passes: 2,
                }),
                EffectStage::Shader(ShaderStage {
                    shader: ShaderModule {
                        path: "popup-mask.frag".into(),
                    },
                    uniforms: std::collections::BTreeMap::new(),
                    textures: std::collections::BTreeMap::from([(
                        "popup_mask".into(),
                        EffectInput::PopupSource(WindowSourceInclude::Full),
                    )]),
                }),
            ],
            alpha: EffectAlphaMode::Preserve,
        };
        let effects = WindowEffectConfig {
            behind: Some(WindowEffectSlot {
                effect,
                outsets: EffectOutsets::default(),
            }),
            ..Default::default()
        };

        assert!(validate_popup_effect_config(effects).is_ok());
    }
}
