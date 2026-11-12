//! Keys from a `zwp_virtual_keyboard_v1` client reach key binds, matched by
//! the keysyms of the keymap the client uploaded, the way `wtype` sends them.
//! Keys from the input method's own virtual keyboard go to the focused client
//! instead.

mod common;

use std::{cell::RefCell, io::Write, os::fd::AsFd};

use shojiwm_rs::prelude::*;
use wayland_client::{
    Connection, Dispatch, QueueHandle,
    protocol::{
        wl_keyboard::{self, KeymapFormat, WlKeyboard},
        wl_seat,
    },
};
use wayland_protocols_misc::zwp_input_method_v2::client::{
    zwp_input_method_keyboard_grab_v2::{self, ZwpInputMethodKeyboardGrabV2},
    zwp_input_method_manager_v2::ZwpInputMethodManagerV2,
    zwp_input_method_v2::ZwpInputMethodV2,
};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::{
    zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1,
    zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1,
};

thread_local! {
    static FIRED: RefCell<Vec<&'static str>> = const { RefCell::new(Vec::new()) };
    /// Key presses as `(evdev code, sent to)`.
    static KEYS: RefCell<Vec<(u32, &'static str)>> = const { RefCell::new(Vec::new()) };
}

fn fired(name: &'static str) -> impl Fn() + 'static {
    move || FIRED.with(|fired| fired.borrow_mut().push(name))
}

fn setup() {
    COMPOSITOR
        .key
        .bind("space", "Super+space", fired("super-space"));
    COMPOSITOR
        .key
        .bind_release("f13", "F13", fired("f13-release"));
    COMPOSITOR
        .key
        .bind("digit", "Super+code:10", fired("super-code-10"));
}

/// Like wtype's: only the keysyms about to be typed, on keycodes of its own
/// choosing. None of them sit where the session's US layout has them.
const KEYMAP: &str = r#"xkb_keymap {
xkb_keycodes "(unnamed)" { minimum = 8; maximum = 11; <K1> = 9; <K2> = 10; <K3> = 11; };
xkb_types "(unnamed)" { include "complete" };
xkb_compat "(unnamed)" { include "complete" };
xkb_symbols "(unnamed)" { key <K1> {[space]}; key <K2> {[F13]}; key <K3> {[1]}; };
};
"#;
const SPACE: u32 = 1;
/// On xkb keycode 10, where the session's US layout has `1`.
const F13: u32 = 2;
const ONE: u32 = 3;
/// Mod4, the logo modifier, in the keymap above.
const LOGO: u32 = 1 << 6;

fn tap(session: &mut common::Session, keyboard: &ZwpVirtualKeyboardV1, key: u32, mods: u32) {
    if mods != 0 {
        keyboard.modifiers(mods, 0, 0, 0);
    }
    keyboard.key(0, key, 1);
    keyboard.key(0, key, 0);
    if mods != 0 {
        keyboard.modifiers(0, 0, 0, 0);
    }
    session.roundtrip();
}

fn virtual_keyboard(session: &mut common::Session, seat: &wl_seat::WlSeat) -> ZwpVirtualKeyboardV1 {
    let manager: ZwpVirtualKeyboardManagerV1 = session.bind("zwp_virtual_keyboard_manager_v1", 1);
    let keyboard = manager.create_virtual_keyboard(seat, &session.queue.handle(), ());
    let mut file = common::tempfile("keymap");
    file.write_all(KEYMAP.as_bytes()).unwrap();
    file.write_all(&[0]).unwrap();
    keyboard.keymap(
        KeymapFormat::XkbV1 as u32,
        file.as_fd(),
        KEYMAP.len() as u32 + 1,
    );
    session.roundtrip();
    keyboard
}

#[test]
fn virtual_keyboard_keys_fire_binds_by_their_own_keysyms() {
    let mut session = common::Session::new(setup);
    session.map_toplevel();
    let seat: wl_seat::WlSeat = session.bind("wl_seat", 1);
    let keyboard = virtual_keyboard(&mut session, &seat);

    tap(&mut session, &keyboard, SPACE, LOGO);
    tap(&mut session, &keyboard, SPACE, 0);
    tap(&mut session, &keyboard, F13, 0);
    assert_eq!(
        FIRED.with(|fired| fired.borrow().clone()),
        vec!["super-space", "f13-release"]
    );
    assert!(session.state.virtual_keymap_active());

    keyboard.destroy();
    session.roundtrip();
    assert!(!session.state.virtual_keymap_active());
}

#[test]
fn code_binds_match_the_session_key_of_a_virtual_keysym() {
    let mut session = common::Session::new(setup);
    session.map_toplevel();
    let seat: wl_seat::WlSeat = session.bind("wl_seat", 1);
    let keyboard = virtual_keyboard(&mut session, &seat);

    // F13 sits on the virtual keycode 10, `1` on 11; `code:10` names the
    // session's `1` key.
    tap(&mut session, &keyboard, F13, LOGO);
    tap(&mut session, &keyboard, ONE, LOGO);
    assert_eq!(
        FIRED.with(|fired| fired.borrow().clone()),
        vec!["super-code-10"]
    );
}

/// fcitx5's shape: the input method grabs the keyboard and sends the keys it
/// does not handle back through a virtual keyboard of its own.
#[test]
fn input_method_virtual_keyboard_keys_skip_its_keyboard_grab() {
    let mut session = common::Session::new(setup);
    session.map_toplevel();
    let qh = session.queue.handle();
    let seat: wl_seat::WlSeat = session.bind("wl_seat", 1);
    seat.get_keyboard(&qh, ());
    let manager: ZwpInputMethodManagerV2 = session.bind("zwp_input_method_manager_v2", 1);
    let input_method = manager.get_input_method(&seat, &qh, ());
    input_method.grab_keyboard(&qh, ());
    let keyboard = virtual_keyboard(&mut session, &seat);
    focus_the_window(&mut session);
    KEYS.with(|keys| keys.borrow_mut().clear());

    tap(&mut session, &keyboard, SPACE, LOGO);
    tap(&mut session, &keyboard, ONE, 0);
    assert_eq!(
        KEYS.with(|keys| keys.borrow().clone()),
        vec![(SPACE, "wl_keyboard"), (ONE, "wl_keyboard")]
    );
    assert!(FIRED.with(|fired| fired.borrow().is_empty()));
    assert!(!session.state.virtual_keymap_active());
}

fn focus_the_window(session: &mut common::Session) {
    let surface = session
        .state
        .space
        .elements()
        .next()
        .and_then(|window| window.toplevel())
        .map(|toplevel| toplevel.wl_surface().clone());
    assert!(surface.is_some());
    let keyboard = session.state.seat.get_keyboard().unwrap();
    keyboard.set_focus(
        &mut session.state,
        surface,
        smithay::utils::SERIAL_COUNTER.next_serial(),
    );
    session.roundtrip();
}

impl Dispatch<WlKeyboard, ()> for common::Client {
    fn event(
        _: &mut Self,
        _: &WlKeyboard,
        event: wl_keyboard::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_keyboard::Event::Key {
            key,
            state: wayland_client::WEnum::Value(wl_keyboard::KeyState::Pressed),
            ..
        } = event
        {
            KEYS.with(|keys| keys.borrow_mut().push((key, "wl_keyboard")));
        }
    }
}

impl Dispatch<ZwpInputMethodKeyboardGrabV2, ()> for common::Client {
    fn event(
        _: &mut Self,
        _: &ZwpInputMethodKeyboardGrabV2,
        event: zwp_input_method_keyboard_grab_v2::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwp_input_method_keyboard_grab_v2::Event::Key {
            key,
            state: wayland_client::WEnum::Value(wl_keyboard::KeyState::Pressed),
            ..
        } = event
        {
            KEYS.with(|keys| keys.borrow_mut().push((key, "grab")));
        }
    }
}

impl Dispatch<ZwpInputMethodManagerV2, ()> for common::Client {
    fn event(
        _: &mut Self,
        _: &ZwpInputMethodManagerV2,
        _: <ZwpInputMethodManagerV2 as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwpInputMethodV2, ()> for common::Client {
    fn event(
        _: &mut Self,
        _: &ZwpInputMethodV2,
        _: <ZwpInputMethodV2 as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}
