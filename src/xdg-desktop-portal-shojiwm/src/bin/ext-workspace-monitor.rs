//! Debug helper: monitor ext-workspace-v1 events, flagging protocol anomalies
//! such as duplicate `workspace_enter` for a workspace already in the group
//! (the way external bars end up rendering duplicated workspace buttons).
//!
//!   WAYLAND_DISPLAY=wayland-2 ext-workspace-monitor [seconds] [--create NAME]
//!
//! With `--create`, after the first `done` it asks the first group that
//! advertises `create_workspace` for a workspace named NAME, then activates
//! the workspace whose id ends in `:NAME` once it appears.
//!
//! Exits non-zero if any duplicate enter was observed.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use wayland_client::protocol::wl_registry;
use wayland_client::{Connection, Dispatch, QueueHandle, event_created_child};
use wayland_protocols::ext::workspace::v1::client::{
    ext_workspace_group_handle_v1::{self, ExtWorkspaceGroupHandleV1},
    ext_workspace_handle_v1::{self, ExtWorkspaceHandleV1},
    ext_workspace_manager_v1::{self, ExtWorkspaceManagerV1},
};

#[derive(Default)]
struct App {
    workspace_ids: HashMap<u32, String>,
    group_members: HashMap<u32, Vec<u32>>,
    duplicate_enters: u32,
    events: u32,
    manager: Option<ExtWorkspaceManagerV1>,
    /// Groups that advertise `create_workspace`.
    creatable_groups: Vec<ExtWorkspaceGroupHandleV1>,
    create: Option<String>,
    create_sent: bool,
    activated: bool,
}

fn proto_id<P: wayland_client::Proxy>(proxy: &P) -> u32 {
    proxy.id().protocol_id()
}

impl Dispatch<wl_registry::WlRegistry, ()> for App {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name, interface, ..
        } = event
            && interface == "ext_workspace_manager_v1"
        {
            let manager = registry.bind::<ExtWorkspaceManagerV1, _, _>(name, 1, qh, ());
            println!("bound ext_workspace_manager_v1");
            state.manager = Some(manager);
        }
    }
}

impl Dispatch<ExtWorkspaceManagerV1, ()> for App {
    fn event(
        state: &mut Self,
        _: &ExtWorkspaceManagerV1,
        event: ext_workspace_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        state.events += 1;
        match event {
            ext_workspace_manager_v1::Event::WorkspaceGroup { workspace_group } => {
                println!("event: new group #{}", proto_id(&workspace_group));
                state
                    .group_members
                    .insert(proto_id(&workspace_group), Vec::new());
            }
            ext_workspace_manager_v1::Event::Workspace { workspace } => {
                println!("event: new workspace #{}", proto_id(&workspace));
            }
            ext_workspace_manager_v1::Event::Done => {
                let mut summary = Vec::new();
                for (group, members) in &state.group_members {
                    let names = members
                        .iter()
                        .map(|member| {
                            state
                                .workspace_ids
                                .get(member)
                                .cloned()
                                .unwrap_or_else(|| format!("#{member}"))
                        })
                        .collect::<Vec<_>>();
                    summary.push(format!("group#{group}=[{}]", names.join(",")));
                }
                println!("done: {}", summary.join(" "));
                if let (Some(name), false) = (state.create.clone(), state.create_sent) {
                    match (state.creatable_groups.first(), &state.manager) {
                        (Some(group), Some(manager)) => {
                            println!("request: create_workspace({name}) on group#{}", proto_id(group));
                            group.create_workspace(name);
                            manager.commit();
                            state.create_sent = true;
                        }
                        _ => println!("no group advertises create_workspace"),
                    }
                }
            }
            _ => {}
        }
    }

    event_created_child!(App, ExtWorkspaceManagerV1, [
        ext_workspace_manager_v1::EVT_WORKSPACE_GROUP_OPCODE => (ExtWorkspaceGroupHandleV1, ()),
        ext_workspace_manager_v1::EVT_WORKSPACE_OPCODE => (ExtWorkspaceHandleV1, ()),
    ]);
}

impl Dispatch<ExtWorkspaceGroupHandleV1, ()> for App {
    fn event(
        state: &mut Self,
        group: &ExtWorkspaceGroupHandleV1,
        event: ext_workspace_group_handle_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        state.events += 1;
        let group_id = proto_id(group);
        match event {
            ext_workspace_group_handle_v1::Event::WorkspaceEnter { workspace } => {
                let workspace_num = proto_id(&workspace);
                let name = state
                    .workspace_ids
                    .get(&workspace_num)
                    .cloned()
                    .unwrap_or_else(|| format!("#{workspace_num}"));
                let members = state.group_members.entry(group_id).or_default();
                if members.contains(&workspace_num) {
                    state.duplicate_enters += 1;
                    println!("!!! DUPLICATE workspace_enter: group#{group_id} <- {name}");
                } else {
                    println!("event: workspace_enter group#{group_id} <- {name}");
                }
                members.push(workspace_num);
            }
            ext_workspace_group_handle_v1::Event::WorkspaceLeave { workspace } => {
                let workspace_num = proto_id(&workspace);
                let name = state
                    .workspace_ids
                    .get(&workspace_num)
                    .cloned()
                    .unwrap_or_else(|| format!("#{workspace_num}"));
                println!("event: workspace_leave group#{group_id} -> {name}");
                if let Some(members) = state.group_members.get_mut(&group_id) {
                    members.retain(|member| *member != workspace_num);
                }
            }
            ext_workspace_group_handle_v1::Event::Capabilities { capabilities } => {
                println!("event: group#{group_id} capabilities={capabilities:?}");
                let can_create = matches!(
                    capabilities,
                    wayland_client::WEnum::Value(capabilities)
                        if capabilities.contains(ext_workspace_group_handle_v1::GroupCapabilities::CreateWorkspace)
                );
                state.creatable_groups.retain(|known| known != group);
                if can_create {
                    state.creatable_groups.push(group.clone());
                }
            }
            ext_workspace_group_handle_v1::Event::Removed => {
                println!("event: group#{group_id} removed");
                state.group_members.remove(&group_id);
            }
            _ => {}
        }
    }
}

impl Dispatch<ExtWorkspaceHandleV1, ()> for App {
    fn event(
        state: &mut Self,
        workspace: &ExtWorkspaceHandleV1,
        event: ext_workspace_handle_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        state.events += 1;
        let workspace_num = proto_id(workspace);
        match event {
            ext_workspace_handle_v1::Event::Id { id } => {
                println!("event: workspace #{workspace_num} id={id}");
                let created = state
                    .create
                    .as_ref()
                    .is_some_and(|name| state.create_sent && id.ends_with(&format!(":{name}")));
                if created && !state.activated {
                    if let Some(manager) = &state.manager {
                        println!("request: activate {id}");
                        workspace.activate();
                        manager.commit();
                        state.activated = true;
                    }
                }
                state.workspace_ids.insert(workspace_num, id);
            }
            ext_workspace_handle_v1::Event::State { state: ws_state } => {
                println!("event: workspace #{workspace_num} state={ws_state:?}");
            }
            ext_workspace_handle_v1::Event::Removed => {
                println!("event: workspace #{workspace_num} removed");
            }
            _ => {}
        }
    }
}

fn main() {
    let args = std::env::args().collect::<Vec<_>>();
    let seconds: u64 = args
        .get(1)
        .and_then(|value| value.parse().ok())
        .unwrap_or(10);
    let create = args
        .iter()
        .position(|arg| arg == "--create")
        .and_then(|index| args.get(index + 1))
        .cloned();
    let conn = Connection::connect_to_env().expect("connect");
    let mut queue = conn.new_event_queue::<App>();
    let qh = queue.handle();
    let _registry = conn.display().get_registry(&qh, ());
    let mut app = App {
        create,
        ..App::default()
    };

    let deadline = Instant::now() + Duration::from_secs(seconds);
    while Instant::now() < deadline {
        queue.flush().expect("flush");
        conn.prepare_read().map(|guard| {
            let _ = guard.read_without_dispatch();
        });
        queue
            .dispatch_pending(&mut app)
            .expect("dispatch");
        std::thread::sleep(Duration::from_millis(20));
    }
    println!(
        "summary: events={} duplicate_enters={}",
        app.events, app.duplicate_enters
    );
    std::process::exit(if app.duplicate_enters > 0 { 1 } else { 0 });
}
