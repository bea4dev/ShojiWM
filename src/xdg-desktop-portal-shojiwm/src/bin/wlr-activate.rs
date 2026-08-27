//! Debug helper: activate a toplevel by app_id via
//! wlr-foreign-toplevel-management, like a taskbar click would.
//!
//!   WAYLAND_DISPLAY=wayland-2 wlr-activate org.gnome.TextEditor

use std::collections::HashMap;

use wayland_client::protocol::{wl_registry, wl_seat};
use wayland_client::{Connection, Dispatch, QueueHandle, event_created_child};
use wayland_protocols_wlr::foreign_toplevel::v1::client::{
    zwlr_foreign_toplevel_handle_v1::{self, ZwlrForeignToplevelHandleV1},
    zwlr_foreign_toplevel_manager_v1::{self, ZwlrForeignToplevelManagerV1},
};

#[derive(Default)]
struct App {
    seat: Option<wl_seat::WlSeat>,
    manager_bound: bool,
    app_ids: HashMap<u32, String>,
    handles: Vec<ZwlrForeignToplevelHandleV1>,
    done: bool,
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
                "zwlr_foreign_toplevel_manager_v1" => {
                    registry.bind::<ZwlrForeignToplevelManagerV1, _, _>(
                        name,
                        version.min(3),
                        qh,
                        (),
                    );
                    state.manager_bound = true;
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

impl Dispatch<ZwlrForeignToplevelManagerV1, ()> for App {
    fn event(
        state: &mut Self,
        _: &ZwlrForeignToplevelManagerV1,
        event: zwlr_foreign_toplevel_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwlr_foreign_toplevel_manager_v1::Event::Finished = event {
            state.done = true;
        }
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
        use wayland_client::Proxy;
        match event {
            zwlr_foreign_toplevel_handle_v1::Event::AppId { app_id } => {
                state.app_ids.insert(handle.id().protocol_id(), app_id);
                if !state.handles.iter().any(|h| h == handle) {
                    state.handles.push(handle.clone());
                }
            }
            _ => {}
        }
    }
}

fn main() {
    use wayland_client::Proxy;
    let target = std::env::args().nth(1).expect("usage: wlr-activate <app_id>");
    let conn = Connection::connect_to_env().expect("connect");
    let mut queue = conn.new_event_queue::<App>();
    let qh = queue.handle();
    let _registry = conn.display().get_registry(&qh, ());
    let mut app = App::default();
    queue.roundtrip(&mut app).expect("roundtrip 1");
    queue.roundtrip(&mut app).expect("roundtrip 2");

    let seat = app.seat.clone().expect("no wl_seat");
    let handle = app
        .handles
        .iter()
        .find(|h| {
            app.app_ids
                .get(&h.id().protocol_id())
                .is_some_and(|id| id == &target)
        })
        .unwrap_or_else(|| panic!("no toplevel with app_id {target:?}"));
    handle.activate(&seat);
    queue.roundtrip(&mut app).expect("roundtrip 3");
    println!("activated {target}");
}
