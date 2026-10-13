//! `org_kde_plasma_virtual_desktop_management` server (KDE's
//! org-kde-plasma-virtual-desktop.xml).
//!
//! plasmashell's Pager (and the taskbar's "move to desktop") read KWin's
//! virtual desktops through this protocol. ShojiWM has workspaces instead;
//! the TypeScript config says which workspaces to show as desktops
//! (`virtualDesktops` in `COMPOSITOR.workspace.configure`).
//!
//! ```text
//!   config ──> desktops [id, name, active, windows]
//!                 │
//!   manager:  desktop_created(id, position) ... rows ... done
//!   client:   get_virtual_desktop(id) ──> desktop_id, name, activated, done
//!   click:    request_activate ──> workspace.event.onActivate(id)
//! ```
//!
//! Which desktop a window is on goes out on its `org_kde_plasma_window`
//! (`virtual_desktop_entered` / `_left`, see `plasma_window_management.rs`).

use std::collections::{HashMap, HashSet};

use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
    Weak as WaylandWeak, backend::ClientId,
};
use wayland_protocols_plasma::plasma_virtual_desktop::server::{
    org_kde_plasma_virtual_desktop::{self, OrgKdePlasmaVirtualDesktop},
    org_kde_plasma_virtual_desktop_management::{self, OrgKdePlasmaVirtualDesktopManagement},
};

use crate::{
    runtime_workspace::{RuntimeVirtualDesktopConfig, RuntimeWorkspaceActivateRequestSnapshot},
    state::ShojiWM,
};

/// KWin's version (2 adds `rows`).
const VERSION: u32 = 2;

struct Desktop {
    id: String,
    name: String,
    active: bool,
    resources: Vec<WaylandWeak<OrgKdePlasmaVirtualDesktop>>,
}

impl Desktop {
    fn live_resources(&mut self) -> Vec<OrgKdePlasmaVirtualDesktop> {
        self.resources.retain(|resource| resource.upgrade().is_ok());
        self.resources
            .iter()
            .filter_map(|resource| resource.upgrade().ok())
            .collect()
    }

    /// Everything about the desktop, for a new desktop object.
    fn send_all(&self, resource: &OrgKdePlasmaVirtualDesktop) {
        resource.desktop_id(self.id.clone());
        resource.name(self.name.clone());
        if self.active {
            resource.activated();
        }
        resource.done();
    }
}

pub struct PlasmaVirtualDesktopState {
    managers: Vec<OrgKdePlasmaVirtualDesktopManagement>,
    /// In order: a desktop's position is its index.
    desktops: Vec<Desktop>,
    rows: u32,
    /// Window id to the id of the desktop it is on.
    window_desktops: HashMap<String, String>,
    minimized_windows: HashSet<String>,
    /// False until the config sends desktops: windows stay on all desktops.
    configured: bool,
}

impl PlasmaVirtualDesktopState {
    pub fn new(display: &DisplayHandle) -> Self {
        display.create_global::<ShojiWM, OrgKdePlasmaVirtualDesktopManagement, _>(VERSION, ());
        Self {
            managers: Vec::new(),
            desktops: Vec::new(),
            rows: 1,
            window_desktops: HashMap::new(),
            minimized_windows: HashSet::new(),
            configured: false,
        }
    }

    /// The desktop the window is on, `None` when it is on all of them (no
    /// desktops configured, or a window no workspace holds, like plasmashell's).
    pub fn desktop_of(&self, window_id: &str) -> Option<String> {
        self.window_desktops.get(window_id).cloned()
    }

    /// True for the desktop being shown.
    pub fn is_active(&self, desktop_id: &str) -> bool {
        self.desktops
            .iter()
            .any(|desktop| desktop.id == desktop_id && desktop.active)
    }

    /// For a window on a desktop: whether it is really minimized.
    pub fn is_minimized(&self, window_id: &str) -> bool {
        self.minimized_windows.contains(window_id)
    }

    /// Apply the config's desktops and tell the clients what changed.
    /// Returns true when windows moved between desktops or minimized.
    pub fn sync(&mut self, config: RuntimeVirtualDesktopConfig) -> bool {
        self.managers.retain(|manager| manager.is_alive());
        let mut changed = !self.configured;
        self.configured = true;

        // Desktops that are gone.
        let next_ids: HashSet<&str> = config.desktops.iter().map(|desktop| desktop.id.as_str()).collect();
        let mut kept = Vec::new();
        for mut desktop in std::mem::take(&mut self.desktops) {
            if next_ids.contains(desktop.id.as_str()) {
                kept.push(desktop);
                continue;
            }
            for resource in desktop.live_resources() {
                resource.removed();
            }
            for manager in &self.managers {
                manager.desktop_removed(desktop.id.clone());
            }
            changed = true;
        }

        // New and changed desktops, in the new order.
        let mut next = Vec::new();
        for (position, entry) in config.desktops.iter().enumerate() {
            match kept.iter().position(|desktop| desktop.id == entry.id) {
                Some(index) => {
                    let mut desktop = kept.remove(index);
                    let resources = desktop.live_resources();
                    let mut desktop_changed = false;
                    if desktop.name != entry.name {
                        desktop.name = entry.name.clone();
                        for resource in &resources {
                            resource.name(entry.name.clone());
                        }
                        desktop_changed = true;
                    }
                    if desktop.active != entry.active {
                        desktop.active = entry.active;
                        for resource in &resources {
                            if entry.active {
                                resource.activated();
                            } else {
                                resource.deactivated();
                            }
                        }
                        desktop_changed = true;
                    }
                    if desktop_changed {
                        for resource in &resources {
                            resource.done();
                        }
                        changed = true;
                    }
                    next.push(desktop);
                }
                None => {
                    for manager in &self.managers {
                        manager.desktop_created(entry.id.clone(), position as u32);
                    }
                    next.push(Desktop {
                        id: entry.id.clone(),
                        name: entry.name.clone(),
                        active: entry.active,
                        resources: Vec::new(),
                    });
                    changed = true;
                }
            }
        }
        self.desktops = next;

        let rows = config.rows.unwrap_or(1).max(1);
        if rows != self.rows {
            self.rows = rows;
            for manager in &self.managers {
                if manager.version() >= 2 {
                    manager.rows(rows);
                }
            }
            changed = true;
        }
        if changed {
            for manager in &self.managers {
                manager.done();
            }
        }

        let window_desktops: HashMap<String, String> = config
            .desktops
            .iter()
            .flat_map(|desktop| {
                desktop
                    .window_ids
                    .iter()
                    .map(|window_id| (window_id.clone(), desktop.id.clone()))
            })
            .collect();
        let minimized_windows: HashSet<String> = config.minimized_window_ids.into_iter().collect();
        let windows_changed =
            window_desktops != self.window_desktops || minimized_windows != self.minimized_windows;
        self.window_desktops = window_desktops;
        self.minimized_windows = minimized_windows;
        windows_changed
    }

    fn announce_all(&self, manager: &OrgKdePlasmaVirtualDesktopManagement) {
        for (position, desktop) in self.desktops.iter().enumerate() {
            manager.desktop_created(desktop.id.clone(), position as u32);
        }
        if manager.version() >= 2 {
            manager.rows(self.rows);
        }
        manager.done();
    }
}

impl GlobalDispatch<OrgKdePlasmaVirtualDesktopManagement, ()> for ShojiWM {
    fn bind(
        state: &mut Self,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<OrgKdePlasmaVirtualDesktopManagement>,
        _global_data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        let manager = data_init.init(resource, ());
        let desktops = &mut state.plasma_virtual_desktop_state;
        desktops.announce_all(&manager);
        desktops.managers.push(manager);
    }
}

impl Dispatch<OrgKdePlasmaVirtualDesktopManagement, ()> for ShojiWM {
    fn request(
        state: &mut Self,
        _client: &Client,
        _manager: &OrgKdePlasmaVirtualDesktopManagement,
        request: org_kde_plasma_virtual_desktop_management::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        use org_kde_plasma_virtual_desktop_management::Request;
        match request {
            Request::GetVirtualDesktop { id, desktop_id } => {
                let resource = data_init.init(id, desktop_id.clone());
                let desktops = &mut state.plasma_virtual_desktop_state;
                match desktops.desktops.iter_mut().find(|desktop| desktop.id == desktop_id) {
                    Some(desktop) => {
                        desktop.send_all(&resource);
                        desktop.resources.push(resource.downgrade());
                    }
                    // Gone already (or never was).
                    None => resource.removed(),
                }
            }
            // Workspaces come and go with their windows: adding or removing
            // a desktop by hand is not supported.
            Request::RequestCreateVirtualDesktop { .. } => {}
            Request::RequestRemoveVirtualDesktop { .. } => {}
            _ => {}
        }
    }

    fn destroyed(
        state: &mut Self,
        _client: ClientId,
        resource: &OrgKdePlasmaVirtualDesktopManagement,
        _data: &(),
    ) {
        state
            .plasma_virtual_desktop_state
            .managers
            .retain(|manager| manager != resource);
    }
}

impl Dispatch<OrgKdePlasmaVirtualDesktop, String> for ShojiWM {
    fn request(
        state: &mut Self,
        _client: &Client,
        _resource: &OrgKdePlasmaVirtualDesktop,
        request: org_kde_plasma_virtual_desktop::Request,
        desktop_id: &String,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        use org_kde_plasma_virtual_desktop::Request;
        if let Request::RequestActivate = request {
            // The same path as an ext-workspace bar click.
            state.runtime_workspace_activate(RuntimeWorkspaceActivateRequestSnapshot {
                workspace_id: desktop_id.clone(),
                group_id: None,
                window_id: None,
            });
        }
    }

    fn destroyed(
        _state: &mut Self,
        _client: ClientId,
        _resource: &OrgKdePlasmaVirtualDesktop,
        _desktop_id: &String,
    ) {
        // Dead resources are dropped on the next update.
    }
}

impl ShojiWM {
    /// The config's virtual desktops changed: tell the desktop clients, and
    /// the taskbar which window is on which desktop.
    pub fn consume_runtime_virtual_desktop_config(&mut self, config: RuntimeVirtualDesktopConfig) {
        if self.plasma_virtual_desktop_state.sync(config) {
            self.sync_plasma_window_states();
        }
    }

    /// A Pager drag or the taskbar's "move to desktop": move the window to
    /// that desktop's workspace.
    pub(crate) fn move_window_to_virtual_desktop(&mut self, window_id: String, desktop_id: String) {
        self.runtime_workspace_activate(RuntimeWorkspaceActivateRequestSnapshot {
            workspace_id: desktop_id,
            group_id: None,
            window_id: Some(window_id),
        });
    }
}
