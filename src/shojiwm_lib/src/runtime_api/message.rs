//! Compositor → runtime messages.
//!
//! [`RuntimeRequest`] needs an answer within the current compositor turn (a
//! decoration tree, a decision, the dirty set of a frame), so it is sent with
//! `ConfigRuntime::request` and borrows its snapshots. [`RuntimeEvent`] is
//! fire-and-forget and owned, so a runtime living on another thread can queue
//! it as is.
//!
//! Messages are plain Rust values, never serialized on the way in: a Rust
//! runtime matches on them directly, and a foreign runtime converts them into
//! whatever its own fast path wants.

use std::collections::BTreeMap;

use crate::{
    keyboard_layout::{KeyboardLayoutSnapshot, KeyboardLocksSnapshot},
    runtime_input::RuntimeInputDeviceSnapshot,
    runtime_workspace::RuntimeWorkspaceActivateRequestSnapshot,
    ssd::{
        BackgroundEffectConfig, DecorationNode, ShaderUniformValue, DecorationCachedEvaluationResult, DecorationEvaluationResult,
        DecorationHandlerInvocation, DecorationKeyBindingInvocation,
        DecorationPointerMoveAsyncInvocation, DecorationSchedulerTick,
        DecorationWindowMoveInvocation, DecorationWindowResizeInvocation,
        DecorationWindowStateRequestInvocation, GestureSwipeEventSnapshot,
        LayerEffectEvaluationResult, PointerMoveEventSnapshot, PopupEffectEvaluationResult,
        WaylandLayerSnapshot, WaylandOutputSnapshot, WaylandPopupSnapshot, WaylandWindowSnapshot,
        WindowActivateRequestEventSnapshot, WindowDecorationClientSnapshot,
        WindowDecorationDecisionSnapshot, WindowDecorationPolicyContextSnapshot, WindowFullscreenRequestEventSnapshot,
        WindowMaximizeRequestEventSnapshot, WindowMinimizeRequestEventSnapshot,
        WindowMoveEventSnapshot, WindowResizeEventSnapshot,
    },
};

/// A request that must be answered before the compositor continues.
#[derive(Debug, Clone, Copy)]
pub enum RuntimeRequest<'a> {
    Decoration(DecorationRequest<'a>),
    /// Advance timers and animations; answered with the frame's dirty set.
    SchedulerTick(SchedulerTickRequest<'a>),
    Window(WindowRequest<'a>),
    Input(InputRequest<'a>),
    Effect(EffectRequest<'a>),
    Workspace(WorkspaceRequest<'a>),
}

/// Which clock a scheduler tick advances.
///
/// Time in the runtime is kept per output: an output's clock is the
/// presentation time of the frames it shows, so work bound to that output
/// (polls, animations of the windows and layers on it) steps exactly once per
/// frame and is stamped with the vblank that frame lands on. `output` names
/// the output whose frame is about to be rendered; `None` is a wall-clock
/// tick from a timer, which advances only the work bound to outputs that are
/// not rendering frames (`frame_outputs` does not list them) — a powered-off
/// or disconnected output, or every output under a backend without frame
/// ticks.
#[derive(Debug, Clone, Copy)]
pub struct SchedulerTickRequest<'a> {
    pub output: Option<&'a str>,
    /// Outputs that render frames and therefore tick their own clock.
    pub frame_outputs: &'a [String],
}

#[derive(Debug, Clone, Copy)]
pub enum DecorationRequest<'a> {
    /// Build the full decoration tree of a window.
    Evaluate {
        window: &'a WaylandWindowSnapshot,
        /// Evaluate without committing runtime state (initial size probing).
        preview: bool,
    },
    /// Re-evaluate a window the runtime already knows, reusing its cache.
    /// `window` is `None` for closing windows, whose snapshot is frozen.
    EvaluateCached {
        window_id: &'a str,
        window: Option<&'a WaylandWindowSnapshot>,
        force_full: bool,
    },
    /// Server-side or client-side decorations for this window.
    Policy {
        window: &'a WaylandWindowSnapshot,
        context: &'a WindowDecorationPolicyContextSnapshot,
    },
    /// The legacy KDE decoration manager's default mode for a client that
    /// just bound it. GTK 3 decides on its own title bar from this alone.
    KdeDefaultMode {
        client: &'a WindowDecorationClientSnapshot,
    },
    /// A decoration node handler (button click, state change) fired.
    InvokeHandler {
        window_id: &'a str,
        handler_id: &'a str,
    },
    /// The window starts closing; the runtime may run a close animation.
    StartClose { window_id: &'a str },
    /// The window is gone; drop its state.
    Closed { window_id: &'a str },
}

#[derive(Debug, Clone, Copy)]
pub enum WindowRequest<'a> {
    Resize {
        window_id: &'a str,
        event: &'a WindowResizeEventSnapshot,
    },
    Move {
        window_id: &'a str,
        event: &'a WindowMoveEventSnapshot,
    },
    Maximize {
        window: &'a WaylandWindowSnapshot,
        event: &'a WindowMaximizeRequestEventSnapshot,
    },
    Minimize {
        window: &'a WaylandWindowSnapshot,
        event: &'a WindowMinimizeRequestEventSnapshot,
    },
    Fullscreen {
        window: &'a WaylandWindowSnapshot,
        event: &'a WindowFullscreenRequestEventSnapshot,
    },
    Activate {
        window: &'a WaylandWindowSnapshot,
        event: &'a WindowActivateRequestEventSnapshot,
    },
}

#[derive(Debug, Clone, Copy)]
pub enum InputRequest<'a> {
    KeyBinding { binding_id: &'a str },
    PointerMove(&'a PointerMoveEventSnapshot),
    GestureSwipe(&'a GestureSwipeEventSnapshot),
    /// The pointer entered, left or pressed against a `defineRegion` region.
    /// Answered like a pointer hook.
    PointerRegion(&'a crate::runtime_pointer_region::PointerRegionEventSnapshot),
    /// Input taken by the config's input grab `grab_id`. Answered like a
    /// pointer hook; a runtime that does not grab answers `Unhandled`.
    InputGrab {
        grab_id: u64,
        event: &'a crate::runtime_input_grab::InputGrabEventSnapshot,
    },
    /// A drag started, changed or ended. Sent only while the runtime asks
    /// for it (`RuntimeEventConfigUpdate::dnd`). Answered like a pointer hook.
    Dnd(&'a crate::dnd::DndEventSnapshot),
}

#[derive(Debug, Clone, Copy)]
pub enum EffectRequest<'a> {
    /// The output background effect; asked once after (re)loading.
    Background,
    Layers {
        output_name: &'a str,
        layers: &'a [WaylandLayerSnapshot],
    },
    Popups {
        output_name: &'a str,
        popups: &'a [WaylandPopupSnapshot],
    },
}

#[derive(Debug, Clone, Copy)]
pub enum WorkspaceRequest<'a> {
    Activate(&'a RuntimeWorkspaceActivateRequestSnapshot),
}

/// The direct answer to a [`RuntimeRequest`]. Side effects that are not part
/// of the answer go through `RuntimeHost` instead.
#[derive(Debug, Clone)]
pub enum RuntimeReply {
    Evaluation(Box<DecorationEvaluationResult>),
    CachedEvaluation(Box<DecorationCachedEvaluationResult>),
    DecorationPolicy(WindowDecorationDecisionSnapshot),
    SchedulerTick(DecorationSchedulerTick),
    Handler(Box<DecorationHandlerInvocation>),
    KeyBinding(DecorationKeyBindingInvocation),
    WindowResize(DecorationWindowResizeInvocation),
    WindowMove(DecorationWindowMoveInvocation),
    WindowStateRequest(DecorationWindowStateRequestInvocation),
    PointerHook(DecorationPointerMoveAsyncInvocation),
    BackgroundEffect(Option<BackgroundEffectConfig>),
    LayerEffects(LayerEffectEvaluationResult),
    PopupEffects(PopupEffectEvaluationResult),
    /// Handled, nothing to report.
    Done,
    /// The runtime does not implement this request; the compositor falls back
    /// to its built-in behavior.
    Unhandled,
}

/// A notification that needs no answer.
#[derive(Debug, Clone)]
pub enum RuntimeEvent {
    /// Connected outputs changed (mode, scale, position, hotplug).
    DisplayState(BTreeMap<String, WaylandOutputSnapshot>),
    /// Input devices changed.
    InputState(BTreeMap<String, RuntimeInputDeviceSnapshot>),
    KeyboardLayout(KeyboardLayoutSnapshot),
    /// Caps Lock or Num Lock changed.
    KeyboardLocks(KeyboardLocksSnapshot),
    /// Pointer hook the config asked to receive asynchronously. A result, if
    /// any, comes back as `HostMessage::PointerHookResult`.
    PointerMove(PointerMoveEventSnapshot),
    /// Asynchronous gesture hook, same delivery as [`Self::PointerMove`].
    GestureSwipe(GestureSwipeEventSnapshot),
    /// Touchpad pinches, delivered without waiting for a result.
    GesturePinch(crate::ssd::GesturePinchEventSnapshot),
}

/// One change to a decoration tree the compositor already holds, returned by
/// a cached evaluation instead of a whole new tree.
// boxing left as a follow-up (touches all construction/match sites)
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone)]
pub enum CompositionPatch {
    /// A structural or otherwise generic node change. This remains the
    /// compatibility fallback and performs one serde_v8 conversion.
    ReplaceNode {
        node_id: String,
        node: DecorationNode,
    },
    /// A node whose own props changed: `node` comes without children, and the
    /// node keeps the ones it has. A root prop (a window's shadow) then does
    /// not send the whole tree across the bridge again.
    ReplaceProps {
        node_id: String,
        node: DecorationNode,
    },
    /// The steady animation fast path. Mutate one uniform in the compositor's
    /// persistent tree without decoding or rebuilding the shader pipeline.
    ShaderUniform {
        node_id: String,
        stage_index: usize,
        name: String,
        value: ShaderUniformValue,
    },
}

pub const SHADER_INPUT_STAGE_INDEX: usize = u32::MAX as usize;
/// `stage_index` of a uniform of a node's `paint` shader.
pub const PAINT_STAGE_INDEX: usize = u32::MAX as usize - 1;
/// `stage_index` of a uniform of a node's `overlay` shader.
pub const OVERLAY_STAGE_INDEX: usize = u32::MAX as usize - 2;

impl CompositionPatch {
    pub fn node_id(&self) -> &str {
        match self {
            Self::ReplaceNode { node_id, .. }
            | Self::ReplaceProps { node_id, .. }
            | Self::ShaderUniform { node_id, .. } => node_id,
        }
    }

    /// The node a patch puts in place; without children for `ReplaceProps`.
    pub fn replacement_node(&self) -> Option<&DecorationNode> {
        match self {
            Self::ReplaceNode { node, .. } | Self::ReplaceProps { node, .. } => Some(node),
            Self::ShaderUniform { .. } => None,
        }
    }
}

