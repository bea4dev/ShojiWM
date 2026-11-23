//! A cursor capture session (ext-image-copy-capture-v1) tells its client
//! where the pointer is on the captured output: `enter` and `position` when
//! the pointer comes onto it, `leave` when it goes off.

mod common;

use std::cell::RefCell;

use smithay::{input::pointer::MotionEvent, utils::SERIAL_COUNTER};
use wayland_client::{
    Connection, Dispatch, QueueHandle,
    protocol::{wl_output, wl_pointer, wl_seat},
};
use wayland_protocols::ext::{
    image_capture_source::v1::client::{
        ext_image_capture_source_v1, ext_output_image_capture_source_manager_v1,
    },
    image_copy_capture::v1::client::{
        ext_image_copy_capture_cursor_session_v1, ext_image_copy_capture_manager_v1,
    },
};

thread_local! {
    static EVENTS: RefCell<Vec<Seen>> = const { RefCell::new(Vec::new()) };
}

#[derive(Debug, Clone, PartialEq)]
enum Seen {
    Enter,
    Leave,
    Position(i32, i32),
}

fn seen() -> Vec<Seen> {
    EVENTS.with(|events| events.borrow().clone())
}

fn clear() {
    EVENTS.with(|events| events.borrow_mut().clear());
}

fn setup() {}

/// What the input path does on motion: move the pointer, then tell the
/// cursor sessions.
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
    state.update_cursor_capture_sessions();
    session.roundtrip();
}

#[test]
fn cursor_session_follows_the_pointer_on_its_output() {
    let mut session = common::Session::new(setup);
    let qh = session.queue.handle();
    let seat: wl_seat::WlSeat = session.bind("wl_seat", 1);
    let pointer = seat.get_pointer(&qh, ());
    let output: wl_output::WlOutput = session.bind("wl_output", 1);
    let sources: ext_output_image_capture_source_manager_v1::ExtOutputImageCaptureSourceManagerV1 =
        session.bind("ext_output_image_capture_source_manager_v1", 1);
    let captures: ext_image_copy_capture_manager_v1::ExtImageCopyCaptureManagerV1 =
        session.bind("ext_image_copy_capture_manager_v1", 1);
    let source = sources.create_source(&output, &qh, ());
    move_pointer(&mut session, 2000.0, 100.0);
    captures.create_pointer_cursor_session(&source, &pointer, &qh, ());
    session.roundtrip();
    // The pointer is off the only output: the session says nothing yet.
    assert_eq!(seen(), vec![]);

    // The test output is 1024x768 at scale 1: buffer pixels are logical ones.
    move_pointer(&mut session, 100.5, 200.25);
    assert_eq!(seen(), vec![Seen::Enter, Seen::Position(100, 200)]);

    clear();
    move_pointer(&mut session, 300.0, 400.0);
    assert_eq!(seen(), vec![Seen::Position(300, 400)]);

    clear();
    move_pointer(&mut session, 2000.0, 100.0);
    assert_eq!(seen(), vec![Seen::Leave]);
}

impl Dispatch<ext_image_copy_capture_cursor_session_v1::ExtImageCopyCaptureCursorSessionV1, ()>
    for common::Client
{
    fn event(
        _: &mut Self,
        _: &ext_image_copy_capture_cursor_session_v1::ExtImageCopyCaptureCursorSessionV1,
        event: ext_image_copy_capture_cursor_session_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let seen = match event {
            ext_image_copy_capture_cursor_session_v1::Event::Enter => Seen::Enter,
            ext_image_copy_capture_cursor_session_v1::Event::Leave => Seen::Leave,
            ext_image_copy_capture_cursor_session_v1::Event::Position { x, y } => {
                Seen::Position(x, y)
            }
            _ => return,
        };
        EVENTS.with(|events| events.borrow_mut().push(seen));
    }
}

macro_rules! ignore_events {
    ($($proxy:ty),*) => {$(
        impl Dispatch<$proxy, ()> for common::Client {
            fn event(_: &mut Self, _: &$proxy, _: <$proxy as wayland_client::Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
        }
    )*};
}

ignore_events!(
    wl_pointer::WlPointer,
    ext_image_capture_source_v1::ExtImageCaptureSourceV1,
    ext_output_image_capture_source_manager_v1::ExtOutputImageCaptureSourceManagerV1,
    ext_image_copy_capture_manager_v1::ExtImageCopyCaptureManagerV1
);
