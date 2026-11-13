//! A layer surface that holds the keyboard keeps it when it drops from
//! exclusive to on-demand keyboard interactivity, as a shell panel does after
//! it maps.

mod common;

use smithay::{
    backend::input::{KeyState, Keycode},
    input::keyboard::FilterResult,
    utils::SERIAL_COUNTER,
};
use wayland_client::Proxy;
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_surface_v1::KeyboardInteractivity;

fn setup() {}

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

#[test]
fn a_layer_that_turns_on_demand_while_its_popup_holds_the_keyboard_keeps_it() {
    let mut session = common::Session::new(setup);
    session.map_toplevel_with_buffer("behind", 400, 300);
    let bar = session.map_layer("bar");

    let (panel, layer) = session.map_keyboard_layer("panel", KeyboardInteractivity::Exclusive);
    let (menu, popup) = session.open_grabbing_layer_popup(&layer);
    // The popup grab moves the keyboard onto the menu with the next key.
    tap_key(&mut session);
    assert_eq!(keyboard_focus(&session), Some(menu.id().protocol_id()));

    layer.set_keyboard_interactivity(KeyboardInteractivity::OnDemand);
    panel.commit();
    session.roundtrip();

    // Once the menu is closed and the next key has ended the grab, a commit of
    // another layer, which elects the keyboard focus again, leaves it on the panel.
    popup.destroy();
    menu.destroy();
    session.roundtrip();
    // What the backend does before each render; the grab ends only then.
    session.state.cleanup_popups_with_debug("test");
    tap_key(&mut session);
    bar.commit();
    session.roundtrip();
    assert_eq!(keyboard_focus(&session), Some(panel.id().protocol_id()));
}
