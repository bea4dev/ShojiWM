#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DisplayModePreference {
    Auto,
    Exact {
        width: u16,
        height: u16,
        refresh_mhz: Option<i32>,
    },
}

impl Default for DisplayModePreference {
    fn default() -> Self {
        Self::Auto
    }
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
    pub resolution: Option<RuntimeDisplayModePreference>,
    pub position: Option<RuntimeOutputPositionPreference>,
    pub scale: Option<f64>,
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
        self.tty_outputs
            .as_ref()
            .is_none_or(|outputs| outputs.iter().any(|candidate| tty_output_names_match(candidate, output_name)))
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
    if let Some((prefix, rest)) = name.split_once('-') {
        if prefix.starts_with("card") && prefix[4..].chars().all(|ch| ch.is_ascii_digit()) {
            return rest;
        }
    }
    name
}
