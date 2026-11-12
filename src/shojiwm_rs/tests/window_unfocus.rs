//! `window.unfocus()` takes the keyboard from the window that has it and
//! hands it to no other.

mod common;

use shojiwm_rs::prelude::*;

fn setup() {
    COMPOSITOR.window.composition(|_| ClientWindow::new());
    // What the test asks of a window from outside a callback is collected
    // on the scheduler's next turn, and a timer keeps it turning.
    set_interval(1.0, || {});
}

#[test]
fn unfocus_leaves_the_keyboard_with_no_window() {
    let mut session = common::Session::new(setup);
    session.map_toplevel_with_buffer("a", 200, 200);
    session.map_toplevel_with_buffer("b", 200, 200);
    session.state.refresh_window_decorations().unwrap();
    let windows = COMPOSITOR.window.list();
    let (a, b) = (windows[0], windows[1]);
    let owner = |session: &common::Session| session.state.window_keyboard_focus.clone();
    let typing = |session: &common::Session| {
        let keyboard = session.state.seat.get_keyboard().unwrap();
        keyboard.current_focus().is_some()
    };

    a.focus();
    session.roundtrip();
    let first = owner(&session);
    assert!(first.is_some(), "focus gives it the keyboard");
    assert!(typing(&session));

    // A window that does not have the keyboard takes it from nobody.
    b.unfocus();
    session.roundtrip();
    assert_eq!(owner(&session), first);

    a.unfocus();
    session.roundtrip();
    assert_eq!(owner(&session), None);
    assert!(!typing(&session));

    assert!(!COMPOSITOR.session.is_locked());
}
