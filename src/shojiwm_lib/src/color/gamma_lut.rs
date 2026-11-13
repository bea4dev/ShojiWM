//! A CRTC's `GAMMA_LUT`, for wlr-gamma-control.
//!
//! Written with the legacy SET_PROPERTY ioctl like the properties in
//! [`super::drm_metadata`]: the kernel folds it into the CRTC's atomic state
//! and smithay's commits never touch it, so it stays applied under every
//! frame until it is replaced or cleared.

use std::io;

use smithay::reexports::drm::control::{Device as ControlDevice, crtc, property};

use super::drm_metadata::find_object_property;

/// `struct drm_color_lut` from the kernel's uapi.
#[repr(C)]
#[derive(Clone, Copy)]
struct DrmColorLut {
    red: u16,
    green: u16,
    blue: u16,
    reserved: u16,
}

/// Entries per channel the CRTC's `GAMMA_LUT` takes, or `None` when the
/// driver has no gamma LUT.
pub fn crtc_gamma_size(device: &impl ControlDevice, crtc: crtc::Handle) -> Option<u32> {
    find_object_property(device, crtc, "GAMMA_LUT")?;
    let (_, size) = find_object_property(device, crtc, "GAMMA_LUT_SIZE")?;
    u32::try_from(size).ok().filter(|size| *size > 0)
}

/// Load `ramps` (red, then green, then blue, `crtc_gamma_size` entries each)
/// into the CRTC's `GAMMA_LUT`, or clear it back to the bypass for `None`.
pub fn set_crtc_gamma(
    device: &impl ControlDevice,
    crtc: crtc::Handle,
    ramps: Option<&[u16]>,
) -> io::Result<()> {
    let Some((info, _)) = find_object_property(device, crtc, "GAMMA_LUT") else {
        return Err(io::Error::other("the CRTC has no GAMMA_LUT"));
    };
    let blob = match ramps {
        Some(ramps) => {
            let size = ramps.len() / 3;
            if size == 0 || ramps.len() != size * 3 {
                return Err(io::Error::other("gamma ramps are not three equal channels"));
            }
            let lut: Vec<DrmColorLut> = (0..size)
                .map(|index| DrmColorLut {
                    red: ramps[index],
                    green: ramps[size + index],
                    blue: ramps[2 * size + index],
                    reserved: 0,
                })
                .collect();
            let property::Value::Blob(blob) = device.create_property_blob(lut.as_slice())? else {
                return Err(io::Error::other("create_property_blob returned non-blob"));
            };
            blob
        }
        None => 0,
    };
    if let Err(error) = device.set_property(crtc, info.handle(), blob) {
        if blob != 0 {
            let _ = device.destroy_property_blob(blob);
        }
        return Err(error);
    }
    // The CRTC state holds its own reference to the blob it uses, so this
    // handle is not needed past the commit.
    if blob != 0 {
        let _ = device.destroy_property_blob(blob);
    }
    Ok(())
}
