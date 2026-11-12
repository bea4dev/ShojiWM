//! The laptop lid's switch, reported to the config.
use std::time::{Duration, Instant};

use tracing::debug;

use crate::state::ShojiWM;

/// How long after a lid switch device comes up its toggles are the state it
/// started in. libinput reports a lid that is closed already right after the
/// device (`post_added`), in the same dispatch.
const INITIAL_STATE_WINDOW: Duration = Duration::from_millis(500);

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub struct LidSwitchSnapshot {
    pub closed: bool,
    /// The state the switch was in when it came up (at startup, or when the
    /// session comes back from another VT), or the state a reloaded config
    /// starts with; not a movement of the lid. A broken switch can report
    /// closed this way while the lid is open.
    pub initial: bool,
}

impl ShojiWM {
    /// A device with a switch came up; what it reports next is where it is.
    pub fn switch_device_added(&mut self) {
        self.lid_switch_initial_until = Some(Instant::now() + INITIAL_STATE_WINDOW);
    }

    /// Record a lid switch toggle from an input device and tell the config.
    pub fn lid_switch_toggled(&mut self, closed: bool) {
        // libinput reports a newly added switch's state once at most.
        let initial = self
            .lid_switch_initial_until
            .take()
            .is_some_and(|until| Instant::now() < until);
        self.lid_closed = Some(closed);
        self.lid_initial = initial;
        if !closed {
            self.redraw_internal_panels();
        }
        self.sync_lid_switch();
    }

    /// Send the built-in panels a whole new frame.
    ///
    /// A panel in self refresh (PSR, Panel Replay) shows what it stored and
    /// waits for the next frame with damage. Closing the lid can cut its power
    /// without the driver hearing of it (amdgpu, an eDP OLED here), so with
    /// nothing to suspend the lid it opens dark until something redraws.
    fn redraw_internal_panels(&mut self) {
        let panels: Vec<String> = crate::backend::tty::tty_connected_outputs(self)
            .iter()
            .map(|output| output.name())
            .filter(|name| {
                ["eDP", "LVDS", "DSI"]
                    .iter()
                    .any(|kind| name.starts_with(kind))
            })
            .collect();
        if panels.is_empty() {
            return;
        }
        debug!(?panels, "lid opened; redrawing the built-in panels");
        self.full_damage_pending_outputs.extend(panels);
        self.schedule_redraw();
    }

    /// Send the lid's state to the config when it differs from the last one
    /// sent. Nothing is sent before a lid switch has reported.
    pub fn sync_lid_switch(&mut self) {
        let Some(closed) = self.lid_closed else {
            return;
        };
        if self.config_runtime.sync_lid_switch(LidSwitchSnapshot {
            closed,
            initial: self.lid_initial,
        }) {
            self.runtime_scheduler_enabled = true;
        }
    }
}
