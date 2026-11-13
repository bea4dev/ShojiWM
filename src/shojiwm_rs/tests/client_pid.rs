//! Windows and layer surfaces carry the pid of the client behind them, read
//! from the socket's credentials. The test's client runs in the test process.

mod common;

use std::{cell::RefCell, rc::Rc};

use shojiwm_rs::prelude::*;

thread_local! {
    static LAYER_PIDS: Rc<RefCell<Vec<Option<i32>>>> = Rc::default();
    static WINDOW_PIDS: Rc<RefCell<Vec<Option<i32>>>> = Rc::default();
}

fn setup() {
    COMPOSITOR.event.on_create_layer(|layer| {
        LAYER_PIDS.with(|pids| pids.borrow_mut().push(layer.pid));
    });
    COMPOSITOR.event.on_open(|window| {
        WINDOW_PIDS.with(|pids| pids.borrow_mut().push(window.pid()));
    });
}

#[test]
fn windows_and_layers_carry_the_client_pid() {
    let pid = Some(std::process::id() as i32);
    let mut session = common::Session::new(setup);

    session.map_toplevel();
    let window = session.state.space.elements().next().cloned().unwrap();
    assert_eq!(session.state.snapshot_window(&window).pid, pid);

    session.map_layer("bar");
    let layers = session.state.snapshot_layers();
    assert_eq!(layers.len(), 1);
    assert_eq!(layers[0].pid, pid);
    // The render path hands layers to the config; there is none here.
    session
        .state
        .refresh_layer_effects_for_output("TEST-1")
        .unwrap();

    assert_eq!(LAYER_PIDS.with(|pids| pids.borrow().clone()), vec![pid]);
    assert_eq!(WINDOW_PIDS.with(|pids| pids.borrow().clone()), vec![pid]);
}
