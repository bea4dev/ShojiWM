pub mod async_assets;
pub mod clipped_memory;
pub mod clipped_surface;
pub mod damage;
pub mod damage_blink;
pub mod decoration;
pub mod icon;
pub mod rounded;
pub mod shader_effect;
pub mod snapshot;
pub mod text;
pub mod tty;
pub mod visual;
pub mod window;
pub mod winit;

use std::{
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use smithay::{
    backend::{
        drm::DrmNode,
        libinput::{LibinputInputBackend, LibinputSessionInterface},
        session::{Session, libseat::LibSeatSession},
        udev::{UdevBackend, primary_gpu},
    },
    reexports::{calloop::EventLoop, input::Libinput, wayland_server::Display},
};
use tracing::{info, trace, warn};

use crate::{
    backend::tty::{device_added, render_if_needed},
    config::tty_output_names_match,
    spawn_client,
    state::ShojiWM,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShojiWMBackend {
    WInit,
    TTY,
}

impl ShojiWMBackend {
    pub fn run(&self) -> Result<(), Box<dyn std::error::Error>> {
        match self {
            ShojiWMBackend::WInit => run_winit(),
            ShojiWMBackend::TTY => run_tty_udev(),
        }
    }
}

fn tty_maintenance_debug_enabled() -> bool {
    std::env::var_os("SHOJI_TTY_MAINTENANCE_DEBUG")
        .is_some_and(|value| value != "0" && !value.is_empty())
}

fn run_winit() -> Result<(), Box<dyn std::error::Error>> {
    let mut event_loop: EventLoop<ShojiWM> = EventLoop::try_new()?;
    let display: Display<ShojiWM> = Display::new()?;
    let mut state = ShojiWM::new(&mut event_loop, display);

    info!("initializing winit backend");
    winit::init_winit(&mut event_loop, &mut state)?;

    unsafe { std::env::set_var("WAYLAND_DISPLAY", &state.socket_name) };
    state.warmup_decoration_runtime();

    spawn_client();

    event_loop.run(None, &mut state, |_| {})?;
    Ok(())
}

pub fn run_tty_udev() -> Result<(), Box<dyn std::error::Error>> {
    let mut event_loop: EventLoop<ShojiWM> = EventLoop::try_new()?;
    let display: Display<ShojiWM> = Display::new()?;
    let mut state = ShojiWM::new(&mut event_loop, display);

    let (mut session, _session_notifier) = LibSeatSession::new()?;
    let seat_name = session.seat();
    info!(seat = %seat_name, "initialized tty session");

    let udev = UdevBackend::new(&seat_name)?;

    let mut libinput =
        Libinput::new_with_udev::<LibinputSessionInterface<LibSeatSession>>(session.clone().into());
    libinput.udev_assign_seat(&seat_name).map_err(|_| "")?;
    let libinput_backend = LibinputInputBackend::new(libinput);

    event_loop
        .handle()
        .insert_source(libinput_backend, |event, _, state| {
            state.record_event_source_wake("libinput");
            state.request_tty_maintenance("libinput");
            state.process_input_event(event);
        })?;

    let primary_node = primary_gpu(session.seat())?
        .as_ref()
        .map(DrmNode::from_path)
        .transpose()?;
    if let Some(primary_node) = primary_node {
        info!(?primary_node, "selected primary drm node");
    } else {
        warn!("no primary drm node reported by smithay");
    }

    let candidates = udev
        .device_list()
        .map(|(dev_id, path)| {
            let node = DrmNode::from_dev_id(dev_id)?;
            Ok(TtyDeviceCandidate {
                node,
                path: path.to_path_buf(),
                connected_connectors: connected_drm_connectors(path),
                is_primary: primary_node.is_some_and(|primary| primary == node),
            })
        })
        .collect::<Result<Vec<_>, Box<dyn std::error::Error>>>()?;

    if candidates.is_empty() {
        return Err("no drm devices found for tty backend".into());
    }

    for candidate in &candidates {
        if candidate.connected_connectors.is_empty() {
            info!(
                ?candidate.node,
                path = ?candidate.path,
                is_primary = candidate.is_primary,
                "discovered drm device without connected connectors"
            );
        } else {
            info!(
                ?candidate.node,
                path = ?candidate.path,
                is_primary = candidate.is_primary,
                connectors = ?candidate.connected_connectors,
                "discovered drm device with connected connectors"
            );
        }
    }

    let selected_devices = select_tty_devices(&candidates)?;
    info!(
        selected = ?selected_devices
            .iter()
            .map(|candidate| candidate.path.clone())
            .collect::<Vec<_>>(),
        "selected tty drm devices"
    );

    for candidate in selected_devices {
        let outputs_before = state.space.outputs().count();
        info!(
            ?candidate.node,
            path = ?candidate.path,
            connectors = ?candidate.connected_connectors,
            "initializing drm device"
        );
        device_added(
            &mut state,
            &event_loop,
            &mut session,
            candidate.node,
            &candidate.path,
        )?;

        let outputs_after = state.space.outputs().count();
        if outputs_after == outputs_before {
            warn!(
                ?candidate.node,
                path = ?candidate.path,
                "drm device initialized but did not add any outputs"
            );
        }
    }

    if state.space.outputs().next().is_none() {
        return Err(
            "tty backend did not find any connected drm outputs; set SHOJI_TTY_DRM_DEVICE=/dev/dri/cardN to override device selection"
                .into(),
        );
    }

    unsafe { std::env::set_var("WAYLAND_DISPLAY", &state.socket_name) };
    info!(socket = ?state.socket_name, "set wayland display for tty backend");
    state.warmup_decoration_runtime();
    std::process::Command::new("weston-terminal").spawn().ok();
    info!("spawned weston-terminal");

    spawn_client();

    let mut last_idle_maintenance_at = Instant::now();
    let maintenance_debug = tty_maintenance_debug_enabled();
    while state.is_running {
        let dispatch_timeout = if state.needs_redraw {
            Some(Duration::ZERO)
        } else {
            Some(Duration::from_millis(16))
        };
        if event_loop.dispatch(dispatch_timeout, &mut state).is_err() {
            break;
        }

        let event_source_wakes = state.take_event_source_wake_counts();
        let maintenance_pending = state.take_tty_maintenance_pending();
        let maintenance_reasons = state.take_tty_maintenance_reasons();
        let dispatched_wayland_requests = state.take_wayland_display_dispatched_request_count();
        let allow_idle_maintenance =
            last_idle_maintenance_at.elapsed() >= Duration::from_secs(1);

        // Ordering matters here.
        //
        // The Firefox high-CPU regression came from treating generic `wayland-display` wakeups as
        // a reason to run full TTY maintenance every loop iteration. A naive fix that merely
        // suppressed maintenance then caused another bug: the first commit after an animation or
        // other state transition would not become visible until some unrelated event (for example
        // pointer motion) triggered another redraw.
        //
        // The stable arrangement is:
        //
        // 1. dispatch Wayland/input/runtime events
        // 2. if maintenance is explicitly pending, run `space.refresh()/popups.cleanup()`
        //    *before* rendering
        // 3. render if a redraw is needed
        // 4. flush clients only if maintenance or rendering actually ran
        //
        // This preserves the pre-render refresh that newly dispatched commits depend on, while
        // avoiding the old self-amplifying "display wake => maintenance => flush => more display
        // wake" loop that Firefox could trigger.
        let should_run_maintenance = maintenance_pending || allow_idle_maintenance;
        if maintenance_debug
            && (should_run_maintenance
                || state.needs_redraw
                || !event_source_wakes.is_empty()
                || dispatched_wayland_requests > 0)
        {
            info!(
                needs_redraw = state.needs_redraw,
                maintenance_pending,
                maintenance_reasons = ?maintenance_reasons,
                allow_idle_maintenance,
                dispatched_wayland_requests,
                event_source_wakes = ?event_source_wakes,
                "tty maintenance decision",
            );
        }

        let mut ran_pre_render_maintenance = false;
        if should_run_maintenance {
            ran_pre_render_maintenance = true;
            let window_count_before_refresh = state.space.elements().count();
            state.space.refresh();
            let window_count_after_refresh = state.space.elements().count();
            if window_count_after_refresh != window_count_before_refresh {
                state.schedule_redraw();
            }
            state.popups.cleanup();
        }

        let mut rendered_this_iteration = false;
        if state.needs_redraw {
            trace!("tty loop observed pending redraw");
            rendered_this_iteration = true;
        }
        render_if_needed(&mut state, &event_loop.handle())?;

        if rendered_this_iteration || ran_pre_render_maintenance {
            let _ = state.display_handle.flush_clients();
        }
        if ran_pre_render_maintenance && !rendered_this_iteration && !state.needs_redraw {
            last_idle_maintenance_at = Instant::now();
        }
    }

    info!("tty backend loop exited");
    Ok(())
}

#[derive(Debug, Clone)]
struct TtyDeviceCandidate {
    node: DrmNode,
    path: PathBuf,
    connected_connectors: Vec<String>,
    is_primary: bool,
}

fn select_tty_devices(
    candidates: &[TtyDeviceCandidate],
) -> Result<Vec<&TtyDeviceCandidate>, Box<dyn std::error::Error>> {
    let desired_outputs = std::env::var_os("SHOJI_TTY_OUTPUT")
        .map(|value| {
            value
                .to_string_lossy()
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>()
        })
        .filter(|outputs| !outputs.is_empty());

    if let Some(override_value) = std::env::var_os("SHOJI_TTY_DRM_DEVICE") {
        if override_value == "all" {
            return Ok(candidates.iter().collect());
        }

        let override_values = override_value
            .to_string_lossy()
            .split(',')
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>();
        let selected = candidates
            .iter()
            .filter(|candidate| {
                override_values
                    .iter()
                    .any(|value| path_matches_override(&candidate.path, OsStr::new(value)))
            })
            .collect::<Vec<_>>();

        if selected.is_empty() {
            return Err(format!(
                "SHOJI_TTY_DRM_DEVICE={:?} did not match any discovered drm device",
                override_value
            )
            .into());
        }

        return Ok(selected);
    }

    let candidates = if let Some(desired_outputs) = &desired_outputs {
        let selected = candidates
            .iter()
            .filter(|candidate| {
                candidate.connected_connectors.iter().any(|connector| {
                    desired_outputs
                        .iter()
                        .any(|desired| tty_output_names_match(desired, connector))
                })
            })
            .collect::<Vec<_>>();
        if selected.is_empty() {
            return Err(format!(
                "SHOJI_TTY_OUTPUT={:?} did not match any connected drm connector",
                desired_outputs
            )
            .into());
        }
        selected
    } else {
        candidates.iter().collect::<Vec<_>>()
    };

    let connected = candidates
        .iter()
        .copied()
        .filter(|candidate| !candidate.connected_connectors.is_empty())
        .collect::<Vec<_>>();
    if !connected.is_empty() {
        if let Some(primary_connected) = connected
            .iter()
            .copied()
            .find(|candidate| candidate.is_primary)
        {
            return Ok(vec![primary_connected]);
        }

        let best = connected
            .iter()
            .copied()
            .max_by_key(|candidate| candidate.connected_connectors.len())
            .unwrap();
        return Ok(vec![best]);
    }

    let primary = candidates
        .iter()
        .filter(|candidate| candidate.is_primary)
        .collect::<Vec<_>>();
    if !primary.is_empty() {
        warn!("no connected drm connectors detected; falling back to primary gpu");
        return Ok(vec![primary[0]]);
    }

    warn!(
        "no connected drm connectors detected and no primary gpu match found; falling back to first drm device"
    );
    Ok(vec![&candidates[0]])
}

fn path_matches_override(path: &Path, override_value: &OsStr) -> bool {
    path == Path::new(override_value) || path.file_name().is_some_and(|name| name == override_value)
}

fn connected_drm_connectors(card_path: &Path) -> Vec<String> {
    let Some(card_name) = card_path.file_name().and_then(|name| name.to_str()) else {
        return Vec::new();
    };

    fs::read_dir("/sys/class/drm")
        .ok()
        .into_iter()
        .flat_map(|entries| entries.filter_map(Result::ok))
        .filter_map(|entry| {
            let name = entry.file_name();
            let name = name.to_str()?;
            (name.starts_with(card_name) && name.as_bytes().get(card_name.len()) == Some(&b'-'))
                .then_some((name.to_string(), entry.path()))
        })
        .filter_map(|(name, path)| {
            let status = fs::read_to_string(path.join("status")).ok()?;
            (status.trim() == "connected").then_some(name)
        })
        .collect()
}
