mod external_picker;
mod i18n;
mod picker;
mod pipewire_stream;
mod screencast;
mod sources;
mod toplevel_stream;

use std::error::Error;
use std::sync::Arc;

use tracing_subscriber::EnvFilter;

const BUS_NAME: &str = "org.freedesktop.impl.portal.desktop.shojiwm";

fn main() -> Result<(), Box<dyn Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    tracing::info!("xdg-desktop-portal-shojiwm starting");
    i18n::init();

    // Build a multi-thread tokio runtime that lives on a dedicated worker
    // thread. The main thread is reserved for eframe/winit because winit
    // strongly prefers the main thread for its EventLoop on Linux.
    let runtime = Arc::new(
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .thread_name("portal-tokio")
            .build()?,
    );

    let (picker_handle, thumbnail_tx) = picker::setup();

    // Wait for a Wayland compositor before claiming the portal bus name.
    //
    // On logout the compositor dies and this process exits with it; systemd
    // restarts it immediately — in the gap between sessions. Claiming the
    // name while unable to serve poisons xdg-desktop-portal, which OUTLIVES
    // the session and reads each backend's properties only once per proxy:
    // it caches AvailableSourceTypes=0 for its whole lifetime and apps that
    // honour the bitmask (OBS) silently lose their PipeWire capture sources
    // in every subsequent session. Holding off the claim until the picker's
    // compositor connection can succeed means a mid-gap restart just waits
    // here until the next session's compositor is up.
    let mut waited = std::time::Duration::ZERO;
    loop {
        match wayland_client::Connection::connect_to_env() {
            Ok(_) => break,
            Err(e) => {
                if waited.is_zero() {
                    tracing::info!(
                        "waiting for a wayland compositor before claiming the bus name: {e}"
                    );
                }
                if waited >= std::time::Duration::from_secs(120) {
                    // WAYLAND_DISPLAY may have changed names across the
                    // relogin; exit so systemd relaunches us with fresh env.
                    tracing::error!("no wayland compositor after 120s; exiting");
                    std::process::exit(1);
                }
                std::thread::sleep(std::time::Duration::from_millis(500));
                waited += std::time::Duration::from_millis(500);
            }
        }
    }

    // Bring up zbus before iced starts spinning.
    let connection = runtime.block_on(async {
        zbus::connection::Builder::session()?
            .name(BUS_NAME)?
            .serve_at(
                "/org/freedesktop/portal/desktop",
                screencast::ScreenCast::new(picker_handle.clone(), thumbnail_tx),
            )?
            .build()
            .await
    })?;

    tracing::info!(bus = BUS_NAME, "claimed D-Bus name; serving ScreenCast");

    // Spawn a ctrl-c watcher on the runtime that will tear us down.
    let rt_for_signal = runtime.clone();
    std::thread::Builder::new()
        .name("portal-signal".into())
        .spawn(move || {
            rt_for_signal.block_on(async {
                if let Err(e) = tokio::signal::ctrl_c().await {
                    tracing::warn!("ctrl_c watcher failed: {e}");
                }
                tracing::info!("shutting down (ctrl-c)");
                std::process::exit(0);
            });
        })?;

    // Block here forever, owning the main thread for the winit event loop.
    // If the compositor connection breaks, iced/winit returns and the portal
    // is no longer useful: the picker cannot show UI, and future ScreenCast
    // requests may silently miss the ShojiWM backend. Terminate the process so
    // the systemd user unit relaunches us on the current Wayland display.
    let picker_result = picker::run_on_main_thread();
    drop(connection);
    match picker_result {
        Ok(()) => {
            tracing::error!("picker event loop exited unexpectedly");
        }
        Err(e) => {
            tracing::error!("picker iced exited: {e}");
        }
    }

    std::process::exit(1);
}
