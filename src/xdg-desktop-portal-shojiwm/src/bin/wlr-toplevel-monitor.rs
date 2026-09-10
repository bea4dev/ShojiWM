//! Debug helper: monitor wlr-foreign-toplevel-management events, printing
//! every title/app_id/state/output_enter/output_leave/done per handle and a
//! per-`done` summary of which outputs each toplevel currently advertises.
//! Optionally sends `set_minimized` / `unset_minimized` to a toplevel by
//! app_id after a delay, the way a taskbar click would.
//!
//!   WAYLAND_DISPLAY=wayland-2 wlr-toplevel-monitor [seconds] \
//!       [--minimize <app_id> [--after <seconds>]] [--unminimize <app_id>]
//!
//! Flags "!!! NO OUTPUTS" when a toplevel that once entered an output ends a
//! `done` batch with no outputs — the condition under which per-output
//! taskbars (e.g. noctalia) drop the window's icon.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use wayland_client::protocol::{wl_output, wl_registry, wl_seat};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, event_created_child};
use wayland_protocols_wlr::foreign_toplevel::v1::client::{
    zwlr_foreign_toplevel_handle_v1::{self, ZwlrForeignToplevelHandleV1},
    zwlr_foreign_toplevel_manager_v1::{self, ZwlrForeignToplevelManagerV1},
};

#[derive(Default)]
struct Toplevel {
    title: String,
    app_id: String,
    outputs: Vec<u32>,
    saw_output_enter: bool,
    states: Vec<u32>,
}

#[derive(Default)]
struct App {
    seat: Option<wl_seat::WlSeat>,
    manager_global: Option<(u32, u32)>,
    output_names: HashMap<u32, String>,
    toplevels: HashMap<u32, Toplevel>,
    handles: Vec<ZwlrForeignToplevelHandleV1>,
    no_output_batches: u32,
    start: Option<Instant>,
}

impl App {
    fn stamp(&self) -> String {
        let elapsed = self.start.map_or(0.0, |start| start.elapsed().as_secs_f64());
        format!("[{elapsed:7.3}s]")
    }

    fn output_name(&self, id: u32) -> String {
        self.output_names
            .get(&id)
            .cloned()
            .unwrap_or_else(|| format!("wl_output#{id}"))
    }

    fn label(&self, id: u32) -> String {
        self.toplevels.get(&id).map_or_else(
            || format!("#{id}"),
            |toplevel| format!("#{id}({} \"{}\")", toplevel.app_id, toplevel.title),
        )
    }
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
            name,
            interface,
            version,
        } = event
        {
            match interface.as_str() {
                "wl_seat" => {
                    state.seat =
                        Some(registry.bind::<wl_seat::WlSeat, _, _>(name, version.min(7), qh, ()));
                }
                "wl_output" => {
                    registry.bind::<wl_output::WlOutput, _, _>(name, version.min(4), qh, ());
                }
                "zwlr_foreign_toplevel_manager_v1" => {
                    // Bind the manager only after wl_output binds have been
                    // processed (see main), so initial output_enter events can
                    // reference this client's output resources.
                    state.manager_global = Some((name, version.min(3)));
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for App {
    fn event(
        _: &mut Self,
        _: &wl_seat::WlSeat,
        _: wl_seat::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_output::WlOutput, ()> for App {
    fn event(
        state: &mut Self,
        output: &wl_output::WlOutput,
        event: wl_output::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_output::Event::Name { name } = event {
            state.output_names.insert(output.id().protocol_id(), name);
        }
    }
}

impl Dispatch<ZwlrForeignToplevelManagerV1, ()> for App {
    fn event(
        _: &mut Self,
        _: &ZwlrForeignToplevelManagerV1,
        _: zwlr_foreign_toplevel_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }

    event_created_child!(App, ZwlrForeignToplevelManagerV1, [
        zwlr_foreign_toplevel_manager_v1::EVT_TOPLEVEL_OPCODE => (ZwlrForeignToplevelHandleV1, ()),
    ]);
}

impl Dispatch<ZwlrForeignToplevelHandleV1, ()> for App {
    fn event(
        state: &mut Self,
        handle: &ZwlrForeignToplevelHandleV1,
        event: zwlr_foreign_toplevel_handle_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let id = handle.id().protocol_id();
        if !state.handles.iter().any(|h| h == handle) {
            state.handles.push(handle.clone());
        }
        state.toplevels.entry(id).or_default();
        let stamp = state.stamp();
        match event {
            zwlr_foreign_toplevel_handle_v1::Event::Title { title } => {
                println!("{stamp} #{id} title={title:?}");
                state.toplevels.get_mut(&id).unwrap().title = title;
            }
            zwlr_foreign_toplevel_handle_v1::Event::AppId { app_id } => {
                println!("{stamp} #{id} app_id={app_id:?}");
                state.toplevels.get_mut(&id).unwrap().app_id = app_id;
            }
            zwlr_foreign_toplevel_handle_v1::Event::OutputEnter { output } => {
                let output_id = output.id().protocol_id();
                let name = state.output_name(output_id);
                println!("{stamp} {} output_enter {name}", state.label(id));
                let toplevel = state.toplevels.get_mut(&id).unwrap();
                toplevel.saw_output_enter = true;
                if !toplevel.outputs.contains(&output_id) {
                    toplevel.outputs.push(output_id);
                }
            }
            zwlr_foreign_toplevel_handle_v1::Event::OutputLeave { output } => {
                let output_id = output.id().protocol_id();
                let name = state.output_name(output_id);
                println!("{stamp} {} output_leave {name}", state.label(id));
                let toplevel = state.toplevels.get_mut(&id).unwrap();
                toplevel.outputs.retain(|current| *current != output_id);
            }
            zwlr_foreign_toplevel_handle_v1::Event::State { state: raw } => {
                let states: Vec<u32> = raw
                    .chunks_exact(4)
                    .map(|chunk| u32::from_ne_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
                    .collect();
                let names: Vec<&str> = states
                    .iter()
                    .map(|value| match value {
                        0 => "maximized",
                        1 => "minimized",
                        2 => "activated",
                        3 => "fullscreen",
                        _ => "?",
                    })
                    .collect();
                println!("{stamp} {} state={names:?}", state.label(id));
                state.toplevels.get_mut(&id).unwrap().states = states;
            }
            zwlr_foreign_toplevel_handle_v1::Event::Done => {
                let toplevel = &state.toplevels[&id];
                let outputs: Vec<String> = toplevel
                    .outputs
                    .iter()
                    .map(|output| state.output_name(*output))
                    .collect();
                let minimized = toplevel.states.contains(&1);
                let flag = if toplevel.saw_output_enter && outputs.is_empty() {
                    state.no_output_batches += 1;
                    " !!! NO OUTPUTS"
                } else {
                    ""
                };
                println!(
                    "{stamp} {} done outputs={outputs:?} minimized={minimized}{flag}",
                    state.label(id)
                );
            }
            zwlr_foreign_toplevel_handle_v1::Event::Closed => {
                println!("{stamp} {} closed", state.label(id));
                state.toplevels.remove(&id);
                state.handles.retain(|h| h != handle);
            }
            _ => {}
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut seconds: u64 = 10;
    let mut minimize: Option<String> = None;
    let mut unminimize: Option<String> = None;
    let mut after: u64 = 2;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--minimize" => {
                minimize = args.get(index + 1).cloned();
                index += 1;
            }
            "--unminimize" => {
                unminimize = args.get(index + 1).cloned();
                index += 1;
            }
            "--after" => {
                after = args
                    .get(index + 1)
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(after);
                index += 1;
            }
            value => {
                if let Ok(parsed) = value.parse() {
                    seconds = parsed;
                }
            }
        }
        index += 1;
    }

    let conn = Connection::connect_to_env().expect("connect");
    let mut queue = conn.new_event_queue::<App>();
    let qh = queue.handle();
    let registry = conn.display().get_registry(&qh, ());
    let mut app = App {
        start: Some(Instant::now()),
        ..App::default()
    };
    queue.roundtrip(&mut app).expect("roundtrip registry");
    queue.roundtrip(&mut app).expect("roundtrip outputs");
    let (name, version) = app.manager_global.expect("no zwlr_foreign_toplevel_manager_v1");
    registry.bind::<ZwlrForeignToplevelManagerV1, _, _>(name, version, &qh, ());

    let deadline = Instant::now() + Duration::from_secs(seconds);
    let action_at = Instant::now() + Duration::from_secs(after);
    let mut action_sent = false;
    while Instant::now() < deadline {
        queue.flush().expect("flush");
        if let Some(guard) = conn.prepare_read() {
            let _ = guard.read_without_dispatch();
        }
        queue.dispatch_pending(&mut app).expect("dispatch");

        if !action_sent && Instant::now() >= action_at {
            action_sent = true;
            for (target, set) in [(&minimize, true), (&unminimize, false)] {
                let Some(target) = target else { continue };
                let handle = app.handles.iter().find(|h| {
                    app.toplevels
                        .get(&h.id().protocol_id())
                        .is_some_and(|toplevel| &toplevel.app_id == target)
                });
                match handle {
                    Some(handle) => {
                        if set {
                            handle.set_minimized();
                        } else {
                            handle.unset_minimized();
                        }
                        println!(
                            "{} >>> sent {} to {target}",
                            app.stamp(),
                            if set { "set_minimized" } else { "unset_minimized" }
                        );
                    }
                    None => println!("{} >>> no toplevel with app_id {target:?}", app.stamp()),
                }
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    println!(
        "summary: toplevels={} no_output_batches={}",
        app.toplevels.len(),
        app.no_output_batches
    );
    std::process::exit(if app.no_output_batches > 0 { 1 } else { 0 });
}
