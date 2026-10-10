//! A real xdg-toplevel client talking to an in-process smithay server over a
//! socket pair, for presentation tests that need genuine `wl_surface` state.
#![cfg(test)]

use std::{collections::HashMap, fs::File, os::fd::AsFd, os::unix::net::UnixStream, sync::Arc};

use smithay::{
    backend::renderer::utils::on_commit_buffer_handler,
    desktop::Window,
    reexports::rustix::fs::{MemfdFlags, memfd_create},
    utils::Serial,
    wayland::{
        buffer::BufferHandler,
        compositor::{CompositorClientState, CompositorHandler, CompositorState},
        shell::xdg::{
            PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
        },
        shm::{ShmHandler, ShmState},
    },
};
use wayland_client::{
    Connection, Dispatch, EventQueue, QueueHandle,
    protocol::{
        wl_buffer, wl_callback, wl_compositor, wl_registry, wl_shm, wl_shm_pool, wl_surface,
    },
};
use wayland_protocols::xdg::shell::client::{xdg_surface, xdg_toplevel, xdg_wm_base};
use wayland_server::{Client, Display, backend::ClientData, protocol as server};

pub(crate) struct TestClient {
    display: Display<Server>,
    server: Server,
    connection: Connection,
    queue: EventQueue<ClientState>,
    client: ClientState,
}

impl TestClient {
    pub(crate) fn new() -> Self {
        // A socket pair keeps the real protocol exchange independent of the
        // desktop's display and IPC namespace.
        let display = Display::new().unwrap();
        let mut handle = display.handle();
        let server = Server {
            compositor: CompositorState::new::<Server>(&handle),
            shell: XdgShellState::new::<Server>(&handle),
            shm: ShmState::new::<Server>(&handle, []),
            window: None,
        };
        let (server_socket, client_socket) = UnixStream::pair().unwrap();
        handle
            .insert_client(server_socket, Arc::new(ServerClient::default()))
            .unwrap();
        let connection = Connection::from_socket(client_socket).unwrap();
        let queue = connection.new_event_queue();
        let qh = queue.handle();
        let registry = connection.display().get_registry(&qh, ());
        let mut test = Self {
            display,
            server,
            connection,
            queue,
            client: ClientState::default(),
        };
        test.sync();
        let globals = &test.client.globals;
        let compositor = registry.bind::<wl_compositor::WlCompositor, _, _>(
            globals["wl_compositor"],
            1,
            &qh,
            (),
        );
        let shell =
            registry.bind::<xdg_wm_base::XdgWmBase, _, _>(globals["xdg_wm_base"], 1, &qh, ());
        let shm = registry.bind::<wl_shm::WlShm, _, _>(globals["wl_shm"], 1, &qh, ());
        let surface = compositor.create_surface(&qh, ());
        let xdg = shell.get_xdg_surface(&surface, &qh, ());
        let _toplevel = xdg.get_toplevel(&qh, ());
        surface.commit();
        test.sync();

        let file = File::from(memfd_create(c"presentation-test", MemfdFlags::CLOEXEC).unwrap());
        file.set_len(100 * 100 * 4).unwrap();
        let pool = shm.create_pool(file.as_fd(), 100 * 100 * 4, &qh, ());
        let buffer = pool.create_buffer(0, 100, 100, 400, wl_shm::Format::Argb8888, &qh, ());
        surface.attach(Some(&buffer), 0, 0);
        surface.commit();
        test.sync();
        assert_eq!(test.window().bbox().size, (100, 100).into());
        test
    }

    pub(crate) fn window(&self) -> Window {
        self.server.window.as_ref().unwrap().clone()
    }

    fn sync(&mut self) {
        self.connection.display().sync(&self.queue.handle(), ());
        self.connection.flush().unwrap();
        self.display.dispatch_clients(&mut self.server).unwrap();
        self.display.flush_clients().unwrap();
        self.queue.prepare_read().unwrap().read().unwrap();
        self.queue.dispatch_pending(&mut self.client).unwrap();
    }
}

struct Server {
    compositor: CompositorState,
    shell: XdgShellState,
    shm: ShmState,
    window: Option<Window>,
}

#[derive(Default)]
struct ServerClient(CompositorClientState);
impl ClientData for ServerClient {}

impl CompositorHandler for Server {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor
    }
    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        &client.get_data::<ServerClient>().unwrap().0
    }
    fn commit(&mut self, surface: &server::wl_surface::WlSurface) {
        on_commit_buffer_handler::<Self>(surface);
        if let Some(window) = &self.window {
            window.on_commit();
        }
    }
}
impl BufferHandler for Server {
    fn buffer_destroyed(&mut self, _: &server::wl_buffer::WlBuffer) {}
}
impl ShmHandler for Server {
    fn shm_state(&self) -> &ShmState {
        &self.shm
    }
}
impl XdgShellHandler for Server {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.shell
    }
    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        surface.send_configure();
        self.window = Some(Window::new_wayland_window(surface));
    }
    fn new_popup(&mut self, _: PopupSurface, _: PositionerState) {}
    fn grab(&mut self, _: PopupSurface, _: server::wl_seat::WlSeat, _: Serial) {}
    fn reposition_request(&mut self, _: PopupSurface, _: PositionerState, _: u32) {}
}
smithay::delegate_dispatch2!(Server);

#[derive(Default)]
struct ClientState {
    globals: HashMap<String, u32>,
}
impl Dispatch<wl_registry::WlRegistry, ()> for ClientState {
    fn event(
        state: &mut Self,
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
            state.globals.insert(interface, name);
        }
    }
}
impl Dispatch<xdg_surface::XdgSurface, ()> for ClientState {
    fn event(
        _: &mut Self,
        surface: &xdg_surface::XdgSurface,
        event: xdg_surface::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            surface.ack_configure(serial);
        }
    }
}
wayland_client::delegate_noop!(ClientState: ignore wl_compositor::WlCompositor);
wayland_client::delegate_noop!(ClientState: ignore wl_surface::WlSurface);
wayland_client::delegate_noop!(ClientState: ignore wl_callback::WlCallback);
wayland_client::delegate_noop!(ClientState: ignore wl_shm::WlShm);
wayland_client::delegate_noop!(ClientState: ignore wl_shm_pool::WlShmPool);
wayland_client::delegate_noop!(ClientState: ignore wl_buffer::WlBuffer);
wayland_client::delegate_noop!(ClientState: ignore xdg_wm_base::XdgWmBase);
wayland_client::delegate_noop!(ClientState: ignore xdg_toplevel::XdgToplevel);
