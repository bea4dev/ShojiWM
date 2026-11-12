//! The window that has the seat's keyboard, reported to the config.
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;

use crate::state::ShojiWM;

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyboardFocusSnapshot {
    /// The window whose surface, or whose popup, receives the keys; `None`
    /// while a layer surface, the lock screen or nothing does.
    pub window_id: Option<String>,
}

/// The surface the keyboard was last seen on, and what it made of it.
#[derive(Debug, Default)]
pub(crate) struct KeyboardFocus {
    surface: Option<WlSurface>,
    snapshot: KeyboardFocusSnapshot,
}

impl ShojiWM {
    /// The keyboard is on `focused` now: tell the config when that is
    /// another window than the last, or none after one.
    pub(crate) fn keyboard_focus_moved(&mut self, focused: Option<&WlSurface>) {
        let known = &self.keyboard_focus;
        // A surface that is not a window's may be one the next time: a
        // toplevel the space does not hold yet.
        let same = known.surface.as_ref() == focused
            && (focused.is_none() || known.snapshot.window_id.is_some());
        if !same {
            self.keyboard_focus = KeyboardFocus {
                surface: focused.cloned(),
                snapshot: KeyboardFocusSnapshot {
                    window_id: focused.and_then(|surface| {
                        self.space
                            .elements()
                            .find(|window| self.surface_belongs_to_window(window, surface))
                            .map(|window| self.snapshot_window(window).id)
                    }),
                },
            };
        }
        if self
            .config_runtime
            .sync_keyboard_focus(&self.keyboard_focus.snapshot)
        {
            self.runtime_scheduler_enabled = true;
        }
    }

    /// [`Self::keyboard_focus_moved`] by what the seat says. Smithay calls
    /// `focus_changed` for a surface the keyboard is given and not when it is
    /// taken from every one, so this runs where the compositor does that and
    /// on every scheduler tick. Not from `focus_changed` itself: the keyboard
    /// is locked there.
    pub fn sync_keyboard_focus(&mut self) {
        let surface = self
            .seat
            .get_keyboard()
            .and_then(|keyboard| keyboard.current_focus());
        self.keyboard_focus_moved(surface.as_ref());
    }
}
