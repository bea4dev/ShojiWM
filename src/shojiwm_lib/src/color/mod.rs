//! Color management core: output color modes, blend spaces, and the
//! parametric image descriptions shared by the `wp_color_management_v1`
//! protocol (`protocols/color_management.rs`), the DRM signaling layer
//! (`drm_metadata`), and — in a later phase — the render pipeline.

pub mod colorimetry;
pub mod drm_metadata;
pub mod primaries;

use drm_metadata::EdidHdrMetadata;
use tracing::{info, warn};

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
    /// The target color volume's luminance range (cd/m²): what the display
    /// can actually show. Only output descriptions carry one. Clients derive
    /// their HDR headroom from it — Chromium takes `max / reference`, and
    /// without it treats the output as SDR.
    pub target_luminance: Option<TargetLuminance>,
}

/// Minimum and maximum luminance of a target color volume, in cd/m².
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq
)]
pub struct TargetLuminance {
    pub min: f32,
    pub max: f32,
}

impl ImageDescription {
    pub const SRGB: Self = Self {
        primaries: ColorPrimaries::Srgb,
        tf: TransferCharacteristics::Srgb,
        luminances: None,
        max_cll: None,
        max_fall: None,
        target_luminance: None,
    };

    pub fn effective_luminances(&self) -> Luminances {
        self.luminances
            .unwrap_or_else(|| self.tf.default_luminances())
    }

    /// The content's peak in cd/m²: MaxCLL when the client sent a usable one,
    /// else the description's declared maximum. The protocol takes max_cll
    /// from CTA-861-H, where 0 means "unknown", and Mesa's Vulkan WSI sends
    /// exactly that for an HDR10 swapchain created without VkHdrMetadataEXT.
    /// Taken at face value, a peak at or below the black level collapses the
    /// BT.2390 range and paints the whole surface black, so it counts as
    /// missing.
    pub fn content_peak_nits(&self) -> f32 {
        let luminances = self.effective_luminances();
        self.max_cll
            .map(|cll| cll as f32)
            .filter(|&nits| nits > luminances.min)
            .unwrap_or(luminances.max)
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
        /// What SDR white (compositing-space 1.0) is shown at, cd/m².
        sdr_white_luminance: f32,
        /// The primaries the composited (sRGB-relative) picture is shown in:
        /// sRGB, or the panel's native ones; see `RenderColorTarget::primaries`.
        sdr_primaries: primaries::PrimariesChromaticities,
    },
}

impl OutputColorMode {
    /// Whether the two modes put the same signal on the wire, i.e. differ in
    /// SDR white at most. That is a change of the encode alone; the connector
    /// state (Colorspace, HDR metadata) stays as it is.
    pub fn same_signal(&self, other: &Self) -> bool {
        match (*self, *other) {
            (OutputColorMode::Sdr, OutputColorMode::Sdr) => true,
            (
                OutputColorMode::Hdr10 {
                    max_display_luminance: max_a,
                    min_display_luminance: min_a,
                    ..
                },
                OutputColorMode::Hdr10 {
                    max_display_luminance: max_b,
                    min_display_luminance: min_b,
                    ..
                },
            ) => max_a == max_b && min_a == min_b,
            _ => false,
        }
    }

    /// The compositing-space target for rendering to an output in this mode.
    pub fn render_target(&self) -> RenderColorTarget {
        match *self {
            OutputColorMode::Sdr => RenderColorTarget::SDR,
            OutputColorMode::Hdr10 {
                max_display_luminance,
                sdr_white_luminance,
                sdr_primaries,
                ..
            } => RenderColorTarget {
                headroom: (max_display_luminance / sdr_white_luminance.max(1.0)).max(1.0),
                encode_gamma: crate::backend::hdr_pipeline::sdr_reference_gamma(),
                primaries: sdr_primaries,
            },
        }
    }
}

/// How color-managed content is brought into the compositing space for the
/// output being rendered.
///
/// Compositing happens on display-referred, gamma-encoded BT.709 values with
/// SDR white at 1.0. On an SDR output that range is all there is. On an HDR10
/// output the space extends past it: values above 1.0 are highlights up to the
/// display's peak (`headroom` = peak / SDR white), and values below 0.0 are
/// colors outside BT.709, which the encode pass carries back into BT.2020.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RenderColorTarget {
    /// Compositing-space ceiling: 1.0 on SDR, display peak / SDR white on HDR.
    pub headroom: f32,
    /// 0.0: encode with the piecewise sRGB curve (SDR framebuffers). Otherwise
    /// the pure power the HDR encode pass decodes with (`SHOJI_SDR_GAMMA`), so
    /// that tagged content comes back out at exactly its absolute luminance.
    pub encode_gamma: f32,
    /// The primaries the compositing space's sRGB-relative values are shown
    /// in: sRGB (exact), or the panel's native ones (stretched, as the panel
    /// does in SDR mode). Tagged content is converted to sRGB-relative values
    /// first, so the choice applies to all content alike.
    pub primaries: primaries::PrimariesChromaticities,
}

impl RenderColorTarget {
    pub const SDR: Self = Self {
        headroom: 1.0,
        encode_gamma: 0.0,
        primaries: primaries::SRGB,
    };
}

thread_local! {
    static RENDER_COLOR_TARGET: std::cell::Cell<RenderColorTarget> =
        const { std::cell::Cell::new(RenderColorTarget::SDR) };
}

/// The target set by the innermost live [`RenderColorTargetGuard`], or SDR.
/// Elements read it when they are built, which happens while one output is
/// being rendered.
pub fn render_color_target() -> RenderColorTarget {
    RENDER_COLOR_TARGET.with(|target| target.get())
}

/// Sets the render color target until dropped, then restores the previous one.
pub struct RenderColorTargetGuard {
    previous: RenderColorTarget,
}

impl RenderColorTargetGuard {
    pub fn new(target: RenderColorTarget) -> Self {
        Self {
            previous: RENDER_COLOR_TARGET.with(|current| current.replace(target)),
        }
    }
}

impl Drop for RenderColorTargetGuard {
    fn drop(&mut self) {
        RENDER_COLOR_TARGET.with(|current| current.set(self.previous));
    }
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
    /// EDID-derived HDMI link capability. Kept beside `edid_hdr` because the
    /// two are read from the same blob and answer the same question together:
    /// whether an HDR mode is not merely supported but actually deliverable.
    pub hdmi_link: Option<drm_metadata::HdmiLinkCapability>,
    /// DRM blob id of the HDR_OUTPUT_METADATA currently applied to the
    /// connector; destroyed on disconnect.
    pub hdr_metadata_blob: Option<u64>,
}

impl OutputColorState {
    pub fn new(
        mode: OutputColorMode,
        edid_hdr: Option<EdidHdrMetadata>,
        hdmi_link: Option<drm_metadata::HdmiLinkCapability>,
        hdr_metadata_blob: Option<u64>,
    ) -> Self {
        let description = match mode {
            OutputColorMode::Sdr => ImageDescription::SRGB,
            OutputColorMode::Hdr10 {
                max_display_luminance,
                min_display_luminance,
                sdr_white_luminance,
                ..
            } => ImageDescription {
                primaries: ColorPrimaries::Bt2020,
                tf: TransferCharacteristics::St2084Pq,
                // PQ is absolute: its range is fixed at min + 10000 cd/m².
                // The reference is where SDR white lands on this output.
                luminances: Some(Luminances {
                    min: min_display_luminance,
                    max: 10000.0,
                    reference: sdr_white_luminance,
                }),
                max_cll: None,
                max_fall: None,
                // What the display shows; peak / reference is the headroom.
                target_luminance: Some(TargetLuminance {
                    min: min_display_luminance,
                    max: max_display_luminance,
                }),
            },
        };
        // Blending happens on gamma-encoded values; the HDR path
        // (backend/hdr_pipeline.rs) composites into an fp16 intermediate that
        // extends past SDR white (see `RenderColorTarget`) and PQ-encodes as a
        // final pass. BlendSpace::LinearBt2020 would be linear-light blending.
        Self {
            mode,
            blend_space: BlendSpace::Srgb,
            description,
            edid_hdr,
            hdmi_link,
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

/// Display luminance supplied by the user, for the very common case of an
/// EDID that advertises PQ support but omits the luminance fields entirely.
///
/// A Philips 8505 does exactly that: `supports_pq: true` with `max_luminance`,
/// `min_luminance` and `max_frame_avg_luminance` all `None`, on a panel that
/// measures around 400 cd/m2. With nothing to go on the fallback below claims
/// 1000, and that figure reaches clients through `ImageDescription::luminances`.
/// There is no way to derive the truth, so let it be stated.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct HdrLuminanceOverride {
    /// Peak display luminance in cd/m². Ignored outside 50..=10000.
    pub max: Option<f32>,
    /// Black level in cd/m². Ignored outside 0..=10.
    pub min: Option<f32>,
    /// What SDR white is shown at, cd/m². Ignored outside 10..=1000.
    pub sdr_white: Option<f32>,
    /// The panel backlight's setting, 0..=1, when SDR white follows it (no
    /// `sdr_white`, and the output has a backlight). In HDR10 the backlight
    /// stops doing anything — the signal states absolute luminance — so the
    /// brightness the user sets is applied to SDR white instead.
    pub backlight_fraction: Option<f32>,
    /// Show SDR content in the panel's native gamut (when its EDID states one)
    /// rather than as exact sRGB.
    pub sdr_native_gamut: bool,
}

impl HdrLuminanceOverride {
    fn sanitized_max(self, output_name: &str) -> Option<f32> {
        self.max.filter(|value| {
            let ok = (50.0..=10000.0).contains(value);
            if !ok {
                warn!(
                    output = output_name,
                    value, "hdr.maxLuminance outside 50..=10000 cd/m2; ignoring"
                );
            }
            ok
        })
    }

    fn sanitized_sdr_white(self, output_name: &str) -> Option<f32> {
        self.sdr_white.filter(|value| {
            let ok = (10.0..=1000.0).contains(value);
            if !ok {
                warn!(
                    output = output_name,
                    value, "hdr.sdrLuminance outside 10..=1000 cd/m2; ignoring"
                );
            }
            ok
        })
    }

    fn sanitized_min(self, output_name: &str) -> Option<f32> {
        self.min.filter(|value| {
            let ok = (0.0..=10.0).contains(value);
            if !ok {
                warn!(
                    output = output_name,
                    value, "hdr.minLuminance outside 0..=10 cd/m2; ignoring"
                );
            }
            ok
        })
    }
}

/// The dimmest SDR white the backlight can set, cd/m².
const MIN_BACKLIGHT_SDR_WHITE: f32 = 2.0;

/// Decide how to drive a connector: HDR10 only when the user opted the
/// output in (runtime display config `hdr: true` or the env override,
/// resolved by the caller into `hdr_requested`) *and* its EDID advertises
/// ST 2084 support.
///
/// Luminance precedence is config override, then EDID, then the fallback
/// constants. Note this figure describes the DISPLAY, and since the metadata
/// fix it no longer reaches `HDR_OUTPUT_METADATA` — the mastering peak on the
/// wire is what the encode actually emits. This value is what gets advertised
/// to clients as the output's capability.
pub fn resolve_output_mode(
    output_name: &str,
    hdr_requested: bool,
    edid_hdr: Option<&EdidHdrMetadata>,
    luminance_override: HdrLuminanceOverride,
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
    let max_override = luminance_override.sanitized_max(output_name);
    let min_override = luminance_override.sanitized_min(output_name);
    if max_override.is_some() || min_override.is_some() {
        info!(
            output = output_name,
            max_override,
            min_override,
            edid_max = ?edid.max_luminance,
            edid_min = ?edid.min_luminance,
            "applying configured display luminance override"
        );
    }
    let max_display_luminance = max_override
        .or(edid.max_luminance)
        .unwrap_or(1000.0);
    // Following the backlight, full brightness reaches the panel's suggested
    // SDR maximum (what the backlight itself tops out at in SDR mode). A floor
    // keeps a zeroed backlight from blacking the desktop out.
    let backlight_sdr_white = luminance_override.backlight_fraction.map(|fraction| {
        let full = edid
            .max_sdr_luminance
            .or(edid.max_frame_avg_luminance)
            .unwrap_or(max_display_luminance)
            .min(max_display_luminance);
        (fraction.clamp(0.0, 1.0) * full).max(MIN_BACKLIGHT_SDR_WHITE)
    });
    // SDR white cannot sit above the peak: there would be no headroom left,
    // and the encode would clip SDR content.
    let sdr_white_luminance = luminance_override
        .sanitized_sdr_white(output_name)
        .or(backlight_sdr_white)
        .unwrap_or_else(crate::backend::hdr_pipeline::sdr_reference_nits)
        .min(max_display_luminance);
    // In SDR mode the panel shows sRGB values on its own primaries, which on a
    // wide-gamut panel is noticeably more saturated than sRGB. Exact sRGB in
    // HDR then reads as washed out next to it, so by default SDR content keeps
    // the look it has in SDR mode.
    let sdr_primaries = edid
        .native_primaries
        .filter(|_| luminance_override.sdr_native_gamut)
        .unwrap_or(primaries::SRGB);
    OutputColorMode::Hdr10 {
        max_display_luminance,
        min_display_luminance: min_override
            .or(edid.min_luminance)
            .unwrap_or(0.005),
        sdr_white_luminance,
        sdr_primaries,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pq_with_max_cll(max_cll: Option<u32>) -> ImageDescription {
        ImageDescription {
            primaries: ColorPrimaries::Bt2020,
            tf: TransferCharacteristics::St2084Pq,
            luminances: None,
            max_cll,
            max_fall: None,
            target_luminance: None,
        }
    }

    /// What Mesa's WSI sends for an HDR10 swapchain with no HDR metadata.
    #[test]
    fn zero_max_cll_means_unknown() {
        assert_eq!(pq_with_max_cll(Some(0)).content_peak_nits(), 10000.0);
    }

    #[test]
    fn missing_max_cll_falls_back_to_declared_max() {
        assert_eq!(pq_with_max_cll(None).content_peak_nits(), 10000.0);
    }

    #[test]
    fn real_max_cll_is_the_content_peak() {
        assert_eq!(pq_with_max_cll(Some(1000)).content_peak_nits(), 1000.0);
    }

    fn oled_edid() -> EdidHdrMetadata {
        EdidHdrMetadata {
            supports_pq: true,
            supports_hlg: false,
            max_luminance: Some(616.0),
            max_frame_avg_luminance: Some(400.0),
            min_luminance: Some(0.0005),
            max_sdr_luminance: Some(400.0),
            native_primaries: None,
        }
    }

    fn sdr_white(luminance_override: HdrLuminanceOverride) -> f32 {
        match resolve_output_mode("eDP-1", true, Some(&oled_edid()), luminance_override) {
            OutputColorMode::Hdr10 { sdr_white_luminance, .. } => sdr_white_luminance,
            OutputColorMode::Sdr => panic!("should drive HDR10"),
        }
    }

    /// In HDR10 the backlight does nothing, so its setting moves SDR white,
    /// reaching the panel's suggested SDR maximum at full brightness.
    #[test]
    fn sdr_white_follows_the_backlight() {
        let backlight = |fraction| HdrLuminanceOverride {
            backlight_fraction: Some(fraction),
            ..HdrLuminanceOverride::default()
        };
        assert_eq!(sdr_white(backlight(1.0)), 400.0);
        assert!((sdr_white(backlight(0.39)) - 156.0).abs() < 0.01);
        // A zeroed backlight dims the desktop without blacking it out.
        assert_eq!(sdr_white(backlight(0.0)), MIN_BACKLIGHT_SDR_WHITE);
        // A configured luminance wins over the backlight.
        assert_eq!(
            sdr_white(HdrLuminanceOverride {
                sdr_white: Some(300.0),
                ..backlight(0.5)
            }),
            300.0
        );
    }
}
