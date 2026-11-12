//! A laptop lid's switch reaches the config as an event and as the state
//! `COMPOSITOR.input.lid_switch()` holds.

mod common;

use std::cell::RefCell;

use shojiwm_lib::lid_switch::LidSwitchSnapshot;
use shojiwm_rs::prelude::*;
use smithay::backend::input::{
    Device, DeviceCapability, Event, InputBackend, InputEvent, Switch, SwitchState,
    SwitchToggleEvent, UnusedEvent,
};

thread_local! {
    static LIDS: RefCell<Vec<LidSwitchSnapshot>> = const { RefCell::new(Vec::new()) };
}

fn setup() {
    COMPOSITOR
        .event
        .on_lid_switch(|lid| LIDS.with(|lids| lids.borrow_mut().push(*lid)));
}

#[derive(Debug)]
struct Switches;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct SwitchDevice;

impl Device for SwitchDevice {
    fn id(&self) -> String {
        "lid-switch".into()
    }

    fn name(&self) -> String {
        "Lid Switch".into()
    }

    fn has_capability(&self, capability: DeviceCapability) -> bool {
        capability == DeviceCapability::Switch
    }

    fn usb_id(&self) -> Option<(u32, u32)> {
        None
    }

    fn syspath(&self) -> Option<std::path::PathBuf> {
        None
    }
}

struct Toggle {
    switch: Switch,
    state: SwitchState,
}

impl Event<Switches> for Toggle {
    fn time(&self) -> u64 {
        0
    }

    fn device(&self) -> SwitchDevice {
        SwitchDevice
    }
}

impl SwitchToggleEvent<Switches> for Toggle {
    fn switch(&self) -> Option<Switch> {
        Some(self.switch)
    }

    fn state(&self) -> SwitchState {
        self.state
    }
}

impl InputBackend for Switches {
    type Device = SwitchDevice;
    type KeyboardKeyEvent = UnusedEvent;
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
    type SwitchToggleEvent = Toggle;
    type SpecialEvent = ();
}

fn toggle(session: &mut common::Session, switch: Switch, state: SwitchState) {
    session
        .state
        .process_input_event(InputEvent::<Switches>::SwitchToggle {
            event: Toggle { switch, state },
        });
}

fn lids() -> Vec<LidSwitchSnapshot> {
    LIDS.with(|lids| lids.borrow().clone())
}

fn moved(closed: bool) -> LidSwitchSnapshot {
    LidSwitchSnapshot {
        closed,
        initial: false,
    }
}

#[test]
fn lid_switch_reaches_the_config() {
    let mut session = common::Session::new(setup);
    assert_eq!(COMPOSITOR.input.lid_switch().get_untracked(), None);

    toggle(&mut session, Switch::Lid, SwitchState::On);
    assert_eq!(lids(), [moved(true)]);
    assert_eq!(
        COMPOSITOR.input.lid_switch().get_untracked(),
        Some(moved(true))
    );

    // Libinput repeats a switch's state; the config hears changes only.
    toggle(&mut session, Switch::Lid, SwitchState::On);
    // Tablet mode is a different switch.
    toggle(&mut session, Switch::TabletMode, SwitchState::Off);
    toggle(&mut session, Switch::Lid, SwitchState::Off);
    assert_eq!(lids(), [moved(true), moved(false)]);
    assert_eq!(
        COMPOSITOR.input.lid_switch().get_untracked(),
        Some(moved(false))
    );

    // A switch that comes up closed (at startup, or back from another VT)
    // reports where it is, which is not a movement.
    session
        .state
        .process_input_event(InputEvent::<Switches>::DeviceAdded {
            device: SwitchDevice,
        });
    toggle(&mut session, Switch::Lid, SwitchState::On);
    assert_eq!(
        lids().last(),
        Some(&LidSwitchSnapshot {
            closed: true,
            initial: true,
        })
    );
}
