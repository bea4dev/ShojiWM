use smithay::reexports::calloop::channel::Sender;
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
            net::{SocketAddr, UnixListener, UnixStream},
            process::CommandExt,
        },
    },
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::Mutex,
    time::{Duration, Instant},
};
use tracing::{info, warn};

const TMP_UNIX_DIR: &str = "/tmp";
const X11_TMP_UNIX_DIR: &str = "/tmp/.X11-unix";
const SATELLITE_LOG_FILE: &str = "xwayland-satellite.log";

pub struct SatelliteInstance {
    pub display_name: String,
    pub display_number: u32,
    /// Present when satellite runs in-process; it keeps what a restart needs.
    pub embedded: Option<EmbeddedSatellite>,
    _unix_guard: UnlinkGuard,
    _lock_guard: UnlinkGuard,
}

struct UnlinkGuard(PathBuf);

impl Drop for UnlinkGuard {
    fn drop(&mut self) {
        let _ = unlink(&self.0);
    }
}

/// How xwayland-satellite is run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SatelliteMode {
    /// In-process, on its own thread (the default).
    Embedded,
    /// As a separate process, from this executable path or `PATH` lookup.
    External(PathBuf),
}

/// The satellite mode chosen by the environment, or `None` when satellite is
/// disabled and the built-in Xwayland support is used instead.
///
/// - `SHOJI_XWAYLAND_SATELLITE=0|off`: disabled.
/// - `SHOJI_XWAYLAND_SATELLITE_PATH=<path>` (or `--xwayland-satellite-path`):
///   external, running that binary.
/// - `SHOJI_XWAYLAND_SATELLITE=external`: external, running the
///   `xwayland-satellite` found in `PATH`.
/// - otherwise: embedded.
pub fn satellite_mode() -> Option<SatelliteMode> {
    satellite_mode_from(
        std::env::var_os("SHOJI_XWAYLAND_SATELLITE"),
        std::env::var_os("SHOJI_XWAYLAND_SATELLITE_PATH"),
    )
}

fn satellite_mode_from(setting: Option<OsString>, path: Option<OsString>) -> Option<SatelliteMode> {
    if setting
        .as_ref()
        .is_some_and(|value| value == "0" || value == "off")
    {
        return None;
    }
    if let Some(path) = path.filter(|path| !path.is_empty()) {
        return Some(SatelliteMode::External(expand_tilde_in_path(path)));
    }
    if setting.is_some_and(|value| value == "external") {
        return Some(SatelliteMode::External(PathBuf::from("xwayland-satellite")));
    }
    Some(SatelliteMode::Embedded)
}

pub fn satellite_requested() -> bool {
    satellite_mode().is_some()
}

/// Start xwayland-satellite as a separate process running `path`.
pub fn spawn_external_satellite(
    path: &Path,
) -> Result<SatelliteInstance, Box<dyn std::error::Error>> {
    if !test_listenfd_support(path) {
        return Err(format!(
            "{} does not support --test-listenfd-support / -listenfd integration",
            path.display()
        )
        .into());
    }

    ensure_x11_unix_dir()?;
    let (display_number, _lock_fd, lock_guard, abstract_listener, unix_listener, unix_guard) =
        reserve_x11_display(0)?;
    let display_name = format!(":{display_number}");

    let child = spawn_satellite_process(
        path,
        &display_name,
        abstract_listener.as_ref(),
        &unix_listener,
    )?;

    spawn_waiter_thread(path.to_path_buf(), child);

    Ok(SatelliteInstance {
        display_name,
        display_number,
        embedded: None,
        _unix_guard: unix_guard,
        _lock_guard: lock_guard,
    })
}

/// Reserve an X11 display for an in-process satellite. Nothing runs yet;
/// [`EmbeddedSatellite::start`] starts it.
pub fn reserve_embedded_satellite(
    events: Sender<SatelliteEvent>,
) -> Result<SatelliteInstance, Box<dyn std::error::Error>> {
    // Satellite runs `Xwayland` from `PATH`. Without it no run can ever serve
    // the display, and an exported `DISPLAY` whose socket nobody answers makes
    // every X11 client (fcitx5, Chromium, ...) hang in its connection setup.
    if find_in_path(XWAYLAND_BINARY).is_none() {
        return Err(format!("{XWAYLAND_BINARY} not found in PATH").into());
    }
    ensure_x11_unix_dir()?;
    let (display_number, _lock_fd, lock_guard, abstract_listener, unix_listener, unix_guard) =
        reserve_x11_display(0)?;
    let display_name = format!(":{display_number}");

    let listeners = abstract_listener
        .into_iter()
        .chain(std::iter::once(unix_listener))
        .map(OwnedFd::from)
        .collect();

    Ok(SatelliteInstance {
        embedded: Some(EmbeddedSatellite {
            display_name: display_name.clone(),
            listeners,
            flags: glamor_flags(),
            events,
            started_at: None,
            restart_delay: INITIAL_RESTART_DELAY,
            generation: 0,
            ready: false,
            failed_starts: 0,
        }),
        display_name,
        display_number,
        _unix_guard: unix_guard,
        _lock_guard: lock_guard,
    })
}

/// What the embedded satellite thread reports to the compositor's event loop.
#[derive(Debug)]
pub enum SatelliteEvent {
    /// Xwayland is up and satellite manages it.
    Ready {
        generation: u64,
        display: String,
        xwayland_pid: u32,
    },
    /// The satellite thread ended: Xwayland exited, or satellite panicked.
    Exited { generation: u64, panicked: bool },
}

/// Delay before the first restart after a quick failure.
const INITIAL_RESTART_DELAY: Duration = Duration::from_millis(500);
/// Longest delay between restarts while satellite keeps failing.
const MAX_RESTART_DELAY: Duration = Duration::from_secs(30);
/// A run this long counts as healthy and resets the restart delay.
const HEALTHY_RUN: Duration = Duration::from_secs(60);
/// Consecutive runs that end before Xwayland is ready, after which satellite
/// is given up on.
const MAX_FAILED_STARTS: u32 = 3;
/// The Xwayland executable satellite runs.
const XWAYLAND_BINARY: &str = "Xwayland";

/// xwayland-satellite running in-process.
///
/// Satellite still talks to the compositor over the Wayland protocol, through
/// one end of a socket pair that the compositor inserts as a client; only the
/// process boundary is gone. The X11 display (lock file and listening sockets)
/// is reserved once and handed to every run, so a restarted satellite serves the
/// same `DISPLAY`.
pub struct EmbeddedSatellite {
    display_name: String,
    /// The display's listening sockets, kept for restarts; each run gets
    /// duplicates, which it passes on to Xwayland.
    listeners: Vec<OwnedFd>,
    flags: Vec<String>,
    events: Sender<SatelliteEvent>,
    started_at: Option<Instant>,
    restart_delay: Duration,
    /// Identifies the current run, so a late event from an older one is ignored.
    generation: u64,
    /// Whether the current run reported Xwayland ready.
    ready: bool,
    /// Consecutive runs that ended before Xwayland was ready.
    failed_starts: u32,
}

impl EmbeddedSatellite {
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Start satellite on a new thread. Returns the compositor's end of its
    /// Wayland connection, to be inserted as a client.
    pub fn start(&mut self) -> io::Result<UnixStream> {
        let (compositor_end, satellite_end) = UnixStream::pair()?;
        let listenfds = self
            .listeners
            .iter()
            .map(OwnedFd::try_clone)
            .collect::<io::Result<Vec<_>>>()?;
        self.generation += 1;
        self.ready = false;
        let generation = self.generation;
        let data = EmbeddedRunData {
            display: self.display_name.clone(),
            listenfds: Mutex::new(listenfds),
            flags: self.flags.clone(),
            server: Mutex::new(Some(satellite_end)),
            events: self.events.clone(),
            generation,
        };
        let events = self.events.clone();
        std::thread::Builder::new()
            .name("xwayland-satellite".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    satellite::main(data);
                }));
                let panicked = result.is_err();
                let _ = events.send(SatelliteEvent::Exited {
                    generation,
                    panicked,
                });
            })?;
        self.started_at = Some(Instant::now());
        info!(
            display = %self.display_name,
            generation,
            "started embedded xwayland-satellite"
        );
        Ok(compositor_end)
    }

    /// The current run reported Xwayland ready.
    pub fn mark_ready(&mut self) {
        self.ready = true;
        self.failed_starts = 0;
    }

    /// The current run ended. Returns `false` when satellite should be given
    /// up on: Xwayland never came up in several runs in a row, so the display
    /// would only keep X11 clients waiting.
    pub fn run_ended(&mut self) -> bool {
        if !self.ready {
            self.failed_starts += 1;
        }
        self.failed_starts < MAX_FAILED_STARTS
    }

    /// How long to wait before restarting after the current run ended: short
    /// after a long healthy run, growing while satellite keeps failing fast.
    pub fn next_restart_delay(&mut self) -> Duration {
        let ran = self
            .started_at
            .map_or(Duration::ZERO, |start| start.elapsed());
        self.restart_delay = if ran >= HEALTHY_RUN {
            INITIAL_RESTART_DELAY
        } else {
            (self.restart_delay * 2).min(MAX_RESTART_DELAY)
        };
        self.restart_delay
    }
}

/// The first executable file named `name` in `PATH`.
fn find_in_path(name: &str) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(name))
        .find(|path| {
            std::fs::metadata(path)
                .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        })
}

/// `-glamor` for Xwayland, from `SHOJI_XWAYLAND_SATELLITE_GLAMOR`.
fn glamor_flags() -> Vec<String> {
    std::env::var("SHOJI_XWAYLAND_SATELLITE_GLAMOR")
        .ok()
        .filter(|glamor| matches!(glamor.as_str(), "gl" | "es" | "none"))
        .map(|glamor| vec!["-glamor".to_owned(), glamor])
        .unwrap_or_default()
}

/// What satellite needs from its embedder, for one run.
struct EmbeddedRunData {
    display: String,
    listenfds: Mutex<Vec<OwnedFd>>,
    flags: Vec<String>,
    server: Mutex<Option<UnixStream>>,
    events: Sender<SatelliteEvent>,
    generation: u64,
}

impl satellite::RunData for EmbeddedRunData {
    fn display(&self) -> Option<&str> {
        Some(&self.display)
    }

    fn listenfds(&mut self) -> Vec<OwnedFd> {
        std::mem::take(&mut *self.listenfds.lock().unwrap_or_else(|p| p.into_inner()))
    }

    fn flags(&self) -> &[String] {
        &self.flags
    }

    fn server(&self) -> Option<UnixStream> {
        self.server.lock().unwrap_or_else(|p| p.into_inner()).take()
    }

    fn xwayland_ready(&self, display: String, pid: u32) {
        let _ = self.events.send(SatelliteEvent::Ready {
            generation: self.generation,
            display,
            xwayland_pid: pid,
        });
    }
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

type ReservedX11Display = (
    u32,
    OwnedFd,
    UnlinkGuard,
    Option<UnixListener>,
    UnixListener,
    UnlinkGuard,
);

fn reserve_x11_display(start: u32) -> Result<ReservedX11Display, Box<dyn std::error::Error>> {
    for display_number in start..start + 50 {
        let lock_path = PathBuf::from(format!("/tmp/.X{display_number}-lock"));
        let flags = OFlags::WRONLY | OFlags::CLOEXEC | OFlags::CREATE | OFlags::EXCL;
        let lock_fd = match open(&lock_path, flags, 0o444.into()) {
            Ok(fd) => fd,
            // A lock left by a server that is gone (killed, crashed) holds the display for no
            // one: take it back with its socket, as Xorg and Xwayland do, and try again.
            Err(_) if lock_is_stale(&lock_path) => {
                info!(
                    display = %format_args!(":{display_number}"),
                    "removing a stale X11 display lock"
                );
                let _ = fs::remove_file(&lock_path);
                let _ = fs::remove_file(format!("/tmp/.X11-unix/X{display_number}"));
                let Ok(fd) = open(&lock_path, flags, 0o444.into()) else {
                    continue;
                };
                fd
            }
            Err(_) => continue,
        };
        let lock_guard = UnlinkGuard(lock_path);

        let pid_string = format!("{:>10}\n", getpid().as_raw_nonzero());
        rustix::io::write(&lock_fd, pid_string.as_bytes())?;

        match open_display_sockets(display_number) {
            Ok((abstract_listener, unix_listener, unix_guard)) => {
                return Ok((
                    display_number,
                    lock_fd,
                    lock_guard,
                    abstract_listener,
                    unix_listener,
                    unix_guard,
                ));
            }
            Err(error) if x11_socket_is_in_use(error.as_ref()) => {
                info!(
                    display = %format_args!(":{display_number}"),
                    ?error,
                    "X11 display socket is already in use; trying the next display"
                );
            }
            Err(error) => return Err(error),
        }
    }

    Err(io::Error::other("no free X11 display found").into())
}

/// Whether the X11 lock file at `path` names a process that no longer runs. A lock that cannot
/// be read, or holds no number, is not called stale: it may be one being written.
fn lock_is_stale(path: &Path) -> bool {
    let Ok(text) = fs::read_to_string(path) else {
        return false;
    };
    let Ok(pid) = text.trim().parse::<u32>() else {
        return false;
    };
    pid != 0 && !Path::new(&format!("/proc/{pid}")).exists()
}

fn x11_socket_is_in_use(error: &(dyn std::error::Error + 'static)) -> bool {
    error.downcast_ref::<io::Error>().is_some_and(|error| {
        matches!(
            error.kind(),
            io::ErrorKind::AddrInUse | io::ErrorKind::AlreadyExists
        )
    })
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
        .unwrap_or_else(|| crate::shoji_log_dir().join(SATELLITE_LOG_FILE))
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

#[cfg(test)]
mod mode_tests {
    use super::{SatelliteMode, satellite_mode_from};
    use std::{ffi::OsString, path::PathBuf};

    fn mode(setting: Option<&str>, path: Option<&str>) -> Option<SatelliteMode> {
        satellite_mode_from(setting.map(OsString::from), path.map(OsString::from))
    }

    #[test]
    fn embedded_by_default() {
        assert_eq!(mode(None, None), Some(SatelliteMode::Embedded));
        assert_eq!(mode(Some("1"), None), Some(SatelliteMode::Embedded));
        assert_eq!(mode(None, Some("")), Some(SatelliteMode::Embedded));
    }

    #[test]
    fn external_from_path_lookup_or_an_explicit_binary() {
        assert_eq!(
            mode(Some("external"), None),
            Some(SatelliteMode::External(PathBuf::from("xwayland-satellite")))
        );
        assert_eq!(
            mode(None, Some("/opt/xwls/xwayland-satellite")),
            Some(SatelliteMode::External(PathBuf::from(
                "/opt/xwls/xwayland-satellite"
            )))
        );
        // An explicit binary wins over the PATH lookup.
        assert_eq!(
            mode(Some("external"), Some("/opt/xwls/xwayland-satellite")),
            Some(SatelliteMode::External(PathBuf::from(
                "/opt/xwls/xwayland-satellite"
            )))
        );
    }

    #[test]
    fn off_disables_satellite_even_with_a_path() {
        assert_eq!(mode(Some("off"), None), None);
        assert_eq!(mode(Some("0"), Some("/opt/xwls/xwayland-satellite")), None);
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::{
        X11_TMP_UNIX_DIR, bind_to_abstract_socket, bind_to_unix_socket, ensure_x11_unix_dir,
        lock_is_stale, reserve_x11_display,
    };
    use std::{
        fs,
        path::PathBuf,
        sync::atomic::{AtomicU32, Ordering},
    };

    static NEXT_TEST_DISPLAY: AtomicU32 = AtomicU32::new(0);

    struct TestDisplayPaths {
        display_numbers: [u32; 2],
    }

    impl TestDisplayPaths {
        fn new() -> Self {
            let display_number = 100_000
                + (std::process::id() % 10_000) * 100
                + NEXT_TEST_DISPLAY.fetch_add(2, Ordering::Relaxed);
            let paths = Self {
                display_numbers: [display_number, display_number + 1],
            };
            paths.remove();
            paths
        }

        fn start(&self) -> u32 {
            self.display_numbers[0]
        }

        fn remove(&self) {
            for display_number in self.display_numbers {
                let _ = fs::remove_file(format!("/tmp/.X{display_number}-lock"));
                let _ = fs::remove_file(format!("{X11_TMP_UNIX_DIR}/X{display_number}"));
            }
        }
    }

    impl Drop for TestDisplayPaths {
        fn drop(&mut self) {
            self.remove();
        }
    }

    #[test]
    fn a_lock_of_a_process_that_is_gone_is_stale() {
        let dir = std::env::temp_dir().join(format!("shojiwm-lock-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let lock = dir.join(".X7-lock");
        // The test's own process runs: its lock is not stale.
        std::fs::write(&lock, format!("{:>10}\n", std::process::id())).unwrap();
        assert!(!lock_is_stale(&lock));
        // No process has this number (above pid_max).
        std::fs::write(&lock, format!("{:>10}\n", 4_194_305)).unwrap();
        assert!(lock_is_stale(&lock));
        // Being written: no number yet.
        std::fs::write(&lock, "").unwrap();
        assert!(!lock_is_stale(&lock));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn skips_a_display_with_an_occupied_abstract_socket() {
        ensure_x11_unix_dir().expect("prepare X11 socket directory");
        let paths = TestDisplayPaths::new();
        let occupied = bind_to_abstract_socket(paths.start()).expect("occupy abstract socket");

        let reservation = reserve_x11_display(paths.start()).expect("reserve next display");

        assert_eq!(reservation.0, paths.start() + 1);
        drop(reservation);
        drop(occupied);
    }

    #[test]
    fn does_not_unlink_an_occupied_pathname_socket() {
        ensure_x11_unix_dir().expect("prepare X11 socket directory");
        let paths = TestDisplayPaths::new();
        let (occupied, occupied_guard) =
            bind_to_unix_socket(paths.start()).expect("occupy pathname socket");

        let reservation = reserve_x11_display(paths.start()).expect("reserve next display");

        assert_eq!(reservation.0, paths.start() + 1);
        assert!(PathBuf::from(format!("{X11_TMP_UNIX_DIR}/X{}", paths.start())).exists());
        drop(reservation);
        drop(occupied);
        drop(occupied_guard);
    }
}
