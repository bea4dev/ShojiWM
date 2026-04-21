use smithay::reexports::rustix::{
    self,
    fs::{OFlags, lstat, mkdir, open, unlink},
    io::{Errno, FdFlags, fcntl_setfd},
    process::{getpid, getuid},
};
use std::{
    ffi::OsString,
    fs::{self, OpenOptions},
    io,
    os::{
        fd::{AsRawFd, BorrowedFd, OwnedFd},
        unix::{
            net::{SocketAddr, UnixListener},
            process::CommandExt,
        },
    },
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
};
use tracing::warn;

const TMP_UNIX_DIR: &str = "/tmp";
const X11_TMP_UNIX_DIR: &str = "/tmp/.X11-unix";
const SATELLITE_LOG_FILE: &str = "xwayland-satellite.log";

pub struct SatelliteInstance {
    pub display_name: String,
    pub display_number: u32,
    _unix_guard: UnlinkGuard,
    _lock_guard: UnlinkGuard,
}

struct UnlinkGuard(PathBuf);

impl Drop for UnlinkGuard {
    fn drop(&mut self) {
        let _ = unlink(&self.0);
    }
}

pub fn satellite_requested() -> bool {
    std::env::var_os("SHOJI_XWAYLAND_SATELLITE").is_some_and(|value| value != "0" && value != "off")
}

pub fn spawn_satellite() -> Result<SatelliteInstance, Box<dyn std::error::Error>> {
    let path = expand_tilde_in_path(
        std::env::var_os("SHOJI_XWAYLAND_SATELLITE_PATH")
            .unwrap_or_else(|| OsString::from("xwayland-satellite")),
    );

    if !test_listenfd_support(&path) {
        return Err(format!(
            "{} does not support --test-listenfd-support / -listenfd integration",
            path.display()
        )
        .into());
    }

    ensure_x11_unix_dir()?;
    let (display_number, _lock_fd, lock_guard) = pick_x11_display(0)?;
    let (abstract_listener, unix_listener, unix_guard) = open_display_sockets(display_number)?;
    let display_name = format!(":{display_number}");

    let child = spawn_satellite_process(
        &path,
        &display_name,
        abstract_listener.as_ref(),
        &unix_listener,
    )?;

    spawn_waiter_thread(path, child);

    Ok(SatelliteInstance {
        display_name,
        display_number,
        _unix_guard: unix_guard,
        _lock_guard: lock_guard,
    })
}

fn expand_tilde_in_path(path: OsString) -> PathBuf {
    let path = PathBuf::from(path);
    let Some(raw) = path.to_str() else {
        return path;
    };
    if raw == "~" {
        return std::env::var_os("HOME").map(PathBuf::from).unwrap_or(path);
    }
    if let Some(rest) = raw.strip_prefix("~/") {
        return std::env::var_os("HOME")
            .map(PathBuf::from)
            .map(|home| home.join(rest))
            .unwrap_or(path);
    }
    path
}

fn test_listenfd_support(path: &Path) -> bool {
    let mut child = match Command::new(path)
        .args([":0", "--test-listenfd-support"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .env_remove("DISPLAY")
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            warn!(?error, path = ?path, "failed to spawn xwayland-satellite probe");
            return false;
        }
    };

    match child.wait() {
        Ok(status) => status.success(),
        Err(error) => {
            warn!(?error, path = ?path, "failed to wait for xwayland-satellite probe");
            false
        }
    }
}

fn ensure_x11_unix_dir() -> Result<(), Box<dyn std::error::Error>> {
    match mkdir(X11_TMP_UNIX_DIR, 0o1777.into()) {
        Ok(()) => Ok(()),
        Err(Errno::EXIST) => ensure_x11_unix_perms(),
        Err(error) => {
            Err(io::Error::other(format!("failed to create {X11_TMP_UNIX_DIR}: {error}")).into())
        }
    }
}

fn ensure_x11_unix_perms() -> Result<(), Box<dyn std::error::Error>> {
    let x11_tmp = lstat(X11_TMP_UNIX_DIR)?;
    let tmp = lstat(TMP_UNIX_DIR)?;

    if !(x11_tmp.st_uid == tmp.st_uid || x11_tmp.st_uid == getuid().as_raw()) {
        return Err(io::Error::other("wrong ownership for /tmp/.X11-unix").into());
    }
    if (x11_tmp.st_mode & 0o022) != 0o022 {
        return Err(io::Error::other("/tmp/.X11-unix is not writable").into());
    }
    if (x11_tmp.st_mode & 0o1000) != 0o1000 {
        return Err(io::Error::other("/tmp/.X11-unix is missing the sticky bit").into());
    }

    Ok(())
}

fn pick_x11_display(start: u32) -> Result<(u32, OwnedFd, UnlinkGuard), Box<dyn std::error::Error>> {
    for display_number in start..start + 50 {
        let lock_path = PathBuf::from(format!("/tmp/.X{display_number}-lock"));
        let flags = OFlags::WRONLY | OFlags::CLOEXEC | OFlags::CREATE | OFlags::EXCL;
        let Ok(lock_fd) = open(&lock_path, flags, 0o444.into()) else {
            continue;
        };

        let pid_string = format!("{:>10}\n", getpid().as_raw_nonzero());
        rustix::io::write(&lock_fd, pid_string.as_bytes())?;
        return Ok((display_number, lock_fd, UnlinkGuard(lock_path)));
    }

    Err(io::Error::other("no free X11 display found").into())
}

fn bind_to_socket(addr: &SocketAddr) -> Result<UnixListener, Box<dyn std::error::Error>> {
    Ok(UnixListener::bind_addr(addr)?)
}

#[cfg(target_os = "linux")]
fn bind_to_abstract_socket(
    display_number: u32,
) -> Result<UnixListener, Box<dyn std::error::Error>> {
    use std::os::linux::net::SocketAddrExt;

    let name = format!("/tmp/.X11-unix/X{display_number}");
    let addr = SocketAddr::from_abstract_name(name)?;
    bind_to_socket(&addr)
}

#[cfg(not(target_os = "linux"))]
fn bind_to_abstract_socket(
    _display_number: u32,
) -> Result<UnixListener, Box<dyn std::error::Error>> {
    Err(io::Error::other("abstract X11 sockets are unsupported on this platform").into())
}

fn bind_to_unix_socket(
    display_number: u32,
) -> Result<(UnixListener, UnlinkGuard), Box<dyn std::error::Error>> {
    let path = PathBuf::from(format!("/tmp/.X11-unix/X{display_number}"));
    let _ = unlink(&path);
    let addr = SocketAddr::from_pathname(&path)?;
    let listener = bind_to_socket(&addr)?;
    Ok((listener, UnlinkGuard(path)))
}

fn open_display_sockets(
    display_number: u32,
) -> Result<(Option<UnixListener>, UnixListener, UnlinkGuard), Box<dyn std::error::Error>> {
    #[cfg(target_os = "linux")]
    let abstract_listener = Some(bind_to_abstract_socket(display_number)?);
    #[cfg(not(target_os = "linux"))]
    let abstract_listener = None;

    let (unix_listener, unix_guard) = bind_to_unix_socket(display_number)?;
    Ok((abstract_listener, unix_listener, unix_guard))
}

fn spawn_satellite_process(
    path: &Path,
    display_name: &str,
    abstract_listener: Option<&UnixListener>,
    unix_listener: &UnixListener,
) -> Result<Child, Box<dyn std::error::Error>> {
    let abstract_raw = abstract_listener.map(AsRawFd::as_raw_fd);
    let unix_raw = unix_listener.as_raw_fd();
    let glamor = std::env::var("SHOJI_XWAYLAND_SATELLITE_GLAMOR").ok();

    let mut command = Command::new(path);
    command
        .arg(display_name)
        .arg("-listenfd")
        .arg(unix_raw.to_string())
        .env_remove("DISPLAY")
        .stdin(Stdio::null());

    if satellite_logging_enabled() {
        let log_path = satellite_log_path();
        if let Some(parent) = log_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let log_file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&log_path)?;
        let stderr_file = log_file.try_clone()?;
        command.stdout(Stdio::from(log_file));
        command.stderr(Stdio::from(stderr_file));
    } else {
        command.stdout(Stdio::null());
        command.stderr(Stdio::null());
    }

    if let Some(abstract_raw) = abstract_raw {
        command.arg("-listenfd").arg(abstract_raw.to_string());
    }
    if let Some(glamor) = glamor.as_deref()
        && matches!(glamor, "gl" | "es" | "none")
    {
        command.arg("-glamor").arg(glamor);
    }

    unsafe {
        command.pre_exec(move || {
            let unix_fd = BorrowedFd::borrow_raw(unix_raw);
            fcntl_setfd(unix_fd, FdFlags::empty()).map_err(|error| {
                io::Error::other(format!("failed to pass unix socket fd: {error}"))
            })?;

            if let Some(abstract_raw) = abstract_raw {
                let abstract_fd = BorrowedFd::borrow_raw(abstract_raw);
                fcntl_setfd(abstract_fd, FdFlags::empty()).map_err(|error| {
                    io::Error::other(format!("failed to pass abstract socket fd: {error}"))
                })?;
            }

            Ok(())
        });
    }

    Ok(command.spawn()?)
}

fn satellite_logging_enabled() -> bool {
    std::env::var_os("SHOJI_XWAYLAND_SATELLITE_LOG")
        .is_some_and(|value| value != "0" && value != "off")
}

fn satellite_log_path() -> PathBuf {
    std::env::var_os("SHOJI_XWAYLAND_SATELLITE_LOG_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."))
                .join("shoji_wm")
                .join("logs")
                .join(SATELLITE_LOG_FILE)
        })
}

fn spawn_waiter_thread(path: PathBuf, mut child: Child) {
    let _ = std::thread::Builder::new()
        .name("xwayland-satellite-wait".to_string())
        .spawn(move || match child.wait() {
            Ok(status) => {
                warn!(path = ?path, ?status, "xwayland-satellite exited");
            }
            Err(error) => {
                warn!(path = ?path, ?error, "failed waiting for xwayland-satellite");
            }
        });
}
