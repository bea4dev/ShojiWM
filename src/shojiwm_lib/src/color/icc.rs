//! Monitor ICC profiles (`icc: "/path/to/profile.icc"` in the display config).
//!
//! The composited frame is sRGB-relative, gamma-encoded RGB. On an output with
//! a profile the encode pass (`backend::hdr_pipeline`) runs it through a 3D LUT
//! that maps those values to the device RGB the profile asks for, built here
//! with Little CMS once per profile.
//!
//! The source side is sRGB primaries with a pure 2.2 power law, not the
//! piecewise sRGB curve: what an uncalibrated SDR panel does with the values,
//! and what the HDR encode decodes untagged content with (see
//! `output_encode.frag`). The intent is relative colorimetric with black point
//! compensation, the usual choice for a display.
//!
//! Calibration curves (the `vcgt` tag written by DisplayCAL/ArgyllCMS) are
//! baked into the LUT after the transform. The profile describes the display
//! *with* them loaded, and applying them here instead of in the CRTC gamma LUT
//! keeps them off a hardware cursor that would not get the profile anyway and
//! works on cross-GPU outputs, whose CRTC is driven by another device.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, atomic::AtomicU64},
    time::SystemTime,
};

use lcms2::{
    CIExyY, CIExyYTRIPLE, ColorSpaceSignature, Flags, Intent, PixelFormat, Profile,
    ProfileClassSignature, Tag, TagSignature, ToneCurve, Transform,
};
use tracing::{info, warn};

/// Grid points per axis. 33 is the usual size for display LUTs; the extra
/// points keep the baked-in calibration curves smooth near black, where they
/// bend most.
pub const LUT_SIZE: usize = 65;

/// A display profile turned into a 3D LUT over the compositing space.
#[derive(Debug)]
pub struct IccLut {
    /// Distinguishes LUTs, so a cache keyed by one never mistakes a reloaded
    /// profile for the one it replaced.
    pub id: u64,
    /// Where it was loaded from, for logs.
    pub path: PathBuf,
    /// `LUT_SIZE`³ device RGB values, red varying fastest:
    /// `data[r + N * (g + N * b)]`.
    pub data: Vec<[f32; 3]>,
}

impl PartialEq for IccLut {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl IccLut {
    /// Read and convert the profile at `path`.
    pub fn load(path: &Path) -> Result<Self, String> {
        let bytes = std::fs::read(path).map_err(|error| error.to_string())?;
        Self::from_icc(path, &bytes)
    }

    pub fn from_icc(path: &Path, bytes: &[u8]) -> Result<Self, String> {
        let profile = Profile::new_icc(bytes).map_err(|error| error.to_string())?;
        if profile.color_space() != ColorSpaceSignature::RgbData {
            return Err(format!(
                "not an RGB profile ({:?})",
                profile.color_space()
            ));
        }
        if profile.device_class() != ProfileClassSignature::DisplayClass {
            // Some tools write monitor profiles with another class; lcms takes
            // any RGB output profile, so only mention it.
            warn!(
                path = %path.display(),
                class = ?profile.device_class(),
                "ICC profile is not a display profile; using it anyway"
            );
        }
        let source = source_profile()?;
        let transform: Transform<[f32; 3], [f32; 3]> = Transform::new_flags(
            &source,
            PixelFormat::RGB_FLT,
            &profile,
            PixelFormat::RGB_FLT,
            Intent::RelativeColorimetric,
            Flags::BLACKPOINT_COMPENSATION | Flags::HIGHRES_PRECALC,
        )
        .map_err(|error| error.to_string())?;

        let step = 1.0 / (LUT_SIZE - 1) as f32;
        let mut input = Vec::with_capacity(LUT_SIZE * LUT_SIZE * LUT_SIZE);
        for b in 0..LUT_SIZE {
            for g in 0..LUT_SIZE {
                for r in 0..LUT_SIZE {
                    input.push([r as f32 * step, g as f32 * step, b as f32 * step]);
                }
            }
        }
        let mut data = vec![[0.0f32; 3]; input.len()];
        transform.transform_pixels(&input, &mut data);

        let vcgt = match profile.read_tag(TagSignature::VcgtTag) {
            Tag::VcgtCurves(curves) => Some(curves),
            _ => None,
        };
        for pixel in &mut data {
            for (channel, value) in pixel.iter_mut().enumerate() {
                let clamped = value.clamp(0.0, 1.0);
                *value = match vcgt {
                    Some(curves) => curves[channel].eval(clamped).clamp(0.0, 1.0),
                    None => clamped,
                };
            }
        }

        static NEXT_ID: AtomicU64 = AtomicU64::new(1);
        Ok(Self {
            id: NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            path: path.to_path_buf(),
            data,
        })
    }

    fn at(&self, r: usize, g: usize, b: usize) -> [f32; 3] {
        self.data[r + LUT_SIZE * (g + LUT_SIZE * b)]
    }

    /// Trilinear lookup of one compositing-space value, as the encode shader
    /// does it.
    pub fn sample(&self, rgb: [f32; 3]) -> [f32; 3] {
        let max = (LUT_SIZE - 1) as f32;
        let position = rgb.map(|value| value.clamp(0.0, 1.0) * max);
        let low = position.map(|value| (value.floor() as usize).min(LUT_SIZE - 2));
        let fraction = [0, 1, 2].map(|axis| position[axis] - low[axis] as f32);
        let mut out = [0.0f32; 3];
        for corner in 0..8 {
            let offset = [corner & 1, (corner >> 1) & 1, (corner >> 2) & 1];
            let weight = (0..3)
                .map(|axis| {
                    if offset[axis] == 1 {
                        fraction[axis]
                    } else {
                        1.0 - fraction[axis]
                    }
                })
                .product::<f32>();
            if weight == 0.0 {
                continue;
            }
            let value = self.at(
                low[0] + offset[0],
                low[1] + offset[1],
                low[2] + offset[2],
            );
            for channel in 0..3 {
                out[channel] += value[channel] * weight;
            }
        }
        out
    }

    /// The LUT as an `Abgr2101010` image `LUT_SIZE`² wide and `LUT_SIZE` high:
    /// blue slices side by side, each with red across and green down. Linear
    /// filtering then interpolates red and green inside a slice, and the shader
    /// mixes two slices for blue.
    pub fn packed_2101010(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.data.len() * 4);
        for g in 0..LUT_SIZE {
            for b in 0..LUT_SIZE {
                for r in 0..LUT_SIZE {
                    let [vr, vg, vb] =
                        self.at(r, g, b).map(|value| (value * 1023.0).round() as u32 & 0x3ff);
                    let word = vr | (vg << 10) | (vb << 20) | (0x3 << 30);
                    out.extend_from_slice(&word.to_le_bytes());
                }
            }
        }
        out
    }
}

/// What the composited values mean before the profile: sRGB primaries and D65,
/// decoded with the same pure power the HDR encode uses for SDR content.
fn source_profile() -> Result<Profile, String> {
    let gamma = crate::backend::hdr_pipeline::sdr_reference_gamma() as f64;
    let curve = ToneCurve::new(gamma);
    Profile::new_rgb(
        &CIExyY {
            x: 0.3127,
            y: 0.3290,
            Y: 1.0,
        },
        &CIExyYTRIPLE {
            Red: CIExyY {
                x: 0.64,
                y: 0.33,
                Y: 1.0,
            },
            Green: CIExyY {
                x: 0.30,
                y: 0.60,
                Y: 1.0,
            },
            Blue: CIExyY {
                x: 0.15,
                y: 0.06,
                Y: 1.0,
            },
        },
        &[&curve, &curve, &curve],
    )
    .map_err(|error| error.to_string())
}

/// `~/` expanded against `$HOME`; anything else as written.
pub fn expand_path(path: &str) -> PathBuf {
    match path.strip_prefix("~/") {
        Some(rest) => std::env::var_os("HOME")
            .map(|home| PathBuf::from(home).join(rest))
            .unwrap_or_else(|| PathBuf::from(path)),
        None => PathBuf::from(path),
    }
}

/// Profiles stay loaded while their file is unchanged, so re-evaluating the
/// config does not rebuild the LUT (and reset the encode pass) every time.
#[derive(Default)]
pub struct IccCache {
    entries: HashMap<PathBuf, (Option<SystemTime>, u64, Option<Arc<IccLut>>)>,
}

impl IccCache {
    /// The LUT for `path`, or `None` when it cannot be loaded (logged once
    /// per file version).
    pub fn get(&mut self, path: &Path) -> Option<Arc<IccLut>> {
        let metadata = std::fs::metadata(path);
        let stamp = metadata
            .as_ref()
            .ok()
            .map(|metadata| (metadata.modified().ok(), metadata.len()));
        if let Some((modified, len, lut)) = self.entries.get(path)
            && stamp == Some((*modified, *len))
        {
            return lut.clone();
        }
        let lut = match IccLut::load(path) {
            Ok(lut) => {
                info!(path = %path.display(), "loaded monitor ICC profile");
                Some(Arc::new(lut))
            }
            Err(error) => {
                warn!(path = %path.display(), %error, "failed to load monitor ICC profile; ignoring it");
                None
            }
        };
        let (modified, len) = stamp.unwrap_or((None, 0));
        self.entries
            .insert(path.to_path_buf(), (modified, len, lut.clone()));
        lut
    }
}

/// Process-wide, since profiles are files and outputs come and go.
pub fn cache() -> &'static Mutex<IccCache> {
    static CACHE: std::sync::OnceLock<Mutex<IccCache>> = std::sync::OnceLock::new();
    CACHE.get_or_init(Default::default)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile_bytes(gamma: f64, primaries: &CIExyYTRIPLE, vcgt: Option<f64>) -> Vec<u8> {
        let curve = ToneCurve::new(gamma);
        let mut profile = Profile::new_rgb(
            &CIExyY {
                x: 0.3127,
                y: 0.3290,
                Y: 1.0,
            },
            primaries,
            &[&curve, &curve, &curve],
        )
        .unwrap();
        if let Some(vcgt) = vcgt {
            let curve = ToneCurve::new(vcgt);
            assert!(profile.write_tag(TagSignature::VcgtTag, Tag::VcgtCurves([&curve, &curve, &curve])));
        }
        profile.icc().unwrap()
    }

    fn srgb_primaries() -> CIExyYTRIPLE {
        CIExyYTRIPLE {
            Red: CIExyY { x: 0.64, y: 0.33, Y: 1.0 },
            Green: CIExyY { x: 0.30, y: 0.60, Y: 1.0 },
            Blue: CIExyY { x: 0.15, y: 0.06, Y: 1.0 },
        }
    }

    fn close(a: [f32; 3], b: [f32; 3], tolerance: f32) -> bool {
        a.iter().zip(b).all(|(a, b)| (a - b).abs() <= tolerance)
    }

    /// A profile that says the display already is the source space changes
    /// nothing.
    #[test]
    fn matching_profile_is_identity() {
        let bytes = profile_bytes(2.2, &srgb_primaries(), None);
        let lut = IccLut::from_icc(Path::new("test.icc"), &bytes).unwrap();
        for rgb in [[0.0, 0.0, 0.0], [1.0, 1.0, 1.0], [0.5, 0.25, 0.75], [0.1, 0.9, 0.3]] {
            assert!(close(lut.sample(rgb), rgb, 0.003), "{rgb:?} -> {:?}", lut.sample(rgb));
        }
    }

    /// A display with a steeper response gets lifted midtones; white and
    /// black stay where they are.
    #[test]
    fn gamma_mismatch_is_corrected() {
        let bytes = profile_bytes(2.4, &srgb_primaries(), None);
        let lut = IccLut::from_icc(Path::new("test.icc"), &bytes).unwrap();
        let expected = 0.5f32.powf(2.2 / 2.4);
        let gray = lut.sample([0.5, 0.5, 0.5]);
        assert!(close(gray, [expected; 3], 0.003), "{gray:?} vs {expected}");
        assert!(close(lut.sample([1.0; 3]), [1.0; 3], 0.002));
        assert!(close(lut.sample([0.0; 3]), [0.0; 3], 0.002));
    }

    /// On a wide-gamut display pure sRGB red lies inside the panel's gamut, so
    /// it takes less than the panel's full red plus some green.
    #[test]
    fn wide_gamut_display_desaturates_srgb() {
        let display_p3 = CIExyYTRIPLE {
            Red: CIExyY { x: 0.680, y: 0.320, Y: 1.0 },
            Green: CIExyY { x: 0.265, y: 0.690, Y: 1.0 },
            Blue: CIExyY { x: 0.150, y: 0.060, Y: 1.0 },
        };
        let bytes = profile_bytes(2.2, &display_p3, None);
        let lut = IccLut::from_icc(Path::new("test.icc"), &bytes).unwrap();
        let red = lut.sample([1.0, 0.0, 0.0]);
        assert!(red[0] < 0.97 && red[1] > 0.1, "{red:?}");
        // Gray stays gray.
        let gray = lut.sample([0.5; 3]);
        assert!(close(gray, [0.5; 3], 0.003), "{gray:?}");
    }

    /// Calibration curves apply after the transform.
    #[test]
    fn vcgt_is_baked_in() {
        let bytes = profile_bytes(2.2, &srgb_primaries(), Some(2.0));
        let lut = IccLut::from_icc(Path::new("test.icc"), &bytes).unwrap();
        let gray = lut.sample([0.5; 3]);
        assert!(close(gray, [0.25; 3], 0.004), "{gray:?}");
    }

    #[test]
    fn packed_layout_puts_blue_slices_side_by_side() {
        let bytes = profile_bytes(2.2, &srgb_primaries(), None);
        let lut = IccLut::from_icc(Path::new("test.icc"), &bytes).unwrap();
        let packed = lut.packed_2101010();
        assert_eq!(packed.len(), LUT_SIZE * LUT_SIZE * LUT_SIZE * 4);
        let word = |x: usize, y: usize| {
            let index = (y * LUT_SIZE * LUT_SIZE + x) * 4;
            u32::from_le_bytes(packed[index..index + 4].try_into().unwrap())
        };
        // Last blue slice, first red column, first green row: pure blue.
        let blue = word((LUT_SIZE - 1) * LUT_SIZE, 0);
        assert!(blue & 0x3ff <= 8, "{blue:#x}");
        assert!((blue >> 10) & 0x3ff <= 8, "{blue:#x}");
        assert!((blue >> 20) & 0x3ff >= 1015, "{blue:#x}");
    }
}
