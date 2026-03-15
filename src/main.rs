use crate::{backend::ShojiWMBackend, state::ShojiWM};
use std::{
    fs::{self, OpenOptions},
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};
use tracing::info;
use tracing_subscriber::EnvFilter;

pub mod backend;
pub mod cursor;
pub mod drawing;
pub mod grabs;
pub mod handlers;
pub mod input;
pub mod state;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    init_logging()?;

    let backend = std::env::args().nth(1);

    let backend = match backend.as_ref().map(|backend| backend.as_str()) {
        Some("--tty") => ShojiWMBackend::TTY,
        _ => ShojiWMBackend::WInit,
    };

    info!(?backend, "starting shoji_wm");
    backend.run()?;

    Ok(())
}

fn init_logging() -> Result<(), Box<dyn std::error::Error>> {
    let log_dir = shoji_log_dir();
    fs::create_dir_all(&log_dir)?;

    let latest_log = log_dir.join("latest.log");
    if latest_log.exists() {
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
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn,shoji_wm=debug"));

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
