use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    ffi::OsString,
    fs,
    os::fd::AsFd,
    sync::{
        Arc,
        atomic::{AtomicI32, Ordering},
    },
    time::{Duration, Instant},
};

use smithay::{
    backend::renderer::element::memory::MemoryRenderBuffer,
    backend::{drm::DrmNode, session::libseat::LibSeatSession},
    desktop::{
        LayerSurface, PopupKind, PopupManager, Space, Window, WindowSurfaceType,
        find_popup_root_surface, layer_map_for_output, utils::under_from_surface_tree,
    },
    input::{
        Seat, SeatState,
        keyboard::ModifiersState,
        pointer::{CursorIcon, CursorImageStatus},
    },
    output::{Mode as OutputMode, Output, Scale as OutputScale},
    reexports::{
        calloop::channel::{Event as ChannelEvent, channel},
        calloop::{
            EventLoop, Interest, LoopHandle, LoopSignal, Mode, PostAction,
            generic::Generic,
            timer::{TimeoutAction, Timer},
        },
        rustix::net::sockopt::socket_peercred,
        wayland_protocols_misc::server_decoration::server::org_kde_kwin_server_decoration_manager::Mode as KdeDecorationMode,
        wayland_server::{
            Display, DisplayHandle, Resource,
            backend::{ClientData, ClientId, DisconnectReason, GlobalId},
            protocol::wl_surface::WlSurface,
        },
    },
    utils::{
        Clock, IsAlive, Logical, Monotonic, Physical, Point, Rectangle, SERIAL_COUNTER, Scale,
    },
    wayland::{
        background_effect::BackgroundEffectState,
        commit_timing::CommitTimingManagerState,
        compositor::{
            CompositorClientState, CompositorState, Damage, SurfaceAttributes, get_parent,
            with_states,
        },
        cursor_shape::CursorShapeManagerState,
        dmabuf::{DmabufGlobal, DmabufState},
        fifo::FifoManagerState,
        fixes::FixesState,
        fractional_scale::FractionalScaleManagerState,
        idle_inhibit::IdleInhibitManagerState,
        idle_notify::IdleNotifierState,
        input_method::InputMethodManagerState,
        output::OutputManagerState,
        pointer_constraints::PointerConstraintsState,
        presentation::PresentationState,
        relative_pointer::RelativePointerManagerState,
        selection::{
            data_device::DataDeviceState, primary_selection::PrimarySelectionState,
            wlr_data_control::DataControlState,
        },
        session_lock::{LockSurface, SessionLockManagerState},
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
        xwayland_shell::XWaylandShellState,
    },
    xwayland::{X11Wm, XWayland, XWaylandEvent},
};
use xcursor::parser::Image;

use crate::activation_environment::publish_activation_environment;
use crate::backend::tty::{
    apply_tty_output_mode, tty_connected_outputs, tty_output_available_modes,
};
use crate::backend::visual::{inverse_transform_point, transformed_rect, transformed_root_rect};
use crate::runtime_input::{
    RuntimeInputConfig, RuntimeInputConfigUpdate, RuntimeInputDeviceSnapshot,
    apply_config_to_libinput_devices, apply_keyboard_config, libinput_device_key,
    snapshot_for_libinput_device,
};
use crate::runtime_key_binding::{
    CompiledRuntimeKeyBinding, RuntimeKeyBindingConfigUpdate, RuntimeKeyBindingEntry,
    compile_runtime_key_bindings,
};
use crate::runtime_pointer::{
    RuntimePointerConfigUpdate, RuntimePointerModifier, parse_runtime_pointer_modifier,
};
use crate::runtime_process::{
    ManagedRuntimeService, RuntimeProcessAction, RuntimeProcessConfigUpdate, RuntimeProcessEntry,
    RuntimeProcessReloadPolicy, RuntimeProcessRestartPolicy, RuntimeProcessRunPolicy,
    kill_runtime_service, should_restart_service, spawn_runtime_process,
};
use crate::runtime_workspace::RuntimeWorkspaceConfigUpdate;
use crate::ssd::{
    BackgroundEffectConfig, DecorationEvaluator, DecorationHandlerInvocation,
    DecorationInteractionSnapshot, DecorationInteractionTarget,
    DecorationPointerMoveAsyncInvocation, DecorationRuntimeAsyncInvocation,
    DecorationRuntimeEvaluator, EmbeddedDecorationEvaluator, LogicalPoint, LogicalRect,
    ManagedWindowAnimationSnapshot, OutputModeSnapshot, OutputPositionSnapshot,
    RuntimeEventConfigUpdate, WaylandOutputSnapshot, WaylandWindowSnapshot, WindowDecorationState,
    WindowPositionSnapshot,
};
use crate::xwayland_satellite::{SatelliteInstance, satellite_requested, spawn_satellite};
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
        RuntimeOutputConfig, RuntimeOutputMode, RuntimeOutputPositionPreference,
    },
    cursor::Cursor,
    drawing::PointerElement,
};
use tracing::{debug, info, warn};

fn runtime_dirty_debug_enabled() -> bool {
    use std::sync::OnceLock;

    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var_os("SHOJI_RUNTIME_DIRTY_DEBUG")
            .or_else(|| std::env::var_os("SHOJI_SSD_SUPPRESSION_DEBUG"))
            .is_some_and(|value| value != "0" && !value.is_empty())
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnedDamageRect {
    pub owner: String,
    pub rect: LogicalRect,
}

#[derive(Debug, Clone)]
pub struct RuntimeGestureSwipeState {
    pub fingers: u32,
    pub total_x: f64,
    pub total_y: f64,
    pub last_timestamp: u64,
    pub velocity_x: f64,
    pub velocity_y: f64,
}

/// Per-surface marker tracking whether we have already reinterpreted the cursor
/// hotspot for an oversized cursor buffer (Xwayland HiDPI workaround). Reset in
/// `SeatHandler::cursor_image` whenever a new cursor surface is set, applied at
/// most once per set_cursor cycle in the commit handler.
#[derive(Debug, Default)]
pub struct CursorOverrideApplied {
    pub applied: bool,
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
pub struct TrackedDecorationInteractionTarget {
    pub window: Window,
    pub window_id: String,
    pub target: DecorationInteractionTarget,
}

impl TrackedDecorationInteractionTarget {
    pub fn same_node(&self, other: &Self) -> bool {
        self.window_id == other.window_id && self.target.node_id == other.target.node_id
    }
}

#[derive(Clone)]
pub struct PopupLifecycleDebugEntry {
    pub surface: WlSurface,
    pub root_surface_id: Option<u32>,
    pub kind: &'static str,
    pub tracked_at: Duration,
}

#[derive(Debug, Clone)]
pub struct ActiveManagedWindowAnimation {
    pub sequence: u64,
    pub started_at_ms: u64,
    pub animation: ManagedWindowAnimationSnapshot,
}

#[derive(Clone)]
pub struct PendingLayerSurface {
    pub output: Output,
    pub layer: LayerSurface,
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
    pub loop_handle: LoopHandle<'static, ShojiWM>,

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
    pub screencopy_state: crate::protocols::screencopy::ScreencopyManagerState,
    pub tearing_control_state: crate::protocols::tearing_control::TearingControlManagerState,
    pub foreign_toplevel_list_state:
        smithay::wayland::foreign_toplevel_list::ForeignToplevelListState,
    pub wlr_foreign_toplevel_manager_state:
        crate::wlr_foreign_toplevel::WlrForeignToplevelManagerState,
    pub ext_workspace_manager_state: crate::workspace_manager::ExtWorkspaceManagerState,
    pub image_capture_source_state: smithay::wayland::image_capture_source::ImageCaptureSourceState,
    pub output_capture_source_state:
        smithay::wayland::image_capture_source::OutputCaptureSourceState,
    pub toplevel_capture_source_state:
        smithay::wayland::image_capture_source::ToplevelCaptureSourceState,
    pub image_copy_capture_state: smithay::wayland::image_copy_capture::ImageCopyCaptureState,
    pub idle_notifier_state: IdleNotifierState<ShojiWM>,
    pub idle_inhibit_manager_state: IdleInhibitManagerState,
    pub idle_inhibited_surfaces: Vec<WlSurface>,
    pub active_idle_inhibit_labels: Vec<String>,
    pub session_lock_state: SessionLockManagerState,
    pub session_lock_active: bool,
    pub session_lock_surfaces: HashMap<String, LockSurface>,
    /// Live capture sessions, kept alive so dropping doesn't auto-send
    /// `stopped` to clients. Keyed by source id.
    pub image_copy_capture_sessions:
        std::collections::HashMap<usize, Vec<smithay::wayland::image_copy_capture::Session>>,
    /// Capture frames the client has requested, waiting to be rendered into
    /// during the next backend frame. Drained per-output / per-toplevel by
    /// the render path.
    pub image_copy_capture_pending: Vec<crate::backend::image_copy_capture_render::PendingCapture>,
    pub single_pixel_buffer_state: SinglePixelBufferState,
    pub fixes_state: FixesState,
    pub seat_state: SeatState<ShojiWM>,
    pub data_device_state: DataDeviceState,
    pub primary_selection_state: PrimarySelectionState,
    pub data_control_state: DataControlState,
    pub popups: PopupManager,

    pub seat: Seat<Self>,

    pub tty_backends: HashMap<DrmNode, BackendData>,
    pub tty_session: Option<LibSeatSession>,
    pub window_decorations: HashMap<Window, WindowDecorationState>,
    pub window_decoration_negotiations: crate::window_decoration::WindowDecorationNegotiationMap,
    pub window_primary_output_names: HashMap<Window, String>,
    pub windows_ready_for_decoration: HashSet<String>,
    pub pending_xdg_state_configure_window_ids: HashSet<String>,
    pub live_window_snapshots: HashMap<String, LiveWindowSnapshot>,
    pub live_window_snapshot_trackers:
        HashMap<String, smithay::backend::renderer::damage::OutputDamageTracker>,
    pub complete_window_snapshots: HashMap<String, LiveWindowSnapshot>,
    pub complete_window_snapshot_trackers:
        HashMap<String, smithay::backend::renderer::damage::OutputDamageTracker>,
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
    pub damage_blink_capture_suppression: HashMap<String, u8>,
    pub runtime_poll_dirty: bool,
    pub runtime_dirty_window_ids: std::collections::HashSet<String>,
    pub runtime_managed_only_window_ids: std::collections::HashSet<String>,
    pub runtime_node_only_window_ids: std::collections::HashSet<String>,
    pub runtime_scheduler_enabled: bool,
    pub runtime_scheduler_kick_generation: u64,
    pub runtime_scheduler_kick_active: bool,
    pub runtime_scheduler_kick_interval_ms: Option<u64>,
    pub runtime_animation_outputs: std::collections::HashSet<String>,
    pub runtime_output_globals: HashMap<String, GlobalId>,
    pub managed_window_animations: HashMap<String, BTreeMap<String, ActiveManagedWindowAnimation>>,
    pub managed_window_animation_sequence: u64,
    pub runtime_output_configs: std::collections::BTreeMap<String, RuntimeOutputConfig>,
    pub runtime_process_config_generation: u64,
    pub runtime_process_supervision_enabled: bool,
    pub runtime_process_entries: BTreeMap<String, RuntimeProcessEntry>,
    pub runtime_process_once_runs: HashMap<String, u64>,
    pub runtime_process_suppressed_services: HashMap<String, u64>,
    pub runtime_managed_services: BTreeMap<String, ManagedRuntimeService>,
    pub runtime_key_binding_entries: BTreeMap<String, RuntimeKeyBindingEntry>,
    pub runtime_key_bindings: Vec<CompiledRuntimeKeyBinding>,
    pub runtime_window_move_modifier: Option<RuntimePointerModifier>,
    pub runtime_window_resize_modifier: Option<RuntimePointerModifier>,
    pub runtime_input_config: RuntimeInputConfig,
    pub runtime_applied_xkb_config: Option<crate::runtime_input::RuntimeKeyboardInputConfig>,
    pub runtime_active_keyboard_device: Option<RuntimeInputDeviceSnapshot>,
    pub runtime_input_devices: BTreeMap<String, RuntimeInputDeviceSnapshot>,
    pub runtime_libinput_devices: HashMap<String, input::Device>,
    pub runtime_pointer_move_enabled: bool,
    pub runtime_pointer_move_async_enabled: bool,
    pub runtime_gesture_swipe_enabled: bool,
    pub runtime_gesture_swipe_async_enabled: bool,
    pub runtime_gesture_swipe: Option<RuntimeGestureSwipeState>,
    pub current_keyboard_modifiers: ModifiersState,
    // Modifier-only tap detection (e.g. "Super"). If no other input occurs
    // between press and release, the release is treated as a tap.
    pub tap_pressed_keys: u32,
    pub tap_armed_modifier: Option<crate::runtime_key_binding::ModifierClass>,
    pub tap_interrupted: bool,
    pub pending_tap_binding_ids: Vec<String>,
    pub suggested_window_offset: Option<(i32, i32)>,
    pub async_asset_dirty: bool,
    pub configured_background_effect: Option<BackgroundEffectConfig>,
    pub configured_layer_effects: HashMap<String, crate::ssd::WindowEffectConfig>,
    pub configured_popup_effects: HashMap<String, crate::ssd::WindowEffectConfig>,
    /// Per-popup surface policies from `COMPOSITOR.rendering.surfacePolicy`,
    /// keyed by popup runtime id. Maintained alongside popup effects.
    pub configured_popup_surface_policies: HashMap<String, crate::ssd::SurfacePolicy>,
    pub layer_effect_evaluation_cache: HashMap<String, crate::ssd::EffectEvaluationCacheEntry>,
    pub popup_effect_evaluation_cache: HashMap<String, crate::ssd::EffectEvaluationCacheEntry>,
    pub config_error_report: Option<crate::config_error::ConfigErrorReport>,
    pub layer_backdrop_cache: HashMap<String, crate::backend::shader_effect::CachedBackdropTexture>,
    pub layer_framebuffer_effect_states:
        HashMap<String, crate::backend::shader_effect::ShaderEffectElementState>,
    pub layer_effect_cache:
        HashMap<String, crate::backend::shader_effect::WindowEffectElementState>,
    pub popup_effect_cache:
        HashMap<String, crate::backend::shader_effect::WindowEffectElementState>,
    pub popup_framebuffer_effect_states:
        HashMap<String, crate::backend::shader_effect::ShaderEffectElementState>,
    pub output_capture_mirrors: HashMap<String, crate::backend::tty::OutputCaptureMirror>,
    pub pointer_contents: PointerContents,
    pub decoration_hover_target: Option<TrackedDecorationInteractionTarget>,
    pub decoration_active_target: Option<TrackedDecorationInteractionTarget>,
    pub layer_shell_on_demand_focus: Option<LayerSurface>,
    pub pending_layer_surfaces: Vec<PendingLayerSurface>,
    pub pending_initial_focus_window_ids: HashSet<String>,
    pub window_keyboard_focus_owner: Option<WlSurface>,
    pub window_keyboard_focus: Option<WlSurface>,
    pub mapped_on_demand_layer_surfaces: HashSet<u32>,
    pub force_full_damage: bool,
    pub debug_previous_scene_signatures: HashMap<String, Vec<String>>,
    pub tty_maintenance_pending: bool,
    pub tty_maintenance_reasons: BTreeSet<&'static str>,
    pub redraw_reason_counts: HashMap<String, u64>,
    pub last_redraw_stats_log_at: Instant,
    pub event_source_wake_counts: BTreeMap<&'static str, u64>,
    pub wayland_display_dispatched_request_count: u64,
    pub popup_latency_debug: Option<PopupLatencyDebugState>,
    pub popup_lifecycle_debug_entries: BTreeMap<u32, PopupLifecycleDebugEntry>,
    pub right_click_debug: RightClickDebugState,
    pub tty_session_active: bool,

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
    pub display_config: DisplayConfig,
    pub clock: Clock<Monotonic>,
    pub fps_counter: crate::backend::fps_counter::FpsCounter,

    pub xwayland_shell_state: XWaylandShellState,
    pub xwayland: Option<XWayland>,
    pub xwm: Option<X11Wm>,
    pub xdisplay: Option<u32>,
    pub xwayland_satellite: Option<SatelliteInstance>,
    /// Refresh rate advertised to Xwayland-like clients through `wl_output.mode`.
    ///
    /// This is intentionally compositor-local state rather than true output state. Native
    /// Wayland clients see each output's real mode, while Xwayland/Xwayland-satellite clients
    /// get a single selected refresh value that follows the output where the X11 window is
    /// expected to run.
    pub xwayland_refresh_override_mhz: Arc<AtomicI32>,
}

fn logical_rect_intersects_output(rect: LogicalRect, output_geo: Rectangle<i32, Logical>) -> bool {
    let left = rect.x.max(output_geo.loc.x);
    let top = rect.y.max(output_geo.loc.y);
    let right = (rect.x + rect.width).min(output_geo.loc.x + output_geo.size.w);
    let bottom = (rect.y + rect.height).min(output_geo.loc.y + output_geo.size.h);
    right > left && bottom > top
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
        let surface = self
            .session_lock_surface_under(pos)
            .or_else(|| self.surface_under(pos));
        let layer = surface
            .as_ref()
            .and_then(|(surface, _)| self.layer_surface_for_hit_surface(surface));
        PointerContents { surface, layer }
    }

    pub fn session_lock_surface_for_output(&self, output: &Output) -> Option<LockSurface> {
        self.session_lock_surfaces
            .get(output.name().as_str())
            .cloned()
    }

    pub fn session_lock_surface_under(
        &self,
        pos: Point<f64, Logical>,
    ) -> Option<(WlSurface, Point<f64, Logical>)> {
        if !self.session_lock_active {
            return None;
        }

        let output = self.space.outputs().find(|output| {
            self.space
                .output_geometry(output)
                .is_some_and(|geometry| geometry.contains(pos.to_i32_round()))
        })?;
        let output_geo = self.space.output_geometry(output)?;
        let lock_surface = self.session_lock_surface_for_output(output)?;
        if !lock_surface.alive() {
            return None;
        }

        under_from_surface_tree(
            lock_surface.wl_surface(),
            pos - output_geo.loc.to_f64(),
            (0, 0),
            WindowSurfaceType::ALL,
        )
        .map(|(surface, loc)| (surface, (loc + output_geo.loc).to_f64()))
    }

    pub fn is_session_lock_surface(&self, surface: &WlSurface) -> bool {
        self.session_lock_surfaces
            .values()
            .any(|lock_surface| lock_surface.wl_surface() == surface)
    }

    pub fn is_session_lock_surface_tree_surface(&self, surface: &WlSurface) -> bool {
        let mut root = surface.clone();
        while let Some(parent) = get_parent(&root) {
            root = parent;
        }
        self.is_session_lock_surface(&root)
    }

    pub fn refresh_idle_inhibit_state(&mut self) {
        let mut live_surfaces = Vec::with_capacity(self.idle_inhibited_surfaces.len());
        let mut active_labels = BTreeSet::new();
        for surface in self.idle_inhibited_surfaces.iter() {
            if !surface.alive() {
                continue;
            }
            if let Some(label) = self.idle_inhibit_surface_visible_label(surface) {
                active_labels.insert(label);
            }
            live_surfaces.push(surface.clone());
        }
        let active_labels = active_labels.into_iter().collect::<Vec<_>>();
        let was_inhibited = !self.active_idle_inhibit_labels.is_empty();
        let inhibited = !active_labels.is_empty();
        if inhibited && !was_inhibited {
            info!(
                apps = ?active_labels,
                "idle inhibition started"
            );
        } else if !inhibited && was_inhibited {
            info!(
                apps = ?self.active_idle_inhibit_labels,
                "idle inhibition stopped"
            );
        }
        self.idle_inhibited_surfaces = live_surfaces;
        self.active_idle_inhibit_labels = active_labels;
        self.idle_notifier_state.set_is_inhibited(inhibited);
    }

    fn idle_inhibit_surface_root(&self, surface: &WlSurface) -> WlSurface {
        let mut root = surface.clone();
        while let Some(parent) = get_parent(&root) {
            root = parent;
        }
        root
    }

    fn idle_inhibit_surface_visible_label(&self, surface: &WlSurface) -> Option<String> {
        let root = self.idle_inhibit_surface_root(surface);
        if let Some(label) = self.space.elements().find_map(|window| {
            let owns_root = window
                .toplevel()
                .is_some_and(|toplevel| toplevel.wl_surface() == &root)
                || window
                    .x11_surface()
                    .and_then(|x11| x11.wl_surface())
                    .as_ref()
                    == Some(&root);
            if !(owns_root && self.window_allows_render(window)) {
                return None;
            }
            let snapshot = self.snapshot_window(window);
            snapshot
                .app_id
                .filter(|app_id| !app_id.is_empty())
                .or_else(|| (!snapshot.title.is_empty()).then_some(snapshot.title))
        }) {
            return Some(label);
        }

        if let Some(label) = self.space.outputs().find_map(|output| {
            let layers = layer_map_for_output(output);
            let layer = layers.layer_for_surface(&root, WindowSurfaceType::TOPLEVEL)?;
            if !layer.alive()
                || !crate::backend::window::layer_surface_is_mapped(layer)
                || layers.layer_geometry(layer).is_none()
            {
                return None;
            }
            let namespace = layer.namespace();
            Some(if namespace.is_empty() {
                "layer-shell".to_string()
            } else {
                namespace.to_string()
            })
        }) {
            return Some(label);
        }

        None
    }

    pub fn output_for_lock_resource(
        &self,
        surface: &LockSurface,
        wl_output: &smithay::reexports::wayland_server::protocol::wl_output::WlOutput,
    ) -> Option<Output> {
        let client = surface.wl_surface().client()?;
        self.space.outputs().find_map(|output| {
            output
                .client_outputs(&client)
                .any(|client_output| &client_output == wl_output)
                .then(|| output.clone())
        })
    }

    pub fn configure_session_lock_surface_for_output(&self, output: &Output) {
        let Some(surface) = self.session_lock_surface_for_output(output) else {
            return;
        };
        let Some(output_geo) = self.space.output_geometry(output) else {
            return;
        };
        surface.with_pending_state(|state| {
            state.size = Some((output_geo.size.w as u32, output_geo.size.h as u32).into());
        });
        surface.send_configure();
    }

    pub fn configure_session_lock_surfaces(&self) {
        for output in self.space.outputs() {
            self.configure_session_lock_surface_for_output(output);
        }
    }

    pub fn focus_session_lock_surface(&mut self, serial: smithay::utils::Serial) {
        if !self.session_lock_active {
            return;
        }

        let focus = self
            .seat
            .get_pointer()
            .and_then(|pointer| {
                self.session_lock_surface_under(pointer.current_location())
                    .map(|(surface, _)| surface)
            })
            .or_else(|| {
                self.space
                    .outputs()
                    .find_map(|output| self.session_lock_surface_for_output(output))
                    .map(|surface| surface.wl_surface().clone())
            });

        if let Some(keyboard) = self.seat.get_keyboard() {
            if keyboard.current_focus().as_ref() != focus.as_ref() {
                keyboard.set_focus(self, focus, serial);
            }
        }
    }

    fn window_root_surface(window: &Window) -> Option<WlSurface> {
        if let Some(toplevel) = window.toplevel() {
            return Some(toplevel.wl_surface().clone());
        }
        window.x11_surface().and_then(|x11| x11.wl_surface())
    }

    fn keyboard_focus_surface_root(surface: &WlSurface) -> WlSurface {
        let mut root = surface.clone();
        while let Some(parent) = smithay::wayland::compositor::get_parent(&root) {
            root = parent;
        }
        root
    }

    fn window_matches_root_surface(window: &Window, root: &WlSurface) -> bool {
        window
            .toplevel()
            .is_some_and(|toplevel| toplevel.wl_surface() == root)
            || window
                .x11_surface()
                .and_then(|x11| x11.wl_surface())
                .as_ref()
                == Some(root)
    }

    pub fn surface_belongs_to_window(&self, window: &Window, surface: &WlSurface) -> bool {
        let mut root = Self::keyboard_focus_surface_root(surface);
        if let Some(popup_root) = self
            .popups
            .find_popup(surface)
            .or_else(|| self.popups.find_popup(&root))
            .and_then(|popup| find_popup_root_surface(&popup).ok())
        {
            root = popup_root;
        }
        Self::window_matches_root_surface(window, &root)
    }

    pub fn set_window_keyboard_focus_target(&mut self, window: Option<&Window>) {
        if let Some(window) = window {
            self.set_window_keyboard_focus_target_surface(window, None);
        } else {
            self.window_keyboard_focus_owner = None;
            self.window_keyboard_focus = None;
        }
    }

    pub fn set_window_keyboard_focus_target_surface(
        &mut self,
        window: &Window,
        _surface: Option<&WlSurface>,
    ) {
        let root = Self::window_root_surface(window);

        self.window_keyboard_focus_owner = root;
        // Popup keyboard grabs temporarily focus popup surfaces themselves.
        // The compositor's persistent window focus target should remain the
        // root surface so short-lived child surfaces do not steal text input.
        self.window_keyboard_focus = self.window_keyboard_focus_owner.clone();
    }

    pub fn sync_window_keyboard_focus_from_surface(&mut self, surface: Option<&WlSurface>) {
        let Some(surface) = surface else {
            self.window_keyboard_focus_owner = None;
            self.window_keyboard_focus = None;
            return;
        };

        let owner_root = self
            .space
            .elements()
            .find(|window| self.surface_belongs_to_window(window, surface))
            .and_then(Self::window_root_surface);
        if let Some(owner_root) = owner_root {
            self.window_keyboard_focus_owner = Some(owner_root.clone());
            self.window_keyboard_focus = Some(owner_root);
        }
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
            let owner_root = self.window_keyboard_focus_owner.clone().or_else(|| {
                self.space
                    .elements()
                    .find(|window| self.surface_belongs_to_window(window, surface))
                    .and_then(Self::window_root_surface)
            });
            let owner_still_mapped = owner_root.as_ref().is_some_and(|owner| {
                self.space
                    .elements()
                    .any(|window| Self::window_matches_root_surface(window, owner))
            });
            let focus_still_valid = surface.alive()
                && self
                    .space
                    .elements()
                    .any(|window| self.surface_belongs_to_window(window, surface));

            if owner_still_mapped {
                if !focus_still_valid {
                    self.window_keyboard_focus = owner_root.clone();
                }
                self.window_keyboard_focus_owner = owner_root;
            } else {
                self.window_keyboard_focus_owner = None;
                self.window_keyboard_focus = None;
            }
        }
    }

    pub fn update_keyboard_focus(&mut self, serial: smithay::utils::Serial) {
        if self.session_lock_active {
            self.focus_session_lock_surface(serial);
            return;
        }

        self.prune_keyboard_focus_targets();

        let exclusive_layer_focus = self.exclusive_layer_focus_surface();
        let on_demand_layer_focus = exclusive_layer_focus
            .is_none()
            .then(|| {
                self.layer_shell_on_demand_focus
                    .as_ref()
                    .map(|layer| layer.wl_surface().clone())
            })
            .flatten();
        let layer_has_focus = exclusive_layer_focus.is_some() || on_demand_layer_focus.is_some();
        let desired_focus = exclusive_layer_focus
            .or(on_demand_layer_focus)
            .or_else(|| self.window_keyboard_focus.clone());

        let focused_window_surface = (!layer_has_focus)
            .then(|| {
                self.window_keyboard_focus_owner
                    .as_ref()
                    .or(desired_focus.as_ref())
            })
            .flatten();

        for candidate in self.space.elements() {
            let should_activate = if let Some(toplevel) = candidate.toplevel() {
                focused_window_surface.is_some_and(|surface| toplevel.wl_surface() == surface)
            } else if let Some(x11) = candidate.x11_surface() {
                match (focused_window_surface, x11.wl_surface()) {
                    (Some(focused), Some(wl)) => focused == &wl,
                    _ => false,
                }
            } else {
                false
            };
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
            if self.should_defer_initial_keyboard_focus(desired_focus.as_ref()) {
                self.schedule_redraw();
            } else {
                self.seat
                    .get_keyboard()
                    .unwrap()
                    .set_focus(self, desired_focus, serial);
                self.schedule_redraw();
            }
        }
    }

    fn should_defer_initial_keyboard_focus(&self, focus: Option<&WlSurface>) -> bool {
        if self.pending_initial_focus_window_ids.is_empty() {
            return false;
        }
        let Some(focus) = focus else {
            return false;
        };

        self.space.elements().any(|window| {
            if !self.surface_belongs_to_window(window, focus) {
                return false;
            }
            let window_id = self.snapshot_window(window).id;
            self.pending_initial_focus_window_ids.contains(&window_id)
                && !self.windows_ready_for_decoration.contains(&window_id)
        })
    }

    pub fn refresh_keyboard_focus_for_keymap_change(&mut self) {
        let Some(keyboard) = self.seat.get_keyboard() else {
            return;
        };
        let Some(focus) = keyboard.current_focus() else {
            return;
        };

        keyboard.set_focus(
            self,
            Option::<WlSurface>::None,
            SERIAL_COUNTER.next_serial(),
        );
        keyboard.set_focus(self, Some(focus), SERIAL_COUNTER.next_serial());
    }

    pub fn new(event_loop: &mut EventLoop<'static, Self>, display: Display<Self>) -> Self {
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
        // The legacy KDE protocol announces this default when a client binds
        // the manager, before any per-window metadata exists. Advertising
        // Server here makes Firefox/Chromium construct an undecorated window
        // up front, and some versions do not rebuild their full CSD after a
        // later per-window Client response. Start from the Wayland-safe CSD
        // baseline; COMPOSITOR.window.decoration.configure can still request
        // SSD for each decoration object once the window is known.
        let kde_decoration_state = KdeDecorationState::new::<Self>(&dh, KdeDecorationMode::Client);
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
        let screencopy_state =
            crate::protocols::screencopy::ScreencopyManagerState::new::<Self, _>(&dh, |_| true);
        let tearing_control_state =
            crate::protocols::tearing_control::TearingControlManagerState::new::<Self>(&dh);
        let foreign_toplevel_list_state =
            smithay::wayland::foreign_toplevel_list::ForeignToplevelListState::new::<Self>(&dh);
        let wlr_foreign_toplevel_manager_state =
            crate::wlr_foreign_toplevel::WlrForeignToplevelManagerState::new::<Self>(&dh);
        let ext_workspace_manager_state =
            crate::workspace_manager::ExtWorkspaceManagerState::new::<Self>(&dh);
        let image_capture_source_state =
            smithay::wayland::image_capture_source::ImageCaptureSourceState::new();
        let output_capture_source_state =
            smithay::wayland::image_capture_source::OutputCaptureSourceState::new::<Self>(&dh);
        let toplevel_capture_source_state =
            smithay::wayland::image_capture_source::ToplevelCaptureSourceState::new::<Self>(&dh);
        let image_copy_capture_state =
            smithay::wayland::image_copy_capture::ImageCopyCaptureState::new::<Self>(&dh);
        let idle_notifier_state = IdleNotifierState::new(&dh, event_loop.handle());
        let idle_inhibit_manager_state = IdleInhibitManagerState::new::<Self>(&dh);
        let session_lock_state = SessionLockManagerState::new::<Self, _>(&dh, |_| true);
        let single_pixel_buffer_state = SinglePixelBufferState::new::<Self>(&dh);
        let fixes_state = FixesState::new::<Self>(&dh);
        let xwayland_shell_state = XWaylandShellState::new::<Self>(&dh);
        TextInputManagerState::new::<Self>(&dh);
        InputMethodManagerState::new::<Self, _>(&dh, |_client| true);
        VirtualKeyboardManagerState::new::<Self, _>(&dh, |_client| true);
        RelativePointerManagerState::new::<Self>(&dh);
        PointerConstraintsState::new::<Self>(&dh);

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
        let loop_handle = event_loop.handle();
        let runtime_paths = crate::install_paths::decoration_runtime_paths();
        let evaluator = EmbeddedDecorationEvaluator::for_paths(
            runtime_paths.script_path,
            runtime_paths.config_path,
        )
        .with_working_dir(runtime_paths.working_dir);
        let config_error_report = match evaluator.preload() {
            Ok(()) => None,
            Err(error) => {
                warn!(?error, "failed to preload TypeScript config");
                Some(crate::config_error::ConfigErrorReport::initial_load(error))
            }
        };
        let decoration_evaluator = DecorationRuntimeEvaluator::Embedded(evaluator);
        let (runtime_async_event_tx, runtime_async_event_rx) = channel();
        decoration_evaluator.set_async_event_sender(runtime_async_event_tx);
        let runtime_async_loop_handle = event_loop.handle();
        let runtime_scheduler_kick_loop_handle = runtime_async_loop_handle.clone();
        runtime_async_loop_handle
            .insert_source(runtime_async_event_rx, move |event, _, state| match event {
                ChannelEvent::Msg(invocation) => match invocation {
                    DecorationRuntimeAsyncInvocation::PointerMove(invocation)
                    | DecorationRuntimeAsyncInvocation::GestureSwipe(invocation) => {
                        state.handle_runtime_pointer_move_async_invocation(
                            invocation,
                            &runtime_scheduler_kick_loop_handle,
                        );
                    }
                    DecorationRuntimeAsyncInvocation::CursorConfig(update) => {
                        state.apply_runtime_cursor_config_update(update);
                    }
                },
                ChannelEvent::Closed => {}
            })
            .expect("Failed to init runtime async event source.");

        // Register a SIGUSR1 source so the embedded runtime can wake the event
        // loop after handling an IPC request.
        Self::register_runtime_wake_signal(event_loop);

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
                    let mut should_redraw = false;
                    match result {
                        AsyncAssetResult::TextReady {
                            spec_hash,
                            width,
                            height,
                            raster_scale,
                            pixels,
                        } => {
                            state.text_rasterizer.handle_async_ready(
                                spec_hash,
                                width,
                                height,
                                raster_scale,
                                pixels,
                            );
                            should_redraw = true;
                        }
                        AsyncAssetResult::TextMissing { spec_hash } => {
                            state.text_rasterizer.handle_async_miss(spec_hash)
                        }
                        AsyncAssetResult::IconReady {
                            spec_hash,
                            width,
                            height,
                            raster_scale,
                            pixels,
                        } => {
                            state.icon_rasterizer.handle_async_ready(
                                spec_hash,
                                width,
                                height,
                                raster_scale,
                                pixels,
                            );
                            should_redraw = true;
                        }
                        AsyncAssetResult::IconMissing { spec_hash } => {
                            state.icon_rasterizer.handle_async_miss(spec_hash)
                        }
                    }
                    if should_redraw {
                        state.async_asset_dirty = true;
                        state.schedule_redraw();
                    }
                }
                ChannelEvent::Closed => {}
            })
            .expect("Failed to init async asset worker.");

        let state = Self {
            start_time,
            display_handle: dh,

            space,
            loop_signal,
            loop_handle,
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
            screencopy_state,
            tearing_control_state,
            foreign_toplevel_list_state,
            wlr_foreign_toplevel_manager_state,
            ext_workspace_manager_state,
            image_capture_source_state,
            output_capture_source_state,
            toplevel_capture_source_state,
            image_copy_capture_state,
            idle_notifier_state,
            idle_inhibit_manager_state,
            idle_inhibited_surfaces: Vec::new(),
            active_idle_inhibit_labels: Vec::new(),
            session_lock_state,
            session_lock_active: false,
            session_lock_surfaces: HashMap::new(),
            image_copy_capture_sessions: std::collections::HashMap::new(),
            image_copy_capture_pending: Vec::new(),
            single_pixel_buffer_state,
            fixes_state,
            seat_state,
            data_device_state,
            primary_selection_state,
            data_control_state,
            popups,
            seat,

            tty_backends: HashMap::new(),
            tty_session: None,
            window_decorations: HashMap::new(),
            window_decoration_negotiations: HashMap::new(),
            window_primary_output_names: HashMap::new(),
            windows_ready_for_decoration: HashSet::new(),
            pending_xdg_state_configure_window_ids: HashSet::new(),
            live_window_snapshots: HashMap::new(),
            live_window_snapshot_trackers: HashMap::new(),
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
            damage_blink_capture_suppression: HashMap::new(),
            runtime_poll_dirty: false,
            runtime_dirty_window_ids: Default::default(),
            runtime_managed_only_window_ids: Default::default(),
            runtime_node_only_window_ids: Default::default(),
            runtime_scheduler_enabled: false,
            runtime_scheduler_kick_generation: 0,
            runtime_scheduler_kick_active: false,
            runtime_scheduler_kick_interval_ms: None,
            runtime_animation_outputs: Default::default(),
            runtime_output_globals: Default::default(),
            managed_window_animations: Default::default(),
            managed_window_animation_sequence: 0,
            runtime_output_configs: Default::default(),
            runtime_process_config_generation: 0,
            runtime_process_supervision_enabled: false,
            runtime_process_entries: Default::default(),
            runtime_process_once_runs: Default::default(),
            runtime_process_suppressed_services: Default::default(),
            runtime_managed_services: Default::default(),
            runtime_key_binding_entries: Default::default(),
            runtime_key_bindings: Vec::new(),
            runtime_window_move_modifier: None,
            runtime_window_resize_modifier: None,
            runtime_input_config: Default::default(),
            runtime_applied_xkb_config: None,
            runtime_active_keyboard_device: None,
            runtime_input_devices: Default::default(),
            runtime_libinput_devices: Default::default(),
            runtime_pointer_move_enabled: false,
            runtime_pointer_move_async_enabled: false,
            runtime_gesture_swipe_enabled: false,
            runtime_gesture_swipe_async_enabled: false,
            runtime_gesture_swipe: None,
            current_keyboard_modifiers: ModifiersState::default(),
            tap_pressed_keys: 0,
            tap_armed_modifier: None,
            tap_interrupted: false,
            pending_tap_binding_ids: Vec::new(),
            suggested_window_offset: None,
            async_asset_dirty: false,
            configured_background_effect: None,
            configured_layer_effects: HashMap::new(),
            configured_popup_effects: HashMap::new(),
            configured_popup_surface_policies: HashMap::new(),
            layer_effect_evaluation_cache: HashMap::new(),
            popup_effect_evaluation_cache: HashMap::new(),
            config_error_report,
            layer_backdrop_cache: HashMap::new(),
            layer_framebuffer_effect_states: HashMap::new(),
            layer_effect_cache: HashMap::new(),
            popup_effect_cache: HashMap::new(),
            popup_framebuffer_effect_states: HashMap::new(),
            output_capture_mirrors: HashMap::new(),
            pointer_contents: PointerContents::default(),
            decoration_hover_target: None,
            decoration_active_target: None,
            layer_shell_on_demand_focus: None,
            pending_layer_surfaces: Vec::new(),
            pending_initial_focus_window_ids: HashSet::new(),
            window_keyboard_focus_owner: None,
            window_keyboard_focus: None,
            mapped_on_demand_layer_surfaces: Default::default(),
            force_full_damage,
            debug_previous_scene_signatures: HashMap::new(),
            tty_maintenance_pending: true,
            tty_maintenance_reasons: BTreeSet::new(),
            redraw_reason_counts: HashMap::new(),
            last_redraw_stats_log_at: start_time,
            event_source_wake_counts: BTreeMap::new(),
            wayland_display_dispatched_request_count: 0,
            popup_latency_debug: None,
            popup_lifecycle_debug_entries: BTreeMap::new(),
            right_click_debug: RightClickDebugState {
                pressed_at: None,
                released_at: None,
                location: None,
            },
            tty_session_active: true,

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
            display_config: DisplayConfig::from_env(),
            clock,
            fps_counter: crate::backend::fps_counter::FpsCounter::new(),

            xwayland_shell_state,
            xwayland: None,
            xwm: None,
            xdisplay: None,
            xwayland_satellite: None,
            xwayland_refresh_override_mhz: Arc::new(AtomicI32::new(0)),
        };

        state
    }

    pub fn create_output_global(&mut self, output: &Output) -> GlobalId {
        let output_name = output.name();
        if let Some(global) = self.runtime_output_globals.get(&output_name) {
            return global.clone();
        }

        // Xwayland currently treats `wl_output.mode.refresh` more like process-global timing
        // input than per-surface/per-output state. On mixed-Hz setups this can make GLX/EGL X11
        // clients pace themselves to the wrong monitor even when the Wayland surface has entered
        // the high-Hz output.
        //
        // The Smithay fork exposes a narrow compatibility hook for this: when a client is known
        // to be Xwayland or an Xwayland bridge, advertise the refresh rate of the output selected
        // by ShojiWM's window placement/focus logic. Do not apply this to ordinary Wayland
        // clients; they must continue to see the real mode of every output.
        let refresh_override_mhz = self.xwayland_refresh_override_mhz.clone();
        let global = output.create_global_with_mode_refresh_override::<ShojiWM, _>(
            &self.display_handle,
            move |client| {
                let is_builtin_xwayland = client
                    .get_data::<smithay::xwayland::XWaylandClientData>()
                    .is_some();
                let is_xwayland_bridge = client
                    .get_data::<ClientState>()
                    .is_some_and(|data| data.xwayland_refresh_override);
                if !(is_builtin_xwayland || is_xwayland_bridge) {
                    return None;
                }

                let refresh = refresh_override_mhz.load(Ordering::Acquire);
                (refresh > 0).then_some(refresh)
            },
        );
        self.runtime_output_globals
            .insert(output_name, global.clone());
        global
    }

    pub(crate) fn remove_output_global(&mut self, output: &Output) {
        let output_name = output.name();
        let Some(global) = self.runtime_output_globals.remove(&output_name) else {
            return;
        };

        // Layer-shell clients bind their windows to a specific output. Close and unmap those
        // surfaces before withdrawing wl_output so clients can tear down their windows while the
        // output resource is still valid. Hyprland and niri use the same ordering on hot-unplug.
        self.close_layer_surfaces_for_output(output);

        // `wl_surface.leave` must precede removal of the `wl_output` global. Clients such as
        // GTK/GDK use that ordering to detach their remaining surfaces from the disappearing
        // monitor before invalidating the monitor object itself.
        output.leave_all();

        // Announce global removal immediately, but retain existing wl_output resources briefly.
        // This gives toolkit event loops time to process layer-surface.closed and surface.leave
        // before their monitor object becomes inert.
        self.display_handle.disable_global::<Self>(global.clone());
        let deferred_global = global.clone();
        if let Err(error) = self.loop_handle.insert_source(
            Timer::from_duration(Duration::from_secs(10)),
            move |_, _, state| {
                state
                    .display_handle
                    .remove_global::<Self>(deferred_global.clone());
                TimeoutAction::Drop
            },
        ) {
            warn!(?error, output = %output_name, "failed to defer output global removal");
            self.display_handle.remove_global::<Self>(global);
        }
    }

    pub fn seed_xwayland_refresh_override_from_output(
        &self,
        output: &Output,
        reason: &'static str,
    ) {
        // Provide a deterministic non-zero fallback before any X11 window exists. This value is
        // replaced as soon as Xwayland starts near the pointer or an X11/Xwayland-bridge window is
        // mapped/focused/moved.
        let Some(mode) = output.current_mode() else {
            return;
        };
        if self
            .xwayland_refresh_override_mhz
            .compare_exchange(0, mode.refresh, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            info!(
                output = %output.name(),
                refresh_mhz = mode.refresh,
                reason,
                "seeded xwayland refresh override"
            );
        }
    }

    pub fn update_xwayland_refresh_override_for_window(
        &mut self,
        window: &Window,
        reason: &'static str,
    ) {
        // Keep the override scoped to windows that actually go through Xwayland. For satellite,
        // the visible toplevel is an xdg_toplevel from a bridge process, so client data rather
        // than `window.x11_surface()` is the reliable marker.
        if !self.window_uses_xwayland_refresh_override(window) {
            return;
        }
        let Some(output) = self.output_for_window(window) else {
            return;
        };
        self.update_xwayland_refresh_override_for_output(&output, reason);
    }

    pub fn update_xwayland_refresh_override_from_pointer_or_first(&mut self, reason: &'static str) {
        // Before the first X11 window is mapped, the pointer output is the best available signal
        // for where a launcher-triggered Xwayland client will appear. Falling back to the first
        // output keeps the value defined for headless tests / unusual startup ordering.
        let output = self
            .seat
            .get_pointer()
            .and_then(|pointer| self.output_at_point(pointer.current_location()))
            .or_else(|| self.space.outputs().next().cloned());
        if let Some(output) = output {
            self.update_xwayland_refresh_override_for_output(&output, reason);
        }
    }

    fn update_xwayland_refresh_override_for_output(
        &mut self,
        output: &Output,
        reason: &'static str,
    ) {
        let Some(mode) = output.current_mode() else {
            return;
        };
        let previous = self
            .xwayland_refresh_override_mhz
            .swap(mode.refresh, Ordering::AcqRel);
        if previous == mode.refresh {
            return;
        }

        // Re-send mode events for every output so already-connected Xwayland clients receive the
        // new compatibility refresh immediately. The callback installed in `create_output_global`
        // rewrites only Xwayland-like clients; native Wayland clients still get the real mode.
        info!(
            output = %output.name(),
            previous_refresh_mhz = previous,
            refresh_mhz = mode.refresh,
            reason,
            "updated xwayland refresh override"
        );

        for output in self.space.outputs() {
            if let Some(mode) = output.current_mode() {
                output.change_current_state(Some(mode), None, None, None);
            }
        }
        let _ = self.display_handle.flush_clients();
    }

    fn window_uses_xwayland_refresh_override(&self, window: &Window) -> bool {
        if window.x11_surface().is_some() {
            return true;
        }
        window
            .toplevel()
            .and_then(|toplevel| toplevel.wl_surface().client())
            .is_some_and(|client| {
                client
                    .get_data::<ClientState>()
                    .is_some_and(|data| data.xwayland_refresh_override)
            })
    }

    pub(crate) fn output_at_point(&self, pos: Point<f64, Logical>) -> Option<Output> {
        self.space
            .outputs()
            .find(|output| {
                self.space
                    .output_geometry(output)
                    .is_some_and(|geometry| geometry.contains(pos.to_i32_round()))
            })
            .cloned()
    }

    fn output_for_window(&self, window: &Window) -> Option<Output> {
        // Prefer largest overlap, because it matches how users perceive "the monitor this window
        // is on" during cross-output moves. The center/first-output fallbacks only cover degenerate
        // geometries and early lifecycle states.
        let rect = self.space.element_bbox(window)?;
        self.space
            .outputs()
            .filter_map(|output| {
                let geometry = self.space.output_geometry(output)?;
                let area = geometry
                    .intersection(rect)
                    .map(|overlap| i64::from(overlap.size.w) * i64::from(overlap.size.h))
                    .unwrap_or(0);
                Some((area, output.clone()))
            })
            .max_by_key(|(area, _)| *area)
            .and_then(|(area, output)| (area > 0).then_some(output))
            .or_else(|| {
                let center = Point::from((
                    f64::from(rect.loc.x) + f64::from(rect.size.w) / 2.0,
                    f64::from(rect.loc.y) + f64::from(rect.size.h) / 2.0,
                ));
                self.output_at_point(center)
            })
            .or_else(|| self.space.outputs().next().cloned())
    }

    pub fn start_xwayland(&mut self, event_loop: &EventLoop<'static, ShojiWM>) {
        // Seed from the pointer before spawning Xwayland. Otherwise Xwayland may bind outputs
        // immediately and cache a low-Hz fallback before the first X11 window exists.
        self.update_xwayland_refresh_override_from_pointer_or_first("xwayland-start");

        if satellite_requested() {
            match spawn_satellite() {
                Ok(instance) => {
                    self.xdisplay = Some(instance.display_number);
                    unsafe {
                        std::env::set_var("DISPLAY", &instance.display_name);
                    }
                    publish_activation_environment("xwayland-satellite-display");
                    info!(
                        display = %instance.display_name,
                        "xwayland-satellite started, DISPLAY exported"
                    );
                    self.xwayland_satellite = Some(instance);
                    return;
                }
                Err(error) => {
                    warn!(
                        ?error,
                        "failed to start xwayland-satellite, falling back to built-in XWayland"
                    );
                }
            }
        }

        use std::process::Stdio;

        let (xwayland, client) = match XWayland::spawn(
            &self.display_handle,
            None,
            std::iter::empty::<(String, String)>(),
            std::iter::empty::<String>(),
            true,
            Stdio::null(),
            Stdio::null(),
            |_| (),
        ) {
            Ok(v) => v,
            Err(err) => {
                warn!(?err, "failed to spawn XWayland");
                return;
            }
        };

        // Publish DISPLAY as soon as the X11 socket is reserved, so any child
        // process spawned before XWayland finishes handshake still inherits it.
        // libX11 will retry connecting briefly until Xwayland accepts.
        let display_number = xwayland.display_number();
        self.xdisplay = Some(display_number);
        unsafe {
            std::env::set_var("DISPLAY", format!(":{}", display_number));
        }
        publish_activation_environment("xwayland-display");
        info!(
            display = display_number,
            "XWayland spawned, DISPLAY exported"
        );

        let display_handle = self.display_handle.clone();
        let loop_handle = event_loop.handle();
        let source_handle = loop_handle.clone();
        let insert_result =
            loop_handle.insert_source(xwayland, move |event, _, state| match event {
                XWaylandEvent::Ready {
                    x11_socket,
                    display_number,
                } => {
                    match X11Wm::start_wm(
                        source_handle.clone(),
                        &display_handle,
                        x11_socket,
                        client.clone(),
                    ) {
                        Ok(wm) => {
                            info!(display = display_number, "XWayland ready, X11Wm started");
                            state.xwm = Some(wm);
                        }
                        Err(err) => {
                            warn!(?err, "failed to start X11 window manager");
                        }
                    }
                }
                XWaylandEvent::Error => {
                    warn!("XWayland exited unexpectedly during startup");
                }
            });

        if let Err(err) = insert_result {
            warn!(?err, "failed to insert XWayland event source");
        }
    }

    fn init_wayland_listener(
        display: Display<ShojiWM>,
        event_loop: &mut EventLoop<'static, Self>,
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
                let client_is_xwayland_bridge = is_xwayland_bridge_client(&client_stream);
                if client_is_xwayland_bridge {
                    // xwayland-satellite connects as an ordinary Wayland client, so Smithay's
                    // built-in XWaylandClientData is not available. Mark it at accept time based
                    // on peer credentials and seed the override before it binds wl_output globals.
                    state.update_xwayland_refresh_override_from_pointer_or_first(
                        "xwayland-bridge-connect",
                    );
                }
                info!(
                    client_is_xwayland_bridge,
                    "accepted new wayland client connection"
                );
                // Inside the callback, you should insert the client into the display.
                //
                // You may also associate some data with the client when inserting the client.
                if let Err(error) = state.display_handle.insert_client(
                    client_stream,
                    Arc::new(ClientState {
                        compositor_state: CompositorClientState::default(),
                        xwayland_refresh_override: client_is_xwayland_bridge,
                    }),
                ) {
                    warn!(
                        ?error,
                        client_is_xwayland_bridge, "failed to insert wayland client"
                    );
                    return;
                }
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

    fn runtime_scheduler_interval_ms(&self, next_poll_in_ms: Option<u64>) -> u64 {
        match next_poll_in_ms {
            Some(0) => self.runtime_frame_sync_interval_ms(),
            Some(ms) => ms.clamp(1, 250),
            None => 250,
        }
    }

    fn tick_runtime_scheduler(&mut self) -> u64 {
        self.tick_runtime_scheduler_with(false)
    }

    /// `force=true` skips the idle-fast-path early returns so the runtime is
    /// actually queried even when no animations / polls are pending. Used by
    /// the runtime wake source: an IPC handler in TS just mutated state and we
    /// need to pull the resulting dirty actions through before the next idle
    /// poll would naturally fire (~250 ms later).
    fn tick_runtime_scheduler_with(&mut self, force: bool) -> u64 {
        self.refresh_runtime_processes();
        let managed_window_animation_active = !self.managed_window_animations.is_empty();
        if !force
            && !self.runtime_scheduler_enabled
            && !self.runtime_process_supervision_enabled
            && !managed_window_animation_active
        {
            return 250;
        }

        if !force
            && managed_window_animation_active
            && !self.runtime_scheduler_enabled
            && !self.runtime_process_supervision_enabled
        {
            self.request_tty_maintenance("managed-window-animation-tick");
            self.schedule_redraw();
            return self.runtime_frame_sync_interval_ms();
        }

        let now_ms = Duration::from(self.clock.now()).as_millis() as u64;
        self.sync_runtime_display_state();
        let tick = match self.decoration_evaluator.scheduler_tick(now_ms) {
            Ok(tick) => tick,
            Err(error) => {
                debug!(?error, "failed to tick decoration runtime scheduler");
                self.config_error_report =
                    Some(crate::config_error::ConfigErrorReport::runtime(error));
                self.schedule_redraw();
                self.runtime_scheduler_enabled = false;
                return 250;
            }
        };
        if tick.dirty {
            if runtime_dirty_debug_enabled() {
                info!(
                    dirty_window_ids = ?tick.dirty_window_ids,
                    runtime_dirty = tick.runtime_dirty,
                    dirty_managed_window_ids = ?tick.dirty_managed_window_ids,
                    dirty_window_node_ids = ?tick.dirty_window_node_ids,
                    dirty_layer_ids = ?tick.dirty_layer_ids,
                    next_poll_in_ms = ?tick.next_poll_in_ms,
                    "runtime dirty debug: scheduler tick dirty"
                );
            }
            if tick.runtime_dirty || !tick.dirty_window_ids.is_empty() {
                self.runtime_poll_dirty = true;
            }
            if tick.runtime_dirty {
                self.layer_effect_evaluation_cache.clear();
                self.popup_effect_evaluation_cache.clear();
            } else if !tick.dirty_layer_ids.is_empty() {
                // Layer signatures are indexed per output rather than per
                // layer. Invalidate that domain only; window animation must
                // not force layer/popup effect evaluation every frame.
                self.layer_effect_evaluation_cache.clear();
            }
            let node_only_window_ids = tick
                .dirty_window_node_ids
                .keys()
                .cloned()
                .collect::<Vec<_>>();
            self.mark_runtime_dirty_windows(tick.dirty_window_ids, tick.dirty_managed_window_ids);
            self.runtime_node_only_window_ids
                .extend(node_only_window_ids);
            self.request_tty_maintenance("runtime-scheduler-dirty");
            self.schedule_redraw();
        }

        self.consume_runtime_display_config(tick.display_config);
        self.consume_runtime_workspace_config(tick.workspace_config);
        self.consume_runtime_key_binding_config(tick.key_binding_config);
        self.consume_runtime_pointer_config(tick.pointer_config);
        self.consume_runtime_input_config(tick.input_config);
        self.consume_runtime_event_config(tick.event_config);
        self.consume_runtime_process_config(tick.process_config);
        self.consume_runtime_debug_config(tick.debug_config);
        if !tick.process_actions.is_empty() {
            self.apply_runtime_process_actions(tick.process_actions);
        }

        if !tick.actions.is_empty() {
            self.request_tty_maintenance("runtime-scheduler-actions");
            self.apply_runtime_window_actions(tick.actions);
            self.schedule_redraw();
        }

        self.runtime_scheduler_enabled = tick.next_poll_in_ms.is_some();
        self.runtime_scheduler_interval_ms(tick.next_poll_in_ms)
    }

    fn schedule_runtime_scheduler_kick(
        &mut self,
        loop_handle: &LoopHandle<'_, Self>,
        next_poll_in_ms: Option<u64>,
    ) {
        if next_poll_in_ms.is_none() {
            return;
        }

        self.runtime_scheduler_kick_generation =
            self.runtime_scheduler_kick_generation.wrapping_add(1);
        let generation = self.runtime_scheduler_kick_generation;
        let initial_interval_ms = self.runtime_scheduler_interval_ms(next_poll_in_ms);
        let insert_result = loop_handle.insert_source(
            Timer::from_duration(Duration::from_millis(initial_interval_ms)),
            move |_, _, state| {
                state.record_event_source_wake("runtime-scheduler-kick");
                if state.runtime_scheduler_kick_generation != generation {
                    return TimeoutAction::Drop;
                }

                // An event-specific runtime response may temporarily report no next poll even
                // while another composition signal remains animated. Do not trust that cached
                // Rust-side bit to terminate this timer: the persistent scheduler timer parks
                // itself for 250 ms while a kick is active, so dropping both creates a visible
                // hand-off gap when pointer motion stops. One forced runtime tick establishes
                // the authoritative global animation state before deciding whether to stop.
                let next_interval_ms = state.tick_runtime_scheduler_with(true);
                if state.runtime_scheduler_kick_generation != generation
                    || !state.runtime_scheduler_enabled
                {
                    if state.runtime_scheduler_kick_generation == generation {
                        state.runtime_scheduler_kick_active = false;
                        state.runtime_scheduler_kick_interval_ms = None;
                    }
                    TimeoutAction::Drop
                } else {
                    state.runtime_scheduler_kick_interval_ms = Some(next_interval_ms);
                    TimeoutAction::ToDuration(Duration::from_millis(next_interval_ms))
                }
            },
        );

        match insert_result {
            Ok(_) => {
                self.runtime_scheduler_kick_active = true;
                self.runtime_scheduler_kick_interval_ms = Some(initial_interval_ms);
            }
            Err(error) => {
                self.runtime_scheduler_kick_active = false;
                self.runtime_scheduler_kick_interval_ms = None;
                debug!(?error, "failed to schedule runtime scheduler kick");
            }
        }
    }

    pub(crate) fn schedule_runtime_scheduler_kick_from_state(
        &mut self,
        next_poll_in_ms: Option<u64>,
    ) {
        let loop_handle = self.loop_handle.clone();
        self.schedule_runtime_scheduler_kick(&loop_handle, next_poll_in_ms);
    }

    fn register_runtime_wake_signal(event_loop: &mut EventLoop<'static, Self>) {
        use calloop::signals::{Signal, Signals};

        let signals = match Signals::new(&[Signal::SIGUSR1]) {
            Ok(s) => s,
            Err(error) => {
                warn!(?error, "failed to create SIGUSR1 source");
                return;
            }
        };
        let insert = event_loop
            .handle()
            .insert_source(signals, |_event, _, state| {
                state.record_event_source_wake("runtime-wake-signal");
                let _ = state.tick_runtime_scheduler_with(true);
            });
        if let Err(error) = insert {
            warn!(?error, "failed to register runtime wake signal source");
        }
    }

    fn init_runtime_scheduler(event_loop: &mut EventLoop<'static, Self>) {
        let loop_handle = event_loop.handle();
        loop_handle
            .insert_source(Timer::immediate(), |_, _, state| {
                state.record_event_source_wake("runtime-scheduler-timer");
                if state.runtime_scheduler_kick_active && state.runtime_scheduler_enabled {
                    state.refresh_runtime_processes();
                    return TimeoutAction::ToDuration(Duration::from_millis(250));
                }
                let next_interval_ms = state.tick_runtime_scheduler();
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
            rect: WindowPositionSnapshot::default(),
            is_focused: false,
            is_floating: true,
            is_maximized: false,
            is_fullscreen: false,
            is_xwayland: false,
            decoration: Default::default(),
            size_constraints: Default::default(),
            is_resizable: true,
            is_transient: false,
            parent_id: None,
            icon: None,
            interaction: DecorationInteractionSnapshot::default(),
        };

        let now_ms = Duration::from(self.clock.now()).as_millis() as u64;
        self.sync_runtime_display_state();
        match self.decoration_evaluator.evaluate_window(&snapshot, now_ms) {
            Ok(result) => {
                self.consume_runtime_display_config(result.display_config);
                self.consume_runtime_workspace_config(result.workspace_config);
                self.consume_runtime_key_binding_config(result.key_binding_config);
                self.consume_runtime_pointer_config(result.pointer_config);
                self.consume_runtime_input_config(result.input_config);
                self.consume_runtime_event_config(result.event_config);
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

    pub fn reload_decoration_runtime(&mut self) {
        let Some(current) = self.decoration_evaluator.as_embedded() else {
            self.config_error_report = Some(crate::config_error::ConfigErrorReport::hot_reload(
                "hot reload is only available for the TypeScript runtime",
            ));
            self.schedule_redraw();
            return;
        };

        self.sync_runtime_display_state();
        let persisted = match current.lifecycle_disable("reload") {
            Ok(state) => state,
            Err(error) => {
                warn!(?error, "failed to collect runtime reload state");
                serde_json::Value::Object(Default::default())
            }
        };

        let next = current.fresh_like();
        if let Err(error) =
            next.lifecycle_enable("reload", Some(&persisted))
                .and_then(|invocation| {
                    self.consume_runtime_lifecycle_invocation(invocation);
                    Ok(())
                })
        {
            warn!(?error, "failed to hot reload TypeScript config");
            self.config_error_report =
                Some(crate::config_error::ConfigErrorReport::hot_reload(error));
            self.schedule_redraw();
            return;
        }

        match next.background_effect_config() {
            Ok(config) => {
                self.configured_background_effect = config;
            }
            Err(error) => {
                warn!(
                    ?error,
                    "failed to load hot-reloaded background effect config"
                );
                self.config_error_report =
                    Some(crate::config_error::ConfigErrorReport::hot_reload(error));
                self.schedule_redraw();
                return;
            }
        }

        self.decoration_evaluator = DecorationRuntimeEvaluator::Embedded(next);
        self.mark_all_window_decoration_policies_reloaded();
        self.config_error_report = None;
        self.runtime_poll_dirty = true;
        let live_window_ids = self
            .space
            .elements()
            .map(|window| self.snapshot_window(window).id)
            .collect::<Vec<_>>();
        self.runtime_node_only_window_ids.clear();
        self.runtime_dirty_window_ids.extend(live_window_ids);
        self.configured_layer_effects.clear();
        self.configured_popup_effects.clear();
        self.configured_popup_surface_policies.clear();
        self.layer_effect_evaluation_cache.clear();
        self.popup_effect_evaluation_cache.clear();
        self.request_tty_maintenance("config-hot-reload");
        self.schedule_redraw();
        info!("hot reloaded TypeScript config");
    }

    pub fn enable_initial_decoration_runtime(&mut self) {
        self.sync_runtime_display_state();
        let lifecycle_result = match self.decoration_evaluator.as_embedded() {
            Some(evaluator) => evaluator.lifecycle_enable("initial", None),
            None => return,
        };
        match lifecycle_result {
            Ok(invocation) => {
                self.consume_runtime_lifecycle_invocation(invocation);
            }
            Err(error) => {
                warn!(?error, "failed to run initial config lifecycle");
                self.config_error_report =
                    Some(crate::config_error::ConfigErrorReport::initial_load(error));
                self.schedule_redraw();
                return;
            }
        }

        let background_effect_result = match self.decoration_evaluator.as_embedded() {
            Some(evaluator) => evaluator.background_effect_config(),
            None => return,
        };
        match background_effect_result {
            Ok(config) => {
                self.configured_background_effect = config;
            }
            Err(error) => {
                warn!(?error, "failed to load configured background effect");
                self.config_error_report =
                    Some(crate::config_error::ConfigErrorReport::initial_load(error));
                self.schedule_redraw();
                return;
            }
        }

        self.config_error_report = None;
        self.runtime_poll_dirty = true;
        self.request_tty_maintenance("config-initial-load");
        self.schedule_redraw();
    }

    pub fn snapshot_outputs(&self) -> std::collections::BTreeMap<String, WaylandOutputSnapshot> {
        self.runtime_connected_outputs()
            .into_iter()
            .map(|output| {
                let name = output.name();
                let physical = output.physical_properties();
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
                    name.clone(),
                    WaylandOutputSnapshot {
                        name: name.clone(),
                        description: Some(output.description()),
                        make: Some(physical.make),
                        model: Some(physical.model),
                        serial: Some(physical.serial_number),
                        connector: Some(name.clone()),
                        enabled: self.runtime_output_workspace_enabled(&name),
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

    fn runtime_connected_outputs(&self) -> Vec<Output> {
        let mut outputs = std::collections::BTreeMap::new();
        for output in self.space.outputs() {
            outputs.insert(output.name(), output.clone());
        }
        for output in tty_connected_outputs(self) {
            outputs.insert(output.name(), output);
        }
        outputs.into_values().collect()
    }

    fn runtime_output_mode_setting(&self, output_name: &str) -> RuntimeOutputMode {
        self.runtime_output_configs
            .get(output_name)
            .map(RuntimeOutputConfig::mode)
            .unwrap_or(RuntimeOutputMode::Extend)
    }

    fn runtime_output_workspace_enabled(&self, output_name: &str) -> bool {
        self.runtime_output_mode_setting(output_name) == RuntimeOutputMode::Extend
    }

    pub fn runtime_output_render_enabled(&self, output_name: &str) -> bool {
        self.runtime_output_mode_setting(output_name) != RuntimeOutputMode::Disabled
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
                // Rank the preferred *resolution* (seeded from the
                // connector's PREFERRED/native timing at connect) above raw
                // pixel area, mirroring select_output_mode in the tty
                // backend: kernel `video=` parameters inject synthetic modes
                // into every connector, and on panels like the UX482
                // ScreenPad (native 1920x515) a synthetic 1920x1080 would
                // otherwise win and drive the panel at a timing it cannot
                // display. Compare sizes only, not the full mode: the
                // PREFERRED timing is typically the native resolution at
                // 60Hz, and a full-mode match would pin such panels to 60Hz
                // even when higher-refresh modes at the same resolution
                // exist. The trailing refresh key picks the fastest one.
                let preferred_size = output.preferred_mode().map(|mode| mode.size);
                modes.into_iter().max_by_key(|mode| {
                    (
                        Some(mode.size) == preferred_size,
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

    fn runtime_output_target_scale_value(&self, output: &Output) -> f64 {
        self.runtime_output_configs
            .get(&output.name())
            .and_then(|config| config.scale)
            .unwrap_or_else(|| output.current_scale().fractional_scale())
            .max(0.1)
    }

    fn runtime_output_logical_width_for_mode(output: &Output, mode: OutputMode, scale: f64) -> i32 {
        let physical_size = output.current_transform().transform_size(mode.size);
        ((physical_size.w as f64) / scale.max(0.1)).round().max(1.0) as i32
    }

    fn resolve_runtime_output_mirror_mode(
        &self,
        output: &Output,
        source_mode: OutputMode,
    ) -> Option<OutputMode> {
        let modes =
            tty_output_available_modes(self, &output.name()).unwrap_or_else(|| output.modes());
        let matching_size = modes
            .into_iter()
            .filter(|mode| mode.size == source_mode.size)
            .collect::<Vec<_>>();
        if matching_size.is_empty() {
            return output.current_mode();
        }
        matching_size
            .into_iter()
            .min_by_key(|mode| (i64::from(mode.refresh) - i64::from(source_mode.refresh)).abs())
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
        self.notify_runtime_outputs_changed();
    }

    pub fn apply_runtime_display_configuration(&mut self) {
        let outputs = self.runtime_connected_outputs();
        if outputs.is_empty() {
            return;
        }

        let mut extend_output_names = outputs
            .iter()
            .filter_map(|output| {
                (self.runtime_output_mode_setting(&output.name()) == RuntimeOutputMode::Extend)
                    .then(|| output.name())
            })
            .collect::<std::collections::BTreeSet<_>>();
        if extend_output_names.is_empty()
            && let Some(output) = outputs.first()
        {
            warn!(
                output = %output.name(),
                "runtime display config disabled every output; keeping one output extended"
            );
            extend_output_names.insert(output.name());
        }

        let mut target_modes = std::collections::BTreeMap::new();
        let mut target_scales = std::collections::BTreeMap::new();
        for output in &outputs {
            let target_mode = self
                .runtime_output_configs
                .get(&output.name())
                .and_then(|config| config.resolution.as_ref())
                .and_then(|preference| self.resolve_runtime_output_mode(output, preference));
            target_modes.insert(output.name(), target_mode.or_else(|| output.current_mode()));
            target_scales.insert(
                output.name(),
                self.runtime_output_target_scale_value(output),
            );
        }

        let mut manual_positions = std::collections::BTreeMap::new();
        let mut auto_outputs = Vec::new();
        for output in &outputs {
            if !extend_output_names.contains(&output.name()) {
                continue;
            }
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
                let output = outputs.iter().find(|output| output.name() == *name)?;
                let mode = target_modes.get(name).and_then(|mode| *mode)?;
                let scale = target_scales.get(name).copied().unwrap_or(1.0);
                Some(x + Self::runtime_output_logical_width_for_mode(output, mode, scale))
            })
            .max()
            .unwrap_or(0);

        let mut target_positions = std::collections::BTreeMap::new();
        for (name, (x, y)) in manual_positions {
            target_positions.insert(name, Point::from((x, y)));
        }
        for output_name in auto_outputs {
            target_positions.insert(output_name.clone(), Point::from((auto_cursor_x, 0)));
            if let Some(output) = outputs.iter().find(|output| output.name() == output_name)
                && let Some(mode) = target_modes.get(&output_name).and_then(|mode| *mode)
            {
                let scale = target_scales.get(&output_name).copied().unwrap_or(1.0);
                auto_cursor_x += Self::runtime_output_logical_width_for_mode(output, mode, scale);
            }
        }

        let mut mirror_outputs = Vec::new();
        let mut disabled_outputs = Vec::new();
        for output in &outputs {
            let name = output.name();
            if extend_output_names.contains(&name) {
                continue;
            }
            match self.runtime_output_mode_setting(&name) {
                RuntimeOutputMode::Mirror => mirror_outputs.push(output.clone()),
                RuntimeOutputMode::Disabled | RuntimeOutputMode::Extend => {
                    disabled_outputs.push(output.clone())
                }
            }
        }

        for output in disabled_outputs {
            let name = output.name();
            self.space.unmap_output(&output);
            self.remove_output_global(&output);
            self.screencopy_state.remove_output(&output);
            crate::backend::image_copy_capture_render::fail_pending_output_capture(
                &mut self.image_copy_capture_pending,
                &output,
                smithay::wayland::image_copy_capture::CaptureFailureReason::Unknown,
            );
            self.output_capture_mirrors.remove(&name);
            self.runtime_animation_outputs.remove(&name);
            self.layer_effect_evaluation_cache.remove(&name);
            self.popup_effect_evaluation_cache.remove(&name);
            self.session_lock_surfaces.remove(&name);
            self.damage_blink_visible.remove(&name);
            self.damage_blink_pending.remove(&name);
            self.damage_blink_capture_suppression.remove(&name);
        }

        for output in outputs {
            let name = output.name();
            if !extend_output_names.contains(&name) {
                continue;
            }
            let target_mode = target_modes.get(&name).and_then(|mode| *mode);
            let target_position = target_positions
                .get(&name)
                .copied()
                .unwrap_or_else(|| output.current_location());
            let target_scale = self
                .runtime_output_configs
                .get(&name)
                .and_then(|config| config.scale)
                .map(|_| OutputScale::Fractional(target_scales.get(&name).copied().unwrap_or(1.0)));

            if let Some(mode) = target_mode {
                let current_mode = output.current_mode();
                if current_mode != Some(mode) {
                    let _ = apply_tty_output_mode(self, &name, mode);
                }
            }

            output.change_current_state(target_mode, None, target_scale, Some(target_position));
            self.create_output_global(&output);
            self.space.map_output(&output, target_position);
        }

        for output in mirror_outputs {
            let name = output.name();
            let Some(source_name) = self
                .runtime_output_configs
                .get(&name)
                .and_then(|config| config.source.as_ref())
                .filter(|source| extend_output_names.contains(*source))
                .cloned()
            else {
                self.space.unmap_output(&output);
                continue;
            };
            let Some(source_position) = target_positions.get(&source_name).copied() else {
                self.space.unmap_output(&output);
                continue;
            };
            let source_mode = target_modes.get(&source_name).and_then(|mode| *mode);
            let target_mode = source_mode
                .and_then(|mode| self.resolve_runtime_output_mirror_mode(&output, mode))
                .or_else(|| output.current_mode());
            let target_scale_value = target_scales
                .get(&source_name)
                .copied()
                .unwrap_or_else(|| output.current_scale().fractional_scale())
                .max(0.1);
            let target_scale = Some(OutputScale::Fractional(target_scale_value));

            if let Some(mode) = target_mode
                && output.current_mode() != Some(mode)
            {
                let _ = apply_tty_output_mode(self, &name, mode);
            }
            output.change_current_state(target_mode, None, target_scale, Some(source_position));
            self.create_output_global(&output);
            self.space.map_output(&output, source_position);
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
        self.configure_session_lock_surfaces();
        self.schedule_redraw();
    }

    pub fn sync_runtime_display_state(&self) {
        self.decoration_evaluator
            .sync_display_state(self.snapshot_outputs());
        self.decoration_evaluator
            .sync_input_state(self.runtime_input_device_state().clone());
    }

    pub fn notify_runtime_outputs_changed(&mut self) {
        self.sync_runtime_display_state();
        self.runtime_scheduler_enabled = true;
        self.runtime_poll_dirty = true;
        self.request_tty_maintenance("runtime-output-change");
        self.schedule_redraw();
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
        self.runtime_key_bindings = compile_runtime_key_bindings(&self.runtime_key_binding_entries);
    }

    pub fn consume_runtime_key_binding_config(
        &mut self,
        update: Option<RuntimeKeyBindingConfigUpdate>,
    ) {
        if let Some(update) = update {
            self.apply_runtime_key_binding_config_update(update);
        }
    }

    pub fn apply_runtime_pointer_config_update(&mut self, update: RuntimePointerConfigUpdate) {
        self.runtime_window_move_modifier = update.window_move_modifier.and_then(|shortcut| {
            match parse_runtime_pointer_modifier(&shortcut) {
                Ok(modifier) => Some(modifier),
                Err(error) => {
                    tracing::warn!(
                        window_move_modifier = shortcut,
                        ?error,
                        "ignoring invalid runtime pointer modifier"
                    );
                    None
                }
            }
        });

        self.runtime_window_resize_modifier =
            update.window_resize_modifier.and_then(
                |shortcut| match parse_runtime_pointer_modifier(&shortcut) {
                    Ok(modifier) => Some(modifier),
                    Err(error) => {
                        tracing::warn!(
                            window_resize_modifier = shortcut,
                            ?error,
                            "ignoring invalid runtime pointer modifier"
                        );
                        None
                    }
                },
            );
    }

    pub fn consume_runtime_pointer_config(&mut self, update: Option<RuntimePointerConfigUpdate>) {
        if let Some(update) = update {
            self.apply_runtime_pointer_config_update(update);
        }
    }

    pub fn register_libinput_device(&mut self, mut device: input::Device) {
        let key = libinput_device_key(&mut device);
        let snapshot = snapshot_for_libinput_device(&mut device);
        self.runtime_input_devices.insert(key.clone(), snapshot);
        self.runtime_libinput_devices.insert(key, device);
        self.decoration_evaluator
            .sync_input_state(self.runtime_input_device_state().clone());
        self.apply_runtime_input_config_to_devices();
    }

    pub fn unregister_libinput_device(&mut self, mut device: input::Device) {
        let key = libinput_device_key(&mut device);
        if let Some(snapshot) = self.runtime_input_devices.get(&key)
            && self.runtime_active_keyboard_device.as_ref() == Some(snapshot)
        {
            self.runtime_active_keyboard_device = None;
        }
        self.runtime_input_devices.remove(&key);
        self.runtime_libinput_devices.remove(&key);
        self.decoration_evaluator
            .sync_input_state(self.runtime_input_device_state().clone());
        self.apply_runtime_input_config_to_devices();
    }

    pub fn runtime_input_device_state(&self) -> &BTreeMap<String, RuntimeInputDeviceSnapshot> {
        &self.runtime_input_devices
    }

    fn apply_runtime_input_config_to_devices(&mut self) {
        apply_keyboard_config(self);
        apply_config_to_libinput_devices(
            &self.runtime_input_config,
            &self.runtime_input_devices,
            &mut self.runtime_libinput_devices,
        );
    }

    pub fn apply_runtime_input_config_update(&mut self, update: RuntimeInputConfigUpdate) {
        self.runtime_input_config = update.config;
        self.runtime_applied_xkb_config = None;
        self.apply_runtime_input_config_to_devices();
    }

    pub fn consume_runtime_input_config(&mut self, update: Option<RuntimeInputConfigUpdate>) {
        if let Some(update) = update {
            self.apply_runtime_input_config_update(update);
        }
    }

    pub fn apply_runtime_event_config_update(&mut self, update: RuntimeEventConfigUpdate) {
        self.runtime_pointer_move_enabled = update.pointer_move;
        self.runtime_pointer_move_async_enabled = update.pointer_move_async;
        self.runtime_gesture_swipe_enabled = update.gesture_swipe;
        self.runtime_gesture_swipe_async_enabled = update.gesture_swipe_async;
    }

    pub fn consume_runtime_event_config(&mut self, update: Option<RuntimeEventConfigUpdate>) {
        if let Some(update) = update {
            self.apply_runtime_event_config_update(update);
        }
    }

    pub fn consume_runtime_lifecycle_invocation(
        &mut self,
        invocation: DecorationHandlerInvocation,
    ) {
        self.consume_runtime_display_config(invocation.display_config);
        self.consume_runtime_workspace_config(invocation.workspace_config);
        self.consume_runtime_key_binding_config(invocation.key_binding_config);
        self.consume_runtime_pointer_config(invocation.pointer_config);
        self.consume_runtime_input_config(invocation.input_config);
        self.consume_runtime_event_config(invocation.event_config);
        self.consume_runtime_process_config(invocation.process_config);
        if !invocation.process_actions.is_empty() {
            self.apply_runtime_process_actions(invocation.process_actions);
        }
    }

    pub fn consume_runtime_workspace_config(
        &mut self,
        update: Option<RuntimeWorkspaceConfigUpdate>,
    ) {
        if let Some(update) = update {
            let outputs = self.space.outputs().cloned().collect::<Vec<_>>();
            self.ext_workspace_manager_state
                .sync::<Self>(update, &self.display_handle, &outputs);
        }
    }

    pub fn consume_runtime_debug_config(
        &mut self,
        update: Option<crate::runtime_debug::RuntimeDebugConfigUpdate>,
    ) {
        if let Some(update) = update {
            self.fps_counter.set_enabled(update.fps_counter);
            crate::profiler::set_enabled(update.profile);
        }
    }

    pub fn apply_runtime_cursor_config_update(
        &mut self,
        update: crate::cursor::RuntimeCursorConfigUpdate,
    ) {
        if !self.cursor_theme.apply_runtime_config(update) {
            return;
        }
        self.pointer_images.clear();
        self.current_pointer_image = None;
        self.pointer_element.clear_buffer();
        self.request_tty_maintenance("runtime-cursor-config");
        self.schedule_redraw();
    }

    pub fn mark_runtime_dirty_windows(
        &mut self,
        dirty_window_ids: impl IntoIterator<Item = String>,
        dirty_managed_window_ids: impl IntoIterator<Item = String>,
    ) {
        let managed_only = dirty_managed_window_ids
            .into_iter()
            .collect::<std::collections::HashSet<_>>();
        for window_id in dirty_window_ids {
            if runtime_dirty_debug_enabled() {
                info!(
                    window_id = %window_id,
                    managed_only = managed_only.contains(&window_id),
                    "runtime dirty debug: mark window dirty"
                );
            }
            if managed_only.contains(&window_id)
                && !self.runtime_dirty_window_ids.contains(&window_id)
            {
                self.runtime_managed_only_window_ids
                    .insert(window_id.clone());
            } else {
                self.runtime_managed_only_window_ids.remove(&window_id);
            }
            self.runtime_node_only_window_ids.remove(&window_id);
            self.runtime_dirty_window_ids.insert(window_id);
        }
    }

    pub fn handle_runtime_pointer_move_async_invocation(
        &mut self,
        invocation: DecorationPointerMoveAsyncInvocation,
        loop_handle: &LoopHandle<'_, Self>,
    ) {
        self.handle_runtime_pointer_invocation(invocation, loop_handle, "async");
    }

    pub fn handle_runtime_pointer_move_invocation(
        &mut self,
        invocation: DecorationPointerMoveAsyncInvocation,
    ) {
        let loop_handle = self.loop_handle.clone();
        self.handle_runtime_pointer_invocation(invocation, &loop_handle, "sync");
    }

    fn handle_runtime_pointer_invocation(
        &mut self,
        invocation: DecorationPointerMoveAsyncInvocation,
        loop_handle: &LoopHandle<'_, Self>,
        dispatch_mode: &'static str,
    ) {
        if invocation.dirty {
            self.runtime_poll_dirty = true;
            self.mark_runtime_dirty_windows(
                invocation.dirty_window_ids,
                invocation.dirty_managed_window_ids,
            );
            self.request_tty_maintenance(match dispatch_mode {
                "sync" => "runtime-pointer-move-dirty",
                _ => "runtime-pointer-move-async-dirty",
            });
            self.schedule_redraw();
        }

        self.consume_runtime_display_config(invocation.display_config);
        self.consume_runtime_workspace_config(invocation.workspace_config);
        self.consume_runtime_key_binding_config(invocation.key_binding_config);
        self.consume_runtime_pointer_config(invocation.pointer_config);
        self.consume_runtime_input_config(invocation.input_config);
        self.consume_runtime_event_config(invocation.event_config);
        self.consume_runtime_process_config(invocation.process_config);
        if !invocation.process_actions.is_empty() {
            self.apply_runtime_process_actions(invocation.process_actions);
        }

        if !invocation.actions.is_empty() {
            self.request_tty_maintenance(match dispatch_mode {
                "sync" => "runtime-pointer-move-actions",
                _ => "runtime-pointer-move-async-actions",
            });
            self.apply_runtime_window_actions(invocation.actions);
            self.schedule_redraw();
        }

        if invocation.next_poll_in_ms.is_some() {
            self.runtime_scheduler_enabled = true;
            // Pointer motion already coalesces to the latest event. Do not also restart the
            // animation timer for every completed sync or async motion request: repeatedly
            // replacing its generation makes animation progress depend on pointer traffic and
            // leaves a short gap when the final response hands control back to the timer.
            let requested_interval = self.runtime_scheduler_interval_ms(invocation.next_poll_in_ms);
            let existing_is_fast_enough = self.runtime_scheduler_kick_active
                && self
                    .runtime_scheduler_kick_interval_ms
                    .is_some_and(|interval| interval <= requested_interval);
            if !existing_is_fast_enough {
                self.schedule_runtime_scheduler_kick(loop_handle, invocation.next_poll_in_ms);
            }
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
                        (
                            RuntimeProcessRunPolicy::OncePerConfigVersion,
                            Some(last_run_generation),
                        ) => *last_run_generation != generation,
                        (RuntimeProcessRunPolicy::OncePerConfigVersion, None) => true,
                    };

                    if !should_run {
                        continue;
                    }

                    match spawn_runtime_process(&launch, cwd.as_deref(), &env) {
                        Ok(_child) => {
                            self.runtime_process_once_runs
                                .insert(id.clone(), generation);
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
                        launch, cwd, env, ..
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
                    if self
                        .runtime_managed_services
                        .contains_key(service_id.as_str())
                    {
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

    /// True when the topmost window on `output` is in the committed xdg
    /// Fullscreen state. Mirrors the renderer's fullscreen fast path so
    /// hit-testing follows the same stacking: a fullscreen window sits above
    /// the Top layer (only Overlay stays in front of it). `with_committed_state`
    /// (client-acked) means this flips on only once the client has actually
    /// committed its fullscreen buffer, matching what is on screen.
    fn output_has_topmost_fullscreen_window(&self, output: &Output) -> bool {
        self.windows_for_output_top_to_bottom(output)
            .first()
            .and_then(|window| window.toplevel())
            .is_some_and(|toplevel| {
                toplevel.with_committed_state(|state| {
                    state.is_some_and(|state| {
                        state.states.contains(
                            smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::State::Fullscreen,
                        )
                    })
                })
            })
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

        // When a fullscreen window owns the output, the Top layer drops behind
        // it (Hyprland-style stacking): only Overlay is hit-tested before the
        // window, and Top falls to the lower set — where the fullscreen window,
        // covering the whole output, intercepts every hit first. This makes the
        // Top layer effectively non-interactive while fullscreen, matching what
        // is rendered.
        let fullscreen_active = self.output_has_topmost_fullscreen_window(output);
        let (upper_layers, lower_layers): (&[WlrLayer], &[WlrLayer]) = if fullscreen_active {
            (
                &[WlrLayer::Overlay],
                &[WlrLayer::Top, WlrLayer::Bottom, WlrLayer::Background],
            )
        } else {
            (
                &[WlrLayer::Overlay, WlrLayer::Top],
                &[WlrLayer::Bottom, WlrLayer::Background],
            )
        };

        if let Some(focus) = self
            .layer_surface_under_with_policy(&layers, &output_geo, pos, upper_layers, true)
            .or_else(|| {
                self.layer_surface_under_with_policy(&layers, &output_geo, pos, upper_layers, false)
            })
        {
            return Some(focus);
        }

        if let Some(focus) = self.window_popup_surface_under(pos) {
            return Some(focus);
        }

        let logical_pos = LogicalPoint::new(pos.x.floor() as i32, pos.y.floor() as i32);
        if let Some(focus) = self.window_non_popup_child_surface_under(pos) {
            return Some(focus);
        }

        if let Some((window, decoration)) = self.window_under_transformed(logical_pos) {
            let transformed_client = transformed_rect(
                decoration.client_rect,
                decoration.layout.root.rect,
                decoration.visual_transform,
            );
            if decoration
                .content_clip
                .is_some_and(|clip| clip.clips_surface)
                && !transformed_client.contains(logical_pos)
            {
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

        self.layer_surface_under_with_policy(&layers, &output_geo, pos, lower_layers, false)
    }

    fn surface_has_popup_ancestor_for_hit_test(&self, surface: &WlSurface) -> bool {
        let mut current = Some(surface.clone());
        while let Some(candidate) = current {
            if self.popups.find_popup(&candidate).is_some() {
                return true;
            }
            current = smithay::wayland::compositor::get_parent(&candidate);
        }
        false
    }

    fn is_window_root_surface(window: &Window, surface: &WlSurface) -> bool {
        window
            .toplevel()
            .is_some_and(|toplevel| toplevel.wl_surface() == surface)
    }

    fn window_non_popup_child_surface_under(
        &self,
        pos: Point<f64, Logical>,
    ) -> Option<(WlSurface, Point<f64, Logical>)> {
        for window in self.windows_top_to_bottom() {
            let Some(location) = self.space.element_location(window) else {
                continue;
            };

            if let Some(decoration) = self.window_decorations.get(window) {
                if !self.decoration_allows_input_at(decoration, pos) {
                    continue;
                }
                let local_pos = inverse_transform_point(
                    pos,
                    decoration.layout.root.rect,
                    decoration.visual_transform,
                );
                let window_local_pos = local_pos - location.to_f64();
                if let Some((surface, loc)) =
                    window.surface_under(window_local_pos, WindowSurfaceType::ALL)
                {
                    let transformed_root = transformed_root_rect(
                        decoration.layout.root.rect,
                        decoration.visual_transform,
                    );
                    if Self::is_window_root_surface(window, &surface) {
                        // Root-surface hits inside the SSD root are handled by the
                        // decoration path below. A client-owned CSD resize/input region may
                        // extend outside that root, though; keep that hit attached to the
                        // frontmost window instead of letting a lower window claim it.
                        if transformed_root.contains(LogicalPoint::new(
                            pos.x.floor() as i32,
                            pos.y.floor() as i32,
                        )) {
                            return None;
                        }
                        let desired_local = window_local_pos - loc.to_f64();
                        let surface_origin = pos - desired_local;
                        return Some((surface, surface_origin));
                    }
                    if self.surface_has_popup_ancestor_for_hit_test(&surface) {
                        return None;
                    }
                    let desired_local = window_local_pos - loc.to_f64();
                    let surface_origin = pos - desired_local;
                    return Some((surface, surface_origin));
                }

                let transformed_root =
                    transformed_root_rect(decoration.layout.root.rect, decoration.visual_transform);
                if transformed_root.contains(LogicalPoint::new(
                    pos.x.floor() as i32,
                    pos.y.floor() as i32,
                )) {
                    return None;
                }

                continue;
            }

            let window_local_pos = pos - location.to_f64();
            if let Some((surface, loc)) =
                window.surface_under(window_local_pos, WindowSurfaceType::ALL)
            {
                if Self::is_window_root_surface(window, &surface)
                    || self.surface_has_popup_ancestor_for_hit_test(&surface)
                {
                    return None;
                }
                return Some((surface, loc.to_f64() + location.to_f64()));
            }

            let Some(rect) = self.window_bbox_rect(window) else {
                continue;
            };
            if rect.contains(LogicalPoint::new(
                pos.x.floor() as i32,
                pos.y.floor() as i32,
            )) {
                return None;
            }
        }

        None
    }

    fn window_popup_surface_under(
        &self,
        pos: Point<f64, Logical>,
    ) -> Option<(WlSurface, Point<f64, Logical>)> {
        for window in self.windows_top_to_bottom() {
            let Some(location) = self.space.element_location(window) else {
                continue;
            };

            if let Some(decoration) = self.window_decorations.get(window) {
                if !self.decoration_allows_input_at(decoration, pos) {
                    continue;
                }
                let local_pos = inverse_transform_point(
                    pos,
                    decoration.layout.root.rect,
                    decoration.visual_transform,
                );
                let window_local_pos = local_pos - location.to_f64();

                if let Some((surface, loc)) =
                    window.surface_under(window_local_pos, WindowSurfaceType::POPUP)
                {
                    let desired_local = window_local_pos - loc.to_f64();
                    let surface_origin = pos - desired_local;
                    return Some((surface, surface_origin));
                }

                let transformed_root =
                    transformed_root_rect(decoration.layout.root.rect, decoration.visual_transform);
                if transformed_root.contains(LogicalPoint::new(
                    pos.x.floor() as i32,
                    pos.y.floor() as i32,
                )) {
                    return None;
                }

                continue;
            }

            if let Some((surface, loc)) =
                window.surface_under(pos - location.to_f64(), WindowSurfaceType::POPUP)
            {
                return Some((surface, loc.to_f64() + location.to_f64()));
            }

            let Some(rect) = self.window_bbox_rect(window) else {
                continue;
            };
            if rect.contains(LogicalPoint::new(
                pos.x.floor() as i32,
                pos.y.floor() as i32,
            )) {
                return None;
            }
        }

        None
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
                    && matches!(
                        keyboard_interactivity,
                        smithay::wayland::shell::wlr_layer::KeyboardInteractivity::None
                    )
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
                    .map(|(surface, loc)| {
                        (surface, (loc + layer_geo.loc + output_geo.loc).to_f64())
                    });
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
        let pos = Point::<f64, Logical>::from((f64::from(logical_pos.x), f64::from(logical_pos.y)));
        self.windows_top_to_bottom().into_iter().find_map(|window| {
            let decoration = self.window_decorations.get(window)?;
            if !self.decoration_allows_input_at(decoration, pos) {
                return None;
            }
            let transformed_root =
                transformed_root_rect(decoration.layout.root.rect, decoration.visual_transform);
            transformed_root
                .contains(logical_pos)
                .then_some((window, decoration))
        })
    }

    pub fn raw_window_under(&self, logical_pos: LogicalPoint) -> Option<(&Window, LogicalRect)> {
        let pos = Point::<f64, Logical>::from((f64::from(logical_pos.x), f64::from(logical_pos.y)));
        self.windows_top_to_bottom().into_iter().find_map(|window| {
            if let Some(decoration) = self.window_decorations.get(window)
                && !self.decoration_allows_input_at(decoration, pos)
            {
                return None;
            }
            if let Some(decoration) = self.window_decorations.get(window)
                && decoration.managed_window.force_rect_size
                && decoration
                    .content_clip
                    .is_some_and(|clip| clip.clips_surface)
            {
                let transformed_root =
                    transformed_root_rect(decoration.layout.root.rect, decoration.visual_transform);
                if !transformed_root.contains(logical_pos) {
                    return None;
                }
            }
            let rect = self.window_bbox_rect(window)?;
            rect.contains(logical_pos).then_some((window, rect))
        })
    }

    pub(crate) fn window_bbox_rect(&self, window: &Window) -> Option<LogicalRect> {
        let location = self.space.element_location(window)?;
        let bbox = window.bbox();
        Some(LogicalRect::new(
            location.x + bbox.loc.x,
            location.y + bbox.loc.y,
            bbox.size.w,
            bbox.size.h,
        ))
    }

    pub fn windows_top_to_bottom(&self) -> Vec<&Window> {
        self.sorted_windows_top_to_bottom(self.space.elements())
    }

    pub fn windows_for_output_top_to_bottom<'a>(&'a self, output: &'a Output) -> Vec<&'a Window> {
        self.sorted_windows_top_to_bottom(
            self.space
                .elements()
                .filter(|window| self.window_intersects_output(window, output)),
        )
    }

    fn sorted_windows_top_to_bottom<'a, I>(&self, windows: I) -> Vec<&'a Window>
    where
        I: IntoIterator<Item = &'a Window>,
    {
        let mut indexed = windows.into_iter().enumerate().collect::<Vec<_>>();
        indexed.sort_by(|(left_index, left), (right_index, right)| {
            let left_z = self.managed_window_z_index(left);
            let right_z = self.managed_window_z_index(right);
            right_z
                .cmp(&left_z)
                .then_with(|| right_index.cmp(left_index))
        });
        indexed.into_iter().map(|(_, window)| window).collect()
    }

    fn window_intersects_output(&self, window: &Window, output: &Output) -> bool {
        let Some(output_geo) = self.space.output_geometry(output) else {
            return false;
        };
        let output_name = output.name();

        if let Some(decoration) = self
            .window_decorations
            .get(window)
            .filter(|decoration| decoration.managed_window.managed)
        {
            if !decoration.managed_window_allows_render_on_output(output_name.as_str()) {
                return false;
            }
            let rect = self.window_visual_bounds(window, decoration);
            return logical_rect_intersects_output(rect, output_geo);
        }

        let Some(rect) = self.window_bbox_rect(window) else {
            return false;
        };
        let rect = Rectangle::<i32, Logical>::new(
            Point::from((rect.x, rect.y)),
            (rect.width, rect.height).into(),
        );
        rect.intersection(output_geo).is_some()
    }

    pub fn managed_window_z_index(&self, window: &Window) -> i32 {
        self.window_decorations
            .get(window)
            .filter(|decoration| decoration.managed_window.managed)
            .and_then(|decoration| decoration.managed_window.z_index)
            .unwrap_or(0)
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

        let popup_root = self
            .popups
            .find_popup(surface)
            .or_else(|| self.popups.find_popup(&root))
            .and_then(|popup| smithay::desktop::find_popup_root_surface(&popup).ok());
        let target_root = popup_root.as_ref().unwrap_or(&root);

        self.space.outputs().find_map(|output| {
            let layers = layer_map_for_output(output);
            layers
                .layer_for_surface(target_root, WindowSurfaceType::TOPLEVEL)
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

    #[track_caller]
    pub fn schedule_redraw(&mut self) {
        let caller = std::panic::Location::caller();
        if std::env::var_os("SHOJI_REDRAW_STATS")
            .is_some_and(|value| value != "0" && !value.is_empty())
        {
            let key = format!("{}:{}", caller.file(), caller.line());
            *self.redraw_reason_counts.entry(key).or_default() += 1;

            if self.last_redraw_stats_log_at.elapsed() >= Duration::from_secs(1) {
                let mut counts = std::mem::take(&mut self.redraw_reason_counts)
                    .into_iter()
                    .collect::<Vec<_>>();
                counts.sort_by(|(_, left), (_, right)| right.cmp(left));
                counts.truncate(12);

                info!(
                    top_callers = ?counts,
                    tty_maintenance_pending = self.tty_maintenance_pending,
                    pending_decoration_damage_count = self.pending_decoration_damage.len(),
                    window_source_damage_count = self.window_source_damage.len(),
                    lower_layer_source_damage_count = self.lower_layer_source_damage.len(),
                    upper_layer_source_damage_count = self.upper_layer_source_damage.len(),
                    runtime_poll_dirty = self.runtime_poll_dirty,
                    runtime_dirty_window_ids_count = self.runtime_dirty_window_ids.len(),
                    transform_snapshot_window_ids_count = self.transform_snapshot_window_ids.len(),
                    closing_window_snapshots_count = self.closing_window_snapshots.len(),
                    "redraw stats"
                );
                self.last_redraw_stats_log_at = Instant::now();
            }
        }

        if !self.needs_redraw
            && std::env::var_os("SHOJI_REDRAW_REASON_DEBUG")
                .is_some_and(|value| value != "0" && !value.is_empty())
        {
            info!(
                caller_file = caller.file(),
                caller_line = caller.line(),
                caller_column = caller.column(),
                tty_maintenance_pending = self.tty_maintenance_pending,
                pending_decoration_damage_count = self.pending_decoration_damage.len(),
                window_source_damage_count = self.window_source_damage.len(),
                lower_layer_source_damage_count = self.lower_layer_source_damage.len(),
                upper_layer_source_damage_count = self.upper_layer_source_damage.len(),
                runtime_poll_dirty = self.runtime_poll_dirty,
                runtime_dirty_window_ids_count = self.runtime_dirty_window_ids.len(),
                transform_snapshot_window_ids_count = self.transform_snapshot_window_ids.len(),
                closing_window_snapshots_count = self.closing_window_snapshots.len(),
                "schedule_redraw requested"
            );
        }
        self.needs_redraw = true;
        // Intentionally no `loop_signal.wakeup()` — this mirrors niri's `queue_redraw`
        // (a pure state transition). All callers run inside event-loop dispatch
        // callbacks (Wayland commits, input, DRM VBlank/timer, XWayland), so dispatch
        // is already about to return and run the post-dispatch maintenance + render.
        // Self-waking while `ssd::integration::refresh_window_decorations_for_output`
        // calls `schedule_redraw()` mid-render spins the loop at ~10k iter/sec and —
        // together with an unconditional `flush_clients()` — caused the Firefox CPU
        // regression.
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
        if !self.window_allows_render(window) {
            return None;
        }

        if let Some(decoration) = self.window_decorations.get(window) {
            return Some(self.window_visual_bounds(window, decoration));
        }

        self.window_bbox_rect(window)
    }

    fn window_visual_bounds(
        &self,
        window: &Window,
        decoration: &WindowDecorationState,
    ) -> LogicalRect {
        let root = transformed_root_rect(decoration.layout.root.rect, decoration.visual_transform);
        if decoration
            .content_clip
            .is_some_and(|clip| clip.clips_surface)
        {
            return root;
        }

        let Some(surface) = self.window_bbox_rect(window).map(|rect| {
            transformed_rect(
                rect,
                decoration.layout.root.rect,
                decoration.visual_transform,
            )
        }) else {
            return root;
        };
        let left = root.x.min(surface.x);
        let top = root.y.min(surface.y);
        let right = (root.x + root.width).max(surface.x + surface.width);
        let bottom = (root.y + root.height).max(surface.y + surface.height);
        LogicalRect::new(left, top, right - left, bottom - top)
    }

    pub fn window_allows_render(&self, window: &Window) -> bool {
        self.window_decorations
            .get(window)
            .is_none_or(|decoration| decoration.managed_window_allows_render())
    }

    pub(crate) fn output_name_at_point(&self, pos: Point<f64, Logical>) -> Option<String> {
        self.space.outputs().find_map(|output| {
            self.space
                .output_geometry(output)
                .is_some_and(|geometry| geometry.contains(pos.to_i32_round()))
                .then(|| output.name())
        })
    }

    pub(crate) fn decoration_allows_input_at(
        &self,
        decoration: &WindowDecorationState,
        pos: Point<f64, Logical>,
    ) -> bool {
        let Some(output_name) = self.output_name_at_point(pos) else {
            return decoration.managed_window_allows_input();
        };
        decoration.managed_window_allows_input_on_output(output_name.as_str())
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
            .or_else(|| {
                window
                    .x11_surface()
                    .and_then(|surface| surface.wl_surface())
            })
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
            .iter()
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

        if std::env::var_os("SHOJI_SOURCE_DAMAGE_DEBUG").is_some() {
            let element_location = self.space.element_location(window);
            let geometry = window.geometry();
            tracing::info!(
                window_id = %decoration.snapshot.id,
                title = %decoration.snapshot.title,
                app_id = ?decoration.snapshot.app_id,
                is_xwayland = decoration.snapshot.is_xwayland,
                element_location = ?element_location,
                window_geometry = ?geometry,
                client_rect = ?decoration.client_rect,
                layout_root_rect = ?decoration.layout.root.rect,
                visual_transform = ?decoration.visual_transform,
                raw_damage_rects = ?damage_rects,
                mapped_damage_rects = ?mapped,
                "source damage debug: window damage extracted (inputs + mapping)"
            );
        }

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

        let output_name = output.name().to_string();
        if let Some(remaining) = self.damage_blink_capture_suppression.get_mut(&output_name) {
            *remaining = remaining.saturating_sub(1);
            if *remaining == 0 {
                self.damage_blink_capture_suppression.remove(&output_name);
            }
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
            .entry(output_name)
            .or_default()
            .extend(rects);
    }

    pub fn finish_damage_blink_frame(&mut self) {
        if !self.damage_blink_enabled {
            self.damage_blink_visible.clear();
            self.damage_blink_pending.clear();
            self.damage_blink_capture_suppression.clear();
            return;
        }

        let newly_visible_outputs = self
            .damage_blink_pending
            .iter()
            .filter(|(_, rects)| !rects.is_empty())
            .map(|(name, _)| name.clone())
            .collect::<Vec<_>>();
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
        for output_name in newly_visible_outputs {
            // The next render draws the debug overlay and the render after that erases it.
            // Neither operation is source damage, so do not feed those two frames back into
            // the diagnostic stream.
            self.damage_blink_capture_suppression.insert(output_name, 2);
        }

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
            self.damage_blink_capture_suppression.clear();
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
                self.damage_blink_capture_suppression
                    .insert(output_name.to_string(), 2);
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
    pub xwayland_refresh_override: bool,
}

impl ClientData for ClientState {
    fn initialized(&self, _client_id: ClientId) {}
    fn disconnected(&self, _client_id: ClientId, _reason: DisconnectReason) {}
}

fn is_xwayland_bridge_client<Fd: AsFd>(fd: Fd) -> bool {
    // There is no protocol-level "this client is an Xwayland bridge" bit. For the refresh
    // workaround we need the decision before global binding, so peer credentials are the least
    // invasive option available here. Keep the match intentionally narrow: this path changes
    // advertised output refresh and must not catch arbitrary Wayland clients.
    let Ok(credentials) = socket_peercred(fd) else {
        return false;
    };
    let pid = credentials.pid.as_raw_pid();
    let Some(command) = process_command_for_pid(pid) else {
        return false;
    };

    let is_bridge = command.contains("xwayland-satellite")
        || command.contains("Xwayland")
        || command.contains("Xorg");
    if is_bridge {
        info!(pid, command, "detected xwayland refresh-override client");
    }
    is_bridge
}

fn process_command_for_pid(pid: i32) -> Option<String> {
    if let Ok(cmdline) = fs::read(format!("/proc/{pid}/cmdline")) {
        let command = String::from_utf8_lossy(&cmdline)
            .split('\0')
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        if !command.is_empty() {
            return Some(command);
        }
    }

    fs::read_to_string(format!("/proc/{pid}/comm"))
        .ok()
        .map(|value| value.trim().to_owned())
}
