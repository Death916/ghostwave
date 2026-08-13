//! Configuration loading from ~/.config/ghostwave/config.toml

use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Font size in points (scaled by DPI at runtime).
    pub font_size: f32,
    /// Built-in theme name, see theme.rs
    pub theme: String,
    /// Terminal scrollback lines per pane.
    pub scrollback: usize,
    /// Shell to spawn (defaults to $SHELL).
    pub shell: Option<String>,
    /// Padding around terminal cells in logical px.
    pub padding: f32,
    /// Row gap in px between terminal lines.
    pub row_gap: f32,
    /// Per-pane durable-session log cap in MiB. Logs are front-truncated
    /// when they exceed this; raise it to keep more history, lower it to
    /// bound disk usage.
    pub max_log_mb: u64,
    /// AI assistant pane configuration (OpenAI-compatible chat completions).
    pub ai: AiConfig,
    /// Durable remote sessions (ghostd).
    pub remote: RemoteConfig,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct RemoteConfig {
    /// Auto-reconnect a dropped remote pane (bounded by the attempts below).
    pub auto_reconnect: bool,
    /// Max consecutive auto-reconnect attempts before dropping to manual.
    pub reconnect_attempts: u32,
    /// Initial reconnect delay in seconds (backoff caps at 15s).
    pub reconnect_delay_secs: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct AiConfig {
    /// OpenAI-compatible API base URL, e.g. https://api.minimax.chat/v1
    pub base_url: String,
    /// Model name, e.g. MiniMax-M2
    pub model: String,
    /// Environment variable holding the API key.
    pub api_key_env: String,
    /// System prompt for the assistant.
    pub system_prompt: String,
}

impl Default for AiConfig {
    fn default() -> Self {
        Self {
            base_url: "https://api.minimax.chat/v1".into(),
            model: "MiniMax-M2".into(),
            api_key_env: "MINIMAX_API_KEY".into(),
            system_prompt: "You are a helpful assistant embedded in a terminal app (ghostwave). Be concise; prefer short answers and code snippets.".into(),
        }
    }
}

impl Default for RemoteConfig {
    fn default() -> Self {
        Self {
            auto_reconnect: true,
            reconnect_attempts: 5,
            reconnect_delay_secs: 3,
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            font_size: 12.0,
            theme: "tokyonight".into(),
            scrollback: 10_000,
            shell: None,
            padding: 6.0,
            row_gap: 4.0,
            max_log_mb: 32,
            ai: AiConfig::default(),
            remote: RemoteConfig::default(),
        }
    }
}

impl Config {
    pub fn load() -> Self {
        let path = Self::path();
        match std::fs::read_to_string(&path) {
            Ok(contents) => match toml::from_str(&contents) {
                Ok(cfg) => cfg,
                Err(e) => {
                    eprintln!("ghostwave: failed to parse {}: {e}", path.display());
                    Self::default()
                }
            },
            Err(_) => Self::default(),
        }
    }

    pub fn path() -> PathBuf {
        let base = std::env::var("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                std::env::var("HOME")
                    .map(|h| PathBuf::from(h).join(".config"))
                    .unwrap_or_default()
            });
        base.join("ghostwave").join("config.toml")
    }
}
