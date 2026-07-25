use crate::{backend::ShojiWMBackend, state::ShojiWM};
use mimalloc::MiMalloc;
use std::{
    backtrace::Backtrace,
    fs::{self, OpenOptions},
    panic,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

pub mod activation_environment;
pub mod backend;
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
    init_logging(&args)?;
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
        } else if arg == "--tty-output" {
            if let Some(value) = args.get(index + 1) {
                outputs.extend(split_tty_outputs(value));
                index += 1;
            }
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
        } else if arg == option {
            if let Some(value) = args.get(index + 1).filter(|value| !value.is_empty()) {
                return Some(value.clone());
            }
        }
        index += 1;
    }
    None
}

fn init_logging(args: &CliArgs) -> Result<(), Box<dyn std::error::Error>> {
    if args.log_off {
        return Ok(());
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

    tracing_subscriber::fmt()
        .compact()
        .with_ansi(false)
        .with_writer(std::sync::Mutex::new(log_file))
        .with_env_filter(env_filter)
        .init();

    Ok(())
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
