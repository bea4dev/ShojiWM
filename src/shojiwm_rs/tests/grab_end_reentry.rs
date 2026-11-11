//! A config handler for the end of a window move runs while the compositor
//! may still hold the pointer. Taking an input grab from it used to deadlock
//! on the pointer's lock.

use std::{os::unix::net::UnixStream, sync::mpsc, time::Duration};

use shojiwm_lib::{grabs::move_grab::MoveSurfaceGrab, state::ShojiWM};
use shojiwm_rs::{
    RuntimeBoot,
    cli::CommonArgs,
    prelude::*,
    ssd::{WindowMovePhaseSnapshot, WindowMoveSourceSnapshot},
};
use smithay::{
    backend::input::ButtonState,
    input::pointer::{ButtonEvent, Focus, GrabStartData},
    reexports::{calloop::EventLoop, wayland_server::Display},
    utils::{Rectangle, SERIAL_COUNTER},
};
use wayland_client::{
    Connection, Dispatch, QueueHandle,
    protocol::{wl_compositor, wl_registry, wl_surface},
};
use wayland_protocols::xdg::shell::client::{xdg_surface, xdg_toplevel, xdg_wm_base};

const BTN_LEFT: u32 = 0x110;

fn setup() {
    COMPOSITOR.event.on_window_move(|_, event| {
        if event.phase == WindowMovePhaseSnapshot::End {
            COMPOSITOR.input.grab(InputGrabOptions::new());
        }
    });
}

#[test]
fn input_grab_from_window_move_end_does_not_deadlock() {
    let runtime_dir = std::env::temp_dir().join(format!("shoji-grab-end-{}", std::process::id()));
    std::fs::create_dir_all(&runtime_dir).unwrap();
    // SAFETY: set before any other thread of this test binary starts.
    unsafe { std::env::set_var("XDG_RUNTIME_DIR", &runtime_dir) };

    let (done, finished) = mpsc::channel();
    std::thread::spawn(move || {
        let mut event_loop: EventLoop<ShojiWM> = EventLoop::try_new().unwrap();
        let display: Display<ShojiWM> = Display::new().unwrap();
        let args = CommonArgs::parse(&[], &[]);
        let boot = RuntimeBoot::new(Box::new(ConfigBuilder::new(setup)), &args);
        let mut state = ShojiWM::new(&mut event_loop, display, &boot);
        state.enable_initial_decoration_runtime();

        let socket = UnixStream::connect(runtime_dir.join(&state.socket_name)).unwrap();
        let connection = Connection::from_socket(socket).unwrap();
        let mut queue = connection.new_event_queue();
        let qh = queue.handle();
        let mut client = Client::default();
        let registry = connection.display().get_registry(&qh, ());
        // A few passes: the listener accepts the client on one, its requests
        // are read on the next.
        let mut roundtrip = |state: &mut ShojiWM, client: &mut Client| {
            for _ in 0..5 {
                connection.flush().unwrap();
                event_loop
                    .dispatch(Some(Duration::from_millis(5)), state)
                    .unwrap();
                state.display_handle.flush_clients().unwrap();
                if let Some(guard) = queue.prepare_read() {
                    let _ = guard.read();
                }
                queue.dispatch_pending(client).unwrap();
            }
        };
        roundtrip(&mut state, &mut client);
        let compositor = registry.bind::<wl_compositor::WlCompositor, _, _>(
            client.global("wl_compositor"),
            1,
            &qh,
            (),
        );
        let shell =
            registry.bind::<xdg_wm_base::XdgWmBase, _, _>(client.global("xdg_wm_base"), 1, &qh, ());
        let surface = compositor.create_surface(&qh, ());
        let xdg = shell.get_xdg_surface(&surface, &qh, ());
        let _toplevel = xdg.get_toplevel(&qh, ());
        surface.commit();
        roundtrip(&mut state, &mut client);

        let window = state
            .space
            .elements()
            .next()
            .cloned()
            .expect("toplevel mapped");
        let location = state.space.element_location(&window).unwrap();
        let pointer = state.seat.get_pointer().unwrap();
        let mut grab = MoveSurfaceGrab::start(
            GrabStartData {
                focus: None,
                button: BTN_LEFT,
                location: location.to_f64(),
            },
            window,
            location,
            Rectangle::new(location, (100, 100).into()),
            WindowMoveSourceSnapshot::Modifier,
        );
        grab.notify_start(&mut state);
        pointer.set_grab(&mut state, grab, SERIAL_COUNTER.next_serial(), Focus::Clear);
        pointer.button(
            &mut state,
            &ButtonEvent {
                serial: SERIAL_COUNTER.next_serial(),
                time: 0,
                button: BTN_LEFT,
                state: ButtonState::Released,
            },
        );
        event_loop
            .dispatch(Some(Duration::ZERO), &mut state)
            .unwrap();
        done.send(state.runtime_input_grab.is_some()).unwrap();
    });

    let grabbed = finished
        .recv_timeout(Duration::from_secs(10))
        .expect("the compositor deadlocked releasing the move grab");
    assert!(grabbed, "the End handler's input grab was not applied");
}

#[derive(Default)]
struct Client {
    globals: Vec<(String, u32)>,
}

impl Client {
    fn global(&self, interface: &str) -> u32 {
        self.globals
            .iter()
            .find(|(name, _)| name == interface)
            .unwrap_or_else(|| panic!("{interface} not advertised"))
            .1
    }
}

impl Dispatch<wl_registry::WlRegistry, ()> for Client {
    fn event(
        client: &mut Self,
        _: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name, interface, ..
        } = event
        {
            client.globals.push((interface, name));
        }
    }
}

impl Dispatch<xdg_wm_base::XdgWmBase, ()> for Client {
    fn event(
        _: &mut Self,
        shell: &xdg_wm_base::XdgWmBase,
        event: xdg_wm_base::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            shell.pong(serial);
        }
    }
}

macro_rules! ignore_events {
    ($($proxy:ty),*) => {$(
        impl Dispatch<$proxy, ()> for Client {
            fn event(_: &mut Self, _: &$proxy, _: <$proxy as wayland_client::Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
        }
    )*};
}

ignore_events!(
    wl_compositor::WlCompositor,
    wl_surface::WlSurface,
    xdg_surface::XdgSurface,
    xdg_toplevel::XdgToplevel
);
