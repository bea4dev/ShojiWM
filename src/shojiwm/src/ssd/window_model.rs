use serde::Serialize;
use smithay::{
    desktop::{LayerSurface, Window, layer_map_for_output},
    reexports::{
        wayland_protocols::xdg::shell::server::xdg_toplevel,
        wayland_server::{Resource, protocol::wl_surface::WlSurface},
    },
    wayland::{
        compositor::with_states,
        shell::wlr_layer::{
            Anchor as WlrAnchor, ExclusiveZone as WlrExclusiveZone,
            KeyboardInteractivity as WlrKeyboardInteractivity, Layer as WlrLayer,
        },
        shell::xdg::{SurfaceCachedState, XdgToplevelSurfaceData},
    },
};

use super::DecorationInteractionSnapshot;
use crate::state::ShojiWM;

/// Rust-side snapshot that mirrors the TypeScript `WaylandWindow` view.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WaylandWindowSnapshot {
    pub id: String,
    pub title: String,
    pub app_id: Option<String>,
    pub position: WindowPositionSnapshot,
    pub rect: WindowPositionSnapshot,
    pub is_focused: bool,
    pub is_floating: bool,
    pub is_maximized: bool,
    pub is_fullscreen: bool,
    pub is_xwayland: bool,
    pub decoration: WindowDecorationStateSnapshot,
    pub size_constraints: WindowSizeConstraintsSnapshot,
    pub is_resizable: bool,
    pub is_transient: bool,
    pub parent_id: Option<String>,
    pub icon: Option<WindowIconSnapshot>,
    pub interaction: DecorationInteractionSnapshot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WindowDecorationModeSnapshot {
    Client,
    Server,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum WindowDecorationProtocolSnapshot {
    XdgDecorationV1,
    KdeServerDecoration,
    Xwayland,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum WindowDecorationPolicyReasonSnapshot {
    Initial,
    ClientRequest,
    ClientUnset,
    MetadataChanged,
    Reload,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowDecorationStateSnapshot {
    pub protocol: WindowDecorationProtocolSnapshot,
    pub client_preference: Option<WindowDecorationModeSnapshot>,
    pub configured_mode: WindowDecorationModeSnapshot,
    pub mode: WindowDecorationModeSnapshot,
}

impl Default for WindowDecorationStateSnapshot {
    fn default() -> Self {
        Self {
            protocol: WindowDecorationProtocolSnapshot::None,
            client_preference: None,
            configured_mode: WindowDecorationModeSnapshot::Server,
            mode: WindowDecorationModeSnapshot::Server,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowDecorationPolicyContextSnapshot {
    pub protocol: WindowDecorationProtocolSnapshot,
    pub client_preference: Option<WindowDecorationModeSnapshot>,
    pub can_negotiate: bool,
    pub reason: WindowDecorationPolicyReasonSnapshot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowDecorationDecisionSnapshot {
    pub mode: WindowDecorationModeSnapshot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowSizeSnapshot {
    pub width: i32,
    pub height: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct WindowSizeConstraintsSnapshot {
    pub min: Option<WindowSizeSnapshot>,
    pub max: Option<WindowSizeSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowIconSnapshot {
    pub name: Option<String>,
    pub bytes: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct WindowPositionSnapshot {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowResizePointSnapshot {
    pub x: i32,
    pub y: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PointerModifierStateSnapshot {
    #[serde(rename = "super")]
    pub logo: bool,
    pub alt: bool,
    pub ctrl: bool,
    pub shift: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PointerMovePointSnapshot {
    pub x: f64,
    pub y: f64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum PointerHitTargetSnapshot {
    None,
    Window {
        #[serde(rename = "windowId")]
        window_id: String,
    },
    Layer {
        #[serde(rename = "layerId")]
        layer_id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PointerMoveEventSnapshot {
    pub position: PointerMovePointSnapshot,
    pub delta: PointerMovePointSnapshot,
    pub target: PointerHitTargetSnapshot,
    pub output_name: Option<String>,
    pub modifiers: PointerModifierStateSnapshot,
    pub timestamp: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GestureSwipeEventSnapshot {
    pub phase: GestureSwipePhaseSnapshot,
    pub fingers: u32,
    pub position: Option<PointerMovePointSnapshot>,
    pub delta_x: f64,
    pub delta_y: f64,
    pub total_x: f64,
    pub total_y: f64,
    pub velocity_x: f64,
    pub velocity_y: f64,
    pub output_name: Option<String>,
    pub device: Option<crate::runtime_input::RuntimeInputDeviceSnapshot>,
    pub timestamp: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum GestureSwipePhaseSnapshot {
    Begin,
    Update,
    End,
    Cancel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowResizeEdgesSnapshot {
    pub left: bool,
    pub right: bool,
    pub top: bool,
    pub bottom: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum WindowResizeSourceSnapshot {
    Ssd,
    Modifier,
    ClientCsd,
    Xwayland,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum WindowResizePhaseSnapshot {
    Start,
    Update,
    End,
    Cancel,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowResizeEventSnapshot {
    pub source: WindowResizeSourceSnapshot,
    pub phase: WindowResizePhaseSnapshot,
    pub edges: WindowResizeEdgesSnapshot,
    pub start_pointer: WindowResizePointSnapshot,
    pub current_pointer: WindowResizePointSnapshot,
    pub delta: WindowResizePointSnapshot,
    pub start_rect: WindowPositionSnapshot,
    pub current_rect: WindowPositionSnapshot,
    pub output_name: Option<String>,
    pub timestamp: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum WindowMoveSourceSnapshot {
    Ssd,
    Modifier,
    ClientCsd,
    Xwayland,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum WindowMovePhaseSnapshot {
    Start,
    Update,
    End,
    Cancel,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowMoveEventSnapshot {
    pub source: WindowMoveSourceSnapshot,
    pub phase: WindowMovePhaseSnapshot,
    pub start_pointer: WindowResizePointSnapshot,
    pub current_pointer: WindowResizePointSnapshot,
    pub delta: WindowResizePointSnapshot,
    pub start_rect: WindowPositionSnapshot,
    pub current_rect: WindowPositionSnapshot,
    pub output_name: Option<String>,
    pub modifiers: PointerModifierStateSnapshot,
    pub timestamp: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum WindowStateRequestSourceSnapshot {
    Api,
    ClientCsd,
    XdgActivation,
    Xwayland,
    Keybind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum WindowActivateRequestSourceSnapshot {
    Api,
    XdgActivation,
    Xwayland,
    Keybind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowMaximizeRequestEventSnapshot {
    pub maximized: bool,
    pub source: WindowStateRequestSourceSnapshot,
    pub timestamp: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowMinimizeRequestEventSnapshot {
    pub minimized: bool,
    pub source: WindowStateRequestSourceSnapshot,
    pub timestamp: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowFullscreenRequestEventSnapshot {
    pub fullscreen: bool,
    /// Output the client asked to go fullscreen on (xdg_toplevel.set_fullscreen
    /// carries an optional wl_output). None lets the config pick.
    pub output_name: Option<String>,
    pub source: WindowStateRequestSourceSnapshot,
    pub timestamp: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowActivateRequestEventSnapshot {
    pub source: WindowActivateRequestSourceSnapshot,
    pub timestamp: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagedWindowRectSnapshot {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagedWindowPointSnapshot {
    pub x: f64,
    pub y: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ManagedWindowAnimationMode {
    Override,
    Add,
    Sub,
    Multiply,
}

impl Default for ManagedWindowAnimationMode {
    fn default() -> Self {
        Self::Override
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ManagedWindowAnimationEasingSnapshot {
    Linear,
    CubicBezier { x1: f64, y1: f64, x2: f64, y2: f64 },
}

impl Default for ManagedWindowAnimationEasingSnapshot {
    fn default() -> Self {
        Self::Linear
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagedWindowRectAnimationSnapshot {
    #[serde(default)]
    pub from: Option<ManagedWindowRectSnapshot>,
    pub to: ManagedWindowRectSnapshot,
    pub duration: u64,
    #[serde(default)]
    pub easing: ManagedWindowAnimationEasingSnapshot,
    #[serde(default)]
    pub mode: ManagedWindowAnimationMode,
}

#[derive(Debug, Clone, PartialEq, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagedWindowPointAnimationSnapshot {
    #[serde(default)]
    pub from: Option<ManagedWindowPointSnapshot>,
    pub to: ManagedWindowPointSnapshot,
    pub duration: u64,
    #[serde(default)]
    pub easing: ManagedWindowAnimationEasingSnapshot,
    #[serde(default)]
    pub mode: ManagedWindowAnimationMode,
}

#[derive(Debug, Clone, PartialEq, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagedWindowScalarAnimationSnapshot {
    #[serde(default)]
    pub from: Option<f64>,
    pub to: f64,
    pub duration: u64,
    #[serde(default)]
    pub easing: ManagedWindowAnimationEasingSnapshot,
    #[serde(default)]
    pub mode: ManagedWindowAnimationMode,
}

#[derive(Debug, Clone, PartialEq, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagedWindowAnimationSnapshot {
    #[serde(default = "default_animation_channel")]
    pub channel: String,
    #[serde(default)]
    pub rect: Option<ManagedWindowRectAnimationSnapshot>,
    #[serde(default)]
    pub offset: Option<ManagedWindowPointAnimationSnapshot>,
    #[serde(default)]
    pub opacity: Option<ManagedWindowScalarAnimationSnapshot>,
}

fn default_animation_channel() -> String {
    "default".to_string()
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WaylandLayerSnapshot {
    pub id: String,
    pub namespace: Option<String>,
    pub layer: LayerKindSnapshot,
    pub output_name: String,
    pub position: LayerPositionSnapshot,
    pub anchor: LayerAnchorSnapshot,
    pub exclusive_zone: LayerExclusiveZoneSnapshot,
    pub exclusive_edge: Option<LayerEdgeSnapshot>,
    pub margin: LayerMarginSnapshot,
    pub keyboard_interactivity: KeyboardInteractivitySnapshot,
    pub desired_size: LayerDesiredSizeSnapshot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LayerAnchorSnapshot {
    pub top: bool,
    pub bottom: bool,
    pub left: bool,
    pub right: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(tag = "mode", rename_all = "camelCase")]
pub enum LayerExclusiveZoneSnapshot {
    /// Surface reserves `size` logical pixels along its anchored edge.
    Exclusive { size: u32 },
    /// Surface participates in exclusive-zone avoidance but reserves nothing.
    Neutral,
    /// Surface opts out — compositor may extend it to anchored edges.
    DontCare,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LayerEdgeSnapshot {
    Top,
    Bottom,
    Left,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct LayerMarginSnapshot {
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
    pub left: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum KeyboardInteractivitySnapshot {
    None,
    OnDemand,
    Exclusive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct LayerDesiredSizeSnapshot {
    pub width: i32,
    pub height: i32,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WaylandOutputSnapshot {
    pub name: String,
    pub description: Option<String>,
    pub make: Option<String>,
    pub model: Option<String>,
    pub serial: Option<String>,
    pub connector: Option<String>,
    pub enabled: bool,
    pub resolution: Option<OutputModeSnapshot>,
    pub position: OutputPositionSnapshot,
    pub scale: f64,
    pub available_modes: Vec<OutputModeSnapshot>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputModeSnapshot {
    pub width: i32,
    pub height: i32,
    pub refresh_rate: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputPositionSnapshot {
    pub x: i32,
    pub y: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LayerPositionSnapshot {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

/// What kind of surface an xdg_popup is (transitively) attached to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PopupParentKindSnapshot {
    Layer,
    Window,
}

/// TypeScript-facing snapshot of a mapped xdg_popup, used to evaluate
/// per-popup effects (`COMPOSITOR.effect.popup`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WaylandPopupSnapshot {
    pub id: String,
    /// Runtime id of the root surface this popup belongs to.
    pub parent_id: String,
    pub parent_kind: PopupParentKindSnapshot,
    pub output_name: String,
    /// Output-local logical rect of the popup's geometry box.
    pub position: LayerPositionSnapshot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LayerKindSnapshot {
    Background,
    Bottom,
    Top,
    Overlay,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowTransform {
    pub origin: TransformOrigin,
    pub translate_x: f64,
    pub translate_y: f64,
    pub scale_x: f64,
    pub scale_y: f64,
    pub opacity: f32,
}

impl Default for WindowTransform {
    fn default() -> Self {
        Self {
            origin: TransformOrigin { x: 0.5, y: 0.5 },
            translate_x: 0.0,
            translate_y: 0.0,
            scale_x: 1.0,
            scale_y: 1.0,
            opacity: 1.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransformOrigin {
    pub x: f64,
    pub y: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagedWindowState {
    #[serde(default)]
    pub managed: bool,
    pub rect: Option<ManagedWindowRectSnapshot>,
    pub workspace: Option<serde_json::Value>,
    #[serde(default, rename = "visibleOutputs")]
    pub visible_outputs: Option<Vec<String>>,
    #[serde(default = "default_true")]
    pub visible: bool,
    #[serde(default)]
    pub idle: bool,
    #[serde(default = "default_true")]
    pub interactive: bool,
    #[serde(default)]
    pub force_rect_size: bool,
    #[serde(default)]
    pub tiled: bool,
    /// Per-window tearing permission set from the TS config (`<ManagedWindow allowTearing>`).
    /// `None` means unspecified — the compositor falls back to the client's `wp_tearing_control`
    /// hint. `Some(_)` overrides that hint. Consumed by the fullscreen tearing fast path in
    /// `backend::tty` (`should_tear`).
    #[serde(default)]
    pub allow_tearing: Option<bool>,
    #[serde(default)]
    pub z_index: Option<i32>,
    #[serde(default)]
    pub transform: WindowTransform,
}

impl Default for ManagedWindowState {
    fn default() -> Self {
        Self {
            rect: None,
            managed: false,
            workspace: None,
            visible_outputs: None,
            visible: true,
            idle: false,
            interactive: true,
            force_rect_size: false,
            tiled: false,
            allow_tearing: None,
            z_index: None,
            transform: WindowTransform::default(),
        }
    }
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WaylandWindowAction {
    Close,
    FinalizeClose,
    Maximize,
    Unmaximize,
    Minimize,
    Fullscreen,
    Unfullscreen,
    Focus,
    ScheduleAnimation,
    CancelAnimation,
}

impl ShojiWM {
    /// Build a TypeScript-facing window snapshot for a mapped window.
    pub fn snapshot_window(&self, window: &Window) -> WaylandWindowSnapshot {
        let Some(toplevel) = window.toplevel() else {
            return self.snapshot_x11_window(window);
        };

        let (title, app_id) = with_states(toplevel.wl_surface(), |states| {
            let role = states
                .data_map
                .get::<XdgToplevelSurfaceData>()
                .expect("xdg toplevel surface should have role data")
                .lock()
                .expect("xdg toplevel role mutex poisoned");

            (role.title.clone().unwrap_or_default(), role.app_id.clone())
        });
        let initial_configure_sent = toplevel.is_initial_configure_sent();
        let (min_size, max_size) = with_states(toplevel.wl_surface(), |states| {
            let mut guard = states.cached_state.get::<SurfaceCachedState>();
            let data = if initial_configure_sent {
                guard.current()
            } else {
                // Before the first configure the client may already have
                // double-buffered min/max size constraints. Those constraints
                // are not committed yet, but they are exactly what initial
                // placement needs to decide whether a window is tileable.
                guard.pending()
            };
            (data.min_size, data.max_size)
        });
        let size_constraints = window_size_constraints(min_size, max_size);
        let is_resizable = window_constraints_are_resizable(min_size, max_size);
        let parent_surface = toplevel.parent();
        let parent_id = parent_surface
            .as_ref()
            .and_then(|surface| self.runtime_id_for_toplevel_surface(surface));
        let is_transient = parent_surface.is_some();

        let focused_surface = self
            .seat
            .get_keyboard()
            .and_then(|keyboard| keyboard.current_focus());
        let is_focused = focused_surface
            .as_ref()
            .is_some_and(|focused| self.surface_belongs_to_window(window, focused));
        let (_pending_activated, is_maximized, is_fullscreen) =
            toplevel.with_pending_state(|state| {
                (
                    state.states.contains(xdg_toplevel::State::Activated),
                    state.states.contains(xdg_toplevel::State::Maximized),
                    state.states.contains(xdg_toplevel::State::Fullscreen),
                )
            });
        let position = self
            .space
            .element_location(window)
            .map(|loc| {
                let geometry = window.geometry();
                WindowPositionSnapshot {
                    x: loc.x + geometry.loc.x,
                    y: loc.y + geometry.loc.y,
                    width: geometry.size.w,
                    height: geometry.size.h,
                }
            })
            .unwrap_or_default();

        let runtime_id = if let Some(existing) = self.window_decorations.get(window) {
            existing.snapshot.id.clone()
        } else {
            runtime_id_for_window(window, toplevel.wl_surface().id().protocol_id())
        };
        let rect = self
            .window_decorations
            .get(window)
            .map(|decoration| WindowPositionSnapshot {
                x: decoration.layout.root.rect.x,
                y: decoration.layout.root.rect.y,
                width: decoration.layout.root.rect.width,
                height: decoration.layout.root.rect.height,
            })
            .unwrap_or(position);

        WaylandWindowSnapshot {
            id: runtime_id,
            title,
            app_id: app_id.clone(),
            position,
            rect,
            is_focused,
            // ShojiWM is currently a floating WM; expose that policy explicitly.
            is_floating: true,
            is_maximized,
            is_fullscreen,
            is_xwayland: false,
            decoration: self.window_decoration_snapshot(toplevel.wl_surface(), false),
            size_constraints,
            is_resizable,
            is_transient,
            parent_id,
            icon: app_id.as_ref().map(|name| WindowIconSnapshot {
                name: Some(name.clone()),
                bytes: None,
            }),
            interaction: DecorationInteractionSnapshot::default(),
        }
    }

    fn snapshot_x11_window(&self, window: &Window) -> WaylandWindowSnapshot {
        let Some(x11) = window.x11_surface() else {
            return WaylandWindowSnapshot {
                id: "unknown".into(),
                title: String::new(),
                app_id: None,
                position: WindowPositionSnapshot::default(),
                rect: WindowPositionSnapshot::default(),
                is_focused: false,
                is_floating: true,
                is_maximized: false,
                is_fullscreen: false,
                is_xwayland: false,
                decoration: WindowDecorationStateSnapshot::default(),
                size_constraints: WindowSizeConstraintsSnapshot::default(),
                is_resizable: true,
                is_transient: false,
                parent_id: None,
                icon: None,
                interaction: DecorationInteractionSnapshot::default(),
            };
        };

        let title = x11.title();
        let class = x11.class();
        let app_id = (!class.is_empty()).then_some(class);
        let min_size = x11.min_size().unwrap_or_default();
        let max_size = x11.max_size().unwrap_or_default();
        let size_constraints = window_size_constraints(min_size, max_size);
        let is_resizable = window_constraints_are_resizable(min_size, max_size);
        let transient_for = x11.is_transient_for();
        let parent_id = transient_for.map(|parent| format!("x11:{parent}"));
        let is_transient = parent_id.is_some() || x11.is_modal();

        let focused_surface = self
            .seat
            .get_keyboard()
            .and_then(|keyboard| keyboard.current_focus());
        let is_focused = focused_surface
            .as_ref()
            .is_some_and(|focused| self.surface_belongs_to_window(window, focused));

        let position = self
            .space
            .element_location(window)
            .map(|loc| {
                let geometry = window.geometry();
                WindowPositionSnapshot {
                    x: loc.x + geometry.loc.x,
                    y: loc.y + geometry.loc.y,
                    width: geometry.size.w.max(1),
                    height: geometry.size.h.max(1),
                }
            })
            .unwrap_or_default();

        let runtime_id = if let Some(existing) = self.window_decorations.get(window) {
            existing.snapshot.id.clone()
        } else {
            runtime_id_for_x11_window(&x11)
        };
        let rect = self
            .window_decorations
            .get(window)
            .map(|decoration| WindowPositionSnapshot {
                x: decoration.layout.root.rect.x,
                y: decoration.layout.root.rect.y,
                width: decoration.layout.root.rect.width,
                height: decoration.layout.root.rect.height,
            })
            .unwrap_or(position);

        WaylandWindowSnapshot {
            id: runtime_id,
            title,
            app_id: app_id.clone(),
            position,
            rect,
            is_focused,
            is_floating: true,
            is_maximized: false,
            is_fullscreen: false,
            is_xwayland: true,
            decoration: WindowDecorationStateSnapshot {
                protocol: WindowDecorationProtocolSnapshot::Xwayland,
                ..WindowDecorationStateSnapshot::default()
            },
            size_constraints,
            is_resizable,
            is_transient,
            parent_id,
            icon: app_id.as_ref().map(|name| WindowIconSnapshot {
                name: Some(name.clone()),
                bytes: None,
            }),
            interaction: DecorationInteractionSnapshot::default(),
        }
    }

    pub fn snapshot_windows(&self) -> Vec<WaylandWindowSnapshot> {
        self.space
            .elements()
            .map(|window| self.snapshot_window(window))
            .collect()
    }

    fn runtime_id_for_toplevel_surface(&self, surface: &WlSurface) -> Option<String> {
        self.space.elements().find_map(|window| {
            let toplevel = window.toplevel()?;
            (toplevel.wl_surface() == surface).then(|| {
                self.window_decorations
                    .get(window)
                    .map(|decoration| decoration.snapshot.id.clone())
                    .unwrap_or_else(|| runtime_id_for_window(window, surface.id().protocol_id()))
            })
        })
    }

    pub fn snapshot_layer_surface(
        &self,
        output_name: &str,
        layer: &LayerSurface,
        geometry: smithay::utils::Rectangle<i32, smithay::utils::Logical>,
    ) -> WaylandLayerSnapshot {
        let state = layer.cached_state();
        WaylandLayerSnapshot {
            id: layer_runtime_id(layer),
            namespace: Some(layer.namespace().to_string()),
            layer: match layer.layer() {
                WlrLayer::Background => LayerKindSnapshot::Background,
                WlrLayer::Bottom => LayerKindSnapshot::Bottom,
                WlrLayer::Top => LayerKindSnapshot::Top,
                WlrLayer::Overlay => LayerKindSnapshot::Overlay,
            },
            output_name: output_name.to_string(),
            position: LayerPositionSnapshot {
                x: geometry.loc.x,
                y: geometry.loc.y,
                width: geometry.size.w,
                height: geometry.size.h,
            },
            anchor: LayerAnchorSnapshot {
                top: state.anchor.contains(WlrAnchor::TOP),
                bottom: state.anchor.contains(WlrAnchor::BOTTOM),
                left: state.anchor.contains(WlrAnchor::LEFT),
                right: state.anchor.contains(WlrAnchor::RIGHT),
            },
            exclusive_zone: match state.exclusive_zone {
                WlrExclusiveZone::Exclusive(size) => LayerExclusiveZoneSnapshot::Exclusive { size },
                WlrExclusiveZone::Neutral => LayerExclusiveZoneSnapshot::Neutral,
                WlrExclusiveZone::DontCare => LayerExclusiveZoneSnapshot::DontCare,
            },
            exclusive_edge: state.exclusive_edge.and_then(anchor_to_edge_snapshot),
            margin: LayerMarginSnapshot {
                top: state.margin.top,
                right: state.margin.right,
                bottom: state.margin.bottom,
                left: state.margin.left,
            },
            keyboard_interactivity: match state.keyboard_interactivity {
                WlrKeyboardInteractivity::None => KeyboardInteractivitySnapshot::None,
                WlrKeyboardInteractivity::OnDemand => KeyboardInteractivitySnapshot::OnDemand,
                WlrKeyboardInteractivity::Exclusive => KeyboardInteractivitySnapshot::Exclusive,
            },
            desired_size: LayerDesiredSizeSnapshot {
                width: state.size.w,
                height: state.size.h,
            },
        }
    }

    pub fn snapshot_layers(&self) -> Vec<WaylandLayerSnapshot> {
        let mut layers = Vec::new();
        for output in self.space.outputs() {
            let output_name = output.name().to_string();
            let map = layer_map_for_output(output);
            for layer in map.layers() {
                if let Some(geometry) = map.layer_geometry(layer) {
                    layers.push(self.snapshot_layer_surface(&output_name, layer, geometry));
                }
            }
        }
        layers
    }

    /// TypeScript-facing snapshots of all xdg_popups currently mapped on
    /// layer surfaces and toplevel windows. `position` is output-local
    /// logical, consistent with `WaylandLayerSnapshot::position` (the popup's
    /// geometry box, i.e. the rect a popup-level effect applies to).
    pub fn snapshot_popups(&self) -> Vec<WaylandPopupSnapshot> {
        let mut popups = Vec::new();
        for output in self.space.outputs() {
            let output_name = output.name().to_string();
            let map = layer_map_for_output(output);
            for layer in map.layers() {
                let Some(geometry) = map.layer_geometry(layer) else {
                    continue;
                };
                let parent_id = layer_runtime_id(layer);
                for (popup, popup_offset) in
                    smithay::desktop::PopupManager::popups_for_surface(layer.wl_surface())
                {
                    let popup_geometry = popup.geometry();
                    popups.push(WaylandPopupSnapshot {
                        id: popup_runtime_id(popup.wl_surface()),
                        parent_id: parent_id.clone(),
                        parent_kind: PopupParentKindSnapshot::Layer,
                        output_name: output_name.clone(),
                        position: LayerPositionSnapshot {
                            x: geometry.loc.x + popup_offset.x,
                            y: geometry.loc.y + popup_offset.y,
                            width: popup_geometry.size.w,
                            height: popup_geometry.size.h,
                        },
                    });
                }
            }
        }

        // Toplevel window popups. The popup geometry box sits at the window's
        // space location + the popup's offset (relative to the parent's
        // geometry origin); assign each popup to the output containing it.
        for window in self.space.elements() {
            let Some(toplevel) = window.toplevel() else {
                continue;
            };
            let window_popups: Vec<_> =
                smithay::desktop::PopupManager::popups_for_surface(toplevel.wl_surface()).collect();
            if window_popups.is_empty() {
                continue;
            }
            let Some(window_location) = self.space.element_location(window) else {
                continue;
            };
            let parent_id = self
                .window_decorations
                .get(window)
                .map(|decoration| decoration.snapshot.id.clone())
                .unwrap_or_else(|| self.snapshot_window(window).id);
            for (popup, popup_offset) in window_popups {
                let popup_geometry = popup.geometry();
                let global = smithay::utils::Point::<i32, smithay::utils::Logical>::from((
                    window_location.x + popup_offset.x,
                    window_location.y + popup_offset.y,
                ));
                let Some((output_name, output_loc)) = self
                    .space
                    .outputs()
                    .find_map(|output| {
                        let geometry = self.space.output_geometry(output)?;
                        geometry
                            .contains(global)
                            .then(|| (output.name().to_string(), geometry.loc))
                    })
                    .or_else(|| {
                        self.space.outputs().next().and_then(|output| {
                            let geometry = self.space.output_geometry(output)?;
                            Some((output.name().to_string(), geometry.loc))
                        })
                    })
                else {
                    continue;
                };
                popups.push(WaylandPopupSnapshot {
                    id: popup_runtime_id(popup.wl_surface()),
                    parent_id: parent_id.clone(),
                    parent_kind: PopupParentKindSnapshot::Window,
                    output_name,
                    position: LayerPositionSnapshot {
                        x: global.x - output_loc.x,
                        y: global.y - output_loc.y,
                        width: popup_geometry.size.w,
                        height: popup_geometry.size.h,
                    },
                });
            }
        }
        popups
    }
}

fn window_size_constraints(
    min_size: smithay::utils::Size<i32, smithay::utils::Logical>,
    max_size: smithay::utils::Size<i32, smithay::utils::Logical>,
) -> WindowSizeConstraintsSnapshot {
    WindowSizeConstraintsSnapshot {
        min: size_constraint_snapshot(min_size),
        max: size_constraint_snapshot(max_size),
    }
}

fn size_constraint_snapshot(
    size: smithay::utils::Size<i32, smithay::utils::Logical>,
) -> Option<WindowSizeSnapshot> {
    (size.w > 0 || size.h > 0).then_some(WindowSizeSnapshot {
        width: size.w.max(0),
        height: size.h.max(0),
    })
}

fn window_constraints_are_resizable(
    min_size: smithay::utils::Size<i32, smithay::utils::Logical>,
    max_size: smithay::utils::Size<i32, smithay::utils::Logical>,
) -> bool {
    let width_fixed = min_size.w > 0 && max_size.w > 0 && min_size.w == max_size.w;
    let height_fixed = min_size.h > 0 && max_size.h > 0 && min_size.h == max_size.h;
    !(width_fixed || height_fixed)
}

fn runtime_id_for_window(window: &Window, protocol_id: u32) -> String {
    window
        .toplevel()
        .and_then(|toplevel| {
            toplevel
                .wl_surface()
                .client()
                .map(|client| format!("{:?}:{}", client.id(), protocol_id))
        })
        .unwrap_or_else(|| format!("unknown-client:{protocol_id}"))
}

fn runtime_id_for_x11_window(surface: &smithay::xwayland::X11Surface) -> String {
    format!("x11:{}", surface.window_id())
}

pub fn layer_runtime_id(layer: &LayerSurface) -> String {
    let surface = layer.wl_surface();
    let protocol_id = surface.id().protocol_id();
    let client_id = surface
        .client()
        .map(|client| format!("{:?}", client.id()))
        .unwrap_or_else(|| "unknown-client".to_string());
    format!("{client_id}:{protocol_id}")
}

/// Stable runtime id for an xdg_popup surface (same client:protocol scheme as
/// windows and layers, so it is unique among all runtime ids).
pub fn popup_runtime_id(
    surface: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
) -> String {
    let protocol_id = surface.id().protocol_id();
    let client_id = surface
        .client()
        .map(|client| format!("{:?}", client.id()))
        .unwrap_or_else(|| "unknown-client".to_string());
    format!("{client_id}:{protocol_id}")
}

/// Reduce an `Anchor` bitset (smithay) to the single edge a layer wants its
/// exclusive zone applied to. Returns `None` for ambiguous combinations (e.g.
/// corners or empty), since the layer-shell `exclusive_edge` field is only
/// meaningful when one edge is chosen.
fn anchor_to_edge_snapshot(anchor: WlrAnchor) -> Option<LayerEdgeSnapshot> {
    match anchor {
        a if a == WlrAnchor::TOP => Some(LayerEdgeSnapshot::Top),
        a if a == WlrAnchor::BOTTOM => Some(LayerEdgeSnapshot::Bottom),
        a if a == WlrAnchor::LEFT => Some(LayerEdgeSnapshot::Left),
        a if a == WlrAnchor::RIGHT => Some(LayerEdgeSnapshot::Right),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{PointerHitTargetSnapshot, WaylandWindowAction};

    #[test]
    fn pointer_hit_targets_serialize_for_runtime_events() {
        assert_eq!(
            serde_json::to_value(PointerHitTargetSnapshot::None).expect("serialize none"),
            serde_json::json!({ "kind": "none" }),
        );
        assert_eq!(
            serde_json::to_value(PointerHitTargetSnapshot::Window {
                window_id: "window-1".into(),
            })
            .expect("serialize window"),
            serde_json::json!({ "kind": "window", "windowId": "window-1" }),
        );
        assert_eq!(
            serde_json::to_value(PointerHitTargetSnapshot::Layer {
                layer_id: "layer-1".into(),
            })
            .expect("serialize layer"),
            serde_json::json!({ "kind": "layer", "layerId": "layer-1" }),
        );
    }

    #[test]
    fn wayland_window_actions_serialize_to_camel_case_strings() {
        let close = serde_json::to_string(&WaylandWindowAction::Close).expect("serialize close");
        let finalize_close = serde_json::to_string(&WaylandWindowAction::FinalizeClose)
            .expect("serialize finalize close");
        let maximize =
            serde_json::to_string(&WaylandWindowAction::Maximize).expect("serialize maximize");
        let unmaximize =
            serde_json::to_string(&WaylandWindowAction::Unmaximize).expect("serialize unmaximize");
        let minimize =
            serde_json::to_string(&WaylandWindowAction::Minimize).expect("serialize minimize");
        let focus = serde_json::to_string(&WaylandWindowAction::Focus).expect("serialize focus");
        let schedule_animation = serde_json::to_string(&WaylandWindowAction::ScheduleAnimation)
            .expect("serialize schedule animation");
        let cancel_animation = serde_json::to_string(&WaylandWindowAction::CancelAnimation)
            .expect("serialize cancel animation");

        assert_eq!(close, "\"close\"");
        assert_eq!(finalize_close, "\"finalizeClose\"");
        assert_eq!(maximize, "\"maximize\"");
        assert_eq!(unmaximize, "\"unmaximize\"");
        assert_eq!(minimize, "\"minimize\"");
        assert_eq!(focus, "\"focus\"");
        assert_eq!(schedule_animation, "\"scheduleAnimation\"");
        assert_eq!(cancel_animation, "\"cancelAnimation\"");
    }
}
