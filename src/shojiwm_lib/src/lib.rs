// Compositor code passes lots of independent renderer/output/state args;
// not worth restructuring just for this lint.
#![allow(clippy::too_many_arguments)]
// smithay::desktop::Window hashes/compares by a stable id, so HashMap<Window, _> is fine.
#![allow(clippy::mutable_key_type)]

use crate::{
    backend::ShojiWMBackend,
    runtime_api::{RuntimeBoot, RuntimeLauncher, cli::CommonArgs},
};
use mimalloc::MiMalloc;
use std::{
    backtrace::Backtrace,
    process::ExitCode,
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
pub mod backlight;
pub mod color;
pub mod config;
pub mod config_error;
pub mod control_socket;
pub mod cursor;
pub mod frame_pacing;
pub mod drawing;
pub mod foreign_toplevel;
pub mod grabs;
pub mod handlers;
pub mod input;
pub mod keyboard_layout;
pub mod output_power;
pub mod presentation;
pub mod presentation_test_client;
pub mod process_env;
pub mod profiler;
pub mod protocols;
pub mod runtime_api;
pub mod runtime_debug;
pub mod runtime_input;
pub mod runtime_input_grab;
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

// Owned by the library rather than each binary: every ShojiWM build wants the
// same allocator, and the frame-time budget was measured with this one.
#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

/// Start ShojiWM with `launcher`'s config runtime. This is the whole `main` of
/// a ShojiWM binary:
///
/// ```ignore
/// fn main() -> std::process::ExitCode {
///     shoji_wm::run(MyLauncher::default())
/// }
/// ```
///
/// Every binary accepts the same common options (see
/// [`runtime_api::cli`]) plus whatever its launcher declares.
pub fn run(launcher: impl RuntimeLauncher + 'static) -> ExitCode {
    let args = CommonArgs::from_env(launcher.extra_args());
    if args.help {
        let binary = std::env::args()
            .next()
            .and_then(|path| {
                Path::new(&path)
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
            })
            .unwrap_or_else(|| "shoji_wm".to_string());
        print!(
            "{}",
            runtime_api::cli::help_text(&binary, launcher.name(), launcher.extra_args())
        );
        return ExitCode::SUCCESS;
    }
    if args.version {
        println!("shoji_wm {} ({})", env!("CARGO_PKG_VERSION"), launcher.name());
        return ExitCode::SUCCESS;
    }
    if let Some(command) = args.control_command {
        return match control_socket::send(command) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("Error: {error}");
                ExitCode::FAILURE
            }
        };
    }

    match run_with_args(Box::new(launcher), args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("Error: {error:?}");
            ExitCode::FAILURE
        }
    }
}

fn run_with_args(
    launcher: Box<dyn RuntimeLauncher>,
    args: CommonArgs,
) -> Result<(), Box<dyn std::error::Error>> {
    // Bound for the whole run: dropping this stops the log writer thread,
    // so it has to outlive `backend.run()`.
    let _log_guard = init_logging(&args)?;
    profiler::init();
    install_panic_hook();
    apply_runtime_overrides(&args);
    sanitize_inherited_compositor_environment();

    let backend = if args.tty {
        ShojiWMBackend::TTY
    } else {
        ShojiWMBackend::WInit
    };

    info!(?backend, runtime = launcher.name(), "starting shoji_wm");
    let boot = RuntimeBoot::new(launcher, &args);
    let result = backend.run(boot);
    profiler::dump_if_enabled("shutdown");
    if let Err(error) = &result {
        // `run` prints it to stderr too, but only after the log writer is gone; a session
        // started from a display manager rarely has its stderr anywhere the user looks.
        error!(error = %error, "shoji_wm exited with a fatal error");
    }
    result?;

    Ok(())
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

fn apply_runtime_overrides(args: &CommonArgs) {
    if !args.tty_outputs.is_empty() {
        process_env::set_var("SHOJI_TTY_OUTPUT", args.tty_outputs.join(","));
    }
    if let Some(path) = args.xwayland_satellite_path.as_deref() {
        process_env::set_var("SHOJI_XWAYLAND_SATELLITE_PATH", path);
    }
    if let Some(glamor) = args.xwayland_satellite_glamor.as_deref() {
        process_env::set_var("SHOJI_XWAYLAND_SATELLITE_GLAMOR", glamor);
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
        process_env::set_var(key, "");
    }

    process_env::set_var("XDG_CURRENT_DESKTOP", "ShojiWM");
    process_env::set_var("XDG_SESSION_DESKTOP", "ShojiWM");
    process_env::set_var("XDG_SESSION_TYPE", "wayland");
    process_env::set_var("DESKTOP_SESSION", "ShojiWM");
}

/// Installs the tracing subscriber, returning the writer's worker guard.
///
/// The guard must be held for the lifetime of the process: dropping it flushes
/// and stops the log writer thread.
fn init_logging(
    args: &CommonArgs,
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
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn,shojiwm_lib=info,shoji_wm=info,xwayland_satellite=info"));

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
