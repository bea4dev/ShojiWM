//! `zwlr_gamma_control_manager_v1` (wlr-gamma-control-unstable-v1).
//!
//! Lets night light tools (`wlsunset`, `gammastep`) set an output's gamma
//! ramps. The compositor applies them ([`GammaControlHandler`]); this module
//! keeps the controls and reads the client's table.
//!
//! As in wlroots, a control is exclusive per output: `get_gamma_control` for
//! an output that already has one, or whose gamma cannot be set, gets
//! `failed`. Destroying the control puts the output's default ramps back.

use std::{fs::File, os::unix::fs::FileExt};

use smithay::output::Output;
use smithay::reexports::wayland_protocols_wlr::gamma_control::v1::server::{
    zwlr_gamma_control_manager_v1::{self, ZwlrGammaControlManagerV1},
    zwlr_gamma_control_v1::{self, ZwlrGammaControlV1},
};
use smithay::reexports::wayland_server::backend::ClientId;
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
};
use tracing::warn;

const VERSION: u32 = 1;

pub trait GammaControlHandler {
    fn gamma_control_manager_state(&mut self) -> &mut GammaControlManagerState;
    /// Entries per channel of `output`'s gamma ramps, or `None` when its
    /// gamma cannot be set.
    fn gamma_size(&self, output: &Output) -> Option<u32>;
    /// Apply `ramps` (red, green and blue, `gamma_size` entries each, one
    /// after the other) to `output`, or its default ramps for `None`.
    /// `false` when that failed.
    fn set_gamma(&mut self, output: &Output, ramps: Option<Vec<u16>>) -> bool;
}

#[derive(Debug, Default)]
pub struct GammaControlManagerState {
    controls: Vec<(ZwlrGammaControlV1, Output)>,
}

/// The output a control manages and its ramp size; `None` once it failed and
/// became inert.
#[derive(Debug)]
pub struct GammaControlData {
    target: Option<(Output, u32)>,
}

impl GammaControlManagerState {
    pub fn new<D>(display: &DisplayHandle) -> Self
    where
        D: GlobalDispatch<ZwlrGammaControlManagerV1, ()>,
        D: Dispatch<ZwlrGammaControlManagerV1, ()>,
        D: Dispatch<ZwlrGammaControlV1, GammaControlData>,
        D: 'static,
    {
        display.create_global::<D, ZwlrGammaControlManagerV1, _>(VERSION, ());
        Self::default()
    }

    /// `output` is gone or its gamma can no longer be set: fail its control.
    pub fn output_removed(&mut self, output: &Output) {
        self.controls.retain(|(control, owner)| {
            if owner != output {
                return true;
            }
            control.failed();
            false
        });
    }

    /// Whether a live control manages `control`'s output.
    fn is_live(&self, control: &ZwlrGammaControlV1) -> bool {
        self.controls
            .iter()
            .any(|(existing, _)| existing == control)
    }

    fn remove(&mut self, control: &ZwlrGammaControlV1) -> bool {
        let before = self.controls.len();
        self.controls.retain(|(existing, _)| existing != control);
        self.controls.len() != before
    }
}

impl<D> GlobalDispatch<ZwlrGammaControlManagerV1, (), D> for GammaControlManagerState
where
    D: GlobalDispatch<ZwlrGammaControlManagerV1, ()>,
    D: Dispatch<ZwlrGammaControlManagerV1, ()>,
    D: Dispatch<ZwlrGammaControlV1, GammaControlData>,
    D: GammaControlHandler,
    D: 'static,
{
    fn bind(
        _state: &mut D,
        _dh: &DisplayHandle,
        _client: &Client,
        manager: New<ZwlrGammaControlManagerV1>,
        _data: &(),
        data_init: &mut DataInit<'_, D>,
    ) {
        data_init.init(manager, ());
    }
}

impl<D> Dispatch<ZwlrGammaControlManagerV1, (), D> for GammaControlManagerState
where
    D: Dispatch<ZwlrGammaControlManagerV1, ()>,
    D: Dispatch<ZwlrGammaControlV1, GammaControlData>,
    D: GammaControlHandler,
    D: 'static,
{
    fn request(
        state: &mut D,
        _client: &Client,
        _manager: &ZwlrGammaControlManagerV1,
        request: zwlr_gamma_control_manager_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            zwlr_gamma_control_manager_v1::Request::GetGammaControl { id, output } => {
                let output = Output::from_resource(&output);
                let size = output.as_ref().and_then(|output| state.gamma_size(output));
                let taken = output.as_ref().is_some_and(|output| {
                    state
                        .gamma_control_manager_state()
                        .controls
                        .iter()
                        .any(|(_, owner)| owner == output)
                });
                match (output, size) {
                    (Some(output), Some(size)) if !taken && size > 0 => {
                        let control = data_init.init(
                            id,
                            GammaControlData {
                                target: Some((output.clone(), size)),
                            },
                        );
                        control.gamma_size(size);
                        state
                            .gamma_control_manager_state()
                            .controls
                            .push((control, output));
                    }
                    _ => {
                        let control = data_init.init(id, GammaControlData { target: None });
                        control.failed();
                    }
                }
            }
            zwlr_gamma_control_manager_v1::Request::Destroy => {}
            _ => {}
        }
    }
}

impl<D> Dispatch<ZwlrGammaControlV1, GammaControlData, D> for GammaControlManagerState
where
    D: Dispatch<ZwlrGammaControlV1, GammaControlData>,
    D: GammaControlHandler,
    D: 'static,
{
    fn request(
        state: &mut D,
        _client: &Client,
        control: &ZwlrGammaControlV1,
        request: zwlr_gamma_control_v1::Request,
        data: &GammaControlData,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            zwlr_gamma_control_v1::Request::SetGamma { fd } => {
                // A failed control is inert until the client destroys it.
                let Some((output, size)) = data.target.as_ref() else {
                    return;
                };
                if !state.gamma_control_manager_state().is_live(control) {
                    return;
                }
                let Some(ramps) = read_ramps(File::from(fd), *size) else {
                    control.post_error(
                        zwlr_gamma_control_v1::Error::InvalidGamma,
                        "the gamma table is not 3 * gamma_size 16-bit entries",
                    );
                    return;
                };
                if !state.set_gamma(output, Some(ramps)) {
                    state.gamma_control_manager_state().remove(control);
                    control.failed();
                }
            }
            zwlr_gamma_control_v1::Request::Destroy => {}
            _ => {}
        }
    }

    fn destroyed(
        state: &mut D,
        _client: ClientId,
        control: &ZwlrGammaControlV1,
        data: &GammaControlData,
    ) {
        if state.gamma_control_manager_state().remove(control)
            && let Some((output, _)) = data.target.as_ref()
        {
            state.set_gamma(output, None);
        }
    }
}

/// Reads the red, green and blue ramps, `size` native-endian `u16` each.
/// The client may hand any file; it is read from its start, like wlroots does.
fn read_ramps(file: File, size: u32) -> Option<Vec<u16>> {
    let entries = size as usize * 3;
    let mut bytes = vec![0u8; entries * 2];
    if let Err(error) = file.read_exact_at(&mut bytes, 0) {
        warn!(?error, "could not read a gamma table");
        return None;
    }
    Some(
        bytes
            .chunks_exact(2)
            .map(|pair| u16::from_ne_bytes([pair[0], pair[1]]))
            .collect(),
    )
}

/// Delegate the `zwlr_gamma_control_manager_v1` global to
/// [`GammaControlManagerState`].
#[macro_export]
macro_rules! delegate_gamma_control {
    ($ty: ty) => {
        smithay::reexports::wayland_server::delegate_global_dispatch!($ty: [
            smithay::reexports::wayland_protocols_wlr::gamma_control::v1::server::zwlr_gamma_control_manager_v1::ZwlrGammaControlManagerV1: ()
        ] => $crate::protocols::gamma_control::GammaControlManagerState);

        smithay::reexports::wayland_server::delegate_dispatch!($ty: [
            smithay::reexports::wayland_protocols_wlr::gamma_control::v1::server::zwlr_gamma_control_manager_v1::ZwlrGammaControlManagerV1: ()
        ] => $crate::protocols::gamma_control::GammaControlManagerState);

        smithay::reexports::wayland_server::delegate_dispatch!($ty: [
            smithay::reexports::wayland_protocols_wlr::gamma_control::v1::server::zwlr_gamma_control_v1::ZwlrGammaControlV1: $crate::protocols::gamma_control::GammaControlData
        ] => $crate::protocols::gamma_control::GammaControlManagerState);
    };
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::read_ramps;

    #[test]
    fn ramps_are_read_from_the_start_of_the_file() {
        let path = std::env::temp_dir().join(format!("shoji-gamma-{}", std::process::id()));
        let mut file = std::fs::File::create(&path).unwrap();
        let table: Vec<u16> = (0..6).map(|value| value * 1000).collect();
        for value in &table {
            file.write_all(&value.to_ne_bytes()).unwrap();
        }
        let file = std::fs::File::open(&path).unwrap();
        assert_eq!(read_ramps(file, 2), Some(table));
        let file = std::fs::File::open(&path).unwrap();
        assert_eq!(read_ramps(file, 3), None, "a short table is refused");
        std::fs::remove_file(path).unwrap();
    }
}
