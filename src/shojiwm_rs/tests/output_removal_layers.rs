//! Removing an output closes its layer surfaces and forgets them. A layer a
//! client asks for on the removed output, before it learns the output is
//! gone, is closed too, not placed on another output.

mod common;

use shojiwm_rs::prelude::*;
use smithay::desktop::layer_map_for_output;
use wayland_client::protocol::wl_output;

fn setup() {
    COMPOSITOR.output.create(HeadlessOutputOptions {
        width: 800,
        height: 600,
        ..Default::default()
    });
}

#[test]
fn removing_an_output_closes_its_layers_and_moves_none() {
    let mut session = common::Session::new(setup);
    session.roundtrip();
    let outputs = session.client.globals_of("wl_output");
    let headless: wl_output::WlOutput = session.bind_global(outputs[1], 1);
    session.map_layer_on("wallpaper", Some(&headless));
    session
        .state
        .refresh_layer_effects_for_output("HEADLESS-1")
        .unwrap();
    let layers = COMPOSITOR.layer.list();
    assert_eq!(layers.len(), 1);
    assert_eq!(layers[0].output_name, "HEADLESS-1");

    session.state.remove_headless_output("HEADLESS-1").unwrap();
    // The client still holds the wl_output of the removed output.
    session.map_layer_on("wallpaper", Some(&headless));
    session.roundtrip();

    assert_eq!(session.client.closed_layers, 2);
    let remaining = session.state.space.outputs().next().cloned().unwrap();
    assert_eq!(remaining.name(), "TEST-1");
    assert_eq!(
        layer_map_for_output(&remaining).layers().count(),
        0,
        "a layer of the removed output landed on TEST-1",
    );
    session
        .state
        .refresh_layer_effects_for_output("TEST-1")
        .unwrap();
    assert_eq!(COMPOSITOR.layer.list().len(), 0);
}
