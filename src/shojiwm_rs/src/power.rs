//! `COMPOSITOR.power`: suspend and the like through systemd-logind, and the
//! lid switch taken from it. Only the TTY backend talks to logind; in a nested
//! session these do nothing.

use std::rc::Rc;

use shojiwm_lib::{power::RuntimePowerRequest, runtime_api::HostMessage};

use crate::{
    reactive::ReadSignal,
    runtime::{self, with_registry},
};

pub use shojiwm_lib::power::{LogindLidActions, PowerAction, PowerStateSnapshot, ResumeSnapshot};

/// `COMPOSITOR.power`.
pub struct PowerController;

/// Options of [`PowerController::take_over`].
#[derive(Debug, Clone, Copy, Default)]
pub struct PowerTakeOverOptions {
    /// Handle the lid switch instead of logind, which then leaves it alone:
    /// the config decides, through `on_lid_switch`, and suspends with
    /// [`PowerController::suspend`].
    pub lid: bool,
}

impl PowerController {
    /// What the config handles instead of logind.
    pub fn take_over(&self, options: PowerTakeOverOptions) {
        runtime::send(HostMessage::Power(RuntimePowerRequest::TakeOver {
            lid: options.lid,
        }));
    }

    /// Ask logind to suspend.
    pub fn suspend(&self) {
        self.perform(PowerAction::Suspend);
    }

    /// Ask logind to hibernate.
    pub fn hibernate(&self) {
        self.perform(PowerAction::Hibernate);
    }

    /// Ask logind for `action`.
    pub fn perform(&self, action: PowerAction) {
        runtime::send(HostMessage::Power(RuntimePowerRequest::Perform(action)));
    }

    /// External power and logind's lid settings (tracked); `None` before the
    /// first report.
    pub fn state(&self) -> ReadSignal<Option<PowerStateSnapshot>> {
        runtime::global().power_state.read_only()
    }

    /// The system is back from a suspend or hibernation, whoever started it.
    /// Whether this or the lid opening comes first is not defined.
    pub fn on_resume(&self, listener: impl Fn(&ResumeSnapshot) + 'static) {
        with_registry(|registry| registry.listeners.resume.push(Rc::new(listener)));
    }
}
