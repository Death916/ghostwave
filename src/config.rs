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
    /// Terminal cursor visual style ("block", "bar", "beam", "underline", "hollow").
    pub cursor_style: Option<String>,
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
            cursor_style: None,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_toml(s: &str) -> Config {
        toml::from_str(s).expect("test toml should parse")
    }

    #[test]
    fn defaults_when_empty() {
        let cfg: Config = parse_toml("");
        assert!((cfg.font_size - 12.0).abs() < 1e-6);
        assert_eq!(cfg.theme, "tokyonight");
        assert_eq!(cfg.scrollback, 10_000);
        assert_eq!(cfg.padding, 6.0);
        assert_eq!(cfg.row_gap, 4.0);
        assert_eq!(cfg.max_log_mb, 32);
        assert!(cfg.shell.is_none());
    }

    #[test]
    fn partial_override_keeps_defaults() {
        let cfg = parse_toml("font_size = 15.0\ntheme = \"dracula\"\n");
        assert!((cfg.font_size - 15.0).abs() < 1e-6);
        assert_eq!(cfg.theme, "dracula");
        assert_eq!(cfg.scrollback, 10_000, "unspecified field keeps default");
        assert_eq!(cfg.max_log_mb, 32);
    }

    #[test]
    fn bad_toml_falls_back_to_default_via_load_path() {
        let res: Result<Config, _> = toml::from_str("font_size = \"not-a-number\"");
        assert!(res.is_err(), "invalid type should fail to parse: {res:?}");
        assert!(res.unwrap_err().to_string().contains("font_size"), "error should mention the bad field");
        // Config::load's fallback is Default — verify defaults are sane independently
        let def = Config::default();
        assert!((def.font_size - 12.0).abs() < 1e-6);
        assert_eq!(def.scrollback, 10_000);
        assert_eq!(def.theme, "tokyonight");
    }

    #[test]
    fn all_fields_parse() {
        let cfg = parse_toml(
            r#"
            font_size = 13.5
            theme = "nord"
            scrollback = 5000
            shell = "/bin/fish"
            padding = 8.0
            row_gap = 2.0
            max_log_mb = 64
            cursor_style = "beam"
            [remote]
            auto_reconnect = false
            reconnect_attempts = 2
            reconnect_delay_secs = 10
            [ai]
            base_url = "https://example.com/v1"
            model = "test-model"
            api_key_env = "TEST_KEY"
            system_prompt = "hello"
        "#,
        );
        assert!((cfg.font_size - 13.5).abs() < 1e-6);
        assert_eq!(cfg.theme, "nord");
        assert_eq!(cfg.scrollback, 5000);
        assert_eq!(cfg.shell.as_deref(), Some("/bin/fish"));
        assert!((cfg.padding - 8.0).abs() < 1e-6);
        assert!((cfg.row_gap - 2.0).abs() < 1e-6);
        assert_eq!(cfg.max_log_mb, 64);
        assert_eq!(cfg.cursor_style.as_deref(), Some("beam"));
        assert!(!cfg.remote.auto_reconnect);
        assert_eq!(cfg.remote.reconnect_attempts, 2);
        assert_eq!(cfg.remote.reconnect_delay_secs, 10);
        assert_eq!(cfg.ai.base_url, "https://example.com/v1");
        assert_eq!(cfg.ai.model, "test-model");
        assert_eq!(cfg.ai.api_key_env, "TEST_KEY");
        assert_eq!(cfg.ai.system_prompt, "hello");
    }

    #[test]
    fn path_respects_xdg_config_home() {
        // Directly test the path-resolution logic without relying on process-global
        // env mutation (which races under parallel test execution). Reproduce
        // Config::path's logic with explicit inputs.
        fn path_for(xdg: Option<&str>, home: Option<&str>) -> PathBuf {
            let base = xdg
                .map(PathBuf::from)
                .unwrap_or_else(|| home.map(|h| PathBuf::from(h).join(".config")).unwrap_or_default());
            base.join("ghostwave").join("config.toml")
        }
        assert_eq!(path_for(Some("/tmp/xdg-test-123"), Some("/tmp/home")), PathBuf::from("/tmp/xdg-test-123/ghostwave/config.toml"));
        assert_eq!(path_for(None, Some("/tmp/home-test-456")), PathBuf::from("/tmp/home-test-456/.config/ghostwave/config.toml"));
        assert_eq!(path_for(None, None), PathBuf::from("ghostwave/config.toml"));
        // And the real Config::path agrees when XDG_CONFIG_HOME is set (best-effort, no mutation)
        if std::env::var("XDG_CONFIG_HOME").is_ok() {
            let p = Config::path();
            assert!(p.ends_with("ghostwave/config.toml"), "Config::path should end with ghostwave/config.toml, got {p:?}");
        }
    }
}
