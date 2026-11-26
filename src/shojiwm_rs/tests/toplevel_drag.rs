//! xdg-toplevel-drag-v1: a window attached to a data drag follows the
//! pointer with the client's offset, stays where the drop leaves it, and is
//! never the drop target itself.

mod common;

use std::cell::RefCell;

use smithay::{
    backend::input::ButtonState,
    input::pointer::{ButtonEvent, MotionEvent},
    reexports::wayland_server::Resource,
    utils::{Logical, Point, SERIAL_COUNTER},
};
use wayland_client::{
    Connection, Dispatch, Proxy, QueueHandle, event_created_child,
    protocol::{
        wl_compositor, wl_data_device, wl_data_device_manager, wl_data_offer, wl_data_source,
        wl_pointer, wl_seat, wl_surface,
    },
};
use wayland_protocols::xdg::{
    shell::client::{xdg_toplevel, xdg_wm_base},
    toplevel_drag::v1::client::{xdg_toplevel_drag_manager_v1, xdg_toplevel_drag_v1},
};

const BTN_LEFT: u32 = 0x110;

thread_local! {
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

fn setup() {}

/// The client's side of a drag: the seat's pointer and data device.
struct Drag {
    devices: wl_data_device_manager::WlDataDeviceManager,
    device: wl_data_device::WlDataDevice,
    manager: xdg_toplevel_drag_manager_v1::XdgToplevelDragManagerV1,
}

impl Drag {
    fn new(session: &mut common::Session) -> Self {
        let qh = session.queue.handle();
        let seat: wl_seat::WlSeat = session.bind("wl_seat", 1);
        seat.get_pointer(&qh, ());
        let devices: wl_data_device_manager::WlDataDeviceManager =
            session.bind("wl_data_device_manager", 1);
        let device = devices.get_data_device(&seat, &qh, ());
        let manager = session.bind("xdg_toplevel_drag_manager_v1", 1);
        Self {
            devices,
            device,
            manager,
        }
    }

    /// Press on `origin` and start a drag of a new source; returns the
    /// source's toplevel drag object.
    fn start(
        &self,
        session: &mut common::Session,
        origin: &wl_surface::WlSurface,
        attach_first: Option<(&xdg_toplevel::XdgToplevel, i32, i32)>,
    ) -> xdg_toplevel_drag_v1::XdgToplevelDragV1 {
        button(session, ButtonState::Pressed);
        let Some(Seen::Button(serial)) = seen().last().cloned() else {
            panic!("no button press reached the client: {:?}", seen());
        };
        let qh = session.queue.handle();
        let source = self.devices.create_data_source(&qh, ());
        source.offer("text/plain".into());
        let drag = self.manager.get_xdg_toplevel_drag(&source, &qh, ());
        if let Some((toplevel, x, y)) = attach_first {
            drag.attach(toplevel, x, y);
        }
        self.device.start_drag(Some(&source), origin, None, serial);
        session.roundtrip();
        drag
    }
}

/// A toplevel with `app_id`; with a `width`x`height` buffer, or none yet.
fn toplevel(
    session: &mut common::Session,
    app_id: &str,
    buffer: Option<(i32, i32)>,
) -> (wl_surface::WlSurface, xdg_toplevel::XdgToplevel) {
    let qh = session.queue.handle();
    let compositor: wl_compositor::WlCompositor = session.bind("wl_compositor", 1);
    let shell: xdg_wm_base::XdgWmBase = session.bind("xdg_wm_base", 1);
    let surface = compositor.create_surface(&qh, ());
    let xdg = shell.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    toplevel.set_app_id(app_id.into());
    surface.commit();
    session.roundtrip();
    if let Some((width, height)) = buffer {
        session.attach_buffer(&surface, width, height);
    }
    (surface, toplevel)
}

fn window_of(
    session: &common::Session,
    surface: &wl_surface::WlSurface,
) -> smithay::desktop::Window {
    let id = Proxy::id(surface).protocol_id();
    session
        .state
        .space
        .elements()
        .find(|window| {
            window
                .toplevel()
                .is_some_and(|toplevel| toplevel.wl_surface().id().protocol_id() == id)
        })
        .cloned()
        .expect("the toplevel is in the space")
}

fn location(session: &common::Session, surface: &wl_surface::WlSurface) -> Point<i32, Logical> {
    let window = window_of(session, surface);
    session.state.space.element_location(&window).unwrap()
}

fn place(session: &mut common::Session, surface: &wl_surface::WlSurface, x: i32, y: i32) {
    let window = window_of(session, surface);
    session.state.space.map_element(window, (x, y), false);
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

#[test]
fn an_attached_window_follows_the_pointer_and_is_not_the_drop_target() {
    let mut session = common::Session::new(setup);
    let (below, _) = toplevel(&mut session, "below", Some((300, 300)));
    let (dragged, dragged_toplevel) = toplevel(&mut session, "dragged", Some((200, 150)));
    place(&mut session, &below, 500, 300);
    place(&mut session, &dragged, 50, 50);
    let drag = Drag::new(&mut session);

    move_pointer(&mut session, 100.0, 100.0);
    let object = drag.start(&mut session, &dragged, Some((&dragged_toplevel, 30, 20)));
    assert_eq!(
        location(&session, &dragged),
        Point::from((70, 80)),
        "the window jumps to keep the offset under the pointer"
    );

    move_pointer(&mut session, 600.0, 400.0);
    assert_eq!(location(&session, &dragged), Point::from((570, 380)));
    // The dragged window covers the pointer, yet the window below is the
    // target.
    let below_id = Proxy::id(&below).protocol_id();
    assert_eq!(seen().last(), Some(&Seen::DragEnter(below_id)));

    button(&mut session, ButtonState::Released);
    assert_eq!(seen().last(), Some(&Seen::Drop));
    move_pointer(&mut session, 10.0, 10.0);
    assert_eq!(
        location(&session, &dragged),
        Point::from((570, 380)),
        "the window stays where the drop left it"
    );
    // Destroying the object after the drop is allowed: no protocol error
    // (a roundtrip would fail on one).
    object.destroy();
    session.roundtrip();
}

#[test]
fn a_torn_off_window_is_placed_before_its_first_buffer() {
    let mut session = common::Session::new(setup);
    let (below, _) = toplevel(&mut session, "below", Some((300, 300)));
    place(&mut session, &below, 500, 300);
    let drag = Drag::new(&mut session);

    move_pointer(&mut session, 600.0, 400.0);
    let object = drag.start(&mut session, &below, None);

    // The tab leaves the window: a new toplevel, attached before it maps.
    let (torn, torn_toplevel) = toplevel(&mut session, "torn", None);
    object.attach(&torn_toplevel, 10, 10);
    session.roundtrip();
    assert_eq!(location(&session, &torn), Point::from((590, 390)));

    move_pointer(&mut session, 200.0, 200.0);
    assert_eq!(location(&session, &torn), Point::from((190, 190)));
    session.attach_buffer(&torn, 100, 100);
    assert_eq!(location(&session, &torn), Point::from((190, 190)));

    move_pointer(&mut session, 550.0, 350.0);
    assert_eq!(location(&session, &torn), Point::from((540, 340)));
    let below_id = Proxy::id(&below).protocol_id();
    assert_eq!(seen().last(), Some(&Seen::DragEnter(below_id)));

    button(&mut session, ButtonState::Released);
    assert_eq!(seen().last(), Some(&Seen::Drop));
    object.destroy();
    session.roundtrip();
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
    wl_data_source::WlDataSource,
    xdg_toplevel_drag_manager_v1::XdgToplevelDragManagerV1,
    xdg_toplevel_drag_v1::XdgToplevelDragV1
);
