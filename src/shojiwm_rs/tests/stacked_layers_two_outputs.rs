//! `COMPOSITOR.layer.stacked()` names each layer once, however many outputs
//! there are: the core hands every output's evaluation all the layers.

mod common;

use shojiwm_rs::prelude::*;
use smithay::output::{Mode, Output, PhysicalProperties, Subpixel};
use wayland_client::protocol::wl_output;

fn setup() {}

/// A second output beside the session's TEST-1.
fn add_output(session: &mut common::Session, name: &str) {
    let output = Output::new(
        name.into(),
        PhysicalProperties {
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: "test".into(),
            model: "test".into(),
            serial_number: String::new(),
        },
    );
    let mode = Mode {
        size: (800, 600).into(),
        refresh: 60_000,
    };
    output.change_current_state(Some(mode), None, None, Some((1024, 0).into()));
    output.set_preferred(mode);
    session.state.create_output_global(&output);
    session.state.space.map_output(&output, (1024, 0));
    session.roundtrip();
}

#[test]
fn a_layer_is_stacked_once_with_two_outputs() {
    let mut session = common::Session::new(setup);
    add_output(&mut session, "TEST-2");
    let outputs = session.client.globals_of("wl_output");
    let first: wl_output::WlOutput = session.bind_global(outputs[0], 1);
    let second: wl_output::WlOutput = session.bind_global(outputs[1], 1);
    session.map_layer_on("bar", Some(&first));
    session.map_layer_on("wallpaper", Some(&second));

    let stacked = |session: &mut common::Session| {
        for output in ["TEST-1", "TEST-2", "TEST-1"] {
            session
                .state
                .refresh_layer_effects_for_output(output)
                .unwrap();
        }
        let mut names: Vec<(String, String)> = COMPOSITOR
            .layer
            .stacked()
            .into_iter()
            .map(|layer| (layer.output_name, layer.namespace.unwrap_or_default()))
            .collect();
        names.sort();
        names
    };
    let both = [
        ("TEST-1".to_owned(), "bar".to_owned()),
        ("TEST-2".to_owned(), "wallpaper".to_owned()),
    ];
    assert_eq!(stacked(&mut session), both);

    // A layer more on one output is evaluated for both again.
    session.map_layer_on("osd", Some(&second));
    let names = stacked(&mut session);
    assert_eq!(names.len(), 3, "{names:?}");
    assert!(
        names.contains(&("TEST-2".to_owned(), "osd".to_owned())),
        "{names:?}"
    );
}
