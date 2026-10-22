//! Compositor-side wrapper around the active [`ConfigRuntime`].
//!
//! Turns each message round trip into a typed call and applies the built-in
//! fallback when a runtime answers [`RuntimeReply::Unhandled`].

use std::collections::BTreeMap;

use super::{
    ConfigRuntime, DecorationRequest, EffectRequest, InputRequest, NullRuntime, ReloadPreparation,
    RuntimeError, RuntimeEvent, RuntimeHost, RuntimeReply, RuntimeRequest, SchedulerTickRequest,
    WindowRequest, WorkspaceRequest,
};
use crate::{
    keyboard_layout::{KeyboardLayoutSnapshot, KeyboardLocksSnapshot},
    runtime_input::RuntimeInputDeviceSnapshot,
    runtime_workspace::RuntimeWorkspaceActivateRequestSnapshot,
    ssd::{
        BackgroundEffectConfig, DecorationCachedEvaluationResult, DecorationEvaluationResult,
        DecorationEvaluator, DecorationGestureSwipeAsyncInvocation, DecorationHandlerInvocation,
        DecorationKeyBindingInvocation, DecorationPointerMoveAsyncInvocation,
        DecorationSchedulerTick, DecorationWindowMoveInvocation, DecorationWindowResizeInvocation,
        DecorationWindowStateRequestInvocation, GestureSwipeEventSnapshot,
        LayerEffectEvaluationResult, PointerMoveEventSnapshot, PopupEffectEvaluationResult,
        StaticDecorationEvaluator, WaylandLayerSnapshot, WaylandOutputSnapshot,
        WaylandPopupSnapshot, WaylandWindowSnapshot, WindowActivateRequestEventSnapshot,
        WindowDecorationClientSnapshot, WindowDecorationDecisionSnapshot,
        WindowDecorationModeSnapshot, WindowDecorationPolicyContextSnapshot,
        WindowFullscreenRequestEventSnapshot, WindowMaximizeRequestEventSnapshot,
        WindowMinimizeRequestEventSnapshot, WindowMoveEventSnapshot, WindowResizeEventSnapshot,
    },
};

pub struct RuntimeHandle {
    runtime: Box<dyn ConfigRuntime>,
    name: &'static str,
    host: RuntimeHost,
    /// Last state posted to the runtime, so unchanged snapshots are not
    /// re-sent every frame.
    display_state: Option<BTreeMap<String, WaylandOutputSnapshot>>,
    input_state: Option<BTreeMap<String, RuntimeInputDeviceSnapshot>>,
    keyboard_layout: Option<KeyboardLayoutSnapshot>,
    keyboard_locks: Option<KeyboardLocksSnapshot>,
}

impl std::fmt::Debug for RuntimeHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeHandle")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

impl Default for RuntimeHandle {
    fn default() -> Self {
        Self::new("none", Box::new(NullRuntime), RuntimeHost::detached())
    }
}

fn mismatch(request: &str, reply: &RuntimeReply) -> RuntimeError {
    RuntimeError::RuntimeProtocol(format!("unexpected reply to {request}: {reply:?}"))
}

impl RuntimeHandle {
    pub fn new(name: &'static str, runtime: Box<dyn ConfigRuntime>, host: RuntimeHost) -> Self {
        Self {
            runtime,
            name,
            host,
            display_state: None,
            input_state: None,
            keyboard_layout: None,
            keyboard_locks: None,
        }
    }

    pub fn name(&self) -> &'static str {
        self.name
    }

    pub fn host(&self) -> &RuntimeHost {
        &self.host
    }

    pub fn preload(&mut self) -> Result<(), RuntimeError> {
        self.runtime.preload()
    }

    pub fn enable(&mut self) -> Result<(), RuntimeError> {
        self.runtime.enable()
    }

    pub fn prepare_reload(&mut self) -> Result<ReloadPreparation, RuntimeError> {
        self.runtime.prepare_reload()
    }

    pub fn reload(&mut self) -> Result<(), RuntimeError> {
        self.runtime.reload()
    }

    pub fn shutdown(&mut self) {
        self.runtime.shutdown();
    }

    fn request(
        &mut self,
        now_ms: f64,
        request: RuntimeRequest<'_>,
    ) -> Result<RuntimeReply, RuntimeError> {
        self.runtime.request(now_ms, request)
    }

    fn post(&mut self, now_ms: f64, event: RuntimeEvent) {
        self.runtime.post(now_ms, event);
    }

    pub fn sync_display_state(&mut self, state: BTreeMap<String, WaylandOutputSnapshot>) {
        if self.display_state.as_ref() == Some(&state) {
            return;
        }
        self.display_state = Some(state.clone());
        self.post(0.0, RuntimeEvent::DisplayState(state));
    }

    pub fn sync_input_state(&mut self, state: BTreeMap<String, RuntimeInputDeviceSnapshot>) {
        if self.input_state.as_ref() == Some(&state) {
            return;
        }
        self.input_state = Some(state.clone());
        self.post(0.0, RuntimeEvent::InputState(state));
    }

    /// Returns whether the layout differs from the last one sent.
    pub fn sync_keyboard_layout(&mut self, layout: KeyboardLayoutSnapshot) -> bool {
        if self.keyboard_layout.as_ref() == Some(&layout) {
            return false;
        }
        self.keyboard_layout = Some(layout.clone());
        self.post(0.0, RuntimeEvent::KeyboardLayout(layout));
        true
    }

    /// Returns whether the lock keys differ from the last state sent.
    pub fn sync_keyboard_locks(&mut self, locks: KeyboardLocksSnapshot) -> bool {
        if self.keyboard_locks == Some(locks) {
            return false;
        }
        self.keyboard_locks = Some(locks);
        self.post(0.0, RuntimeEvent::KeyboardLocks(locks));
        true
    }

    /// Forget what was sent, so the next sync re-sends everything. Used after
    /// a reload, whose fresh config has not seen any state yet.
    pub fn reset_synced_state(&mut self) {
        self.display_state = None;
        self.input_state = None;
        self.keyboard_layout = None;
        self.keyboard_locks = None;
    }

    pub fn evaluate_window(
        &mut self,
        window: &WaylandWindowSnapshot,
        now_ms: u64,
    ) -> Result<DecorationEvaluationResult, RuntimeError> {
        self.evaluate(window, now_ms, false)
    }

    pub fn evaluate_window_preview(
        &mut self,
        window: &WaylandWindowSnapshot,
        now_ms: u64,
    ) -> Result<DecorationEvaluationResult, RuntimeError> {
        self.evaluate(window, now_ms, true)
    }

    fn evaluate(
        &mut self,
        window: &WaylandWindowSnapshot,
        now_ms: u64,
        preview: bool,
    ) -> Result<DecorationEvaluationResult, RuntimeError> {
        let request = RuntimeRequest::Decoration(DecorationRequest::Evaluate { window, preview });
        match self.request(now_ms as f64, request)? {
            RuntimeReply::Evaluation(result) => Ok(*result),
            RuntimeReply::Unhandled => StaticDecorationEvaluator.evaluate_window(window, now_ms),
            other => Err(mismatch("decoration evaluate", &other)),
        }
    }

    pub fn window_decoration_policy(
        &mut self,
        window: &WaylandWindowSnapshot,
        context: &WindowDecorationPolicyContextSnapshot,
    ) -> Result<WindowDecorationDecisionSnapshot, RuntimeError> {
        let request = RuntimeRequest::Decoration(DecorationRequest::Policy { window, context });
        match self.request(0.0, request)? {
            RuntimeReply::DecorationPolicy(decision) => Ok(decision),
            RuntimeReply::Unhandled => Ok(WindowDecorationDecisionSnapshot {
                mode: WindowDecorationModeSnapshot::Server,
            }),
            other => Err(mismatch("decoration policy", &other)),
        }
    }

    /// The legacy KDE decoration manager's default mode for `client`; CSD
    /// unless the config chooses.
    pub fn kde_default_decoration_mode(
        &mut self,
        client: &WindowDecorationClientSnapshot,
    ) -> Result<WindowDecorationModeSnapshot, RuntimeError> {
        let request = RuntimeRequest::Decoration(DecorationRequest::KdeDefaultMode { client });
        match self.request(0.0, request)? {
            RuntimeReply::DecorationPolicy(decision) => Ok(decision.mode),
            RuntimeReply::Unhandled => Ok(WindowDecorationModeSnapshot::Client),
            other => Err(mismatch("kde default decoration mode", &other)),
        }
    }

    pub fn evaluate_cached_window(
        &mut self,
        window_id: &str,
        window: Option<&WaylandWindowSnapshot>,
        now_ms: u64,
        force_full: bool,
    ) -> Result<DecorationCachedEvaluationResult, RuntimeError> {
        let request = RuntimeRequest::Decoration(DecorationRequest::EvaluateCached {
            window_id,
            window,
            force_full,
        });
        match self.request(now_ms as f64, request)? {
            RuntimeReply::CachedEvaluation(result) => Ok(*result),
            RuntimeReply::Evaluation(result) => Ok((*result).into()),
            RuntimeReply::Unhandled => match window {
                Some(window) => StaticDecorationEvaluator
                    .evaluate_window(window, now_ms)
                    .map(Into::into),
                None => Err(RuntimeError::Unsupported("cached window evaluation")),
            },
            other => Err(mismatch("cached decoration evaluate", &other)),
        }
    }

    /// A wall-clock timer tick with no output rendering frames: everything
    /// in the runtime advances to `now_ms`.
    pub fn scheduler_tick(&mut self, now_ms: f64) -> Result<DecorationSchedulerTick, RuntimeError> {
        self.scheduler_tick_with(
            now_ms,
            SchedulerTickRequest {
                output: None,
                frame_outputs: &[],
            },
        )
    }

    pub fn scheduler_tick_with(
        &mut self,
        now_ms: f64,
        tick: SchedulerTickRequest<'_>,
    ) -> Result<DecorationSchedulerTick, RuntimeError> {
        match self.request(now_ms, RuntimeRequest::SchedulerTick(tick))? {
            RuntimeReply::SchedulerTick(tick) => Ok(tick),
            RuntimeReply::Unhandled | RuntimeReply::Done => Ok(DecorationSchedulerTick::default()),
            other => Err(mismatch("scheduler tick", &other)),
        }
    }

    pub fn window_closed(&mut self, window_id: &str) -> Result<(), RuntimeError> {
        let request = RuntimeRequest::Decoration(DecorationRequest::Closed { window_id });
        match self.request(0.0, request)? {
            RuntimeReply::Done | RuntimeReply::Unhandled => Ok(()),
            other => Err(mismatch("window closed", &other)),
        }
    }

    fn handler_reply(
        &mut self,
        what: &str,
        now_ms: u64,
        request: RuntimeRequest<'_>,
    ) -> Result<DecorationHandlerInvocation, RuntimeError> {
        match self.request(now_ms as f64, request)? {
            RuntimeReply::Handler(invocation) => Ok(*invocation),
            RuntimeReply::Unhandled | RuntimeReply::Done => {
                Ok(DecorationHandlerInvocation::default())
            }
            other => Err(mismatch(what, &other)),
        }
    }

    pub fn invoke_handler(
        &mut self,
        window_id: &str,
        handler_id: &str,
        now_ms: u64,
    ) -> Result<DecorationHandlerInvocation, RuntimeError> {
        let request = RuntimeRequest::Decoration(DecorationRequest::InvokeHandler {
            window_id,
            handler_id,
        });
        self.handler_reply("decoration handler", now_ms, request)
    }

    pub fn start_close(
        &mut self,
        window_id: &str,
        now_ms: u64,
    ) -> Result<DecorationHandlerInvocation, RuntimeError> {
        let request = RuntimeRequest::Decoration(DecorationRequest::StartClose { window_id });
        self.handler_reply("start close", now_ms, request)
    }

    pub fn workspace_activate(
        &mut self,
        event: &RuntimeWorkspaceActivateRequestSnapshot,
        now_ms: u64,
    ) -> Result<DecorationHandlerInvocation, RuntimeError> {
        let request = RuntimeRequest::Workspace(WorkspaceRequest::Activate(event));
        self.handler_reply("workspace activate", now_ms, request)
    }

    pub fn invoke_key_binding(
        &mut self,
        binding_id: &str,
        now_ms: u64,
    ) -> Result<DecorationKeyBindingInvocation, RuntimeError> {
        let request = RuntimeRequest::Input(InputRequest::KeyBinding { binding_id });
        match self.request(now_ms as f64, request)? {
            RuntimeReply::KeyBinding(invocation) => Ok(invocation),
            RuntimeReply::Unhandled | RuntimeReply::Done => {
                Ok(DecorationKeyBindingInvocation::default())
            }
            other => Err(mismatch("key binding", &other)),
        }
    }

    pub fn window_resize(
        &mut self,
        window_id: &str,
        event: &WindowResizeEventSnapshot,
        now_ms: u64,
    ) -> Result<DecorationWindowResizeInvocation, RuntimeError> {
        let request = RuntimeRequest::Window(WindowRequest::Resize { window_id, event });
        match self.request(now_ms as f64, request)? {
            RuntimeReply::WindowResize(invocation) => Ok(invocation),
            RuntimeReply::Unhandled | RuntimeReply::Done => {
                Ok(DecorationWindowResizeInvocation::default())
            }
            other => Err(mismatch("window resize", &other)),
        }
    }

    pub fn window_move(
        &mut self,
        window_id: &str,
        event: &WindowMoveEventSnapshot,
        now_ms: u64,
    ) -> Result<DecorationWindowMoveInvocation, RuntimeError> {
        let request = RuntimeRequest::Window(WindowRequest::Move { window_id, event });
        match self.request(now_ms as f64, request)? {
            RuntimeReply::WindowMove(invocation) => Ok(invocation),
            RuntimeReply::Unhandled | RuntimeReply::Done => {
                Ok(DecorationWindowMoveInvocation::default())
            }
            other => Err(mismatch("window move", &other)),
        }
    }

    fn window_state_request(
        &mut self,
        what: &str,
        now_ms: u64,
        request: WindowRequest<'_>,
    ) -> Result<DecorationWindowStateRequestInvocation, RuntimeError> {
        match self.request(now_ms as f64, RuntimeRequest::Window(request))? {
            RuntimeReply::WindowStateRequest(invocation) => Ok(invocation),
            RuntimeReply::Unhandled | RuntimeReply::Done => {
                Ok(DecorationWindowStateRequestInvocation::default())
            }
            other => Err(mismatch(what, &other)),
        }
    }

    pub fn window_maximize_request(
        &mut self,
        window: &WaylandWindowSnapshot,
        event: &WindowMaximizeRequestEventSnapshot,
        now_ms: u64,
    ) -> Result<DecorationWindowStateRequestInvocation, RuntimeError> {
        let request = WindowRequest::Maximize { window, event };
        self.window_state_request("window maximize request", now_ms, request)
    }

    pub fn window_minimize_request(
        &mut self,
        window: &WaylandWindowSnapshot,
        event: &WindowMinimizeRequestEventSnapshot,
        now_ms: u64,
    ) -> Result<DecorationWindowStateRequestInvocation, RuntimeError> {
        let request = WindowRequest::Minimize { window, event };
        self.window_state_request("window minimize request", now_ms, request)
    }

    pub fn window_fullscreen_request(
        &mut self,
        window: &WaylandWindowSnapshot,
        event: &WindowFullscreenRequestEventSnapshot,
        now_ms: u64,
    ) -> Result<DecorationWindowStateRequestInvocation, RuntimeError> {
        let request = WindowRequest::Fullscreen { window, event };
        self.window_state_request("window fullscreen request", now_ms, request)
    }

    pub fn window_activate_request(
        &mut self,
        window: &WaylandWindowSnapshot,
        event: &WindowActivateRequestEventSnapshot,
        now_ms: u64,
    ) -> Result<DecorationWindowStateRequestInvocation, RuntimeError> {
        let request = WindowRequest::Activate { window, event };
        self.window_state_request("window activate request", now_ms, request)
    }

    fn pointer_hook_reply(
        &mut self,
        what: &str,
        now_ms: u64,
        request: InputRequest<'_>,
    ) -> Result<DecorationPointerMoveAsyncInvocation, RuntimeError> {
        match self.request(now_ms as f64, RuntimeRequest::Input(request))? {
            RuntimeReply::PointerHook(invocation) => Ok(invocation),
            RuntimeReply::Unhandled | RuntimeReply::Done => {
                Ok(DecorationPointerMoveAsyncInvocation::default())
            }
            other => Err(mismatch(what, &other)),
        }
    }

    pub fn pointer_move(
        &mut self,
        event: &PointerMoveEventSnapshot,
        now_ms: u64,
    ) -> Result<DecorationPointerMoveAsyncInvocation, RuntimeError> {
        self.pointer_hook_reply("pointer move", now_ms, InputRequest::PointerMove(event))
    }

    pub fn pointer_move_async(&mut self, event: PointerMoveEventSnapshot, now_ms: u64) {
        self.post(now_ms as f64, RuntimeEvent::PointerMove(event));
    }

    pub fn gesture_swipe(
        &mut self,
        event: &GestureSwipeEventSnapshot,
        now_ms: u64,
    ) -> Result<DecorationGestureSwipeAsyncInvocation, RuntimeError> {
        self.pointer_hook_reply("gesture swipe", now_ms, InputRequest::GestureSwipe(event))
    }

    pub fn gesture_swipe_async(&mut self, event: GestureSwipeEventSnapshot, now_ms: u64) {
        self.post(now_ms as f64, RuntimeEvent::GestureSwipe(event));
    }

    pub fn gesture_pinch_async(
        &mut self,
        event: crate::ssd::GesturePinchEventSnapshot,
        now_ms: u64,
    ) {
        self.post(now_ms as f64, RuntimeEvent::GesturePinch(event));
    }

    pub fn pointer_region(
        &mut self,
        event: &crate::runtime_pointer_region::PointerRegionEventSnapshot,
        now_ms: u64,
    ) -> Result<DecorationPointerMoveAsyncInvocation, RuntimeError> {
        self.pointer_hook_reply("pointer region", now_ms, InputRequest::PointerRegion(event))
    }

    pub fn dnd_event(
        &mut self,
        event: &crate::dnd::DndEventSnapshot,
        now_ms: u64,
    ) -> Result<DecorationPointerMoveAsyncInvocation, RuntimeError> {
        self.pointer_hook_reply("drag and drop", now_ms, InputRequest::Dnd(event))
    }

    pub fn input_grab_event(
        &mut self,
        grab_id: u64,
        event: &crate::runtime_input_grab::InputGrabEventSnapshot,
        now_ms: u64,
    ) -> Result<DecorationPointerMoveAsyncInvocation, RuntimeError> {
        self.pointer_hook_reply(
            "input grab",
            now_ms,
            InputRequest::InputGrab { grab_id, event },
        )
    }

    pub fn background_effect_config(
        &mut self,
    ) -> Result<Option<BackgroundEffectConfig>, RuntimeError> {
        match self.request(0.0, RuntimeRequest::Effect(EffectRequest::Background))? {
            RuntimeReply::BackgroundEffect(config) => Ok(config),
            RuntimeReply::Unhandled | RuntimeReply::Done => Ok(None),
            other => Err(mismatch("background effect", &other)),
        }
    }

    pub fn evaluate_layer_effects(
        &mut self,
        output_name: &str,
        layers: &[WaylandLayerSnapshot],
        now_ms: u64,
    ) -> Result<LayerEffectEvaluationResult, RuntimeError> {
        let request = RuntimeRequest::Effect(EffectRequest::Layers {
            output_name,
            layers,
        });
        match self.request(now_ms as f64, request)? {
            RuntimeReply::LayerEffects(result) => Ok(result),
            RuntimeReply::Unhandled | RuntimeReply::Done => {
                Ok(LayerEffectEvaluationResult::default())
            }
            other => Err(mismatch("layer effects", &other)),
        }
    }

    pub fn evaluate_popup_effects(
        &mut self,
        output_name: &str,
        popups: &[WaylandPopupSnapshot],
        now_ms: u64,
    ) -> Result<PopupEffectEvaluationResult, RuntimeError> {
        let request = RuntimeRequest::Effect(EffectRequest::Popups {
            output_name,
            popups,
        });
        match self.request(now_ms as f64, request)? {
            RuntimeReply::PopupEffects(result) => Ok(result),
            RuntimeReply::Unhandled | RuntimeReply::Done => {
                Ok(PopupEffectEvaluationResult::default())
            }
            other => Err(mismatch("popup effects", &other)),
        }
    }
}
