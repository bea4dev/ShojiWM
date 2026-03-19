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
}
