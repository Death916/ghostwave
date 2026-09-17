//! iced front-end configuration overlay loaded from ~/.config/ghostwave/config.toml.

use serde::Deserialize;

/// Mode for dynamic pane and window title generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DynamicTitleMode {
    /// Tracks foreground process and falls back to directory or OSC title.
    #[default]
    Process,
    /// Ignores foreground process inspection; honors only OSC escape sequences.
    OscOnly,
    /// Disables dynamic titles; preserves static index/name formatting.
    Off,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum DynamicTitleValue {
    Bool(bool),
    Str(String),
}

impl<'de> Deserialize<'de> for DynamicTitleMode {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        match DynamicTitleValue::deserialize(deserializer)? {
            DynamicTitleValue::Bool(true) => Ok(DynamicTitleMode::Process),
            DynamicTitleValue::Bool(false) => Ok(DynamicTitleMode::Off),
            DynamicTitleValue::Str(s) => match s.to_ascii_lowercase().as_str() {
                "process" => Ok(DynamicTitleMode::Process),
                "osc" | "osc_only" | "osc-only" => Ok(DynamicTitleMode::OscOnly),
                "off" | "false" | "none" => Ok(DynamicTitleMode::Off),
                _ => Ok(DynamicTitleMode::Process),
            },
        }
    }
}

/// Formatting style for the pane's title band.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PaneTitleFormat {
    /// Displays command name when running, directory or OSC title when idle.
    #[default]
    Process,
    /// Prioritizes current working directory over process name.
    Pwd,
    /// Combines process and working directory (`cmd (~/dir)`).
    Both,
}

impl<'de> Deserialize<'de> for PaneTitleFormat {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        match s.to_ascii_lowercase().as_str() {
            "pwd" | "cwd" | "dir" => Ok(PaneTitleFormat::Pwd),
            "both" => Ok(PaneTitleFormat::Both),
            _ => Ok(PaneTitleFormat::Process),
        }
    }
}

/// Configuration settings specific to the iced front-end.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct IcedConfig {
    /// Whether dynamic pane titles are enabled, and which strategy is used.
    pub dynamic_title: DynamicTitleMode,
    /// Which format to use when rendering the pane title.
    pub pane_title_format: PaneTitleFormat,
}

impl Default for IcedConfig {
    fn default() -> Self {
        Self {
            dynamic_title: DynamicTitleMode::Process,
            pane_title_format: PaneTitleFormat::Process,
        }
    }
}

impl IcedConfig {
    /// Loads iced-specific configuration options from the shared configuration file.
    pub fn load() -> Self {
        let path = ghostwave::config::Config::path();
        match std::fs::read_to_string(&path) {
            Ok(s) => toml::from_str(&s).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }
}
