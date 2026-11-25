//! Resizing a headless output keeps the output: clients keep their wl_output
//! and see the new mode on it, and its layer surfaces are configured to the
//! new size instead of closed.

mod common;

use shojiwm_rs::prelude::*;
use wayland_client::{Proxy, protocol::wl_output};

fn setup() {
    COMPOSITOR.output.create(HeadlessOutputOptions {
        width: 800,
        height: 600,
        ..Default::default()
    });
    // What the test asks of the compositor from outside a callback is
    // collected on the scheduler's next turn, and a timer keeps it turning.
    set_interval(1.0, || {});
}

#[test]
fn a_resized_headless_output_keeps_its_wl_output_and_layers() {
    let mut session = common::Session::new(setup);
    session.roundtrip();
    let globals = session.client.globals_of("wl_output");
    let headless: wl_output::WlOutput = session.bind_global(globals[1], 2);
    session.map_layer_on("bar", Some(&headless));
    session
        .state
        .refresh_layer_effects_for_output("HEADLESS-1")
        .unwrap();
    assert_eq!(session.client.layer_configures.last(), Some(&(800, 40)));
    let id = headless.id().protocol_id();
    assert_eq!(session.client.output_modes.last(), Some(&(id, 800, 600)));

    COMPOSITOR.output.resize("HEADLESS-1", 1000, 500);
    session.roundtrip();
    session
        .state
        .refresh_layer_effects_for_output("HEADLESS-1")
        .unwrap();

    assert_eq!(session.client.globals_of("wl_output"), globals);
    assert!(headless.is_alive());
    assert_eq!(
        session.client.output_modes.last(),
        Some(&(id, 1000, 500)),
        "the wl_output never saw the new mode",
    );
    assert_eq!(session.client.layer_configures.last(), Some(&(1000, 40)));
    assert_eq!(session.client.closed_layers, 0);
    let resolution = COMPOSITOR
        .output
        .get("HEADLESS-1")
        .unwrap()
        .resolution
        .unwrap();
    assert_eq!((resolution.width, resolution.height), (1000, 500));
    let layers = COMPOSITOR.layer.list();
    assert_eq!(layers.len(), 1);
    assert_eq!(layers[0].output_name, "HEADLESS-1");

    // The size it already has changes nothing.
    let (modes, configures) = (
        session.client.output_modes.len(),
        session.client.layer_configures.len(),
    );
    COMPOSITOR.output.resize("HEADLESS-1", 1000, 500);
    session.roundtrip();
    assert_eq!(session.client.output_modes.len(), modes);
    assert_eq!(session.client.layer_configures.len(), configures);
}
