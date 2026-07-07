//! Color management core: output color modes, blend spaces, and the
//! parametric image descriptions shared by the `wp_color_management_v1`
//! protocol (`protocols/color_management.rs`), the DRM signaling layer
//! (`drm_metadata`), and — in a later phase — the render pipeline.

pub mod colorimetry;
pub mod drm_metadata;
pub mod primaries;

use drm_metadata::EdidHdrMetadata;
use tracing::warn;

/// Named primaries we support parametrically (no custom chromaticities yet).
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq
)]
pub enum ColorPrimaries {
    Srgb,
    Bt2020,
}

/// Transfer characteristics we support parametrically.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq
)]
pub enum TransferCharacteristics {
    Srgb,
    St2084Pq,
    ExtLinear,
}

/// Luminance metadata in cd/m², from `set_luminances` or the
/// per-transfer-function protocol defaults.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq
)]
pub struct Luminances {
    pub min: f32,
    pub max: f32,
    pub reference: f32,
}

impl TransferCharacteristics {
    /// Protocol-defined default luminances: PQ has fixed absolute levels,
    /// everything else defaults to the SDR 80 cd/m² reference.
    pub fn default_luminances(self) -> Luminances {
        match self {
            TransferCharacteristics::St2084Pq => Luminances {
                min: 0.005,
                max: 10000.0,
                reference: 203.0,
            },
            TransferCharacteristics::Srgb | TransferCharacteristics::ExtLinear => Luminances {
                min: 0.2,
                max: 80.0,
                reference: 80.0,
            },
        }
    }
}

/// An immutable parametric image description, as created through
/// `wp_image_description_creator_params_v1` or synthesized for an output.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq
)]
pub struct ImageDescription {
    pub primaries: ColorPrimaries,
    pub tf: TransferCharacteristics,
    /// `None` => the transfer characteristic's default luminances apply.
    pub luminances: Option<Luminances>,
    /// Maximum content light level (cd/m²), if the client provided one.
    pub max_cll: Option<u32>,
    /// Maximum frame-average light level (cd/m²), if the client provided one.
    pub max_fall: Option<u32>,
}

impl ImageDescription {
    pub const SRGB: Self = Self {
        primaries: ColorPrimaries::Srgb,
        tf: TransferCharacteristics::Srgb,
        luminances: None,
        max_cll: None,
        max_fall: None,
    };

    pub fn effective_luminances(&self) -> Luminances {
        self.luminances
            .unwrap_or_else(|| self.tf.default_luminances())
    }
}

/// What an output is driven as. Decided per-connector in `tty.rs` from
/// EDID capability + the `SHOJI_HDR_OUTPUTS` gate; SDR is the zero-cost
/// default.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum OutputColorMode {
    /// Today's path, bit-for-bit: 8/10-bit scanout, sRGB signal.
    Sdr,
    /// PQ/BT.2020 signal: 10-bit scanout + HDR_OUTPUT_METADATA blob.
    Hdr10 {
        max_display_luminance: f32,
        min_display_luminance: f32,
    },
}

/// The space all compositing (blur, liquid-glass, blending) happens in.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq
)]
pub enum BlendSpace {
    /// Non-linear sRGB in Abgr8888 — unchanged current behavior.
    Srgb,
    /// Linear-light BT.2020 in Abgr16161616F (fp16). Requires
    /// GL_EXT_color_buffer_half_float; not wired up yet (phase 3).
    LinearBt2020,
}

/// Per-output color state, keyed by output name in `ShojiWM::output_color`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OutputColorState {
    pub mode: OutputColorMode,
    pub blend_space: BlendSpace,
    /// The signal description clients observe through
    /// `wp_color_management_output_v1`.
    pub description: ImageDescription,
    /// EDID-derived HDR capabilities (CTA-861-G static metadata block).
    pub edid_hdr: Option<EdidHdrMetadata>,
    /// DRM blob id of the HDR_OUTPUT_METADATA currently applied to the
    /// connector; destroyed on disconnect.
    pub hdr_metadata_blob: Option<u64>,
}

impl OutputColorState {
    pub fn new(
        mode: OutputColorMode,
        edid_hdr: Option<EdidHdrMetadata>,
        hdr_metadata_blob: Option<u64>,
    ) -> Self {
        let description = match mode {
            OutputColorMode::Sdr => ImageDescription::SRGB,
            OutputColorMode::Hdr10 {
                max_display_luminance,
                min_display_luminance,
            } => ImageDescription {
                primaries: ColorPrimaries::Bt2020,
                tf: TransferCharacteristics::St2084Pq,
                luminances: Some(Luminances {
                    min: min_display_luminance,
                    max: max_display_luminance,
                    reference: 203.0,
                }),
                max_cll: None,
                max_fall: None,
            },
        };
        // Blending still happens on sRGB-encoded values; the HDR path
        // (backend/hdr_pipeline.rs) composites into an fp16 intermediate
        // and PQ-encodes as a final pass, so HDR outputs display correctly.
        // BlendSpace::LinearBt2020 activates once per-element
        // linearization lands.
        Self {
            mode,
            blend_space: BlendSpace::Srgb,
            description,
            edid_hdr,
            hdr_metadata_blob,
        }
    }
}

/// True while the runtime display config opts any output into HDR
/// (`hdr: true`). Kept in an atomic because the protocol layer's
/// capability checks run per-request without access to compositor state.
static SESSION_HDR_CONFIGURED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Called whenever a runtime display config update lands, with "does any
/// output request HDR". Widens/narrows the protocol advertisement for
/// clients that bind afterwards.
pub fn set_session_hdr_configured(enabled: bool) {
    SESSION_HDR_CONFIGURED
        .store(
            enabled,
            std::sync::atomic::Ordering::Relaxed
        );
}

/// HDR gate for the protocol advertisement (PQ/BT.2020 capabilities):
/// open when the runtime display config opts an output in, or via the
/// `SHOJI_HDR_OUTPUTS=DP-1,DP-2` (or `all`) env override. Off by default
/// because the render pipeline still composites in sRGB.
pub fn hdr_experiment_enabled() -> bool {
    SESSION_HDR_CONFIGURED
        .load(
            std::sync::atomic::Ordering::Relaxed
        )
        || std::env::var("SHOJI_HDR_OUTPUTS").is_ok_and(|value| !value.trim().is_empty())
}

/// Env-override opt-in for one output, independent of the runtime display
/// config (useful for `cargo run` sessions without a config).
pub fn hdr_output_requested_via_env(
    output_name: &str
) -> bool {
    std::env::var("SHOJI_HDR_OUTPUTS").is_ok_and(|value| {
        value
            .split(',')
            .map(str::trim)
            .any(|entry| entry == "all" || entry == output_name)
    })
}

/// Decide how to drive a connector: HDR10 only when the user opted the
/// output in (runtime display config `hdr: true` or the env override,
/// resolved by the caller into `hdr_requested`) *and* its EDID advertises
/// ST 2084 support.
pub fn resolve_output_mode(
    output_name: &str,
    hdr_requested: bool,
    edid_hdr: Option<&EdidHdrMetadata>,
) -> OutputColorMode {
    if !hdr_requested {
        return OutputColorMode::Sdr;
    }
    let Some(edid) = edid_hdr else {
        warn!(
            output = output_name,
            "HDR requested but EDID has no HDR static metadata block; staying SDR"
        );
        return OutputColorMode::Sdr;
    };
    if !edid.supports_pq {
        warn!(
            output = output_name,
            "HDR requested but display does not advertise ST 2084 (PQ); staying SDR"
        );
        return OutputColorMode::Sdr;
    }
    OutputColorMode::Hdr10 {
        max_display_luminance: edid.max_luminance.unwrap_or(1000.0),
        min_display_luminance: edid.min_luminance.unwrap_or(0.005),
    }
}
