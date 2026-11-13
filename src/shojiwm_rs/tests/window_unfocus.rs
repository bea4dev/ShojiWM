//! `window.unfocus()` takes the keyboard from the window that has it and
//! hands it to no other.

mod common;

use shojiwm_rs::prelude::*;
use smithay::{
    backend::input::{KeyState, Keycode},
    input::keyboard::FilterResult,
    utils::SERIAL_COUNTER,
};

fn setup() {
    COMPOSITOR.window.composition(|_| ClientWindow::new());
    // What the test asks of a window from outside a callback is collected
    // on the scheduler's next turn, and a timer keeps it turning.
    set_interval(1.0, || {});
}

fn tap_key(session: &mut common::Session) {
    let keyboard = session.state.seat.get_keyboard().unwrap();
    for state in [KeyState::Pressed, KeyState::Released] {
        keyboard.input::<(), _>(
            &mut session.state,
            Keycode::new(30 + 8),
            state,
            SERIAL_COUNTER.next_serial(),
            0,
            |_, _, _| FilterResult::Forward,
        );
    }
    session.roundtrip();
}

fn typing(session: &common::Session) -> bool {
    let keyboard = session.state.seat.get_keyboard().unwrap();
    keyboard.current_focus().is_some()
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

#[test]
fn unfocus_closes_the_menu_that_holds_the_keyboard() {
    let mut session = common::Session::new(setup);
    let (_, parent) = session.map_toplevel_with_xdg_surface("a", 300, 300);
    session.state.refresh_window_decorations().unwrap();
    let window = COMPOSITOR.window.list()[0];
    window.focus();
    session.roundtrip();

    let (menu, popup) = session.open_grabbing_window_popup(&parent);
    // The popup grab moves the keyboard onto the menu with the next key.
    tap_key(&mut session);
    assert!(typing(&session));

    window.unfocus();
    session.roundtrip();
    assert!(!typing(&session), "the menu lets the keyboard go");

    // The client destroys the menu it was told is done. The grab that held it
    // does not hand the keyboard back to the window when it ends.
    popup.destroy();
    menu.destroy();
    session.roundtrip();
    // What the backend does before each render.
    session.state.cleanup_popups_with_debug("test");
    tap_key(&mut session);
    assert_eq!(session.state.window_keyboard_focus_owner, None);
    assert!(!typing(&session));
}

#[test]
fn unfocus_behind_the_lock_screen_holds_after_unlock() {
    let mut session = common::Session::new(setup);
    session.map_toplevel_with_buffer("a", 200, 200);
    session.state.refresh_window_decorations().unwrap();
    let window = COMPOSITOR.window.list()[0];
    window.focus();
    session.roundtrip();
    assert!(session.state.window_keyboard_focus_owner.is_some());

    let lock = session.lock_session();
    assert!(COMPOSITOR.session.is_locked());
    window.unfocus();
    session.roundtrip();

    lock.unlock_and_destroy();
    session.roundtrip();
    assert!(!COMPOSITOR.session.is_locked());
    assert_eq!(session.state.window_keyboard_focus_owner, None);
    assert!(!typing(&session));
}
