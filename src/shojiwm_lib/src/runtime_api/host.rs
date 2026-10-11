//! Runtime → compositor channel.
//!
//! Everything a config runtime wants the compositor to *do* (as opposed to the
//! direct answer of a request) travels through [`RuntimeHost`] as a
//! [`HostMessage`]. The compositor drains the queue right after every
//! `ConfigRuntime::request`, which keeps the ordering the old in-band
//! piggyback had (a key binding update lands before the actions of the same
//! reply), and from a calloop ping when a runtime thread sends something on
//! its own (IPC, async pointer hooks).

use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use smithay::reexports::calloop::ping::Ping;

use crate::{
    activation_environment::RuntimeEnvUpdates,
    config::RuntimeDisplayConfigUpdate,
    cursor::RuntimeCursorConfigUpdate,
    output_power::RuntimeOutputPowerRequest,
    runtime_debug::RuntimeDebugConfigUpdate,
    runtime_input::RuntimeInputConfigUpdate,
    runtime_key_binding::RuntimeKeyBindingConfigUpdate,
    runtime_pointer::RuntimePointerConfigUpdate,
    runtime_process::{RuntimeProcessAction, RuntimeProcessConfigUpdate},
    runtime_workspace::RuntimeWorkspaceConfigUpdate,
    ssd::{DecorationPointerMoveAsyncInvocation, RuntimeEventConfigUpdate},
};

/// A side effect requested by the config runtime.
#[derive(Debug, Clone)]
pub enum HostMessage {
    /// Environment changes for spawned processes (`COMPOSITOR.env`).
    Env(RuntimeEnvUpdates),
    Display(RuntimeDisplayConfigUpdate),
    Workspace(RuntimeWorkspaceConfigUpdate),
    KeyBindings(RuntimeKeyBindingConfigUpdate),
    Pointer(RuntimePointerConfigUpdate),
    Input(RuntimeInputConfigUpdate),
    /// Which optional input hooks the runtime wants to receive.
    EventFilter(RuntimeEventConfigUpdate),
    Process(RuntimeProcessConfigUpdate),
    ProcessActions(Vec<RuntimeProcessAction>),
    Debug(RuntimeDebugConfigUpdate),
    Cursor(RuntimeCursorConfigUpdate),
    /// Switch outputs on or off (`COMPOSITOR.output.setPower`).
    OutputPower(RuntimeOutputPowerRequest),
    /// Result of a posted (fire-and-forget) pointer or gesture hook.
    PointerHookResult(DecorationPointerMoveAsyncInvocation),
    /// A hot reload prepared in the background is done. `Ok` makes the
    /// compositor call `ConfigRuntime::reload` at the next quiet point of its
    /// loop; `Err` is shown as a hot reload error and the current config
    /// stays. A runtime may also send it unprompted, e.g. from a file
    /// watcher, to start a reload on its own.
    ReloadReady(Result<(), String>),
    /// When the runtime's scheduled work next comes due; sent whenever that
    /// changes. A runtime that never sends it keeps a single clock and is
    /// ticked on every rendered frame and timer (see `SchedulerTickRequest`).
    Schedule(RuntimeSchedule),
    /// The frame pacing the config chose per output (`COMPOSITOR.rendering.framePacing`).
    FramePacing(crate::frame_pacing::FramePacingConfig),
    /// Every output's custom composition (`COMPOSITOR.rendering.composition`).
    /// Replaces the previous set: outputs missing from it use the default
    /// stacking again.
    OutputCompositions(
        std::collections::HashMap<String, crate::backend::composition::OutputComposition>,
    ),
    /// Start or end the config's input grab (`COMPOSITOR.input.grab`). While
    /// it is held, input arrives as `InputRequest::InputGrab`.
    InputGrab(crate::runtime_input_grab::RuntimeInputGrabUpdate),
    /// `COMPOSITOR.effect.background_effect` changed after it was first read
    /// (one of its signals moved). Replaces the effect; `None` turns it off.
    BackgroundEffect(Option<crate::ssd::BackgroundEffectConfig>),
    /// Move the cursor (`COMPOSITOR.cursor.setPosition`).
    CursorPosition(crate::cursor::RuntimeCursorPositionTarget),
}

/// When a runtime that keeps a clock per output (see `SchedulerTickRequest`)
/// next needs a tick. All times are absolute, in monotonic milliseconds.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RuntimeSchedule {
    /// Per frame-rendering output, the earliest due time on that output's
    /// clock; `0` while something on it animates (every frame). An output
    /// missing from the map has nothing scheduled.
    pub output_due_ms: std::collections::HashMap<String, f64>,
    /// The earliest due time of work only a wall-clock timer tick runs.
    pub timer_due_ms: Option<f64>,
}

/// Config deltas that every runtime reply may carry. A runtime implementation
/// can collect them here and [`publish`](Self::publish) them in one go; the
/// order matches the order the compositor has always applied them in.
#[derive(Debug, Clone, Default)]
pub struct RuntimeConfigDelta {
    pub display_config: Option<RuntimeDisplayConfigUpdate>,
    pub workspace_config: Option<RuntimeWorkspaceConfigUpdate>,
    pub key_binding_config: Option<RuntimeKeyBindingConfigUpdate>,
    pub pointer_config: Option<RuntimePointerConfigUpdate>,
    pub input_config: Option<RuntimeInputConfigUpdate>,
    pub event_config: Option<RuntimeEventConfigUpdate>,
    pub process_config: Option<RuntimeProcessConfigUpdate>,
    pub debug_config: Option<RuntimeDebugConfigUpdate>,
    pub process_actions: Vec<RuntimeProcessAction>,
}

impl RuntimeConfigDelta {
    pub fn is_empty(&self) -> bool {
        self.display_config.is_none()
            && self.workspace_config.is_none()
            && self.key_binding_config.is_none()
            && self.pointer_config.is_none()
            && self.input_config.is_none()
            && self.event_config.is_none()
            && self.process_config.is_none()
            && self.debug_config.is_none()
            && self.process_actions.is_empty()
    }

    pub fn publish(self, host: &RuntimeHost) {
        if self.is_empty() {
            return;
        }
        let mut messages = Vec::new();
        messages.extend(self.display_config.map(HostMessage::Display));
        messages.extend(self.workspace_config.map(HostMessage::Workspace));
        messages.extend(self.key_binding_config.map(HostMessage::KeyBindings));
        messages.extend(self.pointer_config.map(HostMessage::Pointer));
        messages.extend(self.input_config.map(HostMessage::Input));
        messages.extend(self.event_config.map(HostMessage::EventFilter));
        messages.extend(self.process_config.map(HostMessage::Process));
        messages.extend(self.debug_config.map(HostMessage::Debug));
        if !self.process_actions.is_empty() {
            messages.push(HostMessage::ProcessActions(self.process_actions));
        }
        host.send_all(messages);
    }
}

/// Handle a runtime uses to talk back to the compositor. Cheap to clone and
/// usable from any thread.
#[derive(Clone, Default)]
pub struct RuntimeHost {
    inner: Arc<HostInner>,
}

#[derive(Default)]
struct HostInner {
    queue: Mutex<VecDeque<HostMessage>>,
    wake_requested: AtomicBool,
    ping: Mutex<Option<Ping>>,
    cursor_position: Mutex<Option<crate::cursor::CursorPositionSnapshot>>,
}

impl std::fmt::Debug for RuntimeHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeHost").finish_non_exhaustive()
    }
}

impl RuntimeHost {
    /// A host that is not wired to an event loop yet. Messages queue up until
    /// someone drains them; tests use it as is.
    pub fn detached() -> Self {
        Self::default()
    }

    /// Wake `ping` whenever a message arrives or the runtime asks for a tick.
    pub fn attach_ping(&self, ping: Ping) {
        if let Ok(mut slot) = self.inner.ping.lock() {
            *slot = Some(ping);
        }
    }

    pub fn send(&self, message: HostMessage) {
        self.send_all([message]);
    }

    pub fn send_all(&self, messages: impl IntoIterator<Item = HostMessage>) {
        let mut sent = false;
        if let Ok(mut queue) = self.inner.queue.lock() {
            for message in messages {
                queue.push_back(message);
                sent = true;
            }
        }
        if sent {
            self.ping();
        }
    }

    /// Ask the compositor to run a scheduler tick soon, e.g. after the runtime
    /// changed state on its own (IPC request, timer on its thread).
    pub fn wake(&self) {
        self.inner.wake_requested.store(true, Ordering::Release);
        self.ping();
    }

    /// Wake the compositor's event loop without asking for a scheduler tick,
    /// for compositor-side state such as output overlays.
    pub fn notify(&self) {
        self.ping();
    }

    /// Record where the cursor is, for `COMPOSITOR.cursor.getPosition`.
    /// Compositor side, after every pointer motion.
    pub fn set_cursor_position(&self, position: crate::cursor::CursorPositionSnapshot) {
        if let Ok(mut slot) = self.inner.cursor_position.lock() {
            *slot = Some(position);
        }
    }

    /// The cursor as of the last pointer motion; `None` before the first one.
    /// Readable from the runtime's own thread.
    pub fn cursor_position(&self) -> Option<crate::cursor::CursorPositionSnapshot> {
        self.inner.cursor_position.lock().ok()?.clone()
    }

    /// Next queued message, oldest first. Compositor side.
    pub fn pop(&self) -> Option<HostMessage> {
        self.inner.queue.lock().ok()?.pop_front()
    }

    /// Whether [`wake`](Self::wake) was called since the last check.
    /// Compositor side.
    pub fn take_wake_request(&self) -> bool {
        self.inner.wake_requested.swap(false, Ordering::AcqRel)
    }

    fn ping(&self) {
        if let Ok(slot) = self.inner.ping.lock()
            && let Some(ping) = slot.as_ref()
        {
            ping.ping();
        }
    }
}
