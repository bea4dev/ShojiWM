//! A fullscreen window stacks above the Top layers (a taskbar): input and
//! both renderers ask `ShojiWM::fullscreen_window_above_top`. It holds for a
//! window in the Fullscreen state, also when it cannot take the direct
//! scanout path, and for an X11 window that covers the output without the
//! state ("borderless fullscreen" games). A Wayland window is never guessed
//! about.

mod common;

use std::{os::unix::net::UnixStream, rc::Rc};

use shojiwm_rs::prelude::*;
use smithay::reexports::wayland_server::Resource;
use wayland_client::{Connection, EventQueue, Proxy, protocol::wl_registry, protocol::wl_surface};

const OUTPUT_W: i32 = 1024;
const OUTPUT_H: i32 = 768;

/// `full` windows get the whole output; `short` ones leave a 68 px strip,
/// as a config with a frame or a reserved area does.
fn setup() {
    // A listener accepts the request: the compositor keeps the Fullscreen
    // state it staged, and the rect stays the config's.
    COMPOSITOR.event.on_window_fullscreen_request(|_, _| {});
    COMPOSITOR.window.composition(|window| {
        let app_id = window.app_id();
        ManagedWindow::new()
            .rect(Prop::Derived(Rc::new(move || {
                let height = if app_id
                    .get()
                    .as_deref()
                    .is_some_and(|id| id.starts_with("full"))
                {
                    OUTPUT_H
                } else {
                    OUTPUT_H - 68
                };
                Rect::new(0.0, 0.0, OUTPUT_W as f64, height as f64)
            })))
            .child(ClientWindow::new())
    });
}

fn output(session: &common::Session) -> smithay::output::Output {
    session.state.space.outputs().next().unwrap().clone()
}

fn above_top(session: &common::Session) -> Option<u32> {
    session
        .state
        .fullscreen_window_above_top(&output(session))
        .and_then(|window| window.toplevel().map(|t| t.wl_surface().id().protocol_id()))
}

/// What the pointer lands on at the bar's place (the top edge).
fn under_bar(session: &common::Session) -> Option<u32> {
    session
        .state
        .surface_under((500.0, 5.0).into())
        .map(|(surface, _)| surface.id().protocol_id())
}

/// A taskbar on the Top layer across the top of the output.
fn map_bar(session: &mut common::Session) -> wl_surface::WlSurface {
    let bar = session.map_layer("bar");
    session.attach_buffer(&bar, OUTPUT_W, 40);
    bar
}

fn settle(session: &mut common::Session) {
    session.state.refresh_window_decorations().unwrap();
    session.roundtrip();
}

#[test]
fn a_fullscreen_window_stacks_above_top_even_when_it_does_not_cover_the_output() {
    let mut session = common::Session::new(setup);
    let bar = map_bar(&mut session);
    let (surface, toplevel) = session.map_toplevel_with_toplevel("short", OUTPUT_W, OUTPUT_H - 68);
    settle(&mut session);
    let bar_id = Proxy::id(&bar).protocol_id();
    let window_id = Proxy::id(&surface).protocol_id();
    assert_eq!(above_top(&session), None);
    assert_eq!(under_bar(&session), Some(bar_id));

    toplevel.set_fullscreen(None);
    session.roundtrip();
    settle(&mut session);
    // The client acks the configure and commits.
    session.attach_buffer(&surface, OUTPUT_W, OUTPUT_H - 68);
    settle(&mut session);
    assert_eq!(above_top(&session), Some(window_id));
    assert_eq!(under_bar(&session), Some(window_id));
}

#[test]
fn a_wayland_window_that_covers_the_output_is_not_guessed_fullscreen() {
    let mut session = common::Session::new(setup);
    let bar = map_bar(&mut session);
    session.map_toplevel_with_buffer("full", OUTPUT_W, OUTPUT_H);
    settle(&mut session);
    assert_eq!(above_top(&session), None);
    assert_eq!(under_bar(&session), Some(Proxy::id(&bar).protocol_id()));
}

#[test]
fn an_x11_window_that_covers_the_output_stacks_above_top() {
    let mut session = common::Session::new(setup);
    let bar = map_bar(&mut session);
    let bar_id = Proxy::id(&bar).protocol_id();

    // Stand in for xwayland-satellite: a second client on the bridge's
    // connection.
    let (server_end, client_end) = UnixStream::pair().unwrap();
    session.state.insert_embedded_satellite_client(server_end);
    let mut bridge = Bridge::new(client_end, &mut session);

    let short = bridge.toplevel(&mut session, "short-game", OUTPUT_W, OUTPUT_H - 68);
    settle(&mut session);
    bridge.roundtrip(&mut session);
    assert_eq!(above_top(&session), None, "it does not cover the output");
    assert_eq!(under_bar(&session), Some(bar_id));
    drop(short);

    let full = bridge.toplevel(&mut session, "full-game", OUTPUT_W, OUTPUT_H);
    settle(&mut session);
    bridge.roundtrip(&mut session);
    let full_id = Proxy::id(&full).protocol_id();
    assert_eq!(above_top(&session), Some(full_id));
    assert_eq!(under_bar(&session), Some(full_id));
}

/// A client on its own connection, with the harness's dispatch rules.
struct Bridge {
    connection: Connection,
    queue: EventQueue<common::Client>,
    client: common::Client,
    registry: wl_registry::WlRegistry,
}

impl Bridge {
    fn new(socket: UnixStream, session: &mut common::Session) -> Self {
        let connection = Connection::from_socket(socket).unwrap();
        let queue = connection.new_event_queue();
        let registry = connection.display().get_registry(&queue.handle(), ());
        let mut bridge = Self {
            connection,
            queue,
            client: common::Client::default(),
            registry,
        };
        bridge.roundtrip(session);
        bridge
    }

    fn roundtrip(&mut self, session: &mut common::Session) {
        for _ in 0..5 {
            self.connection.flush().unwrap();
            session.roundtrip();
            if let Some(guard) = self.queue.prepare_read() {
                let _ = guard.read();
            }
            self.queue.dispatch_pending(&mut self.client).unwrap();
        }
    }

    fn bind<I>(&self, interface: &str, version: u32) -> I
    where
        I: Proxy + 'static,
        common::Client: wayland_client::Dispatch<I, ()>,
    {
        let name = self.client.globals_of(interface)[0];
        self.registry
            .bind::<I, _, _>(name, version, &self.queue.handle(), ())
    }

    fn toplevel(
        &mut self,
        session: &mut common::Session,
        app_id: &str,
        width: i32,
        height: i32,
    ) -> wl_surface::WlSurface {
        use std::os::fd::AsFd;
        use wayland_client::protocol::{wl_compositor, wl_shm};
        use wayland_protocols::xdg::shell::client::xdg_wm_base;
        let qh = self.queue.handle();
        let compositor: wl_compositor::WlCompositor = self.bind("wl_compositor", 1);
        let shell: xdg_wm_base::XdgWmBase = self.bind("xdg_wm_base", 1);
        let shm: wl_shm::WlShm = self.bind("wl_shm", 1);
        let surface = compositor.create_surface(&qh, ());
        let xdg = shell.get_xdg_surface(&surface, &qh, ());
        let toplevel = xdg.get_toplevel(&qh, ());
        toplevel.set_app_id(app_id.into());
        surface.commit();
        self.roundtrip(session);
        let stride = width * 4;
        let file = common::tempfile("bridge-shm");
        file.set_len((stride * height) as u64).unwrap();
        let pool = shm.create_pool(file.as_fd(), stride * height, &qh, ());
        let buffer =
            pool.create_buffer(0, width, height, stride, wl_shm::Format::Argb8888, &qh, ());
        surface.attach(Some(&buffer), 0, 0);
        surface.damage(0, 0, width, height);
        surface.commit();
        self.roundtrip(session);
        surface
    }
}
