use smithay::reexports::{calloop::EventLoop, wayland_server::Display};

use crate::state::ShojiWM;

pub mod state;
pub mod handlers;
pub mod grabs;
pub mod winit;
pub mod input;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut event_loop: EventLoop<ShojiWM> = EventLoop::try_new()?;

    let display: Display<ShojiWM> = Display::new()?;

    let mut state = ShojiWM::new(&mut event_loop, display);

    // Open a Wayland/X11 window for our nested compositor
    crate::winit::init_winit(&mut event_loop, &mut state)?;

    // Set WAYLAND_DISPLAY to our socket name, so child processes connect to ShojiWM rather
    // than the host compositor
    unsafe { std::env::set_var("WAYLAND_DISPLAY", &state.socket_name); }

    // Spawn a test client, that will run under ShojiWM
    spawn_client();

    event_loop.run(None, &mut state, move |_| {
        // ShojiWM is running
    })?;

    Ok(())
}

fn spawn_client() {
    let mut args = std::env::args().skip(1);
    let flag = args.next();
    let arg = args.next();

    match (flag.as_deref(), arg) {
        (Some("-c") | Some("--command"), Some(command)) => {
            std::process::Command::new(command).spawn().ok();
        }
        _ => {
            std::process::Command::new("weston-terminal").spawn().ok();
        }
    }
}
