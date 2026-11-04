use std::{collections::HashMap, os::unix::net::UnixStream, sync::Arc};

use smithay::wayland::compositor::{CompositorClientState, CompositorHandler, CompositorState};
use wayland_client::{
    Connection, Dispatch, EventQueue, QueueHandle,
    protocol::{
        wl_callback, wl_compositor, wl_registry, wl_subcompositor, wl_subsurface, wl_surface,
    },
};
use wayland_protocols::wp::tearing_control::v1::client::{
    wp_tearing_control_manager_v1::WpTearingControlManagerV1,
    wp_tearing_control_v1::{PresentationHint, WpTearingControlV1},
};
use wayland_server::{Client, Display, backend::ClientData, protocol as server};

use super::{TearingControlManagerState, surface_prefers_tearing};

struct TestClient {
    display: Display<Server>,
    server: Server,
    connection: Connection,
    queue: EventQueue<ClientState>,
    client: ClientState,
    compositor: wl_compositor::WlCompositor,
    subcompositor: wl_subcompositor::WlSubcompositor,
    manager: WpTearingControlManagerV1,
}

impl TestClient {
    fn new() -> Self {
        let mut display = Display::new().unwrap();
        let mut handle = display.handle();
        let mut server = Server {
            compositor: CompositorState::new::<Server>(&handle),
            surfaces: Vec::new(),
        };
        TearingControlManagerState::new::<Server>(&handle);
        let (server_socket, client_socket) = UnixStream::pair().unwrap();
        client_socket
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        handle
            .insert_client(server_socket, Arc::new(ServerClient::default()))
            .unwrap();
        let connection = Connection::from_socket(client_socket).unwrap();
        let mut queue = connection.new_event_queue();
        let qh = queue.handle();
        let registry = connection.display().get_registry(&qh, ());
        let mut client = ClientState::default();
        connection.display().sync(&qh, ());
        connection.flush().unwrap();
        display.dispatch_clients(&mut server).unwrap();
        display.flush_clients().unwrap();
        queue.prepare_read().unwrap().read().unwrap();
        queue.dispatch_pending(&mut client).unwrap();
        let compositor = registry.bind(client.globals["wl_compositor"], 1, &qh, ());
        let subcompositor = registry.bind(client.globals["wl_subcompositor"], 1, &qh, ());
        let manager = registry.bind(client.globals["wp_tearing_control_manager_v1"], 1, &qh, ());
        Self {
            display,
            server,
            connection,
            queue,
            client,
            compositor,
            subcompositor,
            manager,
        }
    }

    fn surface(&self) -> wl_surface::WlSurface {
        self.compositor.create_surface(&self.queue.handle(), ())
    }

    fn control(&self, surface: &wl_surface::WlSurface) -> WpTearingControlV1 {
        self.manager
            .get_tearing_control(surface, &self.queue.handle(), ())
    }

    fn sync(&mut self) {
        self.connection.display().sync(&self.queue.handle(), ());
        self.connection.flush().unwrap();
        self.display.dispatch_clients(&mut self.server).unwrap();
        self.display.flush_clients().unwrap();
        self.queue.prepare_read().unwrap().read().unwrap();
        self.queue.dispatch_pending(&mut self.client).unwrap();
    }

    fn prefers_tearing(&self, index: usize) -> bool {
        surface_prefers_tearing(&self.server.surfaces[index])
    }
}

struct Server {
    compositor: CompositorState,
    surfaces: Vec<server::wl_surface::WlSurface>,
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
    fn new_surface(&mut self, surface: &server::wl_surface::WlSurface) {
        self.surfaces.push(surface.clone());
    }
    fn commit(&mut self, _: &server::wl_surface::WlSurface) {}
}
smithay::delegate_dispatch2!(Server);
crate::delegate_tearing_control!(Server);

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
wayland_client::delegate_noop!(ClientState: ignore wl_compositor::WlCompositor);
wayland_client::delegate_noop!(ClientState: ignore wl_subcompositor::WlSubcompositor);
wayland_client::delegate_noop!(ClientState: ignore wl_subsurface::WlSubsurface);
wayland_client::delegate_noop!(ClientState: ignore wl_surface::WlSurface);
wayland_client::delegate_noop!(ClientState: ignore wl_callback::WlCallback);
wayland_client::delegate_noop!(ClientState: ignore WpTearingControlManagerV1);
wayland_client::delegate_noop!(ClientState: ignore WpTearingControlV1);

#[test]
fn presentation_hints_wait_for_surface_commit() {
    let mut test = TestClient::new();
    let surface = test.surface();
    let control = test.control(&surface);
    test.sync();
    assert!(!test.prefers_tearing(0));
    control.set_presentation_hint(PresentationHint::Async);
    test.sync();
    assert!(
        !test.prefers_tearing(0),
        "uncommitted async hint became current"
    );
    surface.commit();
    test.sync();
    assert!(test.prefers_tearing(0));
    control.set_presentation_hint(PresentationHint::Vsync);
    test.sync();
    assert!(
        test.prefers_tearing(0),
        "uncommitted vsync hint became current"
    );
    surface.commit();
    test.sync();
    assert!(!test.prefers_tearing(0));
    control.set_presentation_hint(PresentationHint::Async);
    control.set_presentation_hint(PresentationHint::Vsync);
    surface.commit();
    test.sync();
    assert!(!test.prefers_tearing(0));
    control.set_presentation_hint(PresentationHint::Vsync);
    control.set_presentation_hint(PresentationHint::Async);
    surface.commit();
    test.sync();
    assert!(test.prefers_tearing(0));
    surface.commit();
    test.sync();
    assert!(
        test.prefers_tearing(0),
        "a commit without a hint must preserve it"
    );
}

#[test]
fn destroying_control_resets_hint_on_next_commit() {
    let mut test = TestClient::new();
    let surface = test.surface();
    let control = test.control(&surface);
    control.set_presentation_hint(PresentationHint::Async);
    surface.commit();
    test.sync();
    assert!(test.prefers_tearing(0));
    control.destroy();
    test.sync();
    assert!(
        test.prefers_tearing(0),
        "destroy reset the hint before commit"
    );
    surface.commit();
    test.sync();
    assert!(!test.prefers_tearing(0));
    let replacement = test.control(&surface);
    replacement.set_presentation_hint(PresentationHint::Async);
    surface.commit();
    test.sync();
    assert!(test.prefers_tearing(0));
}

#[test]
fn recreated_control_replaces_pending_reset_before_commit() {
    let mut test = TestClient::new();
    let surface = test.surface();
    let control = test.control(&surface);
    control.set_presentation_hint(PresentationHint::Async);
    surface.commit();
    test.sync();
    assert!(test.prefers_tearing(0));
    control.destroy();
    let replacement = test.control(&surface);
    replacement.set_presentation_hint(PresentationHint::Async);
    test.sync();
    assert!(test.prefers_tearing(0));
    surface.commit();
    test.sync();
    assert!(
        test.prefers_tearing(0),
        "replacement hint was lost to pending reset"
    );
    replacement.set_presentation_hint(PresentationHint::Vsync);
    test.sync();
    assert!(test.prefers_tearing(0));
    surface.commit();
    test.sync();
    assert!(!test.prefers_tearing(0));
}

#[test]
fn synchronized_subsurface_hint_waits_for_parent_commit() {
    let mut test = TestClient::new();
    let parent = test.surface();
    let child = test.surface();
    let _subsurface = test
        .subcompositor
        .get_subsurface(&child, &parent, &test.queue.handle(), ());
    let control = test.control(&child);
    control.set_presentation_hint(PresentationHint::Async);
    child.commit();
    test.sync();
    assert!(
        !test.prefers_tearing(1),
        "synchronized state applied before parent commit"
    );
    parent.commit();
    test.sync();
    assert!(test.prefers_tearing(1));
    control.destroy();
    child.commit();
    test.sync();
    assert!(
        test.prefers_tearing(1),
        "synchronized reset applied before parent commit"
    );
    parent.commit();
    test.sync();
    assert!(!test.prefers_tearing(1));
}
