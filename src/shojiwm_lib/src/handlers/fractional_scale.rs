//! Which output's scale a surface that asks for `wp_fractional_scale_v1`
//! starts with.

use smithay::desktop::utils::surface_primary_scanout_output;
use smithay::output::Output;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::wayland::compositor::with_states;

/// The output `surface` is shown on: its own or its root's primary scanout
/// output once it has been drawn, else where the window or layer surface
/// owning its root (or its popup's root) sits, else `fallback`.
///
/// No surface lock is held while `output_of_root` runs: finding a layer
/// surface walks its whole surface tree, and `wl_surface` state locks are not
/// reentrant, so holding `surface`'s across it wedges the compositor.
pub(crate) fn preferred_output(
    surface: &WlSurface,
    root: &WlSurface,
    popup_root: Option<&WlSurface>,
    output_of_root: impl Fn(&WlSurface) -> Option<Output>,
    fallback: impl FnOnce() -> Option<Output>,
) -> Option<Output> {
    let scanout = |surface: &WlSurface| {
        with_states(surface, |states| {
            surface_primary_scanout_output(surface, states)
        })
    };
    scanout(surface)
        .or_else(|| (root != surface).then(|| scanout(root)).flatten())
        .or_else(|| output_of_root(root))
        .or_else(|| popup_root.and_then(&output_of_root))
        .or_else(fallback)
}

#[cfg(test)]
mod tests {
    use std::{os::unix::net::UnixStream, sync::Arc, sync::mpsc, time::Duration};

    use smithay::desktop::{LayerSurface, WindowSurfaceType, layer_map_for_output};
    use smithay::output::{Mode, Output, PhysicalProperties, Subpixel};
    use smithay::reexports::wayland_server::protocol::{
        wl_output::WlOutput, wl_surface::WlSurface,
    };
    use smithay::wayland::compositor::{CompositorClientState, CompositorHandler, CompositorState};
    use smithay::wayland::fractional_scale::{FractionalScaleHandler, FractionalScaleManagerState};
    use smithay::wayland::shell::wlr_layer::{Layer, WlrLayerShellHandler, WlrLayerShellState};
    use wayland_client::{
        Connection, Dispatch, QueueHandle,
        protocol::{wl_compositor, wl_registry, wl_surface},
    };
    use wayland_protocols::wp::fractional_scale::v1::client::{
        wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1,
        wp_fractional_scale_v1::WpFractionalScaleV1,
    };
    use wayland_protocols_wlr::layer_shell::v1::client::{
        zwlr_layer_shell_v1::{self, ZwlrLayerShellV1},
        zwlr_layer_surface_v1::{self, ZwlrLayerSurfaceV1},
    };
    use wayland_server::{Client, Display, backend::ClientData};

    struct Server {
        compositor: CompositorState,
        layer_shell: WlrLayerShellState,
        output: Output,
        chosen: Vec<Option<String>>,
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
        fn commit(&mut self, _: &WlSurface) {}
    }
    impl WlrLayerShellHandler for Server {
        fn shell_state(&mut self) -> &mut WlrLayerShellState {
            &mut self.layer_shell
        }
        fn new_layer_surface(
            &mut self,
            surface: smithay::wayland::shell::wlr_layer::LayerSurface,
            _: Option<WlOutput>,
            _: Layer,
            namespace: String,
        ) {
            layer_map_for_output(&self.output)
                .map_layer(&LayerSurface::new(surface, namespace))
                .unwrap();
        }
    }
    impl FractionalScaleHandler for Server {
        fn new_fractional_scale(&mut self, surface: WlSurface) {
            let output = self.output.clone();
            let chosen = super::preferred_output(
                &surface,
                &surface,
                None,
                |root| {
                    layer_map_for_output(&output)
                        .layer_for_surface(root, WindowSurfaceType::TOPLEVEL)
                        .map(|_| output.clone())
                },
                || None,
            );
            self.chosen.push(chosen.map(|output| output.name()));
        }
    }
    smithay::delegate_dispatch2!(Server);

    struct ClientState(std::collections::HashMap<String, u32>);
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
                state.0.insert(interface, name);
            }
        }
    }
    wayland_client::delegate_noop!(ClientState: ignore wl_compositor::WlCompositor);
    wayland_client::delegate_noop!(ClientState: ignore wl_surface::WlSurface);
    wayland_client::delegate_noop!(ClientState: ignore ZwlrLayerShellV1);
    wayland_client::delegate_noop!(ClientState: ignore ZwlrLayerSurfaceV1);
    wayland_client::delegate_noop!(ClientState: ignore WpFractionalScaleManagerV1);
    wayland_client::delegate_noop!(ClientState: ignore WpFractionalScaleV1);
    wayland_client::delegate_noop!(ClientState: ignore wayland_client::protocol::wl_callback::WlCallback);

    /// A bar asking for its fractional scale after its layer surface is
    /// mapped (and before it was ever drawn) gets the layer's output, and
    /// the compositor keeps answering.
    #[test]
    fn layer_surface_fractional_scale_after_mapping_does_not_deadlock() {
        let (done, finished) = mpsc::channel();
        std::thread::spawn(move || {
            let mut display = Display::<Server>::new().unwrap();
            let mut handle = display.handle();
            let output = Output::new(
                "TEST-1".into(),
                PhysicalProperties {
                    size: (0, 0).into(),
                    subpixel: Subpixel::Unknown,
                    make: "test".into(),
                    model: "test".into(),
                    serial_number: String::new(),
                },
            );
            let mode = Mode {
                size: (1024, 768).into(),
                refresh: 60_000,
            };
            output.change_current_state(Some(mode), None, None, Some((0, 0).into()));
            output.set_preferred(mode);
            let mut server = Server {
                compositor: CompositorState::new::<Server>(&handle),
                layer_shell: WlrLayerShellState::new::<Server>(&handle),
                output,
                chosen: Vec::new(),
            };
            FractionalScaleManagerState::new::<Server>(&handle);

            let (server_socket, client_socket) = UnixStream::pair().unwrap();
            handle
                .insert_client(server_socket, Arc::new(ServerClient::default()))
                .unwrap();
            let connection = Connection::from_socket(client_socket).unwrap();
            let mut queue = connection.new_event_queue();
            let qh = queue.handle();
            let registry = connection.display().get_registry(&qh, ());
            let mut client = ClientState(Default::default());
            let mut roundtrip = |server: &mut Server, client: &mut ClientState| {
                connection.display().sync(&qh, ());
                connection.flush().unwrap();
                display.dispatch_clients(server).unwrap();
                display.flush_clients().unwrap();
                queue.prepare_read().unwrap().read().unwrap();
                queue.dispatch_pending(client).unwrap();
            };
            roundtrip(&mut server, &mut client);

            let compositor: wl_compositor::WlCompositor =
                registry.bind(client.0["wl_compositor"], 4, &qh, ());
            let layer_shell: ZwlrLayerShellV1 =
                registry.bind(client.0["zwlr_layer_shell_v1"], 4, &qh, ());
            let fractional: WpFractionalScaleManagerV1 =
                registry.bind(client.0["wp_fractional_scale_manager_v1"], 1, &qh, ());
            let surface = compositor.create_surface(&qh, ());
            let layer = layer_shell.get_layer_surface(
                &surface,
                None,
                zwlr_layer_shell_v1::Layer::Top,
                "bar".into(),
                &qh,
                (),
            );
            layer.set_anchor(
                zwlr_layer_surface_v1::Anchor::Top
                    | zwlr_layer_surface_v1::Anchor::Left
                    | zwlr_layer_surface_v1::Anchor::Right,
            );
            layer.set_size(0, 40);
            surface.commit();
            roundtrip(&mut server, &mut client);

            fractional.get_fractional_scale(&surface, &qh, ());
            roundtrip(&mut server, &mut client);
            done.send(server.chosen).unwrap();
        });
        let chosen = finished
            .recv_timeout(Duration::from_secs(5))
            .expect("the server wedged on get_fractional_scale");
        assert_eq!(chosen, [Some("TEST-1".to_string())]);
    }
}
