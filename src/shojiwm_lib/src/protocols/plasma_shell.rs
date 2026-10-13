//! `org_kde_plasma_shell` server (KDE's plasma-shell.xml).
//!
//! plasmashell gives each of its own windows (Kickoff, tray popups, the
//! calendar, OSDs, notifications, tooltips) a `org_kde_plasma_surface`. On it
//! the shell says what the window is (`set_role`) and where it must appear in
//! global coordinates (`set_position`).
//!
//! This module only records those requests in the surface's data map. The
//! window snapshot carries them to the TypeScript config as `window.plasma`,
//! and the config decides what to do: place the window, skip tiling and
//! decorations, keep it above normal windows.

use std::sync::Mutex;

use serde::Serialize;
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
    protocol::wl_surface::WlSurface,
};
use smithay::wayland::compositor::with_states;
use wayland_protocols_plasma::plasma_shell::server::{
    org_kde_plasma_shell::{self, OrgKdePlasmaShell},
    org_kde_plasma_surface::{self, OrgKdePlasmaSurface},
};

use crate::state::ShojiWM;

/// KWin advertises 8 too: every request up to `open_under_cursor`.
const VERSION: u32 = 8;

/// What a plasma surface is, from `org_kde_plasma_surface.set_role`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PlasmaRole {
    #[default]
    Normal,
    Desktop,
    Panel,
    OnScreenDisplay,
    Notification,
    Tooltip,
    CriticalNotification,
    AppletPopup,
}

impl PlasmaRole {
    /// Map the wire value of the `role` enum. Unknown values count as normal.
    fn from_wire(value: u32) -> Self {
        match value {
            1 => Self::Desktop,
            2 => Self::Panel,
            3 => Self::OnScreenDisplay,
            4 => Self::Notification,
            5 => Self::Tooltip,
            6 => Self::CriticalNotification,
            7 => Self::AppletPopup,
            _ => Self::Normal,
        }
    }
}

/// Everything plasmashell asked for one surface. Lives in the surface's
/// data map, so it exists before and after the window is known.
#[derive(Debug, Clone, Default)]
struct PlasmaSurfaceData {
    role: PlasmaRole,
    /// Global logical position from `set_position` or `open_under_cursor`.
    position: Option<(i32, i32)>,
    skip_taskbar: bool,
    skip_switcher: bool,
    panel_takes_focus: bool,
}

/// The TypeScript view of [`PlasmaSurfaceData`] (`window.plasma`). Flat on
/// purpose: the TS side compares it with a shallow equality check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlasmaSurfaceSnapshot {
    pub role: PlasmaRole,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub x: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub y: Option<i32>,
    pub skip_taskbar: bool,
    pub skip_switcher: bool,
    pub panel_takes_focus: bool,
}

impl PlasmaSurfaceSnapshot {
    /// Windows with a role other than `normal` belong to the shell: no
    /// taskbar entry, no window switcher, no tiling.
    pub fn is_shell_window(&self) -> bool {
        self.role != PlasmaRole::Normal
    }
}

/// The plasma surface state of `surface`, or `None` when plasmashell never
/// called `get_surface` for it.
pub fn plasma_surface_snapshot(surface: &WlSurface) -> Option<PlasmaSurfaceSnapshot> {
    with_states(surface, |states| {
        let data = states.data_map.get::<Mutex<PlasmaSurfaceData>>()?;
        let data = data.lock().unwrap();
        Some(PlasmaSurfaceSnapshot {
            role: data.role,
            x: data.position.map(|(x, _)| x),
            y: data.position.map(|(_, y)| y),
            skip_taskbar: data.skip_taskbar,
            skip_switcher: data.skip_switcher,
            panel_takes_focus: data.panel_takes_focus,
        })
    })
}

fn update_surface_data(surface: &WlSurface, update: impl FnOnce(&mut PlasmaSurfaceData)) {
    with_states(surface, |states| {
        states
            .data_map
            .insert_if_missing_threadsafe(|| Mutex::new(PlasmaSurfaceData::default()));
        let data = states.data_map.get::<Mutex<PlasmaSurfaceData>>().unwrap();
        update(&mut data.lock().unwrap());
    });
}

pub struct PlasmaShellState;

impl PlasmaShellState {
    pub fn new(display: &DisplayHandle) -> Self {
        display.create_global::<ShojiWM, OrgKdePlasmaShell, _>(VERSION, ());
        Self
    }
}

impl GlobalDispatch<OrgKdePlasmaShell, ()> for ShojiWM {
    fn bind(
        _state: &mut Self,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<OrgKdePlasmaShell>,
        _global_data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }
}

impl Dispatch<OrgKdePlasmaShell, ()> for ShojiWM {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &OrgKdePlasmaShell,
        request: org_kde_plasma_shell::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        if let org_kde_plasma_shell::Request::GetSurface { id, surface } = request {
            update_surface_data(&surface, |_| {});
            data_init.init(id, surface);
        }
    }
}

impl Dispatch<OrgKdePlasmaSurface, WlSurface> for ShojiWM {
    fn request(
        state: &mut Self,
        _client: &Client,
        resource: &OrgKdePlasmaSurface,
        request: org_kde_plasma_surface::Request,
        surface: &WlSurface,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        use org_kde_plasma_surface::Request;
        match request {
            Request::SetPosition { x, y } => {
                update_surface_data(surface, |data| data.position = Some((x, y)));
            }
            Request::OpenUnderCursor => {
                let pointer = state.seat.get_pointer().map(|p| p.current_location());
                if let Some(location) = pointer {
                    let position = (location.x.round() as i32, location.y.round() as i32);
                    update_surface_data(surface, |data| data.position = Some(position));
                }
            }
            Request::SetRole { role } => {
                update_surface_data(surface, |data| data.role = PlasmaRole::from_wire(role));
            }
            Request::SetSkipTaskbar { skip } => {
                update_surface_data(surface, |data| data.skip_taskbar = skip != 0);
            }
            Request::SetSkipSwitcher { skip } => {
                update_surface_data(surface, |data| data.skip_switcher = skip != 0);
            }
            Request::SetPanelTakesFocus { takes_focus } => {
                update_surface_data(surface, |data| data.panel_takes_focus = takes_focus != 0);
            }
            // ShojiWM does not hide panels itself. Confirm at once so an
            // auto-hide panel does not wait for an answer that never comes.
            Request::PanelAutoHideHide => resource.auto_hidden_panel_hidden(),
            Request::PanelAutoHideShow => resource.auto_hidden_panel_shown(),
            // set_output (no-op in KWin too), set_panel_behavior, destroy.
            _ => return,
        }
        state.plasma_surface_changed(surface);
    }
}

impl ShojiWM {
    /// A plasma surface request changed what the config must know about the
    /// window of `surface`: hand the window back to the runtime and update
    /// the taskbar flags.
    fn plasma_surface_changed(&mut self, surface: &WlSurface) {
        let Some(window) = self
            .space
            .elements()
            .find(|window| Self::window_matches_root_surface(window, surface))
            .cloned()
        else {
            // Not a toplevel (yet), e.g. a layer-shell panel. The state waits
            // in the surface's data map until a snapshot reads it.
            return;
        };
        let window_id = self.snapshot_window(&window).id;
        tracing::debug!(
            window_id,
            surface = ?surface.id(),
            plasma = ?plasma_surface_snapshot(surface),
            "plasma surface changed"
        );
        self.runtime_dirty_window_ids.insert(window_id.clone());
        self.runtime_managed_only_window_ids.remove(&window_id);
        self.runtime_node_only_window_ids.remove(&window_id);
        self.runtime_poll_dirty = true;
        self.request_tty_maintenance("plasma-surface-changed");
        self.schedule_redraw();
        // The decoration policy may depend on the role (shell windows get none).
        self.mark_window_decoration_metadata_changed(surface);
        self.sync_plasma_window(&window);
    }
}
