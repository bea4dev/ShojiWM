pub mod clipped_surface;
pub mod damage;
pub mod damage_blink;
pub mod decoration;
pub mod rounded;
pub mod text;
pub mod tty;
pub mod winit;
pub mod window;

use std::time::Duration;

use smithay::{
    backend::{
        drm::DrmNode,
        libinput::{LibinputInputBackend, LibinputSessionInterface},
        session::{Session, libseat::LibSeatSession},
        udev::{UdevBackend, primary_gpu},
    },
    reexports::{calloop::EventLoop, input::Libinput, wayland_server::Display},
};
use tracing::{info, trace};

use crate::{
    backend::tty::{device_added, render_if_needed}, spawn_client, state::ShojiWM
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

fn run_winit() -> Result<(), Box<dyn std::error::Error>> {
    let mut event_loop: EventLoop<ShojiWM> = EventLoop::try_new()?;
    let display: Display<ShojiWM> = Display::new()?;
    let mut state = ShojiWM::new(&mut event_loop, display);

    info!("initializing winit backend");
    winit::init_winit(&mut event_loop, &mut state)?;

    unsafe { std::env::set_var("WAYLAND_DISPLAY", &state.socket_name) };

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
            state.process_input_event(event);
        })?;

    // 本当は ShojiWM 側に backend 用フィールドを追加して保持する
    // ここでは説明のため local で初期化フローだけ示す
    let primary = primary_gpu(session.seat())?.expect("no gpu");
    let primary_node = DrmNode::from_path(primary)?;
    info!(?primary_node, "selected primary drm node");

    for (dev_id, path) in udev.device_list() {
        let node = DrmNode::from_dev_id(dev_id)?;
        if node == primary_node {
            info!(?node, path = ?path, "initializing drm device");
            device_added(&mut state, &event_loop, &mut session, node, path)?;
        }
    }

    unsafe { std::env::set_var("WAYLAND_DISPLAY", &state.socket_name) };
    info!(socket = ?state.socket_name, "set wayland display for tty backend");
    std::process::Command::new("weston-terminal").spawn().ok();
    info!("spawned weston-terminal");

    spawn_client();

    while state.is_running {
        if event_loop
            .dispatch(Some(Duration::from_millis(16)), &mut state)
            .is_err()
        {
            break;
        }

        if state.needs_redraw {
            trace!("tty loop observed pending redraw");
        }
        render_if_needed(&mut state)?;

        let window_count_before_refresh = state.space.elements().count();
        state.space.refresh();
        let window_count_after_refresh = state.space.elements().count();
        if window_count_after_refresh != window_count_before_refresh {
            state.schedule_redraw();
        }
        state.popups.cleanup();
        let _ = state.display_handle.flush_clients();
    }

    info!("tty backend loop exited");
    Ok(())
}
