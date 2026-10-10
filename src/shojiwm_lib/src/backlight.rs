//! Panel backlights, read so that an HDR10 output's SDR white can follow the
//! brightness the user sets.
//!
//! In HDR10 the signal states absolute luminance, so the panel stops applying
//! its backlight setting (confirmed on an AMD eDP OLED): brightness keys and
//! `brightnessctl` do nothing. The compositor applies the setting instead, as
//! the luminance it shows SDR white at. Nothing here writes the backlight; it
//! keeps the user's value and takes over again when the output leaves HDR.

use std::path::{Path, PathBuf};

use smithay::reexports::calloop::{
    Interest, LoopHandle, Mode, PostAction,
    generic::Generic,
};
use tracing::{debug, warn};

use crate::state::ShojiWM;

const BACKLIGHT_CLASS: &str = "/sys/class/backlight";

/// A backlight device in `/sys/class/backlight`.
#[derive(Debug, Clone)]
pub struct Backlight {
    path: PathBuf,
}

impl Backlight {
    /// The current level as a fraction of the device's maximum.
    ///
    /// Reads `actual_brightness` — what the driver says the panel is at — over
    /// the requested `brightness`. They differ where the driver maps the request
    /// through a curve: amdgpu's nits-based eDP backlight (an OLED here) put a
    /// request of 155610 (of 399000, in millinits) at an actual 77671, and SDR
    /// white matched SDR mode by eye at 78 cd/m², not 156. For most drivers the
    /// two are equal.
    pub fn fraction(&self) -> Option<f32> {
        let read = |name: &str| -> Option<f32> {
            std::fs::read_to_string(self.path.join(name)).ok()?.trim().parse().ok()
        };
        let max = read("max_brightness").filter(|max| *max > 0.0)?;
        let level = read("actual_brightness").or_else(|| read("brightness"))?;
        Some((level / max).clamp(0.0, 1.0))
    }
}

/// The backlight of the panel on connector `output_name` (`eDP-1`, ...).
///
/// A backlight registered by the display driver sits under its connector
/// (`.../drm/card1/card1-eDP-1/amdgpu_bl1`), which names the output exactly.
/// Otherwise (`acpi_video0`, a vendor's platform device) an internal panel
/// takes the best remaining one by type, as systemd-backlight ranks them.
pub fn for_output(output_name: &str) -> Option<Backlight> {
    let mut by_type: Option<(u8, PathBuf)> = None;
    for entry in std::fs::read_dir(BACKLIGHT_CLASS).ok()?.flatten() {
        let path = entry.path();
        match connector_of(&path) {
            Some(connector) if connector == output_name => {
                return Some(Backlight { path });
            }
            // Bound to another connector.
            Some(_) => continue,
            None => {}
        }
        if !output_name.starts_with("eDP") && !output_name.starts_with("LVDS") {
            continue;
        }
        let rank = match std::fs::read_to_string(path.join("type")).ok()?.trim() {
            "firmware" => 0,
            "platform" => 1,
            _ => 2,
        };
        if by_type.as_ref().is_none_or(|(best, _)| rank < *best) {
            by_type = Some((rank, path));
        }
    }
    by_type.map(|(_, path)| Backlight { path })
}

/// `eDP-1` for a backlight whose parent is the DRM connector `card1-eDP-1`.
fn connector_of(backlight: &Path) -> Option<String> {
    let device = std::fs::canonicalize(backlight.join("device")).ok()?;
    let name = device.file_name()?.to_str()?;
    let rest = name.strip_prefix("card")?;
    let (index, connector) = rest.split_once('-')?;
    index.chars().all(|c| c.is_ascii_digit()).then(|| connector.to_owned())
}

/// Watch for backlight changes. The kernel sends a `change` uevent for every
/// write to `brightness` (`SOURCE=sysfs`), whoever makes it — brightness keys
/// through logind, `brightnessctl`, a power daemon.
pub fn start_monitor(loop_handle: &LoopHandle<'static, ShojiWM>) {
    let socket = match smithay::reexports::udev::MonitorBuilder::new()
        .and_then(|builder| builder.match_subsystem("backlight"))
        .and_then(|builder| builder.listen())
    {
        Ok(socket) => socket,
        Err(error) => {
            warn!(%error, "failed to watch backlights; HDR SDR white will not follow brightness");
            return;
        }
    };
    let source = Generic::new(socket, Interest::READ, Mode::Level);
    let inserted = loop_handle.insert_source(source, |_, socket, state| {
        let mut changed = false;
        for event in socket.iter() {
            changed |= event.event_type() == smithay::reexports::udev::EventType::Change;
        }
        if changed {
            debug!("backlight changed");
            state.backlight_changed();
        }
        Ok(PostAction::Continue)
    });
    if let Err(error) = inserted {
        warn!(%error, "failed to register the backlight monitor");
    }
}
