//! What closing the laptop lid does, set per power source the way Windows and
//! KDE Plasma set it: one action on battery, one on external power.
//!
//! The policy takes the lid from systemd-logind (`COMPOSITOR.power`), so
//! logind no longer suspends on its own, and adds what logind cannot do:
//!
//! - While the lid is closed and another display is on, the built-in panel is
//!   switched off (DPMS) and work goes on there.
//! - Some lid switches stick at "closed". An action runs only when the lid
//!   moves: not for a lid that is closed when the switch comes up (`initial`),
//!   not for one a resume finds closed, and not within `holdoff` of the start
//!   or of a resume (logind's `HoldoffTimeoutSec`).
//!
//! Uses the public API only, so `tests/lid_policy.rs` can load it as is.

use std::{
    cell::RefCell,
    rc::Rc,
    time::{Duration, Instant},
};

use shojiwm_rs::{
    lid_switch::LidSwitchSnapshot, prelude::*, reactive::untrack, ssd::WaylandOutputSnapshot,
};

/// What closing the lid does.
// The default config picks `Logind`; the rest are there to choose from.
#[allow(dead_code)]
#[derive(Clone)]
pub enum LidCloseAction {
    /// What `logind.conf` says in the situation (including its
    /// `HandleLidSwitchDocked` when another display is on).
    Logind,
    Ignore,
    Perform(PowerAction),
    Run(Rc<dyn Fn(&LidCloseContext)>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LidCloseContext {
    /// On external power; `None` when not known.
    pub external_power: Option<bool>,
    /// Another display (not the built-in panel) is on.
    pub external_display: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LidSleepContext {
    /// The sleep the policy is about to ask logind for.
    pub action: PowerAction,
    pub external_power: Option<bool>,
    pub external_display: bool,
}

#[derive(Clone)]
pub struct LidPolicyOptions {
    /// On battery (`HandleLidSwitch` with [`LidCloseAction::Logind`]).
    pub on_battery: LidCloseAction,
    /// On external power (`HandleLidSwitchExternalPower`).
    pub on_external_power: LidCloseAction,
    /// Called right before the policy puts the machine to sleep.
    pub on_sleep: Option<Rc<dyn Fn(&LidSleepContext)>>,
    /// Called after every resume, whoever started the sleep.
    pub on_resume: Option<Rc<dyn Fn(&ResumeSnapshot)>>,
    /// Switch the built-in panel off while the lid is closed and another
    /// display is on.
    pub internal_panel_off: bool,
    /// Closing the lid does nothing this long after the start and a resume.
    pub holdoff: Duration,
}

impl Default for LidPolicyOptions {
    fn default() -> Self {
        Self {
            on_battery: LidCloseAction::Logind,
            on_external_power: LidCloseAction::Logind,
            on_sleep: None,
            on_resume: None,
            internal_panel_off: true,
            holdoff: Duration::from_secs(30),
        }
    }
}

/// Take the lid from logind and do what `options` say when it closes.
pub fn bind_lid_policy(options: LidPolicyOptions) {
    let policy = Rc::new(RefCell::new(LidPolicy {
        holdoff_until: Instant::now() + options.holdoff,
        options,
        lid_closed: false,
        closed_by_movement: false,
        external_display: has_external_display(),
        panels_off: Vec::new(),
    }));

    let lid = Rc::clone(&policy);
    COMPOSITOR
        .event
        .on_lid_switch(move |event| lid.borrow_mut().lid_switched(event));
    let outputs = Rc::clone(&policy);
    COMPOSITOR
        .event
        .on_output_change(move |_| outputs.borrow_mut().outputs_changed());
    let resume = Rc::clone(&policy);
    COMPOSITOR.power.on_resume(move |event| {
        let on_resume = resume.borrow_mut().resumed(event);
        if let Some(on_resume) = on_resume {
            on_resume(event);
        }
    });
    COMPOSITOR
        .power
        .take_over(PowerTakeOverOptions { lid: true });
}

struct LidPolicy {
    options: LidPolicyOptions,
    lid_closed: bool,
    /// Whether the lid closed while we watched, as opposed to being closed
    /// when we started or after a resume: a broken switch can report closed
    /// for a lid that is open.
    closed_by_movement: bool,
    holdoff_until: Instant,
    external_display: bool,
    /// Built-in panels this policy switched off, to switch back on.
    panels_off: Vec<String>,
}

impl LidPolicy {
    fn lid_switched(&mut self, event: &LidSwitchSnapshot) {
        self.lid_closed = event.closed;
        self.closed_by_movement = event.closed && !event.initial;
        self.external_display = has_external_display();
        self.sync_panels();
        if self.closed_by_movement {
            self.run_close_action();
        }
    }

    fn outputs_changed(&mut self) {
        let had_external_display = self.external_display;
        self.external_display = has_external_display();
        self.sync_panels();
        // The last other display went with the lid shut: what closing it now
        // would do, as logind does.
        if had_external_display && !self.external_display && self.closed_by_movement {
            self.run_close_action();
        }
    }

    /// Returns the config's resume callback, called once the policy is no
    /// longer borrowed.
    fn resumed(&mut self, event: &ResumeSnapshot) -> Option<Rc<dyn Fn(&ResumeSnapshot)>> {
        self.holdoff_until = Instant::now() + self.options.holdoff;
        // A switch that still says closed after a resume may be stuck; it runs
        // nothing until it moves again.
        self.closed_by_movement = false;
        if let Some(closed) = event.lid_closed {
            self.lid_closed = closed;
        }
        self.external_display = has_external_display();
        self.sync_panels();
        self.options.on_resume.clone()
    }

    /// The panel follows the lid even when its state is only initial: a
    /// laptop booted shut on a dock keeps its panel dark.
    fn sync_panels(&mut self) {
        let off = self.options.internal_panel_off && self.lid_closed && self.external_display;
        for output in outputs() {
            if !is_internal(&output) {
                continue;
            }
            let switched_off = self.panels_off.contains(&output.name);
            if off && !switched_off {
                self.panels_off.push(output.name.clone());
                set_panel(&output.name, OutputPower::Off);
            } else if !off && switched_off {
                self.panels_off.retain(|name| *name != output.name);
                set_panel(&output.name, OutputPower::On);
            }
        }
    }

    fn run_close_action(&self) {
        if Instant::now() < self.holdoff_until {
            return;
        }
        let state = COMPOSITOR.power.state().get_untracked();
        let external_power = state.as_ref().and_then(|state| state.external_power);
        let context = LidCloseContext {
            external_power,
            external_display: self.external_display,
        };
        let action = if external_power == Some(true) {
            &self.options.on_external_power
        } else {
            &self.options.on_battery
        };
        let action = match action {
            LidCloseAction::Logind => {
                let Some(logind) = state.and_then(|state| state.logind_lid_actions) else {
                    return;
                };
                let name = if self.external_display {
                    logind.docked
                } else if external_power == Some(true) {
                    logind.external_power
                } else {
                    logind.lid
                };
                // `ignore`, and actions run on nobody's behalf (`kexec`).
                match PowerAction::parse(&name) {
                    Some(action) => action,
                    None => return,
                }
            }
            LidCloseAction::Ignore => return,
            LidCloseAction::Perform(action) => *action,
            LidCloseAction::Run(run) => return run(&context),
        };
        if is_sleep(action)
            && let Some(on_sleep) = &self.options.on_sleep
        {
            on_sleep(&LidSleepContext {
                action,
                external_power,
                external_display: self.external_display,
            });
        }
        COMPOSITOR.power.perform(action);
    }
}

fn is_sleep(action: PowerAction) -> bool {
    matches!(
        action,
        PowerAction::Suspend
            | PowerAction::Hibernate
            | PowerAction::HybridSleep
            | PowerAction::SuspendThenHibernate
            | PowerAction::Sleep
    )
}

fn outputs() -> Vec<WaylandOutputSnapshot> {
    untrack(|| COMPOSITOR.output.state().get())
        .into_values()
        .collect()
}

/// Built-in panels, by connector name.
fn is_internal(output: &WaylandOutputSnapshot) -> bool {
    ["eDP", "LVDS", "DSI"]
        .iter()
        .any(|prefix| output.name.starts_with(prefix))
}

fn is_headless(output: &WaylandOutputSnapshot) -> bool {
    output.make.as_deref() == Some("ShojiWM") && output.model.as_deref() == Some("Headless")
}

fn has_external_display() -> bool {
    outputs()
        .iter()
        .any(|output| output.enabled && !is_internal(output) && !is_headless(output))
}

fn set_panel(name: &str, power: OutputPower) {
    COMPOSITOR.output.set_power(
        power,
        OutputPowerOptions {
            output: Some(name.to_string()),
            wake_on_input: false,
        },
    );
}
