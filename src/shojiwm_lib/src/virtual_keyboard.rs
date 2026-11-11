//! Keys from `zwp_virtual_keyboard_v1` clients (`wtype`, on-screen
//! keyboards, remote desktop servers).
//!
//! A virtual keyboard is a keyboard with a keymap of its own: `wtype` uploads
//! one holding just the keysyms it is about to type, on whatever keycodes it
//! picked. While it types, the seat runs that keymap, the way ShojiWM already
//! switches keymaps between keyboards with different layouts, so its keys
//! take the same path as a real key press: key binds by keysym, taps, input
//! grabs, and the focused client, which is sent the keymap. The session's
//! keymap comes back with the next key from another keyboard, or when the
//! virtual keyboard goes away.

use smithay::backend::input::{InputEvent, KeyState, Keycode};
use smithay::input::keyboard::ModifiersState;
use smithay::reexports::wayland_server::backend::ObjectId;
use tracing::warn;
use xkbcommon::xkb;

use crate::protocols::virtual_keyboard::VirtualKeyboardHandler;
use crate::state::ShojiWM;
use crate::virtual_input::{VirtualDevice, VirtualInput, VirtualKey};

/// Device ids of virtual keyboards start with this; their keys bring their
/// own keymap instead of the configured layout.
pub const DEVICE_ID_PREFIX: &str = "zwp-virtual-keyboard-";

pub struct VirtualKeyboard {
    device: VirtualDevice,
    keymap_serial: u64,
    keymap_text: String,
    state: xkb::State,
    pressed: Vec<u32>,
}

/// The seat runs a virtual keyboard's keymap; what to go back to.
pub struct ActiveVirtualKeymap {
    keymap_serial: u64,
    session_keymap: xkb::Keymap,
    session_layout: xkb::LayoutIndex,
}

impl ShojiWM {
    /// Switch the seat to `keyboard`'s keymap unless it already runs it.
    fn activate_virtual_keymap(&mut self, keyboard: &ObjectId) -> bool {
        let Some(virtual_keyboard) = self.virtual_keyboards.get(keyboard) else {
            return false;
        };
        let serial = virtual_keyboard.keymap_serial;
        let text = virtual_keyboard.keymap_text.clone();
        if self
            .active_virtual_keymap
            .as_ref()
            .is_some_and(|active| active.keymap_serial == serial)
        {
            return true;
        }
        let Some(seat_keyboard) = self.seat.get_keyboard() else {
            return false;
        };
        let session = match self.active_virtual_keymap.take() {
            Some(active) => (active.session_keymap, active.session_layout),
            None => seat_keyboard.with_xkb_state(self, |context| {
                let xkb = context.xkb().lock().unwrap();
                // Safety: the keymap is cloned (a reference count), not kept
                // borrowed past this call.
                let keymap = unsafe { xkb.keymap() }.clone();
                (keymap, xkb.active_layout().0)
            }),
        };
        if let Err(error) = seat_keyboard.set_keymap_from_string(self, text) {
            warn!(?error, "could not switch to a virtual keyboard's keymap");
            return false;
        }
        self.active_virtual_keymap = Some(ActiveVirtualKeymap {
            keymap_serial: serial,
            session_keymap: session.0,
            session_layout: session.1,
        });
        true
    }

    /// Put the session's keymap and layout back after a virtual keyboard.
    /// The focused client is sent the keymap; focus stays where it is.
    pub fn restore_session_keymap(&mut self) {
        let Some(active) = self.active_virtual_keymap.take() else {
            return;
        };
        let Some(seat_keyboard) = self.seat.get_keyboard() else {
            return;
        };
        let text = active
            .session_keymap
            .get_as_string(xkb::KEYMAP_FORMAT_TEXT_V1);
        if let Err(error) = seat_keyboard.set_keymap_from_string(self, text) {
            warn!(?error, "could not restore the session keymap");
            return;
        }
        let layout = active.session_layout;
        seat_keyboard.with_xkb_state(self, |mut context| {
            context.set_layout(smithay::input::keyboard::Layout(layout));
        });
        self.current_keyboard_modifiers = seat_keyboard.modifier_state();
    }

    /// Whether the seat runs a virtual keyboard's keymap, whose layout is not
    /// the session's.
    pub fn virtual_keymap_active(&self) -> bool {
        self.active_virtual_keymap.is_some()
    }
}

impl VirtualKeyboardHandler for ShojiWM {
    fn virtual_keyboard_keymap(&mut self, keyboard: &ObjectId, keymap_text: String) -> bool {
        let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
        let Some(keymap) = xkb::Keymap::new_from_string(
            &context,
            keymap_text.clone(),
            xkb::KEYMAP_FORMAT_TEXT_V1,
            xkb::KEYMAP_COMPILE_NO_FLAGS,
        ) else {
            warn!("a virtual keyboard sent a keymap that does not compile");
            return false;
        };
        self.next_virtual_keymap_serial += 1;
        let keymap_serial = self.next_virtual_keymap_serial;
        let state = xkb::State::new(&keymap);
        let previous = self.virtual_keyboards.insert(
            keyboard.clone(),
            VirtualKeyboard {
                device: VirtualDevice {
                    id: format!("{DEVICE_ID_PREFIX}{keymap_serial}"),
                    name: "virtual keyboard".into(),
                    keyboard: true,
                },
                keymap_serial,
                keymap_text,
                state,
                pressed: Vec::new(),
            },
        );
        if let (Some(previous), Some(entry)) = (previous, self.virtual_keyboards.get_mut(keyboard))
        {
            entry.device = previous.device;
            entry.pressed = previous.pressed;
        }
        true
    }

    fn virtual_keyboard_key(
        &mut self,
        keyboard: &ObjectId,
        time_msec: u32,
        key: u32,
        pressed: bool,
    ) {
        let keycode = key + 8;
        if !self.activate_virtual_keymap(keyboard) {
            return;
        }
        let Some(virtual_keyboard) = self.virtual_keyboards.get_mut(keyboard) else {
            return;
        };
        if pressed {
            if !virtual_keyboard.pressed.contains(&key) {
                virtual_keyboard.pressed.push(key);
            }
        } else {
            virtual_keyboard.pressed.retain(|held| *held != key);
        }
        let event = VirtualKey {
            device: virtual_keyboard.device.clone(),
            time: u64::from(time_msec) * 1000,
            keycode: Keycode::new(keycode),
            state: if pressed {
                KeyState::Pressed
            } else {
                KeyState::Released
            },
            count: virtual_keyboard.pressed.len() as u32,
        };
        self.process_input_event(InputEvent::<VirtualInput>::Keyboard { event });
    }

    fn virtual_keyboard_modifiers(
        &mut self,
        keyboard: &ObjectId,
        depressed: u32,
        latched: u32,
        locked: u32,
        group: u32,
    ) {
        if !self.activate_virtual_keymap(keyboard) {
            return;
        }
        let Some(virtual_keyboard) = self.virtual_keyboards.get_mut(keyboard) else {
            return;
        };
        virtual_keyboard
            .state
            .update_mask(depressed, latched, locked, 0, 0, group);
        let mut modifiers = ModifiersState::default();
        modifiers.update_with(&virtual_keyboard.state);
        let Some(seat_keyboard) = self.seat.get_keyboard() else {
            return;
        };
        if seat_keyboard.set_modifier_state(modifiers) != 0 {
            seat_keyboard.advertise_modifier_state(self);
        }
        self.current_keyboard_modifiers = seat_keyboard.modifier_state();
    }

    fn virtual_keyboard_destroyed(&mut self, keyboard: &ObjectId) {
        let Some(virtual_keyboard) = self.virtual_keyboards.remove(keyboard) else {
            return;
        };
        if self
            .active_virtual_keymap
            .as_ref()
            .is_some_and(|active| active.keymap_serial == virtual_keyboard.keymap_serial)
        {
            self.restore_session_keymap();
        }
    }
}
