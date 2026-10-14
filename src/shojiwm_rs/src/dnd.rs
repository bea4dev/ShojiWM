//! Drag and drop (`COMPOSITOR.dnd`): what is drawn while data is dragged
//! between windows, and what happens to it after the drop.
//!
//! Nothing is animated for you. A listener shows **drag views** whose
//! position, opacity and scale are props (signals or animations), and whose
//! content is an element tree like a window decoration.
//!
//! ```no_run
//! use shojiwm_rs::prelude::*;
//!
//! COMPOSITOR.dnd.on_start(|drag| {
//!     let fade = Animation::new(1.0);
//!     let view = drag.show(DragViewOptions::new().opacity(fade).content({
//!         let drag = drag.clone();
//!         move || {
//!             Flex::column()
//!                 .child(drag.icon_slot(Label::new("📄")))
//!                 .child(Label::new(if drag.action().get() == DragAction::Copy { "+ Copy" } else { "" }))
//!         }
//!     }));
//!     drag.on_end(move |_| {
//!         let view = view.clone();
//!         fade.start_then(AnimationOptions::to(0.0, 150.0), move |_| view.dispose());
//!     });
//! });
//! ```
//!
//! Differences from the TypeScript API: the listener runs in a scope that
//! lives as long as the drag (so [`Animation`](crate::animation::Animation)s
//! created there take the place of `drag.animation`), and the application's
//! icon is placed with [`DragSession::icon_slot`] instead of `<DragIcon>`.

use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

pub use shojiwm_lib::dnd::{
    DragAction, DragActions, DragDevice, DragEndKind, DragIconSnapshot as DragIconInfo, DragPoint,
    DragViewAnchor,
};
use shojiwm_lib::{
    dnd::{DndEventSnapshot, DragSnapshot, DragViewCommand, DragViewProps},
    runtime_api::HostMessage,
};

use crate::{
    reactive::{Observer, Prop, ReadSignal, Scope, Signal, batch, untrack},
    runtime::{self, with_registry},
    view::{ClientWindow, Element, Flex, MountedNode, ViewContext},
    window::Window,
};

/// How a drag ended.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DragEnd {
    /// `Dropped`: the button was released (over a target or not).
    /// `Cancelled`: the drag ended some other way (the application gave up).
    pub kind: DragEndKind,
    /// The target accepted the data. A drop over nothing, or on a window
    /// that refused it, is `false`.
    pub accepted: bool,
    /// The window the drop landed on.
    pub target: Option<Window>,
    /// The pointer when the drag ended (global logical coordinates).
    pub position: DragPoint,
}

/// `drag.show(options)`: how a drag view is placed and drawn. Every prop may
/// be a signal or an animation; the view follows it.
#[derive(Clone)]
pub struct DragViewOptions {
    anchor: Prop<DragViewAnchor>,
    x: Prop<f64>,
    y: Prop<f64>,
    opacity: Prop<f64>,
    scale: Prop<f64>,
    content: Option<Rc<dyn Fn() -> Element>>,
    max_linger_ms: f64,
}

impl Default for DragViewOptions {
    fn default() -> Self {
        Self {
            anchor: DragViewAnchor::Pointer.into(),
            x: 0.0.into(),
            y: 0.0.into(),
            opacity: 1.0.into(),
            scale: 1.0.into(),
            content: None,
            max_linger_ms: 5000.0,
        }
    }
}

impl DragViewOptions {
    pub fn new() -> Self {
        Self::default()
    }

    /// `Pointer` (default): `x` / `y` offset the view from the pointer, which
    /// the compositor follows on its own. `Global`: `x` / `y` are global
    /// logical coordinates.
    pub fn anchor(mut self, value: impl Into<Prop<DragViewAnchor>>) -> Self {
        self.anchor = value.into();
        self
    }

    /// Where the view's anchor point (the pointer's spot in the view) goes.
    pub fn x(mut self, value: impl Into<Prop<f64>>) -> Self {
        self.x = value.into();
        self
    }

    pub fn y(mut self, value: impl Into<Prop<f64>>) -> Self {
        self.y = value.into();
        self
    }

    /// Opacity of the whole view, `0..=1`. Below 1 it also fades a
    /// backdrop effect in the content, letting what is behind show through.
    pub fn opacity(mut self, value: impl Into<Prop<f64>>) -> Self {
        self.opacity = value.into();
        self
    }

    /// Scale around the anchor point.
    pub fn scale(mut self, value: impl Into<Prop<f64>>) -> Self {
        self.scale = value.into();
        self
    }

    /// What to draw; [`DragSession::icon_slot`] places the application's
    /// icon. `f` runs again whenever a signal it reads changes, and each run
    /// lays the content out and draws it again: read slow-changing state
    /// there (`drag.action()`, ...) and animate through the props instead.
    /// Without content, the icon alone is drawn.
    pub fn content(mut self, f: impl Fn() -> Element + 'static) -> Self {
        self.content = Some(Rc::new(f));
        self
    }

    /// How long the view may stay after the drag ended if
    /// [`DragView::dispose`] never comes (default 5000 ms).
    pub fn max_linger_ms(mut self, value: f64) -> Self {
        if value.is_finite() {
            self.max_linger_ms = value.max(0.0);
        }
        self
    }
}

struct ViewState {
    id: u64,
    options: DragViewOptions,
    disposed: Cell<bool>,
    props_observer: Observer,
    props_dirty: Rc<Cell<bool>>,
    last_props: Cell<Option<DragViewProps>>,
    content_observer: Observer,
    content_dirty: Rc<Cell<bool>>,
    content_scope: Cell<Option<Scope>>,
    /// Session scope the content is built under.
    scope: Scope,
    /// Wall-clock deadline once the drag ended; the compositor drops the
    /// view then.
    deadline_ms: Cell<Option<f64>>,
    on_closed: RefCell<Vec<Box<dyn FnOnce()>>>,
}

/// A shown drag view.
#[derive(Clone)]
pub struct DragView(Rc<ViewState>);

impl DragView {
    /// Remove the view.
    pub fn dispose(&self) {
        let state = &self.0;
        if state.disposed.replace(true) {
            return;
        }
        state.props_observer.dispose();
        state.content_observer.dispose();
        if let Some(scope) = state.content_scope.take() {
            scope.dispose();
        }
        runtime::send(HostMessage::DragView(DragViewCommand::Dispose { view_id: state.id }));
        let callbacks = std::mem::take(&mut *state.on_closed.borrow_mut());
        for callback in callbacks {
            callback();
        }
    }

    pub fn is_disposed(&self) -> bool {
        self.0.disposed.get()
    }

    /// Run `f` once the view is disposed of (right away if it already is).
    pub fn on_closed(&self, f: impl FnOnce() + 'static) {
        if self.is_disposed() {
            f();
        } else {
            self.0.on_closed.borrow_mut().push(Box::new(f));
        }
    }

    /// Send prop and content changes. Called at the end of every turn.
    fn sync(&self) {
        let state = &self.0;
        if state.disposed.get() {
            return;
        }
        if state.props_dirty.replace(false) {
            let options = &state.options;
            let props = state.props_observer.track(|| DragViewProps {
                anchor: options.anchor.get(),
                x: options.x.get(),
                y: options.y.get(),
                opacity: options.opacity.get(),
                scale: options.scale.get(),
            });
            if state.last_props.get() != Some(props) {
                state.last_props.set(Some(props));
                runtime::send(HostMessage::DragView(DragViewCommand::Props {
                    view_id: state.id,
                    props,
                }));
            }
        }
        if state.content_dirty.replace(false)
            && let Some(content) = state.options.content.clone()
        {
            if let Some(scope) = state.content_scope.take() {
                scope.dispose();
            }
            let scope = state.scope.child();
            state.content_scope.set(Some(scope));
            // Clicks and hovers never reach a drag view: handlers are
            // collected and dropped. Any change in the mounted nodes
            // rebuilds the whole content, like a change `content` read.
            let context = ViewContext {
                dirty: Default::default(),
                handlers: Default::default(),
                on_dirty: {
                    let dirty = state.content_dirty.clone();
                    Rc::new(move || dirty.set(true))
                },
            };
            let tree = scope.run(|| {
                state.content_observer.track(|| {
                    let element = content();
                    let mut node = MountedNode::mount(element, "dnd".into(), &context);
                    node.build(&context)
                })
            });
            runtime::send(HostMessage::DragView(DragViewCommand::Content {
                view_id: state.id,
                content: Some(tree),
            }));
        }
    }
}

type EndListener = Box<dyn FnOnce(&DragEnd)>;

struct SessionState {
    id: u64,
    mime_types: Vec<String>,
    actions: DragActions,
    source_window: Option<Window>,
    device: DragDevice,
    origin: DragPoint,
    scope: Scope,
    action: Signal<DragAction>,
    target: Signal<Option<Window>>,
    icon: Signal<Option<DragIconInfo>>,
    ended: Signal<Option<DragEnd>>,
    end_listeners: RefCell<Vec<EndListener>>,
    views: RefCell<Vec<DragView>>,
}

/// One drag, from start to end, and the views drawn for it.
#[derive(Clone)]
pub struct DragSession(Rc<SessionState>);

impl PartialEq for DragSession {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
}

impl DragSession {
    pub fn id(&self) -> u64 {
        self.0.id
    }

    /// The formats the data is offered in. The data itself is never exposed.
    pub fn mime_types(&self) -> &[String] {
        &self.0.mime_types
    }

    /// The actions the source allows.
    pub fn actions(&self) -> DragActions {
        self.0.actions
    }

    /// What a drop would do now. `None` both over nothing and over a window
    /// that refuses the data; [`target`](Self::target) tells them apart.
    pub fn action(&self) -> ReadSignal<DragAction> {
        self.0.action.read_only()
    }

    /// The window under the pointer.
    pub fn target(&self) -> ReadSignal<Option<Window>> {
        self.0.target.read_only()
    }

    /// The window the drag started from.
    pub fn source_window(&self) -> Option<Window> {
        self.0.source_window
    }

    pub fn device(&self) -> DragDevice {
        self.0.device
    }

    /// The pointer when the drag started (global logical coordinates).
    pub fn origin(&self) -> DragPoint {
        self.0.origin
    }

    /// The application's icon: its size and where its top-left corner sits
    /// relative to the pointer; `None` without one.
    pub fn icon(&self) -> ReadSignal<Option<DragIconInfo>> {
        self.0.icon.read_only()
    }

    /// `None` until the drag ends.
    pub fn ended(&self) -> ReadSignal<Option<DragEnd>> {
        self.0.ended.read_only()
    }

    /// Where the application's icon goes in a view's content, or `fallback`
    /// when the application gave none (`<DragIcon>` in TypeScript). The view
    /// is anchored at the pointer's spot on the icon; without one, at the
    /// content's top-left corner. Use it once per content.
    pub fn icon_slot(&self, fallback: impl Into<Option<Element>>) -> Element {
        if self.0.icon.with(Option::is_some) {
            ClientWindow::new()
        } else {
            fallback.into().unwrap_or_else(Flex::column)
        }
    }

    /// Draw a view for this drag. It stays until [`DragView::dispose`], also
    /// after the drag ended, so a drop or cancel can animate.
    pub fn show(&self, options: DragViewOptions) -> DragView {
        let session = &self.0;
        let id = NEXT_VIEW_ID.with(|next| {
            let id = next.get();
            next.set(id + 1);
            id
        });
        runtime::send(HostMessage::DragView(DragViewCommand::Show {
            view_id: id,
            drag_id: session.id,
            max_linger_ms: Some(options.max_linger_ms as u64),
        }));
        let dirty_flag = || {
            let dirty = Rc::new(Cell::new(true));
            let observer = {
                let dirty = dirty.clone();
                Observer::new(move || dirty.set(true))
            };
            (dirty, observer)
        };
        let (props_dirty, props_observer) = dirty_flag();
        let (content_dirty, content_observer) = dirty_flag();
        let deadline_ms = session
            .ended
            .with_untracked(Option::is_some)
            .then(|| wall_clock_ms() + options.max_linger_ms);
        let view = DragView(Rc::new(ViewState {
            id,
            options,
            disposed: Cell::new(false),
            props_observer,
            props_dirty,
            last_props: Cell::new(None),
            content_observer,
            content_dirty,
            content_scope: Cell::new(None),
            scope: session.scope,
            deadline_ms: Cell::new(deadline_ms),
            on_closed: RefCell::new(Vec::new()),
        }));
        session.views.borrow_mut().push(view.clone());
        view
    }

    /// Run `f` once, when the drag ends (right away if it already has).
    pub fn on_end(&self, f: impl FnOnce(&DragEnd) + 'static) {
        if let Some(end) = self.0.ended.get_untracked() {
            f(&end);
        } else {
            self.0.end_listeners.borrow_mut().push(Box::new(f));
        }
    }
}

thread_local! {
    static SESSIONS: RefCell<Vec<DragSession>> = const { RefCell::new(Vec::new()) };
    static CURRENT: Cell<Option<Signal<Option<DragSession>>>> = const { Cell::new(None) };
    static NEXT_VIEW_ID: Cell<u64> = const { Cell::new(1) };
}

fn current_signal() -> Signal<Option<DragSession>> {
    CURRENT.with(|current| match current.get() {
        Some(signal) if signal.is_alive() => signal,
        _ => {
            let signal = runtime::global().scope.signal(None);
            current.set(Some(signal));
            signal
        }
    })
}

fn wall_clock_ms() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs_f64() * 1000.0)
        .unwrap_or_default()
}

fn window(id: Option<&str>) -> Option<Window> {
    id.and_then(runtime::window_entry).map(|entry| entry.handle)
}

/// `COMPOSITOR.dnd`.
pub struct DndController;

impl DndController {
    /// A drag started. While any listener is registered, the compositor
    /// draws only the views the listeners show; with none, it draws the
    /// application's icon under the pointer. The listener runs in a scope
    /// that lives until the drag ended and its views are gone.
    pub fn on_start(&self, listener: impl Fn(DragSession) + 'static) {
        with_registry(|registry| {
            registry.listeners.dnd_start.push(Rc::new(listener));
            registry.pending.event_filter = true;
        });
    }

    /// The drag in progress (tracked).
    pub fn current(&self) -> Option<DragSession> {
        current_signal().get()
    }
}

fn create_session(drag: &DragSnapshot) -> DragSession {
    let scope = runtime::global().scope.child();
    DragSession(Rc::new(SessionState {
        id: drag.id,
        mime_types: drag.mime_types.clone(),
        actions: drag.actions,
        source_window: window(drag.source_window_id.as_deref()),
        device: drag.device,
        origin: drag.origin,
        scope,
        action: scope.signal(drag.action),
        target: scope.signal(window(drag.target_window_id.as_deref())),
        icon: scope.signal(drag.icon),
        ended: scope.signal(None),
        end_listeners: RefCell::new(Vec::new()),
        views: RefCell::new(Vec::new()),
    }))
}

fn session(id: u64) -> Option<DragSession> {
    SESSIONS.with(|sessions| sessions.borrow().iter().find(|session| session.0.id == id).cloned())
}

/// Route a compositor drag event. Returns whether a listener ran.
pub(crate) fn dispatch(event: &DndEventSnapshot) -> bool {
    sweep(wall_clock_ms());
    match event {
        DndEventSnapshot::Start { drag } => {
            let session = create_session(drag);
            SESSIONS.with(|sessions| sessions.borrow_mut().push(session.clone()));
            let listeners = with_registry(|registry| registry.listeners.dnd_start.clone());
            batch(|| {
                current_signal().set(Some(session.clone()));
                for listener in &listeners {
                    let session = session.clone();
                    session.0.scope.run(|| listener(session.clone()));
                }
            });
            !listeners.is_empty()
        }
        DndEventSnapshot::Update {
            drag_id,
            action,
            target_window_id,
            icon,
        } => {
            let Some(session) = session(*drag_id) else {
                return false;
            };
            batch(|| {
                session.0.action.set(*action);
                session.0.target.set(window(target_window_id.as_deref()));
                session.0.icon.set(*icon);
            });
            true
        }
        DndEventSnapshot::End {
            drag_id,
            kind,
            accepted,
            target_window_id,
            position,
        } => {
            let current = current_signal();
            if untrack(|| current.get()).is_some_and(|session| session.0.id == *drag_id) {
                current.set(None);
            }
            let Some(session) = session(*drag_id) else {
                return false;
            };
            if session.0.ended.get_untracked().is_some() {
                return true;
            }
            let end = DragEnd {
                kind: *kind,
                accepted: *accepted,
                target: window(target_window_id.as_deref()),
                position: *position,
            };
            let now = wall_clock_ms();
            for view in session.0.views.borrow().iter() {
                if view.0.deadline_ms.get().is_none() {
                    view.0.deadline_ms.set(Some(now + view.0.options.max_linger_ms));
                }
            }
            let listeners = std::mem::take(&mut *session.0.end_listeners.borrow_mut());
            batch(|| {
                session.0.ended.set(Some(end));
                for listener in listeners {
                    listener(&end);
                }
            });
            forget_finished();
            true
        }
    }
}

/// Dispose of the views the compositor already removed after their linger.
fn sweep(now_ms: f64) {
    let sessions = SESSIONS.with(|sessions| sessions.borrow().clone());
    for session in sessions {
        let views = session.0.views.borrow().clone();
        for view in views {
            if view.0.deadline_ms.get().is_some_and(|deadline| now_ms >= deadline) {
                view.dispose();
            }
        }
    }
    forget_finished();
}

/// Drop the sessions that ended and have no views left.
fn forget_finished() {
    let finished: Vec<DragSession> = SESSIONS.with(|sessions| {
        let mut sessions = sessions.borrow_mut();
        for session in sessions.iter() {
            session.0.views.borrow_mut().retain(|view| !view.is_disposed());
        }
        let (finished, live) = std::mem::take(&mut *sessions).into_iter().partition(|session| {
            session.0.ended.with_untracked(Option::is_some) && session.0.views.borrow().is_empty()
        });
        *sessions = live;
        finished
    });
    for session in finished {
        session.0.scope.dispose();
    }
}

/// Push view changes. Called at the end of every runtime turn.
pub(crate) fn sync() {
    let views: Vec<DragView> = SESSIONS.with(|sessions| {
        sessions
            .borrow()
            .iter()
            .flat_map(|session| session.0.views.borrow().clone())
            .collect()
    });
    for view in views {
        view.sync();
    }
    forget_finished();
}

/// Forget every drag of the stopping runtime (the compositor drops their
/// views on reload).
pub(crate) fn reset() {
    let sessions = SESSIONS.with(|sessions| std::mem::take(&mut *sessions.borrow_mut()));
    for session in sessions {
        for view in session.0.views.borrow().iter() {
            view.0.disposed.set(true);
            view.0.props_observer.dispose();
            view.0.content_observer.dispose();
        }
        if session.0.scope.is_alive() {
            session.0.scope.dispose();
        }
    }
    CURRENT.with(|current| current.set(None));
}
