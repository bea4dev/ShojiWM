//! `org_kde_plasma_window_management` server (KDE's plasma-window-management.xml).
//!
//! KDE's task manager library (libtaskmanager, behind plasmashell's taskbar)
//! lists windows only through this protocol. Without it the taskbar stays
//! empty and plasmashell logs "The PlasmaWindowManagement protocol hasn't
//! activated in time".
//!
//! It is a close cousin of `zwlr_foreign_toplevel_manager_v1`
//! (`wlr_foreign_toplevel.rs`) and follows the same life cycle: a handle is
//! made when a window maps, kept in sync, and closed when the window goes.
//!
//! The protocol works in two steps:
//!
//! ```text
//!   window maps ──> manager.window_with_uuid(uuid)
//!                       │
//!   client:  manager.get_window_by_uuid(uuid) ──> new org_kde_plasma_window
//!                       │
//!   server:  title, app_id, pid, state, geometry ... initial_state
//!   later:   *_changed events, and `unmapped` when the window closes
//! ```
//!
//! Taskbar requests (activate, minimize, maximize, close) go through the same
//! runtime events as a wlr taskbar click, so the TypeScript window manager
//! stays the owner of window state.

use std::sync::{Arc, Mutex};

use smithay::{
    desktop::{Window, WindowSurfaceType, layer_map_for_output},
    reexports::wayland_server::{
        Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
        Weak as WaylandWeak, backend::ClientId, protocol::wl_surface::WlSurface,
    },
};
use wayland_protocols_plasma::plasma_window_management::server::{
    org_kde_plasma_stacking_order::OrgKdePlasmaStackingOrder,
    org_kde_plasma_window::{self, OrgKdePlasmaWindow},
    org_kde_plasma_window_management::{self, OrgKdePlasmaWindowManagement},
};

use crate::{ssd::WindowPositionSnapshot, state::ShojiWM};

/// KWin's version. libtaskmanager in Plasma 6 asks for 17+ (it uses
/// `get_stacking_order`); 18 only adds the `client_geometry` event.
const VERSION: u32 = 18;

/// Bits of the protocol's `state` enum.
mod flag {
    pub const ACTIVE: u32 = 0x1;
    pub const MINIMIZED: u32 = 0x2;
    pub const MAXIMIZED: u32 = 0x4;
    pub const FULLSCREEN: u32 = 0x8;
    pub const ON_ALL_DESKTOPS: u32 = 0x40;
    pub const CLOSEABLE: u32 = 0x100;
    pub const MINIMIZABLE: u32 = 0x200;
    pub const MAXIMIZABLE: u32 = 0x400;
    pub const FULLSCREENABLE: u32 = 0x800;
    pub const SKIP_TASKBAR: u32 = 0x1000;
    pub const MOVABLE: u32 = 0x8000;
    pub const RESIZABLE: u32 = 0x10000;
    pub const SKIP_SWITCHER: u32 = 0x40000;
}

/// A rect in global logical coordinates: x, y, width, height.
type Rect = (i32, i32, i32, i32);

/// The window properties the protocol carries.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct PlasmaWindowInfo {
    title: String,
    app_id: String,
    pid: u32,
    /// Bitfield of [`flag`] values.
    flags: u32,
    /// The whole window, decorations included (`geometry`).
    geometry: Rect,
    /// The client area only (`client_geometry`, version 18).
    client_geometry: Rect,
}

/// One window as the protocol sees it. Lives in the window's user data.
#[derive(Clone)]
pub struct PlasmaWindowHandle {
    inner: Arc<Mutex<PlasmaWindowInner>>,
}

struct PlasmaWindowInner {
    /// The deprecated numeric id, still sent in `window_with_uuid`.
    internal_id: u32,
    uuid: String,
    /// What the clients were last told.
    info: PlasmaWindowInfo,
    closed: bool,
    resources: Vec<WaylandWeak<OrgKdePlasmaWindow>>,
    /// The window's taskbar entry (`set_minimized_geometry`), relative to the
    /// panel surface that holds it. One per panel; the last one set wins.
    /// The minimize animation flies the window there.
    minimized_geometries: Vec<(WaylandWeak<WlSurface>, Rect)>,
}

impl PlasmaWindowHandle {
    fn new(internal_id: u32, uuid: String) -> Self {
        Self {
            inner: Arc::new(Mutex::new(PlasmaWindowInner {
                internal_id,
                uuid,
                info: PlasmaWindowInfo::default(),
                closed: false,
                resources: Vec::new(),
                minimized_geometries: Vec::new(),
            })),
        }
    }

    /// A handle for a uuid nobody knows: its window objects only ever get
    /// `unmapped`.
    fn closed_placeholder() -> Self {
        let handle = Self::new(0, String::new());
        handle.inner.lock().unwrap().closed = true;
        handle
    }

    fn internal_id(&self) -> u32 {
        self.inner.lock().unwrap().internal_id
    }

    fn uuid(&self) -> String {
        self.inner.lock().unwrap().uuid.clone()
    }

    fn is_closed(&self) -> bool {
        self.inner.lock().unwrap().closed
    }

    fn same_as(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }

    /// True when some client holds a window object for this window, so
    /// changes must be sent.
    fn has_resources(&self) -> bool {
        let mut inner = self.inner.lock().unwrap();
        inner.resources.retain(|resource| resource.upgrade().is_ok());
        !inner.resources.is_empty()
    }

    /// Send the full state to a new window object and keep it for updates.
    fn add_resource(&self, resource: OrgKdePlasmaWindow, info: PlasmaWindowInfo) {
        let mut inner = self.inner.lock().unwrap();
        if inner.closed {
            resource.unmapped();
            return;
        }
        send_title(&resource, &info);
        send_app_id(&resource, &info);
        resource.pid_changed(info.pid);
        resource.state_changed(info.flags);
        send_geometry(&resource, &info);
        send_client_geometry(&resource, &info);
        if resource.version() >= 4 {
            resource.initial_state();
        }
        // Other window objects must not miss a change because the new info
        // replaced theirs: tell them first.
        send_changes(&live_resources(&inner), &inner.info, &info);
        inner.info = info;
        inner.resources.push(resource.downgrade());
    }

    /// Send what changed since the last update to every window object.
    fn update(&self, info: PlasmaWindowInfo) {
        let mut inner = self.inner.lock().unwrap();
        if inner.closed || inner.info == info {
            return;
        }
        send_changes(&live_resources(&inner), &inner.info, &info);
        inner.info = info;
    }

    fn close(&self) {
        let resources = {
            let mut inner = self.inner.lock().unwrap();
            if inner.closed {
                return;
            }
            inner.closed = true;
            let resources = live_resources(&inner);
            inner.resources.clear();
            resources
        };
        for resource in resources {
            resource.unmapped();
        }
    }

    /// Remember (`Some`) or forget (`None`) the taskbar entry on `panel`.
    /// Returns true when it changed.
    fn set_minimized_geometry(&self, panel: &WlSurface, rect: Option<Rect>) -> bool {
        let mut inner = self.inner.lock().unwrap();
        let before = inner.minimized_geometries.clone();
        inner
            .minimized_geometries
            .retain(|(surface, _)| surface.upgrade().is_ok_and(|surface| surface != *panel));
        if let Some(rect) = rect {
            inner.minimized_geometries.push((panel.downgrade(), rect));
        }
        inner.minimized_geometries != before
    }

    /// The last taskbar entry whose panel still exists.
    fn minimized_geometry(&self) -> Option<(WlSurface, Rect)> {
        let inner = self.inner.lock().unwrap();
        inner
            .minimized_geometries
            .iter()
            .rev()
            .find_map(|(surface, rect)| surface.upgrade().ok().map(|surface| (surface, *rect)))
    }

    fn remove_resource(&self, resource: &OrgKdePlasmaWindow) {
        self.inner.lock().unwrap().resources.retain(|candidate| {
            candidate
                .upgrade()
                .is_ok_and(|candidate| candidate != *resource)
        });
    }
}

fn live_resources(inner: &PlasmaWindowInner) -> Vec<OrgKdePlasmaWindow> {
    inner
        .resources
        .iter()
        .filter_map(|resource| resource.upgrade().ok())
        .collect()
}

fn send_title(resource: &OrgKdePlasmaWindow, info: &PlasmaWindowInfo) {
    resource.title_changed(info.title.clone());
}

fn send_app_id(resource: &OrgKdePlasmaWindow, info: &PlasmaWindowInfo) {
    resource.app_id_changed(info.app_id.clone());
}

fn send_geometry(resource: &OrgKdePlasmaWindow, info: &PlasmaWindowInfo) {
    if resource.version() >= 6 {
        let (x, y, width, height) = info.geometry;
        resource.geometry(x, y, width.max(0) as u32, height.max(0) as u32);
    }
}

fn send_client_geometry(resource: &OrgKdePlasmaWindow, info: &PlasmaWindowInfo) {
    if resource.version() >= 18 {
        let (x, y, width, height) = info.client_geometry;
        resource.client_geometry(x, y, width.max(0) as u32, height.max(0) as u32);
    }
}

fn send_changes(resources: &[OrgKdePlasmaWindow], old: &PlasmaWindowInfo, new: &PlasmaWindowInfo) {
    for resource in resources {
        if old.title != new.title {
            send_title(resource, new);
        }
        if old.app_id != new.app_id {
            send_app_id(resource, new);
        }
        if old.pid != new.pid {
            resource.pid_changed(new.pid);
        }
        if old.flags != new.flags {
            resource.state_changed(new.flags);
        }
        if old.geometry != new.geometry {
            send_geometry(resource, new);
        }
        if old.client_geometry != new.client_geometry {
            send_client_geometry(resource, new);
        }
    }
}

pub struct PlasmaWindowManagementState {
    managers: Vec<OrgKdePlasmaWindowManagement>,
    /// Live windows, oldest first. This is also the stacking order the
    /// clients get: ShojiWM's real stacking lives in the TypeScript config.
    handles: Vec<PlasmaWindowHandle>,
    next_internal_id: u32,
}

impl PlasmaWindowManagementState {
    pub fn new(display: &DisplayHandle) -> Self {
        display.create_global::<ShojiWM, OrgKdePlasmaWindowManagement, _>(VERSION, ());
        Self {
            managers: Vec::new(),
            handles: Vec::new(),
            next_internal_id: 1,
        }
    }

    fn announce(manager: &OrgKdePlasmaWindowManagement, handle: &PlasmaWindowHandle) {
        if manager.version() >= 13 {
            manager.window_with_uuid(handle.internal_id(), handle.uuid());
        } else {
            manager.window(handle.internal_id());
        }
    }

    /// Tell `manager` the stacking order changed, in the newest form it
    /// understands.
    fn send_stacking_order(&self, manager: &OrgKdePlasmaWindowManagement) {
        let version = manager.version();
        if version >= 17 {
            // The client fetches the list itself with `get_stacking_order`.
            manager.stacking_order_changed_2();
        } else if version >= 12 {
            let uuids: Vec<String> = self.handles.iter().map(PlasmaWindowHandle::uuid).collect();
            manager.stacking_order_uuid_changed(uuids.join(";"));
        } else if version >= 11 {
            let ids: Vec<u8> = self
                .handles
                .iter()
                .flat_map(|handle| handle.internal_id().to_ne_bytes())
                .collect();
            manager.stacking_order_changed(ids);
        }
    }

    fn windows_changed(&self) {
        for manager in &self.managers {
            self.send_stacking_order(manager);
        }
    }

    fn find_handle(&self, matches: impl Fn(&PlasmaWindowHandle) -> bool) -> Option<PlasmaWindowHandle> {
        self.handles
            .iter()
            .find(|handle| !handle.is_closed() && matches(handle))
            .cloned()
    }
}

impl ShojiWM {
    /// Announce a newly mapped window. Extra calls are no-ops.
    pub fn install_plasma_window(&mut self, window: &Window) {
        if window.user_data().get::<PlasmaWindowHandle>().is_some() {
            return;
        }
        let uuid = self.snapshot_window(window).id;
        let state = &mut self.plasma_window_management_state;
        let handle = PlasmaWindowHandle::new(state.next_internal_id, uuid);
        state.next_internal_id = state.next_internal_id.wrapping_add(1).max(1);
        state.handles.push(handle.clone());
        for manager in &state.managers {
            PlasmaWindowManagementState::announce(manager, &handle);
        }
        state.windows_changed();
        window.user_data().insert_if_missing(|| handle);
    }

    /// Send the window's current title, state and geometry to the clients
    /// that hold it. Cheap when nobody does.
    pub fn sync_plasma_window(&mut self, window: &Window) {
        let Some(handle) = window.user_data().get::<PlasmaWindowHandle>().cloned() else {
            return;
        };
        if !handle.has_resources() {
            return;
        }
        handle.update(self.plasma_window_info(window));
    }

    /// [`Self::sync_plasma_window`] for every window: focus, minimize and
    /// geometry changes land here.
    pub fn sync_plasma_window_states(&mut self) {
        if self.plasma_window_management_state.handles.is_empty() {
            return;
        }
        let windows: Vec<Window> = self.space.elements().cloned().collect();
        for window in windows {
            self.sync_plasma_window(&window);
        }
    }

    /// Send `unmapped` and forget the window.
    pub fn remove_plasma_window(&mut self, window: &Window) {
        let Some(handle) = window.user_data().get::<PlasmaWindowHandle>().cloned() else {
            return;
        };
        handle.close();
        let state = &mut self.plasma_window_management_state;
        let count = state.handles.len();
        state.handles.retain(|candidate| !candidate.same_as(&handle));
        if state.handles.len() != count {
            state.windows_changed();
        }
    }

    fn plasma_window_for_handle(&self, handle: &PlasmaWindowHandle) -> Option<Window> {
        self.space
            .elements()
            .find(|window| {
                window
                    .user_data()
                    .get::<PlasmaWindowHandle>()
                    .is_some_and(|candidate| candidate.same_as(handle))
            })
            .cloned()
    }

    fn plasma_window_info(&self, window: &Window) -> PlasmaWindowInfo {
        let snapshot = self.snapshot_window(window);
        // Same rule as the wlr taskbar: an idle window is minimized (or on
        // another workspace, which a taskbar shows the same way).
        let minimized = self
            .window_decorations
            .get(window)
            .is_some_and(|decoration| decoration.managed_window.idle);
        let shell_window = snapshot
            .plasma
            .as_ref()
            .is_some_and(|plasma| plasma.is_shell_window());

        // ShojiWM has no virtual desktops, so every window is on all of them.
        let mut flags = flag::ON_ALL_DESKTOPS;
        let mut set = |bit: u32, on: bool| {
            if on {
                flags |= bit;
            }
        };
        set(flag::ACTIVE, snapshot.is_focused);
        set(flag::MINIMIZED, minimized);
        set(flag::MAXIMIZED, snapshot.is_maximized);
        set(flag::FULLSCREEN, snapshot.is_fullscreen);
        if shell_window {
            // plasmashell's own popups, panels and notifications.
            set(flag::SKIP_TASKBAR | flag::SKIP_SWITCHER, true);
        } else {
            set(
                flag::CLOSEABLE
                    | flag::MINIMIZABLE
                    | flag::MAXIMIZABLE
                    | flag::FULLSCREENABLE
                    | flag::MOVABLE,
                true,
            );
            set(flag::RESIZABLE, snapshot.is_resizable);
            if let Some(plasma) = snapshot.plasma.as_ref() {
                set(flag::SKIP_TASKBAR, plasma.skip_taskbar);
                set(flag::SKIP_SWITCHER, plasma.skip_switcher);
            }
        }

        let rect = |r: &crate::ssd::WindowPositionSnapshot| -> Rect {
            (
                r.x.round() as i32,
                r.y.round() as i32,
                r.width.round() as i32,
                r.height.round() as i32,
            )
        };
        PlasmaWindowInfo {
            title: snapshot.title.clone(),
            app_id: snapshot.app_id.clone().unwrap_or_default(),
            pid: self.window_pid(window),
            flags,
            geometry: rect(&snapshot.rect),
            client_geometry: rect(&snapshot.position),
        }
    }

    /// Where the window's taskbar entry is, in global logical coordinates:
    /// the `set_minimized_geometry` rect moved by its panel's position.
    /// `None` when no taskbar set one, or its panel is gone.
    pub fn plasma_minimized_geometry(&self, window: &Window) -> Option<WindowPositionSnapshot> {
        let handle = window.user_data().get::<PlasmaWindowHandle>()?;
        let (panel, (x, y, width, height)) = handle.minimized_geometry()?;
        let (origin_x, origin_y) = self.surface_origin(&panel)?;
        Some(WindowPositionSnapshot {
            x: (origin_x + x) as f64,
            y: (origin_y + y) as f64,
            width: width as f64,
            height: height as f64,
        })
    }

    /// The global logical position of a panel surface's top-left corner.
    /// plasmashell's panels are toplevel windows; a layer-shell panel works too.
    fn surface_origin(&self, surface: &WlSurface) -> Option<(i32, i32)> {
        if let Some(window) = self
            .space
            .elements()
            .find(|window| Self::window_matches_root_surface(window, surface))
        {
            // The element location is the surface's corner (the window
            // snapshot adds the geometry offset to it for `position`).
            let loc = self.space.element_location(window)?;
            return Some((loc.x, loc.y));
        }
        for output in self.space.outputs() {
            let map = layer_map_for_output(output);
            let Some(layer) = map.layer_for_surface(surface, WindowSurfaceType::TOPLEVEL) else {
                continue;
            };
            let geometry = map.layer_geometry(layer)?;
            let output_loc = self.space.output_geometry(output)?.loc;
            return Some((output_loc.x + geometry.loc.x, output_loc.y + geometry.loc.y));
        }
        None
    }

    /// A taskbar entry moved: send the window's snapshot to the runtime again.
    fn plasma_minimized_geometry_changed(&mut self, window: &Window) {
        let window_id = self.snapshot_window(window).id;
        self.runtime_dirty_window_ids.insert(window_id.clone());
        self.runtime_managed_only_window_ids.remove(&window_id);
        self.runtime_node_only_window_ids.remove(&window_id);
        self.runtime_poll_dirty = true;
    }

    /// The process id of the window's client, or 0 when unknown.
    fn window_pid(&self, window: &Window) -> u32 {
        if let Some(x11) = window.x11_surface() {
            return x11.pid().unwrap_or(0);
        }
        window
            .toplevel()
            .and_then(|toplevel| toplevel.wl_surface().client())
            .and_then(|client| client.get_credentials(&self.display_handle).ok())
            .map(|credentials| credentials.pid.max(0) as u32)
            .unwrap_or(0)
    }

    /// `set_state`: each bit in `changed` is a state the client wants set
    /// (bit also in `state`) or cleared.
    fn plasma_window_set_state(&mut self, window: &Window, changed: u32, state: u32) {
        let wants = |bit: u32| state & bit != 0;
        let api = crate::ssd::WindowStateRequestSourceSnapshot::Api;
        if changed & flag::MINIMIZED != 0 {
            self.minimize_window_from_taskbar(window, wants(flag::MINIMIZED));
        }
        if changed & flag::MAXIMIZED != 0 {
            self.request_window_maximize(window, wants(flag::MAXIMIZED), api);
        }
        if changed & flag::FULLSCREEN != 0 {
            self.request_window_fullscreen(window, wants(flag::FULLSCREEN), None, api);
        }
        // Activate last: a task-bar click on a minimized window sends
        // "unminimize + activate" in one request.
        if changed & flag::ACTIVE != 0 && wants(flag::ACTIVE) {
            self.activate_window_from_taskbar(window);
        }
        self.sync_taskbar_window_states();
    }
}

impl GlobalDispatch<OrgKdePlasmaWindowManagement, ()> for ShojiWM {
    fn bind(
        state: &mut Self,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<OrgKdePlasmaWindowManagement>,
        _global_data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        let manager = data_init.init(resource, ());
        // ShojiWM has no "show desktop" mode.
        manager.show_desktop_changed(0);
        let state = &mut state.plasma_window_management_state;
        for handle in &state.handles {
            PlasmaWindowManagementState::announce(&manager, handle);
        }
        state.send_stacking_order(&manager);
        state.managers.push(manager);
    }
}

impl Dispatch<OrgKdePlasmaWindowManagement, ()> for ShojiWM {
    fn request(
        state: &mut Self,
        _client: &Client,
        _manager: &OrgKdePlasmaWindowManagement,
        request: org_kde_plasma_window_management::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        use org_kde_plasma_window_management::Request;
        match request {
            Request::GetWindow { id, internal_window_id } => {
                let handle = state
                    .plasma_window_management_state
                    .find_handle(|handle| handle.internal_id() == internal_window_id);
                state.init_plasma_window(id, handle, data_init);
            }
            Request::GetWindowByUuid { id, internal_window_uuid } => {
                let handle = state
                    .plasma_window_management_state
                    .find_handle(|handle| handle.uuid() == internal_window_uuid);
                state.init_plasma_window(id, handle, data_init);
            }
            Request::GetStackingOrder { stacking_order } => {
                let order = data_init.init(stacking_order, ());
                let uuids: Vec<String> = state
                    .plasma_window_management_state
                    .handles
                    .iter()
                    .map(PlasmaWindowHandle::uuid)
                    .collect();
                // `done` is a destructor event. Send it after this dispatch,
                // never inside the dispatch that created the object (see
                // `defer_image_description_info` for the crash this avoids).
                state.loop_handle.insert_idle(move |_state| {
                    for uuid in uuids {
                        order.window(uuid);
                    }
                    order.done();
                });
            }
            // "Show desktop" (minimize everything) is not supported.
            Request::ShowDesktop { .. } => {}
            _ => {}
        }
    }

    fn destroyed(
        state: &mut Self,
        _client: ClientId,
        resource: &OrgKdePlasmaWindowManagement,
        _data: &(),
    ) {
        state
            .plasma_window_management_state
            .managers
            .retain(|manager| manager != resource);
    }
}

impl ShojiWM {
    fn init_plasma_window(
        &mut self,
        id: New<OrgKdePlasmaWindow>,
        handle: Option<PlasmaWindowHandle>,
        data_init: &mut DataInit<'_, Self>,
    ) {
        let window = handle
            .as_ref()
            .and_then(|handle| self.plasma_window_for_handle(handle));
        let (Some(handle), Some(window)) = (handle, window) else {
            // Unknown or already closed: the protocol says to answer with
            // `unmapped` right away.
            let resource = data_init.init(id, PlasmaWindowHandle::closed_placeholder());
            resource.unmapped();
            return;
        };
        let resource = data_init.init(id, handle.clone());
        handle.add_resource(resource, self.plasma_window_info(&window));
    }
}

impl Dispatch<OrgKdePlasmaStackingOrder, ()> for ShojiWM {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &OrgKdePlasmaStackingOrder,
        _request: <OrgKdePlasmaStackingOrder as Resource>::Request,
        _data: &(),
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        // The interface has no requests.
    }
}

impl Dispatch<OrgKdePlasmaWindow, PlasmaWindowHandle> for ShojiWM {
    fn request(
        state: &mut Self,
        _client: &Client,
        _resource: &OrgKdePlasmaWindow,
        request: org_kde_plasma_window::Request,
        handle: &PlasmaWindowHandle,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        use org_kde_plasma_window::Request;
        if handle.is_closed() {
            return;
        }
        let Some(window) = state.plasma_window_for_handle(handle) else {
            return;
        };
        match request {
            Request::SetState { flags, state: wanted } => {
                state.plasma_window_set_state(&window, flags, wanted);
            }
            Request::Close => state.close_window_from_taskbar(&window),
            // No icon bytes: dropping the fd closes it, so the client reads
            // an empty icon and falls back to the app's .desktop icon.
            Request::GetIcon { fd } => drop(fd),
            // The taskbar entry, for the Magic Lamp minimize animation.
            Request::SetMinimizedGeometry { panel, x, y, width, height } => {
                let rect = (x as i32, y as i32, width as i32, height as i32);
                if handle.set_minimized_geometry(&panel, Some(rect)) {
                    state.plasma_minimized_geometry_changed(&window);
                }
            }
            Request::UnsetMinimizedGeometry { panel } => {
                if handle.set_minimized_geometry(&panel, None) {
                    state.plasma_minimized_geometry_changed(&window);
                }
            }
            // Not supported: virtual desktops, activities, highlight,
            // interactive move/resize, send to output.
            _ => {}
        }
    }

    fn destroyed(
        _state: &mut Self,
        _client: ClientId,
        resource: &OrgKdePlasmaWindow,
        handle: &PlasmaWindowHandle,
    ) {
        handle.remove_resource(resource);
    }
}
