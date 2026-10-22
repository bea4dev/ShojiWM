//! Stacking order of windows (`createWindowStack`): a z-index signal per
//! window that compositions read, kept in raise/lower order.

use std::{cell::RefCell, collections::HashMap, rc::Rc};

use crate::{
    reactive::{ReadSignal, Scope, Signal, untrack},
    window::Window,
};

/// Where [`WindowStack::add`] puts a window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    Front,
    Back,
}

struct Inner {
    base: i32,
    step: i32,
    order: Vec<Window>,
    z_index: HashMap<Window, Signal<i32>>,
    scope: Scope,
}

/// A front-to-back ordering of windows. Clone it to share.
#[derive(Clone)]
pub struct WindowStack(Rc<RefCell<Inner>>);

impl Default for WindowStack {
    fn default() -> Self {
        Self::new(0, 1)
    }
}

impl WindowStack {
    pub fn new(base_z_index: i32, step: i32) -> Self {
        Self(Rc::new(RefCell::new(Inner {
            base: base_z_index,
            step,
            order: Vec::new(),
            z_index: HashMap::new(),
            scope: untrack(Scope::root),
        })))
    }

    fn signal(&self, window: Window) -> Signal<i32> {
        let mut inner = self.0.borrow_mut();
        let (base, scope) = (inner.base, inner.scope);
        *inner
            .z_index
            .entry(window)
            .or_insert_with(|| scope.signal(base))
    }

    fn value(&self, window: Window) -> i32 {
        let inner = self.0.borrow();
        inner
            .z_index
            .get(&window)
            .map(|signal| signal.get_untracked())
            .unwrap_or(inner.base)
    }

    fn move_to(&self, window: Window, placement: Placement) {
        let signal = self.signal(window);
        let next = {
            let mut inner = self.0.borrow_mut();
            let current = inner
                .order
                .iter()
                .position(|candidate| *candidate == window);
            let target = match placement {
                Placement::Front => inner.order.len().saturating_sub(1),
                Placement::Back => 0,
            };
            if current.is_some() && current == Some(target) {
                return;
            }
            inner.order.retain(|candidate| *candidate != window);
            let edge = match placement {
                Placement::Front => inner.order.last(),
                Placement::Back => inner.order.first(),
            }
            .copied();
            let next = match edge {
                None => inner.base,
                Some(edge) => {
                    let edge_value = inner
                        .z_index
                        .get(&edge)
                        .map(|signal| signal.get_untracked())
                        .unwrap_or(inner.base);
                    match placement {
                        Placement::Front => edge_value + inner.step,
                        Placement::Back => edge_value - inner.step,
                    }
                }
            };
            match placement {
                Placement::Front => inner.order.push(window),
                Placement::Back => inner.order.insert(0, window),
            }
            next
        };
        signal.set(next);
    }

    pub fn add(&self, window: Window, placement: Placement) {
        self.move_to(window, placement);
    }

    pub fn remove(&self, window: Window) {
        let mut inner = self.0.borrow_mut();
        inner.order.retain(|candidate| *candidate != window);
        if let Some(signal) = inner.z_index.remove(&window) {
            signal.dispose();
        }
    }

    pub fn has(&self, window: Window) -> bool {
        self.0.borrow().order.contains(&window)
    }

    pub fn raise(&self, window: Window) {
        self.move_to(window, Placement::Front);
    }

    pub fn lower(&self, window: Window) {
        self.move_to(window, Placement::Back);
    }

    /// The window's z-index, as a signal a composition can bind.
    pub fn z_index(&self, window: Window) -> ReadSignal<i32> {
        self.signal(window).read_only()
    }

    pub fn z_index_value(&self, window: Window) -> i32 {
        self.value(window)
    }

    /// Back to front.
    pub fn windows(&self) -> Vec<Window> {
        self.0.borrow().order.clone()
    }

    pub fn front(&self) -> Option<Window> {
        self.0.borrow().order.last().copied()
    }
}
