use std::{collections::HashMap, ffi::OsString, sync::Arc};

use smithay::{
    backend::drm::DrmNode,
    backend::renderer::element::memory::MemoryRenderBuffer,
    desktop::{PopupManager, Space, Window, WindowSurfaceType, layer_map_for_output},
    input::{Seat, SeatState, pointer::CursorImageStatus},
    output::Output,
    reexports::{
        wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode as DecorationMode,
        calloop::{EventLoop, Interest, LoopSignal, Mode, PostAction, generic::Generic},
        wayland_server::{
            Display, DisplayHandle,
            backend::{ClientData, ClientId, DisconnectReason},
            protocol::wl_surface::WlSurface,
        },
    },
    utils::{Clock, Logical, Monotonic, Physical, Point, Rectangle, Scale},
    wayland::{
        commit_timing::CommitTimingManagerState,
        compositor::{CompositorClientState, CompositorState},
        dmabuf::{DmabufGlobal, DmabufState},
        cursor_shape::CursorShapeManagerState,
        fixes::FixesState,
        fifo::FifoManagerState,
        fractional_scale::FractionalScaleManagerState,
        output::OutputManagerState,
        presentation::PresentationState,
        selection::{
            data_device::DataDeviceState,
            primary_selection::PrimarySelectionState,
            wlr_data_control::DataControlState,
        },
        shell::xdg::{XdgShellState, decoration::XdgDecorationState},
        shell::wlr_layer::Layer as WlrLayer,
        shell::wlr_layer::WlrLayerShellState,
        shm::ShmState,
        single_pixel_buffer::SinglePixelBufferState,
        socket::ListeningSocketSource,
        viewporter::ViewporterState,
    },
};
use xcursor::parser::Image;

use crate::{
    backend::{text::TextRasterizer, tty::BackendData},
    config::DisplayConfig,
    cursor::Cursor,
    drawing::PointerElement,
};
use crate::ssd::{DecorationRuntimeEvaluator, LogicalRect, NodeDecorationEvaluator, WindowDecorationState};
use tracing::{debug, info};

pub struct ShojiWM {
    pub start_time: std::time::Instant,
    pub socket_name: OsString,
    pub display_handle: DisplayHandle,

    pub space: Space<Window>,
    pub loop_signal: LoopSignal,

    // Smithay State
    pub compositor_state: CompositorState,
    pub xdg_shell_state: XdgShellState,
    pub layer_shell_state: WlrLayerShellState,
    pub xdg_decoration_state: XdgDecorationState,
    pub shm_state: ShmState,
    pub cursor_shape_manager_state: CursorShapeManagerState,
    pub output_manager_state: OutputManagerState,
    pub presentation_state: PresentationState,
    pub fifo_manager_state: FifoManagerState,
    pub commit_timing_manager_state: CommitTimingManagerState,
    pub viewporter_state: ViewporterState,
    pub fractional_scale_manager_state: FractionalScaleManagerState,
    pub single_pixel_buffer_state: SinglePixelBufferState,
    pub fixes_state: FixesState,
    pub seat_state: SeatState<ShojiWM>,
    pub data_device_state: DataDeviceState,
    pub primary_selection_state: PrimarySelectionState,
    pub data_control_state: DataControlState,
    pub popups: PopupManager,

    pub seat: Seat<Self>,

    pub tty_backends: HashMap<DrmNode, BackendData>,
    pub window_decorations: HashMap<Window, WindowDecorationState>,
    pub window_commit_times: HashMap<Window, std::time::Duration>,
    pub pending_decoration_damage: Vec<LogicalRect>,
    pub decoration_evaluator: DecorationRuntimeEvaluator,
    pub dmabuf_state: DmabufState,
    pub dmabuf_global: Option<DmabufGlobal>,
    pub damage_blink_enabled: bool,
    pub damage_blink_visible: HashMap<String, Vec<LogicalRect>>,
    pub damage_blink_pending: HashMap<String, Vec<LogicalRect>>,

    pub is_running: bool,
    pub needs_redraw: bool,
    pub cursor_status: CursorImageStatus,
    pub cursor_theme: Cursor,
    pub pointer_images: Vec<(Image, MemoryRenderBuffer)>,
    pub current_pointer_image: Option<Image>,
    pub pointer_element: PointerElement,
    pub text_rasterizer: TextRasterizer,
    pub default_decoration_mode: DecorationMode,
    pub display_config: DisplayConfig,
    pub clock: Clock<Monotonic>,
}

impl ShojiWM {
    pub fn new(event_loop: &mut EventLoop<Self>, display: Display<Self>) -> Self {
        let start_time = std::time::Instant::now();

        let dh = display.handle();

        // Here we initialize implementations of some wayland protocols
        // Some of them require us to implement traits on the Smallvil state,
        // you can find those implementations in the `crate::handlers` module

        // Initialize protocols needed for displaying windows
        let compositor_state = CompositorState::new::<Self>(&dh);
        let xdg_shell_state = XdgShellState::new::<Self>(&dh);
        let layer_shell_state = WlrLayerShellState::new::<Self>(&dh);
        let xdg_decoration_state = XdgDecorationState::new::<Self>(&dh);
        let shm_state = ShmState::new::<Self>(&dh, vec![]);
        let popups = PopupManager::default();
        let cursor_shape_manager_state = CursorShapeManagerState::new::<Self>(&dh);
        let clock = Clock::<Monotonic>::new();
        let presentation_state = PresentationState::new::<Self>(&dh, clock.id() as u32);

        let output_manager_state = OutputManagerState::new_with_xdg_output::<Self>(&dh);
        let fifo_manager_state = FifoManagerState::new::<Self>(&dh);
        let commit_timing_manager_state = CommitTimingManagerState::new::<Self>(&dh);
        let viewporter_state = ViewporterState::new::<Self>(&dh);
        let fractional_scale_manager_state = FractionalScaleManagerState::new::<Self>(&dh);
        let single_pixel_buffer_state = SinglePixelBufferState::new::<Self>(&dh);
        let fixes_state = FixesState::new::<Self>(&dh);

        // Data device is responsible for clipboard and drag-and-drop
        let data_device_state = DataDeviceState::new::<Self>(&dh);
        let primary_selection_state = PrimarySelectionState::new::<Self>(&dh);
        let data_control_state =
            DataControlState::new::<Self, _>(&dh, Some(&primary_selection_state), |_| true);

        // A seat is a group of keyboards, pointer and touch devices.
        // A seat typically has a pointer and maintains a keyboard focus and a pointer focus.
        let mut seat_state = SeatState::new();
        let mut seat: Seat<Self> = seat_state.new_wl_seat(&dh, "winit");

        // Notify clients that we have a keyboard, for the sake of the example we assume that keyboard is always present.
        // You may want to track keyboard hot-plug in real compositor.
        seat.add_keyboard(Default::default(), 200, 25).unwrap();

        // Notify clients that we have a pointer (mouse)
        // Here we assume that there is always pointer plugged in
        seat.add_pointer();

        // A space represents a two-dimensional plane. Windows and Outputs can be mapped onto it.
        //
        // Windows get a position and stacking order through mapping.
        // Outputs become views of a part of the Space and can be rendered via Space::render_output.
        let space = Space::default();

        // Setup a wayland socket that will be used to accept clients
        let socket_name = Self::init_wayland_listener(display, event_loop);

        // Get the loop signal, used to stop the event loop
        let loop_signal = event_loop.get_signal();
        let decoration_evaluator = if std::path::Path::new("node_modules/.bin/tsx").exists() {
            DecorationRuntimeEvaluator::Node(
                NodeDecorationEvaluator::for_workspace("packages/config/src/index.tsx")
                    .with_working_dir(std::env::current_dir().unwrap_or_else(|_| ".".into())),
            )
        } else {
            DecorationRuntimeEvaluator::Static(Default::default())
        };

        let damage_blink_enabled = std::env::args().any(|arg| arg == "--damage-blink")
            || std::env::var_os("SHOJI_DAMAGE_BLINK")
                .is_some_and(|value| value != "0" && !value.is_empty());

        Self {
            start_time,
            display_handle: dh,

            space,
            loop_signal,
            socket_name,

            compositor_state,
            xdg_shell_state,
            layer_shell_state,
            xdg_decoration_state,
            shm_state,
            cursor_shape_manager_state,
            output_manager_state,
            presentation_state,
            fifo_manager_state,
            commit_timing_manager_state,
            viewporter_state,
            fractional_scale_manager_state,
            single_pixel_buffer_state,
            fixes_state,
            seat_state,
            data_device_state,
            primary_selection_state,
            data_control_state,
            popups,
            seat,

            tty_backends: HashMap::new(),
            window_decorations: HashMap::new(),
            window_commit_times: HashMap::new(),
            pending_decoration_damage: Vec::new(),
            decoration_evaluator,
            dmabuf_state: DmabufState::new(),
            dmabuf_global: None,
            damage_blink_enabled,
            damage_blink_visible: HashMap::new(),
            damage_blink_pending: HashMap::new(),

            is_running: true,
            needs_redraw: true,
            cursor_status: CursorImageStatus::default_named(),
            cursor_theme: Cursor::load(),
            pointer_images: Vec::new(),
            current_pointer_image: None,
            pointer_element: PointerElement::default(),
            text_rasterizer: TextRasterizer::new(),
            // SSD rendering is available, so prefer compositor-side decorations by default.
            default_decoration_mode: DecorationMode::ServerSide,
            display_config: DisplayConfig::default(),
            clock,
        }
    }

    fn init_wayland_listener(
        display: Display<ShojiWM>,
        event_loop: &mut EventLoop<Self>,
    ) -> OsString {
        // Creates a new listening socket, automatically choosing the next available `wayland` socket name.
        let listening_socket = ListeningSocketSource::new_auto().unwrap();

        // Get the name of the listening socket.
        // Clients will connect to this socket.
        let socket_name = listening_socket.socket_name().to_os_string();

        let loop_handle = event_loop.handle();

        loop_handle
            .insert_source(listening_socket, move |client_stream, _, state| {
                info!("accepted new wayland client connection");
                // Inside the callback, you should insert the client into the display.
                //
                // You may also associate some data with the client when inserting the client.
                state
                    .display_handle
                    .insert_client(client_stream, Arc::new(ClientState::default()))
                    .unwrap();
            })
            .expect("Failed to init the wayland event source.");

        // You also need to add the display itself to the event loop, so that client events will be processed by wayland-server.
        loop_handle
            .insert_source(
                Generic::new(display, Interest::READ, Mode::Level),
                |_, display, state| {
                    // Safety: we don't drop the display
                    unsafe {
                        display.get_mut().dispatch_clients(state).unwrap();
                    }
                    Ok(PostAction::Continue)
                },
            )
            .unwrap();

        socket_name
    }

    pub fn surface_under(
        &self,
        pos: Point<f64, Logical>,
    ) -> Option<(WlSurface, Point<f64, Logical>)> {
        let output = self.space.outputs().find(|output| {
            self.space
                .output_geometry(output)
                .is_some_and(|geometry| geometry.contains(pos.to_i32_round()))
        })?;
        let output_geo = self.space.output_geometry(output).unwrap();
        let layers = layer_map_for_output(output);

        if let Some(focus) = [WlrLayer::Overlay, WlrLayer::Top]
            .into_iter()
            .flat_map(|target_layer| layers.layers_on(target_layer).rev())
            .find_map(|layer| {
                let layer_geo = layers.layer_geometry(layer).unwrap();
                let layer_loc = layer_geo.loc - layer.geometry().loc;
                let result = layer
                    .surface_under(
                        pos - output_geo.loc.to_f64() - layer_loc.to_f64(),
                        WindowSurfaceType::ALL,
                    )
                    .map(|(surface, loc)| (surface, (loc + layer_loc + output_geo.loc).to_f64()));
                debug!(
                    pos = ?pos,
                    output = %output.name(),
                    layer = ?layer.layer(),
                    layer_geo = ?layer_geo,
                    layer_surface_geo = ?layer.geometry(),
                    layer_origin = ?layer_loc,
                    hit = result.is_some(),
                    "layer-shell top/overlay hit-test"
                );
                result
            })
        {
            return Some(focus);
        }

        if let Some(focus) = self.space.element_under(pos).and_then(|(window, location)| {
            window
                .surface_under(pos - location.to_f64(), WindowSurfaceType::ALL)
                .map(|(surface, loc)| (surface, (loc + location).to_f64()))
        }) {
            return Some(focus);
        }

        [WlrLayer::Bottom, WlrLayer::Background]
            .into_iter()
            .flat_map(|target_layer| layers.layers_on(target_layer).rev())
            .find_map(|layer| {
                let layer_geo = layers.layer_geometry(layer).unwrap();
                let layer_loc = layer_geo.loc - layer.geometry().loc;
                let result = layer
                    .surface_under(
                        pos - output_geo.loc.to_f64() - layer_loc.to_f64(),
                        WindowSurfaceType::ALL,
                    )
                    .map(|(surface, loc)| (surface, (loc + layer_loc + output_geo.loc).to_f64()));
                debug!(
                    pos = ?pos,
                    output = %output.name(),
                    layer = ?layer.layer(),
                    layer_geo = ?layer_geo,
                    layer_surface_geo = ?layer.geometry(),
                    layer_origin = ?layer_loc,
                    hit = result.is_some(),
                    "layer-shell bottom/background hit-test"
                );
                result
            })
    }

    pub fn shutdown(&mut self) {
        info!("shutdown requested");
        self.is_running = false;
        self.loop_signal.stop();
    }

    pub fn schedule_redraw(&mut self) {
        self.needs_redraw = true;
    }

    pub fn damage_blink_rects_for_output(&self, output: &Output) -> &[LogicalRect] {
        self.damage_blink_visible
            .get(&output.name())
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub fn record_damage_blink(
        &mut self,
        output: &Output,
        damage: &[Rectangle<i32, Physical>],
    ) {
        if !self.damage_blink_enabled {
            return;
        }

        let Some(output_geo) = self.space.output_geometry(output) else {
            return;
        };
        let scale = Scale::from(output.current_scale().fractional_scale());
        let rects = damage
            .iter()
            .filter(|rect| rect.size.w > 0 && rect.size.h > 0)
            .map(|rect| {
                let logical = rect.to_f64().to_logical(scale).to_i32_round();
                LogicalRect::new(
                    output_geo.loc.x + logical.loc.x,
                    output_geo.loc.y + logical.loc.y,
                    logical.size.w,
                    logical.size.h,
                )
            })
            .collect::<Vec<_>>();

        self.damage_blink_pending
            .insert(output.name().to_string(), rects);
    }

    pub fn finish_damage_blink_frame(&mut self) {
        if !self.damage_blink_enabled {
            self.damage_blink_visible.clear();
            self.damage_blink_pending.clear();
            return;
        }

        let previous_visible = self
            .damage_blink_visible
            .values()
            .flat_map(|rects| rects.iter().copied())
            .collect::<Vec<_>>();
        let had_visible = self.damage_blink_visible.values().any(|rects| !rects.is_empty());
        self.damage_blink_visible = std::mem::take(&mut self.damage_blink_pending);
        let next_visible = self
            .damage_blink_visible
            .values()
            .flat_map(|rects| rects.iter().copied())
            .collect::<Vec<_>>();
        let has_visible = self.damage_blink_visible.values().any(|rects| !rects.is_empty());

        self.pending_decoration_damage.extend(previous_visible);
        self.pending_decoration_damage.extend(next_visible);

        if had_visible || has_visible {
            self.schedule_redraw();
        }
    }
}

/// One instance of this type per client.
#[derive(Default)]
pub struct ClientState {
    pub compositor_state: CompositorClientState,
}

impl ClientData for ClientState {
    fn initialized(&self, _client_id: ClientId) {}
    fn disconnected(&self, _client_id: ClientId, _reason: DisconnectReason) {}
}
