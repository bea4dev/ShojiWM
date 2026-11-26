//! `focus()` is the config's own initiative, which the compositor refuses
//! for a window that opened before the one the keyboard is meant for while
//! the user has used neither. `focus_on_request()` is a pick, never refused.

mod common;

use shojiwm_rs::prelude::*;

fn setup() {
    COMPOSITOR.window.composition(|_| ClientWindow::new());
    // What the test asks of a window from outside a callback is collected
    // on the scheduler's next turn, and a timer keeps it turning.
    set_interval(1.0, || {});
}

/// The window that is focused, once the windows' snapshots caught up.
fn focused(session: &mut common::Session) -> Option<String> {
    session.state.refresh_window_decorations().unwrap();
    session.roundtrip();
    COMPOSITOR
        .window
        .list()
        .into_iter()
        .find(|window| window.is_focused().get_untracked())
        .map(|window| window.id())
}

#[test]
fn a_pick_is_not_refused_for_a_window_that_opened_later() {
    let mut session = common::Session::new(setup);
    session.map_toplevel_with_buffer("older", 200, 200);
    session.map_toplevel_with_buffer("newer", 200, 200);
    session.state.refresh_window_decorations().unwrap();
    let windows = COMPOSITOR.window.list();
    let (older, newer) = (windows[0], windows[1]);

    newer.focus();
    session.roundtrip();
    assert_eq!(focused(&mut session), Some(newer.id()));

    // Nobody has typed into either or pressed on either: the older one
    // asking for itself is refused, and the config can tell.
    older.focus();
    session.roundtrip();
    assert_eq!(focused(&mut session), Some(newer.id()));

    older.focus_on_request();
    session.roundtrip();
    assert_eq!(focused(&mut session), Some(older.id()));
}
