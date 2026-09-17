// Compositor code passes lots of independent renderer/output/state args;
// not worth restructuring just for this lint.
#![allow(clippy::too_many_arguments)]
// smithay::desktop::Window hashes/compares by a stable id, so HashMap<Window, _> is fine.
#![allow(clippy::mutable_key_type)]

use crate::{backend::ShojiWMBackend, state::ShojiWM};
use mimalloc::MiMalloc;
use std::{
    backtrace::Backtrace,
    fs::{self, OpenOptions},
    panic,
    path::{
        Path, 
        PathBuf
    },
    time::{
        Duration, 
        SystemTime, 
        UNIX_EPOCH
    },
};
use tracing::{
    error, 
    info,
    warn
};
use tracing_subscriber::EnvFilter;

pub mod activation_environment;
pub mod backend;
pub mod color;
pub mod config;
pub mod config_error;
pub mod cursor;
pub mod drawing;
pub mod foreign_toplevel;
pub mod grabs;
pub mod handlers;
pub mod input;
pub mod install_paths;
pub mod presentation;
pub mod profiler;
pub mod protocols;
pub mod runtime_debug;
pub mod runtime_input;
pub mod runtime_key_binding;
pub mod runtime_pointer;
pub mod runtime_process;
pub mod runtime_workspace;
pub mod ssd;
pub mod state;
pub mod window_decoration;
pub mod wlr_foreign_toplevel;
pub mod workspace_manager;
pub mod xwayland_satellite;

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Block SIGUSR1 in the main thread *before* spawning any threads.
    // The TS runtime uses SIGUSR1 to wake the compositor after IPC-driven
    // state changes; calloop's signalfd source reads it from a single
    // controlled point. Subsequent threads inherit the mask, so no random
    // worker can absorb the signal first.
    block_runtime_wake_signal();

    let args = CliArgs::parse();
    // Bound for the whole of main: dropping this stops the log writer thread,
    // so it has to outlive `backend.run()`.
    let _log_guard = init_logging(&args)?;
    profiler::init();
    install_panic_hook();
    apply_runtime_overrides(&args);
    init_runtime_paths(&args);
    sanitize_inherited_compositor_environment();

    let backend = if args.tty {
        ShojiWMBackend::TTY
    } else {
        ShojiWMBackend::WInit
    };

    info!(?backend, "starting shoji_wm");
    let result = backend.run();
    profiler::dump_if_enabled("shutdown");
    result?;

    Ok(())
}

fn block_runtime_wake_signal() {
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, libc::SIGUSR1);
        libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut());
    }
}

fn install_panic_hook() {
    let default_hook = panic::take_hook();
    panic::set_hook(Box::new(move |panic_info| {
        let location = panic_info
            .location()
            .map(|location| {
                format!(
                    "{}:{}:{}",
                    location.file(),
                    location.line(),
                    location.column()
                )
            })
            .unwrap_or_else(|| "<unknown>".to_string());
        let payload = panic_payload_message(panic_info);
        let thread = std::thread::current();
        let thread_name = thread.name().unwrap_or("<unnamed>");
        let backtrace = Backtrace::force_capture();

        error!(
            thread = thread_name,
            location = %location,
            payload = %payload,
            backtrace = %backtrace,
            "panic"
        );
        eprintln!("panic: thread={thread_name} location={location} payload={payload}\n{backtrace}");

        default_hook(panic_info);
    }));
}

fn panic_payload_message(panic_info: &panic::PanicHookInfo<'_>) -> String {
    let payload = panic_info.payload();
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_string()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "non-string panic payload".to_string()
    }
}

fn apply_runtime_overrides(args: &CliArgs) {
    if !args.tty_outputs.is_empty() {
        unsafe { std::env::set_var("SHOJI_TTY_OUTPUT", args.tty_outputs.join(",")) };
    }
    if let Some(path) = args.xwayland_satellite_path.as_deref() {
        unsafe { std::env::set_var("SHOJI_XWAYLAND_SATELLITE_PATH", path) };
    }
    if let Some(glamor) = args.xwayland_satellite_glamor.as_deref() {
        unsafe { std::env::set_var("SHOJI_XWAYLAND_SATELLITE_GLAMOR", glamor) };
    }
}

fn sanitize_inherited_compositor_environment() {
    for key in [
        "NIRI_SOCKET",
        "HYPRLAND_INSTANCE_SIGNATURE",
        "SWAYSOCK",
        "I3SOCK",
        "LABWC_PID",
    ] {
        unsafe { std::env::set_var(key, "") };
    }

    unsafe {
        std::env::set_var("XDG_CURRENT_DESKTOP", "ShojiWM");
        std::env::set_var("XDG_SESSION_DESKTOP", "ShojiWM");
        std::env::set_var("XDG_SESSION_TYPE", "wayland");
        std::env::set_var("DESKTOP_SESSION", "ShojiWM");
    }
}

#[derive(Debug, Clone)]
struct CliArgs {
    tty: bool,
    log_off: bool,
    no_log_rotate: bool,
    tty_outputs: Vec<String>,
    xwayland_satellite_path: Option<String>,
    xwayland_satellite_glamor: Option<String>,
    dev: bool,
    config_path: Option<PathBuf>,
    runtime_dir: Option<PathBuf>,
    decoration_runtime: Option<PathBuf>,
}

impl CliArgs {
    fn parse() -> Self {
        let args: Vec<String> = std::env::args().skip(1).collect();
        let env_log_off =
            std::env::var_os("SHOJI_LOG").is_some_and(|value| value == "off" || value == "0");
        let env_no_rotate = std::env::var_os("SHOJI_LOG_ROTATE")
            .is_some_and(|value| value == "0" || value == "off");
        let env_xwayland_satellite_path = std::env::var("SHOJI_XWAYLAND_SATELLITE_PATH").ok();
        let env_xwayland_satellite_glamor = std::env::var("SHOJI_XWAYLAND_SATELLITE_GLAMOR").ok();

        let tty_outputs = parse_tty_outputs(&args);
        let xwayland_satellite_path =
            parse_option_value(&args, "--xwayland-satellite-path").or(env_xwayland_satellite_path);
        let xwayland_satellite_glamor = parse_option_value(&args, "--xwayland-satellite-glamor")
            .or(env_xwayland_satellite_glamor)
            .filter(|value| matches!(value.as_str(), "gl" | "es" | "none"));
        let config_path = parse_option_value(&args, "--config").map(PathBuf::from);
        let runtime_dir = parse_option_value(&args, "--runtime-dir").map(PathBuf::from);
        let decoration_runtime =
            parse_option_value(&args, "--decoration-runtime").map(PathBuf::from);

        Self {
            tty: args.iter().any(|arg| arg == "--tty"),
            log_off: args.iter().any(|arg| arg == "--log-off") || env_log_off,
            no_log_rotate: args.iter().any(|arg| arg == "--no-log-rotate") || env_no_rotate,
            tty_outputs,
            xwayland_satellite_path,
            xwayland_satellite_glamor,
            dev: args.iter().any(|arg| arg == "--dev"),
            config_path,
            runtime_dir,
            decoration_runtime,
        }
    }
}

fn init_runtime_paths(args: &CliArgs) {
    install_paths::init_runtime_path_options(install_paths::RuntimePathOptions {
        dev: args.dev,
        config_path: args.config_path.clone(),
        runtime_dir: args.runtime_dir.clone(),
        decoration_runtime: args.decoration_runtime.clone(),
    });
}

fn parse_tty_outputs(args: &[String]) -> Vec<String> {
    let mut outputs = Vec::new();
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        if let Some(value) = arg.strip_prefix("--tty-output=") {
            outputs.extend(split_tty_outputs(value));
        } else if arg == "--tty-output"
            && let Some(value) = args.get(index + 1) {
                outputs.extend(split_tty_outputs(value));
                index += 1;
            }
        index += 1;
    }
    outputs
}

fn split_tty_outputs(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn parse_option_value(args: &[String], option: &str) -> Option<String> {
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        if let Some(value) = arg.strip_prefix(&format!("{option}=")) {
            if !value.is_empty() {
                return Some(value.to_string());
            }
        } else if arg == option
            && let Some(value) = args.get(index + 1).filter(|value| !value.is_empty()) {
                return Some(value.clone());
            }
        index += 1;
    }
    None
}

/// Installs the tracing subscriber, returning the writer's worker guard.
///
/// The guard must be held for the lifetime of the process: dropping it flushes
/// and stops the log writer thread.
fn init_logging(
    args: &CliArgs,
) -> Result<Option<tracing_appender::non_blocking::WorkerGuard>, Box<dyn std::error::Error>> {
    if args.log_off {
        return Ok(None);
    }

    let log_dir = shoji_log_dir();
    fs::create_dir_all(&log_dir)?;

    let latest_log = log_dir.join("latest.log");
    if !args.no_log_rotate && latest_log.exists() {
        let rolled = log_dir.join(format!("{}.log", startup_timestamp_millis()));
        fs::rename(&latest_log, rolled)?;
    }

    let log_file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&latest_log)?;
    let env_filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn,shoji_wm=info"));

    // The compositor runs a single-threaded event loop, so a synchronous
    // writer here puts a filesystem write directly in the path of input and
    // rendering. On a near-full btrfs that write can stall for seconds to
    // minutes, which surfaces as a frozen cursor with nothing in the
    // log to explain it: the blocked write is the missing line. Hand the
    // file to a worker thread instead, and drop records once the queue backs
    // up — losing log lines always beats stalling the session.
    let (writer, guard) = tracing_appender::non_blocking::NonBlockingBuilder::default()
        .lossy(true)
        .buffered_lines_limit(64 * 1024)
        .finish(log_file);

    tracing_subscriber::fmt()
        .compact()
        .with_ansi(false)
        .with_writer(writer)
        .with_env_filter(env_filter)
        .init();

    // After the subscriber is installed, so the summary lands in the log it
    // just pruned around.
    prune_rotated_logs(&log_dir, &LogRetention::from_env());

    Ok(Some(guard))
}

/// Retention policy for rotated session logs.
///
/// Rotation renames `latest.log` to `<epoch-ms>.log` on every start and
/// nothing ever removed the result, so the directory grew without bound: it
/// reached 71 GB / 112 files, one runaway session having left a single 54 GB
/// file behind. Ordinary tty sessions are 50 KB–900 KB, so the default budget
/// holds hundreds of them.
struct LogRetention {
    /// Newest rotated logs kept unconditionally, so one oversized session
    /// cannot evict the recent history that is actually worth having.
    min_keep: usize,
    /// Budget for all rotated logs combined, `latest.log` excluded.
    /// `None` disables the size check.
    max_total_bytes: Option<u64>,
    /// Age limit for rotated logs. `None` disables the age check.
    max_age: Option<Duration>,
}

impl Default for LogRetention {
    fn default() -> Self {
        Self {
            min_keep: 5,
            max_total_bytes: Some(512 * 1024 * 1024),
            max_age: Some(
                Duration::from_secs(
                    30 * 24 * 60 * 60
                )
            ),
        }
    }
}

impl LogRetention {
    /// `SHOJI_LOG_KEEP_BYTES` and `SHOJI_LOG_KEEP_DAYS` override the defaults;
    /// `0` or `off` disables that check. Matches how `SHOJI_LOG` and
    /// `SHOJI_LOG_ROTATE` gate the rest of the logging setup.
    fn from_env() -> Self {
        let mut retention = Self::default();
        if let Some(bytes) = parse_retention_env(
            "SHOJI_LOG_KEEP_BYTES",
        ) {
            retention.max_total_bytes = (bytes > 0)
                .then_some(bytes);
        }
        if let Some(days) = parse_retention_env(
            "SHOJI_LOG_KEEP_DAYS",
        ) {
            retention.max_age =
                (days > 0).then(|| Duration::from_secs(days.saturating_mul(24 * 60 * 60)));
        }
        retention
    }
}

fn parse_retention_env(
    name: &str
) -> Option<u64> {
    let value = std::env::var(name)
        .ok()?;
    let value = value
        .trim();
    if value.eq_ignore_ascii_case("off") {
        return Some(0);
    }
    value
        .parse()
        .ok()
}

/// Drop rotated logs that fall outside `retention`.
///
/// Only files this rotation produced (`<epoch-ms>.log`) are considered, so
/// `latest.log` and anything a human put here are left alone. Failures are
/// reported and swallowed: a log directory we cannot tidy is not a reason to
/// refuse to start a session.
fn prune_rotated_logs(
    log_dir: &Path,
    retention: &LogRetention,
) {
    let entries = match fs::read_dir(log_dir) {
        Ok(entries) => entries,
        Err(err) => {
            warn!(
                ?err,
                "could not read log directory to apply retention",
            );
            return;
        }
    };

    let mut rotated: Vec<(u128, u64, PathBuf)> = Vec::new();
    for entry in entries.flatten() {
        let path = entry
            .path();
        let Some(stamp) = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_suffix(".log"))
            .and_then(|stem| stem.parse::<u128>().ok())
        else {
            continue;
        };
        let size = entry
            .metadata()
            .map(|metadata| metadata.len()).unwrap_or(0);
        rotated
            .push(
                (
                    stamp,
                    size, 
                    path,
                ),
            );
    }

    // Newest first. The rotation timestamp is a better ordering key than
    // mtime, which a copy or a backup pass can rewrite.
    rotated.sort_unstable_by_key(|entry| std::cmp::Reverse(entry.0));

    let now = startup_timestamp_millis();
    let mut kept_bytes = 0_u64;
    let mut removed_files = 0_usize;
    let mut removed_bytes = 0_u64;

    for (index, (stamp, size, path)) in rotated.iter().enumerate() {
        let too_old = retention
            .max_age
            .is_some_and(|max_age| now.saturating_sub(*stamp) > max_age.as_millis());
        let over_budget = retention
            .max_total_bytes
            .is_some_and(|budget| kept_bytes.saturating_add(*size) > budget);

        if index < retention.min_keep || !(too_old || over_budget) {
            kept_bytes = kept_bytes.saturating_add(*size);
            continue;
        }

        match fs::remove_file(path) {
            Ok(()) => {
                removed_files += 1;
                removed_bytes = removed_bytes.saturating_add(*size);
            }
            Err(err) => {
                // Keep counting it against the budget: it is still on disk.
                warn!(?path, ?err, "could not remove expired session log");
                kept_bytes = kept_bytes.saturating_add(*size);
            }
        }
    }

    if removed_files > 0 {
        info!(
            removed_files,
            removed_mib = removed_bytes / (1024 * 1024),
            kept_files = rotated.len() - removed_files,
            kept_mib = kept_bytes / (1024 * 1024),
            "pruned rotated session logs"
        );
    }
}

fn shoji_log_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("shoji_wm")
        .join("logs")
}

fn startup_timestamp_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}
