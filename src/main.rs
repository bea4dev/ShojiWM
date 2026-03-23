use crate::{backend::ShojiWMBackend, state::ShojiWM};
use std::{
    fs::{self, OpenOptions},
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};
use tracing::info;
use tracing_subscriber::EnvFilter;

pub mod backend;
pub mod config;
pub mod cursor;
pub mod drawing;
pub mod grabs;
pub mod handlers;
pub mod input;
pub mod presentation;
pub mod ssd;
pub mod state;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = CliArgs::parse();
    init_logging(&args)?;
    apply_runtime_overrides(&args);

    let backend = if args.tty {
        ShojiWMBackend::TTY
    } else {
        ShojiWMBackend::WInit
    };

    info!(?backend, "starting shoji_wm");
    backend.run()?;

    Ok(())
}

fn apply_runtime_overrides(args: &CliArgs) {
    if !args.tty_outputs.is_empty() {
        unsafe { std::env::set_var("SHOJI_TTY_OUTPUT", args.tty_outputs.join(",")) };
    }
}

#[derive(Debug, Clone)]
struct CliArgs {
    tty: bool,
    log_off: bool,
    no_log_rotate: bool,
    tty_outputs: Vec<String>,
}

impl CliArgs {
    fn parse() -> Self {
        let args: Vec<String> = std::env::args().skip(1).collect();
        let env_log_off = std::env::var_os("SHOJI_LOG")
            .is_some_and(|value| value == "off" || value == "0");
        let env_no_rotate = std::env::var_os("SHOJI_LOG_ROTATE")
            .is_some_and(|value| value == "0" || value == "off");

        let tty_outputs = parse_tty_outputs(&args);

        Self {
            tty: args.iter().any(|arg| arg == "--tty"),
            log_off: args.iter().any(|arg| arg == "--log-off") || env_log_off,
            no_log_rotate: args.iter().any(|arg| arg == "--no-log-rotate") || env_no_rotate,
            tty_outputs,
        }
    }
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
    let file_writer = move || {
        log_file
            .try_clone()
            .expect("failed to clone latest.log for tracing")
    };

    let env_filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn,shoji_wm=info"));

    tracing_subscriber::fmt()
        .compact()
        .with_ansi(false)
        .with_writer(file_writer)
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

fn spawn_client() {
    std::process::Command::new("kitty").spawn().ok();
}
