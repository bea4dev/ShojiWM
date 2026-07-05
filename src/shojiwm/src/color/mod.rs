/// What an output is driven as. Decided per-connector in tty.rs from
/// EDID capability + config override; SDR is the zero-cost default.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum OutputColorMode {
    /// Today's path, bit-for-bit: Argb8888 scanout, sRGB blending.
    Sdr,
    /// PQ/BT.2020 signal: 10-bit scanout + HDR_OUTPUT_METADATA blob.
    Hdr10 { max_display_luminance: f32, min_display_luminance: f32 },
}

/// The space all compositing (blur, liquid-glass, blending) happens in.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BlendSpace {
    /// Non-linear sRGB in Abgr8888 — unchanged current behavior.
    Srgb,
    /// Linear-light BT.2020 in Abgr16161616F (fp16). Requires
    /// GL_EXT_color_buffer_half_float; probed once at device_added.
    LinearBt2020,
}

pub struct OutputColorState {
    pub mode: OutputColorMode,
    pub blend_space: BlendSpace,
    /// EDID-derived display capabilities (smithay-drm-extras).
    pub edid_hdr: Option<EdidHdrMetadata>,
    /// Cached DRM property handles + current metadata blob id.
    pub drm_props: Option<HdrDrmProps>,
}