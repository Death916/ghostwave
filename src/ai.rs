//! AI chat assistant pane for the iced front-end backed by OpenAI-compatible
//! chat completions endpoints.
//!
//! Requests execute on a background worker thread via channels so the UI
//! remains responsive. State updates and chat history are presented through
//! native iced widgets.

use std::sync::mpsc::{channel, Receiver};

use ghostwave::config::AiConfig;
use iced::widget::{button, column, container, row, scrollable, text, text_input, Space};
use iced::{alignment, border, Element, Fill, Padding, Theme};

/// The role of a chat participant in an OpenAI-compatible completion conversation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Role {
    /// End-user prompt.
    User,
    /// Model assistant response.
    Assistant,
    /// System instruction or context prompt.
    System,
}

impl Role {
    /// Returns the string identifier for API serialization.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
            Self::System => "system",
        }
    }
}

/// A single chat message in the conversation history.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Msg {
    /// The participant role.
    pub role: Role,
    /// Message body text.
    pub text: String,
}

impl Msg {
    /// Creates a new message with the given role and text.
    pub fn new(role: Role, text: impl Into<String>) -> Self {
        Self {
            role,
            text: text.into(),
        }
    }

    /// Creates a user message.
    pub fn user(text: impl Into<String>) -> Self {
        Self::new(Role::User, text)
    }

    /// Creates an assistant message.
    pub fn assistant(text: impl Into<String>) -> Self {
        Self::new(Role::Assistant, text)
    }

    /// Creates a system message.
    pub fn system(text: impl Into<String>) -> Self {
        Self::new(Role::System, text)
    }
}

/// Actions emitted by the AI assistant pane widgets.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AiMessage {
    /// Content of the prompt text input changed.
    InputChanged(String),
    /// Prompt submitted for processing.
    Submit,
    /// Reset conversation history to the initial system prompt.
    Clear,
}

/// AI assistant pane state managing conversation history, input prompt, and background worker thread.
#[derive(Debug)]
pub struct AiPane {
    /// Full message history including system prompt, user queries, and assistant replies.
    pub msgs: Vec<Msg>,
    /// Current content of the input text box.
    pub input: String,
    /// Whether a request is currently being processed by the worker thread.
    pub busy: bool,
    /// Line scroll offset (retained for scrollback operations and compatibility).
    pub scroll: usize,
    /// Maximum scroll offset calculated from line wrapping.
    pub max_scroll: usize,
    /// Most recent error message, if any.
    pub error: Option<String>,
    /// Receiver channel for results from the background worker thread.
    rx: Option<Receiver<Result<String, String>>>,
    /// Endpoint and model settings for chat completions.
    config: AiConfig,
}

impl AiPane {
    /// Creates a new AI pane with the given configuration.
    pub fn new(config: AiConfig) -> Self {
        Self {
            msgs: vec![Msg::system(config.system_prompt.clone())],
            input: String::new(),
            busy: false,
            scroll: 0,
            max_scroll: 0,
            error: None,
            rx: None,
            config,
        }
    }

    /// Formats the conversation history into `(role, content)` pairs suitable for API requests.
    pub fn history(&self) -> Vec<(String, String)> {
        self.msgs
            .iter()
            .map(|m| (m.role.as_str().to_string(), m.text.clone()))
            .collect()
    }

    /// Submits the current input prompt to the background worker thread.
    pub fn send(&mut self) {
        let prompt = self.input.trim().to_string();
        if prompt.is_empty() || self.busy {
            return;
        }
        self.msgs.push(Msg::user(prompt));
        self.input.clear();
        self.busy = true;
        self.error = None;

        let (tx, rx) = channel();
        self.rx = Some(rx);

        let config = self.config.clone();
        let history = self.history();

        std::thread::spawn(move || {
            let result = crate::ai::request_chat(&config, &history);
            let _ = tx.send(result);
        });
    }

    /// Ingests a response from the worker thread, updating messages and state.
    pub fn handle_response(&mut self, result: Result<String, String>) {
        self.busy = false;
        match result {
            Ok(text) => {
                self.msgs.push(Msg::assistant(text));
                self.scroll = usize::MAX;
                self.error = None;
            }
            Err(e) => {
                self.error = Some(e);
            }
        }
    }

    /// Drains a completed response from the worker thread, if available.
    pub fn poll(&mut self) {
        if let Some(rx) = &self.rx {
            if let Ok(result) = rx.try_recv() {
                self.handle_response(result);
            }
        }
    }

    /// Resets the conversation history back to the initial system prompt and clears errors.
    pub fn clear(&mut self) {
        self.msgs = vec![Msg::system(self.config.system_prompt.clone())];
        self.error = None;
    }

    /// Appends a single character to the input prompt.
    pub fn type_char(&mut self, c: char) {
        self.input.push(c);
    }

    /// Removes the last character from the input prompt.
    pub fn backspace(&mut self) {
        self.input.pop();
    }

    /// Adjusts line scroll offset by delta, respecting bottom pin (`usize::MAX`).
    pub fn scroll_lines(&mut self, delta: i32) {
        let base = if self.scroll == usize::MAX {
            self.max_scroll
        } else {
            self.scroll
        };
        let new_scroll = (base as i32 + delta).max(0) as usize;
        self.scroll = if new_scroll >= self.max_scroll {
            usize::MAX
        } else {
            new_scroll
        };
    }

    /// Dispatches an [`AiMessage`] action to mutate pane state.
    pub fn update(&mut self, message: AiMessage) {
        match message {
            AiMessage::InputChanged(text) => {
                self.input = text;
            }
            AiMessage::Submit => {
                self.send();
            }
            AiMessage::Clear => {
                self.clear();
            }
        }
    }

    /// Returns a reference to the active configuration.
    pub fn config(&self) -> &AiConfig {
        &self.config
    }

    /// Attaches a receiver channel directly (used for testing or custom streams).
    #[cfg(test)]
    pub fn set_rx(&mut self, rx: Receiver<Result<String, String>>) {
        self.rx = Some(rx);
    }

    /// Renders the AI assistant pane widget tree.
    pub fn view(&self, theme: &Theme) -> Element<'_, AiMessage> {
        let palette = theme.extended_palette();

        let header = row![
            text("Wave AI").size(15).color(palette.primary.base.color),
            Space::new().width(Fill),
            text(&self.config.model)
                .size(11)
                .color(palette.background.weak.text),
            button(text("Clear").size(11))
                .padding(Padding::from([3, 8]))
                .on_press(AiMessage::Clear),
        ]
        .spacing(8)
        .align_y(alignment::Vertical::Center)
        .padding(Padding::from([2, 4]));

        let mut message_list = column![].spacing(10).width(Fill);

        for msg in &self.msgs {
            match msg.role {
                Role::User => {
                    let card = container(
                        column![
                            text("You").size(11).color(palette.primary.base.color),
                            text(&msg.text).size(13).color(palette.background.base.text),
                        ]
                        .spacing(4),
                    )
                    .padding(Padding::from([8, 12]))
                    .max_width(640.0)
                    .style(|theme: &Theme| {
                        let p = theme.extended_palette();
                        container::Style::default()
                            .background(p.background.weak.color)
                            .color(p.background.base.text)
                            .border(border::rounded(8).width(1.0).color(p.primary.base.color))
                    });

                    message_list = message_list.push(
                        container(card)
                            .width(Fill)
                            .align_right(Fill),
                    );
                }
                Role::Assistant => {
                    let card = container(
                        column![
                            row![
                                text("Wave AI").size(11).color(palette.success.base.color),
                                text(format!("({})", self.config.model))
                                    .size(10)
                                    .color(palette.background.weak.text),
                            ]
                            .spacing(6)
                            .align_y(alignment::Vertical::Center),
                            text(&msg.text).size(13).color(palette.background.base.text),
                        ]
                        .spacing(4),
                    )
                    .padding(Padding::from([8, 12]))
                    .max_width(720.0)
                    .style(|theme: &Theme| {
                        let p = theme.extended_palette();
                        container::Style::default()
                            .background(p.background.weak.color)
                            .color(p.background.base.text)
                            .border(border::rounded(8).width(1.0).color(p.background.strong.color))
                    });

                    message_list = message_list.push(
                        container(card)
                            .width(Fill)
                            .align_left(Fill),
                    );
                }
                Role::System => {
                    let card = container(
                        column![
                            text("System Prompt").size(10).color(palette.secondary.base.color),
                            text(&msg.text).size(11).color(palette.background.weak.text),
                        ]
                        .spacing(2),
                    )
                    .padding(Padding::from([6, 10]))
                    .width(Fill)
                    .style(|theme: &Theme| {
                        let p = theme.extended_palette();
                        container::Style::default()
                            .background(p.background.weakest.color)
                            .color(p.background.weak.text)
                            .border(border::rounded(6).width(1.0).color(p.background.weak.color))
                    });

                    message_list = message_list.push(card);
                }
            }
        }

        if self.busy {
            let thinking_card = container(
                row![
                    text("Wave AI is thinking…").size(12).color(palette.primary.base.color),
                ]
                .spacing(6)
                .align_y(alignment::Vertical::Center),
            )
            .padding(Padding::from([8, 12]))
            .style(|theme: &Theme| {
                let p = theme.extended_palette();
                container::Style::default()
                    .background(p.background.weak.color)
                    .border(border::rounded(8).width(1.0).color(p.primary.weak.color))
            });

            message_list = message_list.push(
                container(thinking_card)
                    .width(Fill)
                    .align_left(Fill),
            );
        }

        let history_scrollable = scrollable(
            container(message_list)
                .width(Fill)
                .padding(Padding::from([4, 8])),
        )
        .height(Fill)
        .width(Fill);

        let mut content = column![header, history_scrollable]
            .spacing(8)
            .width(Fill)
            .height(Fill);

        if let Some(err) = &self.error {
            let error_card = container(
                text(format!("Error: {err}"))
                    .size(12)
                    .color(palette.danger.base.color),
            )
            .padding(Padding::from([4, 8]))
            .width(Fill)
            .style(|theme: &Theme| {
                let p = theme.extended_palette();
                container::Style::default()
                    .background(p.background.weak.color)
                    .border(border::rounded(4).width(1.0).color(p.danger.base.color))
            });
            content = content.push(error_card);
        }

        let status_row = if self.busy {
            row![
                text("Generating response…")
                    .size(11)
                    .color(palette.primary.base.color),
                Space::new().width(Fill),
                text(&self.config.base_url)
                    .size(11)
                    .color(palette.background.weak.text),
            ]
        } else {
            row![
                text("Ready")
                    .size(11)
                    .color(palette.background.weak.text),
                Space::new().width(Fill),
                text(format!("{}: {}", self.config.model, self.config.base_url))
                    .size(11)
                    .color(palette.background.weak.text),
            ]
        }
        .align_y(alignment::Vertical::Center)
        .padding(Padding::from([1, 4]));

        content = content.push(status_row);

        let can_submit = !self.busy && !self.input.trim().is_empty();

        let input_box = text_input("Ask a question or enter a prompt…", &self.input)
            .on_input(AiMessage::InputChanged)
            .on_submit_maybe(can_submit.then_some(AiMessage::Submit))
            .padding(Padding::from([8, 12]))
            .width(Fill);

        let send_btn = button(
            text(if self.busy { "Thinking…" } else { "Send" }).size(13),
        )
        .padding(Padding::from([8, 16]))
        .on_press_maybe(can_submit.then_some(AiMessage::Submit));

        let input_row = row![input_box, send_btn]
            .spacing(8)
            .align_y(alignment::Vertical::Center);

        content = content.push(input_row);

        container(content)
            .padding(8)
            .width(Fill)
            .height(Fill)
            .style(|theme: &Theme| {
                let p = theme.extended_palette();
                container::Style::default()
                    .background(p.background.base.color)
                    .color(p.background.base.text)
            })
            .into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> AiConfig {
        AiConfig {
            base_url: "http://localhost:8080/v1".into(),
            model: "test-model".into(),
            api_key_env: "TEST_API_KEY".into(),
            system_prompt: "You are a helpful assistant.".into(),
        }
    }

    #[test]
    fn message_construction_and_roles() {
        let user_msg = Msg::user("hello");
        assert_eq!(user_msg.role, Role::User);
        assert_eq!(user_msg.role.as_str(), "user");
        assert_eq!(user_msg.text, "hello");

        let assistant_msg = Msg::assistant("hi there");
        assert_eq!(assistant_msg.role, Role::Assistant);
        assert_eq!(assistant_msg.role.as_str(), "assistant");
        assert_eq!(assistant_msg.text, "hi there");

        let system_msg = Msg::system("system instruction");
        assert_eq!(system_msg.role, Role::System);
        assert_eq!(system_msg.role.as_str(), "system");
        assert_eq!(system_msg.text, "system instruction");

        assert_ne!(user_msg, assistant_msg);
        assert_eq!(Msg::new(Role::User, "hello"), user_msg);
    }

    #[test]
    fn history_formatting() {
        let mut pane = AiPane::new(test_config());
        let initial_history = pane.history();
        assert_eq!(initial_history.len(), 1);
        assert_eq!(
            initial_history[0],
            (
                "system".to_string(),
                "You are a helpful assistant.".to_string()
            )
        );

        pane.msgs.push(Msg::user("write a test"));
        pane.msgs.push(Msg::assistant("here is a test"));

        let formatted = pane.history();
        assert_eq!(formatted.len(), 3);
        assert_eq!(
            formatted[0],
            (
                "system".to_string(),
                "You are a helpful assistant.".to_string()
            )
        );
        assert_eq!(
            formatted[1],
            ("user".to_string(), "write a test".to_string())
        );
        assert_eq!(
            formatted[2],
            ("assistant".to_string(), "here is a test".to_string())
        );
    }

    #[test]
    fn input_submission_non_empty() {
        let mut pane = AiPane::new(test_config());
        pane.input = "  list files  ".into();
        pane.send();

        assert_eq!(pane.msgs.len(), 2);
        assert_eq!(pane.msgs[0].role, Role::System);
        assert_eq!(pane.msgs[1].role, Role::User);
        assert_eq!(pane.msgs[1].text, "list files");
        assert!(pane.input.is_empty());
        assert!(pane.busy);
        assert!(pane.error.is_none());
    }

    #[test]
    fn input_submission_ignores_empty_and_whitespace() {
        let mut pane = AiPane::new(test_config());

        pane.input = String::new();
        pane.send();
        assert_eq!(pane.msgs.len(), 1);
        assert!(!pane.busy);

        pane.input = "   \t\n  ".into();
        pane.send();
        assert_eq!(pane.msgs.len(), 1);
        assert!(!pane.busy);
    }

    #[test]
    fn input_submission_blocked_while_busy() {
        let mut pane = AiPane::new(test_config());
        pane.busy = true;
        pane.input = "second prompt".into();
        pane.send();

        assert_eq!(pane.msgs.len(), 1);
        assert_eq!(pane.input, "second prompt");
    }

    #[test]
    fn response_handling_success() {
        let mut pane = AiPane::new(test_config());
        pane.busy = true;
        pane.error = Some("previous error".into());

        pane.handle_response(Ok("completed answer".into()));

        assert!(!pane.busy);
        assert!(pane.error.is_none());
        assert_eq!(pane.msgs.len(), 2);
        assert_eq!(pane.msgs[1].role, Role::Assistant);
        assert_eq!(pane.msgs[1].text, "completed answer");
        assert_eq!(pane.scroll, usize::MAX);
    }

    #[test]
    fn response_handling_error() {
        let mut pane = AiPane::new(test_config());
        pane.busy = true;

        pane.handle_response(Err("connection timeout".into()));

        assert!(!pane.busy);
        assert_eq!(pane.error.as_deref(), Some("connection timeout"));
        assert_eq!(pane.msgs.len(), 1);
    }

    #[test]
    fn poll_drains_channel_response() {
        let mut pane = AiPane::new(test_config());
        let (tx, rx) = channel();
        pane.set_rx(rx);
        pane.busy = true;

        tx.send(Ok("response via channel".into())).unwrap();
        pane.poll();

        assert!(!pane.busy);
        assert_eq!(pane.msgs.len(), 2);
        assert_eq!(pane.msgs[1].text, "response via channel");
    }

    #[test]
    fn poll_drains_channel_error() {
        let mut pane = AiPane::new(test_config());
        let (tx, rx) = channel();
        pane.set_rx(rx);
        pane.busy = true;

        tx.send(Err("API key invalid".into())).unwrap();
        pane.poll();

        assert!(!pane.busy);
        assert_eq!(pane.error.as_deref(), Some("API key invalid"));
    }

    #[test]
    fn update_actions() {
        let mut pane = AiPane::new(test_config());

        pane.update(AiMessage::InputChanged("new query".into()));
        assert_eq!(pane.input, "new query");

        pane.update(AiMessage::Submit);
        assert_eq!(pane.msgs.len(), 2);
        assert_eq!(pane.msgs[1].text, "new query");
        assert!(pane.input.is_empty());

        pane.error = Some("temp error".into());
        pane.update(AiMessage::Clear);
        assert_eq!(pane.msgs.len(), 1);
        assert_eq!(pane.msgs[0].role, Role::System);
        assert!(pane.error.is_none());
    }

    #[test]
    fn scroll_lines_preserves_position_from_bottom() {
        let mut pane = AiPane::new(test_config());
        pane.max_scroll = 50;
        pane.scroll = usize::MAX;

        pane.scroll_lines(-5);
        assert_eq!(pane.scroll, 45);

        pane.scroll_lines(5);
        assert_eq!(pane.scroll, usize::MAX);

        pane.scroll = 10;
        pane.scroll_lines(-20);
        assert_eq!(pane.scroll, 0);
    }

    #[test]
    fn typing_and_backspace() {
        let mut pane = AiPane::new(test_config());
        pane.type_char('a');
        pane.type_char('b');
        pane.type_char('c');
        assert_eq!(pane.input, "abc");

        pane.backspace();
        assert_eq!(pane.input, "ab");
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
