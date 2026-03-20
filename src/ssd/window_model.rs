use serde::Serialize;
use smithay::{
    desktop::Window,
    reexports::{
        wayland_protocols::xdg::shell::server::xdg_toplevel,
        wayland_server::Resource,
    },
    wayland::{
        compositor::with_states,
        shell::xdg::XdgToplevelSurfaceData,
    },
};

use crate::state::ShojiWM;
use super::DecorationInteractionSnapshot;

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

            (
                role.title.clone().unwrap_or_default(),
                role.app_id.clone(),
            )
        });

        let (is_focused, is_maximized, is_fullscreen) = toplevel.with_pending_state(|state| {
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
            let protocol_id = toplevel.wl_surface().id().protocol_id();
            let client_id = toplevel
                .wl_surface()
                .client()
                .map(|client| format!("{:?}", client.id()))
                .unwrap_or_else(|| "unknown-client".to_string());
            format!("{client_id}:{protocol_id}")
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
}

#[cfg(test)]
mod tests {
    use super::WaylandWindowAction;

    #[test]
    fn wayland_window_actions_serialize_to_camel_case_strings() {
        let close = serde_json::to_string(&WaylandWindowAction::Close).expect("serialize close");
        let finalize_close =
            serde_json::to_string(&WaylandWindowAction::FinalizeClose).expect("serialize finalize close");
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
