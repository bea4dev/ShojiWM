//! `ManagedWindow::confine_pointer` keeps the pointer inside the focused
//! window, with no pointer constraint from the client.

mod common;

use std::cell::Cell;

use shojiwm_rs::prelude::*;
use smithay::backend::input::{
    AbsolutePositionEvent, Device, DeviceCapability, Event, InputBackend, InputEvent,
    PointerMotionAbsoluteEvent, PointerMotionEvent, UnusedEvent,
};

/// A mouse that moves by `PointerMotion` and jumps by `PointerMotionAbsolute`.
#[derive(Debug)]
struct Mouse;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct MouseDevice;

impl Device for MouseDevice {
    fn id(&self) -> String {
        "test-pointer".into()
    }

    fn name(&self) -> String {
        "test pointer".into()
    }

    fn has_capability(&self, capability: DeviceCapability) -> bool {
        capability == DeviceCapability::Pointer
    }

    fn usb_id(&self) -> Option<(u32, u32)> {
        None
    }

    fn syspath(&self) -> Option<std::path::PathBuf> {
        None
    }
}

struct Motion {
    time: u64,
    dx: f64,
    dy: f64,
}

/// A position as a fraction of the output layout.
struct Absolute {
    time: u64,
    x: f64,
    y: f64,
}

impl Event<Mouse> for Motion {
    fn time(&self) -> u64 {
        self.time
    }

    fn device(&self) -> MouseDevice {
        MouseDevice
    }
}

impl Event<Mouse> for Absolute {
    fn time(&self) -> u64 {
        self.time
    }

    fn device(&self) -> MouseDevice {
        MouseDevice
    }
}

impl PointerMotionEvent<Mouse> for Motion {
    fn delta_x(&self) -> f64 {
        self.dx
    }

    fn delta_y(&self) -> f64 {
        self.dy
    }

    fn delta_x_unaccel(&self) -> f64 {
        self.dx
    }

    fn delta_y_unaccel(&self) -> f64 {
        self.dy
    }
}

impl AbsolutePositionEvent<Mouse> for Absolute {
    fn x(&self) -> f64 {
        self.x
    }

    fn y(&self) -> f64 {
        self.y
    }

    fn x_transformed(&self, width: i32) -> f64 {
        self.x * f64::from(width)
    }

    fn y_transformed(&self, height: i32) -> f64 {
        self.y * f64::from(height)
    }
}

impl PointerMotionAbsoluteEvent<Mouse> for Absolute {}

impl InputBackend for Mouse {
    type Device = MouseDevice;
    type KeyboardKeyEvent = UnusedEvent;
    type PointerAxisEvent = UnusedEvent;
    type PointerButtonEvent = UnusedEvent;
    type PointerMotionEvent = Motion;
    type PointerMotionAbsoluteEvent = Absolute;
    type GestureSwipeBeginEvent = UnusedEvent;
    type GestureSwipeUpdateEvent = UnusedEvent;
    type GestureSwipeEndEvent = UnusedEvent;
    type GesturePinchBeginEvent = UnusedEvent;
    type GesturePinchUpdateEvent = UnusedEvent;
    type GesturePinchEndEvent = UnusedEvent;
    type GestureHoldBeginEvent = UnusedEvent;
    type GestureHoldEndEvent = UnusedEvent;
    type TouchDownEvent = UnusedEvent;
    type TouchUpEvent = UnusedEvent;
    type TouchMotionEvent = UnusedEvent;
    type TouchCancelEvent = UnusedEvent;
    type TouchFrameEvent = UnusedEvent;
    type TabletToolAxisEvent = UnusedEvent;
    type TabletToolProximityEvent = UnusedEvent;
    type TabletToolTipEvent = UnusedEvent;
    type TabletToolButtonEvent = UnusedEvent;
    type SwitchToggleEvent = UnusedEvent;
    type SpecialEvent = ();
}

thread_local! {
    static CONFINE: Cell<bool> = const { Cell::new(true) };
}

fn setup() {
    COMPOSITOR.window.composition(|_| {
        ManagedWindow::new()
            .confine_pointer(CONFINE.with(Cell::get))
            .child(ClientWindow::new())
    });
    // What the test asks of a window from outside a callback is collected
    // on the scheduler's next turn, and a timer keeps it turning.
    set_interval(1.0, || {});
}

/// Puts the pointer at the centre of the only window, focuses that window,
/// then moves the pointer far to the right. Returns the window's right edge
/// and where the pointer ended up.
fn push_pointer_out(session: &mut common::Session) -> (f64, f64) {
    session.map_toplevel_with_buffer("game", 300, 200);
    session.state.refresh_window_decorations().unwrap();
    COMPOSITOR.window.list()[0].focus();
    session.roundtrip();
    session.state.refresh_window_decorations().unwrap();

    let window = session.state.space.elements().next().cloned().unwrap();
    let geometry = session.state.space.element_geometry(&window).unwrap();
    let centre = geometry.loc.to_f64() + geometry.size.to_f64().downscale(2.0).to_point();
    session
        .state
        .process_input_event(InputEvent::<Mouse>::PointerMotionAbsolute {
            event: Absolute {
                time: 0,
                x: centre.x / 1024.0,
                y: centre.y / 768.0,
            },
        });
    session
        .state
        .process_input_event(InputEvent::<Mouse>::PointerMotion {
            event: Motion {
                time: 1,
                dx: 2000.0,
                dy: 0.0,
            },
        });
    let pointer = session.state.seat.get_pointer().unwrap();
    (
        f64::from(geometry.loc.x + geometry.size.w),
        pointer.current_location().x,
    )
}

#[test]
fn the_pointer_stops_at_the_edge_of_a_confining_window() {
    CONFINE.with(|confine| confine.set(true));
    let mut session = common::Session::new(setup);
    let (right, x) = push_pointer_out(&mut session);
    assert!(x < right, "pointer at {x} left a window ending at {right}");
    assert!(x > right - 1.0, "pointer at {x} stopped short of {right}");
}

#[test]
fn without_the_prop_the_pointer_leaves_the_window() {
    CONFINE.with(|confine| confine.set(false));
    let mut session = common::Session::new(setup);
    let (right, x) = push_pointer_out(&mut session);
    assert!(
        x > right,
        "pointer at {x} stayed inside a window ending at {right}"
    );
}
