//! Keys from a `zwp_virtual_keyboard_v1` client reach key binds, matched by
//! the keysyms of the keymap the client uploaded, the way `wtype` sends them.

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
xkb_keycodes "(unnamed)" { minimum = 8; maximum = 10; <K1> = 9; <K2> = 10; };
xkb_types "(unnamed)" { include "complete" };
xkb_compat "(unnamed)" { include "complete" };
xkb_symbols "(unnamed)" { key <K1> {[space]}; key <K2> {[F13]}; };
};
"#;
const SPACE: u32 = 1;
const F13: u32 = 2;
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

#[test]
fn virtual_keyboard_keys_fire_binds_by_their_own_keysyms() {
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

fn tempfile() -> File {
    let path = std::env::temp_dir().join(format!("shoji-vk-keymap-{}", std::process::id()));
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
