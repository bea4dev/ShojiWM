//! A layer surface that holds the keyboard keeps it when it drops from
//! exclusive to on-demand keyboard interactivity, as a shell panel does after
//! it maps.

mod common;

use wayland_client::Proxy;
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_surface_v1::KeyboardInteractivity;

fn setup() {}

fn keyboard_focus(session: &common::Session) -> Option<u32> {
    session
        .state
        .seat
        .get_keyboard()
        .and_then(|keyboard| keyboard.current_focus())
        .map(|surface| smithay::reexports::wayland_server::Resource::id(&surface).protocol_id())
}

#[test]
fn an_exclusive_layer_that_turns_on_demand_keeps_the_keyboard() {
    let mut session = common::Session::new(setup);
    session.map_toplevel_with_buffer("behind", 400, 300);

    let (panel, layer) = session.map_keyboard_layer("panel", KeyboardInteractivity::Exclusive);
    session.roundtrip();
    assert_eq!(keyboard_focus(&session), Some(panel.id().protocol_id()));

    layer.set_keyboard_interactivity(KeyboardInteractivity::OnDemand);
    panel.commit();
    session.roundtrip();
    assert_eq!(keyboard_focus(&session), Some(panel.id().protocol_id()));

    // A later commit that changes nothing keeps it too.
    panel.commit();
    session.roundtrip();
    assert_eq!(keyboard_focus(&session), Some(panel.id().protocol_id()));
}

#[test]
fn an_exclusive_layer_that_turns_none_gives_the_keyboard_back() {
    let mut session = common::Session::new(setup);
    session.map_toplevel_with_buffer("behind", 400, 300);

    let (panel, layer) = session.map_keyboard_layer("panel", KeyboardInteractivity::Exclusive);
    session.roundtrip();
    assert_eq!(keyboard_focus(&session), Some(panel.id().protocol_id()));

    layer.set_keyboard_interactivity(KeyboardInteractivity::None);
    panel.commit();
    session.roundtrip();
    assert_ne!(keyboard_focus(&session), Some(panel.id().protocol_id()));
}
