//! AI assistant pane (waveterm-style chat) backed by any OpenAI-compatible
//! chat completions endpoint. Requests run on a worker thread so the UI
//! never blocks; responses arrive over a channel.

use std::sync::mpsc::{channel, Receiver, Sender};

use macroquad::prelude::*;

use crate::config::AiConfig;

#[derive(Clone, Debug)]
pub enum Role {
    User,
    Assistant,
    System,
}

#[derive(Clone, Debug)]
pub struct Msg {
    pub role: Role,
    pub text: String,
}

#[derive(Debug)]
pub struct AiPane {
    pub msgs: Vec<Msg>,
    pub input: String,
    pub busy: bool,
    pub scroll: usize,
    pub error: Option<String>,
    rx: Option<Receiver<Result<String, String>>>,
    config: AiConfig,
}

impl AiPane {
    pub fn new(config: AiConfig) -> Self {
        Self {
            msgs: vec![Msg { role: Role::System, text: config.system_prompt.clone() }],
            input: String::new(),
            busy: false,
            scroll: 0,
            error: None,
            rx: None,
            config,
        }
    }

    pub fn send(&mut self) {
        let prompt = self.input.trim().to_string();
        if prompt.is_empty() || self.busy {
            return;
        }
        self.msgs.push(Msg { role: Role::User, text: prompt.clone() });
        self.input.clear();
        self.busy = true;
        self.error = None;

        let (tx, rx) = channel();
        self.rx = Some(rx);

        let config = self.config.clone();
        let mut history: Vec<(String, String)> = self
            .msgs
            .iter()
            .map(|m| {
                let role = match m.role {
                    Role::User => "user",
                    Role::Assistant => "assistant",
                    Role::System => "system",
                };
                (role.to_string(), m.text.clone())
            })
            .collect();
        history.push(("user".into(), prompt));

        std::thread::spawn(move || {
            let result = request_chat(&config, &history);
            let _ = tx.send(result);
        });
    }

    /// Drain a completed response, if any.
    pub fn poll(&mut self) {
        if let Some(rx) = &self.rx {
            if let Ok(result) = rx.try_recv() {
                self.busy = false;
                match result {
                    Ok(text) => {
                        self.msgs.push(Msg { role: Role::Assistant, text });
                        self.scroll = usize::MAX;
                    }
                    Err(e) => self.error = Some(e),
                }
            }
        }
    }

    pub fn clear(&mut self) {
        self.msgs = vec![Msg { role: Role::System, text: self.config.system_prompt.clone() }];
        self.error = None;
    }

    pub fn type_char(&mut self, c: char) {
        self.input.push(c);
    }

    pub fn backspace(&mut self) {
        self.input.pop();
    }

    pub fn render(&mut self, rect: crate::layout::Rect, font: &Font, font_px: u16, theme: crate::theme::Theme) {
        draw_rectangle(rect.x, rect.y, rect.w, rect.h, color_u8!(theme.bg.0, theme.bg.1, theme.bg.2, 255));

        let header_h = font_px as f32 + 16.0;
        // Header
        draw_text_ex(
            "Wave AI",
            rect.x + 8.0,
            rect.y + font_px as f32 + 4.0,
            TextParams { font: Some(font), font_size: font_px, color: color_u8!(theme.frame_active.0, theme.frame_active.1, theme.frame_active.2, 255), ..Default::default() },
        );

        let line_h = (font_px as f32 * 1.3).max(13.0);
        let input_h = font_px as f32 + 20.0;
        let content_h = rect.h - header_h - input_h;

        // Messages (only user + assistant rendered)
        let mut visible: Vec<(Role, String)> = self
            .msgs
            .iter()
            .filter(|m| !matches!(m.role, Role::System))
            .map(|m| (m.role.clone(), m.text.clone()))
            .collect();
        if self.busy {
            visible.push((Role::Assistant, "…".into()));
        }

        // Simple wrapping: split each message into lines at max_chars
        let max_chars = ((rect.w - 16.0) / (font_px as f32 * 0.55)).max(20.0) as usize;
        let mut wrapped: Vec<(Role, String)> = Vec::new();
        for (role, text) in &visible {
            let mut s = text.as_str();
            while s.len() > max_chars {
                let mut cut = max_chars;
                while cut > 0 && !s.is_char_boundary(cut) {
                    cut -= 1;
                }
                let (a, b) = s.split_at(cut);
                wrapped.push((role.clone(), a.to_string()));
                s = b;
            }
            wrapped.push((role.clone(), s.to_string()));
        }

        let total_lines = wrapped.len();
        let visible_lines = (content_h / line_h) as usize;
        let scroll = if self.scroll == usize::MAX {
            total_lines.saturating_sub(visible_lines)
        } else {
            self.scroll.min(total_lines.saturating_sub(visible_lines))
        };

        let mut y = rect.y + header_h;
        for (i, (role, line)) in wrapped.iter().enumerate() {
            if i < scroll {
                continue;
            }
            if y > rect.y + rect.h - input_h {
                break;
            }
            let (col, prefix) = match role {
                Role::User => (theme.frame_active, "❯ "),
                Role::Assistant => (theme.fg, "  "),
                _ => (theme.fg, ""),
            };
            draw_text_ex(
                &format!("{prefix}{line}"),
                rect.x + 8.0,
                y,
                TextParams { font: Some(font), font_size: font_px, color: color_u8!(col.0, col.1, col.2, 255), ..Default::default() },
            );
            y += line_h;
        }

        if let Some(err) = &self.error {
            draw_text_ex(err, rect.x + 8.0, rect.y + rect.h - input_h - line_h, TextParams {
                font: Some(font), font_size: font_px, color: color_u8!(theme.search_match.0, theme.search_match.1, theme.search_match.2, 255), ..Default::default()
            });
        }

        // Input box
        draw_rectangle(rect.x + 4.0, rect.y + rect.h - input_h, rect.w - 8.0, input_h - 6.0, color_u8!(theme.tab_bg.0, theme.tab_bg.1, theme.tab_bg.2, 255));
        let prompt = if self.busy { "…" } else { "❯" };
        draw_text_ex(
            &format!("{prompt} {}", self.input),
            rect.x + 10.0,
            rect.y + rect.h - input_h + font_px as f32 + 2.0,
            TextParams { font: Some(font), font_size: font_px, color: color_u8!(theme.fg.0, theme.fg.1, theme.fg.2, 255), ..Default::default() },
        );
    }

    pub fn scroll_lines(&mut self, delta: i32) {
        if self.scroll == usize::MAX {
            self.scroll = 0;
        }
        self.scroll = (self.scroll as i32 + delta).max(0) as usize;
    }
}

fn request_chat(config: &AiConfig, history: &[(String, String)]) -> Result<String, String> {
    let key = std::env::var(&config.api_key_env).map_err(|_| {
        format!("AI: set ${} (or configure config.ai.api_key_env)", config.api_key_env)
    })?;

    let messages: Vec<serde_json::Value> = history
        .iter()
        .map(|(role, content)| {
            serde_json::json!({ "role": role, "content": content })
        })
        .collect();
    let body = serde_json::json!({
        "model": config.model,
        "messages": messages,
        "stream": false,
    });

    let url = format!("{}/chat/completions", config.base_url.trim_end_matches('/'));
    let resp = ureq::post(&url)
        .set("Authorization", &format!("Bearer {key}"))
        .set("Content-Type", "application/json")
        .timeout(std::time::Duration::from_secs(120))
        .send_string(&body.to_string())
        .map_err(|e| format!("AI request failed: {e}"))?;

    let text = resp.into_string().map_err(|e| format!("AI read failed: {e}"))?;
    let v: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("AI bad response: {e}"))?;
    let content = v["choices"][0]["message"]["content"]
        .as_str()
        .ok_or_else(|| format!("AI: unexpected response shape: {}", &text[..text.len().min(200)]))?;
    Ok(content.trim().to_string())
}
