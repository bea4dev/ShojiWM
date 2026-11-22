//! A keyboard for driving the compositor's input path from a test.

use smithay::{
    backend::input::{
        Device, DeviceCapability, Event, InputBackend, KeyState, KeyboardKeyEvent, UnusedEvent,
    },
    input::keyboard::Keycode,
};

#[derive(Debug)]
pub struct Keyboard;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct KeyboardDevice;

impl Device for KeyboardDevice {
    fn id(&self) -> String {
        "test-keyboard".into()
    }

    fn name(&self) -> String {
        "test keyboard".into()
    }

    fn has_capability(&self, capability: DeviceCapability) -> bool {
        capability == DeviceCapability::Keyboard
    }

    fn usb_id(&self) -> Option<(u32, u32)> {
        None
    }

    fn syspath(&self) -> Option<std::path::PathBuf> {
        None
    }
}

/// A press or release of an xkb keycode (evdev code plus 8).
pub struct Key {
    pub time: u64,
    pub keycode: u32,
    pub state: KeyState,
}

impl Event<Keyboard> for Key {
    fn time(&self) -> u64 {
        self.time
    }

    fn device(&self) -> KeyboardDevice {
        KeyboardDevice
    }
}

impl KeyboardKeyEvent<Keyboard> for Key {
    fn key_code(&self) -> Keycode {
        Keycode::new(self.keycode)
    }

    fn state(&self) -> KeyState {
        self.state
    }

    fn count(&self) -> u32 {
        u32::from(self.state == KeyState::Pressed)
    }
}

impl InputBackend for Keyboard {
    type Device = KeyboardDevice;
    type KeyboardKeyEvent = Key;
    type PointerAxisEvent = UnusedEvent;
    type PointerButtonEvent = UnusedEvent;
    type PointerMotionEvent = UnusedEvent;
    type PointerMotionAbsoluteEvent = UnusedEvent;
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
