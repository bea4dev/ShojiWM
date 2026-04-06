use std::{
    collections::{HashMap, HashSet},
    ffi::OsString,
    sync::Arc,
    time::Duration,
};

use smithay::{
    backend::drm::DrmNode,
    backend::renderer::element::memory::MemoryRenderBuffer,
    desktop::{PopupManager, Space, Window, WindowSurfaceType, layer_map_for_output},
    input::{
        Seat, SeatState,
        pointer::{CursorIcon, CursorImageStatus},
    },
    output::{Mode as OutputMode, Output, Scale as OutputScale},
    reexports::{
        calloop::channel::{Event as ChannelEvent, channel},
        calloop::{
            EventLoop, Interest, LoopSignal, Mode, PostAction,
            generic::Generic,
            timer::{TimeoutAction, Timer},
        },
        wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode as DecorationMode,
        wayland_protocols_misc::server_decoration::server::org_kde_kwin_server_decoration_manager::Mode as KdeDecorationMode,
        wayland_server::{
            Display, DisplayHandle,
            backend::{ClientData, ClientId, DisconnectReason},
            protocol::wl_surface::WlSurface,
        },
    },
    utils::{Clock, Logical, Monotonic, Physical, Point, Rectangle, Scale},
    wayland::{
        background_effect::BackgroundEffectState,
        commit_timing::CommitTimingManagerState,
        compositor::{
            CompositorClientState, CompositorState, Damage, SurfaceAttributes, with_states,
        },
        cursor_shape::CursorShapeManagerState,
        dmabuf::{DmabufGlobal, DmabufState},
        fifo::FifoManagerState,
        fixes::FixesState,
        fractional_scale::FractionalScaleManagerState,
        input_method::InputMethodManagerState,
        output::OutputManagerState,
        presentation::PresentationState,
        selection::{
            data_device::DataDeviceState, primary_selection::PrimarySelectionState,
            wlr_data_control::DataControlState,
        },
        shell::kde::decoration::KdeDecorationState,
        shell::wlr_layer::Layer as WlrLayer,
        shell::wlr_layer::WlrLayerShellState,
        shell::xdg::{XdgShellState, decoration::XdgDecorationState},
        shm::ShmState,
        single_pixel_buffer::SinglePixelBufferState,
        socket::ListeningSocketSource,
        text_input::TextInputManagerState,
        viewporter::ViewporterState,
        virtual_keyboard::VirtualKeyboardManagerState,
    },
};
use xcursor::parser::Image;

use crate::backend::tty::{apply_tty_output_mode, tty_output_available_modes};
use crate::backend::visual::{inverse_transform_point, transformed_rect, transformed_root_rect};
use crate::ssd::{
    BackgroundEffectConfig, DecorationEvaluator, DecorationInteractionSnapshot,
    DecorationRuntimeEvaluator, LogicalPoint, LogicalRect, NodeDecorationEvaluator,
    OutputModeSnapshot, OutputPositionSnapshot, WaylandOutputSnapshot, WaylandWindowSnapshot,
    WindowDecorationState, WindowPositionSnapshot,
};
use crate::{
    backend::{
        async_assets::{AsyncAssetResult, spawn_async_asset_worker},
        icon::IconRasterizer,
        snapshot::{ClosingWindowSnapshot, LiveWindowSnapshot},
        text::TextRasterizer,
        tty::BackendData,
    },
    config::{
        DisplayConfig, RuntimeDisplayConfigUpdate, RuntimeDisplayModePreference,
        RuntimeOutputConfig, RuntimeOutputPositionPreference,
    },
    cursor::Cursor,
    drawing::PointerElement,
};
use tracing::{debug, info, warn};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnedDamageRect {
    pub owner: String,
    pub rect: LogicalRect,
}

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
    pub window_primary_output_names: HashMap<Window, String>,
    pub windows_ready_for_decoration: HashSet<String>,
    pub live_window_snapshots: HashMap<String, LiveWindowSnapshot>,
    pub complete_window_snapshots: HashMap<String, LiveWindowSnapshot>,
    pub complete_window_snapshot_trackers: HashMap<String, smithay::backend::renderer::damage::OutputDamageTracker>,
    pub closing_window_snapshots: HashMap<String, ClosingWindowSnapshot>,
    pub snapshot_dirty_window_ids: HashSet<String>,
    pub transform_snapshot_window_ids: HashSet<String>,
    pub window_commit_times: HashMap<Window, std::time::Duration>,
    pub scene_generation: u64,
    pub window_scene_generation: u64,
    pub lower_layer_scene_generation: u64,
    pub upper_layer_scene_generation: u64,
    pub window_source_damage: Vec<OwnedDamageRect>,
    pub lower_layer_source_damage: Vec<OwnedDamageRect>,
    pub upper_layer_source_damage: Vec<OwnedDamageRect>,
    pub pending_decoration_damage: Vec<LogicalRect>,
    pub decoration_evaluator: DecorationRuntimeEvaluator,
    pub dmabuf_state: DmabufState,
    pub dmabuf_global: Option<DmabufGlobal>,
    pub background_effect_state: BackgroundEffectState,
    pub damage_blink_enabled: bool,
    pub damage_blink_visible: HashMap<String, Vec<LogicalRect>>,
    pub damage_blink_pending: HashMap<String, Vec<LogicalRect>>,
    pub runtime_poll_dirty: bool,
    pub runtime_dirty_window_ids: std::collections::HashSet<String>,
    pub runtime_scheduler_enabled: bool,
    pub runtime_animation_outputs: std::collections::HashSet<String>,
    pub runtime_output_configs: std::collections::BTreeMap<String, RuntimeOutputConfig>,
    pub suggested_window_offset: Option<(i32, i32)>,
    pub async_asset_dirty: bool,
    pub configured_background_effect: Option<BackgroundEffectConfig>,
    pub configured_layer_effects: HashMap<String, BackgroundEffectConfig>,
    pub layer_backdrop_cache: HashMap<String, crate::backend::shader_effect::CachedBackdropTexture>,
    pub force_full_damage: bool,
    pub debug_previous_scene_signatures: HashMap<String, Vec<String>>,

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
    fn output_auto_sort_key(output_name: &str) -> (i32, String) {
        let rank = if output_name.starts_with("eDP")
            || output_name.starts_with("LVDS")
            || output_name.starts_with("DSI")
        {
            0
        } else {
            1
        };
        (rank, output_name.to_string())
    }

    fn runtime_frame_sync_interval_ms(&self) -> u64 {
        self.space
            .outputs()
            .filter_map(|output| {
                output.current_mode().map(|mode| {
                    let secs = 1_000f64 / mode.refresh as f64;
                    (secs * 1000.0).round() as u64
                })
            })
            .filter(|ms| *ms > 0)
            .min()
            .unwrap_or(8)
            .clamp(1, 250)
    }

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
        let background_effect_state = BackgroundEffectState::new::<Self>(&dh);

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
        let (decoration_evaluator, configured_background_effect) =
            if std::path::Path::new("node_modules/.bin/tsx").exists() {
                let evaluator =
                    NodeDecorationEvaluator::for_workspace("packages/config/src/index.tsx")
                        .with_working_dir(std::env::current_dir().unwrap_or_else(|_| ".".into()));
                let configured_background_effect = match evaluator.background_effect_config() {
                    Ok(config) => config,
                    Err(error) => {
                        warn!(?error, "failed to load configured background effect");
                        None
                    }
                };
                (
                    DecorationRuntimeEvaluator::Node(evaluator),
                    configured_background_effect,
                )
            } else {
                (DecorationRuntimeEvaluator::Static(Default::default()), None)
            };

        let damage_blink_enabled = std::env::args().any(|arg| arg == "--damage-blink")
            || std::env::var_os("SHOJI_DAMAGE_BLINK")
                .is_some_and(|value| value != "0" && !value.is_empty());
        let force_full_damage = std::env::args().any(|arg| arg == "--force-full-damage")
            || std::env::var_os("SHOJI_FORCE_FULL_DAMAGE")
                .is_some_and(|value| value != "0" && !value.is_empty());

        let (async_asset_tx, async_asset_rx) = channel();
        let async_asset_job_sender = spawn_async_asset_worker(async_asset_tx);
        event_loop
            .handle()
            .insert_source(async_asset_rx, |event, _, state| match event {
                ChannelEvent::Msg(result) => {
                    match result {
                        AsyncAssetResult::TextReady {
                            spec_hash,
                            width,
                            height,
                            raster_scale,
                            pixels,
                        } => state.text_rasterizer.handle_async_ready(
                            spec_hash,
                            width,
                            height,
                            raster_scale,
                            pixels,
                        ),
                        AsyncAssetResult::TextMissing { spec_hash } => {
                            state.text_rasterizer.handle_async_miss(spec_hash)
                        }
                        AsyncAssetResult::IconReady {
                            spec_hash,
                            width,
                            height,
                            raster_scale,
                            pixels,
                        } => state.icon_rasterizer.handle_async_ready(
                            spec_hash,
                            width,
                            height,
                            raster_scale,
                            pixels,
                        ),
                        AsyncAssetResult::IconMissing { spec_hash } => {
                            state.icon_rasterizer.handle_async_miss(spec_hash)
                        }
                    }
                    state.async_asset_dirty = true;
                    state.schedule_redraw();
                }
                ChannelEvent::Closed => {}
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
            window_primary_output_names: HashMap::new(),
            windows_ready_for_decoration: HashSet::new(),
            live_window_snapshots: HashMap::new(),
            complete_window_snapshots: HashMap::new(),
            complete_window_snapshot_trackers: HashMap::new(),
            closing_window_snapshots: HashMap::new(),
            snapshot_dirty_window_ids: HashSet::new(),
            transform_snapshot_window_ids: HashSet::new(),
            window_commit_times: HashMap::new(),
            scene_generation: 0,
            window_scene_generation: 0,
            lower_layer_scene_generation: 0,
            upper_layer_scene_generation: 0,
            window_source_damage: Vec::new(),
            lower_layer_source_damage: Vec::new(),
            upper_layer_source_damage: Vec::new(),
            pending_decoration_damage: Vec::new(),
            decoration_evaluator,
            dmabuf_state: DmabufState::new(),
            dmabuf_global: None,
            background_effect_state,
            damage_blink_enabled,
            damage_blink_visible: HashMap::new(),
            damage_blink_pending: HashMap::new(),
            runtime_poll_dirty: false,
            runtime_dirty_window_ids: Default::default(),
            runtime_scheduler_enabled: false,
            runtime_animation_outputs: Default::default(),
            runtime_output_configs: Default::default(),
            suggested_window_offset: None,
            async_asset_dirty: false,
            configured_background_effect,
            configured_layer_effects: HashMap::new(),
            layer_backdrop_cache: HashMap::new(),
            force_full_damage,
            debug_previous_scene_signatures: HashMap::new(),

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
            display_config: DisplayConfig::from_env(),
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
                state.sync_runtime_display_state();
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
                    state
                        .runtime_dirty_window_ids
                        .extend(tick.dirty_window_ids.into_iter());
                    state.schedule_redraw();
                }

                state.consume_runtime_display_config(tick.display_config);

                if !tick.actions.is_empty() {
                    state.apply_runtime_window_actions(tick.actions);
                    state.schedule_redraw();
                }

                state.runtime_scheduler_enabled = tick.next_poll_in_ms.is_some();
                let next_interval_ms = match tick.next_poll_in_ms {
                    Some(0) => state.runtime_frame_sync_interval_ms(),
                    Some(ms) => ms.clamp(1, 250),
                    None => 250,
                };
                TimeoutAction::ToDuration(Duration::from_millis(next_interval_ms))
            })
            .expect("Failed to init runtime scheduler.");
    }

    pub fn warmup_decoration_runtime(&mut self) {
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

        let now_ms = Duration::from(self.clock.now()).as_millis() as u64;
        self.sync_runtime_display_state();
        match self.decoration_evaluator.evaluate_window(&snapshot, now_ms) {
            Ok(result) => {
                self.consume_runtime_display_config(result.display_config);
            }
            Err(error) => {
                warn!(?error, "failed to warm up decoration runtime");
                return;
            }
        }

        let _ = self.decoration_evaluator.window_closed(&snapshot.id);
        debug!(window_id = snapshot.id, "warmed up decoration runtime");
    }

    pub fn snapshot_outputs(&self) -> std::collections::BTreeMap<String, WaylandOutputSnapshot> {
        self.space
            .outputs()
            .map(|output| {
                let name = output.name();
                let available_modes = tty_output_available_modes(self, &name)
                    .unwrap_or_else(|| output.modes())
                    .into_iter()
                    .map(|mode| OutputModeSnapshot {
                        width: mode.size.w,
                        height: mode.size.h,
                        refresh_rate: mode.refresh as f64 / 1000.0,
                    })
                    .collect::<Vec<_>>();
                let resolution = output.current_mode().map(|mode| OutputModeSnapshot {
                    width: mode.size.w,
                    height: mode.size.h,
                    refresh_rate: mode.refresh as f64 / 1000.0,
                });
                let location = output.current_location();
                (
                    name,
                    WaylandOutputSnapshot {
                        resolution,
                        position: OutputPositionSnapshot {
                            x: location.x,
                            y: location.y,
                        },
                        scale: output.current_scale().fractional_scale(),
                        available_modes,
                    },
                )
            })
            .collect()
    }

    fn resolve_runtime_output_mode(
        &self,
        output: &Output,
        preference: &RuntimeDisplayModePreference,
    ) -> Option<OutputMode> {
        let modes =
            tty_output_available_modes(self, &output.name()).unwrap_or_else(|| output.modes());
        if modes.is_empty() {
            return output.current_mode();
        }
        match preference {
            RuntimeDisplayModePreference::Best(value) if value == "best" => {
                modes.into_iter().max_by_key(|mode| {
                    (
                        i64::from(mode.size.w) * i64::from(mode.size.h),
                        mode.refresh,
                    )
                })
            }
            RuntimeDisplayModePreference::Exact {
                width,
                height,
                refresh_rate,
            } => {
                let exact = modes
                    .into_iter()
                    .filter(|mode| {
                        mode.size.w == i32::from(*width) && mode.size.h == i32::from(*height)
                    })
                    .collect::<Vec<_>>();
                if exact.is_empty() {
                    return None;
                }
                match refresh_rate {
                    Some(refresh_rate) => exact.into_iter().min_by_key(|mode| {
                        ((mode.refresh as f64 / 1000.0 - refresh_rate).abs() * 1000.0) as i64
                    }),
                    None => exact.into_iter().max_by_key(|mode| mode.refresh),
                }
            }
            _ => None,
        }
    }

    pub fn apply_runtime_display_config_update(&mut self, update: RuntimeDisplayConfigUpdate) {
        for (output_name, config) in update.outputs {
            match config {
                Some(config) => {
                    self.runtime_output_configs.insert(output_name, config);
                }
                None => {
                    self.runtime_output_configs.remove(&output_name);
                }
            }
        }
        self.apply_runtime_display_configuration();
    }

    pub fn apply_runtime_display_configuration(&mut self) {
        let outputs = self.space.outputs().cloned().collect::<Vec<_>>();
        if outputs.is_empty() {
            return;
        }

        let mut target_modes = std::collections::BTreeMap::new();
        for output in &outputs {
            let target_mode = self
                .runtime_output_configs
                .get(&output.name())
                .and_then(|config| config.resolution.as_ref())
                .and_then(|preference| self.resolve_runtime_output_mode(output, preference));
            target_modes.insert(output.name(), target_mode.or_else(|| output.current_mode()));
        }

        let mut manual_positions = std::collections::BTreeMap::new();
        let mut auto_outputs = Vec::new();
        for output in &outputs {
            match self
                .runtime_output_configs
                .get(&output.name())
                .and_then(|config| config.position.as_ref())
            {
                Some(RuntimeOutputPositionPreference::Exact { x, y }) => {
                    manual_positions.insert(output.name(), (*x, *y));
                }
                Some(RuntimeOutputPositionPreference::Auto(value)) if value == "auto" => {
                    auto_outputs.push(output.name());
                }
                None => auto_outputs.push(output.name()),
                _ => auto_outputs.push(output.name()),
            }
        }

        auto_outputs.sort_by_key(|name| Self::output_auto_sort_key(name));
        let mut auto_cursor_x = manual_positions
            .iter()
            .filter_map(|(name, (x, _))| {
                target_modes
                    .get(name)
                    .and_then(|mode| *mode)
                    .map(|mode| x + mode.size.w)
            })
            .max()
            .unwrap_or(0);

        let mut target_positions = std::collections::BTreeMap::new();
        for (name, (x, y)) in manual_positions {
            target_positions.insert(name, Point::from((x, y)));
        }
        for output_name in auto_outputs {
            target_positions.insert(output_name.clone(), Point::from((auto_cursor_x, 0)));
            if let Some(mode) = target_modes.get(&output_name).and_then(|mode| *mode) {
                auto_cursor_x += mode.size.w;
            }
        }

        for output in outputs {
            let name = output.name();
            let target_mode = target_modes.get(&name).and_then(|mode| *mode);
            let target_position = target_positions
                .get(&name)
                .copied()
                .unwrap_or_else(|| output.current_location());
            let target_scale = self
                .runtime_output_configs
                .get(&name)
                .and_then(|config| config.scale)
                .map(|scale| OutputScale::Fractional(scale.max(0.1)));

            if let Some(mode) = target_mode {
                let current_mode = output.current_mode();
                if current_mode != Some(mode) {
                    let _ = apply_tty_output_mode(self, &name, mode);
                }
            }

            output.change_current_state(target_mode, None, target_scale, Some(target_position));
            self.space.map_output(&output, target_position);
        }

        for output in self.space.outputs() {
            if let Some(geometry) = self.space.output_geometry(output) {
                self.pending_decoration_damage.push(LogicalRect::new(
                    geometry.loc.x,
                    geometry.loc.y,
                    geometry.size.w,
                    geometry.size.h,
                ));
            }
        }
        self.schedule_redraw();
    }

    pub fn sync_runtime_display_state(&self) {
        self.decoration_evaluator
            .sync_display_state(self.snapshot_outputs());
    }

    pub fn consume_runtime_display_config(&mut self, update: Option<RuntimeDisplayConfigUpdate>) {
        if let Some(update) = update {
            self.apply_runtime_display_config_update(update);
        }
    }

    pub fn output_layout_bounds(&self) -> Option<Rectangle<i32, Logical>> {
        let mut outputs = self
            .space
            .outputs()
            .filter_map(|output| self.space.output_geometry(output));
        let first = outputs.next()?;
        Some(outputs.fold(first, |bounds, geometry| {
            let left = bounds.loc.x.min(geometry.loc.x);
            let top = bounds.loc.y.min(geometry.loc.y);
            let right = (bounds.loc.x + bounds.size.w).max(geometry.loc.x + geometry.size.w);
            let bottom = (bounds.loc.y + bounds.size.h).max(geometry.loc.y + geometry.size.h);
            Rectangle::new((left, top).into(), (right - left, bottom - top).into())
        }))
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
            let transformed_client = transformed_rect(
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
            transformed_root
                .contains(logical_pos)
                .then_some((window, decoration))
        })
    }

    pub fn raw_window_under(&self, logical_pos: LogicalPoint) -> Option<(&Window, LogicalRect)> {
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

    pub fn logical_damage_rect_for_window(&self, window: &Window) -> Option<LogicalRect> {
        if let Some(decoration) = self.window_decorations.get(window) {
            return Some(transformed_root_rect(
                decoration.layout.root.rect,
                decoration.visual_transform,
            ));
        }

        let location = self.space.element_location(window)?;
        let bbox = window.bbox();
        Some(LogicalRect::new(
            location.x + bbox.loc.x,
            location.y + bbox.loc.y,
            bbox.size.w,
            bbox.size.h,
        ))
    }

    pub fn logical_source_damage_rects_for_surface(
        &self,
        window: &Window,
        surface: &WlSurface,
    ) -> Vec<LogicalRect> {
        let Some(decoration) = self.window_decorations.get(window) else {
            return self
                .logical_damage_rect_for_window(window)
                .into_iter()
                .collect();
        };
        let Some(root_surface) = window
            .toplevel()
            .map(|surface| surface.wl_surface().clone())
        else {
            return self
                .logical_damage_rect_for_window(window)
                .into_iter()
                .collect();
        };
        if surface != &root_surface {
            return self
                .logical_damage_rect_for_window(window)
                .into_iter()
                .collect();
        }

        let damage_rects = with_states(surface, |states| {
            let mut cached = states.cached_state.get::<SurfaceAttributes>();
            let attrs = cached.current();
            let buffer_scale = attrs.buffer_scale.max(1);
            attrs
                .damage
                .iter()
                .map(|damage| match damage {
                    Damage::Surface(rect) => {
                        LogicalRect::new(rect.loc.x, rect.loc.y, rect.size.w, rect.size.h)
                    }
                    Damage::Buffer(rect) => LogicalRect::new(
                        rect.loc.x.div_euclid(buffer_scale),
                        rect.loc.y.div_euclid(buffer_scale),
                        rect.size
                            .w
                            .saturating_add(buffer_scale.saturating_sub(1))
                            .div_euclid(buffer_scale),
                        rect.size
                            .h
                            .saturating_add(buffer_scale.saturating_sub(1))
                            .div_euclid(buffer_scale),
                    ),
                })
                .collect::<Vec<_>>()
        });

        if damage_rects.is_empty() {
            return self
                .logical_damage_rect_for_window(window)
                .into_iter()
                .collect();
        }

        let mapped = damage_rects
            .into_iter()
            .map(|rect| {
                transformed_rect(
                    LogicalRect::new(
                        decoration.client_rect.x + rect.x,
                        decoration.client_rect.y + rect.y,
                        rect.width,
                        rect.height,
                    ),
                    decoration.layout.root.rect,
                    decoration.visual_transform,
                )
            })
            .collect::<Vec<_>>();

        mapped
    }

    pub fn clear_source_damage(&mut self) {
        self.window_source_damage.clear();
        self.lower_layer_source_damage.clear();
        self.upper_layer_source_damage.clear();
    }

    pub fn damage_blink_rects_for_output(&self, output: &Output) -> &[LogicalRect] {
        self.damage_blink_visible
            .get(&output.name())
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub fn record_damage_blink(&mut self, output: &Output, damage: &[Rectangle<i32, Physical>]) {
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
            .entry(output.name().to_string())
            .or_default()
            .extend(rects);
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
        let had_visible = self
            .damage_blink_visible
            .values()
            .any(|rects| !rects.is_empty());
        self.damage_blink_visible = std::mem::take(&mut self.damage_blink_pending);
        let next_visible = self
            .damage_blink_visible
            .values()
            .flat_map(|rects| rects.iter().copied())
            .collect::<Vec<_>>();
        let has_visible = self
            .damage_blink_visible
            .values()
            .any(|rects| !rects.is_empty());

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
