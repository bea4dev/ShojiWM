//! Thread-local state of a running Rust config: registered callbacks,
//! per-window entries and the queues the adapter drains into replies.
//!
//! Rule of thumb for this module: never call user code (listeners,
//! composition functions, signal writes) while a `RefCell` here is borrowed.
//! Clone the `Rc`s out first.

use std::{
    any::{Any, TypeId},
    cell::{Cell, RefCell},
    collections::{BTreeMap, BTreeSet, HashMap},
    rc::Rc,
};

use shojiwm_lib::{
    activation_environment::{RuntimeEnvOperation, RuntimeEnvUpdates},
    frame_pacing::FramePacing,
    config::RuntimeOutputConfig,
    cursor::RuntimeCursorConfigUpdate,
    keyboard_layout::KeyboardLayoutSnapshot,
    runtime_api::{HostMessage, RuntimeHost},
    runtime_debug::RuntimeDebugConfigUpdate,
    runtime_input::{RuntimeInputConfig, RuntimeInputDeviceSnapshot},
    runtime_key_binding::RuntimeKeyBindingEntry,
    runtime_process::{RuntimeProcessAction, RuntimeProcessEntry},
    runtime_workspace::{RuntimeWorkspaceActivateRequestSnapshot, RuntimeWorkspaceConfigUpdate},
    ssd::{
        GestureSwipeEventSnapshot, ManagedWindowState, PointerMoveEventSnapshot,
        RuntimeWindowAction, SurfacePolicy, WaylandLayerSnapshot, WaylandOutputSnapshot,
        WaylandPopupSnapshot, WaylandWindowSnapshot, WindowActivateRequestEventSnapshot,
        WindowDecorationModeSnapshot, WindowDecorationPolicyContextSnapshot, WindowEffectConfig,
        WindowFullscreenRequestEventSnapshot, WindowMaximizeRequestEventSnapshot,
        WindowMinimizeRequestEventSnapshot, WindowMoveEventSnapshot, WindowResizeEventSnapshot,
        WindowTransform,
    },
};

use crate::{
    animation::TimerHandle,
    compositor::{
        DisableEvent, EnableEvent, InputChangeEvent, OutputChangeEvent, SurfaceRef,
    },
    effect::{Effect, SurfaceEffects},
    output_composition::OutputStack,
    reactive::{Observer, Scope, Signal},
    view::{Composition, ManagedWindow, ViewContext, ViewTree},
    window::{Window, WindowSignals},
};

pub(crate) type Listener<A> = Rc<dyn Fn(A)>;
/// A listener taking its event by reference.
pub(crate) type RefListener<E> = Rc<dyn Fn(&E)>;
/// A listener for an event about one window.
pub(crate) type WindowListener<E> = Rc<dyn Fn(Window, &E)>;

#[derive(Default)]
pub(crate) struct Listeners {
    pub enable: Vec<RefListener<EnableEvent>>,
    pub disable: Vec<RefListener<DisableEvent>>,
    pub open: Vec<Listener<Window>>,
    pub initial_configure: Vec<Listener<Window>>,
    pub first_commit: Vec<Listener<Window>>,
    pub close: Vec<Listener<Window>>,
    pub start_close: Vec<Listener<Window>>,
    pub focus: Vec<Rc<dyn Fn(Window, bool)>>,
    pub window_resize: Vec<WindowListener<WindowResizeEventSnapshot>>,
    pub window_move: Vec<WindowListener<WindowMoveEventSnapshot>>,
    pub maximize_request: Vec<WindowListener<WindowMaximizeRequestEventSnapshot>>,
    pub minimize_request: Vec<WindowListener<WindowMinimizeRequestEventSnapshot>>,
    pub fullscreen_request: Vec<WindowListener<WindowFullscreenRequestEventSnapshot>>,
    pub activate_request: Vec<WindowListener<WindowActivateRequestEventSnapshot>>,
    pub output_change: Vec<RefListener<OutputChangeEvent>>,
    pub input_change: Vec<RefListener<InputChangeEvent>>,
    pub keyboard_layout: Vec<RefListener<KeyboardLayoutSnapshot>>,
    pub pointer_move: Vec<RefListener<PointerMoveEventSnapshot>>,
    pub pointer_move_async: Vec<RefListener<PointerMoveEventSnapshot>>,
    pub gesture_swipe: Vec<RefListener<GestureSwipeEventSnapshot>>,
    pub gesture_swipe_async: Vec<RefListener<GestureSwipeEventSnapshot>>,
    pub create_layer: Vec<RefListener<WaylandLayerSnapshot>>,
    pub update_layer: Vec<RefListener<WaylandLayerSnapshot>>,
    pub destroy_layer: Vec<RefListener<WaylandLayerSnapshot>>,
    pub workspace_activate: Vec<RefListener<RuntimeWorkspaceActivateRequestSnapshot>>,
}

pub(crate) type EffectFn<S> = Rc<dyn Fn(&S) -> SurfaceEffects>;
/// A value the config computes for each output.
pub(crate) type PerOutputFn<T> = Rc<dyn Fn(&WaylandOutputSnapshot) -> T>;
pub(crate) type SurfacePolicyFn = Rc<dyn Fn(SurfaceRef<'_>) -> Option<SurfacePolicy>>;
/// Per-window state signals, keyed by state key name and value type.
pub(crate) type WindowStateSlots = HashMap<(&'static str, TypeId), Rc<dyn Any>>;
pub(crate) type CompositionFn = Rc<dyn Fn(Window) -> Composition>;
pub(crate) type DecorationPolicyFn =
    Rc<dyn Fn(&WaylandWindowSnapshot, &WindowDecorationPolicyContextSnapshot) -> WindowDecorationModeSnapshot>;
pub(crate) type OutputConfigureFn =
    Rc<dyn Fn(&crate::compositor::OutputContext) -> BTreeMap<String, Option<RuntimeOutputConfig>>>;
pub(crate) type InputConfigureFn =
    Rc<dyn Fn(&mut RuntimeInputConfig, &BTreeMap<String, RuntimeInputDeviceSnapshot>)>;
pub(crate) type WorkspaceConfigureFn = Rc<dyn Fn() -> RuntimeWorkspaceConfigUpdate>;

/// Which config domains changed since the last publish.
#[derive(Default)]
pub(crate) struct Pending {
    pub key_bindings: bool,
    pub pointer: bool,
    pub processes: bool,
    pub event_filter: bool,
    pub debug: bool,
    pub display: Option<BTreeMap<String, Option<RuntimeOutputConfig>>>,
    pub input: Option<RuntimeInputConfig>,
    pub workspace: Option<RuntimeWorkspaceConfigUpdate>,
    pub process_actions: Vec<RuntimeProcessAction>,
    pub env_operations: Vec<RuntimeEnvOperation>,
    pub env_publish: BTreeSet<String>,
    pub cursor: Option<RuntimeCursorConfigUpdate>,
}

#[derive(Default)]
pub(crate) struct Registry {
    pub listeners: Listeners,
    pub composition: Option<CompositionFn>,
    pub decoration_policy: Option<DecorationPolicyFn>,
    pub key_bindings: Vec<RuntimeKeyBindingEntry>,
    pub key_handlers: HashMap<String, Rc<dyn Fn()>>,
    pub window_move_modifier: Option<String>,
    pub window_resize_modifier: Option<String>,
    pub processes: Vec<RuntimeProcessEntry>,
    pub debug: RuntimeDebugConfigUpdate,
    pub env: BTreeMap<String, String>,
    pub output_configure: Option<OutputConfigureFn>,
    pub desired_outputs: Option<BTreeMap<String, Option<RuntimeOutputConfig>>>,
    pub input_configure: Option<InputConfigureFn>,
    pub desired_input: Option<RuntimeInputConfig>,
    pub workspace_configure: Option<WorkspaceConfigureFn>,
    pub desired_workspaces: Option<RuntimeWorkspaceConfigUpdate>,
    pub background_effect: Option<Rc<dyn Fn() -> Option<Effect>>>,
    pub frame_pacing: Option<PerOutputFn<FramePacing>>,
    pub output_composition: Option<PerOutputFn<OutputStack>>,
    pub layer_effect: Option<EffectFn<WaylandLayerSnapshot>>,
    pub popup_effect: Option<EffectFn<WaylandPopupSnapshot>>,
    pub window_effect: Option<Rc<dyn Fn(Window) -> SurfaceEffects>>,
    pub surface_policy: Option<SurfacePolicyFn>,
    pub channels: Vec<Rc<dyn Fn() -> bool>>,
    pub pending: Pending,
}

thread_local! {
    pub(crate) static REGISTRY: RefCell<Registry> = RefCell::new(Registry::default());
    static HOST: RefCell<Option<RuntimeHost>> = const { RefCell::new(None) };
    static WAYLAND_DISPLAY: RefCell<Option<String>> = const { RefCell::new(None) };
    static ACTIONS: RefCell<Vec<RuntimeWindowAction>> = const { RefCell::new(Vec::new()) };
    static DIRTY_WINDOWS: RefCell<BTreeSet<String>> = const { RefCell::new(BTreeSet::new()) };
    static LAYER_EFFECTS_DIRTY: Cell<bool> = const { Cell::new(false) };
    pub(crate) static WINDOWS: RefCell<Windows> = RefCell::new(Windows::default());
    pub(crate) static GLOBAL: RefCell<Option<GlobalSignals>> = const { RefCell::new(None) };
}

/// Compositor-wide reactive state, created when the runtime starts.
#[derive(Clone, Copy)]
pub(crate) struct GlobalSignals {
    pub scope: Scope,
    pub outputs: Signal<BTreeMap<String, WaylandOutputSnapshot>>,
    pub inputs: Signal<BTreeMap<String, RuntimeInputDeviceSnapshot>>,
    pub layers: Signal<BTreeMap<String, WaylandLayerSnapshot>>,
    pub keyboard_layout: Signal<Option<KeyboardLayoutSnapshot>>,
}

pub(crate) fn global() -> GlobalSignals {
    GLOBAL.with(|global| {
        *global.borrow_mut().get_or_insert_with(|| {
            let scope = Scope::root();
            GlobalSignals {
                scope,
                outputs: scope.signal(BTreeMap::new()),
                inputs: scope.signal(BTreeMap::new()),
                layers: scope.signal(BTreeMap::new()),
                keyboard_layout: scope.signal(None),
            }
        })
    })
}

pub(crate) fn with_registry<R>(f: impl FnOnce(&mut Registry) -> R) -> R {
    REGISTRY.with(|registry| f(&mut registry.borrow_mut()))
}

pub(crate) fn set_host(host: RuntimeHost) {
    HOST.with(|slot| *slot.borrow_mut() = Some(host));
}

pub(crate) fn set_wayland_display(display: Option<String>) {
    WAYLAND_DISPLAY.with(|slot| *slot.borrow_mut() = display);
}

/// The compositor's own Wayland socket name, from [`LaunchContext::wayland_display`].
///
/// [`LaunchContext::wayland_display`]: shojiwm_lib::runtime_api::LaunchContext::wayland_display
pub(crate) fn wayland_display() -> Option<String> {
    WAYLAND_DISPLAY.with(|slot| slot.borrow().clone())
}

pub(crate) fn host() -> Option<RuntimeHost> {
    HOST.with(|slot| slot.borrow().clone())
}

pub(crate) fn push_action(action: RuntimeWindowAction) {
    ACTIONS.with(|actions| actions.borrow_mut().push(action));
}

pub(crate) fn has_pending_actions() -> bool {
    ACTIONS.with(|actions| !actions.borrow().is_empty())
}

pub(crate) fn drain_actions() -> Vec<RuntimeWindowAction> {
    ACTIONS.with(|actions| std::mem::take(&mut *actions.borrow_mut()))
}

pub(crate) fn drain_actions_for(window_id: &str) -> Vec<RuntimeWindowAction> {
    ACTIONS.with(|actions| {
        let mut actions = actions.borrow_mut();
        let (mine, rest): (Vec<_>, Vec<_>) = actions
            .drain(..)
            .partition(|action| action.window_id == window_id);
        *actions = rest;
        mine
    })
}

pub(crate) fn mark_window_dirty(window_id: &str) {
    DIRTY_WINDOWS.with(|dirty| {
        dirty.borrow_mut().insert(window_id.to_owned());
    });
}

pub(crate) fn take_dirty_windows() -> BTreeSet<String> {
    DIRTY_WINDOWS.with(|dirty| std::mem::take(&mut *dirty.borrow_mut()))
}

pub(crate) fn mark_layer_effects_dirty() {
    LAYER_EFFECTS_DIRTY.with(|dirty| dirty.set(true));
}

pub(crate) fn take_layer_effects_dirty() -> bool {
    LAYER_EFFECTS_DIRTY.with(|dirty| dirty.replace(false))
}

/// Everything the config keeps per window.
pub(crate) struct WindowEntry {
    pub id: Rc<str>,
    pub handle: Window,
    pub scope: Scope,
    pub signals: WindowSignals,
    pub snapshot: RefCell<WaylandWindowSnapshot>,
    pub state: RefCell<WindowStateSlots>,
    pub context: ViewContext,
    pub view: RefCell<Option<ComposedView>>,
    pub managed_state: RefCell<ManagedWindowState>,
    pub window_effects: RefCell<Option<WindowEffectConfig>>,
    pub effects_observer: Observer,
    pub close_duration_ms: Cell<u64>,
    pub close_started: Cell<bool>,
    pub close_timer: Cell<Option<TimerHandle>>,
    pub opened: Cell<bool>,
    pub initial_configured: Cell<bool>,
    pub first_committed: Cell<bool>,
    pub preconfigured: Cell<bool>,
}

pub(crate) struct ComposedView {
    pub scope: Scope,
    pub root_observer: Observer,
    pub tree: ViewTree,
    pub managed: Option<(Box<ManagedWindow>, Observer)>,
}

impl WindowEntry {
    pub fn transform(&self) -> WindowTransform {
        self.managed_state.borrow().transform
    }
}

#[derive(Default)]
pub(crate) struct Windows {
    slots: Vec<Option<Rc<WindowEntry>>>,
    generations: Vec<u32>,
    free: Vec<u32>,
    by_id: HashMap<Rc<str>, u32>,
    /// Ids in the order windows were first seen.
    pub order: Vec<Rc<str>>,
}

impl Windows {
    pub fn reserve(&mut self) -> (u32, u32) {
        if let Some(slot) = self.free.pop() {
            (slot, self.generations[slot as usize])
        } else {
            self.slots.push(None);
            self.generations.push(0);
            ((self.slots.len() - 1) as u32, 0)
        }
    }

    pub fn insert(&mut self, slot: u32, entry: Rc<WindowEntry>) {
        self.by_id.insert(entry.id.clone(), slot);
        self.order.push(entry.id.clone());
        self.slots[slot as usize] = Some(entry);
    }

    pub fn get(&self, slot: u32, generation: u32) -> Option<Rc<WindowEntry>> {
        if self.generations.get(slot as usize) != Some(&generation) {
            return None;
        }
        self.slots[slot as usize].clone()
    }

    pub fn by_id(&self, id: &str) -> Option<Rc<WindowEntry>> {
        let slot = *self.by_id.get(id)?;
        self.slots[slot as usize].clone()
    }

    pub fn remove(&mut self, id: &str) -> Option<Rc<WindowEntry>> {
        let slot = self.by_id.remove(id)?;
        self.order.retain(|candidate| &**candidate != id);
        let entry = self.slots[slot as usize].take();
        self.generations[slot as usize] = self.generations[slot as usize].wrapping_add(1);
        self.free.push(slot);
        entry
    }

    pub fn clear(&mut self) {
        let ids: Vec<Rc<str>> = self.by_id.keys().cloned().collect();
        for id in ids {
            self.remove(&id);
        }
    }

    pub fn all(&self) -> Vec<Rc<WindowEntry>> {
        self.order
            .iter()
            .filter_map(|id| self.by_id(id))
            .collect()
    }
}

pub(crate) fn window_entry(id: &str) -> Option<Rc<WindowEntry>> {
    WINDOWS.with(|windows| windows.borrow().by_id(id))
}

pub(crate) fn all_windows() -> Vec<Rc<WindowEntry>> {
    WINDOWS.with(|windows| windows.borrow().all())
}

/// Run a list of listeners, cloned out of the registry first.
pub(crate) fn emit<L: ?Sized>(select: impl FnOnce(&Listeners) -> Vec<Rc<L>>, call: impl Fn(&L)) -> bool {
    let listeners = with_registry(|registry| select(&registry.listeners));
    for listener in &listeners {
        call(listener);
    }
    !listeners.is_empty()
}

/// Send `message` to the compositor right away.
pub(crate) fn send(message: HostMessage) {
    if let Some(host) = host() {
        host.send(message);
    }
}

pub(crate) fn send_all(messages: Vec<HostMessage>) {
    if let Some(host) = host() {
        host.send_all(messages);
    }
}

/// Publish every config domain that changed, in the compositor's canonical
/// order. Called before a reply is returned.
pub(crate) fn publish_pending() {
    let messages = with_registry(|registry| {
        let pending = std::mem::take(&mut registry.pending);
        let mut messages = Vec::new();
        if !pending.env_operations.is_empty() || !pending.env_publish.is_empty() {
            messages.push(HostMessage::Env(RuntimeEnvUpdates {
                operations: pending.env_operations,
                publish: pending.env_publish.into_iter().collect(),
            }));
        }
        if let Some(cursor) = pending.cursor {
            messages.push(HostMessage::Cursor(cursor));
        }
        if let Some(outputs) = pending.display {
            messages.push(HostMessage::Display(
                shojiwm_lib::config::RuntimeDisplayConfigUpdate { outputs },
            ));
        }
        if let Some(workspace) = pending.workspace {
            messages.push(HostMessage::Workspace(workspace));
        }
        if pending.key_bindings {
            messages.push(HostMessage::KeyBindings(
                shojiwm_lib::runtime_key_binding::RuntimeKeyBindingConfigUpdate {
                    entries: registry.key_bindings.clone(),
                },
            ));
        }
        if pending.pointer {
            messages.push(HostMessage::Pointer(
                shojiwm_lib::runtime_pointer::RuntimePointerConfigUpdate {
                    window_move_modifier: registry.window_move_modifier.clone(),
                    window_resize_modifier: registry.window_resize_modifier.clone(),
                },
            ));
        }
        if let Some(config) = pending.input {
            messages.push(HostMessage::Input(
                shojiwm_lib::runtime_input::RuntimeInputConfigUpdate { config },
            ));
        }
        if pending.event_filter {
            let listeners = &registry.listeners;
            messages.push(HostMessage::EventFilter(shojiwm_lib::ssd::RuntimeEventConfigUpdate {
                pointer_move: !listeners.pointer_move.is_empty(),
                pointer_move_async: !listeners.pointer_move_async.is_empty(),
                gesture_swipe: !listeners.gesture_swipe.is_empty(),
                gesture_swipe_async: !listeners.gesture_swipe_async.is_empty(),
            }));
        }
        if pending.processes {
            messages.push(HostMessage::Process(
                shojiwm_lib::runtime_process::RuntimeProcessConfigUpdate {
                    entries: registry.processes.clone(),
                },
            ));
        }
        if pending.debug {
            messages.push(HostMessage::Debug(registry.debug));
        }
        if !pending.process_actions.is_empty() {
            messages.push(HostMessage::ProcessActions(pending.process_actions));
        }
        messages
    });
    if !messages.is_empty()
        && let Some(host) = host()
    {
        host.send_all(messages);
    }
}

/// Drop everything: a fresh runtime starts from nothing.
pub(crate) fn reset() {
    REGISTRY.with(|registry| *registry.borrow_mut() = Registry::default());
    HOST.with(|host| *host.borrow_mut() = None);
    WAYLAND_DISPLAY.with(|display| *display.borrow_mut() = None);
    ACTIONS.with(|actions| actions.borrow_mut().clear());
    DIRTY_WINDOWS.with(|dirty| dirty.borrow_mut().clear());
    // Cleared rather than replaced, so generations keep counting and stale
    // `Window` handles never alias new windows.
    WINDOWS.with(|windows| windows.borrow_mut().clear());
    GLOBAL.with(|global| *global.borrow_mut() = None);
    LAYER_EFFECTS_DIRTY.with(|dirty| dirty.set(false));
}
