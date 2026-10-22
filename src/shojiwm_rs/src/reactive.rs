//! Fine-grained reactivity in the style of SolidJS: signals, memos, effects
//! and the scopes that own them.
//!
//! ```
//! use shojiwm_rs::reactive::*;
//!
//! let scope = Scope::root();
//! let count = scope.signal(1);
//! let doubled = scope.memo(move || count.get() * 2);
//! assert_eq!(doubled.get(), 2);
//! count.set(5);
//! assert_eq!(doubled.get(), 10);
//! scope.dispose();
//! ```
//!
//! Everything lives in a thread-local arena and handles are `Copy`, so they
//! can be captured by any number of `move` closures. The compositor calls the
//! config on a single thread; the types are deliberately `!Send`.
//!
//! Propagation is push-pull (the "reactively" three-colour algorithm): a write
//! marks direct observers dirty and everything further down "check"; memos
//! recompute lazily when read, and observers whose memo inputs turned out
//! unchanged are not notified at all.

use std::{any::Any, cell::RefCell, fmt, marker::PhantomData, rc::Rc};

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) struct NodeId {
    index: u32,
    generation: u32,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) struct ScopeId {
    index: u32,
    generation: u32,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum State {
    Clean,
    Check,
    Dirty,
}

/// Recomputes a memo into its slot; returns whether the value changed.
type MemoCompute = Rc<dyn Fn(&Rc<dyn Any>) -> bool>;

enum Kind {
    Signal,
    /// Recomputes into its slot; returns whether the value changed.
    Memo(MemoCompute),
    /// Re-runs `run` inside `scope`, which is cleared before every run.
    Effect {
        run: Rc<dyn Fn()>,
        scope: ScopeId,
    },
    /// A computation evaluated by someone else (the view serializer). It is
    /// only told that it went stale.
    Observer(Rc<dyn Fn()>),
}

struct Node {
    kind: Kind,
    value: Option<Rc<dyn Any>>,
    state: State,
    sources: Vec<NodeId>,
    observers: Vec<NodeId>,
}

struct ScopeData {
    parent: Option<ScopeId>,
    children: Vec<ScopeId>,
    nodes: Vec<NodeId>,
    cleanups: Vec<Box<dyn FnOnce()>>,
}

struct Arena<T> {
    slots: Vec<Option<T>>,
    generations: Vec<u32>,
    free: Vec<u32>,
}

impl<T> Default for Arena<T> {
    fn default() -> Self {
        Self {
            slots: Vec::new(),
            generations: Vec::new(),
            free: Vec::new(),
        }
    }
}

impl<T> Arena<T> {
    fn insert(&mut self, make: impl FnOnce(u32) -> T) -> (u32, u32) {
        if let Some(index) = self.free.pop() {
            let generation = self.generations[index as usize];
            self.slots[index as usize] = Some(make(generation));
            (index, generation)
        } else {
            let index = self.slots.len() as u32;
            self.generations.push(0);
            self.slots.push(Some(make(0)));
            (index, 0)
        }
    }

    fn remove(&mut self, index: u32, generation: u32) -> Option<T> {
        let slot = self.slots.get_mut(index as usize)?;
        if self.generations[index as usize] != generation {
            return None;
        }
        let value = slot.take()?;
        self.generations[index as usize] = generation.wrapping_add(1);
        self.free.push(index);
        Some(value)
    }

    fn get(&self, index: u32, generation: u32) -> Option<&T> {
        if *self.generations.get(index as usize)? != generation {
            return None;
        }
        self.slots[index as usize].as_ref()
    }

    fn get_mut(&mut self, index: u32, generation: u32) -> Option<&mut T> {
        if *self.generations.get(index as usize)? != generation {
            return None;
        }
        self.slots[index as usize].as_mut()
    }
}

#[derive(Default)]
struct Runtime {
    nodes: Arena<Node>,
    scopes: Arena<ScopeData>,
    observer: Option<NodeId>,
    owner: Option<ScopeId>,
    batch_depth: u32,
    pending: Vec<NodeId>,
    flushing: bool,
}

thread_local! {
    static RUNTIME: RefCell<Runtime> = RefCell::new(Runtime::default());
}

fn with_runtime<R>(f: impl FnOnce(&mut Runtime) -> R) -> R {
    RUNTIME.with(|runtime| f(&mut runtime.borrow_mut()))
}

impl Runtime {
    fn node(&self, id: NodeId) -> Option<&Node> {
        self.nodes.get(id.index, id.generation)
    }

    fn node_mut(&mut self, id: NodeId) -> Option<&mut Node> {
        self.nodes.get_mut(id.index, id.generation)
    }

    fn scope_mut(&mut self, id: ScopeId) -> Option<&mut ScopeData> {
        self.scopes.get_mut(id.index, id.generation)
    }

    fn create_node(&mut self, kind: Kind, value: Option<Rc<dyn Any>>, state: State) -> NodeId {
        let (index, generation) = self.nodes.insert(|_| Node {
            kind,
            value,
            state,
            sources: Vec::new(),
            observers: Vec::new(),
        });
        let id = NodeId { index, generation };
        if let Some(owner) = self.owner
            && let Some(scope) = self.scope_mut(owner)
        {
            scope.nodes.push(id);
        }
        id
    }

    fn create_scope(&mut self, parent: Option<ScopeId>) -> ScopeId {
        let (index, generation) = self.scopes.insert(|_| ScopeData {
            parent,
            children: Vec::new(),
            nodes: Vec::new(),
            cleanups: Vec::new(),
        });
        let id = ScopeId { index, generation };
        if let Some(parent) = parent
            && let Some(parent) = self.scope_mut(parent)
        {
            parent.children.push(id);
        }
        id
    }

    fn track(&mut self, source: NodeId) {
        let Some(observer) = self.observer else {
            return;
        };
        if observer == source {
            return;
        }
        let Some(node) = self.node_mut(observer) else {
            return;
        };
        if node.sources.contains(&source) {
            return;
        }
        node.sources.push(source);
        if let Some(source) = self.node_mut(source) {
            source.observers.push(observer);
        }
    }

    fn unlink_sources(&mut self, id: NodeId) {
        let sources = match self.node_mut(id) {
            Some(node) => std::mem::take(&mut node.sources),
            None => return,
        };
        for source in sources {
            if let Some(source) = self.node_mut(source) {
                source.observers.retain(|observer| *observer != id);
            }
        }
    }

    fn mark(&mut self, id: NodeId, state: State) {
        let Some(node) = self.node_mut(id) else {
            return;
        };
        if node.state >= state {
            return;
        }
        let was_clean = node.state == State::Clean;
        node.state = state;
        let queued = matches!(node.kind, Kind::Effect { .. } | Kind::Observer(_));
        let observers = node.observers.clone();
        if was_clean && queued {
            self.pending.push(id);
        }
        for observer in observers {
            self.mark(observer, State::Check);
        }
    }

    fn dispose_node(&mut self, id: NodeId) {
        self.unlink_sources(id);
        let Some(node) = self.nodes.remove(id.index, id.generation) else {
            return;
        };
        for observer in node.observers {
            if let Some(observer) = self.node_mut(observer) {
                observer.sources.retain(|source| *source != id);
            }
        }
        if let Kind::Effect { scope, .. } = node.kind {
            // Deferred to the caller: cleanups run user code.
            let _ = scope;
        }
    }
}

/// Run user code with no runtime borrow held.
fn node_state(id: NodeId) -> Option<State> {
    with_runtime(|runtime| runtime.node(id).map(|node| node.state))
}

fn update_if_necessary(id: NodeId) {
    if node_state(id) == Some(State::Check) {
        let sources = with_runtime(|runtime| {
            runtime
                .node(id)
                .map(|node| node.sources.clone())
                .unwrap_or_default()
        });
        for source in sources {
            update_if_necessary(source);
            if node_state(id) == Some(State::Dirty) {
                break;
            }
        }
    }
    if node_state(id) == Some(State::Dirty) {
        recompute(id);
    }
    with_runtime(|runtime| {
        if let Some(node) = runtime.node_mut(id) {
            node.state = State::Clean;
        }
    });
}

enum Recompute {
    Memo(MemoCompute, Rc<dyn Any>),
    Effect(Rc<dyn Fn()>, ScopeId),
    Observer(Rc<dyn Fn()>),
}

fn recompute(id: NodeId) {
    let work = with_runtime(|runtime| {
        let node = runtime.node(id)?;
        Some(match &node.kind {
            Kind::Signal => return None,
            Kind::Memo(compute) => Recompute::Memo(compute.clone(), node.value.clone()?),
            Kind::Effect { run, scope } => Recompute::Effect(run.clone(), *scope),
            Kind::Observer(notify) => Recompute::Observer(notify.clone()),
        })
    });
    match work {
        None => {}
        Some(Recompute::Memo(compute, slot)) => {
            with_runtime(|runtime| runtime.unlink_sources(id));
            let changed = with_tracking(Some(id), None, || compute(&slot));
            with_runtime(|runtime| {
                let Some(node) = runtime.node_mut(id) else {
                    return;
                };
                node.state = State::Clean;
                if changed {
                    let observers = node.observers.clone();
                    for observer in observers {
                        if let Some(observer) = runtime.node_mut(observer) {
                            observer.state = State::Dirty;
                        }
                    }
                }
            });
        }
        Some(Recompute::Effect(run, scope)) => {
            clear_scope(scope);
            with_runtime(|runtime| runtime.unlink_sources(id));
            with_tracking(Some(id), Some(scope), || run());
        }
        Some(Recompute::Observer(notify)) => {
            with_runtime(|runtime| {
                if let Some(node) = runtime.node_mut(id) {
                    node.state = State::Clean;
                }
            });
            notify();
        }
    }
}

/// Run `f` with `observer` collecting dependencies and `owner` (if given)
/// owning whatever `f` creates. Restored even if `f` panics.
fn with_tracking<R>(observer: Option<NodeId>, owner: Option<ScopeId>, f: impl FnOnce() -> R) -> R {
    struct Restore {
        observer: Option<NodeId>,
        owner: Option<ScopeId>,
    }
    impl Drop for Restore {
        fn drop(&mut self) {
            let (observer, owner) = (self.observer, self.owner);
            with_runtime(|runtime| {
                runtime.observer = observer;
                runtime.owner = owner;
            });
        }
    }
    let _restore = with_runtime(|runtime| {
        let restore = Restore {
            observer: runtime.observer,
            owner: runtime.owner,
        };
        runtime.observer = observer;
        if owner.is_some() {
            runtime.owner = owner;
        }
        restore
    });
    f()
}

fn flush() {
    let started = with_runtime(|runtime| {
        if runtime.flushing || runtime.batch_depth > 0 {
            false
        } else {
            runtime.flushing = true;
            true
        }
    });
    if !started {
        return;
    }
    struct Done;
    impl Drop for Done {
        fn drop(&mut self) {
            with_runtime(|runtime| runtime.flushing = false);
        }
    }
    let _done = Done;
    let mut rounds = 0;
    loop {
        let pending = with_runtime(|runtime| std::mem::take(&mut runtime.pending));
        if pending.is_empty() {
            break;
        }
        rounds += 1;
        if rounds > 10_000 {
            tracing::error!("reactive flush did not settle; an effect keeps re-triggering itself");
            with_runtime(|runtime| runtime.pending.clear());
            break;
        }
        for id in pending {
            update_if_necessary(id);
        }
    }
}

fn clear_scope(id: ScopeId) {
    let (children, nodes, cleanups) = with_runtime(|runtime| match runtime.scope_mut(id) {
        Some(scope) => (
            std::mem::take(&mut scope.children),
            std::mem::take(&mut scope.nodes),
            std::mem::take(&mut scope.cleanups),
        ),
        None => (Vec::new(), Vec::new(), Vec::new()),
    });
    for child in children.into_iter().rev() {
        dispose_scope(child);
    }
    for cleanup in cleanups.into_iter().rev() {
        untrack(cleanup);
    }
    for node in nodes {
        let effect_scope =
            with_runtime(|runtime| match runtime.node(node).map(|node| &node.kind) {
                Some(Kind::Effect { scope, .. }) => Some(*scope),
                _ => None,
            });
        with_runtime(|runtime| runtime.dispose_node(node));
        if let Some(scope) = effect_scope {
            dispose_scope(scope);
        }
    }
}

fn dispose_scope(id: ScopeId) {
    clear_scope(id);
    with_runtime(|runtime| {
        if let Some(scope) = runtime.scopes.remove(id.index, id.generation)
            && let Some(parent) = scope.parent
            && let Some(parent) = runtime.scope_mut(parent)
        {
            parent.children.retain(|child| *child != id);
        }
    });
}

fn notify_write(id: NodeId) {
    with_runtime(|runtime| {
        let observers = runtime
            .node(id)
            .map(|node| node.observers.clone())
            .unwrap_or_default();
        for observer in observers {
            runtime.mark(observer, State::Dirty);
        }
    });
    flush();
}

fn slot_of<T: 'static>(id: NodeId) -> Option<Rc<RefCell<T>>> {
    let value = with_runtime(|runtime| runtime.node(id).and_then(|node| node.value.clone()))?;
    value.downcast::<RefCell<T>>().ok()
}

/// Run `f` without tracking what it reads.
pub fn untrack<R>(f: impl FnOnce() -> R) -> R {
    with_tracking(None, None, f)
}

/// Defer effects and observer notifications until `f` returns.
pub fn batch<R>(f: impl FnOnce() -> R) -> R {
    struct Leave;
    impl Drop for Leave {
        fn drop(&mut self) {
            with_runtime(|runtime| runtime.batch_depth -= 1);
        }
    }
    with_runtime(|runtime| runtime.batch_depth += 1);
    let result = {
        let _leave = Leave;
        f()
    };
    flush();
    result
}

/// Register `f` to run when the current owner (scope or effect run) is
/// disposed. Outside of any owner it never runs.
pub fn on_cleanup(f: impl FnOnce() + 'static) {
    with_runtime(|runtime| {
        if let Some(owner) = runtime.owner
            && let Some(scope) = runtime.scope_mut(owner)
        {
            scope.cleanups.push(Box::new(f));
        }
    });
}

/// The scope that currently owns new reactive nodes, if any.
pub fn current_scope() -> Option<Scope> {
    with_runtime(|runtime| runtime.owner).map(|id| Scope { id })
}

/// An owner of reactive nodes. Disposing a scope disposes everything created
/// in it (signals, memos, effects, child scopes) and runs its cleanups.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Scope {
    id: ScopeId,
}

impl fmt::Debug for Scope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Scope({}v{})", self.id.index, self.id.generation)
    }
}

impl Scope {
    /// A new scope with no parent; it lives until disposed.
    pub fn root() -> Self {
        Self {
            id: with_runtime(|runtime| runtime.create_scope(None)),
        }
    }

    /// A scope disposed together with `self`.
    pub fn child(self) -> Self {
        Self {
            id: with_runtime(|runtime| runtime.create_scope(Some(self.id))),
        }
    }

    pub fn is_alive(self) -> bool {
        with_runtime(|runtime| {
            runtime
                .scopes
                .get(self.id.index, self.id.generation)
                .is_some()
        })
    }

    /// Run `f` with this scope owning what it creates.
    pub fn run<R>(self, f: impl FnOnce() -> R) -> R {
        with_tracking(with_runtime(|runtime| runtime.observer), Some(self.id), f)
    }

    pub fn signal<T: 'static>(self, value: T) -> Signal<T> {
        self.run(|| signal(value))
    }

    pub fn memo<T: PartialEq + 'static>(self, f: impl Fn() -> T + 'static) -> Memo<T> {
        self.run(|| memo(f))
    }

    pub fn effect(self, f: impl Fn() + 'static) {
        self.run(|| effect(f))
    }

    pub fn on_cleanup(self, f: impl FnOnce() + 'static) {
        self.run(|| on_cleanup(f))
    }

    /// Dispose everything owned by the scope but keep the scope itself.
    pub fn clear(self) {
        clear_scope(self.id);
    }

    pub fn dispose(self) {
        dispose_scope(self.id);
    }
}

/// A reactive read-write value.
pub struct Signal<T: 'static> {
    id: NodeId,
    _marker: PhantomData<*const T>,
}

impl<T> Clone for Signal<T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T> Copy for Signal<T> {}
impl<T> PartialEq for Signal<T> {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}
impl<T> Eq for Signal<T> {}
impl<T> fmt::Debug for Signal<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Signal({}v{})", self.id.index, self.id.generation)
    }
}

/// Create a signal owned by the current scope.
pub fn signal<T: 'static>(value: T) -> Signal<T> {
    let slot: Rc<dyn Any> = Rc::new(RefCell::new(value));
    let id = with_runtime(|runtime| runtime.create_node(Kind::Signal, Some(slot), State::Clean));
    Signal {
        id,
        _marker: PhantomData,
    }
}

impl<T: 'static> Signal<T> {
    /// Read without cloning; tracked.
    pub fn with<R>(&self, f: impl FnOnce(&T) -> R) -> R {
        self.try_with(f)
            .expect("read a disposed signal (its scope was disposed)")
    }

    /// `None` once the owning scope has been disposed.
    pub fn try_with<R>(&self, f: impl FnOnce(&T) -> R) -> Option<R> {
        let slot = slot_of::<T>(self.id)?;
        with_runtime(|runtime| runtime.track(self.id));
        let value = slot.borrow();
        Some(f(&value))
    }

    pub fn with_untracked<R>(&self, f: impl FnOnce(&T) -> R) -> R {
        untrack(|| self.with(f))
    }

    /// Replace the value and notify observers, even if it is equal.
    pub fn set_always(&self, value: T) {
        let Some(slot) = slot_of::<T>(self.id) else {
            return;
        };
        let old = std::mem::replace(&mut *slot.borrow_mut(), value);
        drop(old);
        notify_write(self.id);
    }

    /// Mutate in place and notify observers.
    pub fn update(&self, f: impl FnOnce(&mut T)) {
        let Some(slot) = slot_of::<T>(self.id) else {
            return;
        };
        f(&mut slot.borrow_mut());
        notify_write(self.id);
    }

    pub fn is_alive(&self) -> bool {
        with_runtime(|runtime| runtime.node(self.id).is_some())
    }

    pub fn dispose(self) {
        with_runtime(|runtime| runtime.dispose_node(self.id));
    }
}

impl<T: PartialEq + 'static> Signal<T> {
    /// Store `value`; observers are notified only if it differs.
    pub fn set(&self, value: T) {
        let Some(slot) = slot_of::<T>(self.id) else {
            return;
        };
        {
            let mut current = slot.borrow_mut();
            if *current == value {
                return;
            }
            *current = value;
        }
        notify_write(self.id);
    }
}

impl<T: Clone + 'static> Signal<T> {
    /// Read (tracked) and clone the value.
    pub fn get(&self) -> T {
        self.with(T::clone)
    }

    pub fn get_untracked(&self) -> T {
        self.with_untracked(T::clone)
    }

    /// Derive a memo, like `signal(x => ...)` in the TypeScript SDK.
    pub fn map<U: PartialEq + 'static>(self, f: impl Fn(&T) -> U + 'static) -> Memo<U> {
        memo(move || self.with(|value| f(value)))
    }
}

impl<T: 'static> Signal<T> {
    /// A handle that can read but not write.
    pub fn read_only(self) -> ReadSignal<T> {
        ReadSignal(self)
    }
}

/// A read-only view of a [`Signal`], e.g. a window's title.
pub struct ReadSignal<T: 'static>(Signal<T>);

impl<T> Clone for ReadSignal<T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T> Copy for ReadSignal<T> {}
impl<T> PartialEq for ReadSignal<T> {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}
impl<T> fmt::Debug for ReadSignal<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl<T: 'static> ReadSignal<T> {
    pub fn with<R>(&self, f: impl FnOnce(&T) -> R) -> R {
        self.0.with(f)
    }

    pub fn try_with<R>(&self, f: impl FnOnce(&T) -> R) -> Option<R> {
        self.0.try_with(f)
    }

    pub fn with_untracked<R>(&self, f: impl FnOnce(&T) -> R) -> R {
        self.0.with_untracked(f)
    }
}

impl<T: Clone + 'static> ReadSignal<T> {
    pub fn get(&self) -> T {
        self.0.get()
    }

    pub fn get_untracked(&self) -> T {
        self.0.get_untracked()
    }

    pub fn map<U: PartialEq + 'static>(self, f: impl Fn(&T) -> U + 'static) -> Memo<U> {
        self.0.map(f)
    }
}

impl<T: Clone> Get<T> for ReadSignal<T> {
    fn get(&self) -> T {
        ReadSignal::get(self)
    }
}

impl<T> From<ReadSignal<T>> for Prop<T> {
    fn from(signal: ReadSignal<T>) -> Self {
        Self::Signal(signal.0)
    }
}

impl<T> From<Signal<T>> for ReadSignal<T> {
    fn from(signal: Signal<T>) -> Self {
        Self(signal)
    }
}

/// A derived value, recomputed lazily when one of its inputs changes.
pub struct Memo<T: 'static> {
    id: NodeId,
    _marker: PhantomData<*const T>,
}

impl<T> Clone for Memo<T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T> Copy for Memo<T> {}
impl<T> fmt::Debug for Memo<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Memo({}v{})", self.id.index, self.id.generation)
    }
}

/// Create a memo owned by the current scope.
pub fn memo<T: PartialEq + 'static>(f: impl Fn() -> T + 'static) -> Memo<T> {
    let slot: Rc<dyn Any> = Rc::new(RefCell::new(None::<T>));
    let compute = Rc::new(move |slot: &Rc<dyn Any>| -> bool {
        let next = f();
        let slot = slot
            .clone()
            .downcast::<RefCell<Option<T>>>()
            .expect("memo slot type");
        let mut current = slot.borrow_mut();
        if current.as_ref() == Some(&next) {
            return false;
        }
        *current = Some(next);
        true
    });
    let id =
        with_runtime(|runtime| runtime.create_node(Kind::Memo(compute), Some(slot), State::Dirty));
    Memo {
        id,
        _marker: PhantomData,
    }
}

impl<T: 'static> Memo<T> {
    pub fn with<R>(&self, f: impl FnOnce(&T) -> R) -> R {
        self.try_with(f)
            .expect("read a disposed memo (its scope was disposed)")
    }

    pub fn try_with<R>(&self, f: impl FnOnce(&T) -> R) -> Option<R> {
        with_runtime(|runtime| runtime.node(self.id).map(|_| ()))?;
        update_if_necessary(self.id);
        let slot = slot_of::<Option<T>>(self.id)?;
        with_runtime(|runtime| runtime.track(self.id));
        let value = slot.borrow();
        value.as_ref().map(f)
    }

    pub fn with_untracked<R>(&self, f: impl FnOnce(&T) -> R) -> R {
        untrack(|| self.with(f))
    }

    pub fn dispose(self) {
        with_runtime(|runtime| runtime.dispose_node(self.id));
    }
}

impl<T: Clone + 'static> Memo<T> {
    pub fn get(&self) -> T {
        self.with(T::clone)
    }

    pub fn get_untracked(&self) -> T {
        self.with_untracked(T::clone)
    }

    pub fn map<U: PartialEq + 'static>(self, f: impl Fn(&T) -> U + 'static) -> Memo<U> {
        memo(move || self.with(|value| f(value)))
    }
}

/// Run `f` now and again whenever something it read changes. Whatever `f`
/// creates is disposed before the next run.
pub fn effect(f: impl Fn() + 'static) {
    let id = with_runtime(|runtime| {
        let scope = runtime.create_scope(runtime.owner);
        runtime.create_node(
            Kind::Effect {
                run: Rc::new(f),
                scope,
            },
            None,
            State::Dirty,
        )
    });
    update_if_necessary(id);
}

/// A computation evaluated by its owner through [`Observer::track`]; the
/// runtime only reports that it went stale, by calling `notify`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) struct Observer {
    id: NodeId,
}

impl Observer {
    pub(crate) fn new(notify: impl Fn() + 'static) -> Self {
        let id = with_runtime(|runtime| {
            runtime.create_node(Kind::Observer(Rc::new(notify)), None, State::Clean)
        });
        Self { id }
    }

    /// Evaluate `f`, replacing the observer's dependencies with what it reads.
    pub(crate) fn track<R>(self, f: impl FnOnce() -> R) -> R {
        with_runtime(|runtime| {
            runtime.unlink_sources(self.id);
            if let Some(node) = runtime.node_mut(self.id) {
                node.state = State::Clean;
            }
        });
        with_tracking(Some(self.id), None, f)
    }

    pub(crate) fn dispose(self) {
        with_runtime(|runtime| runtime.dispose_node(self.id));
    }
}

/// Anything that can be read reactively.
pub trait Get<T> {
    /// Tracked read.
    fn get(&self) -> T;
}

impl<T: Clone> Get<T> for Signal<T> {
    fn get(&self) -> T {
        Signal::get(self)
    }
}

impl<T: Clone> Get<T> for Memo<T> {
    fn get(&self) -> T {
        Memo::get(self)
    }
}

/// A value that is either fixed or read reactively: what view props and
/// shader uniforms accept.
pub enum Prop<T: 'static> {
    Static(T),
    Signal(Signal<T>),
    Memo(Memo<T>),
    Derived(Rc<dyn Fn() -> T>),
}

impl<T: Clone> Clone for Prop<T> {
    fn clone(&self) -> Self {
        match self {
            Self::Static(value) => Self::Static(value.clone()),
            Self::Signal(signal) => Self::Signal(*signal),
            Self::Memo(memo) => Self::Memo(*memo),
            Self::Derived(f) => Self::Derived(f.clone()),
        }
    }
}

impl<T: fmt::Debug> fmt::Debug for Prop<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Static(value) => f.debug_tuple("Static").field(value).finish(),
            Self::Signal(signal) => f.debug_tuple("Signal").field(signal).finish(),
            Self::Memo(memo) => f.debug_tuple("Memo").field(memo).finish(),
            Self::Derived(_) => f.write_str("Derived(..)"),
        }
    }
}

impl<T: Clone + 'static> Prop<T> {
    /// A prop recomputed from `f` every time it is read.
    pub fn derive(f: impl Fn() -> T + 'static) -> Self {
        Self::Derived(Rc::new(f))
    }

    /// Tracked read.
    pub fn get(&self) -> T {
        match self {
            Self::Static(value) => value.clone(),
            Self::Signal(signal) => signal.get(),
            Self::Memo(memo) => memo.get(),
            Self::Derived(f) => f(),
        }
    }

    pub fn get_untracked(&self) -> T {
        untrack(|| self.get())
    }

    pub fn is_static(&self) -> bool {
        matches!(self, Self::Static(_))
    }

    /// Transform the value, keeping it reactive.
    pub fn map<U: Clone + 'static>(self, f: impl Fn(T) -> U + 'static) -> Prop<U> {
        match self {
            Self::Static(value) => Prop::Static(f(value)),
            other => Prop::Derived(Rc::new(move || f(other.get()))),
        }
    }
}

/// Shorthand for [`Prop::derive`].
pub fn derive<T: Clone + 'static>(f: impl Fn() -> T + 'static) -> Prop<T> {
    Prop::derive(f)
}

impl<T> From<T> for Prop<T> {
    fn from(value: T) -> Self {
        Self::Static(value)
    }
}

impl<T> From<Signal<T>> for Prop<T> {
    fn from(signal: Signal<T>) -> Self {
        Self::Signal(signal)
    }
}

impl<T> From<Memo<T>> for Prop<T> {
    fn from(memo: Memo<T>) -> Self {
        Self::Memo(memo)
    }
}

impl From<&str> for Prop<String> {
    fn from(value: &str) -> Self {
        Self::Static(value.to_owned())
    }
}

impl From<Signal<&'static str>> for Prop<String> {
    fn from(signal: Signal<&'static str>) -> Self {
        Self::Derived(Rc::new(move || signal.get().to_owned()))
    }
}

impl From<Memo<&'static str>> for Prop<String> {
    fn from(memo: Memo<&'static str>) -> Self {
        Self::Derived(Rc::new(move || memo.get().to_owned()))
    }
}

macro_rules! numeric_props {
    ($($from:ty => $to:ty),* $(,)?) => {$(
        impl From<$from> for Prop<$to> {
            fn from(value: $from) -> Self {
                Self::Static(value as $to)
            }
        }
    )*};
}

numeric_props!(i32 => f64, i32 => f32);

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn memo_follows_signal_lazily() {
        let scope = Scope::root();
        let runs = Rc::new(Cell::new(0));
        let count = scope.signal(1);
        let doubled = {
            let runs = runs.clone();
            scope.memo(move || {
                runs.set(runs.get() + 1);
                count.get() * 2
            })
        };
        assert_eq!(runs.get(), 0);
        assert_eq!(doubled.get(), 2);
        count.set(3);
        count.set(4);
        assert_eq!(runs.get(), 1);
        assert_eq!(doubled.get(), 8);
        assert_eq!(runs.get(), 2);
        scope.dispose();
    }

    #[test]
    fn effect_reruns_and_cleans_up() {
        let scope = Scope::root();
        let seen = Rc::new(RefCell::new(Vec::new()));
        let cleanups = Rc::new(Cell::new(0));
        let value = scope.signal("a");
        {
            let seen = seen.clone();
            let cleanups = cleanups.clone();
            scope.effect(move || {
                seen.borrow_mut().push(value.get());
                let cleanups = cleanups.clone();
                on_cleanup(move || cleanups.set(cleanups.get() + 1));
            });
        }
        value.set("b");
        value.set("b");
        assert_eq!(*seen.borrow(), ["a", "b"]);
        assert_eq!(cleanups.get(), 1);
        scope.dispose();
        assert_eq!(cleanups.get(), 2);
        value.set("c");
        assert_eq!(seen.borrow().len(), 2);
    }

    #[test]
    fn unchanged_memo_does_not_notify() {
        let scope = Scope::root();
        let number = scope.signal(2);
        let even = scope.memo(move || number.get() % 2 == 0);
        let notified = Rc::new(Cell::new(0));
        let observer = {
            let notified = notified.clone();
            scope.run(|| Observer::new(move || notified.set(notified.get() + 1)))
        };
        assert!(observer.track(|| even.get()));
        number.set(4);
        assert_eq!(notified.get(), 0);
        number.set(5);
        assert_eq!(notified.get(), 1);
        observer.dispose();
        scope.dispose();
    }

    #[test]
    fn batch_defers_effects() {
        let scope = Scope::root();
        let a = scope.signal(1);
        let b = scope.signal(1);
        let runs = Rc::new(Cell::new(0));
        {
            let runs = runs.clone();
            scope.effect(move || {
                let _ = a.get() + b.get();
                runs.set(runs.get() + 1);
            });
        }
        batch(|| {
            a.set(2);
            b.set(2);
        });
        assert_eq!(runs.get(), 2);
        scope.dispose();
    }

    #[test]
    fn effect_may_write_signals() {
        let scope = Scope::root();
        let source = scope.signal(1);
        let mirror = scope.signal(0);
        scope.effect(move || mirror.set(source.get() * 10));
        assert_eq!(mirror.get_untracked(), 10);
        source.set(2);
        assert_eq!(mirror.get_untracked(), 20);
        scope.dispose();
    }

    #[test]
    fn disposed_handles_read_as_none() {
        let scope = Scope::root();
        let value = scope.signal(1);
        scope.dispose();
        assert!(value.try_with(|value| *value).is_none());
        value.set(3);
    }

    #[test]
    fn prop_reads_every_source_kind() {
        let scope = Scope::root();
        let signal = scope.signal(2.0_f64);
        let memo = scope.memo(move || signal.get() + 1.0);
        let props: [Prop<f64>; 4] = [
            1.0.into(),
            signal.into(),
            memo.into(),
            derive(move || signal.get() * 10.0),
        ];
        let values: Vec<f64> = props.iter().map(Prop::get).collect();
        assert_eq!(values, [1.0, 2.0, 3.0, 20.0]);
        scope.dispose();
    }
}
