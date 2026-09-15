#[derive(Debug, Clone, PartialEq, Eq)]
#[derive(Default)]
pub enum DisplayModePreference {
    #[default]
    Auto,
    Exact {
        width: u16,
        height: u16,
        refresh_mhz: Option<i32>,
    },
}


#[derive(Debug, Clone, Default)]
pub struct DisplayConfig {
    pub default_mode: DisplayModePreference,
    pub tty_outputs: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeDisplayConfigUpdate {
    #[serde(default)]
    pub outputs: std::collections::BTreeMap<String, Option<RuntimeOutputConfig>>,
}

#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeOutputConfig {
    pub mode: Option<RuntimeOutputMode>,
    pub source: Option<String>,
    pub resolution: Option<RuntimeDisplayModePreference>,
    pub position: Option<RuntimeOutputPositionPreference>,
    pub scale: Option<f64>,
    pub transform: Option<RuntimeOutputTransform>,
    pub hdr: Option<bool>,
    /// Real peak luminance of the display in cd/m². Only needed when the
    /// EDID advertises PQ but omits its luminance fields, which is common.
    pub hdr_max_luminance: Option<f32>,
    /// Real black level of the display in cd/m². Same caveat.
    pub hdr_min_luminance: Option<f32>,
    /// Physical subpixel layout of the panel, advertised to clients in
    /// `wl_output.geometry`. Unset keeps what the kernel reported for the
    /// connector, which is `unknown` for most panels.
    pub subpixel: Option<RuntimeOutputSubpixel>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Deserialize)]
pub enum RuntimeOutputTransform {
    #[default]
    #[serde(rename = "normal")]
    Normal,
    #[serde(rename = "rotate-90")]
    Rotate90,
    #[serde(rename = "rotate-180")]
    Rotate180,
    #[serde(rename = "rotate-270")]
    Rotate270,
    #[serde(rename = "flipped")]
    Flipped,
    #[serde(rename = "flipped-90")]
    Flipped90,
    #[serde(rename = "flipped-180")]
    Flipped180,
    #[serde(rename = "flipped-270")]
    Flipped270,
}

impl RuntimeOutputTransform {
    pub fn to_smithay(self) -> smithay::utils::Transform {
        use smithay::utils::Transform;
        match self {
            Self::Normal => Transform::Normal,
            Self::Rotate90 => Transform::_90,
            Self::Rotate180 => Transform::_180,
            Self::Rotate270 => Transform::_270,
            Self::Flipped => Transform::Flipped,
            Self::Flipped90 => Transform::Flipped90,
            Self::Flipped180 => Transform::Flipped180,
            Self::Flipped270 => Transform::Flipped270,
        }
    }
}

/// How a panel's subpixels are physically arranged: the `wl_output.subpixel`
/// values, kebab-cased like [`RuntimeOutputTransform`]. It describes the panel
/// as built, so it is never adjusted for the output's transform.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
pub enum RuntimeOutputSubpixel {
    #[serde(rename = "unknown")]
    Unknown,
    #[serde(rename = "none")]
    None,
    #[serde(rename = "horizontal-rgb")]
    HorizontalRgb,
    #[serde(rename = "horizontal-bgr")]
    HorizontalBgr,
    #[serde(rename = "vertical-rgb")]
    VerticalRgb,
    #[serde(rename = "vertical-bgr")]
    VerticalBgr,
}

impl RuntimeOutputSubpixel {
    pub fn to_smithay(self) -> smithay::output::Subpixel {
        use smithay::output::Subpixel;
        match self {
            Self::Unknown => Subpixel::Unknown,
            Self::None => Subpixel::None,
            Self::HorizontalRgb => Subpixel::HorizontalRgb,
            Self::HorizontalBgr => Subpixel::HorizontalBgr,
            Self::VerticalRgb => Subpixel::VerticalRgb,
            Self::VerticalBgr => Subpixel::VerticalBgr,
        }
    }
}

/// The subpixel layout an output should advertise: the display config's when
/// it names one, otherwise the one detected for the connector.
pub fn resolve_output_subpixel(
    configured: Option<RuntimeOutputSubpixel>,
    detected: smithay::output::Subpixel,
) -> smithay::output::Subpixel {
    configured.map_or(detected, RuntimeOutputSubpixel::to_smithay)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RuntimeOutputMode {
    Extend,
    Disabled,
    Mirror,
}

impl RuntimeOutputConfig {
    pub fn mode(&self) -> RuntimeOutputMode {
        self.mode.unwrap_or(RuntimeOutputMode::Extend)
    }
}

#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
#[serde(untagged)]
pub enum RuntimeDisplayModePreference {
    Best(String),
    Exact {
        width: u16,
        height: u16,
        #[serde(rename = "refreshRate")]
        refresh_rate: Option<f64>,
    },
}

#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
#[serde(untagged)]
pub enum RuntimeOutputPositionPreference {
    Auto(String),
    Exact { x: i32, y: i32 },
}

impl DisplayConfig {
    pub fn from_env() -> Self {
        Self {
            default_mode: DisplayModePreference::default(),
            tty_outputs: parse_tty_outputs_from_env(),
        }
    }

    pub fn tty_output_allowed(&self, output_name: &str) -> bool {
        self.tty_outputs.as_ref().is_none_or(|outputs| {
            outputs
                .iter()
                .any(|candidate| tty_output_names_match(candidate, output_name))
        })
    }
}

fn parse_tty_outputs_from_env() -> Option<Vec<String>> {
    let value = std::env::var_os("SHOJI_TTY_OUTPUT")?;
    let outputs = value
        .to_string_lossy()
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    (!outputs.is_empty()).then_some(outputs)
}

pub fn tty_output_names_match(candidate: &str, actual: &str) -> bool {
    normalize_tty_output_name(candidate) == normalize_tty_output_name(actual)
}

fn normalize_tty_output_name(name: &str) -> &str {
    if let Some((prefix, rest)) = name.split_once('-')
        && prefix.starts_with("card") && prefix[4..].chars().all(|ch| ch.is_ascii_digit()) {
            return rest;
        }
    name
}

#[cfg(test)]
mod tests {
    use super::*;

    // The TS config sends transform as kebab-case strings; a rename drift
    // would silently drop rotation on every hot reload.
    #[test]
    fn runtime_output_config_parses_all_transform_names() {
        let cases = [
            ("normal", RuntimeOutputTransform::Normal),
            ("rotate-90", RuntimeOutputTransform::Rotate90),
            ("rotate-180", RuntimeOutputTransform::Rotate180),
            ("rotate-270", RuntimeOutputTransform::Rotate270),
            ("flipped", RuntimeOutputTransform::Flipped),
            ("flipped-90", RuntimeOutputTransform::Flipped90),
            ("flipped-180", RuntimeOutputTransform::Flipped180),
            ("flipped-270", RuntimeOutputTransform::Flipped270),
        ];
        for (name, expected) in cases {
            let json = format!(r#"{{"mode":"extend","transform":"{name}"}}"#);
            let config: RuntimeOutputConfig = serde_json::from_str(&json)
                .unwrap_or_else(|error| panic!("failed to parse transform {name}: {error}"));
            assert_eq!(config.transform, Some(expected));
        }
    }

    #[test]
    fn runtime_output_config_transform_defaults_to_none() {
        let config: RuntimeOutputConfig =
            serde_json::from_str(r#"{"mode":"extend","scale":1.5}"#).unwrap();
        assert_eq!(config.transform, None);
    }

    /// The `hdr` opt-in arrives from the TypeScript display config; missing
    /// means None so older configs keep their SDR behavior.
    #[test]
    fn runtime_output_config_parses_hdr_flag() {
        let update: RuntimeDisplayConfigUpdate = serde_json::from_str(
            r#"{"outputs":{
                "HDMI-A-3":{"mode":"extend","resolution":"best","hdr":true},
                "eDP-1":{"mode":"extend","resolution":"best"}
            }}"#,
        )
        .expect("display config update should parse");
        assert_eq!(
            update
                .outputs["HDMI-A-3"]
                .as_ref()
                .unwrap()
                .hdr,
            Some(true)
        );
        assert_eq!(
            update.outputs["eDP-1"].as_ref().unwrap().hdr,
            None,
        );
    }

    // MinkaConf writes these names into minka-settings.json; a rename drift
    // would make the whole display config update fail to parse.
    #[test]
    fn runtime_output_config_parses_all_subpixel_names() {
        let cases = [
            ("unknown", RuntimeOutputSubpixel::Unknown),
            ("none", RuntimeOutputSubpixel::None),
            ("horizontal-rgb", RuntimeOutputSubpixel::HorizontalRgb),
            ("horizontal-bgr", RuntimeOutputSubpixel::HorizontalBgr),
            ("vertical-rgb", RuntimeOutputSubpixel::VerticalRgb),
            ("vertical-bgr", RuntimeOutputSubpixel::VerticalBgr),
        ];
        for (name, expected) in cases {
            let json = format!(r#"{{"mode":"extend","subpixel":"{name}"}}"#);
            let config: RuntimeOutputConfig = serde_json::from_str(&json)
                .unwrap_or_else(|error| panic!("failed to parse subpixel {name}: {error}"));
            assert_eq!(config.subpixel, Some(expected));
        }
    }

    /// Absent and `null` both mean "keep the detected layout".
    #[test]
    fn runtime_output_config_subpixel_absent_or_null_is_none() {
        let absent: RuntimeOutputConfig =
            serde_json::from_str(r#"{"mode":"extend"}"#).unwrap();
        assert_eq!(absent.subpixel, None);
        let null: RuntimeOutputConfig =
            serde_json::from_str(r#"{"mode":"mirror","source":"eDP-1","subpixel":null}"#)
                .unwrap();
        assert_eq!(null.subpixel, None);
    }

    #[test]
    fn configured_subpixel_wins_and_unset_keeps_the_detected_layout() {
        use smithay::output::Subpixel;
        assert_eq!(
            resolve_output_subpixel(None, Subpixel::VerticalBgr),
            Subpixel::VerticalBgr,
        );
        assert_eq!(
            resolve_output_subpixel(Some(RuntimeOutputSubpixel::HorizontalRgb), Subpixel::Unknown),
            Subpixel::HorizontalRgb,
        );
        // An explicit "unknown" still overrides a kernel report the user
        // knows to be wrong.
        assert_eq!(
            resolve_output_subpixel(Some(RuntimeOutputSubpixel::Unknown), Subpixel::HorizontalBgr),
            Subpixel::Unknown,
        );
    }

    /// The snapshot reports layouts in the same spelling the config accepts,
    /// so a value read back from a snapshot can be written straight into the
    /// config. Pin the two enums to each other through smithay's.
    #[test]
    fn subpixel_snapshot_names_parse_back_as_config_values() {
        use crate::ssd::OutputSubpixelSnapshot;
        use smithay::output::Subpixel;
        for subpixel in [
            Subpixel::Unknown,
            Subpixel::None,
            Subpixel::HorizontalRgb,
            Subpixel::HorizontalBgr,
            Subpixel::VerticalRgb,
            Subpixel::VerticalBgr,
        ] {
            let json = serde_json::to_string(&OutputSubpixelSnapshot::from(subpixel)).unwrap();
            let parsed: RuntimeOutputSubpixel = serde_json::from_str(&json)
                .unwrap_or_else(|error| panic!("snapshot name {json} does not parse: {error}"));
            assert_eq!(parsed.to_smithay(), subpixel);
        }
    }
}
