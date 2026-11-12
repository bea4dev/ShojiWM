//! The config is told which window has the seat's keyboard, and that none
//! has while a layer surface holds it.

mod common;

use std::cell::RefCell;

use shojiwm_rs::prelude::*;
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_surface_v1::KeyboardInteractivity;

thread_local! {
    static HEARD: RefCell<Vec<Option<String>>> = const { RefCell::new(Vec::new()) };
}

fn setup() {
    COMPOSITOR.window.composition(|_| ClientWindow::new());
    COMPOSITOR.event.on_keyboard_focus_change(|focus| {
        HEARD.with(|heard| heard.borrow_mut().push(focus.window_id.clone()))
    });
    // What the test asks of a window from outside a callback is collected
    // on the scheduler's next turn, and a timer keeps it turning.
    set_interval(1.0, || {});
}

fn heard() -> Vec<Option<String>> {
    HEARD.with(|heard| heard.borrow().clone())
}

fn held() -> Option<String> {
    COMPOSITOR
        .input
        .keyboard_focus()
        .get_untracked()
        .and_then(|focus| focus.window_id)
}

#[test]
fn the_window_with_the_keyboard_reaches_the_config() {
    let mut session = common::Session::new(setup);
    session.map_toplevel_with_buffer("a", 200, 200);
    session.map_toplevel_with_buffer("b", 200, 200);
    session.state.refresh_window_decorations().unwrap();
    let windows = COMPOSITOR.window.list();
    let (a, b) = (windows[0], windows[1]);

    a.focus();
    session.roundtrip();
    assert_eq!(held(), Some(a.id()));
    b.focus();
    session.roundtrip();
    assert_eq!(held(), Some(b.id()));
    assert_eq!(heard().last(), Some(&Some(b.id())));

    // A panel that takes the keyboard.
    let (panel, layer) = session.map_keyboard_layer("panel", KeyboardInteractivity::Exclusive);
    session.roundtrip();
    session.state.refresh_window_decorations().unwrap();
    session.roundtrip();
    assert_eq!(held(), None);

    // The panel lets go: the keyboard is the window's again.
    layer.set_keyboard_interactivity(KeyboardInteractivity::None);
    panel.commit();
    session.roundtrip();
    assert_eq!(held(), Some(b.id()));

    // Every change was heard once, and nothing in between.
    let moves: Vec<Option<String>> = heard().into_iter().skip_while(Option::is_none).collect();
    assert_eq!(moves, [Some(a.id()), Some(b.id()), None, Some(b.id())]);
}
