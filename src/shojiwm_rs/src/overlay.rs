//! Output overlays (`COMPOSITOR.effect.overlay`): an effect drawn over a
//! whole output, e.g. a transition that starts from a frozen
//! [`snapshot_source`] of the screen.
//!
//! ```no_run
//! use shojiwm_rs::prelude::*;
//!
//! let fade = Animation::new(1.0);
//! let effect = Effect::new(snapshot_source())
//!     .stage(shader_stage("./fade.frag").uniform("alpha", fade.signal()));
//! if let Ok(overlay) = COMPOSITOR.effect.overlay("DP-1", Overlay::new(effect).persistent(true)) {
//!     overlay.on_ready(move || fade.start(AnimationOptions::to(0.0, 300.0)));
//! }
//! ```
//!
//! Unlike the TypeScript API, creating an overlay does not wait for its
//! first frame: [`OverlayHandle::on_ready`] runs once it has been drawn
//! (the snapshot taken), and [`OverlayHandle::on_closed`] once it is gone.

use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::{Arc, atomic::AtomicBool},
};

use shojiwm_lib::backend::overlay::{self as native, Control};

use crate::{
    effect::{Effect, Source},
    reactive::Observer,
    runtime,
};

/// A frozen, cursor-free capture of the output, made when the overlay's
/// first frame is rendered (`snapshotSource()`). Only overlays can read it.
pub fn snapshot_source() -> Source {
    Source::Named(native::SNAPSHOT_NAME.to_owned())
}

/// Where an overlay is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OverlayPlacement {
    /// Above everything but the cursor.
    #[default]
    Top,
    /// Above windows but below Top/Overlay layer-shell surfaces.
    BelowLayers,
}

/// What to draw (`overlay(output, { effect, placement, persistent, maxDuration })`).
#[derive(Debug, Clone)]
pub struct Overlay {
    effect: Effect,
    placement: OverlayPlacement,
    persistent: bool,
    max_duration_ms: f64,
}

impl Overlay {
    /// Uniforms of `effect` may be signals; the overlay follows them.
    pub fn new(effect: Effect) -> Self {
        Self {
            effect,
            placement: OverlayPlacement::Top,
            persistent: false,
            max_duration_ms: 10_000.0,
        }
    }

    pub fn placement(mut self, placement: OverlayPlacement) -> Self {
        self.placement = placement;
        self
    }

    /// Remain after the first frame until disposed (default: one frame).
    pub fn persistent(mut self, persistent: bool) -> Self {
        self.persistent = persistent;
        self
    }

    /// Deadline in milliseconds (default 10 s); a persistent overlay only
    /// bounds its first frame with it.
    pub fn max_duration_ms(mut self, duration: f64) -> Self {
        self.max_duration_ms = duration;
        self
    }
}

struct Entry {
    control: Arc<Control>,
    effect: Effect,
    observer: Observer,
    /// A signal the effect read changed since it was last sent.
    dirty: Rc<Cell<bool>>,
    ready_fired: bool,
    on_ready: Vec<Rc<dyn Fn()>>,
    on_closed: Vec<Rc<dyn Fn()>>,
}

thread_local! {
    static OWNER: RefCell<Option<Arc<AtomicBool>>> = const { RefCell::new(None) };
    static ENTRIES: RefCell<Vec<Rc<RefCell<Entry>>>> = const { RefCell::new(Vec::new()) };
}

fn owner() -> Arc<AtomicBool> {
    OWNER.with(|owner| {
        owner
            .borrow_mut()
            .get_or_insert_with(|| Arc::new(AtomicBool::new(true)))
            .clone()
    })
}

/// A running overlay.
#[derive(Clone)]
pub struct OverlayHandle(Rc<RefCell<Entry>>);

impl OverlayHandle {
    /// Remove the overlay.
    pub fn dispose(&self) {
        self.0.borrow().control.dispose();
    }

    /// The first frame with the overlay has been rendered.
    pub fn is_ready(&self) -> bool {
        self.0.borrow().control.is_ready()
    }

    /// Disposed, timed out, or dropped by the compositor.
    pub fn is_closed(&self) -> bool {
        self.0.borrow().control.is_closed()
    }

    /// Run `f` once the first frame has been rendered (right away on a later
    /// turn if it already has).
    pub fn on_ready(&self, f: impl Fn() + 'static) {
        self.0.borrow_mut().on_ready.push(Rc::new(f));
        wake();
    }

    /// Run `f` once the overlay is gone.
    pub fn on_closed(&self, f: impl Fn() + 'static) {
        self.0.borrow_mut().on_closed.push(Rc::new(f));
        wake();
    }
}

fn wake() {
    if let Some(host) = runtime::host() {
        host.wake();
    }
}

pub(crate) fn create(output: &str, overlay: Overlay) -> Result<OverlayHandle, String> {
    let host = runtime::host().ok_or("output overlays need a running config")?;
    let placement = match overlay.placement {
        OverlayPlacement::Top => "top",
        OverlayPlacement::BelowLayers => "below-layers",
    };
    let dirty = Rc::new(Cell::new(false));
    let observer = {
        let dirty = dirty.clone();
        runtime::global().scope.run(|| {
            // Pushed by `sync` at the end of the turn that changed it.
            Observer::new(move || dirty.set(true))
        })
    };
    let effect = overlay.effect;
    let compiled = observer.track(|| effect.compile());
    let id = native::create(
        &owner(),
        output.to_owned(),
        placement.to_owned(),
        overlay.max_duration_ms,
        compiled,
        overlay.persistent,
        host,
    )
    .map_err(|error| {
        observer.dispose();
        error.to_string()
    })?;
    let control = native::get(&owner(), id).map_err(|error| error.to_string())?;
    let handle = OverlayHandle(Rc::new(RefCell::new(Entry {
        control,
        effect,
        observer,
        dirty,
        ready_fired: false,
        on_ready: Vec::new(),
        on_closed: Vec::new(),
    })));
    ENTRIES.with(|entries| entries.borrow_mut().push(handle.0.clone()));
    Ok(handle)
}

/// Push effect changes, fire `on_ready` / `on_closed` and forget closed
/// overlays. Called at the end of every runtime turn.
pub(crate) fn sync() {
    let entries = ENTRIES.with(|entries| entries.borrow().clone());
    for entry in entries {
        let closed = entry.borrow().control.is_closed();
        if closed {
            let (observer, callbacks) = {
                let mut entry = entry.borrow_mut();
                (entry.observer, std::mem::take(&mut entry.on_closed))
            };
            observer.dispose();
            ENTRIES.with(|entries| {
                entries
                    .borrow_mut()
                    .retain(|other| !Rc::ptr_eq(other, &entry))
            });
            for callback in callbacks {
                callback();
            }
            continue;
        }
        let dirty = entry.borrow().dirty.replace(false);
        if dirty {
            let (observer, effect, control) = {
                let entry = entry.borrow();
                (entry.observer, entry.effect.clone(), entry.control.clone())
            };
            let compiled = observer.track(|| effect.compile());
            if let Err(error) = control.update(compiled) {
                tracing::error!(%error, "output overlay update failed");
                control.dispose();
            }
        }
        let ready = {
            let entry = entry.borrow();
            !entry.ready_fired && entry.control.is_ready()
        };
        if ready {
            let callbacks = {
                let mut entry = entry.borrow_mut();
                entry.ready_fired = true;
                std::mem::take(&mut entry.on_ready)
            };
            for callback in callbacks {
                callback();
            }
        } else if entry.borrow().ready_fired {
            // `on_ready` registered after the overlay was already ready.
            let callbacks = std::mem::take(&mut entry.borrow_mut().on_ready);
            for callback in callbacks {
                callback();
            }
        }
    }
}

/// Close every overlay of the stopping runtime.
pub(crate) fn reset() {
    if let Some(owner) = OWNER.with(|owner| owner.borrow_mut().take()) {
        native::close_owner(&owner);
    }
    let entries = ENTRIES.with(|entries| std::mem::take(&mut *entries.borrow_mut()));
    for entry in entries {
        entry.borrow().observer.dispose();
    }
}
