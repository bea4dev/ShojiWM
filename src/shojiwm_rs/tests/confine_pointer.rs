//! `ManagedWindow::confine_pointer` keeps the pointer inside the focused
//! window, with no pointer constraint from the client.

mod common;

use std::cell::Cell;

use common::mouse::{Absolute, Button, Motion, Mouse};
use shojiwm_rs::prelude::*;
use smithay::backend::input::InputEvent;
use wayland_protocols::xdg::shell::client::xdg_surface;

thread_local! {
    static CONFINE: Cell<bool> = const { Cell::new(true) };
}

fn setup() {
    COMPOSITOR.window.composition(|_| {
        ManagedWindow::new()
            .confine_pointer(CONFINE.with(Cell::get))
            .child(ClientWindow::new())
    });
    // What the test asks of a window from outside a callback is collected
    // on the scheduler's next turn, and a timer keeps it turning.
    set_interval(1.0, || {});
}

/// Maps a window, focuses it and puts the pointer at its centre. Returns the
/// window's xdg surface and right edge.
fn point_at_focused_window(session: &mut common::Session) -> (xdg_surface::XdgSurface, f64) {
    let (_, xdg) = session.map_toplevel_with_xdg_surface("game", 300, 200);
    session.state.refresh_window_decorations().unwrap();
    COMPOSITOR.window.list()[0].focus();
    session.roundtrip();
    session.state.refresh_window_decorations().unwrap();

    let window = session.state.space.elements().next().cloned().unwrap();
    let geometry = session.state.space.element_geometry(&window).unwrap();
    let centre = geometry.loc.to_f64() + geometry.size.to_f64().downscale(2.0).to_point();
    session
        .state
        .process_input_event(InputEvent::<Mouse>::PointerMotionAbsolute {
            event: Absolute {
                time: 0,
                x: centre.x / 1024.0,
                y: centre.y / 768.0,
            },
        });
    (xdg, f64::from(geometry.loc.x + geometry.size.w))
}

/// Moves the pointer far to the right and returns where it ended up.
fn push_right(session: &mut common::Session) -> f64 {
    session
        .state
        .process_input_event(InputEvent::<Mouse>::PointerMotion {
            event: Motion {
                time: 1,
                dx: 2000.0,
                dy: 0.0,
            },
        });
    let pointer = session.state.seat.get_pointer().unwrap();
    pointer.current_location().x
}

/// Puts the pointer at the centre of the only window, focuses that window,
/// then moves the pointer far to the right. Returns the window's right edge
/// and where the pointer ended up.
fn push_pointer_out(session: &mut common::Session) -> (f64, f64) {
    let (_, right) = point_at_focused_window(session);
    (right, push_right(session))
}

#[test]
fn the_pointer_stops_at_the_edge_of_a_confining_window() {
    CONFINE.with(|confine| confine.set(true));
    let mut session = common::Session::new(setup);
    let (right, x) = push_pointer_out(&mut session);
    assert!(x < right, "pointer at {x} left a window ending at {right}");
    assert!(x > right - 1.0, "pointer at {x} stopped short of {right}");
}

#[test]
fn without_the_prop_the_pointer_leaves_the_window() {
    CONFINE.with(|confine| confine.set(false));
    let mut session = common::Session::new(setup);
    let (right, x) = push_pointer_out(&mut session);
    assert!(
        x > right,
        "pointer at {x} stayed inside a window ending at {right}"
    );
}

#[test]
fn a_held_button_keeps_the_pointer_in() {
    CONFINE.with(|confine| confine.set(true));
    let mut session = common::Session::new(setup);
    let (_, right) = point_at_focused_window(&mut session);
    // BTN_LEFT, held like a game's fire or dig button.
    session
        .state
        .process_input_event(InputEvent::<Mouse>::PointerButton {
            event: Button {
                time: 1,
                code: 0x110,
                pressed: true,
            },
        });
    let x = push_right(&mut session);
    assert!(x < right, "pointer at {x} left a window ending at {right}");
}

#[test]
fn the_pointer_reaches_a_menu_outside_the_window() {
    CONFINE.with(|confine| confine.set(true));
    let mut session = common::Session::new(setup);
    let (xdg, right) = point_at_focused_window(&mut session);
    // The menu's grab holds the pointer; the menu can hang outside the window.
    session.open_grabbing_window_popup(&xdg);
    let x = push_right(&mut session);
    assert!(
        x > right,
        "pointer at {x} stayed inside a window ending at {right}"
    );
}
