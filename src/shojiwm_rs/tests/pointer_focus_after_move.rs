//! Windows the config moves under a pointer that stays still take the
//! pointer: the window that slid under it gets hover, and during a data drag
//! it becomes the drop target, so a drop without further motion lands there.

mod common;

use std::{
    cell::RefCell,
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixStream,
    path::PathBuf,
    rc::Rc,
    time::Duration,
};

use shojiwm_rs::{ipc::IpcServer, prelude::*};
use smithay::{
    backend::input::ButtonState,
    input::pointer::{ButtonEvent, MotionEvent},
    reexports::wayland_server::Resource,
    utils::SERIAL_COUNTER,
};
use wayland_client::{
    Connection, Dispatch, Proxy, QueueHandle, event_created_child,
    protocol::{
        wl_data_device, wl_data_device_manager, wl_data_offer, wl_data_source, wl_pointer, wl_seat,
    },
};

const BTN_LEFT: u32 = 0x110;
const SIZE: i32 = 300;

thread_local! {
    static IPC: RefCell<Option<IpcServer>> = const { RefCell::new(None) };
    static EVENTS: RefCell<Vec<Seen>> = const { RefCell::new(Vec::new()) };
}

#[derive(Debug, Clone, PartialEq)]
enum Seen {
    Button(u32),
    DragEnter(u32),
    Drop,
}

fn seen() -> Vec<Seen> {
    EVENTS.with(|events| events.borrow().clone())
}

fn ipc_path() -> PathBuf {
    std::env::temp_dir().join(format!("shoji-focus-ipc-{}.sock", std::process::id()))
}

/// Window `a` on the left and `b` on the right, or the other way round when
/// an IPC client asks.
fn setup() {
    let swapped = signal(false);
    let ipc = IpcServer::bind(ipc_path()).unwrap();
    ipc.handle("swap", move |_| {
        swapped.set(!swapped.get());
        Ok(serde_json::Value::Null)
    });
    IPC.with(|slot| *slot.borrow_mut() = Some(ipc));
    COMPOSITOR.window.composition(move |window| {
        let app_id = window.app_id();
        ManagedWindow::new()
            .rect(Prop::Derived(Rc::new(move || {
                let left = (app_id.get().as_deref() == Some("a")) != swapped.get();
                let x = if left { 0.0 } else { 500.0 };
                Rect::new(x, 0.0, SIZE as f64, SIZE as f64)
            })))
            .child(ClientWindow::new())
    });
}

/// Swap the windows from the config, as it moves windows from its handlers,
/// then do what a frame does first: re-read the windows.
fn swap(session: &mut common::Session, ipc: &mut BufReader<UnixStream>) {
    ipc.get_mut()
        .write_all(b"{\"id\":1,\"method\":\"swap\"}\n")
        .unwrap();
    let mut reply = String::new();
    for _ in 0..200 {
        session.roundtrip();
        if ipc.read_line(&mut reply).is_ok_and(|read| read > 0) {
            break;
        }
    }
    assert!(reply.contains("\"result\""), "no answer to swap: {reply:?}");
    session.state.refresh_window_decorations().unwrap();
    session.roundtrip();
}

fn move_pointer(session: &mut common::Session, x: f64, y: f64) {
    let state = &mut session.state;
    let pointer = state.seat.get_pointer().unwrap();
    let location = (x, y).into();
    state.pointer_contents = state.pointer_contents_at(location);
    let under = state.pointer_contents.surface.clone();
    pointer.motion(
        state,
        under,
        &MotionEvent {
            location,
            serial: SERIAL_COUNTER.next_serial(),
            time: 0,
        },
    );
    pointer.frame(state);
    session.roundtrip();
}

fn button(session: &mut common::Session, state: ButtonState) {
    let pointer = session.state.seat.get_pointer().unwrap();
    pointer.button(
        &mut session.state,
        &ButtonEvent {
            serial: SERIAL_COUNTER.next_serial(),
            time: 0,
            button: BTN_LEFT,
            state,
        },
    );
    pointer.frame(&mut session.state);
    session.roundtrip();
}

fn pointer_focus(session: &common::Session) -> Option<u32> {
    let pointer = session.state.seat.get_pointer().unwrap();
    pointer
        .current_focus()
        .map(|surface| surface.id().protocol_id())
}

#[test]
fn pointer_focus_follows_windows_moving_under_a_still_pointer() {
    let mut session = common::Session::new(setup);
    let surface_a = session.map_toplevel_with_buffer("a", SIZE, SIZE);
    let surface_b = session.map_toplevel_with_buffer("b", SIZE, SIZE);
    let (a, b) = (surface_a.id().protocol_id(), surface_b.id().protocol_id());
    let mut ipc = BufReader::new(UnixStream::connect(ipc_path()).unwrap());
    ipc.get_ref()
        .set_read_timeout(Some(Duration::from_millis(5)))
        .unwrap();
    let qh = session.queue.handle();
    let seat: wl_seat::WlSeat = session.bind("wl_seat", 1);
    seat.get_pointer(&qh, ());
    let devices: wl_data_device_manager::WlDataDeviceManager =
        session.bind("wl_data_device_manager", 1);
    let device = devices.get_data_device(&seat, &qh, ());
    move_pointer(&mut session, 150.0, 150.0);
    assert_eq!(pointer_focus(&session), Some(a));

    // Hover: b slides under the pointer, then a again.
    swap(&mut session, &mut ipc);
    assert_eq!(pointer_focus(&session), Some(b));
    swap(&mut session, &mut ipc);
    assert_eq!(pointer_focus(&session), Some(a));

    // A drag from a, held still while b slides under it, drops on b.
    button(&mut session, ButtonState::Pressed);
    let Some(Seen::Button(serial)) = seen().last().cloned() else {
        panic!("no button press reached the client: {:?}", seen());
    };
    let source = devices.create_data_source(&qh, ());
    source.offer("text/plain".into());
    device.start_drag(Some(&source), &surface_a, None, serial);
    session.roundtrip();
    // The drag enters its first target on motion.
    move_pointer(&mut session, 151.0, 150.0);
    assert_eq!(seen().last(), Some(&Seen::DragEnter(a)));

    swap(&mut session, &mut ipc);
    assert_eq!(seen().last(), Some(&Seen::DragEnter(b)));
    button(&mut session, ButtonState::Released);
    assert_eq!(seen().last(), Some(&Seen::Drop));
}

impl Dispatch<wl_pointer::WlPointer, ()> for common::Client {
    fn event(
        _: &mut Self,
        _: &wl_pointer::WlPointer,
        event: wl_pointer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_pointer::Event::Button {
            serial,
            state: wayland_client::WEnum::Value(wl_pointer::ButtonState::Pressed),
            ..
        } = event
        {
            EVENTS.with(|events| events.borrow_mut().push(Seen::Button(serial)));
        }
    }
}

impl Dispatch<wl_data_device::WlDataDevice, ()> for common::Client {
    fn event(
        _: &mut Self,
        _: &wl_data_device::WlDataDevice,
        event: wl_data_device::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let seen = match event {
            wl_data_device::Event::Enter { surface, .. } => {
                Seen::DragEnter(surface.id().protocol_id())
            }
            wl_data_device::Event::Drop => Seen::Drop,
            _ => return,
        };
        EVENTS.with(|events| events.borrow_mut().push(seen));
    }

    event_created_child!(common::Client, wl_data_device::WlDataDevice, [
        wl_data_device::EVT_DATA_OFFER_OPCODE => (wl_data_offer::WlDataOffer, ()),
    ]);
}

macro_rules! ignore_events {
    ($($proxy:ty),*) => {$(
        impl Dispatch<$proxy, ()> for common::Client {
            fn event(_: &mut Self, _: &$proxy, _: <$proxy as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
        }
    )*};
}

ignore_events!(
    wl_data_device_manager::WlDataDeviceManager,
    wl_data_offer::WlDataOffer,
    wl_data_source::WlDataSource
);
