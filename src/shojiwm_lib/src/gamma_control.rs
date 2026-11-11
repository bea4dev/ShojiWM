//! Gamma ramps from wlr-gamma-control clients (night light), applied to the
//! output's CRTC by the TTY backend
//! ([`crate::backend::tty::set_output_gamma`]). The nested backend has no
//! gamma to set, so its outputs refuse controls.
//!
//! The ramps are kept per output so that they come back after a VT switch:
//! another DRM master may have changed or cleared the LUT meanwhile.

use smithay::output::Output;

use crate::{
    protocols::gamma_control::{GammaControlHandler, GammaControlManagerState},
    state::ShojiWM,
};

impl ShojiWM {
    /// Load every output's client gamma again, after the session resumed.
    pub(crate) fn reapply_gamma(&mut self) {
        let outputs: Vec<Output> = self
            .space
            .outputs()
            .filter(|output| self.gamma_ramps.contains_key(&output.name()))
            .cloned()
            .collect();
        for output in outputs {
            let ramps = self.gamma_ramps.get(&output.name()).cloned();
            if !crate::backend::tty::set_output_gamma(self, &output, ramps.as_deref()) {
                self.gamma_ramps.remove(&output.name());
                self.gamma_control_manager_state.output_removed(&output);
            }
        }
    }

    pub(crate) fn forget_output_gamma(&mut self, output: &Output) {
        self.gamma_ramps.remove(&output.name());
        self.gamma_control_manager_state.output_removed(output);
    }
}

impl GammaControlHandler for ShojiWM {
    fn gamma_control_manager_state(&mut self) -> &mut GammaControlManagerState {
        &mut self.gamma_control_manager_state
    }

    fn gamma_size(&self, output: &Output) -> Option<u32> {
        crate::backend::tty::output_gamma_size(self, output)
    }

    fn set_gamma(&mut self, output: &Output, ramps: Option<Vec<u16>>) -> bool {
        // While the session is paused there is no DRM access; resuming
        // applies what is kept here.
        let applied = !self.tty_session_active
            || crate::backend::tty::set_output_gamma(self, output, ramps.as_deref());
        match ramps {
            Some(ramps) if applied => {
                self.gamma_ramps.insert(output.name(), ramps);
            }
            _ => {
                self.gamma_ramps.remove(&output.name());
            }
        }
        applied
    }
}
