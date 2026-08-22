//! `zwlr_foreign_toplevel_manager_v1` server.
//!
//! Smithay currently provides a helper for `ext_foreign_toplevel_list_v1`, but
//! not for the older wlroots management protocol. This module mirrors the
//! existing ext foreign-toplevel lifecycle and keeps wlroots taskbar/dock
//! clients in sync with ShojiWM's managed-window model.

use std::sync::{Arc, Mutex};

use smithay::{
    desktop::Window,
    output::Output,
    reexports::wayland_server::{
        Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
        Weak as WaylandWeak,
        backend::{ClientId, GlobalId},
        protocol::{wl_output::WlOutput, wl_surface::WlSurface},
    },
};
use wayland_protocols_wlr::foreign_toplevel::v1::server::{
    zwlr_foreign_toplevel_handle_v1::{
        self, State as WlrToplevelState, ZwlrForeignToplevelHandleV1,
    },
    zwlr_foreign_toplevel_manager_v1::{self, ZwlrForeignToplevelManagerV1},
};

use crate::{backend::tty::capture_live_snapshot_for_close, state::ShojiWM};

#[derive(Clone)]
pub struct WlrForeignToplevelHandle {
    inner: Arc<Mutex<WlrForeignToplevelHandleInner>>,
}

struct WlrForeignToplevelHandleInner {
    window_id: String,
    title: String,
    app_id: String,
    states: Vec<WlrToplevelState>,
    parent_id: Option<String>,
    closed: bool,
    rectangle: Option<WlrForeignToplevelRectangle>,
    instances: Vec<WlrForeignToplevelInstance>,
}

struct WlrForeignToplevelInstance {
    resource: WaylandWeak<ZwlrForeignToplevelHandleV1>,
    entered_outputs: Vec<String>,
    output_resources: Vec<WlrForeignToplevelOutputResource>,
}

#[derive(Clone)]
struct WlrForeignToplevelOutputResource {
    name: String,
    resource: WaylandWeak<WlOutput>,
}

struct WlrForeignToplevelInitialOutputs {
    entered_outputs: Vec<String>,
    output_resources: Vec<WlrForeignToplevelOutputResource>,
}

#[allow(dead_code)]
struct WlrForeignToplevelRectangle {
    surface: WaylandWeak<WlSurface>,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
}

impl WlrForeignToplevelHandle {
    fn new(
        window_id: String,
        title: String,
        app_id: String,
        states: Vec<WlrToplevelState>,
        parent_id: Option<String>,
    ) -> Self {
        Self {
            inner: Arc::new(Mutex::new(WlrForeignToplevelHandleInner {
                window_id,
                title,
                app_id,
                states,
                parent_id,
                closed: false,
                rectangle: None,
                instances: Vec::new(),
            })),
        }
    }

    fn window_id(&self) -> String {
        self.inner.lock().unwrap().window_id.clone()
    }

    fn is_closed(&self) -> bool {
        self.inner.lock().unwrap().closed
    }

    fn same_as(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }

    fn init_instance(
        &self,
        resource: ZwlrForeignToplevelHandleV1,
        dh: &DisplayHandle,
        outputs: &[Output],
        all_outputs: &[Output],
        parent: Option<&WlrForeignToplevelHandle>,
    ) {
        let (title, app_id, states) = {
            let inner = self.inner.lock().unwrap();
            (
                inner.title.clone(),
                inner.app_id.clone(),
                inner.states.clone(),
            )
        };

        resource.title(title);
        resource.app_id(app_id);
        let initial_outputs = send_initial_outputs(&resource, dh, outputs, all_outputs);
        send_state(&resource, &states);
        if resource.version() >= 3 {
            send_parent_event(
                &resource,
                parent.and_then(|handle| handle.resource_for_same_client(&resource)),
            );
        }
        resource.done();

        self.inner
            .lock()
            .unwrap()
            .instances
            .push(WlrForeignToplevelInstance {
                resource: resource.downgrade(),
                entered_outputs: initial_outputs.entered_outputs,
                output_resources: initial_outputs.output_resources,
            });
    }

    fn send_title_app_id(&self, title: &str, app_id: &str) {
        let (title_changed, app_id_changed, resources) = {
            let mut inner = self.inner.lock().unwrap();
            let title_changed = inner.title != title;
            let app_id_changed = inner.app_id != app_id;
            if !title_changed && !app_id_changed {
                return;
            }
            inner.title = title.to_string();
            inner.app_id = app_id.to_string();
            retain_live_instances(&mut inner);
            (title_changed, app_id_changed, live_instances(&inner))
        };

        for resource in resources {
            if title_changed {
                resource.title(title.to_string());
            }
            if app_id_changed {
                resource.app_id(app_id.to_string());
            }
            resource.done();
        }
    }

    fn send_parent(&self, parent_id: Option<String>, parent: Option<&WlrForeignToplevelHandle>) {
        let resources = {
            let mut inner = self.inner.lock().unwrap();
            if inner.parent_id == parent_id {
                return;
            }
            inner.parent_id = parent_id;
            retain_live_instances(&mut inner);
            live_instances(&inner)
        };

        for resource in resources {
            if resource.version() >= 3 {
                send_parent_event(
                    &resource,
                    parent.and_then(|handle| handle.resource_for_same_client(&resource)),
                );
                resource.done();
            }
        }
    }

    fn send_states(&self, states: Vec<WlrToplevelState>) {
        let resources = {
            let mut inner = self.inner.lock().unwrap();
            if inner.states == states {
                return;
            }
            inner.states = states.clone();
            retain_live_instances(&mut inner);
            live_instances(&inner)
        };

        for resource in resources {
            send_state(&resource, &states);
            resource.done();
        }
    }

    fn send_outputs(&self, outputs: &[Output]) {
        let desired_names: Vec<String> = outputs.iter().map(Output::name).collect();
        let instances = {
            let mut inner = self.inner.lock().unwrap();
            retain_live_instances(&mut inner);
            inner
                .instances
                .iter()
                .filter_map(|instance| {
                    instance.resource.upgrade().ok().map(|resource| {
                        (
                            resource,
                            instance.entered_outputs.clone(),
                            instance.output_resources.clone(),
                        )
                    })
                })
                .collect::<Vec<_>>()
        };

        for (resource, mut entered_outputs, output_resources) in instances {
            let changed = update_outputs_for_instance(
                &resource,
                &desired_names,
                &output_resources,
                &mut entered_outputs,
            );
            if changed {
                self.update_instance_outputs(&resource, entered_outputs);
            }
        }
    }

    fn send_closed(&self) {
        let resources = {
            let mut inner = self.inner.lock().unwrap();
            if inner.closed {
                return;
            }
            inner.closed = true;
            retain_live_instances(&mut inner);
            let resources = live_instances(&inner);
            inner.instances.clear();
            resources
        };

        for resource in resources {
            resource.closed();
        }
    }

    fn remove_instance(&self, resource: &ZwlrForeignToplevelHandleV1) {
        self.inner.lock().unwrap().instances.retain(|instance| {
            instance
                .resource
                .upgrade()
                .is_ok_and(|instance| instance != *resource)
        });
    }

    fn resource_for_same_client(
        &self,
        resource: &ZwlrForeignToplevelHandleV1,
    ) -> Option<ZwlrForeignToplevelHandleV1> {
        let client = resource.client()?;
        let client_id = client.id();
        let mut inner = self.inner.lock().unwrap();
        retain_live_instances(&mut inner);
        live_instances(&inner).into_iter().find(|candidate| {
            candidate
                .client()
                .is_some_and(|client| client.id() == client_id)
        })
    }

    fn set_rectangle(&self, rectangle: Option<WlrForeignToplevelRectangle>) {
        self.inner.lock().unwrap().rectangle = rectangle;
    }

    fn update_instance_outputs(
        &self,
        resource: &ZwlrForeignToplevelHandleV1,
        entered_outputs: Vec<String>,
    ) {
        let mut inner = self.inner.lock().unwrap();
        if let Some(instance) = inner.instances.iter_mut().find(|instance| {
            instance
                .resource
                .upgrade()
                .is_ok_and(|candidate| candidate == *resource)
        }) {
            instance.entered_outputs = entered_outputs;
        }
    }
}

fn live_instances(inner: &WlrForeignToplevelHandleInner) -> Vec<ZwlrForeignToplevelHandleV1> {
    inner
        .instances
        .iter()
        .filter_map(|instance| instance.resource.upgrade().ok())
        .collect()
}

fn retain_live_instances(inner: &mut WlrForeignToplevelHandleInner) {
    inner
        .instances
        .retain(|instance| instance.resource.upgrade().is_ok());
}

fn send_state(resource: &ZwlrForeignToplevelHandleV1, states: &[WlrToplevelState]) {
    let mut bytes = Vec::with_capacity(states.len() * std::mem::size_of::<u32>());
    for state in states {
        bytes.extend_from_slice(&(*state as u32).to_ne_bytes());
    }
    resource.state(bytes);
}

fn send_parent_event(
    resource: &ZwlrForeignToplevelHandleV1,
    parent: Option<ZwlrForeignToplevelHandleV1>,
) {
    if resource.version() < 3 {
        return;
    }
    resource.parent(parent.as_ref());
}

fn send_initial_outputs(
    resource: &ZwlrForeignToplevelHandleV1,
    dh: &DisplayHandle,
    outputs: &[Output],
    all_outputs: &[Output],
) -> WlrForeignToplevelInitialOutputs {
    let output_names: Vec<String> = outputs.iter().map(Output::name).collect();
    let mut entered_outputs = Vec::new();
    let mut output_resources = Vec::new();
    let Some(client) = dh.get_client(resource.id()).ok() else {
        return WlrForeignToplevelInitialOutputs {
            entered_outputs,
            output_resources,
        };
    };
    for output in all_outputs {
        let name = output.name();
        let Some(wl_output) = output.client_outputs(&client).next() else {
            continue;
        };
        output_resources.push(WlrForeignToplevelOutputResource {
            name: name.clone(),
            resource: wl_output.downgrade(),
        });
        if !output_names.iter().any(|output_name| output_name == &name) {
            continue;
        }
        resource.output_enter(&wl_output);
        entered_outputs.push(name);
    }
    WlrForeignToplevelInitialOutputs {
        entered_outputs,
        output_resources,
    }
}

fn update_outputs_for_instance(
    resource: &ZwlrForeignToplevelHandleV1,
    desired_names: &[String],
    output_resources: &[WlrForeignToplevelOutputResource],
    entered_outputs: &mut Vec<String>,
) -> bool {
    if entered_outputs == desired_names {
        return false;
    }

    for entered_name in entered_outputs.clone() {
        if desired_names.iter().any(|name| name == &entered_name) {
            continue;
        }
        let Some(wl_output) = cached_output_resource(output_resources, &entered_name) else {
            continue;
        };
        resource.output_leave(&wl_output);
    }

    for name in desired_names {
        if entered_outputs.iter().any(|entered| entered == name) {
            continue;
        }
        let Some(wl_output) = cached_output_resource(output_resources, name) else {
            continue;
        };
        resource.output_enter(&wl_output);
    }

    let changed = entered_outputs != desired_names;
    if changed {
        *entered_outputs = desired_names.to_vec();
        resource.done();
    }
    changed
}

fn cached_output_resource(
    output_resources: &[WlrForeignToplevelOutputResource],
    name: &str,
) -> Option<WlOutput> {
    output_resources
        .iter()
        .find(|output| output.name == name)
        .and_then(|output| output.resource.upgrade().ok())
}

#[derive(Debug)]
pub struct WlrForeignToplevelManagerGlobalData;

pub struct WlrForeignToplevelManagerState {
    global: GlobalId,
    managers: Vec<ZwlrForeignToplevelManagerV1>,
    handles: Vec<WlrForeignToplevelHandle>,
    dh: DisplayHandle,
}

impl WlrForeignToplevelManagerState {
    pub fn new<D>(dh: &DisplayHandle) -> Self
    where
        D: GlobalDispatch<ZwlrForeignToplevelManagerV1, WlrForeignToplevelManagerGlobalData>
            + Dispatch<ZwlrForeignToplevelManagerV1, ()>
            + Dispatch<ZwlrForeignToplevelHandleV1, WlrForeignToplevelHandle>
            + 'static,
    {
        let global = dh.create_global::<D, ZwlrForeignToplevelManagerV1, _>(
            3,
            WlrForeignToplevelManagerGlobalData,
        );
        Self {
            global,
            managers: Vec::new(),
            handles: Vec::new(),
            dh: dh.clone(),
        }
    }

    pub fn global(&self) -> GlobalId {
        self.global.clone()
    }

    fn new_toplevel<D>(
        &mut self,
        window_id: String,
        title: String,
        app_id: String,
        states: Vec<WlrToplevelState>,
        parent_id: Option<String>,
        outputs: &[Output],
        all_outputs: &[Output],
        parent: Option<&WlrForeignToplevelHandle>,
    ) -> WlrForeignToplevelHandle
    where
        D: Dispatch<ZwlrForeignToplevelHandleV1, WlrForeignToplevelHandle> + 'static,
    {
        let handle = WlrForeignToplevelHandle::new(window_id, title, app_id, states, parent_id);
        for manager in &self.managers {
            let Ok(client) = self.dh.get_client(manager.id()) else {
                continue;
            };
            let Ok(resource) = client.create_resource::<ZwlrForeignToplevelHandleV1, _, D>(
                &self.dh,
                manager.version(),
                handle.clone(),
            ) else {
                continue;
            };
            manager.toplevel(&resource);
            handle.init_instance(resource, &self.dh, outputs, all_outputs, parent);
        }
        self.handles.push(handle.clone());
        handle
    }

    fn handle_for_window_id(&self, window_id: &str) -> Option<WlrForeignToplevelHandle> {
        self.handles
            .iter()
            .find(|handle| handle.window_id() == window_id && !handle.is_closed())
            .cloned()
    }

    fn remove_toplevel(&mut self, handle: &WlrForeignToplevelHandle) {
        handle.send_closed();
        self.handles.retain(|candidate| !candidate.same_as(handle));
    }
}

pub trait WlrForeignToplevelManagerHandler:
    GlobalDispatch<ZwlrForeignToplevelManagerV1, WlrForeignToplevelManagerGlobalData>
    + Dispatch<ZwlrForeignToplevelManagerV1, ()>
    + Dispatch<ZwlrForeignToplevelHandleV1, WlrForeignToplevelHandle>
{
    fn wlr_foreign_toplevel_manager_state(&mut self) -> &mut WlrForeignToplevelManagerState;
    fn wlr_foreign_toplevel_window(&self, handle: &WlrForeignToplevelHandle) -> Option<Window>;
    fn wlr_foreign_toplevel_outputs(&self, handle: &WlrForeignToplevelHandle) -> Vec<Output>;
    fn wlr_foreign_toplevel_all_outputs(&self) -> Vec<Output>;
    fn wlr_foreign_toplevel_parent(
        &self,
        handle: &WlrForeignToplevelHandle,
    ) -> Option<WlrForeignToplevelHandle>;
    fn wlr_foreign_toplevel_activate(&mut self, handle: &WlrForeignToplevelHandle);
    fn wlr_foreign_toplevel_close(&mut self, handle: &WlrForeignToplevelHandle);
    fn wlr_foreign_toplevel_set_maximized(
        &mut self,
        handle: &WlrForeignToplevelHandle,
        maximized: bool,
    );
    fn wlr_foreign_toplevel_set_minimized(
        &mut self,
        handle: &WlrForeignToplevelHandle,
        minimized: bool,
    );
    fn wlr_foreign_toplevel_set_fullscreen(
        &mut self,
        handle: &WlrForeignToplevelHandle,
        fullscreen: bool,
        output: Option<WlOutput>,
    );
}

impl<D> GlobalDispatch<ZwlrForeignToplevelManagerV1, WlrForeignToplevelManagerGlobalData, D>
    for WlrForeignToplevelManagerState
where
    D: WlrForeignToplevelManagerHandler + 'static,
{
    fn bind(
        state: &mut D,
        dh: &DisplayHandle,
        client: &Client,
        resource: New<ZwlrForeignToplevelManagerV1>,
        _global_data: &WlrForeignToplevelManagerGlobalData,
        data_init: &mut DataInit<'_, D>,
    ) {
        let manager = data_init.init(resource, ());
        let manager_version = manager.version();
        let mut live_handles = Vec::new();
        {
            let manager_state = state.wlr_foreign_toplevel_manager_state();
            manager_state.handles.retain(|handle| !handle.is_closed());
            live_handles.extend(manager_state.handles.iter().cloned());
        }
        for handle in &live_handles {
            let outputs = state.wlr_foreign_toplevel_outputs(handle);
            let all_outputs = state.wlr_foreign_toplevel_all_outputs();
            let parent = state.wlr_foreign_toplevel_parent(handle);
            let Ok(resource) = client.create_resource::<ZwlrForeignToplevelHandleV1, _, D>(
                dh,
                manager_version,
                handle.clone(),
            ) else {
                continue;
            };
            manager.toplevel(&resource);
            handle.init_instance(resource, dh, &outputs, &all_outputs, parent.as_ref());
        }
        state
            .wlr_foreign_toplevel_manager_state()
            .managers
            .push(manager);
    }
}

impl<D> Dispatch<ZwlrForeignToplevelManagerV1, (), D> for WlrForeignToplevelManagerState
where
    D: WlrForeignToplevelManagerHandler,
{
    fn request(
        state: &mut D,
        _client: &Client,
        manager: &ZwlrForeignToplevelManagerV1,
        request: zwlr_foreign_toplevel_manager_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            zwlr_foreign_toplevel_manager_v1::Request::Stop => {
                manager.finished();
                state
                    .wlr_foreign_toplevel_manager_state()
                    .managers
                    .retain(|instance| instance != manager);
            }
            _ => unreachable!(),
        }
    }

    fn destroyed(
        state: &mut D,
        _client: ClientId,
        resource: &ZwlrForeignToplevelManagerV1,
        _data: &(),
    ) {
        state
            .wlr_foreign_toplevel_manager_state()
            .managers
            .retain(|instance| instance != resource);
    }
}

impl<D> Dispatch<ZwlrForeignToplevelHandleV1, WlrForeignToplevelHandle, D>
    for WlrForeignToplevelManagerState
where
    D: WlrForeignToplevelManagerHandler,
{
    fn request(
        state: &mut D,
        _client: &Client,
        resource: &ZwlrForeignToplevelHandleV1,
        request: zwlr_foreign_toplevel_handle_v1::Request,
        handle: &WlrForeignToplevelHandle,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, D>,
    ) {
        if handle.is_closed()
            && !matches!(request, zwlr_foreign_toplevel_handle_v1::Request::Destroy)
        {
            return;
        }
        match request {
            zwlr_foreign_toplevel_handle_v1::Request::SetMaximized => {
                state.wlr_foreign_toplevel_set_maximized(handle, true);
            }
            zwlr_foreign_toplevel_handle_v1::Request::UnsetMaximized => {
                state.wlr_foreign_toplevel_set_maximized(handle, false);
            }
            zwlr_foreign_toplevel_handle_v1::Request::SetMinimized => {
                state.wlr_foreign_toplevel_set_minimized(handle, true);
            }
            zwlr_foreign_toplevel_handle_v1::Request::UnsetMinimized => {
                state.wlr_foreign_toplevel_set_minimized(handle, false);
            }
            zwlr_foreign_toplevel_handle_v1::Request::Activate { seat: _ } => {
                state.wlr_foreign_toplevel_activate(handle);
            }
            zwlr_foreign_toplevel_handle_v1::Request::Close => {
                state.wlr_foreign_toplevel_close(handle);
            }
            zwlr_foreign_toplevel_handle_v1::Request::SetRectangle {
                surface,
                x,
                y,
                width,
                height,
            } => {
                if width < 0 || height < 0 || (width == 0) != (height == 0) {
                    resource.post_error(
                        zwlr_foreign_toplevel_handle_v1::Error::InvalidRectangle,
                        "invalid foreign toplevel rectangle",
                    );
                    return;
                }
                let rectangle = if width == 0 && height == 0 {
                    None
                } else {
                    Some(WlrForeignToplevelRectangle {
                        surface: surface.downgrade(),
                        x,
                        y,
                        width,
                        height,
                    })
                };
                handle.set_rectangle(rectangle);
            }
            zwlr_foreign_toplevel_handle_v1::Request::Destroy => {}
            zwlr_foreign_toplevel_handle_v1::Request::SetFullscreen { output } => {
                state.wlr_foreign_toplevel_set_fullscreen(handle, true, output);
            }
            zwlr_foreign_toplevel_handle_v1::Request::UnsetFullscreen => {
                state.wlr_foreign_toplevel_set_fullscreen(handle, false, None);
            }
            _ => unreachable!(),
        }
    }

    fn destroyed(
        _state: &mut D,
        _client: ClientId,
        resource: &ZwlrForeignToplevelHandleV1,
        handle: &WlrForeignToplevelHandle,
    ) {
        handle.remove_instance(resource);
    }
}

fn states_for_snapshot(
    snapshot: &crate::ssd::WaylandWindowSnapshot,
    minimized: bool,
) -> Vec<WlrToplevelState> {
    let mut states = Vec::new();
    if snapshot.is_maximized {
        states.push(WlrToplevelState::Maximized);
    }
    if minimized {
        states.push(WlrToplevelState::Minimized);
    }
    if snapshot.is_focused {
        states.push(WlrToplevelState::Activated);
    }
    if snapshot.is_fullscreen {
        states.push(WlrToplevelState::Fullscreen);
    }
    states
}

impl ShojiWM {
    pub fn install_wlr_foreign_toplevel(&mut self, window: &Window, title: &str, app_id: &str) {
        if window
            .user_data()
            .get::<WlrForeignToplevelHandle>()
            .is_some()
        {
            return;
        }
        let snapshot = self.snapshot_window(window);
        let minimized = self.wlr_window_is_minimized(window);
        let outputs = self.wlr_outputs_for_window(window);
        let all_outputs = self.wlr_all_outputs();
        let parent = snapshot.parent_id.as_deref().and_then(|parent_id| {
            self.wlr_foreign_toplevel_manager_state
                .handle_for_window_id(parent_id)
        });
        let handle = self
            .wlr_foreign_toplevel_manager_state
            .new_toplevel::<Self>(
                snapshot.id.clone(),
                title.to_string(),
                app_id.to_string(),
                states_for_snapshot(&snapshot, minimized),
                snapshot.parent_id.clone(),
                &outputs,
                &all_outputs,
                parent.as_ref(),
            );
        window.user_data().insert_if_missing(|| handle);
    }

    pub fn sync_wlr_foreign_toplevel(&mut self, window: &Window, title: &str, app_id: &str) {
        let Some(handle) = window
            .user_data()
            .get::<WlrForeignToplevelHandle>()
            .cloned()
        else {
            return;
        };
        handle.send_title_app_id(title, app_id);
        let snapshot = self.snapshot_window(window);
        self.sync_wlr_handle_from_snapshot(window, &handle, &snapshot);
    }

    pub fn sync_wlr_foreign_toplevel_states(&mut self) {
        let windows: Vec<_> = self.space.elements().cloned().collect();
        for window in windows {
            let Some(handle) = window
                .user_data()
                .get::<WlrForeignToplevelHandle>()
                .cloned()
            else {
                continue;
            };
            let snapshot = self.snapshot_window(&window);
            self.sync_wlr_handle_from_snapshot(&window, &handle, &snapshot);
        }
    }

    pub fn remove_wlr_foreign_toplevel(&mut self, window: &Window) {
        let Some(handle) = window
            .user_data()
            .get::<WlrForeignToplevelHandle>()
            .cloned()
        else {
            return;
        };
        self.wlr_foreign_toplevel_manager_state
            .remove_toplevel(&handle);
    }

    fn sync_wlr_handle_from_snapshot(
        &self,
        window: &Window,
        handle: &WlrForeignToplevelHandle,
        snapshot: &crate::ssd::WaylandWindowSnapshot,
    ) {
        handle.send_states(states_for_snapshot(
            snapshot,
            self.wlr_window_is_minimized(window),
        ));
        let parent = snapshot.parent_id.as_deref().and_then(|parent_id| {
            self.wlr_foreign_toplevel_manager_state
                .handle_for_window_id(parent_id)
        });
        handle.send_parent(snapshot.parent_id.clone(), parent.as_ref());
        let outputs = self.wlr_outputs_for_window(window);
        handle.send_outputs(&outputs);
    }

    fn wlr_window_is_minimized(&self, window: &Window) -> bool {
        self.window_decorations
            .get(window)
            .is_some_and(|decoration| decoration.managed_window.idle)
    }

    fn wlr_outputs_for_window(&self, window: &Window) -> Vec<Output> {
        let Some(decoration) = self.window_decorations.get(window) else {
            return self.wlr_outputs_for_unmanaged_window(window);
        };
        // `idle` (minimized or on an inactive workspace) must NOT clear the
        // output list: taskbars group wlr-foreign-toplevel entries by output,
        // so sending output_leave for every output on minimize removed the
        // window's icon from the bar entirely. Idle windows keep advertising
        // their home outputs (`visible_outputs` below covers windows whose
        // rect is parked off-screen); only an explicit `visible = false`
        // hides the toplevel from per-output consumers.
        if !decoration.managed_window.visible {
            return Vec::new();
        }
        if let Some(visible_outputs) = decoration.managed_window.visible_outputs.as_ref() {
            let outputs = self
                .space
                .outputs()
                .filter(|output| {
                    self.runtime_output_render_enabled(&output.name())
                        && visible_outputs.iter().any(|name| name == &output.name())
                })
                .cloned()
                .collect::<Vec<_>>();
            return outputs;
        }
        let rect = smithay::utils::Rectangle::new(
            smithay::utils::Point::from((
                decoration.layout.root.rect.x,
                decoration.layout.root.rect.y,
            )),
            (
                decoration.layout.root.rect.width,
                decoration.layout.root.rect.height,
            )
                .into(),
        );
        
        self.wlr_outputs_for_rect(rect)
    }

    fn wlr_outputs_for_unmanaged_window(&self, window: &Window) -> Vec<Output> {
        let Some(rect) = self.space.element_bbox(window) else {
            return Vec::new();
        };
        
        self.wlr_outputs_for_rect(rect)
    }

    fn wlr_all_outputs(&self) -> Vec<Output> {
        self.space.outputs().cloned().collect()
    }

    fn wlr_outputs_for_rect(
        &self,
        rect: smithay::utils::Rectangle<i32, smithay::utils::Logical>,
    ) -> Vec<Output> {
        self.space
            .outputs()
            .filter(|output| self.runtime_output_render_enabled(&output.name()))
            .filter(|output| {
                self.space
                    .output_geometry(output)
                    .and_then(|geometry| geometry.intersection(rect))
                    .is_some()
            })
            .cloned()
            .collect()
    }
}

impl WlrForeignToplevelManagerHandler for ShojiWM {
    fn wlr_foreign_toplevel_manager_state(&mut self) -> &mut WlrForeignToplevelManagerState {
        &mut self.wlr_foreign_toplevel_manager_state
    }

    fn wlr_foreign_toplevel_window(&self, handle: &WlrForeignToplevelHandle) -> Option<Window> {
        let window_id = handle.window_id();
        self.space
            .elements()
            .find(|window| self.snapshot_window(window).id == window_id)
            .cloned()
    }

    fn wlr_foreign_toplevel_outputs(&self, handle: &WlrForeignToplevelHandle) -> Vec<Output> {
        self.wlr_foreign_toplevel_window(handle)
            .map(|window| self.wlr_outputs_for_window(&window))
            .unwrap_or_default()
    }

    fn wlr_foreign_toplevel_all_outputs(&self) -> Vec<Output> {
        self.wlr_all_outputs()
    }

    fn wlr_foreign_toplevel_parent(
        &self,
        handle: &WlrForeignToplevelHandle,
    ) -> Option<WlrForeignToplevelHandle> {
        let window = self.wlr_foreign_toplevel_window(handle)?;
        let snapshot = self.snapshot_window(&window);
        let parent_id = snapshot.parent_id.as_deref()?;
        self.wlr_foreign_toplevel_manager_state
            .handle_for_window_id(parent_id)
    }

    fn wlr_foreign_toplevel_activate(&mut self, handle: &WlrForeignToplevelHandle) {
        let Some(window) = self.wlr_foreign_toplevel_window(handle) else {
            return;
        };
        // Picking a window out of a taskbar is the user choosing it, even
        // though the click itself landed on the panel rather than on the window
        // — the input layer cannot attribute it. Record it so a later successor
        // election knows about it, and so the focus-ordering rule cannot refuse
        // it or anything the handler focuses instead.
        self.note_window_user_input(&window);
        let previous = std::mem::replace(&mut self.user_input_in_flight, true);
        self.request_window_activate(
            &window,
            crate::ssd::WindowActivateRequestSourceSnapshot::Api,
        );
        self.user_input_in_flight = previous;
        self.sync_wlr_foreign_toplevel_states();
    }

    fn wlr_foreign_toplevel_close(&mut self, handle: &WlrForeignToplevelHandle) {
        let Some(window) = self.wlr_foreign_toplevel_window(handle) else {
            return;
        };
        if let Err(error) = capture_live_snapshot_for_close(self, &window) {
            tracing::warn!(
                ?error,
                "failed to capture foreign toplevel before close request"
            );
        }
        if let Some(toplevel) = window.toplevel() {
            toplevel.send_close();
        } else if let Some(x11) = window.x11_surface() {
            let _ = x11.close();
        }
    }

    fn wlr_foreign_toplevel_set_maximized(
        &mut self,
        handle: &WlrForeignToplevelHandle,
        maximized: bool,
    ) {
        let Some(window) = self.wlr_foreign_toplevel_window(handle) else {
            return;
        };
        self.request_window_maximize(
            &window,
            maximized,
            crate::ssd::WindowStateRequestSourceSnapshot::Api,
        );
        self.sync_wlr_foreign_toplevel_states();
    }

    fn wlr_foreign_toplevel_set_minimized(
        &mut self,
        handle: &WlrForeignToplevelHandle,
        minimized: bool,
    ) {
        let Some(window) = self.wlr_foreign_toplevel_window(handle) else {
            return;
        };
        self.request_window_minimize(
            &window,
            minimized,
            crate::ssd::WindowStateRequestSourceSnapshot::Api,
        );
        self.sync_wlr_foreign_toplevel_states();
    }

    fn wlr_foreign_toplevel_set_fullscreen(
        &mut self,
        handle: &WlrForeignToplevelHandle,
        fullscreen: bool,
        output: Option<WlOutput>,
    ) {
        let Some(window) = self.wlr_foreign_toplevel_window(handle) else {
            return;
        };
        let output_name = output
            .as_ref()
            .and_then(smithay::output::Output::from_resource)
            .map(|output| output.name());
        self.request_window_fullscreen(
            &window,
            fullscreen,
            output_name,
            crate::ssd::WindowStateRequestSourceSnapshot::Api,
        );
        self.sync_wlr_foreign_toplevel_states();
    }
}

#[macro_export]
macro_rules! delegate_wlr_foreign_toplevel {
    ($ty: ty) => {
        const _: () = {
            use $crate::wlr_foreign_toplevel::{
                WlrForeignToplevelHandle, WlrForeignToplevelManagerGlobalData,
                WlrForeignToplevelManagerState,
            };
            use smithay::reexports::wayland_server::{
                delegate_dispatch, delegate_global_dispatch,
            };
            use wayland_protocols_wlr::foreign_toplevel::v1::server::{
                zwlr_foreign_toplevel_handle_v1::ZwlrForeignToplevelHandleV1,
                zwlr_foreign_toplevel_manager_v1::ZwlrForeignToplevelManagerV1,
            };

            delegate_global_dispatch!(
                $ty: [ZwlrForeignToplevelManagerV1: WlrForeignToplevelManagerGlobalData] => WlrForeignToplevelManagerState
            );
            delegate_dispatch!(
                $ty: [ZwlrForeignToplevelManagerV1: ()] => WlrForeignToplevelManagerState
            );
            delegate_dispatch!(
                $ty: [ZwlrForeignToplevelHandleV1: WlrForeignToplevelHandle] => WlrForeignToplevelManagerState
            );
        };
    };
}
