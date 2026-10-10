//! EDID HDR capability probing and DRM connector properties for HDR10
//! signaling: `max bpc`, `Colorspace`, and the `HDR_OUTPUT_METADATA`
//! (SMPTE ST 2086 / CTA-861.3) property blob.
//!
//! Property writes use the legacy SET_PROPERTY ioctl on purpose: the kernel
//! folds them into the connector's atomic state, and smithay's commit path
//! never touches these three properties, so they persist across the
//! DrmOutputManager's own atomic commits.

use std::io;

use smithay::reexports::drm::control::{
    Device as ControlDevice,
    ResourceHandle,
    connector,
    crtc,
    property,
};
use tracing::{
    debug,
    info,
    warn,
};

use super::{ColorPrimaries, OutputColorMode};

/// CTA-861-G HDR static metadata parsed from the EDID.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq
)]
pub struct EdidHdrMetadata {
    /// Display accepts SMPTE ST 2084 (PQ) EOTF.
    pub supports_pq: bool,
    /// Display accepts Hybrid Log-Gamma EOTF.
    pub supports_hlg: bool,
    /// Desired max luminance (cd/m²), if the panel reports one.
    pub max_luminance: Option<f32>,
    /// Desired max frame-average luminance (cd/m²), if reported.
    pub max_frame_avg_luminance: Option<f32>,
    /// Desired min luminance (cd/m²), if reported (needs max to decode).
    pub min_luminance: Option<f32>,
    /// The panel's suggested maximum for full-screen SDR content (cd/m²), from
    /// a DisplayID Brightness Luminance Range block. What SDR white reaches at
    /// full brightness when it follows the backlight.
    pub max_sdr_luminance: Option<f32>,
}

const PROP_EDID: &str = "EDID";
const PROP_COLORSPACE: &str = "Colorspace";
const PROP_HDR_OUTPUT_METADATA: &str = "HDR_OUTPUT_METADATA";
const PROP_MAX_BPC: &str = "max bpc";
const PROP_BROADCAST_RGB: &str = "Broadcast RGB";
const PROP_CONTENT_TYPE: &str = "content type";
/// CRTC colour-pipeline blobs ShojiWM never programs; 0 means bypass.
const CRTC_COLOR_BLOBS: [&str; 3] = ["DEGAMMA_LUT", "CTM", "GAMMA_LUT"];

/// Kernel uapi `hdr_metadata_infoframe` (drm_mode.h), CTA-861.3 static
/// metadata type 1. Chromaticities in 0.00002 units, max mastering
/// luminance in cd/m², min in 0.0001 cd/m².
#[repr(C)]
struct HdrMetadataInfoframe {
    eotf: u8,
    metadata_type: u8,
    display_primaries: [[u16; 2]; 3],
    white_point: [u16; 2],
    max_display_mastering_luminance: u16,
    min_display_mastering_luminance: u16,
    max_cll: u16,
    max_fall: u16,
}

/// Kernel uapi `hdr_output_metadata` (drm_mode.h).
#[repr(C)]
struct HdrOutputMetadata {
    metadata_type: u32,
    hdmi_metadata_type1: HdrMetadataInfoframe,
}

/// CTA-861-G EOTF code for SMPTE ST 2084 (PQ).
const HDMI_EOTF_ST2084: u8 = 2;
/// CTA-861-G static metadata descriptor type 1.
const HDMI_STATIC_METADATA_TYPE1: u8 = 0;

fn find_connector_property(
    device: &impl ControlDevice,
    conn: &connector::Info,
    name: &str,
) -> Option<(property::Info, property::RawValue)> {
    find_object_property(
        device,
        conn
            .handle(), 
        name,
    )
}

fn find_object_property(
    device: &impl ControlDevice,
    handle: impl ResourceHandle,
    name: &str,
) -> Option<(property::Info, property::RawValue)> {
    let props = device.get_properties(handle).ok()?;
    for (handle, value) in props.iter() {
        let Ok(info) = device.get_property(*handle) else {
            continue;
        };
        if info.name().to_str() == Ok(name) {
            return Some((info, *value));
        }
    }
    None
}

/// Read the connector's EDID blob and extract the CTA-861-G HDR static
/// metadata data block, if the display has one.
pub fn read_edid_hdr(
    device: &impl ControlDevice,
    conn: &connector::Info,
) -> Option<EdidHdrMetadata> {
    let (_, blob_id) = find_connector_property(
        device,
        conn,
        PROP_EDID)?;
    if blob_id == 0 {
        return None;
    }
    let edid = device.get_property_blob(blob_id).ok()?;
    parse_edid_hdr(&edid)
}

/// Call `visit(tag, payload)` for every CTA-861 data block in the EDID, where
/// `payload` is the block's bytes after its header byte.
///
/// CTA data blocks live in two places. The familiar one is a CTA-861 extension
/// block (tag 0x02). The other is a DisplayID extension block (tag 0x70), whose
/// "CTA-861 DisplayID Data Block" (tag 0x81) carries a CTA data block
/// collection of its own. Laptop eDP panels commonly use only the latter: the
/// Samsung ATNA40CU05 OLED, for one, declares its HDR static metadata (ST 2084,
/// 616 cd/m2) there and has no CTA extension at all, so reading CTA extensions
/// alone took an HDR panel for SDR.
fn for_each_cta_data_block(edid: &[u8], mut visit: impl FnMut(u8, &[u8])) {
    if edid.len() < 128 {
        return;
    }
    let extension_count = edid[126] as usize;
    for block_index in 1..=extension_count {
        let start = block_index * 128;
        let Some(block) = edid.get(start..start + 128) else {
            break;
        };
        match block[0] {
            // CTA-861 extension. Byte 2 is the offset of the detailed timing
            // descriptors; the data block collection sits between byte 4 and it.
            0x02 => {
                let dtd_offset = (block[2] as usize).min(128);
                if dtd_offset >= 4 {
                    visit_cta_data_block_collection(&block[4..dtd_offset], &mut visit);
                }
            }
            // DisplayID extension: [0x70, version, section bytes, product type,
            // extension count], then data blocks of [tag, revision, payload
            // length, payload], then the section checksum.
            0x70 => visit_displayid_data_blocks(block, |tag, payload| {
                // CTA-861 DisplayID Data Block (DisplayID 1.3 and 2.0).
                if tag == 0x81 {
                    visit_cta_data_block_collection(payload, &mut visit);
                }
            }),
            _ => {}
        }
    }
}

/// Call `visit(tag, payload)` for each data block of a DisplayID extension
/// block (tag 0x70).
fn visit_displayid_data_blocks(block: &[u8], mut visit: impl FnMut(u8, &[u8])) {
    let section_end = (5 + block[2] as usize).min(127);
    let mut index = 5;
    while index + 3 <= section_end {
        let tag = block[index];
        let length = block[index + 2] as usize;
        let payload_end = index + 3 + length;
        if payload_end > section_end {
            break;
        }
        visit(tag, &block[index + 3..payload_end]);
        index = payload_end;
    }
}

/// The DisplayID 2.0 Brightness Luminance Range block (tag 0x2E): three IEEE
/// half floats, the minimum, the suggested maximum and the boost maximum for
/// full-screen SDR content. Returns the suggested maximum.
fn parse_displayid_max_sdr_luminance(edid: &[u8]) -> Option<f32> {
    let extension_count = edid.get(126).copied()? as usize;
    let mut found = None;
    for block_index in 1..=extension_count {
        let start = block_index * 128;
        let Some(block) = edid.get(start..start + 128) else {
            break;
        };
        if block[0] == 0x70 {
            visit_displayid_data_blocks(block, |tag, payload| {
                if tag == 0x2e && payload.len() >= 4 && found.is_none() {
                    found = Some(half_to_f32(u16::from_le_bytes([payload[2], payload[3]])));
                }
            });
        }
    }
    found.filter(|nits| nits.is_finite() && *nits > 0.0)
}

fn half_to_f32(bits: u16) -> f32 {
    let mantissa = f32::from(bits & 0x3ff);
    let magnitude = match (bits >> 10) & 0x1f {
        0 => mantissa * 2f32.powi(-24),
        0x1f => f32::INFINITY,
        exponent => (1.0 + mantissa / 1024.0) * 2f32.powi(i32::from(exponent) - 15),
    };
    if bits & 0x8000 != 0 { -magnitude } else { magnitude }
}

/// Call `visit(tag, payload)` for each data block of a CTA data block
/// collection: one header byte (tag in bits 7-5, length in bits 4-0), then
/// `length` payload bytes.
fn visit_cta_data_block_collection(bytes: &[u8], visit: &mut impl FnMut(u8, &[u8])) {
    let mut index = 0;
    while index < bytes.len() {
        let header = bytes[index];
        let length = (header & 0x1f) as usize;
        let Some(payload) = bytes.get(index + 1..index + 1 + length) else {
            break;
        };
        visit(header >> 5, payload);
        index += 1 + length;
    }
}

/// Find the CTA-861 HDR static metadata data block (extended tag 0x06), in a
/// CTA-861 extension or in a DisplayID one (see [`for_each_cta_data_block`]).
pub fn parse_edid_hdr(edid: &[u8]) -> Option<EdidHdrMetadata> {
    let mut found = None;
    for_each_cta_data_block(edid, |tag, payload| {
        // Extended tag block (7) with extended tag 0x06 = HDR static
        // metadata. Payload after the extended tag: [eotf bitfield,
        // descriptor bitfield, optional max/max-frame-avg/min luminance codes].
        if found.is_some() || tag != 0x07 || payload.len() < 2 || payload[0] != 0x06 {
            return;
        }
        let payload = &payload[1..];
        let eotfs = payload[0];
        let max_code = payload.get(2).copied().filter(|&code| code != 0);
        let max_frame_avg_code = payload.get(3).copied().filter(|&code| code != 0);
        let min_code = payload.get(4).copied();
        let max_luminance = max_code.map(cta_luminance);
        let max_frame_avg_luminance = max_frame_avg_code.map(cta_luminance);
        // Min luminance decoding needs the max value as reference.
        let min_luminance = match (max_luminance, min_code) {
            (Some(max), Some(code)) => {
                let fraction = code as f32 / 255.0;
                Some(max * fraction * fraction / 100.0)
            }
            _ => None,
        };
        found = Some(EdidHdrMetadata {
            supports_pq: eotfs & (1 << 2) != 0,
            supports_hlg: eotfs & (1 << 3) != 0,
            max_luminance,
            max_frame_avg_luminance,
            min_luminance,
            max_sdr_luminance: None,
        });
    });
    found.map(|metadata| EdidHdrMetadata {
        max_sdr_luminance: parse_displayid_max_sdr_luminance(edid),
        ..metadata
    })
}

/// What the sink says its HDMI link can carry, read from the EDID's
/// vendor-specific data blocks.
///
/// This exists because link bandwidth, not the compositor, is what decides
/// whether an HDR mode is usable. 3840x2160@60 has a 594 MHz pixel clock; at
/// 8 bpc that is 594 MHz of TMDS character rate and fits HDMI 2.0's 600 MHz
/// ceiling, but PQ needs 10 bpc, which costs 594 * 10/8 = 742.5 MHz and does
/// not. The driver then silently falls back to 8 bpc or to subsampled chroma,
/// and PQ tolerates neither — 8-bit PQ bands severely in near-black because
/// the curve spends most of its code range there by design.
///
/// So the mode list has to be filtered against this before an HDR mode is
/// offered, rather than letting the user pick one that cannot work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HdmiLinkCapability {
    /// Max TMDS character rate in kHz. HDMI 1.4b/2.0 VSDB byte 7, in units
    /// of 5 MHz. `None` when the block is absent or truncated.
    pub max_tmds_khz: Option<u32>,
    /// HDMI 2.1 `Max_FRL_Rate` code from the HDMI Forum VSDB, 0 meaning FRL
    /// is not supported. Best-effort: the sink is free to omit the block.
    pub max_frl_rate: Option<u8>,
}

impl HdmiLinkCapability {
    /// Human-readable standard, inferred from what the sink advertises
    /// rather than from any version field — sinks lie about the latter.
    pub fn standard(&self) -> &'static str {
        if self.max_frl_rate.is_some_and(|rate| rate > 0) {
            "HDMI 2.1"
        } else {
            match self.max_tmds_khz {
                // >340 MHz is the HDMI 2.0 signalling threshold.
                Some(khz) if khz > 340_000 => "HDMI 2.0",
                Some(_) => "HDMI 1.4",
                None => "HDMI (unknown)",
            }
        }
    }

    /// Total usable link bandwidth in Gbit/s.
    ///
    /// TMDS carries 10 bits per character on each of 3 data lanes, so the
    /// ceiling is `rate * 10 * 3`. FRL instead runs 3 or 4 lanes at a fixed
    /// per-lane rate, enumerated by the `Max_FRL_Rate` code.
    pub fn max_bandwidth_gbps(&self) -> Option<f32> {
        if let Some(rate) = self.max_frl_rate.filter(|rate| *rate > 0) {
            return match rate {
                1 => Some(9.0),   // 3 Gbps x 3 lanes
                2 => Some(18.0),  // 6 Gbps x 3 lanes
                3 => Some(24.0),  // 6 Gbps x 4 lanes
                4 => Some(32.0),  // 8 Gbps x 4 lanes
                5 => Some(40.0),  // 10 Gbps x 4 lanes
                6 => Some(48.0),  // 12 Gbps x 4 lanes
                _ => None,
            };
        }
        self.max_tmds_khz
            .map(|khz| khz as f32 / 1_000_000.0 * 10.0 * 3.0)
    }

    /// Whether a mode fits, given its pixel clock and the bits per component
    /// the signal needs. RGB 4:4:4 and YCbCr 4:4:4 scale the TMDS character
    /// rate by `bpc / 8`; this deliberately does NOT model 4:2:2 or 4:2:0,
    /// because a desktop cannot use subsampled chroma without destroying
    /// text, so those rates are not honestly available to us.
    pub fn mode_fits(&self, pixel_clock_khz: u32, bpc: u32) -> Option<bool> {
        let required = self.required_tmds_khz(pixel_clock_khz, bpc);
        if let Some(rate) = self.max_frl_rate.filter(|rate| *rate > 0) {
            // FRL is a different transport; approximate by comparing raw
            // bitrates rather than character rates.
            let _ = rate;
            let needed_gbps = pixel_clock_khz as f32 / 1_000_000.0 * (bpc * 3) as f32;
            return self
                .max_bandwidth_gbps()
                .map(|available| needed_gbps <= available);
        }
        self.max_tmds_khz.map(|max| required <= max)
    }

    /// TMDS character rate a mode needs at the given bit depth, in kHz.
    pub fn required_tmds_khz(&self, pixel_clock_khz: u32, bpc: u32) -> u32 {
        pixel_clock_khz.saturating_mul(bpc) / 8
    }
}

/// Read the HDMI vendor-specific data blocks describing link capability.
///
/// Two blocks matter, both Vendor-Specific Data Blocks (CTA tag 3),
/// distinguished by their IEEE OUI stored least-significant byte first:
///   * `00-0C-03` — the HDMI 1.4b VSDB, whose byte 7 is Max_TMDS_Clock.
///   * `C4-5D-D8` — the HDMI Forum VSDB (HDMI 2.1), carrying Max_FRL_Rate.
/// A 2.1 sink normally publishes both, so both are collected.
pub fn parse_edid_hdmi_link(edid: &[u8]) -> Option<HdmiLinkCapability> {
    let mut caps = HdmiLinkCapability::default();
    let mut found = false;
    for_each_cta_data_block(edid, |tag, payload| {
        // Vendor-Specific Data Block.
        if tag != 0x03 || payload.len() < 3 {
            return;
        }
        match payload[0..3] {
            // HDMI 1.4b VSDB. Byte 6 of the payload (after the 3-byte
            // OUI, 2-byte source physical address and the flags byte)
            // is Max_TMDS_Clock in 5 MHz units; 0 means "not stated".
            [0x03, 0x0C, 0x00] => {
                if let Some(&code) = payload.get(6)
                    && code != 0
                {
                    caps.max_tmds_khz = Some(u32::from(code) * 5_000);
                    found = true;
                }
            }
            // HDMI Forum VSDB. Byte 4 restates the TMDS ceiling, and
            // Max_FRL_Rate sits in the high nibble of byte 7.
            //
            // The OUI is C4-5D-D8 written big-endian, but EDID stores
            // it least-significant byte first, so the bytes on the wire
            // are D8 5D C4. Getting this backwards makes the block
            // invisible and the sink looks like plain HDMI 1.4 — which
            // is exactly what happened here first time: a 600 MHz sink
            // reported as 300 MHz because only the 1.4b block matched.
            [0xD8, 0x5D, 0xC4] => {
                if let Some(&code) = payload.get(4)
                    && code != 0
                {
                    caps.max_tmds_khz = caps
                        .max_tmds_khz
                        .max(Some(u32::from(code) * 5_000));
                }
                if let Some(&byte) = payload.get(7) {
                    caps.max_frl_rate = Some(byte >> 4);
                }
                found = true;
            }
            _ => {}
        }
    });
    found.then_some(caps)
}

/// Read the connector's EDID and extract its HDMI link capability.
pub fn read_edid_hdmi_link(
    device: &impl ControlDevice,
    conn: &connector::Info,
) -> Option<HdmiLinkCapability> {
    let (_, blob_id) = find_connector_property(device, conn, PROP_EDID)?;
    if blob_id == 0 {
        return None;
    }
    let edid = device.get_property_blob(blob_id).ok()?;
    parse_edid_hdmi_link(&edid)
}

/// CTA-861-G luminance code decoding: 50 * 2^(code/32) cd/m².
fn cta_luminance(code: u8) -> f32 {
    50.0 * 2f32.powf(code as f32 / 32.0)
}

/// Put the connector into HDR10 signaling: max bpc >= 10, Colorspace =
/// BT2020_RGB, and an ST 2086 metadata blob. Returns the blob id so the
/// caller can destroy it on disconnect.
pub fn apply_hdr_connector_state(
    device: &impl ControlDevice,
    conn: &connector::Info,
    mode: &OutputColorMode,
) -> io::Result<Option<u64>> {
    let OutputColorMode::Hdr10 {
        max_display_luminance,
        min_display_luminance,
        sdr_white_luminance,
    } = *mode
    else {
        return Ok(None);
    };

    // Raise the link depth so the 10-bit scanout format isn't dithered
    // back down; missing property is fine (some drivers always run 10-bit).
    if let Some((info, current)) = find_connector_property(device, conn, PROP_MAX_BPC) {
        let target = match info.value_type() {
            property::ValueType::UnsignedRange(_, max) => (*max).min(10),
            _ => 10,
        };
        if current < target {
            device.set_property(conn.handle(), info.handle(), target)?;
        }
    }

    // Without a Colorspace property the sink would interpret the PQ signal
    // as sRGB — bail instead of producing garbage.
    let (colorspace_info, _) = find_connector_property(device, conn, PROP_COLORSPACE)
        .ok_or_else(|| io::Error::other("connector has no Colorspace property"))?;
    let bt2020_value = match colorspace_info.value_type() {
        property::ValueType::Enum(values) => values
            .values()
            .1
            .iter()
            .find(|entry| entry
                .name()
                .to_str() == Ok("BT2020_RGB"))
            .map(|entry| entry.value()),
        _ => None,
    }
    .ok_or_else(|| io::Error::other("Colorspace property has no BT2020_RGB entry"))?;

    let (metadata_info, _) = find_connector_property(
        device,
        conn,
        PROP_HDR_OUTPUT_METADATA
    )
        .ok_or_else(|| io::Error::other("connector has no HDR_OUTPUT_METADATA property"))?;

    // What we actually put on the wire, as opposed to what the sink can do —
    // although since the encode carries HDR content up to the display's peak,
    // the two now coincide.
    //
    // The encode pass maps SDR white to `sdr_white_luminance` and HDR content
    // above it, clamping at `max_display_luminance`: that is a hard ceiling of
    // the signal, so the compositor is the mastering display and its peak is
    // the display's. Reporting it lets the sink skip tone mapping it does not
    // need.
    //
    // MaxCLL and MaxFALL go out as the same ceiling rather than 0. CTA-861 reads
    // 0 as "unknown", and a sink told nothing (with the old 1000 cd/m2 fallback
    // as mastering peak) tone-mapped for range the signal never used — measured
    // on a Philips 8505, worst at the dark end. A ceiling is a true upper bound.
    //
    // `max_display_luminance` comes from the config override, then the EDID,
    // then a 1000 cd/m2 fallback; an EDID without luminance figures (common)
    // should be met with `hdrMaxLuminance` in the config.
    let content_peak_nits = max_display_luminance.max(sdr_white_luminance);
    let content_peak = content_peak_nits.round().clamp(0.0, f32::from(u16::MAX)) as u16;

    let chroma = ColorPrimaries::Bt2020.chromaticities();
    let metadata = HdrOutputMetadata {
        metadata_type: HDMI_STATIC_METADATA_TYPE1 as u32,
        hdmi_metadata_type1: HdrMetadataInfoframe {
            eotf: HDMI_EOTF_ST2084,
            metadata_type: HDMI_STATIC_METADATA_TYPE1,
            display_primaries: [
                [
                    chroma.red.to_cta861().0,
                    chroma.red.to_cta861().1
                ],
                [
                    chroma.green.to_cta861().0,
                    chroma.green.to_cta861().1
                ],
                [
                    chroma.blue.to_cta861().0,
                    chroma.blue.to_cta861().1
                ],
            ],
            white_point: [
                chroma.white.to_cta861().0,
                chroma.white.to_cta861().1
            ],
            max_display_mastering_luminance: content_peak,
            min_display_mastering_luminance: (min_display_luminance * 10000.0).round() as u16,
            // Upper bounds, not estimates: no pixel and no frame average can
            // exceed what the encode clamps to.
            max_cll: content_peak,
            max_fall: content_peak,
        },
    };
    let blob = device.create_property_blob(&metadata)?;
    let property::Value::Blob(blob_id) = blob else {
        return Err(io::Error::other("create_property_blob returned non-blob"));
    };
    device.set_property(
        conn.handle(),
        metadata_info.handle(),
        blob_id
    )?;
    device.set_property(
        conn.handle(),
        colorspace_info.handle(),
        bt2020_value
    )?;
    debug!(
        connector = ?conn.handle(),
        blob_id,
        content_peak_nits,
        sink_max_display_luminance = max_display_luminance,
        min_display_luminance,
        "applied HDR10 connector state"
    );
    Ok(Some(blob_id))
}

/// Best-effort reset for SDR outputs: clears HDR metadata and Colorspace
/// leftovers from a previous session so the sink drops out of HDR mode.
pub fn reset_hdr_connector_state(
    device: &impl ControlDevice,
    conn: &connector::Info
) {
    if let Some((info, current)) = find_connector_property(
        device,
        conn,
        PROP_HDR_OUTPUT_METADATA
    )
        && current != 0
            && let Err(error) = device.set_property(
                conn.handle(),
                info.handle(),
                0
            ) {
                warn!(
                    ?error,
                    "failed to clear HDR_OUTPUT_METADATA"
                );
            }
    if let Some((info, current)) = find_connector_property(
        device,
        conn,
        PROP_COLORSPACE
    ) {
        let default_value = match info.value_type() {
            property::ValueType::Enum(values) => values
                .values()
                .1
                .iter()
                .find(|entry| entry.name()
                    .to_str() == Ok("Default"))
                .map(|entry| entry.value()),
            _ => None,
        };
        if let Some(default_value) = default_value
            && current != default_value
                && let Err(error) =
                    device.set_property(
                        conn.handle(),
                        info.handle(),
                        default_value
                    )
                {
                    warn!(
                        ?error,
                        "failed to reset Colorspace"
                    );
                }
    }
}

/// The raw value of the enum entry called `entry` in an enum property.
fn enum_entry_value(info: &property::Info, entry: &str) -> Option<property::RawValue> {
    match info.value_type() {
        property::ValueType::Enum(values) => values
            .values()
            .1
            .iter()
            .find(|candidate| candidate.name().to_str() == Ok(entry))
            .map(|candidate| candidate.value()),
        _ => None,
    }
}

/// Reset colour state that ShojiWM does not manage, so nothing a previous DRM
/// master left behind applies to this output. Best-effort: missing properties
/// are skipped and failures are logged.
///
/// ShojiWM never programs the CRTC's colour pipeline, and smithay's commits only
/// touch CRTC_ID/ACTIVE/MODE_ID/FB_ID, so a DEGAMMA_LUT, CTM or GAMMA_LUT set by
/// plymouth, fbcon or another compositor stays applied under every frame. On
/// Intel that is worse than a no-op even when the table is an identity ramp: a
/// 256-entry GAMMA_LUT (the i*257 ramp the kernel reads back from the boot
/// palette, or one plymouth set; found on every active CRTC here) puts the pipe
/// in 8-bit legacy gamma mode, which caps an HDR output at 8 bits before the
/// 10-bit plane reaches the 12-bit link. Clearing the blobs (0) restores the
/// bypass.
///
/// `Broadcast RGB` and `content type` likewise go back to the kernel defaults
/// (Automatic, No Data) instead of whatever the last master chose, e.g. a KWin
/// session that used Full range or the Graphics content type.
pub fn reset_inherited_color_state(
    device: &impl ControlDevice,
    crtc: crtc::Handle,
    conn: &connector::Info,
) {
    let output = format!(
        "{}-{}",
        conn
            .interface()
                .as_str(),
        conn
            .interface_id()
    );
    // What actually changed, so the INFO line below tells "reset something",
    // "found nothing to reset" and "never ran" apart in an ordinary session log.
    let mut reset: Vec<&'static str> = Vec::new();
    for name in CRTC_COLOR_BLOBS {
        if let Some((info, current)) = find_object_property(device, crtc, name)
            && current != 0
        {
            match device.set_property(
                crtc,
                info.handle(),
                0,
            ) {
                Ok(()) => {
                    debug!(
                        %output,
                        property = name,
                        previous_blob = current,
                        "cleared inherited CRTC colour blob"
                    );
                    reset.push(name);
                }
                Err(error) => warn!(
                    %output,
                    ?crtc,
                    property = name,
                    ?error,
                    "failed to clear inherited CRTC colour blob"
                ),
            }
        }
    }
    for (name, default) in [
        (PROP_BROADCAST_RGB, "Automatic"),
        (PROP_CONTENT_TYPE, "No Data"),
    ] {
        let Some((info, current)) = find_connector_property(device, conn, name) else {
            continue;
        };
        let Some(value) = enum_entry_value(&info, default) else {
            continue;
        };
        if current == value {
            continue;
        }
        match device.set_property(
            conn
                .handle(),
            info
                .handle(),
            value,
        ) {
            Ok(()) => reset.push(name),
            Err(error) => warn!(
                %output,
                property = name,
                ?error,
                "failed to reset inherited connector property"
            ),
        }
    }
    if reset.is_empty() {
        info!(
            %output,
            ?crtc,
            "inherited colour state: nothing to reset"
        );
    } else {
        info!(
            %output,
            ?crtc,
            ?reset,
            "reset inherited colour state"
        );
    }
}

/// Free an HDR_OUTPUT_METADATA blob created by [`apply_hdr_connector_state`].
/// Best-effort: the kernel reclaims blobs at fd close anyway.
pub fn destroy_metadata_blob(
    device: &impl ControlDevice,
    blob: u64
) {
    if let Err(
        error
    ) = device.destroy_property_blob(
        blob
    ) {
        warn!(
            ?error,
            blob,
            "failed to destroy HDR metadata blob"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Base EDID block + one CTA-861 extension carrying an HDR static
    /// metadata data block (extended tag 0x06).
    fn edid_with_hdr_block(
        eotfs: u8,
        max: u8,
        favg: u8,
        min: u8
    ) -> Vec<u8> {
        let mut edid = vec![0u8; 256];
        // one extension block
        edid[126] = 1;
        let ext = 128;
        // CTA-861 tag
        edid[ext] = 0x02;
        // DTDs start at byte 12; data blocks in 4..12
        edid[ext + 2] = 12;
        // extended tag block, length 6
        edid[ext + 4] = (0x07 << 5) | 6;
        // HDR static metadata
        edid[ext + 5] = 0x06;
        edid[ext + 6] = eotfs;
        // static metadata type 1
        edid[ext + 7] = 0x01;
        edid[ext + 8] = max;
        edid[ext + 9] = favg;
        edid[ext + 10] = min;
        edid
    }

    #[test]
    fn parses_hdr_static_metadata() {
        // EOTF bits: SDR (0) + ST 2084 (2). Code 96 = 50 * 2^3 = 400 cd/m².
        let edid = edid_with_hdr_block(
            0b0000_0101,
            96,
            64,
            255);
        let hdr = parse_edid_hdr(
            &edid
        ).expect(
            "HDR block should parse"
        );
        assert!(
            hdr.supports_pq
        );
        assert!(
            !hdr.supports_hlg
        );
        assert_eq!(
            hdr.max_luminance,
            Some(
                400.0
            )
        );
        assert_eq!(
            hdr.max_frame_avg_luminance,
            Some(
                200.0
            )
        );
        // min code 255 => max * 1.0² / 100.
        assert_eq!(
            hdr.min_luminance,
            Some(
                4.0
            )
        );
    }

    /// Laid out like a Samsung ATNA40CU05 OLED (eDP): no CTA-861 extension,
    /// the HDR static metadata only inside a DisplayID 2.0 extension's
    /// "CTA-861 DisplayID Data Block".
    #[test]
    fn parses_hdr_static_metadata_inside_displayid() {
        let mut edid = vec![0u8; 256];
        edid[126] = 1;
        let mut ext = vec![0x70, 0x20, 0, 0x02, 0x00];
        // An unrelated data block first: product identification, 3 bytes.
        ext.extend_from_slice(&[0x20, 0x00, 0x03, 0x4c, 0x83, 0x00]);
        // CTA-861 DisplayID Data Block: colorimetry (BT2020RGB), then HDR
        // static metadata with SDR + ST 2084, type 1, codes 116/96/2.
        let cta = [0xe3, 0x05, 0x80, 0x00, 0xe6, 0x06, 0x05, 0x01, 0x74, 0x60, 0x02];
        ext.extend_from_slice(&[0x81, 0x00, cta.len() as u8]);
        ext.extend_from_slice(&cta);
        // Brightness Luminance Range: 0.0005 / 400 / 616 cd/m² as half floats.
        ext.extend_from_slice(&[0x2e, 0x00, 0x06, 0x18, 0x10, 0x40, 0x5e, 0xd0, 0x60]);
        ext[2] = (ext.len() - 5) as u8;
        edid[128..128 + ext.len()].copy_from_slice(&ext);

        let hdr = parse_edid_hdr(&edid).expect("HDR block inside DisplayID should parse");
        assert!(hdr.supports_pq);
        assert!(!hdr.supports_hlg);
        let max = hdr.max_luminance.expect("max luminance");
        assert!((max - 616.88).abs() < 0.01, "max {max}");
        assert_eq!(hdr.max_frame_avg_luminance, Some(400.0));
        assert!(hdr.min_luminance.is_some_and(|min| min > 0.0 && min < 0.001));
        assert_eq!(hdr.max_sdr_luminance, Some(400.0));
    }

    #[test]
    fn ignores_edid_without_hdr_block() {
        // Plain base block, no extensions.
        let edid = vec![0u8; 128];
        assert_eq!(
            parse_edid_hdr(
                &edid
            ),
            None
        );
        // CTA extension present but empty data block collection.
        let mut edid = vec![0u8; 256];
        edid[126] = 1;
        edid[128] = 0x02;
        edid[130] = 4;
        assert_eq!(
            parse_edid_hdr(
                &edid
            ),
            None,
        );
    }

    #[test]
    fn zero_luminance_codes_mean_unknown() {
        let edid = edid_with_hdr_block(
            0b0000_0100,
            0,
            0,
            0,
        );
        let hdr = parse_edid_hdr(
            &edid
        ).expect(
            "HDR block should parse",
        );
        assert!(
            hdr.supports_pq,
        );
        assert_eq!(
            hdr.max_luminance,
            None,
        );
        assert_eq!(
            hdr.min_luminance,
            None,
        );
    }

    #[test]
    fn hdr_metadata_blob_matches_kernel_layout() {
        // The kernel copies sizeof(struct hdr_output_metadata) bytes; a
        // layout drift would corrupt the infoframe silently.
        assert_eq!(
            std::mem::size_of::<HdrMetadataInfoframe>(),
            26,
        );
        assert_eq!(
            std::mem::size_of::<HdrOutputMetadata>(),
            32,
        );
        assert_eq!(
            std::mem::offset_of!(
                HdrOutputMetadata,
                hdmi_metadata_type1,
            ),
            4,
        );
    }
}
