//! The [`ConfigRuntime`] that drives a reactive Rust config: it turns the
//! compositor's requests into signal updates and listener calls, and the
//! resulting dirty state into replies.

use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, HashMap},
    panic::{AssertUnwindSafe, catch_unwind},
    path::PathBuf,
    rc::Rc,
};

use shojiwm_lib::{
    runtime_api::{
        ConfigRuntime, DecorationRequest, EffectRequest, HostMessage, InputRequest, LaunchContext,
        ReloadPreparation, RuntimeError, RuntimeEvent, RuntimeLauncher, RuntimeReply,
        RuntimeRequest, SchedulerTickRequest, WindowRequest, WorkspaceRequest,
    },
    ssd::{
        DecorationCachedEvaluationResult, DecorationEvaluationResult,
        DecorationHandlerInvocation, DecorationKeyBindingInvocation,
        DecorationPointerMoveAsyncInvocation, DecorationSchedulerTick,
        DecorationWindowMoveInvocation, DecorationWindowResizeInvocation,
        DecorationWindowStateRequestInvocation, LayerEffectEvaluationResult, ManagedWindowState,
        PopupEffectEvaluationResult, RuntimeLayerEffectAssignment, RuntimePopupEffectAssignment,
        RuntimeWindowAction, WaylandLayerSnapshot, WaylandPopupSnapshot, WaylandWindowAction,
        WaylandWindowSnapshot, WindowDecorationDecisionSnapshot, validate_layer_effect_config,
        validate_popup_effect_config,
    },
};

use crate::{
    animation::{self, set_timeout},
    assets,
    compositor::{
        self, DisableEvent, EnableEvent, InputChangeEvent, OutputChangeEvent, SurfaceRef,
    },
    reactive::{Observer, Scope, batch, untrack},
    runtime::{self, ComposedView, WindowEntry, emit, with_registry},
    view::{Child, Composition, DirtySet, Flex, MountedNode, ViewContext, ViewTree},
    window::{Window, WindowSignals},
};

/// Starts a reactive Rust config. `setup` plays the role of the TypeScript
/// config module: it runs once at startup and registers everything through
/// [`COMPOSITOR`](crate::COMPOSITOR).
pub struct ConfigBuilder {
    name: &'static str,
    setup: Rc<dyn Fn()>,
    asset_root: Option<PathBuf>,
}

impl ConfigBuilder {
    pub fn new(setup: impl Fn() + 'static) -> Self {
        Self {
            name: "rust",
            setup: Rc::new(setup),
            asset_root: None,
        }
    }

    /// Name shown in logs and `--help` (default `"rust"`).
    pub fn name(mut self, name: &'static str) -> Self {
        self.name = name;
        self
    }

    /// Directory relative shader and image paths are resolved against.
    pub fn asset_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.asset_root = Some(root.into());
        self
    }

    /// Run the compositor with this config. This is the whole `main`.
    pub fn run(self) -> std::process::ExitCode {
        shojiwm_lib::run(self)
    }
}

impl RuntimeLauncher for ConfigBuilder {
    fn name(&self) -> &'static str {
        self.name
    }

    fn launch(&self, context: LaunchContext) -> Box<dyn ConfigRuntime> {
        Box::new(ReactiveRuntime::start(
            self.setup.clone(),
            self.asset_root.clone(),
            context,
        ))
    }
}

/// Shorthand for `ConfigBuilder::new(setup).run()`.
pub fn run_config(setup: impl Fn() + 'static) -> std::process::ExitCode {
    ConfigBuilder::new(setup).run()
}

/// The runtime behind [`ConfigBuilder`]. Public so tests and embedders can
/// drive it directly.
pub struct ReactiveRuntime {
    setup: Rc<dyn Fn()>,
    loaded: bool,
    enabled: bool,
}

impl ReactiveRuntime {
    pub fn start(setup: Rc<dyn Fn()>, asset_root: Option<PathBuf>, context: LaunchContext) -> Self {
        // The reactive arena is kept: rebuilding it would restart generation
        // numbers and let stale handles alias new nodes.
        crate::published::reset();
        crate::overlay::reset();
        crate::input_grab::reset();
        runtime::reset();
        LAYER_OBSERVERS.with(|observers| observers.borrow_mut().clear());
        animation::reset_clock();
        runtime::set_host(context.host.clone());
        runtime::set_wayland_display(context.wayland_display.clone());
        let root = asset_root
            .or_else(|| context.runtime_dir.clone())
            .or_else(|| {
                context
                    .config_path
                    .as_ref()
                    .and_then(|path| path.parent().map(PathBuf::from))
            })
            .unwrap_or_else(|| PathBuf::from("."));
        assets::set_root(root);
        Self {
            setup,
            loaded: false,
            enabled: false,
        }
    }

    fn load(&mut self) -> Result<(), RuntimeError> {
        if self.loaded {
            return Ok(());
        }
        self.loaded = true;
        let setup = self.setup.clone();
        guard(|| {
            batch(|| untrack(|| setup()));
            Ok(())
        })
    }
}

fn guard<R>(f: impl FnOnce() -> Result<R, RuntimeError>) -> Result<R, RuntimeError> {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(result) => result,
        Err(payload) => {
            let message = payload
                .downcast_ref::<&str>()
                .map(|message| (*message).to_owned())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "non-string panic payload".to_owned());
            Err(RuntimeError::RuntimeProtocol(format!("config panicked: {message}")))
        }
    }
}

/// Start of every compositor turn: advance the clock (`None` for a scheduler
/// tick, which keeps its own time) and hand queued cross-thread messages to
/// their handlers.
fn begin_turn(now_ms: Option<f64>) {
    if let Some(now_ms) = now_ms {
        animation::begin_turn(now_ms);
    }
    let channels = with_registry(|registry| registry.channels.clone());
    if !channels.is_empty() {
        batch(|| {
            for drain in &channels {
                drain();
            }
        });
    }
}

fn next_poll() -> Option<u64> {
    if animation::has_running_animations() {
        Some(0)
    } else {
        animation::next_timer_delay()
    }
}

/// Dirty windows and queued actions, in the shape every mutation reply uses.
struct Mutation {
    dirty_window_ids: Vec<String>,
    dirty_managed_window_ids: Vec<String>,
    dirty_window_node_ids: HashMap<String, Vec<String>>,
    actions: Vec<RuntimeWindowAction>,
}

impl Mutation {
    fn is_dirty(&self) -> bool {
        !self.dirty_window_ids.is_empty()
    }
}

fn collect_mutation() -> Mutation {
    let mut mutation = Mutation {
        dirty_window_ids: Vec::new(),
        dirty_managed_window_ids: Vec::new(),
        dirty_window_node_ids: HashMap::new(),
        actions: runtime::drain_actions(),
    };
    for id in runtime::take_dirty_windows() {
        let Some(entry) = runtime::window_entry(&id) else {
            continue;
        };
        let dirty = entry.context.dirty.borrow();
        if dirty.is_empty() {
            continue;
        }
        mutation.dirty_window_ids.push(id.clone());
        let structural = dirty.full || dirty.recompose || dirty.effects;
        if !structural && dirty.nodes.is_empty() && dirty.uniforms.is_empty() {
            mutation.dirty_managed_window_ids.push(id);
        } else if !structural {
            let mut nodes: Vec<String> = dirty.nodes.iter().cloned().collect();
            for (node, _, _) in &dirty.uniforms {
                if !nodes.contains(node) {
                    nodes.push(node.clone());
                }
            }
            mutation.dirty_window_node_ids.insert(id, nodes);
        }
    }
    mutation
}

fn create_window(snapshot: &WaylandWindowSnapshot) -> Rc<WindowEntry> {
    let (slot, generation) = runtime::WINDOWS.with(|windows| windows.borrow_mut().reserve());
    let handle = Window { slot, generation };
    let id: Rc<str> = snapshot.id.as_str().into();
    let scope = untrack(Scope::root);
    let signals = untrack(|| WindowSignals::new(scope, snapshot));
    let context = ViewContext {
        dirty: Rc::new(RefCell::new(DirtySet::default())),
        handlers: Rc::default(),
        on_dirty: {
            let id = id.clone();
            Rc::new(move || runtime::mark_window_dirty(&id))
        },
    };
    let effects_observer = scope.run(|| {
        let context = context.clone();
        Observer::new(move || context.mark(|dirty| dirty.effects = true))
    });
    let entry = Rc::new(WindowEntry {
        id,
        handle,
        scope,
        signals,
        snapshot: RefCell::new(snapshot.clone()),
        state: RefCell::default(),
        context,
        view: RefCell::new(None),
        managed_state: RefCell::new(ManagedWindowState::default()),
        window_effects: RefCell::new(None),
        effects_observer,
        close_duration_ms: Cell::new(0),
        close_started: Cell::new(false),
        close_timer: Cell::new(None),
        opened: Cell::new(false),
        initial_configured: Cell::new(false),
        first_committed: Cell::new(false),
        preconfigured: Cell::new(false),
    });
    runtime::WINDOWS.with(|windows| windows.borrow_mut().insert(slot, entry.clone()));
    entry
}

/// Store a new snapshot; fires `on_focus` when focus flipped.
fn update_snapshot(entry: &Rc<WindowEntry>, snapshot: &WaylandWindowSnapshot) {
    let focus_changed = entry.snapshot.borrow().is_focused != snapshot.is_focused;
    *entry.snapshot.borrow_mut() = snapshot.clone();
    let signals = entry.signals;
    let window = entry.handle;
    batch(|| {
        signals.update(snapshot);
        if focus_changed {
            emit(|listeners| listeners.focus.clone(), |listener| listener(window, snapshot.is_focused));
        }
    });
}

fn emit_window(select: impl FnOnce(&runtime::Listeners) -> Vec<Rc<dyn Fn(Window)>>, window: Window) -> bool {
    batch(|| emit(select, |listener| listener(window)))
}

/// Entry for `snapshot`, created (with `on_open` / `on_focus`) if new.
fn ensure_window(snapshot: &WaylandWindowSnapshot) -> (Rc<WindowEntry>, bool) {
    match runtime::window_entry(&snapshot.id) {
        Some(entry) => {
            update_snapshot(&entry, snapshot);
            (entry, false)
        }
        None => {
            let entry = create_window(snapshot);
            let window = entry.handle;
            batch(|| {
                if !entry.opened.replace(true) {
                    emit(|listeners| listeners.open.clone(), |listener| listener(window));
                }
                emit(|listeners| listeners.focus.clone(), |listener| listener(window, snapshot.is_focused));
            });
            (entry, true)
        }
    }
}

fn mark_first_commit(entry: &Rc<WindowEntry>) {
    if !entry.first_committed.replace(true) {
        emit_window(|listeners| listeners.first_commit.clone(), entry.handle);
        entry.context.mark_full();
    }
}

fn has_composition() -> bool {
    with_registry(|registry| registry.composition.is_some())
}

/// Run the composition function (again) and mount its result.
fn compose(entry: &Rc<WindowEntry>) {
    let Some(composition) = with_registry(|registry| registry.composition.clone()) else {
        return;
    };
    let previous = entry.view.borrow_mut().take();
    if let Some(previous) = previous {
        previous.root_observer.dispose();
        previous.scope.dispose();
    }

    let context = entry.context.clone();
    let scope = entry.scope.child();
    let root_observer = scope.run(|| {
        let context = context.clone();
        Observer::new(move || context.mark_recompose())
    });
    let window = entry.handle;
    let composed = scope.run(|| root_observer.track(|| composition(window)));

    let (managed, children) = match composed {
        Composition::Managed(mut managed) => {
            let children = managed.take_children();
            (Some(managed), children)
        }
        Composition::Unmanaged(element) => (None, vec![Child::Element(element)]),
    };
    let root_element = match <[Child; 1]>::try_from(children) {
        Ok([Child::Element(element)]) => element,
        Ok([dynamic]) => Flex::column().child(dynamic),
        Err(children) => Flex::column().children(children),
    };
    let root = scope.run(|| MountedNode::mount(root_element, "root".to_owned(), &context));
    let tree = scope.run(|| ViewTree::new(root, &context));

    let managed = managed.map(|managed| {
        let observer = scope.run(|| {
            let context = context.clone();
            Observer::new(move || context.mark_managed())
        });
        (managed, observer)
    });
    *entry.view.borrow_mut() = Some(ComposedView {
        scope,
        root_observer,
        tree,
        managed,
    });
    resolve_managed(entry);
}

fn resolve_managed(entry: &Rc<WindowEntry>) {
    let window = entry.handle;
    let policy = with_registry(|registry| registry.surface_policy.clone());
    let state = {
        let view = entry.view.borrow();
        match view.as_ref().and_then(|view| view.managed.as_ref()) {
            Some((managed, observer)) => observer.track(|| {
                let mut state = managed.resolve();
                state.surface_policy = policy.as_ref().and_then(|policy| policy(SurfaceRef::Toplevel(window)));
                state
            }),
            None => ManagedWindowState {
                surface_policy: policy.as_ref().and_then(|policy| untrack(|| policy(SurfaceRef::Toplevel(window)))),
                ..ManagedWindowState::default()
            },
        }
    };
    *entry.managed_state.borrow_mut() = state;
}

fn resolve_window_effects(entry: &Rc<WindowEntry>) {
    let Some(effect) = with_registry(|registry| registry.window_effect.clone()) else {
        return;
    };
    let window = entry.handle;
    let config = entry.effects_observer.track(|| effect(window).compile());
    *entry.window_effects.borrow_mut() = Some(config);
}

struct Refresh {
    full: bool,
    patches: Vec<shojiwm_lib::runtime_api::CompositionPatch>,
    dirty_node_ids: Vec<String>,
    managed_only: bool,
}

/// Bring the window's tree, managed state and effects up to date.
fn refresh(entry: &Rc<WindowEntry>, force_full: bool) -> Refresh {
    let dirty = std::mem::take(&mut *entry.context.dirty.borrow_mut());
    let first = entry.view.borrow().is_none();
    if first || dirty.recompose {
        compose(entry);
        resolve_window_effects(entry);
        return Refresh {
            full: true,
            patches: Vec::new(),
            dirty_node_ids: Vec::new(),
            managed_only: false,
        };
    }
    if dirty.effects {
        resolve_window_effects(entry);
    }
    let full = force_full || dirty.full;
    let managed_only = !full
        && dirty.managed
        && dirty.nodes.is_empty()
        && dirty.uniforms.is_empty()
        && !dirty.effects;
    let (patches, dirty_node_ids) = {
        let mut view = entry.view.borrow_mut();
        let view = view.as_mut().expect("composed above");
        if full {
            view.tree.rebuild(&entry.context);
            (Vec::new(), Vec::new())
        } else {
            let update = view.tree.update(dirty.nodes, dirty.uniforms, &entry.context);
            (update.patches, update.dirty_node_ids)
        }
    };
    if full || dirty.managed {
        resolve_managed(entry);
    }
    Refresh {
        full,
        patches,
        dirty_node_ids,
        managed_only,
    }
}

fn current_tree(entry: &Rc<WindowEntry>) -> Option<shojiwm_lib::ssd::DecorationNode> {
    entry
        .view
        .borrow()
        .as_ref()
        .map(|view| view.tree.tree.clone())
}

fn evaluation(entry: &Rc<WindowEntry>, actions: Vec<RuntimeWindowAction>) -> Result<RuntimeReply, RuntimeError> {
    let Some(node) = current_tree(entry) else {
        return Ok(RuntimeReply::Unhandled);
    };
    let managed_window = entry.managed_state.borrow().clone();
    Ok(RuntimeReply::Evaluation(Box::new(DecorationEvaluationResult {
        node,
        transform: managed_window.transform,
        managed_window,
        window_effects: entry.window_effects.borrow().clone(),
        dirty_node_ids: Vec::new(),
        next_poll_in_ms: next_poll(),
        actions,
    })))
}

fn evaluate(snapshot: &WaylandWindowSnapshot, preview: bool) -> Result<RuntimeReply, RuntimeError> {
    let (entry, created) = ensure_window(snapshot);
    if preview {
        if !entry.initial_configured.replace(true) {
            emit_window(|listeners| listeners.initial_configure.clone(), entry.handle);
        }
        entry.preconfigured.set(true);
    } else {
        if entry.preconfigured.replace(false) {
            entry.context.mark_full();
        }
        if created || !entry.first_committed.get() {
            mark_first_commit(&entry);
        }
    }
    if !has_composition() {
        return Ok(RuntimeReply::Unhandled);
    }
    refresh(&entry, false);
    let actions = if preview {
        runtime::drain_actions_for(&snapshot.id)
    } else {
        runtime::drain_actions()
    };
    evaluation(&entry, actions)
}

fn evaluate_cached(
    window_id: &str,
    snapshot: Option<&WaylandWindowSnapshot>,
    force_full: bool,
) -> Result<RuntimeReply, RuntimeError> {
    let entry = match (runtime::window_entry(window_id), snapshot) {
        (Some(entry), Some(snapshot)) => {
            update_snapshot(&entry, snapshot);
            entry
        }
        (Some(entry), None) => entry,
        (None, Some(snapshot)) => {
            let (entry, _) = ensure_window(snapshot);
            mark_first_commit(&entry);
            entry
        }
        (None, None) => {
            return Err(RuntimeError::RuntimeProtocol(format!(
                "no state for window {window_id}"
            )));
        }
    };
    if !has_composition() {
        return Ok(RuntimeReply::Unhandled);
    }
    let refresh = refresh(&entry, force_full);
    let managed_window = entry.managed_state.borrow().clone();
    Ok(RuntimeReply::CachedEvaluation(Box::new(DecorationCachedEvaluationResult {
        node: if refresh.full { current_tree(&entry) } else { None },
        node_patches: refresh.patches,
        transform: managed_window.transform,
        managed_window,
        window_effects: entry.window_effects.borrow().clone(),
        window_effect_uniform_only: false,
        dirty_node_ids: refresh.dirty_node_ids,
        managed_window_only: refresh.managed_only,
        next_poll_in_ms: next_poll(),
        actions: Vec::new(),
    })))
}

fn handler_invocation(invoked: bool) -> DecorationHandlerInvocation {
    let mutation = collect_mutation();
    DecorationHandlerInvocation {
        invoked,
        close_animation_duration_ms: None,
        node: None,
        transform: None,
        managed_window: None,
        window_effects: None,
        dirty_window_ids: mutation.dirty_window_ids,
        dirty_managed_window_ids: mutation.dirty_managed_window_ids,
        dirty_window_node_ids: mutation.dirty_window_node_ids,
        actions: mutation.actions,
        next_poll_in_ms: next_poll(),
    }
}

macro_rules! mutation_reply {
    ($ty:ident, $invoked:expr) => {{
        let invoked = $invoked;
        let mutation = collect_mutation();
        $ty {
            invoked,
            dirty: mutation.is_dirty(),
            dirty_window_ids: mutation.dirty_window_ids,
            dirty_managed_window_ids: mutation.dirty_managed_window_ids,
            dirty_window_node_ids: mutation.dirty_window_node_ids,
            dirty_layer_node_ids: HashMap::new(),
            actions: mutation.actions,
            next_poll_in_ms: next_poll(),
        }
    }};
}

fn invoke_handler(window_id: &str, handler_id: &str) -> DecorationHandlerInvocation {
    let handler = runtime::window_entry(window_id)
        .and_then(|entry| entry.context.handlers.borrow().get(handler_id).cloned());
    let invoked = match handler {
        Some(handler) => {
            batch(|| handler());
            true
        }
        None => false,
    };
    handler_invocation(invoked)
}

fn start_close(window_id: &str) -> DecorationHandlerInvocation {
    let Some(entry) = runtime::window_entry(window_id) else {
        return handler_invocation(false);
    };
    if !entry.close_started.replace(true) {
        emit_window(|listeners| listeners.start_close.clone(), entry.handle);
        let duration = entry.close_duration_ms.get();
        let id = window_id.to_owned();
        let finalize = move || {
            runtime::push_action(RuntimeWindowAction {
                window_id: id.clone(),
                action: WaylandWindowAction::FinalizeClose,
                animation: None,
                channel: None,
            });
        };
        if duration == 0 {
            finalize();
        } else {
            entry
                .close_timer
                .set(Some(set_timeout(duration as f64, finalize)));
        }
    }
    if has_composition() && entry.view.borrow().is_some() {
        refresh(&entry, false);
    }
    let mut invocation = handler_invocation(true);
    invocation.close_animation_duration_ms = Some(entry.close_duration_ms.get());
    invocation.transform = Some(entry.transform());
    invocation.managed_window = Some(entry.managed_state.borrow().clone());
    invocation.window_effects = entry.window_effects.borrow().clone();
    if !invocation.dirty_window_ids.iter().any(|id| id == window_id) {
        invocation.dirty_window_ids.push(window_id.to_owned());
    }
    invocation
}

fn window_closed(window_id: &str) {
    let Some(entry) = runtime::window_entry(window_id) else {
        return;
    };
    emit_window(|listeners| listeners.close.clone(), entry.handle);
    if let Some(timer) = entry.close_timer.take() {
        timer.cancel();
    }
    runtime::WINDOWS.with(|windows| windows.borrow_mut().remove(window_id));
    let view = entry.view.borrow_mut().take();
    if let Some(view) = view {
        view.root_observer.dispose();
        view.scope.dispose();
    }
    entry.scope.dispose();
}

fn scheduler_tick(tick: SchedulerTickRequest<'_>, now_ms: f64) -> DecorationSchedulerTick {
    batch(|| animation::tick(tick.output, tick.frame_outputs, now_ms));
    let mutation = collect_mutation();
    let dirty_layer_ids: Vec<String> = if runtime::take_layer_effects_dirty() {
        compositor::COMPOSITOR.layer.list().into_iter().map(|layer| layer.id).collect()
    } else {
        Vec::new()
    };
    DecorationSchedulerTick {
        dirty: mutation.is_dirty() || !dirty_layer_ids.is_empty(),
        runtime_dirty: false,
        dirty_window_ids: mutation.dirty_window_ids,
        dirty_managed_window_ids: mutation.dirty_managed_window_ids,
        dirty_window_node_ids: mutation.dirty_window_node_ids,
        dirty_layer_ids,
        dirty_layer_node_ids: HashMap::new(),
        actions: mutation.actions,
        next_poll_in_ms: next_poll(),
    }
}

/// Window for a state request, created if the compositor asks about a
/// window before its first evaluation.
fn request_window(snapshot: &WaylandWindowSnapshot) -> Window {
    ensure_window(snapshot).0.handle
}

thread_local! {
    static LAYER_OBSERVERS: RefCell<BTreeMap<String, Observer>> = const { RefCell::new(BTreeMap::new()) };
}

fn effects_observer(key: &str) -> Observer {
    LAYER_OBSERVERS.with(|observers| {
        *observers
            .borrow_mut()
            .entry(key.to_owned())
            .or_insert_with(|| {
                runtime::global()
                    .scope
                    .run(|| Observer::new(runtime::mark_layer_effects_dirty))
            })
    })
}

fn sync_layers(output_name: &str, layers: &[WaylandLayerSnapshot]) {
    let global = runtime::global();
    let previous = global.layers.get_untracked();
    let mut next = previous.clone();
    next.retain(|_, layer| layer.output_name != output_name);
    for layer in layers {
        next.insert(layer.id.clone(), layer.clone());
    }
    if next == previous {
        return;
    }
    batch(|| {
        global.layers.set(next.clone());
        for (id, layer) in &previous {
            if layer.output_name == output_name && !next.contains_key(id) {
                emit(|listeners| listeners.destroy_layer.clone(), |listener| listener(layer));
            }
        }
        for layer in layers {
            match previous.get(&layer.id) {
                None => emit(|listeners| listeners.create_layer.clone(), |listener| listener(layer)),
                Some(old) if old != layer => {
                    emit(|listeners| listeners.update_layer.clone(), |listener| listener(layer))
                }
                Some(_) => false,
            };
        }
    });
}

fn layer_effects(output_name: &str, layers: &[WaylandLayerSnapshot]) -> LayerEffectEvaluationResult {
    sync_layers(output_name, layers);
    let Some(effect) = with_registry(|registry| registry.layer_effect.clone()) else {
        return LayerEffectEvaluationResult::default();
    };
    let observer = effects_observer(&format!("layer:{output_name}"));
    let effects = observer.track(|| {
        layers
            .iter()
            .map(|layer| RuntimeLayerEffectAssignment {
                layer_id: layer.id.clone(),
                effects: match validate_layer_effect_config(effect(layer).compile()) {
                    Ok(config) => Some(config),
                    Err(error) => {
                        tracing::warn!(layer = %layer.id, %error, "invalid layer effect");
                        None
                    }
                },
            })
            .collect()
    });
    LayerEffectEvaluationResult {
        effects,
        next_poll_in_ms: next_poll(),
    }
}

fn popup_effects(output_name: &str, popups: &[WaylandPopupSnapshot]) -> PopupEffectEvaluationResult {
    let (effect, policy) = with_registry(|registry| {
        (registry.popup_effect.clone(), registry.surface_policy.clone())
    });
    if effect.is_none() && policy.is_none() {
        return PopupEffectEvaluationResult::default();
    }
    let observer = effects_observer(&format!("popup:{output_name}"));
    let effects = observer.track(|| {
        popups
            .iter()
            .map(|popup| RuntimePopupEffectAssignment {
                popup_id: popup.id.clone(),
                effects: effect.as_ref().and_then(|effect| {
                    match validate_popup_effect_config(effect(popup).compile()) {
                        Ok(config) => Some(config),
                        Err(error) => {
                            tracing::warn!(popup = %popup.id, %error, "invalid popup effect");
                            None
                        }
                    }
                }),
                surface_policy: policy.as_ref().and_then(|policy| {
                    policy(SurfaceRef::Popup {
                        popup,
                        parent_kind: popup.parent_kind,
                    })
                }),
            })
            .collect()
    });
    PopupEffectEvaluationResult {
        effects,
        next_poll_in_ms: next_poll(),
    }
}

fn pointer_hook(invoked: bool) -> DecorationPointerMoveAsyncInvocation {
    mutation_reply!(DecorationPointerMoveAsyncInvocation, invoked)
}

fn set_display_state(state: BTreeMap<String, shojiwm_lib::ssd::WaylandOutputSnapshot>) {
    let global = runtime::global();
    let previous = global.outputs.get_untracked();
    if previous == state {
        return;
    }
    let event = OutputChangeEvent {
        added: state
            .values()
            .filter(|output| !previous.contains_key(&output.name))
            .cloned()
            .collect(),
        removed: previous
            .values()
            .filter(|output| !state.contains_key(&output.name))
            .cloned()
            .collect(),
        changed: state
            .values()
            .filter(|output| previous.get(&output.name).is_some_and(|old| old != *output))
            .cloned()
            .collect(),
        current: state.clone(),
    };
    batch(|| {
        global.outputs.set(state);
        animation::sync_output_poll_groups();
        compositor::reconfigure_outputs(false);
        emit(|listeners| listeners.output_change.clone(), |listener| listener(&event));
    });
}

fn set_input_state(state: BTreeMap<String, shojiwm_lib::runtime_input::RuntimeInputDeviceSnapshot>) {
    let global = runtime::global();
    let previous = global.inputs.get_untracked();
    if previous == state {
        return;
    }
    let event = InputChangeEvent {
        added: state
            .iter()
            .filter(|(key, _)| !previous.contains_key(*key))
            .map(|(_, device)| device.clone())
            .collect(),
        removed: previous
            .iter()
            .filter(|(key, _)| !state.contains_key(*key))
            .map(|(_, device)| device.clone())
            .collect(),
        devices: state.clone(),
    };
    batch(|| {
        global.inputs.set(state);
        compositor::reconfigure_input(false);
        emit(|listeners| listeners.input_change.clone(), |listener| listener(&event));
    });
}

/// Send the config's pending deltas, then whatever reactive state changed.
fn publish() {
    if let Err(error) = guard(|| {
        crate::published::publish();
        Ok(())
    }) {
        tracing::error!(%error, "rust config failed to publish its state");
    }
    runtime::publish_pending();
}

/// Ask for a tick when windows went dirty outside of a request.
fn wake_if_dirty() {
    let dirty = runtime::WINDOWS.with(|windows| {
        windows
            .borrow()
            .all()
            .iter()
            .any(|entry| !entry.context.dirty.borrow().is_empty())
    });
    if (dirty || runtime::has_pending_actions())
        && let Some(host) = runtime::host()
    {
        host.wake();
    }
}

impl ConfigRuntime for ReactiveRuntime {
    fn preload(&mut self) -> Result<(), RuntimeError> {
        self.load()
    }

    fn enable(&mut self) -> Result<(), RuntimeError> {
        self.load()?;
        if self.enabled {
            return Ok(());
        }
        self.enabled = true;
        guard(|| {
            batch(|| {
                let event = EnableEvent {
                    reason: "initial".to_owned(),
                };
                emit(|listeners| listeners.enable.clone(), |listener| listener(&event));
            });
            with_registry(|registry| {
                let pending = &mut registry.pending;
                pending.key_bindings = true;
                pending.pointer = true;
                pending.processes = true;
                pending.event_filter = true;
                pending.debug = true;
            });
            compositor::reconfigure_outputs(true);
            compositor::reconfigure_input(true);
            compositor::COMPOSITOR.workspace.reconfigure();
            Ok(())
        })?;
        publish();
        Ok(())
    }

    fn prepare_reload(&mut self) -> Result<ReloadPreparation, RuntimeError> {
        Err(RuntimeError::Unsupported("hot reload of a compiled Rust config"))
    }

    fn reload(&mut self) -> Result<(), RuntimeError> {
        Err(RuntimeError::Unsupported("hot reload of a compiled Rust config"))
    }

    fn request(&mut self, now_ms: f64, request: RuntimeRequest<'_>) -> Result<RuntimeReply, RuntimeError> {
        let reply = guard(|| {
            begin_turn((!matches!(request, RuntimeRequest::SchedulerTick(_))).then_some(now_ms));
            Ok(match request {
                RuntimeRequest::Decoration(request) => match request {
                    DecorationRequest::Evaluate { window, preview } => evaluate(window, preview)?,
                    DecorationRequest::EvaluateCached {
                        window_id,
                        window,
                        force_full,
                    } => evaluate_cached(window_id, window, force_full)?,
                    DecorationRequest::Policy { window, context } => {
                        match with_registry(|registry| registry.decoration_policy.clone()) {
                            Some(policy) => RuntimeReply::DecorationPolicy(WindowDecorationDecisionSnapshot {
                                mode: untrack(|| policy(window, context)),
                            }),
                            None => RuntimeReply::Unhandled,
                        }
                    }
                    DecorationRequest::InvokeHandler {
                        window_id,
                        handler_id,
                    } => RuntimeReply::Handler(Box::new(invoke_handler(window_id, handler_id))),
                    DecorationRequest::StartClose { window_id } => {
                        RuntimeReply::Handler(Box::new(start_close(window_id)))
                    }
                    DecorationRequest::Closed { window_id } => {
                        window_closed(window_id);
                        RuntimeReply::Done
                    }
                },
                // A frame tick carries its frame's presentation time and advances
                // only that output's clock; a timer tick carries wall-clock time.
                RuntimeRequest::SchedulerTick(tick) => {
                    RuntimeReply::SchedulerTick(scheduler_tick(tick, now_ms))
                }
                RuntimeRequest::Window(request) => match request {
                    WindowRequest::Resize { window_id, event } => {
                        let invoked = runtime::window_entry(window_id).is_some_and(|entry| {
                            batch(|| {
                                emit(|listeners| listeners.window_resize.clone(), |listener| {
                                    listener(entry.handle, event)
                                })
                            })
                        });
                        RuntimeReply::WindowResize(mutation_reply!(DecorationWindowResizeInvocation, invoked))
                    }
                    WindowRequest::Move { window_id, event } => {
                        let invoked = runtime::window_entry(window_id).is_some_and(|entry| {
                            batch(|| {
                                emit(|listeners| listeners.window_move.clone(), |listener| {
                                    listener(entry.handle, event)
                                })
                            })
                        });
                        RuntimeReply::WindowMove(mutation_reply!(DecorationWindowMoveInvocation, invoked))
                    }
                    WindowRequest::Maximize { window, event } => {
                        let window = request_window(window);
                        let invoked = batch(|| {
                            emit(|listeners| listeners.maximize_request.clone(), |listener| listener(window, event))
                        });
                        RuntimeReply::WindowStateRequest(mutation_reply!(DecorationWindowStateRequestInvocation, invoked))
                    }
                    WindowRequest::Minimize { window, event } => {
                        let window = request_window(window);
                        let invoked = batch(|| {
                            emit(|listeners| listeners.minimize_request.clone(), |listener| listener(window, event))
                        });
                        RuntimeReply::WindowStateRequest(mutation_reply!(DecorationWindowStateRequestInvocation, invoked))
                    }
                    WindowRequest::Fullscreen { window, event } => {
                        let window = request_window(window);
                        let invoked = batch(|| {
                            emit(|listeners| listeners.fullscreen_request.clone(), |listener| listener(window, event))
                        });
                        RuntimeReply::WindowStateRequest(mutation_reply!(DecorationWindowStateRequestInvocation, invoked))
                    }
                    WindowRequest::Activate { window, event } => {
                        let window = request_window(window);
                        let invoked = batch(|| {
                            emit(|listeners| listeners.activate_request.clone(), |listener| listener(window, event))
                        });
                        RuntimeReply::WindowStateRequest(mutation_reply!(DecorationWindowStateRequestInvocation, invoked))
                    }
                },
                RuntimeRequest::Input(request) => match request {
                    InputRequest::KeyBinding { binding_id } => {
                        let handler = with_registry(|registry| registry.key_handlers.get(binding_id).cloned());
                        let invoked = match handler {
                            Some(handler) => {
                                batch(|| handler());
                                true
                            }
                            None => false,
                        };
                        RuntimeReply::KeyBinding(mutation_reply!(DecorationKeyBindingInvocation, invoked))
                    }
                    InputRequest::PointerMove(event) => {
                        let invoked = batch(|| emit(|listeners| listeners.pointer_move.clone(), |listener| listener(event)));
                        RuntimeReply::PointerHook(pointer_hook(invoked))
                    }
                    InputRequest::GestureSwipe(event) => {
                        let invoked = batch(|| emit(|listeners| listeners.gesture_swipe.clone(), |listener| listener(event)));
                        RuntimeReply::PointerHook(pointer_hook(invoked))
                    }
                    InputRequest::InputGrab { grab_id, event } => {
                        let invoked = batch(|| crate::input_grab::dispatch(grab_id, event));
                        RuntimeReply::PointerHook(pointer_hook(invoked))
                    }
                },
                RuntimeRequest::Effect(request) => match request {
                    EffectRequest::Background => {
                        RuntimeReply::BackgroundEffect(crate::published::background_effect())
                    }
                    EffectRequest::Layers { output_name, layers } => {
                        RuntimeReply::LayerEffects(layer_effects(output_name, layers))
                    }
                    EffectRequest::Popups { output_name, popups } => {
                        RuntimeReply::PopupEffects(popup_effects(output_name, popups))
                    }
                },
                RuntimeRequest::Workspace(WorkspaceRequest::Activate(event)) => {
                    let invoked = batch(|| emit(|listeners| listeners.workspace_activate.clone(), |listener| listener(event)));
                    RuntimeReply::Handler(Box::new(handler_invocation(invoked)))
                }
            })
        });
        publish();
        reply
    }

    fn post(&mut self, now_ms: f64, event: RuntimeEvent) {
        let result = guard(|| {
            begin_turn(Some(now_ms));
            match event {
                RuntimeEvent::DisplayState(state) => set_display_state(state),
                RuntimeEvent::InputState(state) => set_input_state(state),
                RuntimeEvent::KeyboardLayout(layout) => {
                    batch(|| {
                        runtime::global().keyboard_layout.set(Some(layout.clone()));
                        emit(|listeners| listeners.keyboard_layout.clone(), |listener| listener(&layout));
                    });
                }
                RuntimeEvent::PointerMove(event) => {
                    let invoked = batch(|| emit(|listeners| listeners.pointer_move_async.clone(), |listener| listener(&event)));
                    runtime::send(HostMessage::PointerHookResult(pointer_hook(invoked)));
                }
                RuntimeEvent::GestureSwipe(event) => {
                    let invoked = batch(|| emit(|listeners| listeners.gesture_swipe_async.clone(), |listener| listener(&event)));
                    runtime::send(HostMessage::PointerHookResult(pointer_hook(invoked)));
                }
            }
            Ok(())
        });
        if let Err(error) = result {
            tracing::error!(%error, "rust config failed to handle an event");
        }
        publish();
        wake_if_dirty();
    }

    fn shutdown(&mut self) {
        let _ = guard(|| {
            batch(|| {
                let event = DisableEvent {
                    reason: "shutdown".to_owned(),
                };
                emit(|listeners| listeners.disable.clone(), |listener| listener(&event));
            });
            Ok(())
        });
        runtime::publish_pending();
        crate::overlay::reset();
        crate::input_grab::reset();
    }
}
