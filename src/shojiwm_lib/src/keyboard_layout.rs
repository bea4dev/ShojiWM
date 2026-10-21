//! Active seat layout for config runtime shell integration; never records keys.
use crate::state::ShojiWM;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct KeyboardLayoutSnapshot {
    pub index: u32,
    pub name: String,
}

/// The lock keys' state, for an indicator. Kept apart from the layout so that
/// toggling a lock key is not reported as a layout change.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyboardLocksSnapshot {
    pub caps_lock: bool,
    pub num_lock: bool,
}

impl ShojiWM {
    /// Send the keyboard's layout and lock keys to the config, each only when
    /// it changed.
    pub fn sync_keyboard_state(&mut self) {
        let Some(keyboard) = self.seat.get_keyboard() else {
            return;
        };
        let modifiers = keyboard.modifier_state();
        let locks = KeyboardLocksSnapshot {
            caps_lock: modifiers.caps_lock,
            num_lock: modifiers.num_lock,
        };
        let layout = keyboard.with_xkb_state(self, |context| {
            let xkb = context.xkb().lock().unwrap();
            let layout = xkb.active_layout();
            KeyboardLayoutSnapshot {
                index: layout.0,
                name: xkb.layout_name(layout).to_owned(),
            }
        });
        let layout_changed = self.config_runtime.sync_keyboard_layout(layout);
        let locks_changed = self.config_runtime.sync_keyboard_locks(locks);
        if layout_changed || locks_changed {
            // Wake even an idle runtime so panels receive the new state.
            self.runtime_scheduler_enabled = true;
        }
    }
}
