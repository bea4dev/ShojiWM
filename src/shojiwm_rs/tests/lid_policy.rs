//! The default config's lid policy (`examples/default_config/lid_policy.rs`)
//! takes the lid from logind and runs its action only when the lid moves: not
//! for a lid that is closed when the switch comes up, nor for one a resume
//! finds closed (a stuck switch), until it moves again. Its sleep and resume
//! callbacks run around that.

mod common;

#[path = "../examples/default_config/lid_policy.rs"]
#[allow(dead_code)]
mod lid_policy;

use std::{cell::RefCell, rc::Rc, time::Duration};

use lid_policy::{LidCloseAction, LidPolicyOptions, LidSleepContext, bind_lid_policy};
use shojiwm_rs::power::PowerAction;
use smithay::backend::input::{
    Device, DeviceCapability, Event, InputBackend, InputEvent, Switch, SwitchState,
    SwitchToggleEvent, UnusedEvent,
};

thread_local! {
    static EVENTS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

fn record(event: impl Into<String>) {
    EVENTS.with(|events| events.borrow_mut().push(event.into()));
}

fn setup() {
    bind_lid_policy(LidPolicyOptions {
        // No logind in the tests, so the suspend itself goes nowhere.
        on_battery: LidCloseAction::Perform(PowerAction::Suspend),
        on_sleep: Some(Rc::new(|context: &LidSleepContext| {
            record(format!(
                "sleep {:?} external_display={}",
                context.action, context.external_display
            ));
        })),
        on_resume: Some(Rc::new(|resume| {
            record(format!("resume lid_closed={:?}", resume.lid_closed));
        })),
        holdoff: Duration::ZERO,
        ..Default::default()
    });
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

fn events() -> Vec<String> {
    EVENTS.with(|events| events.borrow().clone())
}

#[test]
fn lid_policy_acts_on_movements_only() {
    let mut session = common::Session::new(setup);
    assert!(session.state.lid_taken_from_logind());

    // Closed when the switch came up: where the lid is, not a movement.
    session
        .state
        .process_input_event(InputEvent::<Switches>::DeviceAdded {
            device: SwitchDevice,
        });
    toggle(&mut session, Switch::Lid, SwitchState::On);
    assert!(events().is_empty());

    toggle(&mut session, Switch::Lid, SwitchState::Off);
    toggle(&mut session, Switch::Lid, SwitchState::On);
    // TEST-1 is not a built-in panel: another display is on.
    assert_eq!(events(), ["sleep Suspend external_display=true"]);

    // A resume that finds the lid closed runs nothing, and neither does the
    // stuck switch saying so again.
    session.state.system_resumed();
    toggle(&mut session, Switch::Lid, SwitchState::On);
    assert_eq!(
        events(),
        [
            "sleep Suspend external_display=true",
            "resume lid_closed=Some(true)"
        ]
    );

    toggle(&mut session, Switch::Lid, SwitchState::Off);
    toggle(&mut session, Switch::Lid, SwitchState::On);
    assert_eq!(
        events(),
        [
            "sleep Suspend external_display=true",
            "resume lid_closed=Some(true)",
            "sleep Suspend external_display=true",
        ]
    );
}
