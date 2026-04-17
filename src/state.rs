use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    ffi::OsString,
    sync::Arc,
    time::Duration,
};

use smithay::{
    backend::drm::DrmNode,
    backend::renderer::element::memory::MemoryRenderBuffer,
    desktop::{
        LayerSurface, PopupKind, PopupManager, Space, Window, WindowSurfaceType,
        find_popup_root_surface,
        layer_map_for_output,
    },
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
            Resource,
        },
    },
    utils::{Clock, IsAlive, Logical, Monotonic, Physical, Point, Rectangle, Scale},
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
        xdg_activation::XdgActivationState,
    },
};
use xcursor::parser::Image;

use crate::backend::tty::{apply_tty_output_mode, tty_output_available_modes};
use crate::backend::visual::{inverse_transform_point, transformed_rect, transformed_root_rect};
use crate::runtime_process::{
    ManagedRuntimeService, RuntimeProcessAction, RuntimeProcessConfigUpdate, RuntimeProcessEntry,
    RuntimeProcessReloadPolicy, RuntimeProcessRestartPolicy, RuntimeProcessRunPolicy,
    kill_runtime_service, should_restart_service, spawn_runtime_process,
};
use crate::runtime_key_binding::{
    CompiledRuntimeKeyBinding, RuntimeKeyBindingConfigUpdate, RuntimeKeyBindingEntry,
    compile_runtime_key_bindings,
};
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

#[derive(Debug, Clone, Copy)]
pub struct PopupLatencyDebugState {
    pub surface_id: u32,
    pub created_at: Duration,
    pub committed_at: Option<Duration>,
}

#[derive(Debug, Clone, Copy)]
pub struct RightClickDebugState {
    pub pressed_at: Option<Duration>,
    pub released_at: Option<Duration>,
    pub location: Option<Point<f64, Logical>>,
}

#[derive(Clone, Default)]
pub struct PointerContents {
    pub surface: Option<(WlSurface, Point<f64, Logical>)>,
    pub layer: Option<LayerSurface>,
}

#[derive(Clone)]
pub struct PopupLifecycleDebugEntry {
    pub surface: WlSurface,
    pub root_surface_id: Option<u32>,
    pub kind: &'static str,
    pub tracked_at: Duration,
}

fn popup_lifecycle_debug_enabled() -> bool {
    std::env::var_os("SHOJI_POPUP_LIFECYCLE_DEBUG")
        .is_some_and(|value| value != "0" && !value.is_empty())
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
    pub xdg_activation_state: XdgActivationState,
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
    pub runtime_process_config_generation: u64,
    pub runtime_process_supervision_enabled: bool,
    pub runtime_process_entries: BTreeMap<String, RuntimeProcessEntry>,
    pub runtime_process_once_runs: HashMap<String, u64>,
    pub runtime_process_suppressed_services: HashMap<String, u64>,
    pub runtime_managed_services: BTreeMap<String, ManagedRuntimeService>,
    pub runtime_key_binding_entries: BTreeMap<String, RuntimeKeyBindingEntry>,
    pub runtime_key_bindings: Vec<CompiledRuntimeKeyBinding>,
    pub suggested_window_offset: Option<(i32, i32)>,
    pub async_asset_dirty: bool,
    pub configured_background_effect: Option<BackgroundEffectConfig>,
    pub configured_layer_effects: HashMap<String, BackgroundEffectConfig>,
    pub layer_backdrop_cache: HashMap<String, crate::backend::shader_effect::CachedBackdropTexture>,
    pub pointer_contents: PointerContents,
    pub layer_shell_on_demand_focus: Option<LayerSurface>,
    pub window_keyboard_focus: Option<WlSurface>,
    pub mapped_on_demand_layer_surfaces: HashSet<u32>,
    pub force_full_damage: bool,
    pub debug_previous_scene_signatures: HashMap<String, Vec<String>>,
    pub tty_maintenance_pending: bool,
    pub tty_maintenance_reasons: BTreeSet<&'static str>,
    pub event_source_wake_counts: BTreeMap<&'static str, u64>,
    pub wayland_display_dispatched_request_count: u64,
    pub popup_latency_debug: Option<PopupLatencyDebugState>,
    pub popup_lifecycle_debug_entries: BTreeMap<u32, PopupLifecycleDebugEntry>,
    pub right_click_debug: RightClickDebugState,

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

    pub fn pointer_contents_at(&self, pos: Point<f64, Logical>) -> PointerContents {
        let surface = self.surface_under(pos);
        let layer = surface
            .as_ref()
            .and_then(|(surface, _)| self.layer_surface_for_hit_surface(surface));
        PointerContents { surface, layer }
    }

    pub fn set_window_keyboard_focus_target(&mut self, window: Option<&Window>) {
        self.window_keyboard_focus = window.and_then(|window| {
            window
                .toplevel()
                .map(|toplevel| toplevel.wl_surface().clone())
        });
    }

    pub fn focus_layer_surface_if_on_demand(&mut self, layer: Option<LayerSurface>) {
        if let Some(layer) = layer {
            if matches!(
                layer.cached_state().keyboard_interactivity,
                smithay::wayland::shell::wlr_layer::KeyboardInteractivity::OnDemand
            ) {
                self.layer_shell_on_demand_focus = Some(layer);
                return;
            }
        }

        self.layer_shell_on_demand_focus = None;
    }

    fn exclusive_layer_focus_surface(&self) -> Option<WlSurface> {
        let target_layers = [
            WlrLayer::Overlay,
            WlrLayer::Top,
            WlrLayer::Bottom,
            WlrLayer::Background,
        ];

        self.space.outputs().find_map(|output| {
            let layers = layer_map_for_output(output);
            target_layers.iter().find_map(|target_layer| {
                layers.layers_on(*target_layer).find_map(|layer| {
                    let mapped = layers.layer_geometry(layer).is_some();
                    (mapped
                        && matches!(
                            layer.cached_state().keyboard_interactivity,
                            smithay::wayland::shell::wlr_layer::KeyboardInteractivity::Exclusive
                        ))
                    .then(|| layer.wl_surface().clone())
                })
            })
        })
    }

    fn prune_keyboard_focus_targets(&mut self) {
        if let Some(surface) = self.layer_shell_on_demand_focus.as_ref() {
            let alive = surface.alive();
            let mapped = self.space.outputs().any(|output| {
                let layers = layer_map_for_output(output);
                layers.layer_geometry(surface).is_some()
            });
            let still_on_demand = matches!(
                surface.cached_state().keyboard_interactivity,
                smithay::wayland::shell::wlr_layer::KeyboardInteractivity::OnDemand
            );

            if !(alive && mapped && still_on_demand) {
                self.layer_shell_on_demand_focus = None;
            }
        }

        if let Some(surface) = self.window_keyboard_focus.as_ref() {
            let still_mapped = self.space.elements().any(|window| {
                window
                    .toplevel()
                    .is_some_and(|toplevel| toplevel.wl_surface() == surface)
            });
            if !still_mapped {
                self.window_keyboard_focus = None;
            }
        }
    }

    pub fn update_keyboard_focus(&mut self, serial: smithay::utils::Serial) {
        self.prune_keyboard_focus_targets();

        let desired_focus = self
            .exclusive_layer_focus_surface()
            .or_else(|| {
                self.layer_shell_on_demand_focus
                    .as_ref()
                    .map(|layer| layer.wl_surface().clone())
            })
            .or_else(|| self.window_keyboard_focus.clone());

        let focused_window_surface = desired_focus.as_ref();

        for candidate in self.space.elements() {
            let should_activate = candidate.toplevel().is_some_and(|toplevel| {
                focused_window_surface.is_some_and(|surface| toplevel.wl_surface() == surface)
            });
            if candidate.set_activated(should_activate) {
                if let Some(toplevel) = candidate.toplevel() {
                    let _ = toplevel.send_pending_configure();
                }
            }
        }

        let current_focus = self
            .seat
            .get_keyboard()
            .and_then(|keyboard| keyboard.current_focus());
        let focus_changed = current_focus.as_ref().map(|surface| surface.id())
            != desired_focus.as_ref().map(|surface| surface.id());

        if focus_changed {
            self.seat
                .get_keyboard()
                .unwrap()
                .set_focus(self, desired_focus, serial);
            self.schedule_redraw();
        }
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
        let xdg_activation_state = XdgActivationState::new::<Self>(&dh);
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
            xdg_activation_state,
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
            runtime_process_config_generation: 0,
            runtime_process_supervision_enabled: false,
            runtime_process_entries: Default::default(),
            runtime_process_once_runs: Default::default(),
            runtime_process_suppressed_services: Default::default(),
            runtime_managed_services: Default::default(),
            runtime_key_binding_entries: Default::default(),
            runtime_key_bindings: Vec::new(),
            suggested_window_offset: None,
            async_asset_dirty: false,
            configured_background_effect,
            configured_layer_effects: HashMap::new(),
            layer_backdrop_cache: HashMap::new(),
            pointer_contents: PointerContents::default(),
            layer_shell_on_demand_focus: None,
            window_keyboard_focus: None,
            mapped_on_demand_layer_surfaces: Default::default(),
            force_full_damage,
            debug_previous_scene_signatures: HashMap::new(),
            tty_maintenance_pending: true,
            tty_maintenance_reasons: BTreeSet::new(),
            event_source_wake_counts: BTreeMap::new(),
            wayland_display_dispatched_request_count: 0,
            popup_latency_debug: None,
            popup_lifecycle_debug_entries: BTreeMap::new(),
            right_click_debug: RightClickDebugState {
                pressed_at: None,
                released_at: None,
                location: None,
            },

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
                state.record_event_source_wake("wayland-listener");
                info!("accepted new wayland client connection");
                // Inside the callback, you should insert the client into the display.
                //
                // You may also associate some data with the client when inserting the client.
                state
                    .display_handle
                    .insert_client(client_stream, Arc::new(ClientState::default()))
                    .unwrap();
                state.request_tty_maintenance("wayland-listener");
            })
            .expect("Failed to init the wayland event source.");

        // You also need to add the display itself to the event loop, so that client events will be processed by wayland-server.
        loop_handle
            .insert_source(
                Generic::new(display, Interest::READ, Mode::Level),
                |_, display, state| {
                    state.record_event_source_wake("wayland-display");
                    // Important: a readable display fd is not, by itself, proof that the TTY
                    // backend should run full maintenance. Firefox in particular can keep this
                    // source waking frequently under level-triggered semantics, and blindly
                    // coupling that wake to `space.refresh()/popups.cleanup()/flush_clients()`
                    // caused a self-amplifying CPU-heavy loop.
                    //
                    // We only request maintenance when `dispatch_clients()` actually consumes one
                    // or more requests. The TTY main loop then decides when to perform the
                    // pre-render refresh/cleanup work.
                    // Safety: we don't drop the display
                    let dispatched = unsafe { display.get_mut().dispatch_clients(state).unwrap() };
                    if dispatched > 0 {
                        state.record_wayland_display_dispatched_requests(dispatched);
                        state.request_tty_maintenance("wayland-display-requests");
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
                state.record_event_source_wake("runtime-scheduler-timer");
                state.refresh_runtime_processes();
                if !state.runtime_scheduler_enabled && !state.runtime_process_supervision_enabled {
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
                    state.request_tty_maintenance("runtime-scheduler-dirty");
                    state.schedule_redraw();
                }

                state.consume_runtime_display_config(tick.display_config);
                state.consume_runtime_key_binding_config(tick.key_binding_config);
                state.consume_runtime_process_config(tick.process_config);
                if !tick.process_actions.is_empty() {
                    state.apply_runtime_process_actions(tick.process_actions);
                }

                if !tick.actions.is_empty() {
                    state.request_tty_maintenance("runtime-scheduler-actions");
                    state.apply_runtime_window_actions(tick.actions);
                    state.schedule_redraw();
                }

                state.runtime_scheduler_enabled = tick.next_poll_in_ms.is_some();
                let next_interval_ms = match tick.next_poll_in_ms {
                    Some(0) => state.runtime_frame_sync_interval_ms(),
                    Some(ms) => ms.clamp(1, 250),
                    None if state.runtime_process_supervision_enabled => 250,
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
                self.consume_runtime_key_binding_config(result.key_binding_config);
                self.consume_runtime_process_config(result.process_config);
                if !result.process_actions.is_empty() {
                    self.apply_runtime_process_actions(result.process_actions);
                }
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

    pub fn apply_runtime_key_binding_config_update(
        &mut self,
        update: RuntimeKeyBindingConfigUpdate,
    ) {
        self.runtime_key_binding_entries = update
            .entries
            .into_iter()
            .map(|entry| (entry.id.clone(), entry))
            .collect();
        self.runtime_key_bindings =
            compile_runtime_key_bindings(&self.runtime_key_binding_entries);
    }

    pub fn consume_runtime_key_binding_config(
        &mut self,
        update: Option<RuntimeKeyBindingConfigUpdate>,
    ) {
        if let Some(update) = update {
            self.apply_runtime_key_binding_config_update(update);
        }
    }

    pub fn apply_runtime_process_config_update(&mut self, update: RuntimeProcessConfigUpdate) {
        self.runtime_process_config_generation =
            self.runtime_process_config_generation.saturating_add(1);
        self.runtime_process_entries = update
            .entries
            .into_iter()
            .map(|entry| (entry.id().to_string(), entry))
            .collect();
        self.reconcile_runtime_processes();
    }

    pub fn consume_runtime_process_config(&mut self, update: Option<RuntimeProcessConfigUpdate>) {
        if let Some(update) = update {
            self.apply_runtime_process_config_update(update);
        }
    }

    pub fn apply_runtime_process_actions(&mut self, actions: Vec<RuntimeProcessAction>) {
        for action in actions {
            if !action.launch.is_valid() {
                warn!(?action, "ignoring invalid runtime process action");
                continue;
            }
            if let Err(error) =
                spawn_runtime_process(&action.launch, action.cwd.as_deref(), &action.env)
            {
                warn!(?error, ?action, "failed to spawn runtime process action");
            }
        }
    }

    pub fn refresh_runtime_processes(&mut self) {
        let service_ids = self
            .runtime_managed_services
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        for service_id in service_ids {
            let status = {
                let Some(service) = self.runtime_managed_services.get_mut(&service_id) else {
                    continue;
                };
                match service.child.try_wait() {
                    Ok(status) => status,
                    Err(error) => {
                        warn!(?error, service_id, "failed to query runtime service status");
                        None
                    }
                }
            };
            let Some(status) = status else {
                continue;
            };
            let Some(service) = self.runtime_managed_services.remove(&service_id) else {
                continue;
            };
            let restart_policy = match &service.spec {
                RuntimeProcessEntry::Service { restart, .. } => *restart,
                RuntimeProcessEntry::Once { .. } => RuntimeProcessRestartPolicy::Never,
            };
            if should_restart_service(restart_policy, status) {
                self.runtime_process_suppressed_services.remove(&service_id);
                info!(
                    service_id,
                    exit_status = status.code(),
                    "runtime service exited and will be restarted"
                );
            } else {
                self.runtime_process_suppressed_services
                    .insert(service_id.clone(), self.runtime_process_config_generation);
                info!(
                    service_id,
                    exit_status = status.code(),
                    "runtime service exited and will stay stopped"
                );
            }
        }

        self.reconcile_runtime_processes();
    }

    fn reconcile_runtime_processes(&mut self) {
        let generation = self.runtime_process_config_generation;
        let desired_service_ids = self
            .runtime_process_entries
            .values()
            .filter_map(|entry| match entry {
                RuntimeProcessEntry::Service { id, .. } => Some(id.clone()),
                RuntimeProcessEntry::Once { .. } => None,
            })
            .collect::<BTreeSet<_>>();

        let active_service_ids = self
            .runtime_managed_services
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        for service_id in active_service_ids {
            let desired = self.runtime_process_entries.get(&service_id);
            let mut should_remove = false;
            let mut should_restart = false;

            match desired {
                Some(RuntimeProcessEntry::Service { reload, .. }) => {
                    if let Some(active) = self.runtime_managed_services.get(&service_id) {
                        if active.spec != *desired.expect("matched Some above") {
                            should_restart = true;
                        } else if *reload == RuntimeProcessReloadPolicy::AlwaysRestart
                            && active.last_started_generation != generation
                        {
                            should_restart = true;
                        }
                    }
                }
                _ => should_remove = true,
            }

            if should_remove || should_restart {
                if let Some(mut service) = self.runtime_managed_services.remove(&service_id) {
                    if let Err(error) = kill_runtime_service(&mut service) {
                        warn!(?error, service_id, "failed to stop runtime service");
                    }
                }
                self.runtime_process_suppressed_services.remove(&service_id);
            }
        }

        let desired_entries = self
            .runtime_process_entries
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for entry in desired_entries {
            match entry {
                RuntimeProcessEntry::Once {
                    id,
                    launch,
                    cwd,
                    env,
                    run_policy,
                } => {
                    if !launch.is_valid() {
                        warn!(process_id = %id, "ignoring invalid runtime once process");
                        continue;
                    }

                    let should_run = match (run_policy, self.runtime_process_once_runs.get(&id)) {
                        (RuntimeProcessRunPolicy::OncePerSession, Some(_)) => false,
                        (RuntimeProcessRunPolicy::OncePerSession, None) => true,
                        (RuntimeProcessRunPolicy::OncePerConfigVersion, Some(last_run_generation)) => {
                            *last_run_generation != generation
                        }
                        (RuntimeProcessRunPolicy::OncePerConfigVersion, None) => true,
                    };

                    if !should_run {
                        continue;
                    }

                    match spawn_runtime_process(&launch, cwd.as_deref(), &env) {
                        Ok(_child) => {
                            self.runtime_process_once_runs.insert(id.clone(), generation);
                            info!(process_id = %id, run_policy = ?run_policy, "started runtime once process");
                        }
                        Err(error) => {
                            warn!(?error, process_id = %id, "failed to start runtime once process");
                        }
                    }
                }
                RuntimeProcessEntry::Service { ref id, .. } => {
                    let service_id = id.clone();
                    let RuntimeProcessEntry::Service {
                        launch,
                        cwd,
                        env,
                        ..
                    } = &entry
                    else {
                        unreachable!("matched service entry above");
                    };
                    if !launch.is_valid() {
                        warn!(process_id = %service_id, "ignoring invalid runtime service");
                        continue;
                    }
                    if self
                        .runtime_process_suppressed_services
                        .get(&service_id)
                        .is_some_and(|last_generation| *last_generation == generation)
                    {
                        continue;
                    }
                    if self.runtime_managed_services.contains_key(service_id.as_str()) {
                        continue;
                    }

                    match spawn_runtime_process(launch, cwd.as_deref(), env) {
                        Ok(child) => {
                            self.runtime_process_suppressed_services.remove(&service_id);
                            self.runtime_managed_services.insert(
                                service_id.clone(),
                                ManagedRuntimeService {
                                    spec: entry,
                                    child,
                                    last_started_generation: generation,
                                },
                            );
                            info!(process_id = %service_id, "started runtime managed service");
                        }
                        Err(error) => {
                            warn!(?error, process_id = %service_id, "failed to start runtime managed service");
                        }
                    }
                }
            }
        }

        self.runtime_process_supervision_enabled = !desired_service_ids.is_empty();
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

        if let Some(focus) = self.layer_surface_under_with_policy(
            &layers,
            &output_geo,
            pos,
            &[WlrLayer::Overlay, WlrLayer::Top],
            true,
        ).or_else(|| {
            self.layer_surface_under_with_policy(
                &layers,
                &output_geo,
                pos,
                &[WlrLayer::Overlay, WlrLayer::Top],
                false,
            )
        }) {
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

        self.layer_surface_under_with_policy(
            &layers,
            &output_geo,
            pos,
            &[WlrLayer::Bottom, WlrLayer::Background],
            false,
        )
    }

    fn layer_surface_under_with_policy(
        &self,
        layers: &smithay::desktop::LayerMap,
        output_geo: &Rectangle<i32, Logical>,
        pos: Point<f64, Logical>,
        target_layers: &[WlrLayer],
        skip_noninteractive_fullscreen: bool,
    ) -> Option<(WlSurface, Point<f64, Logical>)> {
        target_layers
            .iter()
            .copied()
            .flat_map(|target_layer| layers.layers_on(target_layer).rev())
            .find_map(|layer| {
                let layer_geo = layers.layer_geometry(layer).unwrap();
                let keyboard_interactivity = layer.cached_state().keyboard_interactivity;
                let is_full_output_cover =
                    layer_geo.loc == (0, 0).into() && layer_geo.size == output_geo.size;

                if skip_noninteractive_fullscreen
                    && matches!(keyboard_interactivity, smithay::wayland::shell::wlr_layer::KeyboardInteractivity::None)
                    && is_full_output_cover
                {
                    debug!(
                        pos = ?pos,
                        output = %output_geo.loc.x,
                        layer_surface_id = layer.wl_surface().id().protocol_id(),
                        layer = ?layer.layer(),
                        layer_geo = ?layer_geo,
                        keyboard_interactivity = ?keyboard_interactivity,
                        "skipping fullscreen non-interactive layer during preferred hit-test"
                    );
                    return None;
                }

                let result = layer
                    .surface_under(
                        pos - output_geo.loc.to_f64() - layer_geo.loc.to_f64(),
                        WindowSurfaceType::ALL,
                    )
                    .map(|(surface, loc)| (surface, (loc + layer_geo.loc + output_geo.loc).to_f64()));
                debug!(
                    pos = ?pos,
                    output = %output_geo.loc.x,
                    layer_surface_id = layer.wl_surface().id().protocol_id(),
                    layer = ?layer.layer(),
                    layer_geo = ?layer_geo,
                    layer_surface_geo = ?layer.geometry(),
                    layer_origin = ?layer_geo.loc,
                    keyboard_interactivity = ?keyboard_interactivity,
                    hit_surface_id = result.as_ref().map(|(surface, _)| surface.id().protocol_id()),
                    hit = result.is_some(),
                    "layer-shell hit-test"
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

    pub fn layer_surface_under(&self, pos: Point<f64, Logical>) -> Option<LayerSurface> {
        let output = self.space.outputs().find(|output| {
            self.space
                .output_geometry(output)
                .is_some_and(|geometry| geometry.contains(pos.to_i32_round()))
        })?;
        let output_geo = self.space.output_geometry(output)?;
        let layers = layer_map_for_output(output);
        let output_local = pos - output_geo.loc.to_f64();

        [
            WlrLayer::Overlay,
            WlrLayer::Top,
            WlrLayer::Bottom,
            WlrLayer::Background,
        ]
        .into_iter()
        .find_map(|target_layer| {
            let layer = layers.layer_under(target_layer, output_local)?;
            let layer_geo = layers.layer_geometry(layer)?;
            let local = output_local - layer_geo.loc.to_f64();
            layer
                .surface_under(local, WindowSurfaceType::ALL)
                .map(|_| layer.clone())
        })
    }

    pub fn layer_surface_for_hit_surface(&self, surface: &WlSurface) -> Option<LayerSurface> {
        let mut root = surface.clone();
        while let Some(parent) = smithay::wayland::compositor::get_parent(&root) {
            root = parent;
        }

        self.space.outputs().find_map(|output| {
            let layers = layer_map_for_output(output);
            layers
                .layer_for_surface(&root, WindowSurfaceType::TOPLEVEL)
                .cloned()
        })
    }

    pub fn shutdown(&mut self) {
        info!("shutdown requested");
        self.is_running = false;
        self.loop_signal.stop();
    }

    pub fn request_tty_maintenance(&mut self, reason: &'static str) {
        // The TTY backend no longer infers maintenance directly from event-loop wakeups.
        // Instead, subsystems request it explicitly when they know that a later
        // `space.refresh()/popups.cleanup()/flush_clients()` pass is semantically needed.
        self.tty_maintenance_pending = true;
        self.tty_maintenance_reasons.insert(reason);
    }

    pub fn take_tty_maintenance_pending(&mut self) -> bool {
        std::mem::take(&mut self.tty_maintenance_pending)
    }

    pub fn take_tty_maintenance_reasons(&mut self) -> Vec<String> {
        std::mem::take(&mut self.tty_maintenance_reasons)
            .into_iter()
            .map(ToOwned::to_owned)
            .collect()
    }

    pub fn record_event_source_wake(&mut self, source: &'static str) {
        *self.event_source_wake_counts.entry(source).or_default() += 1;
    }

    pub fn take_event_source_wake_counts(&mut self) -> Vec<(String, u64)> {
        std::mem::take(&mut self.event_source_wake_counts)
            .into_iter()
            .map(|(source, count)| (source.to_string(), count))
            .collect()
    }

    pub fn record_wayland_display_dispatched_requests(&mut self, count: usize) {
        self.wayland_display_dispatched_request_count = self
            .wayland_display_dispatched_request_count
            .saturating_add(count as u64);
    }

    pub fn take_wayland_display_dispatched_request_count(&mut self) -> u64 {
        std::mem::take(&mut self.wayland_display_dispatched_request_count)
    }

    pub fn schedule_redraw(&mut self) {
        self.needs_redraw = true;
    }

    pub fn note_xdg_popup_created(&mut self, surface_id: u32) {
        self.popup_latency_debug = Some(PopupLatencyDebugState {
            surface_id,
            created_at: Duration::from(self.clock.now()),
            committed_at: None,
        });
        if std::env::var_os("SHOJI_RIGHT_CLICK_TRACE").is_some() {
            let now = Duration::from(self.clock.now());
            info!(
                surface_id,
                since_right_press_ms = self
                    .right_click_debug
                    .pressed_at
                    .and_then(|pressed| now.checked_sub(pressed))
                    .map(|delta| delta.as_secs_f64() * 1000.0),
                since_right_release_ms = self
                    .right_click_debug
                    .released_at
                    .and_then(|released| now.checked_sub(released))
                    .map(|delta| delta.as_secs_f64() * 1000.0),
                right_click_location = ?self.right_click_debug.location,
                "right click trace: xdg popup created"
            );
        }
        if std::env::var_os("SHOJI_XDG_POPUP_LATENCY_DEBUG").is_some() {
            tracing::info!(surface_id, "xdg popup latency: created");
        }
    }

    pub fn note_right_click_button(
        &mut self,
        pressed: bool,
        location: Point<f64, Logical>,
        source: &'static str,
    ) {
        let now = Duration::from(self.clock.now());
        if pressed {
            self.right_click_debug.pressed_at = Some(now);
        } else {
            self.right_click_debug.released_at = Some(now);
        }
        self.right_click_debug.location = Some(location);

        if std::env::var_os("SHOJI_RIGHT_CLICK_TRACE").is_some() {
            info!(
                source,
                pressed,
                location = ?location,
                "right click trace: button observed"
            );
        }
    }

    pub fn note_xdg_popup_committed(&mut self, surface_id: u32) {
        if let Some(popup_debug) = self.popup_latency_debug.as_mut() {
            if popup_debug.surface_id == surface_id {
                popup_debug.committed_at = Some(Duration::from(self.clock.now()));
                if std::env::var_os("SHOJI_XDG_POPUP_LATENCY_DEBUG").is_some() {
                    tracing::info!(
                        surface_id,
                        created_to_commit_ms = popup_debug
                            .committed_at
                            .and_then(|commit| commit.checked_sub(popup_debug.created_at))
                            .map(|delta| delta.as_secs_f64() * 1000.0),
                        "xdg popup latency: committed"
                    );
                }
            }
        }
    }

    fn popup_kind_name(popup: &PopupKind) -> &'static str {
        match popup {
            PopupKind::Xdg(_) => "xdg",
            PopupKind::InputMethod(_) => "input-method",
        }
    }

    fn popup_surface_id(popup: &PopupKind) -> u32 {
        match popup {
            PopupKind::Xdg(surface) => surface.wl_surface().id().protocol_id(),
            PopupKind::InputMethod(surface) => surface.wl_surface().id().protocol_id(),
        }
    }

    fn popup_root_surface_id(popup: &PopupKind) -> Option<u32> {
        find_popup_root_surface(popup)
            .ok()
            .map(|surface| surface.id().protocol_id())
    }

    pub fn note_popup_tracked(&mut self, popup: &PopupKind, source: &'static str) {
        let surface_id = Self::popup_surface_id(popup);
        let root_surface_id = Self::popup_root_surface_id(popup);
        let kind = Self::popup_kind_name(popup);
        self.popup_lifecycle_debug_entries.insert(
            surface_id,
            PopupLifecycleDebugEntry {
                surface: match popup {
                    PopupKind::Xdg(surface) => surface.wl_surface().clone(),
                    PopupKind::InputMethod(surface) => surface.wl_surface().clone(),
                },
                root_surface_id,
                kind,
                tracked_at: Duration::from(self.clock.now()),
            },
        );
        if popup_lifecycle_debug_enabled() {
            info!(
                source,
                surface_id,
                root_surface_id,
                kind,
                tracked_count = self.popup_lifecycle_debug_entries.len(),
                "popup lifecycle: tracked popup"
            );
        }
    }

    pub fn find_popup_with_debug(
        &mut self,
        surface: &WlSurface,
        source: &'static str,
    ) -> Option<PopupKind> {
        let surface_id = surface.id().protocol_id();
        let found = self.popups.find_popup(surface);
        if popup_lifecycle_debug_enabled() {
            let known = self.popup_lifecycle_debug_entries.get(&surface_id);
            info!(
                source,
                surface_id,
                found = found.is_some(),
                found_kind = found.as_ref().map(Self::popup_kind_name),
                found_root_surface_id = found.as_ref().and_then(Self::popup_root_surface_id),
                known = known.is_some(),
                known_kind = known.map(|entry| entry.kind),
                known_root_surface_id = known.and_then(|entry| entry.root_surface_id),
                tracked_count = self.popup_lifecycle_debug_entries.len(),
                "popup lifecycle: find_popup"
            );
        }
        found
    }

    pub fn note_popup_dismiss_requested(
        &mut self,
        surface: &WlSurface,
        parent_surface_id: Option<u32>,
        source: &'static str,
    ) {
        if !popup_lifecycle_debug_enabled() {
            return;
        }
        let surface_id = surface.id().protocol_id();
        let known = self.popup_lifecycle_debug_entries.get(&surface_id);
        info!(
            source,
            surface_id,
            parent_surface_id,
            known = known.is_some(),
            known_kind = known.map(|entry| entry.kind),
            known_root_surface_id = known.and_then(|entry| entry.root_surface_id),
            tracked_count = self.popup_lifecycle_debug_entries.len(),
            "popup lifecycle: dismiss requested"
        );
    }

    pub fn cleanup_popups_with_debug(&mut self, source: &'static str) {
        if popup_lifecycle_debug_enabled() {
            info!(
                source,
                tracked_count = self.popup_lifecycle_debug_entries.len(),
                "popup lifecycle: cleanup start"
            );
        }
        self.popups.cleanup();

        let mut removed_entries = Vec::new();
        for (surface_id, entry) in &self.popup_lifecycle_debug_entries {
            if self.popups.find_popup(&entry.surface).is_none() {
                removed_entries.push((
                    *surface_id,
                    entry.root_surface_id,
                    entry.kind,
                    Duration::from(self.clock.now())
                        .checked_sub(entry.tracked_at)
                        .map(|delta| delta.as_secs_f64() * 1000.0),
                ));
            }
        }

        for (surface_id, _, _, _) in &removed_entries {
            self.popup_lifecycle_debug_entries.remove(surface_id);
        }

        if popup_lifecycle_debug_enabled() {
            info!(
                source,
                removed = ?removed_entries,
                remaining_tracked_count = self.popup_lifecycle_debug_entries.len(),
                "popup lifecycle: cleanup finish"
            );
        }
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
