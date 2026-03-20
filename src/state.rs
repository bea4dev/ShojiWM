use std::{collections::{HashMap, HashSet}, ffi::OsString, sync::Arc, time::Duration};

use smithay::{
    backend::drm::DrmNode,
    backend::renderer::element::memory::MemoryRenderBuffer,
    desktop::{PopupManager, Space, Window, WindowSurfaceType, layer_map_for_output},
    input::{Seat, SeatState, pointer::{CursorIcon, CursorImageStatus}},
    output::Output,
    reexports::{
        wayland_protocols_misc::server_decoration::server::org_kde_kwin_server_decoration_manager::Mode as KdeDecorationMode,
        wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode as DecorationMode,
        calloop::{EventLoop, Interest, LoopSignal, Mode, PostAction, generic::Generic, timer::{TimeoutAction, Timer}},
        calloop::channel::{channel, Event as ChannelEvent},
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
        input_method::InputMethodManagerState,
        output::OutputManagerState,
        presentation::PresentationState,
        selection::{
            data_device::DataDeviceState,
            primary_selection::PrimarySelectionState,
            wlr_data_control::DataControlState,
        },
        shell::xdg::{XdgShellState, decoration::XdgDecorationState},
        shell::kde::decoration::KdeDecorationState,
        shell::wlr_layer::Layer as WlrLayer,
        shell::wlr_layer::WlrLayerShellState,
        shm::ShmState,
        single_pixel_buffer::SinglePixelBufferState,
        socket::ListeningSocketSource,
        text_input::TextInputManagerState,
        viewporter::ViewporterState,
        virtual_keyboard::VirtualKeyboardManagerState,
    },
};
use xcursor::parser::Image;

use crate::{
    backend::{async_assets::{AsyncAssetResult, spawn_async_asset_worker}, icon::IconRasterizer, snapshot::{ClosingWindowSnapshot, LiveWindowSnapshot}, text::TextRasterizer, tty::BackendData},
    config::DisplayConfig,
    cursor::Cursor,
    drawing::PointerElement,
};
use crate::backend::visual::{inverse_transform_point, transformed_rect, transformed_root_rect};
use crate::ssd::{DecorationEvaluator, DecorationInteractionSnapshot, DecorationRuntimeEvaluator, LogicalPoint, LogicalRect, NodeDecorationEvaluator, WaylandWindowSnapshot, WindowDecorationState, WindowPositionSnapshot};
use tracing::{debug, info, warn};

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
    pub kde_decoration_state: KdeDecorationState,
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
    pub live_window_snapshots: HashMap<String, LiveWindowSnapshot>,
    pub complete_window_snapshots: HashMap<String, LiveWindowSnapshot>,
    pub closing_window_snapshots: HashMap<String, ClosingWindowSnapshot>,
    pub snapshot_dirty_window_ids: HashSet<String>,
    pub window_commit_times: HashMap<Window, std::time::Duration>,
    pub pending_decoration_damage: Vec<LogicalRect>,
    pub decoration_evaluator: DecorationRuntimeEvaluator,
    pub dmabuf_state: DmabufState,
    pub dmabuf_global: Option<DmabufGlobal>,
    pub damage_blink_enabled: bool,
    pub damage_blink_visible: HashMap<String, Vec<LogicalRect>>,
    pub damage_blink_pending: HashMap<String, Vec<LogicalRect>>,
    pub runtime_poll_dirty: bool,
    pub runtime_dirty_window_ids: std::collections::HashSet<String>,
    pub runtime_scheduler_enabled: bool,
    pub suggested_window_offset: Option<(i32, i32)>,
    pub async_asset_dirty: bool,

    pub is_running: bool,
    pub needs_redraw: bool,
    pub cursor_status: CursorImageStatus,
    pub cursor_override: Option<CursorIcon>,
    pub cursor_theme: Cursor,
    pub pointer_images: Vec<(Image, MemoryRenderBuffer)>,
    pub current_pointer_image: Option<Image>,
    pub pointer_element: PointerElement,
    pub text_rasterizer: TextRasterizer,
    pub icon_rasterizer: IconRasterizer,
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
        let kde_decoration_state = KdeDecorationState::new::<Self>(&dh, KdeDecorationMode::Server);
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
        TextInputManagerState::new::<Self>(&dh);
        InputMethodManagerState::new::<Self, _>(&dh, |_client| true);
        VirtualKeyboardManagerState::new::<Self, _>(&dh, |_client| true);

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
        Self::init_runtime_scheduler(event_loop);

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

        let (async_asset_tx, async_asset_rx) = channel();
        let async_asset_job_sender = spawn_async_asset_worker(async_asset_tx);
        event_loop
            .handle()
            .insert_source(async_asset_rx, |event, _, state| {
                match event {
                    ChannelEvent::Msg(result) => {
                        match result {
                            AsyncAssetResult::TextReady {
                                spec_hash,
                                width,
                                height,
                                pixels,
                            } => state
                                .text_rasterizer
                                .handle_async_ready(spec_hash, width, height, pixels),
                            AsyncAssetResult::TextMissing { spec_hash } => {
                                state.text_rasterizer.handle_async_miss(spec_hash)
                            }
                            AsyncAssetResult::IconReady {
                                spec_hash,
                                width,
                                height,
                                pixels,
                            } => state
                                .icon_rasterizer
                                .handle_async_ready(spec_hash, width, height, pixels),
                            AsyncAssetResult::IconMissing { spec_hash } => {
                                state.icon_rasterizer.handle_async_miss(spec_hash)
                            }
                        }
                        state.async_asset_dirty = true;
                        state.schedule_redraw();
                    }
                    ChannelEvent::Closed => {}
                }
            })
            .expect("Failed to init async asset worker.");

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
            kde_decoration_state,
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
            live_window_snapshots: HashMap::new(),
            complete_window_snapshots: HashMap::new(),
            closing_window_snapshots: HashMap::new(),
            snapshot_dirty_window_ids: HashSet::new(),
            window_commit_times: HashMap::new(),
            pending_decoration_damage: Vec::new(),
            decoration_evaluator,
            dmabuf_state: DmabufState::new(),
            dmabuf_global: None,
            damage_blink_enabled,
            damage_blink_visible: HashMap::new(),
            damage_blink_pending: HashMap::new(),
            runtime_poll_dirty: false,
            runtime_dirty_window_ids: Default::default(),
            runtime_scheduler_enabled: false,
            suggested_window_offset: None,
            async_asset_dirty: false,

            is_running: true,
            needs_redraw: true,
            cursor_status: CursorImageStatus::default_named(),
            cursor_override: None,
            cursor_theme: Cursor::load(),
            pointer_images: Vec::new(),
            current_pointer_image: None,
            pointer_element: PointerElement::default(),
            text_rasterizer: TextRasterizer::new(Some(async_asset_job_sender.clone())),
            icon_rasterizer: IconRasterizer::new(Some(async_asset_job_sender)),
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

    fn init_runtime_scheduler(event_loop: &mut EventLoop<Self>) {
        let loop_handle = event_loop.handle();
        loop_handle
            .insert_source(Timer::immediate(), |_, _, state| {
                if !state.runtime_scheduler_enabled {
                    return TimeoutAction::ToDuration(Duration::from_millis(250));
                }

                let now_ms = Duration::from(state.clock.now()).as_millis() as u64;
                let tick = match state.decoration_evaluator.scheduler_tick(now_ms) {
                    Ok(tick) => tick,
                    Err(error) => {
                        debug!(?error, "failed to tick decoration runtime scheduler");
                        state.runtime_scheduler_enabled = false;
                        return TimeoutAction::ToDuration(Duration::from_millis(250));
                    }
                };
                if tick.dirty {
                    state.runtime_poll_dirty = true;
                    state.runtime_dirty_window_ids
                        .extend(tick.dirty_window_ids.into_iter());
                    state.schedule_redraw();
                }

                if !tick.actions.is_empty() {
                    state.apply_runtime_window_actions(tick.actions);
                    state.schedule_redraw();
                }

                state.runtime_scheduler_enabled = tick.next_poll_in_ms.is_some();
                TimeoutAction::ToDuration(Duration::from_millis(
                    tick.next_poll_in_ms.unwrap_or(250).clamp(8, 250),
                ))
            })
            .expect("Failed to init runtime scheduler.");
    }

    pub fn warmup_decoration_runtime(&self) {
        let snapshot = WaylandWindowSnapshot {
            id: "__warmup__".into(),
            title: "warmup".into(),
            app_id: Some("shoji_wm.warmup".into()),
            position: WindowPositionSnapshot::default(),
            is_focused: false,
            is_floating: true,
            is_maximized: false,
            is_fullscreen: false,
            is_xwayland: false,
            icon: None,
            interaction: DecorationInteractionSnapshot::default(),
        };

        if let Err(error) = self.decoration_evaluator.evaluate_window(&snapshot) {
            warn!(?error, "failed to warm up decoration runtime");
            return;
        }

        let _ = self.decoration_evaluator.window_closed(&snapshot.id);
        debug!(window_id = snapshot.id, "warmed up decoration runtime");
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

        let logical_pos = LogicalPoint::new(pos.x.floor() as i32, pos.y.floor() as i32);
        if let Some((window, decoration)) = self.window_under_transformed(logical_pos) {
            let transformed_client =
                transformed_rect(
                    decoration.client_rect,
                    decoration.layout.root.rect,
                    decoration.visual_transform,
                );
            if !transformed_client.contains(logical_pos) {
                return None;
            }

            let Some(location) = self.space.element_location(window) else {
                return None;
            };
            let local_pos = inverse_transform_point(
                pos,
                decoration.layout.root.rect,
                decoration.visual_transform,
            );

            return window
                .surface_under(local_pos - location.to_f64(), WindowSurfaceType::ALL)
                .map(|(surface, loc)| {
                    let desired_local = (local_pos - location.to_f64()) - loc.to_f64();
                    let surface_origin = pos - desired_local;
                    (surface, surface_origin)
                });
        }

        if let Some((window, _)) = self.raw_window_under(logical_pos) {
            let Some(location) = self.space.element_location(window) else {
                return None;
            };

            return window
                .surface_under(pos - location.to_f64(), WindowSurfaceType::ALL)
                .map(|(surface, loc)| (surface, loc.to_f64() + location.to_f64()));
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

    pub fn window_under_transformed(
        &self,
        logical_pos: LogicalPoint,
    ) -> Option<(&Window, &WindowDecorationState)> {
        self.space.elements().rev().find_map(|window| {
            let decoration = self.window_decorations.get(window)?;
            let transformed_root =
                transformed_root_rect(decoration.layout.root.rect, decoration.visual_transform);
            transformed_root.contains(logical_pos).then_some((window, decoration))
        })
    }

    pub fn raw_window_under(
        &self,
        logical_pos: LogicalPoint,
    ) -> Option<(&Window, LogicalRect)> {
        self.space.elements().rev().find_map(|window| {
            let location = self.space.element_location(window)?;
            let bbox = window.bbox();
            let rect = LogicalRect::new(
                location.x + bbox.loc.x,
                location.y + bbox.loc.y,
                bbox.size.w,
                bbox.size.h,
            );
            rect.contains(logical_pos).then_some((window, rect))
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

    pub fn finish_damage_blink_for_outputs<'a>(
        &mut self,
        outputs: impl IntoIterator<Item = &'a str>,
    ) {
        if !self.damage_blink_enabled {
            self.damage_blink_visible.clear();
            self.damage_blink_pending.clear();
            return;
        }

        let mut scheduled = false;

        for output_name in outputs {
            let previous_visible = self
                .damage_blink_visible
                .remove(output_name)
                .unwrap_or_default();
            let next_visible = self
                .damage_blink_pending
                .remove(output_name)
                .unwrap_or_default();

            let had_visible = !previous_visible.is_empty();
            let has_visible = !next_visible.is_empty();

            self.pending_decoration_damage
                .extend(previous_visible.iter().copied());
            self.pending_decoration_damage
                .extend(next_visible.iter().copied());

            if has_visible {
                self.damage_blink_visible
                    .insert(output_name.to_string(), next_visible);
            }

            scheduled |= had_visible || has_visible;
        }

        if scheduled {
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
