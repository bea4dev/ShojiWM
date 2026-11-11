//! An output made by the config with no connector behind it is an ordinary
//! output: it gets a wl_output, the display configuration places it right of
//! the connected outputs, the config sees it, layer surfaces can target it,
//! and removing it takes all of that away again.

mod common;

use std::sync::Mutex;

use shojiwm_rs::prelude::*;
use smithay::{desktop::layer_map_for_output, utils::Point};
use wayland_client::protocol::wl_output;

static SEEN: Mutex<Vec<Vec<String>>> = Mutex::new(Vec::new());

fn setup() {
    COMPOSITOR.event.on_output_change(|_| {
        let mut outputs = COMPOSITOR.output.list();
        outputs.sort();
        SEEN.lock().unwrap().push(outputs);
    });
    let name = COMPOSITOR.output.create(HeadlessOutputOptions {
        width: 800,
        height: 600,
        ..Default::default()
    });
    assert_eq!(name, "HEADLESS-1");
}

#[test]
fn headless_output_is_an_ordinary_output() {
    let mut session = common::Session::new(setup);
    session.roundtrip();

    let output = session
        .state
        .space
        .outputs()
        .find(|output| output.name() == "HEADLESS-1")
        .cloned()
        .expect("the headless output is mapped");
    let geometry = session.state.space.output_geometry(&output).unwrap();
    assert_eq!(geometry.loc, Point::from((1024, 0)));
    assert_eq!(geometry.size, (800, 600).into());
    assert_eq!(
        session
            .state
            .space
            .output_under((1100.0, 100.0))
            .next()
            .map(|output| output.name()),
        Some("HEADLESS-1".to_string()),
    );
    assert!(
        SEEN.lock()
            .unwrap()
            .iter()
            .any(|outputs| outputs == &["HEADLESS-1", "TEST-1"]),
        "the config never saw the headless output",
    );

    let outputs = session.client.globals_of("wl_output");
    assert_eq!(outputs.len(), 2);
    let headless: wl_output::WlOutput = session.bind_global(outputs[1], 1);
    session.map_layer_on("panel", Some(&headless));
    session
        .state
        .refresh_layer_effects_for_output("HEADLESS-1")
        .unwrap();
    assert_eq!(layer_map_for_output(&output).layers().count(), 1);

    session.state.remove_headless_output("HEADLESS-1").unwrap();
    session.roundtrip();
    assert_eq!(session.state.space.outputs().count(), 1);
    assert_eq!(session.client.globals_of("wl_output").len(), 1);
    assert_eq!(SEEN.lock().unwrap().last().unwrap(), &["TEST-1"]);
    assert!(session.state.remove_headless_output("TEST-1").is_err());
}
