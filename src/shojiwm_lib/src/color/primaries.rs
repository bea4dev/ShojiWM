//! CIE 1931 chromaticity data for the named primaries we support, plus the
//! two wire encodings that consume it (Wayland protocol micro-units and
//! CTA-861-G / DRM HDR metadata units).

use super::ColorPrimaries;

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq
)]
pub struct Chromaticity {
    pub x: f32,
    pub y: f32,
}

impl Chromaticity {
    /// `wp_image_description_info_v1.primaries` encoding: CIE xy × 1,000,000.
    pub fn to_protocol(self) -> (
        i32,
        i32
    ) {
        (
            (self.x * 1_000_000.0).round() as i32,
            (self.y * 1_000_000.0).round() as i32,
        )
    }

    /// CTA-861-G / kernel `hdr_output_metadata` encoding: CIE xy in units
    /// of 0.00002 (i.e. × 50,000).
    pub fn to_cta861(self) -> (
        u16,
        u16
    ) {
        (
            (self.x * 50_000.0).round() as u16,
            (self.y * 50_000.0).round() as u16,
        )
    }
}

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq
)]
pub struct PrimariesChromaticities {
    pub red: Chromaticity,
    pub green: Chromaticity,
    pub blue: Chromaticity,
    pub white: Chromaticity,
}

/// BT.709 primaries with D65 white point (shared by sRGB).
pub const SRGB: PrimariesChromaticities = PrimariesChromaticities {
    red: Chromaticity {
        x: 0.640,
        y: 0.330
    },
    green: Chromaticity {
        x: 0.300,
        y: 0.600
    },
    blue: Chromaticity {
        x: 0.150,
        y: 0.060
    },
    white: Chromaticity {
        x: 0.3127,
        y: 0.3290
    },
};

/// BT.2020 primaries with D65 white point.
pub const BT2020: PrimariesChromaticities = PrimariesChromaticities {
    red: Chromaticity {
        x: 0.708,
        y: 0.292
    },
    green: Chromaticity {
        x: 0.170,
        y: 0.797
    },
    blue: Chromaticity {
        x: 0.131, 
        y: 0.046
    },
    white: Chromaticity {
        x: 0.3127,
        y: 0.3290
    },
};

impl ColorPrimaries {
    pub fn chromaticities(self) -> PrimariesChromaticities {
        match self {
            ColorPrimaries::Srgb => SRGB,
            ColorPrimaries::Bt2020 => BT2020,
        }
    }
}
