//! Port of `packages/config/src/window-animation.ts`: rect animations run by
//! the compositor while the config keeps the declarative target.

use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
};

use shojiwm_rs::prelude::*;

/// The declarative target and token of each running rect animation.
type ActiveTargets = HashMap<(Window, &'static str), (Rect, u64)>;

thread_local! {
    static ACTIVE_TARGETS: RefCell<ActiveTargets> =
        RefCell::new(HashMap::new());
    static TOKEN: Cell<u64> = const { Cell::new(0) };
}

fn channel(key: &WindowStateKey<Rect>) -> String {
    format!("rect:{}", key.name())
}

/// Animate the window's rect state to `to`. The state jumps to the target
/// immediately; the compositor interpolates what is shown.
pub fn play_rect_animation(
    window: Window,
    key: &'static WindowStateKey<Rect>,
    to: Rect,
    easing: Easing,
    duration_ms: f64,
) {
    let rect = window.state(key);
    let from = rect.get_untracked();

    // Layout and focus updates can ask for the same target repeatedly while
    // the compositor is already interpolating toward it; re-scheduling would
    // race with those re-evaluations, so a repeated target is a no-op.
    let slot = (window, key.name());
    let same_target = ACTIVE_TARGETS.with(|targets| {
        targets
            .borrow()
            .get(&slot)
            .is_some_and(|(target, _)| *target == to)
    });
    if same_target {
        return;
    }

    rect.set(to);
    let token = TOKEN.with(|token| {
        token.set(token.get() + 1);
        token.get()
    });
    ACTIVE_TARGETS.with(|targets| targets.borrow_mut().insert(slot, (to, token)));
    window.schedule_animation(ManagedAnimation::new(channel(key)).rect(
        Some(from),
        to,
        duration_ms as u64,
        easing,
        AnimationMode::Override,
    ));
    set_timeout(duration_ms, move || {
        ACTIVE_TARGETS.with(|targets| {
            let mut targets = targets.borrow_mut();
            if targets
                .get(&slot)
                .is_some_and(|(_, current)| *current == token)
            {
                targets.remove(&slot);
            }
        });
    });
}

pub fn stop_rect_animation(window: Window, key: &'static WindowStateKey<Rect>) {
    ACTIVE_TARGETS.with(|targets| targets.borrow_mut().remove(&(window, key.name())));
    window.cancel_animation(&channel(key));
}
