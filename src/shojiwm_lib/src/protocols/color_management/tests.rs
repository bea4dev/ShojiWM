use std::{collections::HashMap, os::unix::net::UnixStream, sync::Arc};

use smithay::wayland::compositor::{
    CompositorClientState, CompositorHandler, CompositorState, TraversalAction,
    with_surface_tree_downward,
};
use wayland_client::{
    Connection, Dispatch, EventQueue, QueueHandle,
    protocol::{
        wl_callback, wl_compositor, wl_registry, wl_subcompositor, wl_subsurface, wl_surface,
    },
};
use wayland_protocols::wp::color_management::v1::client::{
    wp_color_management_surface_feedback_v1::WpColorManagementSurfaceFeedbackV1,
    wp_color_management_surface_v1::WpColorManagementSurfaceV1,
    wp_color_manager_v1::{Primaries, RenderIntent, TransferFunction, WpColorManagerV1},
    wp_image_description_creator_params_v1::WpImageDescriptionCreatorParamsV1,
    wp_image_description_v1::{self, WpImageDescriptionV1},
};
use wayland_server::{Client, Display, backend::ClientData, protocol as server};

use super::{ColorManagementHandler, ColorManagementState, surface_image_description};
use crate::color::ImageDescription;

const PREFERRED_DESCRIPTION: ImageDescription = ImageDescription {
    luminances: Some(crate::color::Luminances {
        min: 0.0,
        max: 203.0,
        reference: 100.0,
    }),
    ..ImageDescription::SRGB
};

struct TestClient {
    display: Display<Server>,
    server: Server,
    connection: Connection,
    queue: EventQueue<ClientState>,
    client: ClientState,
    compositor: wl_compositor::WlCompositor,
    subcompositor: wl_subcompositor::WlSubcompositor,
    manager: WpColorManagerV1,
}

impl TestClient {
    fn new() -> Self {
        let mut display = Display::new().unwrap();
        let mut handle = display.handle();
        let mut server = Server {
            compositor: CompositorState::new::<Server>(&handle),
            color: ColorManagementState::new::<Server>(&handle),
            surfaces: Vec::new(),
        };
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
        let manager = registry.bind(client.globals["wp_color_manager_v1"], 1, &qh, ());
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

    fn control(&self, surface: &wl_surface::WlSurface) -> WpColorManagementSurfaceV1 {
        self.manager.get_surface(surface, &self.queue.handle(), ())
    }

    fn description(&mut self) -> WpImageDescriptionV1 {
        let creator = self
            .manager
            .create_parametric_creator(&self.queue.handle(), ());
        creator.set_primaries_named(Primaries::Srgb);
        creator.set_tf_named(TransferFunction::Srgb);
        let previous = self.client.ready;
        let description = creator.create(&self.queue.handle(), ());
        self.sync();
        assert_eq!(
            self.client.ready,
            previous + 1,
            "description must be ready before use"
        );
        description
    }

    fn preferred_description(&mut self, surface: &wl_surface::WlSurface) -> WpImageDescriptionV1 {
        let feedback = self
            .manager
            .get_surface_feedback(surface, &self.queue.handle(), ());
        let previous = self.client.ready;
        let description = feedback.get_preferred(&self.queue.handle(), ());
        self.sync();
        assert_eq!(self.client.ready, previous + 1);
        feedback.destroy();
        description
    }

    fn sync(&mut self) {
        self.connection.display().sync(&self.queue.handle(), ());
        self.connection.flush().unwrap();
        self.display.dispatch_clients(&mut self.server).unwrap();
        self.display.flush_clients().unwrap();
        self.queue.prepare_read().unwrap().read().unwrap();
        self.queue.dispatch_pending(&mut self.client).unwrap();
    }

    fn current(&self, index: usize) -> Option<ImageDescription> {
        let surface = &self.server.surfaces[index];
        let description = surface_image_description(surface);
        with_surface_tree_downward(
            surface,
            (),
            |_, _, _| TraversalAction::DoChildren(()),
            |visited, states, _| {
                if visited == surface {
                    assert_eq!(super::image_description_from_states(states), description);
                }
            },
            |_, _, _| true,
        );
        description
    }
}

struct Server {
    compositor: CompositorState,
    color: ColorManagementState,
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

impl ColorManagementHandler for Server {
    fn color_management_state(&mut self) -> &mut ColorManagementState {
        &mut self.color
    }
    fn output_image_description(&mut self, _: &server::wl_output::WlOutput) -> ImageDescription {
        ImageDescription::SRGB
    }
    fn surface_preferred_description(
        &mut self,
        _: &server::wl_surface::WlSurface,
    ) -> ImageDescription {
        PREFERRED_DESCRIPTION
    }
    fn defer_image_description_info(
        &mut self,
        _: super::WpImageDescriptionInfoV1,
        _: ImageDescription,
    ) {
        unreachable!("these tests do not request description information");
    }
}
smithay::delegate_dispatch2!(Server);
crate::delegate_color_management!(Server);

#[derive(Default)]
struct ClientState {
    globals: HashMap<String, u32>,
    ready: usize,
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
impl Dispatch<WpImageDescriptionV1, ()> for ClientState {
    fn event(
        state: &mut Self,
        _: &WpImageDescriptionV1,
        event: wp_image_description_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wp_image_description_v1::Event::Ready { .. } => state.ready += 1,
            wp_image_description_v1::Event::Failed { cause, msg } => {
                panic!("description failed: {cause:?}: {msg}")
            }
            _ => {}
        }
    }
}
wayland_client::delegate_noop!(ClientState: ignore wl_compositor::WlCompositor);
wayland_client::delegate_noop!(ClientState: ignore wl_subcompositor::WlSubcompositor);
wayland_client::delegate_noop!(ClientState: ignore wl_subsurface::WlSubsurface);
wayland_client::delegate_noop!(ClientState: ignore wl_surface::WlSurface);
wayland_client::delegate_noop!(ClientState: ignore wl_callback::WlCallback);
wayland_client::delegate_noop!(ClientState: ignore WpColorManagerV1);
wayland_client::delegate_noop!(ClientState: ignore WpColorManagementSurfaceV1);
wayland_client::delegate_noop!(ClientState: ignore WpColorManagementSurfaceFeedbackV1);
wayland_client::delegate_noop!(ClientState: ignore WpImageDescriptionCreatorParamsV1);

#[test]
fn description_is_visible_after_commit() {
    let mut test = TestClient::new();
    let surface = test.surface();
    test.sync();
    assert_eq!(test.current(0), None);
    let control = test.control(&surface);
    let description = test.description();
    control.set_image_description(&description, RenderIntent::Perceptual);
    surface.commit();
    test.sync();
    assert_eq!(test.current(0), Some(ImageDescription::SRGB));
}

#[test]
fn description_waits_for_surface_commit() {
    let mut test = TestClient::new();
    let surface = test.surface();
    let control = test.control(&surface);
    let description = test.description();
    control.set_image_description(&description, RenderIntent::Perceptual);
    test.sync();
    assert_eq!(
        test.current(0),
        None,
        "uncommitted description became current"
    );
    assert!(super::any_surface_tagged());
    surface.commit();
    test.sync();
    assert_eq!(test.current(0), Some(ImageDescription::SRGB));
    surface.commit();
    test.sync();
    assert_eq!(test.current(0), Some(ImageDescription::SRGB));
}

#[test]
fn unset_waits_for_surface_commit() {
    let mut test = TestClient::new();
    let surface = test.surface();
    let control = test.control(&surface);
    let description = test.description();
    control.set_image_description(&description, RenderIntent::Perceptual);
    surface.commit();
    test.sync();
    assert_eq!(test.current(0), Some(ImageDescription::SRGB));
    control.unset_image_description();
    test.sync();
    assert_eq!(
        test.current(0),
        Some(ImageDescription::SRGB),
        "uncommitted unset became current"
    );
    surface.commit();
    test.sync();
    assert_eq!(test.current(0), None);
}

#[test]
fn destroy_waits_for_surface_commit() {
    let mut test = TestClient::new();
    let surface = test.surface();
    let control = test.control(&surface);
    let description = test.description();
    control.set_image_description(&description, RenderIntent::Perceptual);
    surface.commit();
    test.sync();
    assert_eq!(test.current(0), Some(ImageDescription::SRGB));
    control.destroy();
    test.sync();
    assert_eq!(
        test.current(0),
        Some(ImageDescription::SRGB),
        "destroy reset before commit"
    );
    surface.commit();
    test.sync();
    assert_eq!(test.current(0), None);
    let replacement = test.control(&surface);
    replacement.set_image_description(&description, RenderIntent::Perceptual);
    surface.commit();
    test.sync();
    assert_eq!(test.current(0), Some(ImageDescription::SRGB));
}

#[test]
fn synchronized_subsurface_waits_for_parent_commit() {
    let mut test = TestClient::new();
    let parent = test.surface();
    let child = test.surface();
    let _subsurface = test
        .subcompositor
        .get_subsurface(&child, &parent, &test.queue.handle(), ());
    let control = test.control(&child);
    let description = test.description();
    control.set_image_description(&description, RenderIntent::Perceptual);
    child.commit();
    test.sync();
    assert_eq!(
        test.current(1),
        None,
        "synchronized description became current before parent commit"
    );
    parent.commit();
    test.sync();
    assert_eq!(test.current(1), Some(ImageDescription::SRGB));
    control.unset_image_description();
    child.commit();
    test.sync();
    assert_eq!(test.current(1), Some(ImageDescription::SRGB));
    parent.commit();
    test.sync();
    assert_eq!(test.current(1), None);

    control.set_image_description(&description, RenderIntent::Perceptual);
    child.commit();
    control.unset_image_description();
    parent.commit();
    test.sync();
    assert_eq!(
        test.current(1),
        Some(ImageDescription::SRGB),
        "pending unset leaked into cached child commit"
    );
    child.commit();
    parent.commit();
    test.sync();
    assert_eq!(test.current(1), None);

    control.set_image_description(&description, RenderIntent::Perceptual);
    child.commit();
    parent.commit();
    test.sync();
    assert_eq!(test.current(1), Some(ImageDescription::SRGB));
    control.destroy();
    child.commit();
    test.sync();
    assert_eq!(test.current(1), Some(ImageDescription::SRGB));
    parent.commit();
    test.sync();
    assert_eq!(test.current(1), None);
}

#[test]
fn surface_can_be_destroyed_before_its_color_control() {
    let mut test = TestClient::new();
    let surface = test.surface();
    let control = test.control(&surface);
    let description = test.description();
    control.set_image_description(&description, RenderIntent::Perceptual);
    surface.commit();
    test.sync();
    assert_eq!(test.current(0), Some(ImageDescription::SRGB));
    surface.destroy();
    test.sync();
    assert_eq!(surface_image_description(&test.server.surfaces[0]), None);
    control.destroy();
    test.sync();
}

#[test]
fn last_pending_state_wins_and_description_has_copy_semantics() {
    let mut test = TestClient::new();
    let surface = test.surface();
    let control = test.control(&surface);
    let description = test.description();
    control.set_image_description(&description, RenderIntent::Perceptual);
    control.unset_image_description();
    surface.commit();
    test.sync();
    assert_eq!(test.current(0), None);

    control.unset_image_description();
    control.set_image_description(&description, RenderIntent::Perceptual);
    description.destroy();
    test.sync();
    assert_eq!(test.current(0), None);
    surface.commit();
    test.sync();
    assert_eq!(test.current(0), Some(ImageDescription::SRGB));
}

#[test]
fn last_pending_description_replaces_the_previous_description() {
    let mut test = TestClient::new();
    let surface = test.surface();
    let control = test.control(&surface);
    let srgb = test.description();
    let preferred = test.preferred_description(&surface);
    control.set_image_description(&srgb, RenderIntent::Perceptual);
    control.set_image_description(&preferred, RenderIntent::Perceptual);
    test.sync();
    assert_eq!(test.current(0), None);
    surface.commit();
    test.sync();
    assert_eq!(test.current(0), Some(PREFERRED_DESCRIPTION));
    control.set_image_description(&preferred, RenderIntent::Perceptual);
    control.set_image_description(&srgb, RenderIntent::Perceptual);
    test.sync();
    assert_eq!(test.current(0), Some(PREFERRED_DESCRIPTION));
    surface.commit();
    test.sync();
    assert_eq!(test.current(0), Some(ImageDescription::SRGB));
}

#[test]
fn replacement_control_overwrites_pending_destroy_reset() {
    let mut test = TestClient::new();
    let surface = test.surface();
    let control = test.control(&surface);
    let description = test.description();
    control.set_image_description(&description, RenderIntent::Perceptual);
    surface.commit();
    test.sync();
    assert_eq!(test.current(0), Some(ImageDescription::SRGB));
    control.destroy();
    let replacement = test.control(&surface);
    replacement.set_image_description(&description, RenderIntent::Perceptual);
    test.sync();
    assert_eq!(test.current(0), Some(ImageDescription::SRGB));
    surface.commit();
    test.sync();
    assert_eq!(test.current(0), Some(ImageDescription::SRGB));
}

#[test]
fn duplicate_control_is_rejected_before_surface_commit() {
    let mut test = TestClient::new();
    let surface = test.surface();
    let _control = test.control(&surface);
    test.sync();
    let _duplicate = test.control(&surface);
    test.connection.flush().unwrap();
    test.display.dispatch_clients(&mut test.server).unwrap();
    test.display.flush_clients().unwrap();
    let result = test.queue.prepare_read().unwrap().read();
    assert!(result.is_err());
    let error = test
        .connection
        .protocol_error()
        .expect("expected duplicate-control protocol error");
    assert_eq!(error.object_interface, "wp_color_manager_v1");
    assert_eq!(error.code, wayland_protocols::wp::color_management::v1::client::wp_color_manager_v1::Error::SurfaceExists as u32);
}
