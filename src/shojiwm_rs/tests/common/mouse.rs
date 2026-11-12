//! A mouse for driving the compositor's input path from a test.

use smithay::backend::input::{
    AbsolutePositionEvent, ButtonState, Device, DeviceCapability, Event, InputBackend,
    PointerButtonEvent, PointerMotionAbsoluteEvent, PointerMotionEvent, UnusedEvent,
};

/// A mouse that moves by `PointerMotion` and jumps by `PointerMotionAbsolute`.
#[derive(Debug)]
pub struct Mouse;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MouseDevice;

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

pub struct Motion {
    pub time: u64,
    pub dx: f64,
    pub dy: f64,
}

/// A position as a fraction of the output layout.
pub struct Absolute {
    pub time: u64,
    pub x: f64,
    pub y: f64,
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

/// A press or release of an evdev button code (`BTN_LEFT` is 0x110).
pub struct Button {
    pub time: u64,
    pub code: u32,
    pub pressed: bool,
}

impl Event<Mouse> for Button {
    fn time(&self) -> u64 {
        self.time
    }

    fn device(&self) -> MouseDevice {
        MouseDevice
    }
}

impl PointerButtonEvent<Mouse> for Button {
    fn button_code(&self) -> u32 {
        self.code
    }

    fn state(&self) -> ButtonState {
        if self.pressed {
            ButtonState::Pressed
        } else {
            ButtonState::Released
        }
    }
}

impl InputBackend for Mouse {
    type Device = MouseDevice;
    type KeyboardKeyEvent = UnusedEvent;
    type PointerAxisEvent = UnusedEvent;
    type PointerButtonEvent = Button;
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
