//! EDID HDR capability probing and DRM connector properties for HDR10
//! signaling: `max bpc`, `Colorspace`, and the `HDR_OUTPUT_METADATA`
//! (SMPTE ST 2086 / CTA-861.3) property blob.
//!
//! Property writes use the legacy SET_PROPERTY ioctl on purpose: the kernel
//! folds them into the connector's atomic state, and smithay's commit path
//! never touches these three properties, so they persist across the
//! DrmOutputManager's own atomic commits.

use std::io;

use smithay::reexports::drm::control::{Device as ControlDevice, connector, property};
use tracing::{debug, warn};

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
}

const PROP_EDID: &str = "EDID";
const PROP_COLORSPACE: &str = "Colorspace";
const PROP_HDR_OUTPUT_METADATA: &str = "HDR_OUTPUT_METADATA";
const PROP_MAX_BPC: &str = "max bpc";

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
    let props = device.get_properties(conn.handle()).ok()?;
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

/// Scan EDID extension blocks for a CTA-861 block containing the HDR
/// static metadata data block (extended tag 0x06).
pub fn parse_edid_hdr(edid: &[u8]) -> Option<EdidHdrMetadata> {
    if edid.len() < 128 {
        return None;
    }
    let extension_count = edid[126] as usize;
    for block_index in 1..=extension_count {
        let start = block_index * 128;
        let Some(block) = edid.get(start..start + 128) else {
            break;
        };
        // CTA-861 extension block tag.
        if block[0] != 0x02 {
            continue;
        }
        // Byte 2: offset of the detailed timing descriptors; the data
        // block collection sits between byte 4 and that offset.
        let dtd_offset = (block[2] as usize).min(128);
        if dtd_offset < 4 {
            continue;
        }
        let mut index = 4;
        while index < dtd_offset {
            let header = block[index];
            let tag = header >> 5;
            let length = (header & 0x1f) as usize;
            if index + 1 + length > dtd_offset {
                break;
            }
            // Extended tag block (7) with extended tag 0x06 = HDR static
            // metadata. Payload: [eotf bitfield, descriptor bitfield,
            // optional max/max-frame-avg/min luminance codes].
            if tag == 0x07 && length >= 2 && block[index + 1] == 0x06 {
                let payload = &block[index + 2..index + 1 + length];
                let eotfs = payload[0];
                let max_code = payload.get(2)
                    .copied()
                    .filter(|&code| code != 0);
                let max_frame_avg_code = payload
                    .get(3)
                    .copied()
                    .filter(|&code| code != 0);
                let min_code = payload
                    .get(4)
                    .copied();
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
                return Some(EdidHdrMetadata {
                    supports_pq: eotfs & (1 << 2) != 0,
                    supports_hlg: eotfs & (1 << 3) != 0,
                    max_luminance,
                    max_frame_avg_luminance,
                    min_luminance,
                });
            }
            index += 1 + length;
        }
    }
    None
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
            max_display_mastering_luminance: max_display_luminance.round() as u16,
            min_display_mastering_luminance: (min_display_luminance * 10000.0).round() as u16,
            // 0 = unknown; we don't track content light levels yet.
            max_cll: 0,
            max_fall: 0,
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
    {
        if current != 0 {
            if let Err(error) = device.set_property(
                conn.handle(),
                info.handle(),
                0
            ) {
                warn!(
                    ?error,
                    "failed to clear HDR_OUTPUT_METADATA"
                );
            }
        }
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
        if let Some(default_value) = default_value {
            if current != default_value {
                if let Err(error) =
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
