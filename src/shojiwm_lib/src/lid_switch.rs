//! The laptop lid's switch, reported to the config.
use crate::state::ShojiWM;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub struct LidSwitchSnapshot {
    pub closed: bool,
}

impl ShojiWM {
    /// Record a lid switch toggle from an input device and tell the config.
    pub fn lid_switch_toggled(&mut self, closed: bool) {
        self.lid_closed = Some(closed);
        self.sync_lid_switch();
    }

    /// Send the lid's state to the config when it differs from the last one
    /// sent. Nothing is sent before a lid switch has reported.
    pub fn sync_lid_switch(&mut self) {
        let Some(closed) = self.lid_closed else {
            return;
        };
        if self
            .config_runtime
            .sync_lid_switch(LidSwitchSnapshot { closed })
        {
            self.runtime_scheduler_enabled = true;
        }
    }
}
