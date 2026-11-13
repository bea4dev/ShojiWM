//! The Num Lock state a session starts with (`keyboard.numLock`), and the
//! state file that carries it from one session to the next for `"restore"`.
//!
//! The compositor owns the xkb state, so nothing else sets Num Lock at login:
//! without this every session starts with it off.
use std::{
    ffi::OsString,
    io::Write,
    path::{Path, PathBuf},
    sync::mpsc,
};

use smithay::{input::SeatHandler, reexports::calloop::channel};
use tracing::warn;

use crate::{runtime_input::RuntimeNumLock, state::ShojiWM};

/// Session bookkeeping for `keyboard.numLock`.
#[derive(Debug, Default)]
pub struct NumLockState {
    /// The startup state was set (or asked for, with `"restore"`) in this
    /// session. It is set once only, so that a config reload or a new
    /// keyboard does not undo what the user did with the key since.
    started: bool,
    /// A `"restore"` read of the state file is in flight. A change seen
    /// before it comes back is the user's, and wins over the file.
    restore_pending: bool,
    /// The last Num Lock state seen on the session keymap (never a virtual
    /// keyboard's), to see changes and to put it back after a keymap change.
    last: Option<bool>,
    /// The thread that writes the state file, started at the first save.
    writer: Option<mpsc::Sender<bool>>,
}

/// What the first config with `numLock` does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StartupAction {
    Set(bool),
    ReadSaved,
}

fn startup_action(mode: Option<RuntimeNumLock>, started: bool) -> Option<StartupAction> {
    if started {
        return None;
    }
    match mode? {
        RuntimeNumLock::On => Some(StartupAction::Set(true)),
        RuntimeNumLock::Off => Some(StartupAction::Set(false)),
        RuntimeNumLock::Restore => Some(StartupAction::ReadSaved),
    }
}

/// Whether an observed state is a change to save. The first state seen is
/// where the session started, not a change, and must not overwrite the file
/// before it was read.
fn should_save(mode: Option<RuntimeNumLock>, previous: Option<bool>, current: bool) -> bool {
    mode == Some(RuntimeNumLock::Restore) && previous.is_some_and(|previous| previous != current)
}

fn state_file_path() -> Option<PathBuf> {
    resolve_state_file_path(std::env::var_os("XDG_STATE_HOME"), std::env::var_os("HOME"))
}

/// `$XDG_STATE_HOME/shoji_wm/num-lock`, else `~/.local/state/shoji_wm/num-lock`.
/// A relative `XDG_STATE_HOME` is invalid per the XDG spec and is ignored.
fn resolve_state_file_path(
    state_home: Option<OsString>,
    home: Option<OsString>,
) -> Option<PathBuf> {
    let state_home = state_home
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            home.map(PathBuf::from)
                .filter(|path| path.is_absolute())
                .map(|home| home.join(".local").join("state"))
        })?;
    Some(state_home.join("shoji_wm").join("num-lock"))
}

fn parse_state(text: &str) -> Option<bool> {
    match text.trim() {
        "on" => Some(true),
        "off" => Some(false),
        _ => None,
    }
}

/// A missing or unreadable file is no saved state.
fn read_state(path: &Path) -> Option<bool> {
    parse_state(&std::fs::read_to_string(path).ok()?)
}

/// Write through a temporary file and a rename, so that a crash or a power
/// cut mid-write leaves the previous state rather than a truncated file.
fn write_state(path: &Path, on: bool) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let temporary = path.with_extension("tmp");
    {
        let mut file = std::fs::File::create(&temporary)?;
        file.write_all(if on { b"on\n" } else { b"off\n" })?;
        file.sync_all()?;
    }
    std::fs::rename(&temporary, path)
}

/// The writer thread keeps the event loop off the disk. It writes only the
/// latest of the states queued while it was busy, so a burst of toggles is
/// one write.
fn spawn_writer() -> Option<mpsc::Sender<bool>> {
    let (sender, receiver) = mpsc::channel::<bool>();
    let spawned = std::thread::Builder::new()
        .name("shojiwm-num-lock".into())
        .spawn(move || {
            let Some(path) = state_file_path() else {
                warn!("num lock: no XDG_STATE_HOME or HOME; the state is not saved");
                return;
            };
            while let Ok(mut on) = receiver.recv() {
                while let Ok(next) = receiver.try_recv() {
                    on = next;
                }
                if let Err(error) = write_state(&path, on) {
                    warn!(?error, ?path, "num lock: could not save the state");
                }
            }
        });
    match spawned {
        Ok(_) => Some(sender),
        Err(error) => {
            warn!(?error, "num lock: could not start the writer thread");
            None
        }
    }
}

impl ShojiWM {
    /// `keyboard.numLock` of the global keyboard config. There is one seat
    /// keyboard state for all devices, so a per-device value has no meaning.
    fn num_lock_mode(&self) -> Option<RuntimeNumLock> {
        self.runtime_input_config
            .global
            .as_ref()?
            .keyboard
            .as_ref()?
            .num_lock
    }

    /// Set the session's startup Num Lock state, the first time a config
    /// with `numLock` is applied. Call it after the keymap is in place: a
    /// new keymap starts with no lock set.
    pub fn apply_num_lock_startup(&mut self) {
        if self.virtual_keymap_active() || self.seat.get_keyboard().is_none() {
            return;
        }
        let Some(action) = startup_action(self.num_lock_mode(), self.num_lock.started) else {
            return;
        };
        self.num_lock.started = true;
        match action {
            StartupAction::Set(on) => self.set_num_lock(on),
            StartupAction::ReadSaved => self.read_saved_num_lock(),
        }
    }

    /// Read the state file on a thread; the result comes back to the event
    /// loop through a channel.
    fn read_saved_num_lock(&mut self) {
        let (sender, receiver) = channel::channel::<Option<bool>>();
        // The channel removes itself once the thread drops its sender.
        let inserted = self
            .loop_handle
            .insert_source(receiver, |event, _, state: &mut ShojiWM| {
                if let channel::Event::Msg(saved) = event {
                    state.saved_num_lock_read(saved);
                }
            });
        if let Err(error) = inserted {
            warn!(?error, "num lock: could not watch the state file read");
            return;
        }
        self.num_lock.restore_pending = true;
        let spawned = std::thread::Builder::new()
            .name("shojiwm-num-lock-read".into())
            .spawn(move || {
                let _ = sender.send(state_file_path().and_then(|path| read_state(&path)));
            });
        if let Err(error) = spawned {
            warn!(?error, "num lock: could not start the state file read");
            self.num_lock.restore_pending = false;
        }
    }

    fn saved_num_lock_read(&mut self, saved: Option<bool>) {
        if !std::mem::take(&mut self.num_lock.restore_pending) {
            return;
        }
        // No saved state: leave Num Lock as the session has it.
        if let Some(on) = saved
            && !self.virtual_keymap_active()
        {
            self.set_num_lock(on);
        }
    }

    /// The Num Lock state of the session keymap. While a virtual keyboard's
    /// keymap runs, that is the last one seen before it.
    pub fn session_num_lock(&self) -> Option<bool> {
        if self.virtual_keymap_active() {
            return self.num_lock.last;
        }
        self.seat
            .get_keyboard()
            .map(|keyboard| keyboard.modifier_state().num_lock)
    }

    /// A new keymap starts with no lock set (smithay builds a new xkb state).
    /// With `numLock` set, put back the state from before the change, so a
    /// config reload or a layout change does not turn Num Lock off.
    pub fn keep_num_lock_after_keymap_change(&mut self, before: Option<bool>) {
        if self.num_lock_mode().is_none() {
            return;
        }
        if let Some(on) = before {
            self.set_num_lock(on);
        }
    }

    /// Called with each session keyboard state the config is sent.
    pub fn num_lock_observed(&mut self, on: bool) {
        let previous = self.num_lock.last.replace(on);
        if previous.is_some_and(|previous| previous != on) {
            // The user's own toggle wins over a saved state still on its way.
            self.num_lock.restore_pending = false;
        }
        if should_save(self.num_lock_mode(), previous, on) {
            self.save_num_lock(on);
        }
    }

    fn save_num_lock(&mut self, on: bool) {
        if self.num_lock.writer.is_none() {
            self.num_lock.writer = spawn_writer();
        }
        let Some(writer) = &self.num_lock.writer else {
            return;
        };
        if writer.send(on).is_err() {
            // The thread ended (no state directory); do not restart it.
            warn!("num lock: the writer thread stopped; the state is not saved");
        }
    }

    fn set_num_lock(&mut self, on: bool) {
        let Some(keyboard) = self.seat.get_keyboard() else {
            return;
        };
        let mut modifiers = keyboard.modifier_state();
        if modifiers.num_lock != on {
            modifiers.num_lock = on;
            let leds = keyboard.led_state();
            let layout = keyboard.with_xkb_state(self, |context| {
                let xkb = context.xkb().lock().unwrap();
                xkb.active_layout()
            });
            if keyboard.set_modifier_state(modifiers) != 0 {
                // set_modifier_state drops the locked layout (it masks the
                // layout index with a state flag): put the active layout back.
                keyboard.with_xkb_state(self, |mut context| context.set_layout(layout));
                // set_modifier_state tells neither the focused client nor
                // the keyboards' LEDs.
                keyboard.advertise_modifier_state(self);
                let new_leds = keyboard.led_state();
                if new_leds != leds {
                    let seat = self.seat.clone();
                    self.led_state_changed(&seat, new_leds);
                }
                self.current_keyboard_modifiers = keyboard.modifier_state();
            }
        }
        // Set by the compositor, not toggled by the user: not a change to
        // save. The config and panels see it through sync_keyboard_state.
        self.num_lock.last = Some(on);
        self.sync_keyboard_state();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_sets_the_configured_state_once() {
        assert_eq!(
            startup_action(Some(RuntimeNumLock::On), false),
            Some(StartupAction::Set(true))
        );
        assert_eq!(
            startup_action(Some(RuntimeNumLock::Off), false),
            Some(StartupAction::Set(false))
        );
        assert_eq!(
            startup_action(Some(RuntimeNumLock::Restore), false),
            Some(StartupAction::ReadSaved)
        );
        assert_eq!(startup_action(Some(RuntimeNumLock::On), true), None);
        assert_eq!(startup_action(Some(RuntimeNumLock::Restore), true), None);
        assert_eq!(startup_action(None, false), None);
    }

    #[test]
    fn only_restore_saves_and_only_changes() {
        let restore = Some(RuntimeNumLock::Restore);
        assert!(should_save(restore, Some(false), true));
        assert!(should_save(restore, Some(true), false));
        assert!(!should_save(restore, Some(true), true));
        // The first state seen must not overwrite the saved one.
        assert!(!should_save(restore, None, false));
        assert!(!should_save(Some(RuntimeNumLock::On), Some(false), true));
        assert!(!should_save(None, Some(false), true));
    }

    #[test]
    fn state_file_path_follows_xdg_state_home() {
        assert_eq!(
            resolve_state_file_path(Some("/state".into()), Some("/home/user".into())),
            Some(PathBuf::from("/state/shoji_wm/num-lock"))
        );
        assert_eq!(
            resolve_state_file_path(Some("relative".into()), Some("/home/user".into())),
            Some(PathBuf::from("/home/user/.local/state/shoji_wm/num-lock"))
        );
        assert_eq!(
            resolve_state_file_path(None, Some("/home/user".into())),
            Some(PathBuf::from("/home/user/.local/state/shoji_wm/num-lock"))
        );
        assert_eq!(resolve_state_file_path(None, None), None);
    }

    #[test]
    fn parse_state_accepts_on_and_off_only() {
        assert_eq!(parse_state("on\n"), Some(true));
        assert_eq!(parse_state(" off "), Some(false));
        assert_eq!(parse_state("ON"), None);
        assert_eq!(parse_state(""), None);
    }

    #[test]
    fn state_file_round_trip() {
        let dir = std::env::temp_dir().join(format!("shoji-num-lock-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("shoji_wm").join("num-lock");

        assert_eq!(read_state(&path), None);
        write_state(&path, true).unwrap();
        assert_eq!(read_state(&path), Some(true));
        write_state(&path, false).unwrap();
        assert_eq!(read_state(&path), Some(false));
        assert!(!path.with_extension("tmp").exists());

        std::fs::write(&path, "garbage").unwrap();
        assert_eq!(read_state(&path), None);

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
