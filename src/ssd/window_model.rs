use serde::Serialize;
use smithay::{
    desktop::{LayerSurface, Window, layer_map_for_output},
    reexports::{wayland_protocols::xdg::shell::server::xdg_toplevel, wayland_server::Resource},
    wayland::{
        compositor::with_states, shell::wlr_layer::Layer as WlrLayer,
        shell::xdg::XdgToplevelSurfaceData,
    },
};

use super::DecorationInteractionSnapshot;
use crate::state::ShojiWM;

/// Rust-side snapshot that mirrors the TypeScript `WaylandWindow` view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WaylandWindowSnapshot {
    pub id: String,
    pub title: String,
    pub app_id: Option<String>,
    pub position: WindowPositionSnapshot,
    pub is_focused: bool,
    pub is_floating: bool,
    pub is_maximized: bool,
    pub is_fullscreen: bool,
    pub is_xwayland: bool,
    pub icon: Option<WindowIconSnapshot>,
    pub interaction: DecorationInteractionSnapshot,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WaylandLayerSnapshot {
    pub id: String,
    pub namespace: Option<String>,
    pub layer: LayerKindSnapshot,
    pub output_name: String,
    pub position: LayerPositionSnapshot,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WaylandOutputSnapshot {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LayerPositionSnapshot {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WaylandWindowAction {
    Close,
    FinalizeClose,
    Maximize,
    Minimize,
}

impl ShojiWM {
    /// Build a TypeScript-facing window snapshot for a mapped window.
    pub fn snapshot_window(&self, window: &Window) -> WaylandWindowSnapshot {
        let Some(toplevel) = window.toplevel() else {
            return WaylandWindowSnapshot {
                id: "unknown".into(),
                title: String::new(),
                app_id: None,
                position: WindowPositionSnapshot::default(),
                is_focused: false,
                is_floating: true,
                is_maximized: false,
                is_fullscreen: false,
                is_xwayland: false,
                icon: None,
                interaction: DecorationInteractionSnapshot::default(),
            };
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

        let focused_surface = self
            .seat
            .get_keyboard()
            .and_then(|keyboard| keyboard.current_focus());
        let is_focused = focused_surface
            .as_ref()
            .is_some_and(|focused| focused == &toplevel.wl_surface().clone());
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

        WaylandWindowSnapshot {
            id: runtime_id,
            title,
            app_id: app_id.clone(),
            position,
            is_focused,
            // ShojiWM is currently a floating WM; expose that policy explicitly.
            is_floating: true,
            is_maximized,
            is_fullscreen,
            is_xwayland: false,
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

    pub fn snapshot_layer_surface(
        &self,
        output_name: &str,
        layer: &LayerSurface,
        geometry: smithay::utils::Rectangle<i32, smithay::utils::Logical>,
    ) -> WaylandLayerSnapshot {
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

pub fn layer_runtime_id(layer: &LayerSurface) -> String {
    let surface = layer.wl_surface();
    let protocol_id = surface.id().protocol_id();
    let client_id = surface
        .client()
        .map(|client| format!("{:?}", client.id()))
        .unwrap_or_else(|| "unknown-client".to_string());
    format!("{client_id}:{protocol_id}")
}

#[cfg(test)]
mod tests {
    use super::WaylandWindowAction;

    #[test]
    fn wayland_window_actions_serialize_to_camel_case_strings() {
        let close = serde_json::to_string(&WaylandWindowAction::Close).expect("serialize close");
        let finalize_close = serde_json::to_string(&WaylandWindowAction::FinalizeClose)
            .expect("serialize finalize close");
        let maximize =
            serde_json::to_string(&WaylandWindowAction::Maximize).expect("serialize maximize");
        let minimize =
            serde_json::to_string(&WaylandWindowAction::Minimize).expect("serialize minimize");

        assert_eq!(close, "\"close\"");
        assert_eq!(finalize_close, "\"finalizeClose\"");
        assert_eq!(maximize, "\"maximize\"");
        assert_eq!(minimize, "\"minimize\"");
    }
}
