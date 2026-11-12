//! A `repeat` binding's handler reads `COMPOSITOR.key.is_repeat()` to tell
//! the runs for its held key's repeat from the run for the press.

mod common;

use std::{cell::RefCell, time::Duration};

use common::keyboard::{Key, Keyboard};
use shojiwm_rs::prelude::*;
use smithay::backend::input::{InputEvent, KeyState};

thread_local! {
    static SEEN: RefCell<Vec<bool>> = const { RefCell::new(Vec::new()) };
}

fn setup() {
    COMPOSITOR
        .key
        .bind_with("held", "F1", KeyBindOptions::new().repeat(), || {
            SEEN.with_borrow_mut(|seen| seen.push(COMPOSITOR.key.is_repeat()));
        });
}

/// F1 in the session's keymap: evdev 59 plus 8.
const F1: u32 = 67;

fn key(session: &mut common::Session, state: KeyState) {
    session
        .state
        .process_input_event(InputEvent::<Keyboard>::Keyboard {
            event: Key {
                time: 0,
                keycode: F1,
                state,
            },
        });
}

#[test]
fn a_held_binding_tells_its_repeats_from_the_press() {
    let mut session = common::Session::new(setup);
    session.roundtrip();
    session.state.keyboard_repeat_info = (100, 10);

    key(&mut session, KeyState::Pressed);
    for _ in 0..3 {
        std::thread::sleep(Duration::from_millis(30));
        session.roundtrip();
    }
    key(&mut session, KeyState::Released);
    session.roundtrip();

    let seen = SEEN.with_borrow(Clone::clone);
    assert!(seen.len() >= 3, "the binding ran {} times", seen.len());
    assert!(!seen[0], "the press read as a repeat");
    assert!(
        seen[1..].iter().all(|repeat| *repeat),
        "a repeat read as the press: {seen:?}",
    );
    assert!(!COMPOSITOR.key.is_repeat());
}
