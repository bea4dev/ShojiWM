//! A `transparent` modifier release binding fires whenever the modifier goes
//! up in the keyboard's modifier state: after a chord such as Alt+Tab, after a
//! bare tap, and when a virtual keyboard lets go of it with a `modifiers`
//! request and no key event, the way `wtype -M alt -k Tab -m alt` does.

mod common;

use std::{cell::RefCell, fs::File, io::Write, os::fd::AsFd};

use shojiwm_rs::prelude::*;
use wayland_client::protocol::{wl_keyboard::KeymapFormat, wl_seat};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::{
    zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1,
    zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1,
};

thread_local! {
    static FIRED: RefCell<Vec<&'static str>> = const { RefCell::new(Vec::new()) };
}

fn fired(name: &'static str) -> impl Fn() + 'static {
    move || FIRED.with(|fired| fired.borrow_mut().push(name))
}

fn take_fired() -> Vec<&'static str> {
    FIRED.with(|fired| std::mem::take(&mut *fired.borrow_mut()))
}

fn setup() {
    COMPOSITOR.key.bind("switch", "Alt+Tab", fired("switch"));
    COMPOSITOR.key.bind_with(
        "commit",
        "Alt",
        KeyBindOptions::new().on_release().transparent(),
        fired("commit"),
    );
    COMPOSITOR.key.bind_release("tap", "Alt", fired("tap"));
}

const KEYMAP: &str = r#"xkb_keymap {
xkb_keycodes "(unnamed)" { minimum = 8; maximum = 10; <K1> = 9; <K2> = 10; };
xkb_types "(unnamed)" { include "complete" };
xkb_compat "(unnamed)" { include "complete" };
xkb_symbols "(unnamed)" { key <K1> {[Tab]}; key <K2> {[Alt_L]}; modifier_map Mod1 { <K2> }; };
};
"#;
const TAB: u32 = 1;
const ALT_L: u32 = 2;
/// Mod1, the alt modifier.
const ALT: u32 = 1 << 3;

fn key(keyboard: &ZwpVirtualKeyboardV1, key: u32) {
    keyboard.key(0, key, 1);
    keyboard.key(0, key, 0);
}

#[test]
fn transparent_release_fires_whenever_the_modifier_goes_up() {
    let mut session = common::Session::new(setup);
    session.map_toplevel();
    let qh = session.queue.handle();
    let seat: wl_seat::WlSeat = session.bind("wl_seat", 1);
    let manager: ZwpVirtualKeyboardManagerV1 = session.bind("zwp_virtual_keyboard_manager_v1", 1);
    let keyboard = manager.create_virtual_keyboard(&seat, &qh, ());
    let mut file = tempfile();
    file.write_all(KEYMAP.as_bytes()).unwrap();
    file.write_all(&[0]).unwrap();
    keyboard.keymap(
        KeymapFormat::XkbV1 as u32,
        file.as_fd(),
        KEYMAP.len() as u32 + 1,
    );

    // wtype -M alt -k Tab -m alt: the modifier only ever changes through
    // `modifiers`, never through a key event of its own.
    keyboard.modifiers(ALT, 0, 0, 0);
    key(&keyboard, TAB);
    session.roundtrip();
    assert_eq!(take_fired(), ["switch"]);
    keyboard.modifiers(0, 0, 0, 0);
    session.roundtrip();
    assert_eq!(take_fired(), ["commit"]);

    // Alt held for a chord, with key events for Alt itself: the plain tap
    // binding stays quiet, the transparent one fires.
    keyboard.key(0, ALT_L, 1);
    key(&keyboard, TAB);
    keyboard.key(0, ALT_L, 0);
    session.roundtrip();
    assert_eq!(take_fired(), ["switch", "commit"]);

    // A bare tap fires both.
    key(&keyboard, ALT_L);
    session.roundtrip();
    assert_eq!(take_fired(), ["commit", "tap"]);
}

fn tempfile() -> File {
    let path = std::env::temp_dir().join(format!("shoji-vk-transparent-{}", std::process::id()));
    let file = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&path)
        .unwrap();
    std::fs::remove_file(&path).unwrap();
    file
}
