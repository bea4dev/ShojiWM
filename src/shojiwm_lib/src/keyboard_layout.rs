//! Active seat layout for config runtime shell integration; never records keys.
use crate::state::ShojiWM;

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct KeyboardLayoutSnapshot {
    pub index: u32,
    pub name: String,
    /// The lock keys' state, for an indicator; left out of the serialized
    /// form the TypeScript runtime reads.
    #[serde(skip)]
    pub caps_lock: bool,
    #[serde(skip)]
    pub num_lock: bool,
}

impl ShojiWM {
    pub fn sync_keyboard_layout(&mut self) {
        let Some(keyboard) = self.seat.get_keyboard() else {
            return;
        };
        let locks = keyboard.modifier_state();
        let layout = keyboard.with_xkb_state(self, |context| {
            let xkb = context.xkb().lock().unwrap();
            let layout = xkb.active_layout();
            KeyboardLayoutSnapshot {
                index: layout.0,
                name: xkb.layout_name(layout).to_owned(),
                caps_lock: locks.caps_lock,
                num_lock: locks.num_lock,
            }
        });
        if self.config_runtime.sync_keyboard_layout(layout) {
            // Wake even an idle runtime so panels receive the new layout.
            self.runtime_scheduler_enabled = true;
        }
    }
}
