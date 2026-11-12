//! Compositor control socket behind `shoji_wm --quit` / `--reload`.
//!
//! The same two escape hatches as `Super+Shift+Q` / `Super+Shift+R`, reachable
//! from a terminal, a status bar's click handler or a config keybinding that
//! spawns the command. It belongs to the compositor rather than the config
//! runtime on purpose: reloading is how a broken config gets fixed, so it has
//! to work while the config is the thing that is failing.
//!
//! The socket is `$XDG_RUNTIME_DIR/shojiwm.<WAYLAND_DISPLAY>.sock`, exported as
//! `SHOJIWM_SOCKET`. The wire format is one command per connection:
//!
//! ```text
//! client -> server   "quit\n" | "reload\n" | "key <chord> [<hold-ms>]\n"
//! server -> client   "ok\n" | "error: <message>\n"
//! ```
//!
//! `key Super+Return` presses and releases the chord on a virtual keyboard
//! that goes through the compositor's input path, so it reaches the config's
//! key binds (see [`crate::virtual_input`]). `key XF86AudioRaiseVolume 800`
//! holds it for 800 ms before the release, long enough for key repeat. The
//! answer comes once the event loop has pressed the chord.

use std::{
    ffi::OsStr,
    io::{BufRead, BufReader, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::{Path, PathBuf},
    time::Duration,
};

use smithay::reexports::calloop::{
    LoopHandle,
    channel::{Event as ChannelEvent, Sender, channel},
};
use std::sync::mpsc;
use tracing::{info, warn};

use crate::state::ShojiWM;

pub const SOCKET_ENV: &str = "SHOJIWM_SOCKET";

/// How long a connection may take to send its command line. Only a client
/// that connects and then says nothing ever waits this long.
const READ_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Quit,
    Reload,
    /// Press a chord such as `Super+Shift+Return`, and release it
    /// `hold_ms` later.
    Key {
        chord: String,
        hold_ms: u32,
    },
}

impl Command {
    fn to_line(&self) -> String {
        match self {
            Self::Quit => "quit".into(),
            Self::Reload => "reload".into(),
            Self::Key { chord, hold_ms: 0 } => format!("key {chord}"),
            Self::Key { chord, hold_ms } => format!("key {chord} {hold_ms}"),
        }
    }

    fn parse(line: &str) -> Option<Self> {
        let line = line.trim();
        match line {
            "quit" => Some(Self::Quit),
            "reload" => Some(Self::Reload),
            _ => {
                let mut words = line.strip_prefix("key ")?.split_whitespace();
                let chord = words.next()?.to_string();
                let hold_ms = match words.next() {
                    Some(hold) => hold.parse().ok()?,
                    None => 0,
                };
                words
                    .next()
                    .is_none()
                    .then_some(Self::Key { chord, hold_ms })
            }
        }
    }
}

/// How long a client waits for the event loop to run its command.
const REPLY_TIMEOUT: Duration = Duration::from_secs(5);

type Request = (Command, mpsc::Sender<Result<(), String>>);

/// Where the compositor on `wayland_display` listens.
pub fn socket_path(wayland_display: &OsStr) -> Option<PathBuf> {
    let runtime_dir = std::env::var_os("XDG_RUNTIME_DIR").filter(|dir| !dir.is_empty())?;
    let mut name = std::ffi::OsString::from("shojiwm.");
    name.push(wayland_display);
    name.push(".sock");
    Some(Path::new(&runtime_dir).join(name))
}

/// Removes the socket file when the compositor exits.
pub struct ControlSocket {
    path: PathBuf,
}

impl Drop for ControlSocket {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Bind the control socket for `wayland_display` and export its path. Failing
/// to bind only costs the CLI commands, so it is logged rather than fatal.
pub fn start(
    loop_handle: &LoopHandle<'static, ShojiWM>,
    wayland_display: &OsStr,
) -> Option<ControlSocket> {
    let Some(path) = socket_path(wayland_display) else {
        warn!("XDG_RUNTIME_DIR is not set; shoji_wm --quit/--reload will not work");
        return None;
    };
    // The path is namespaced by the Wayland display, which this process owns
    // now, so anything already there was left behind by a crashed instance.
    let _ = std::fs::remove_file(&path);
    let listener = match UnixListener::bind(&path) {
        Ok(listener) => listener,
        Err(error) => {
            warn!(path = %path.display(), %error, "failed to bind the control socket");
            return None;
        }
    };

    let (sender, receiver) = channel::<Request>();
    if let Err(error) = loop_handle.insert_source(receiver, |event, _, state| {
        if let ChannelEvent::Msg((command, reply)) = event {
            info!(command = command.to_line(), "control socket command");
            let result = match command {
                Command::Quit => {
                    state.shutdown();
                    Ok(())
                }
                Command::Reload => {
                    state.reload_decoration_runtime();
                    Ok(())
                }
                Command::Key { chord, hold_ms } => state.inject_key_chord(&chord, hold_ms),
            };
            let _ = reply.send(result);
        }
    }) {
        warn!(%error, "failed to register the control socket channel");
        let _ = std::fs::remove_file(&path);
        return None;
    }

    let spawned = std::thread::Builder::new()
        .name("shojiwm-control".into())
        .spawn(move || serve(listener, sender));
    if let Err(error) = spawned {
        warn!(%error, "failed to start the control socket thread");
        let _ = std::fs::remove_file(&path);
        return None;
    }

    crate::process_env::set_var(SOCKET_ENV, &path);
    info!(path = %path.display(), "control socket listening");
    Some(ControlSocket { path })
}

fn serve(listener: UnixListener, sender: Sender<Request>) {
    for stream in listener.incoming() {
        let Ok(stream) = stream else {
            continue;
        };
        let reply = match read_command(&stream) {
            Ok(command) => {
                let (reply_sender, reply_receiver) = mpsc::channel();
                if sender.send((command, reply_sender)).is_err() {
                    // The event loop is gone: the compositor is shutting down.
                    return;
                }
                match reply_receiver.recv_timeout(REPLY_TIMEOUT) {
                    Ok(Ok(())) => "ok".to_string(),
                    Ok(Err(error)) => format!("error: {error}"),
                    Err(_) => "error: the compositor did not run the command".to_string(),
                }
            }
            Err(error) => format!("error: {error}"),
        };
        let _ = (&stream).write_all(format!("{reply}\n").as_bytes());
    }
}

fn read_command(stream: &UnixStream) -> Result<Command, String> {
    stream
        .set_read_timeout(Some(READ_TIMEOUT))
        .map_err(|error| error.to_string())?;
    let mut line = String::new();
    BufReader::new(stream)
        .read_line(&mut line)
        .map_err(|error| error.to_string())?;
    Command::parse(&line).ok_or_else(|| format!("unknown command {:?}", line.trim()))
}

/// Client side: send `command` to the running compositor.
///
/// The socket comes from `SHOJIWM_SOCKET`, or failing that from
/// `WAYLAND_DISPLAY`, so a shell started inside the session finds it either way.
pub fn send(command: Command) -> Result<(), String> {
    let path = std::env::var_os(SOCKET_ENV)
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("WAYLAND_DISPLAY")
                .filter(|display| !display.is_empty())
                .and_then(|display| socket_path(&display))
        })
        .ok_or_else(|| {
            format!("no running ShojiWM found ({SOCKET_ENV} and WAYLAND_DISPLAY are unset)")
        })?;
    let mut stream = UnixStream::connect(&path)
        .map_err(|error| format!("cannot connect to {}: {error}", path.display()))?;
    stream
        .write_all(format!("{}\n", command.to_line()).as_bytes())
        .map_err(|error| error.to_string())?;
    let mut reply = String::new();
    BufReader::new(&stream)
        .read_line(&mut reply)
        .map_err(|error| error.to_string())?;
    match reply.trim() {
        "ok" => Ok(()),
        "" => Err("the compositor closed the connection without answering".to_string()),
        other => Err(other.strip_prefix("error: ").unwrap_or(other).to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::Command;

    #[test]
    fn commands_round_trip_through_their_line() {
        for command in [
            Command::Quit,
            Command::Reload,
            Command::Key {
                chord: "Super+Shift+Return".into(),
                hold_ms: 0,
            },
            Command::Key {
                chord: "XF86AudioRaiseVolume".into(),
                hold_ms: 800,
            },
        ] {
            assert_eq!(
                Command::parse(&format!("{}\n", command.to_line())),
                Some(command)
            );
        }
    }

    #[test]
    fn key_needs_a_chord() {
        assert_eq!(Command::parse("key"), None);
        assert_eq!(Command::parse("key   "), None);
        assert_eq!(
            Command::parse(" key  ctrl+c \n"),
            Some(Command::Key {
                chord: "ctrl+c".into(),
                hold_ms: 0
            })
        );
        assert_eq!(Command::parse("press a"), None);
    }

    #[test]
    fn key_hold_is_a_number_of_milliseconds() {
        assert_eq!(
            Command::parse("key a 250"),
            Some(Command::Key {
                chord: "a".into(),
                hold_ms: 250
            })
        );
        assert_eq!(Command::parse("key a long"), None);
        assert_eq!(Command::parse("key a 250 b"), None);
    }
}
