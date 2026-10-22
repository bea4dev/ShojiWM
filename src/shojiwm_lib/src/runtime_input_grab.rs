//! `COMPOSITOR.input.grab()`: while a config holds a grab, keyboard, pointer
//! button, scroll and swipe input goes to the config instead of the clients.
//!
//! The grab is modal and takes everything: there is no per-event "pass
//! through" answer, so input never waits on the runtime to decide where it
//! goes. Pointer motion still moves the cursor; clients just lose pointer
//! focus for the duration. Keys held when the grab starts keep delivering
//! their release to the client that saw the press, so a modifier held to open
//! the grab (`Super+Tab`) is not left stuck down in the focused window.
//!
//! The compositor drops the grab on its own when the screen locks, when the
//! config reloads, and when the runtime fails to handle a grab event, so a
//! broken config can never keep the desktop unreachable.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use smithay::input::keyboard::Keycode;

use crate::ssd::{
    GestureSwipeEventSnapshot, PointerModifierStateSnapshot, PointerMovePointSnapshot,
};

/// Sent by the runtime to start (`active`) or end a grab. Ids are chosen by
/// the runtime; starting a new grab replaces the current one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeInputGrabUpdate {
    pub id: u64,
    pub active: bool,
}

#[derive(Debug, Clone)]
pub struct RuntimeInputGrabState {
    pub id: u64,
    /// Keys that were down when the grab started. Their releases still reach
    /// the focused client (and the grab).
    pub held_keys: HashSet<Keycode>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum InputGrabStateSnapshot {
    Pressed,
    Released,
}

/// One input event delivered to the grab.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum InputGrabEventSnapshot {
    Key {
        /// xkb keysym name, unshifted Latin layout first (`"Tab"`, `"a"`,
        /// `"Super_L"`, `"Escape"`), as key binding shortcuts spell them.
        key: String,
        keycode: u32,
        state: InputGrabStateSnapshot,
        modifiers: PointerModifierStateSnapshot,
        timestamp: u64,
    },
    PointerMotion {
        position: PointerMovePointSnapshot,
        delta: PointerMovePointSnapshot,
        output_name: Option<String>,
        modifiers: PointerModifierStateSnapshot,
        timestamp: u64,
    },
    PointerButton {
        /// Linux input event code (`BTN_LEFT` = 272).
        button: u32,
        /// `"left"`, `"right"`, `"middle"`, `"back"`, `"forward"`, or `None`.
        button_name: Option<String>,
        state: InputGrabStateSnapshot,
        position: PointerMovePointSnapshot,
        output_name: Option<String>,
        modifiers: PointerModifierStateSnapshot,
        timestamp: u64,
    },
    Scroll {
        /// Logical pixels, after the device's scroll factor; positive is
        /// right / down.
        delta_x: f64,
        delta_y: f64,
        /// Wheel clicks (120 per detent), when the device reports them.
        discrete_x: Option<f64>,
        discrete_y: Option<f64>,
        /// `"wheel"`, `"finger"`, `"continuous"` or `"wheelTilt"`.
        source: String,
        position: PointerMovePointSnapshot,
        output_name: Option<String>,
        modifiers: PointerModifierStateSnapshot,
        timestamp: u64,
    },
    Swipe {
        event: GestureSwipeEventSnapshot,
    },
    /// The compositor ended the grab (`"sessionLock"`, `"error"`).
    Cancel {
        reason: String,
    },
}

pub fn button_name(button: u32) -> Option<&'static str> {
    match button {
        0x110 => Some("left"),
        0x111 => Some("right"),
        0x112 => Some("middle"),
        0x113 | 0x116 => Some("back"),
        0x114 | 0x115 => Some("forward"),
        _ => None,
    }
}
