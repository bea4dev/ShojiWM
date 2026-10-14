//! Config state the compositor does not ask for but is sent whenever it
//! changes: the per-output compositions, the frame pacing and, after its
//! first read, the background effect. Signals these read are watched; a
//! change only marks them dirty, and [`publish`] re-evaluates them once at
//! the end of the turn.

use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, HashMap},
    panic::{AssertUnwindSafe, catch_unwind},
    rc::Rc,
};

use shojiwm_lib::{
    backend::composition::OutputComposition,
    frame_pacing::FramePacingConfig,
    runtime_api::{HostMessage, RuntimeSchedule},
    ssd::{BackgroundEffectConfig, WaylandOutputSnapshot},
};

use crate::{
    reactive::{Observer, untrack},
    runtime::{self, with_registry},
};

/// A computation re-run when a signal it read changes.
struct Watched {
    observer: Observer,
    dirty: Rc<Cell<bool>>,
}

impl Watched {
    fn new() -> Self {
        let dirty = Rc::new(Cell::new(true));
        let observer = {
            let dirty = dirty.clone();
            runtime::global().scope.run(|| {
                // Config code only runs inside runtime turns, and every turn
                // ends with `publish`: no need to wake the compositor (that
                // would add a turn per change, which a per-frame poll turns
                // into a loop).
                Observer::new(move || dirty.set(true))
            })
        };
        Self { observer, dirty }
    }

    fn track<R>(&self, f: impl FnOnce() -> R) -> R {
        self.dirty.set(false);
        self.observer.track(f)
    }

    fn dispose(self) {
        self.observer.dispose();
    }
}


struct CompositionSlot {
    /// The snapshot it was evaluated for; a change re-evaluates.
    output: WaylandOutputSnapshot,
    watched: Watched,
    plan: Option<OutputComposition>,
}

#[derive(Default)]
struct State {
    /// Set once the compositor has read the background effect.
    background: Option<Watched>,
    last_background: Option<Option<BackgroundEffectConfig>>,
    /// Identity of the composition function the slots belong to.
    composition_fn: Option<usize>,
    compositions: BTreeMap<String, CompositionSlot>,
    last_compositions: HashMap<String, OutputComposition>,
    last_frame_pacing: Option<FramePacingConfig>,
    last_schedule: Option<RuntimeSchedule>,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

fn caught<R>(what: &str, f: impl FnOnce() -> R) -> Option<R> {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(value) => Some(value),
        Err(payload) => {
            let message = payload
                .downcast_ref::<&str>()
                .map(|message| (*message).to_owned())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_default();
            tracing::error!(%message, "{what} panicked");
            None
        }
    }
}

/// `COMPOSITOR.effect.background_effect`, resolved and watched from now on.
pub(crate) fn background_effect() -> Option<BackgroundEffectConfig> {
    let watched = STATE.with(|state| state.borrow_mut().background.take()).unwrap_or_else(Watched::new);
    let config = resolve_background(&watched);
    STATE.with(|state| {
        let mut state = state.borrow_mut();
        state.background = Some(watched);
        state.last_background = Some(config.clone());
    });
    config
}

fn resolve_background(watched: &Watched) -> Option<BackgroundEffectConfig> {
    let factory = with_registry(|registry| registry.background_effect.clone())?;
    watched
        .track(|| {
            caught("COMPOSITOR.effect.background", || {
                factory().map(|effect| BackgroundEffectConfig {
                    effect: effect.compile(),
                })
            })
        })
        .flatten()
}

fn publish_background(messages: &mut Vec<HostMessage>) {
    let Some(watched) = STATE.with(|state| {
        let mut state = state.borrow_mut();
        match &state.background {
            Some(watched) if watched.dirty.get() => state.background.take(),
            _ => None,
        }
    }) else {
        return;
    };
    let config = resolve_background(&watched);
    let changed = STATE.with(|state| {
        let mut state = state.borrow_mut();
        state.background = Some(watched);
        let changed = state.last_background.as_ref() != Some(&config);
        state.last_background = Some(config.clone());
        changed
    });
    if changed {
        messages.push(HostMessage::BackgroundEffect(config));
    }
}

fn enabled_outputs() -> Vec<WaylandOutputSnapshot> {
    untrack(|| {
        runtime::global().outputs.with(|outputs| {
            outputs
                .values()
                .filter(|output| output.enabled)
                .cloned()
                .collect()
        })
    })
}

fn publish_compositions(messages: &mut Vec<HostMessage>) {
    let compose = with_registry(|registry| registry.output_composition.clone());
    let identity = compose.as_ref().map(|compose| Rc::as_ptr(compose) as *const () as usize);
    let stale = STATE.with(|state| {
        let mut state = state.borrow_mut();
        if state.composition_fn == identity {
            return BTreeMap::new();
        }
        state.composition_fn = identity;
        std::mem::take(&mut state.compositions)
    });
    stale.into_values().for_each(|slot| slot.watched.dispose());

    if let Some(compose) = compose {
        let outputs = enabled_outputs();
        let gone: Vec<CompositionSlot> = STATE.with(|state| {
            let mut state = state.borrow_mut();
            let names: Vec<String> = state
                .compositions
                .keys()
                .filter(|name| !outputs.iter().any(|output| &output.name == *name))
                .cloned()
                .collect();
            names
                .iter()
                .filter_map(|name| state.compositions.remove(name))
                .collect()
        });
        gone.into_iter().for_each(|slot| slot.watched.dispose());
        for output in outputs {
            let slot = STATE.with(|state| state.borrow_mut().compositions.remove(&output.name));
            let mut slot = match slot {
                Some(slot) if slot.output == output => slot,
                other => {
                    if let Some(old) = other {
                        old.watched.dispose();
                    }
                    CompositionSlot {
                    output: output.clone(),
                    watched: Watched::new(),
                    plan: None,
                    }
                }
            };
            if slot.watched.dirty.get() {
                let stack = slot.watched.track(|| {
                    caught("COMPOSITOR.rendering.composition", || compose(&output))
                });
                slot.plan = stack.and_then(|stack| match stack.compile() {
                    Ok(plan) => Some(plan),
                    Err(error) => {
                        tracing::error!(output = %output.name, %error, "invalid output composition");
                        None
                    }
                });
            }
            STATE.with(|state| state.borrow_mut().compositions.insert(output.name.clone(), slot));
        }
    }

    STATE.with(|state| {
        let mut state = state.borrow_mut();
        let plans: HashMap<String, OutputComposition> = state
            .compositions
            .iter()
            .filter_map(|(name, slot)| Some((name.clone(), slot.plan.clone()?)))
            .collect();
        if plans != state.last_compositions {
            state.last_compositions = plans.clone();
            messages.push(HostMessage::OutputCompositions(plans));
        }
    });
}

fn publish_frame_pacing(messages: &mut Vec<HostMessage>) {
    let pacing = with_registry(|registry| registry.frame_pacing.clone());
    let mut config = FramePacingConfig::default();
    if let Some(pacing) = pacing {
        for output in enabled_outputs() {
            if let Some(value) = untrack(|| caught("COMPOSITOR.rendering.frame_pacing", || pacing(&output))) {
                config.outputs.insert(output.name, value);
            }
        }
    }
    STATE.with(|state| {
        let mut state = state.borrow_mut();
        if state.last_frame_pacing.as_ref() != Some(&config) {
            state.last_frame_pacing = Some(config.clone());
            messages.push(HostMessage::FramePacing(config));
        }
    });
}

/// Send whatever changed. Called at the end of every runtime turn.
pub(crate) fn publish() {
    let mut messages = Vec::new();
    publish_compositions(&mut messages);
    publish_background(&mut messages);
    publish_frame_pacing(&mut messages);
    crate::overlay::sync();
    crate::dnd::sync();
    // Last: evaluating compositions can start polls the schedule must see.
    let schedule = crate::animation::schedule();
    STATE.with(|state| {
        let mut state = state.borrow_mut();
        if state.last_schedule.as_ref() != Some(&schedule) {
            state.last_schedule = Some(schedule.clone());
            messages.push(HostMessage::Schedule(schedule));
        }
    });
    if !messages.is_empty() {
        runtime::send_all(messages);
    }
}

pub(crate) fn reset() {
    let state = STATE.with(|state| std::mem::take(&mut *state.borrow_mut()));
    if let Some(watched) = state.background {
        watched.dispose();
    }
    state
        .compositions
        .into_values()
        .for_each(|slot| slot.watched.dispose());
}
