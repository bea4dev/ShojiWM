use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
    time::Duration,
};

use smithay::{
    output::Output,
    reexports::wayland_server::{
        Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
        Weak as WaylandWeak,
        backend::{ClientId, GlobalId},
        protocol::wl_output::WlOutput,
    },
};
use wayland_protocols::ext::workspace::v1::server::{
    ext_workspace_group_handle_v1::{
        self, ExtWorkspaceGroupHandleV1, GroupCapabilities as ExtWorkspaceGroupCapability,
    },
    ext_workspace_handle_v1::{
        self, ExtWorkspaceHandleV1, State as ExtWorkspaceState,
        WorkspaceCapabilities as ExtWorkspaceCapability,
    },
    ext_workspace_manager_v1::{self, ExtWorkspaceManagerV1},
};

use crate::{
    runtime_workspace::{
        RuntimeWorkspaceActivateRequestSnapshot, RuntimeWorkspaceConfigUpdate,
        RuntimeWorkspaceEntry, RuntimeWorkspaceGroupConfig,
    },
    ssd::DecorationEvaluator,
    state::ShojiWM,
};

#[derive(Clone)]
pub struct ExtWorkspaceGroupHandle {
    inner: Arc<Mutex<ExtWorkspaceGroupInner>>,
}

struct ExtWorkspaceGroupInner {
    outputs: Vec<String>,
    workspaces: Vec<String>,
    removed: bool,
    instances: Vec<ExtWorkspaceGroupInstance>,
}

struct ExtWorkspaceGroupInstance {
    resource: WaylandWeak<ExtWorkspaceGroupHandleV1>,
    entered_outputs: Vec<String>,
    output_resources: Vec<ExtWorkspaceOutputResource>,
    entered_workspaces: Vec<String>,
}

#[derive(Clone)]
pub struct ExtWorkspaceHandle {
    inner: Arc<Mutex<ExtWorkspaceInner>>,
}

struct ExtWorkspaceInner {
    id: String,
    group_id: Option<String>,
    name: String,
    coordinates: Vec<u32>,
    active: bool,
    urgent: bool,
    hidden: bool,
    removed: bool,
    instances: Vec<WaylandWeak<ExtWorkspaceHandleV1>>,
}

#[derive(Clone)]
struct ExtWorkspaceOutputResource {
    name: String,
    resource: WaylandWeak<WlOutput>,
}

impl ExtWorkspaceGroupHandle {
    fn new(group: &RuntimeWorkspaceGroupConfig) -> Self {
        Self {
            inner: Arc::new(Mutex::new(ExtWorkspaceGroupInner {
                outputs: group.outputs.clone(),
                workspaces: group
                    .workspaces
                    .iter()
                    .map(|workspace| workspace.id.clone())
                    .collect(),
                removed: false,
                instances: Vec::new(),
            })),
        }
    }

    fn init_instance(
        &self,
        resource: ExtWorkspaceGroupHandleV1,
        dh: &DisplayHandle,
        all_outputs: &[Output],
        workspaces: &[ExtWorkspaceHandle],
    ) {
        let (outputs, workspace_ids) = {
            let inner = self.inner.lock().unwrap();
            (inner.outputs.clone(), inner.workspaces.clone())
        };

        resource.capabilities(ExtWorkspaceGroupCapability::empty());
        let mut entered_outputs = Vec::new();
        let mut output_resources = Vec::new();
        refresh_group_outputs(
            &resource,
            dh,
            &outputs,
            all_outputs,
            &mut output_resources,
            &mut entered_outputs,
        );

        let mut entered_workspaces = Vec::new();
        for workspace_id in &workspace_ids {
            let Some(workspace) = workspaces
                .iter()
                .find(|workspace| workspace.id() == *workspace_id)
            else {
                continue;
            };
            let Some(workspace_resource) = workspace.resource_for_same_client(&resource) else {
                continue;
            };
            resource.workspace_enter(&workspace_resource);
            entered_workspaces.push(workspace_id.clone());
        }

        self.inner
            .lock()
            .unwrap()
            .instances
            .push(ExtWorkspaceGroupInstance {
                resource: resource.downgrade(),
                entered_outputs,
                output_resources,
                entered_workspaces,
            });
    }

    fn update(
        &self,
        group: &RuntimeWorkspaceGroupConfig,
        dh: &DisplayHandle,
        all_outputs: &[Output],
        workspaces: &[ExtWorkspaceHandle],
    ) -> bool {
        let (config_changed, resources) = {
            let mut inner = self.inner.lock().unwrap();
            let next_workspaces: Vec<String> = group
                .workspaces
                .iter()
                .map(|workspace| workspace.id.clone())
                .collect();
            let config_changed = if inner.outputs == group.outputs
                && inner.workspaces == next_workspaces
            {
                false
            } else {
                inner.outputs = group.outputs.clone();
                inner.workspaces = next_workspaces;
                true
            };
            retain_live_group_instances(&mut inner);
            let resources = inner
                .instances
                .iter()
                .filter_map(|instance| {
                    instance.resource.upgrade().ok().map(|resource| {
                        (resource, instance.entered_workspaces.clone())
                    })
                })
                .collect::<Vec<_>>();
            (config_changed, resources)
        };

        let outputs_changed = self.refresh_outputs(dh, all_outputs);
        let desired_workspaces = self.inner.lock().unwrap().workspaces.clone();
        let mut workspaces_changed = false;
        for (resource, mut entered_workspaces) in resources {
            workspaces_changed |= update_group_workspaces(
                &resource,
                &desired_workspaces,
                workspaces,
                &mut entered_workspaces,
            );
        }
        config_changed || outputs_changed || workspaces_changed
    }

    fn refresh_outputs(&self, dh: &DisplayHandle, all_outputs: &[Output]) -> bool {
        let mut inner = self.inner.lock().unwrap();
        let desired_outputs = inner.outputs.clone();
        retain_live_group_instances(&mut inner);
        let mut changed = false;
        for instance in &mut inner.instances {
            let Ok(resource) = instance.resource.upgrade() else {
                continue;
            };
            changed |= refresh_group_outputs(
                &resource,
                dh,
                &desired_outputs,
                all_outputs,
                &mut instance.output_resources,
                &mut instance.entered_outputs,
            );
        }
        changed
    }

    fn remove(&self, workspaces: &[ExtWorkspaceHandle]) {
        let instances = {
            let mut inner = self.inner.lock().unwrap();
            if inner.removed {
                return;
            }
            inner.removed = true;
            retain_live_group_instances(&mut inner);
            inner
                .instances
                .iter()
                .filter_map(|instance| {
                    instance
                        .resource
                        .upgrade()
                        .ok()
                        .map(|resource| (resource, instance.entered_workspaces.clone()))
                })
                .collect::<Vec<_>>()
        };

        for (resource, entered_workspaces) in instances {
            for workspace_id in entered_workspaces {
                let Some(workspace) = workspaces
                    .iter()
                    .find(|workspace| workspace.id() == workspace_id)
                else {
                    continue;
                };
                if let Some(workspace_resource) = workspace.resource_for_same_client(&resource) {
                    resource.workspace_leave(&workspace_resource);
                }
            }
            resource.removed();
        }
    }
}

impl ExtWorkspaceHandle {
    fn new(group_id: &str, entry: &RuntimeWorkspaceEntry) -> Self {
        Self {
            inner: Arc::new(Mutex::new(ExtWorkspaceInner {
                id: entry.id.clone(),
                group_id: Some(group_id.to_string()),
                name: entry.name.clone(),
                coordinates: entry.coordinates.clone(),
                active: entry.active,
                urgent: entry.urgent,
                hidden: entry.hidden,
                removed: false,
                instances: Vec::new(),
            })),
        }
    }

    fn id(&self) -> String {
        self.inner.lock().unwrap().id.clone()
    }

    fn group_id(&self) -> Option<String> {
        self.inner.lock().unwrap().group_id.clone()
    }

    fn is_removed(&self) -> bool {
        self.inner.lock().unwrap().removed
    }

    fn init_instance(&self, resource: ExtWorkspaceHandleV1) {
        let (id, name, coordinates, state) = {
            let inner = self.inner.lock().unwrap();
            (
                inner.id.clone(),
                inner.name.clone(),
                inner.coordinates.clone(),
                workspace_state(&inner),
            )
        };
        resource.id(id);
        resource.name(name);
        resource.coordinates(encode_workspace_coordinates(&coordinates));
        resource.capabilities(ExtWorkspaceCapability::Activate);
        resource.state(state);
        self.inner
            .lock()
            .unwrap()
            .instances
            .push(resource.downgrade());
    }

    fn update(&self, group_id: &str, entry: &RuntimeWorkspaceEntry) -> bool {
        let (name_changed, coordinates_changed, state_changed, resources, name, coordinates, state) = {
            let mut inner = self.inner.lock().unwrap();
            let name_changed = inner.name != entry.name;
            let coordinates_changed = inner.coordinates != entry.coordinates;
            let old_state = workspace_state(&inner);
            inner.group_id = Some(group_id.to_string());
            inner.name = entry.name.clone();
            inner.coordinates = entry.coordinates.clone();
            inner.active = entry.active;
            inner.urgent = entry.urgent;
            inner.hidden = entry.hidden;
            let state = workspace_state(&inner);
            let state_changed = old_state != state;
            retain_live_workspace_instances(&mut inner);
            (
                name_changed,
                coordinates_changed,
                state_changed,
                live_workspace_instances(&inner),
                inner.name.clone(),
                inner.coordinates.clone(),
                state,
            )
        };
        if !name_changed && !coordinates_changed && !state_changed {
            return false;
        }
        for resource in resources {
            if name_changed {
                resource.name(name.clone());
            }
            if coordinates_changed {
                resource.coordinates(encode_workspace_coordinates(&coordinates));
            }
            if state_changed {
                resource.state(state);
            }
        }
        true
    }

    fn remove(&self) {
        let resources = {
            let mut inner = self.inner.lock().unwrap();
            if inner.removed {
                return;
            }
            inner.removed = true;
            retain_live_workspace_instances(&mut inner);
            live_workspace_instances(&inner)
        };
        for resource in resources {
            resource.removed();
        }
    }

    fn resource_for_same_client(
        &self,
        group: &ExtWorkspaceGroupHandleV1,
    ) -> Option<ExtWorkspaceHandleV1> {
        let group_id = Resource::id(group);
        let inner = self.inner.lock().unwrap();
        inner
            .instances
            .iter()
            .filter_map(|resource| resource.upgrade().ok())
            .find(|resource| Resource::id(resource).same_client_as(&group_id))
    }
}

fn encode_workspace_coordinates(coordinates: &[u32]) -> Vec<u8> {
    coordinates
        .iter()
        .flat_map(|coordinate| coordinate.to_ne_bytes())
        .collect()
}

fn workspace_state(inner: &ExtWorkspaceInner) -> ExtWorkspaceState {
    let mut state = ExtWorkspaceState::empty();
    if inner.active {
        state |= ExtWorkspaceState::Active;
    }
    if inner.urgent {
        state |= ExtWorkspaceState::Urgent;
    }
    if inner.hidden {
        state |= ExtWorkspaceState::Hidden;
    }
    state
}

fn retain_live_group_instances(inner: &mut ExtWorkspaceGroupInner) {
    inner
        .instances
        .retain(|instance| instance.resource.upgrade().is_ok());
}

fn retain_live_workspace_instances(inner: &mut ExtWorkspaceInner) {
    inner
        .instances
        .retain(|instance| instance.upgrade().is_ok());
}

fn live_workspace_instances(inner: &ExtWorkspaceInner) -> Vec<ExtWorkspaceHandleV1> {
    inner
        .instances
        .iter()
        .filter_map(|resource| resource.upgrade().ok())
        .collect()
}

fn refresh_group_outputs(
    resource: &ExtWorkspaceGroupHandleV1,
    dh: &DisplayHandle,
    desired_names: &[String],
    all_outputs: &[Output],
    output_resources: &mut Vec<ExtWorkspaceOutputResource>,
    entered_outputs: &mut Vec<String>,
) -> bool {
    let Some(client) = dh.get_client(resource.id()).ok() else {
        return false;
    };
    let mut next_output_resources = Vec::new();
    for output in all_outputs {
        let name = output.name();
        let Some(wl_output) = output.client_outputs(&client).next() else {
            continue;
        };
        next_output_resources.push(ExtWorkspaceOutputResource {
            name: name.clone(),
            resource: wl_output.downgrade(),
        });
    }

    let mut changed = false;
    entered_outputs.retain(|entered_name| {
        let desired = desired_names.iter().any(|name| name == entered_name);
        let previous = cached_output_resource(output_resources, entered_name);
        let next = cached_output_resource(&next_output_resources, entered_name);
        let same_resource = previous
            .as_ref()
            .zip(next.as_ref())
            .is_some_and(|(previous, next)| previous == next);
        if desired && same_resource {
            return true;
        }
        if let Some(previous) = previous {
            resource.output_leave(&previous);
        }
        changed = true;
        false
    });

    for name in desired_names {
        if entered_outputs.iter().any(|entered| entered == name) {
            continue;
        }
        let Some(output) = cached_output_resource(&next_output_resources, name) else {
            continue;
        };
        resource.output_enter(&output);
        entered_outputs.push(name.clone());
        changed = true;
    }

    *output_resources = next_output_resources;
    changed
}

fn update_group_workspaces(
    resource: &ExtWorkspaceGroupHandleV1,
    desired_ids: &[String],
    workspaces: &[ExtWorkspaceHandle],
    entered_workspaces: &mut Vec<String>,
) -> bool {
    let mut changed = false;
    for entered_id in entered_workspaces.clone() {
        if desired_ids.iter().any(|id| id == &entered_id) {
            continue;
        }
        let Some(workspace) = workspaces
            .iter()
            .find(|workspace| workspace.id() == entered_id)
        else {
            continue;
        };
        if let Some(workspace_resource) = workspace.resource_for_same_client(resource) {
            resource.workspace_leave(&workspace_resource);
            changed = true;
        }
    }
    for id in desired_ids {
        if entered_workspaces.iter().any(|entered| entered == id) {
            continue;
        }
        let Some(workspace) = workspaces.iter().find(|workspace| workspace.id() == *id) else {
            continue;
        };
        if let Some(workspace_resource) = workspace.resource_for_same_client(resource) {
            resource.workspace_enter(&workspace_resource);
            changed = true;
        }
    }
    if changed {
        *entered_workspaces = desired_ids.to_vec();
    }
    changed
}

fn cached_output_resource(
    output_resources: &[ExtWorkspaceOutputResource],
    name: &str,
) -> Option<WlOutput> {
    output_resources
        .iter()
        .find(|output| output.name == name)
        .and_then(|output| output.resource.upgrade().ok())
}

pub struct ExtWorkspaceManagerGlobalData;

pub struct ExtWorkspaceManagerState {
    _global: GlobalId,
    managers: Vec<ExtWorkspaceManagerV1>,
    groups: BTreeMap<String, ExtWorkspaceGroupHandle>,
    workspaces: BTreeMap<String, ExtWorkspaceHandle>,
}

impl ExtWorkspaceManagerState {
    pub fn new<D>(dh: &DisplayHandle) -> Self
    where
        D: GlobalDispatch<ExtWorkspaceManagerV1, ExtWorkspaceManagerGlobalData>
            + Dispatch<ExtWorkspaceManagerV1, ()>
            + Dispatch<ExtWorkspaceGroupHandleV1, ExtWorkspaceGroupHandle>
            + Dispatch<ExtWorkspaceHandleV1, ExtWorkspaceHandle>
            + 'static,
    {
        let global =
            dh.create_global::<D, ExtWorkspaceManagerV1, _>(1, ExtWorkspaceManagerGlobalData);
        Self {
            _global: global,
            managers: Vec::new(),
            groups: BTreeMap::new(),
            workspaces: BTreeMap::new(),
        }
    }

    pub fn sync<D>(
        &mut self,
        update: RuntimeWorkspaceConfigUpdate,
        dh: &DisplayHandle,
        all_outputs: &[Output],
    ) where
        D: Dispatch<ExtWorkspaceGroupHandleV1, ExtWorkspaceGroupHandle>
            + Dispatch<ExtWorkspaceHandleV1, ExtWorkspaceHandle>
            + 'static,
    {
        let next_group_ids: BTreeSet<String> =
            update.groups.iter().map(|group| group.id.clone()).collect();
        let next_workspace_ids: BTreeSet<String> = update
            .groups
            .iter()
            .flat_map(|group| {
                group
                    .workspaces
                    .iter()
                    .map(|workspace| workspace.id.clone())
            })
            .collect();

        for group in &update.groups {
            for workspace in &group.workspaces {
                if !self.workspaces.contains_key(&workspace.id) {
                    let handle = ExtWorkspaceHandle::new(&group.id, workspace);
                    self.announce_workspace::<D>(&handle, dh);
                    self.workspaces.insert(workspace.id.clone(), handle);
                }
            }
        }

        for group in &update.groups {
            if !self.groups.contains_key(&group.id) {
                let handle = ExtWorkspaceGroupHandle::new(group);
                self.announce_group::<D>(&handle, dh, all_outputs);
                self.groups.insert(group.id.clone(), handle);
            }
        }

        for group in &update.groups {
            for workspace in &group.workspaces {
                if let Some(handle) = self.workspaces.get(&workspace.id) {
                    handle.update(&group.id, workspace);
                }
            }
        }

        let all_workspaces = self
            .workspaces
            .values().filter(|&workspace| !workspace.is_removed()).cloned()
            .collect::<Vec<_>>();
        for group in &update.groups {
            if let Some(handle) = self.groups.get(&group.id) {
                handle.update(group, dh, all_outputs, &all_workspaces);
            }
        }

        let removed_group_ids = self
            .groups
            .keys()
            .filter(|id| !next_group_ids.contains(*id))
            .cloned()
            .collect::<Vec<_>>();
        for id in removed_group_ids {
            if let Some(group) = self.groups.remove(&id) {
                group.remove(&all_workspaces);
            }
        }

        let removed_workspace_ids = self
            .workspaces
            .keys()
            .filter(|id| !next_workspace_ids.contains(*id))
            .cloned()
            .collect::<Vec<_>>();
        for id in removed_workspace_ids {
            if let Some(workspace) = self.workspaces.remove(&id) {
                workspace.remove();
            }
        }

        self.send_done();
    }

    pub fn refresh_outputs(&mut self, dh: &DisplayHandle, all_outputs: &[Output]) {
        let mut changed = false;
        for group in self.groups.values() {
            changed |= group.refresh_outputs(dh, all_outputs);
        }
        if changed {
            self.send_done();
        }
    }

    fn announce_workspace<D>(&self, handle: &ExtWorkspaceHandle, dh: &DisplayHandle)
    where
        D: Dispatch<ExtWorkspaceHandleV1, ExtWorkspaceHandle> + 'static,
    {
        for manager in &self.managers {
            let Some(client) = dh.get_client(manager.id()).ok() else {
                continue;
            };
            let Ok(resource) = client.create_resource::<ExtWorkspaceHandleV1, _, D>(
                dh,
                manager.version(),
                handle.clone(),
            ) else {
                continue;
            };
            manager.workspace(&resource);
            handle.init_instance(resource);
        }
    }

    fn announce_group<D>(
        &self,
        handle: &ExtWorkspaceGroupHandle,
        dh: &DisplayHandle,
        all_outputs: &[Output],
    ) where
        D: Dispatch<ExtWorkspaceGroupHandleV1, ExtWorkspaceGroupHandle> + 'static,
    {
        let workspaces = self.workspaces.values().cloned().collect::<Vec<_>>();
        for manager in &self.managers {
            let Some(client) = dh.get_client(manager.id()).ok() else {
                continue;
            };
            let Ok(resource) = client.create_resource::<ExtWorkspaceGroupHandleV1, _, D>(
                dh,
                manager.version(),
                handle.clone(),
            ) else {
                continue;
            };
            manager.workspace_group(&resource);
            handle.init_instance(resource, dh, all_outputs, &workspaces);
        }
    }

    fn send_done(&mut self) {
        self.managers.retain(|manager| manager.is_alive());
        for manager in &self.managers {
            manager.done();
        }
    }
}

pub trait ExtWorkspaceManagerHandler:
    GlobalDispatch<ExtWorkspaceManagerV1, ExtWorkspaceManagerGlobalData>
    + Dispatch<ExtWorkspaceManagerV1, ()>
    + Dispatch<ExtWorkspaceGroupHandleV1, ExtWorkspaceGroupHandle>
    + Dispatch<ExtWorkspaceHandleV1, ExtWorkspaceHandle>
{
    fn ext_workspace_manager_state(&mut self) -> &mut ExtWorkspaceManagerState;
    fn ext_workspace_outputs(&self) -> Vec<Output>;
    fn ext_workspace_activate(&mut self, workspace_id: String, group_id: Option<String>);
}

impl<D> GlobalDispatch<ExtWorkspaceManagerV1, ExtWorkspaceManagerGlobalData, D>
    for ExtWorkspaceManagerState
where
    D: ExtWorkspaceManagerHandler + 'static,
{
    fn bind(
        state: &mut D,
        dh: &DisplayHandle,
        client: &Client,
        resource: New<ExtWorkspaceManagerV1>,
        _global_data: &ExtWorkspaceManagerGlobalData,
        data_init: &mut DataInit<'_, D>,
    ) {
        let manager = data_init.init(resource, ());
        let manager_version = manager.version();
        let outputs = state.ext_workspace_outputs();
        let (workspaces, groups) = {
            let manager_state = state.ext_workspace_manager_state();
            (
                manager_state
                    .workspaces
                    .values()
                    .cloned()
                    .collect::<Vec<_>>(),
                manager_state.groups.values().cloned().collect::<Vec<_>>(),
            )
        };

        for workspace in &workspaces {
            let Ok(resource) = client.create_resource::<ExtWorkspaceHandleV1, _, D>(
                dh,
                manager_version,
                workspace.clone(),
            ) else {
                continue;
            };
            manager.workspace(&resource);
            workspace.init_instance(resource);
        }

        for group in &groups {
            let Ok(resource) = client.create_resource::<ExtWorkspaceGroupHandleV1, _, D>(
                dh,
                manager_version,
                group.clone(),
            ) else {
                continue;
            };
            manager.workspace_group(&resource);
            group.init_instance(resource, dh, &outputs, &workspaces);
        }

        manager.done();
        state.ext_workspace_manager_state().managers.push(manager);
    }
}

impl<D> Dispatch<ExtWorkspaceManagerV1, (), D> for ExtWorkspaceManagerState
where
    D: ExtWorkspaceManagerHandler,
{
    fn request(
        state: &mut D,
        _client: &Client,
        manager: &ExtWorkspaceManagerV1,
        request: ext_workspace_manager_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            ext_workspace_manager_v1::Request::Commit => {}
            ext_workspace_manager_v1::Request::Stop => {
                manager.finished();
                state
                    .ext_workspace_manager_state()
                    .managers
                    .retain(|instance| instance != manager);
            }
            _ => unreachable!(),
        }
    }

    fn destroyed(state: &mut D, _client: ClientId, resource: &ExtWorkspaceManagerV1, _data: &()) {
        state
            .ext_workspace_manager_state()
            .managers
            .retain(|instance| instance != resource);
    }
}

impl<D> Dispatch<ExtWorkspaceGroupHandleV1, ExtWorkspaceGroupHandle, D> for ExtWorkspaceManagerState
where
    D: ExtWorkspaceManagerHandler,
{
    fn request(
        _state: &mut D,
        _client: &Client,
        _resource: &ExtWorkspaceGroupHandleV1,
        request: ext_workspace_group_handle_v1::Request,
        _data: &ExtWorkspaceGroupHandle,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            ext_workspace_group_handle_v1::Request::CreateWorkspace { .. } => {}
            ext_workspace_group_handle_v1::Request::Destroy => {}
            _ => unreachable!(),
        }
    }
}

impl<D> Dispatch<ExtWorkspaceHandleV1, ExtWorkspaceHandle, D> for ExtWorkspaceManagerState
where
    D: ExtWorkspaceManagerHandler,
{
    fn request(
        state: &mut D,
        _client: &Client,
        _resource: &ExtWorkspaceHandleV1,
        request: ext_workspace_handle_v1::Request,
        handle: &ExtWorkspaceHandle,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            ext_workspace_handle_v1::Request::Activate => {
                state.ext_workspace_activate(handle.id(), handle.group_id());
            }
            ext_workspace_handle_v1::Request::Deactivate
            | ext_workspace_handle_v1::Request::Assign { .. }
            | ext_workspace_handle_v1::Request::Remove => {}
            ext_workspace_handle_v1::Request::Destroy => {}
            _ => unreachable!(),
        }
    }
}

impl ExtWorkspaceManagerHandler for ShojiWM {
    fn ext_workspace_manager_state(&mut self) -> &mut ExtWorkspaceManagerState {
        &mut self.ext_workspace_manager_state
    }

    fn ext_workspace_outputs(&self) -> Vec<Output> {
        self.space.outputs().cloned().collect()
    }

    fn ext_workspace_activate(&mut self, workspace_id: String, group_id: Option<String>) {
        let event = RuntimeWorkspaceActivateRequestSnapshot {
            workspace_id,
            group_id,
        };
        let now_ms = Duration::from(self.clock.now()).as_millis() as u64;
        self.sync_runtime_display_state();
        match self.decoration_evaluator.workspace_activate(&event, now_ms) {
            Ok(invocation) => {
                let dirty_window_ids = invocation.dirty_window_ids.clone();
                let dirty_managed_window_ids = invocation.dirty_managed_window_ids.clone();
                let actions = invocation.actions.clone();
                let next_poll_in_ms = invocation.next_poll_in_ms;
                self.consume_runtime_lifecycle_invocation(invocation);

                if !dirty_window_ids.is_empty() || !dirty_managed_window_ids.is_empty() {
                    self.runtime_poll_dirty = true;
                    self.mark_runtime_dirty_windows(dirty_window_ids, dirty_managed_window_ids);
                    self.request_tty_maintenance("workspace-manager-activate-dirty");
                    self.schedule_redraw();
                }
                if !actions.is_empty() {
                    self.request_tty_maintenance("workspace-manager-activate-actions");
                    self.apply_runtime_window_actions(actions);
                    self.schedule_redraw();
                }
                self.runtime_scheduler_enabled = next_poll_in_ms.is_some();
                if next_poll_in_ms.is_some() {
                    self.schedule_runtime_scheduler_kick_from_state(next_poll_in_ms);
                }
                if next_poll_in_ms == Some(0) {
                    self.request_tty_maintenance("workspace-manager-activate-animation");
                    self.schedule_redraw();
                }
            }
            Err(error) => {
                tracing::warn!(?error, "workspace activate handler failed");
            }
        }
    }
}

macro_rules! delegate_ext_workspace_manager {
    ($ty: ty) => {
        const _: () = {
            use $crate::workspace_manager::{
                ExtWorkspaceGroupHandle, ExtWorkspaceHandle, ExtWorkspaceManagerGlobalData,
                ExtWorkspaceManagerState,
            };
            use smithay::reexports::wayland_server::{
                delegate_dispatch, delegate_global_dispatch,
            };
            use wayland_protocols::ext::workspace::v1::server::{
                ext_workspace_group_handle_v1::ExtWorkspaceGroupHandleV1,
                ext_workspace_handle_v1::ExtWorkspaceHandleV1,
                ext_workspace_manager_v1::ExtWorkspaceManagerV1,
            };

            delegate_global_dispatch!(
                $ty: [ExtWorkspaceManagerV1: ExtWorkspaceManagerGlobalData] => ExtWorkspaceManagerState
            );
            delegate_dispatch!(
                $ty: [ExtWorkspaceManagerV1: ()] => ExtWorkspaceManagerState
            );
            delegate_dispatch!(
                $ty: [ExtWorkspaceGroupHandleV1: ExtWorkspaceGroupHandle] => ExtWorkspaceManagerState
            );
            delegate_dispatch!(
                $ty: [ExtWorkspaceHandleV1: ExtWorkspaceHandle] => ExtWorkspaceManagerState
            );
        };
    };
}

pub(crate) use delegate_ext_workspace_manager;
