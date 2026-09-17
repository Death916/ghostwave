//! Named-session picker: the modal list the Session-mode `w` bind opens.
//!
//! The widget owns the list and the selection; every key becomes a
//! [`SessionsMessage`] and every message becomes a [`SessionsAction`], which
//! [`crate::session`] applies to the workspace. Nothing here reads or writes the
//! data dir — the list is handed in by the session module, because a picker that
//! listed the disk itself could not be driven in a test without touching it.

use ghostwave::theme::{Rgb, Theme as GhostTheme};
use iced::alignment::{Horizontal, Vertical};
use iced::keyboard::{key::Named, Key, Modifiers};
use iced::widget::{column, container, mouse_area, scrollable, text, Space};
use iced::{Background, Border, Color, Element, Length, Padding, Shadow, Vector};

/// Width of the modal, in px.
const WIDTH: f32 = 460.0;

/// Height of the modal, in px.
const HEIGHT: f32 = 320.0;

/// Messages the picker's keys and rows produce.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionsMessage {
    /// Moves the selection by one entry; the state clamps it to the list.
    Move(i32),
    /// Selects the entry a click landed on.
    Select(usize),
    /// Switches to the selected session.
    Activate,
    /// Starts a new session under the next free name.
    New,
    /// Renames the selected session.
    Rename,
    /// Deletes the selected session.
    Delete,
    /// Dismisses the picker.
    Close,
}

/// What a message asks the workspace to do; the session module is the one that
/// knows how.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionsAction {
    /// Switch the workspace to the named session.
    Switch(String),
    /// Start a new session.
    New,
    /// Rename the named session.
    Rename(String),
    /// Delete the named session.
    Delete(String),
    /// Close the picker.
    Close,
}

/// The saved sessions, the current one, and the entry the keyboard acts on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionsOverlay {
    /// The session names to list, sorted; the current one is always among them.
    pub entries: Vec<String>,
    /// The session the workspace is running inside, marked in the list.
    pub current: String,
    /// Index into `entries` the keyboard acts on.
    pub selected: usize,
}

impl SessionsOverlay {
    /// A picker over `names`, selecting `current`.
    ///
    /// The current name is added when the disk does not know it yet (a session
    /// created this run), so the list always shows where the user is.
    pub fn new(current: impl Into<String>, names: Vec<String>) -> Self {
        let current = current.into();
        let mut entries = names;
        if !entries.iter().any(|name| name == &current) {
            entries.push(current.clone());
            entries.sort();
        }
        let selected = entries
            .iter()
            .position(|name| name == &current)
            .unwrap_or(0);
        Self {
            entries,
            current,
            selected,
        }
    }

    /// The name the keyboard acts on.
    pub fn selected_name(&self) -> Option<&str> {
        self.entries.get(self.selected).map(String::as_str)
    }

    /// Whether `name` is the session the workspace runs inside.
    pub fn is_current(&self, name: &str) -> bool {
        name == self.current
    }

    /// Applies a message: navigation moves the selection and asks for nothing,
    /// every other message yields the action the workspace performs.
    pub fn update(&mut self, message: SessionsMessage) -> Option<SessionsAction> {
        match message {
            SessionsMessage::Move(delta) => {
                self.step(delta);
                None
            }
            SessionsMessage::Select(index) => {
                if index < self.entries.len() {
                    self.selected = index;
                }
                None
            }
            SessionsMessage::Activate => self
                .selected_name()
                .map(|name| SessionsAction::Switch(name.to_string())),
            SessionsMessage::New => Some(SessionsAction::New),
            SessionsMessage::Rename => self
                .selected_name()
                .map(|name| SessionsAction::Rename(name.to_string())),
            SessionsMessage::Delete => self
                .selected_name()
                .map(|name| SessionsAction::Delete(name.to_string())),
            SessionsMessage::Close => Some(SessionsAction::Close),
        }
    }

    /// Moves the selection by `delta`, clamped to the list.
    pub fn step(&mut self, delta: i32) {
        let last = self.entries.len().saturating_sub(1) as i32;
        self.selected = (self.selected as i32 + delta).clamp(0, last) as usize;
    }

    /// Replaces the list, keeping the selection on the current session.
    pub fn relist(&mut self, names: Vec<String>) {
        let current = self.current.clone();
        *self = Self::new(current, names);
    }
}

/// Translates a key into a picker message while the picker owns the keyboard.
///
/// The binds are the reference manager's, minus its remote-only `k`.
pub fn handle_key(key: &Key, modifiers: Modifiers) -> Option<SessionsMessage> {
    match key.as_ref() {
        Key::Named(Named::Escape) => Some(SessionsMessage::Close),
        Key::Named(Named::ArrowUp) => Some(SessionsMessage::Move(-1)),
        Key::Named(Named::ArrowDown) => Some(SessionsMessage::Move(1)),
        Key::Named(Named::Enter) => Some(SessionsMessage::Activate),
        Key::Character(c) if !modifiers.control() && !modifiers.alt() => {
            match c.to_lowercase().as_str() {
                "n" => Some(SessionsMessage::New),
                "r" => Some(SessionsMessage::Rename),
                "d" => Some(SessionsMessage::Delete),
                _ => None,
            }
        }
        _ => None,
    }
}

/// Converts a theme colour into an iced colour.
fn rgb(c: Rgb) -> Color {
    Color::from_rgb8(c.0, c.1, c.2)
}

/// Builds the modal: the list, the current entry marked, and the key hint.
pub fn view<'a>(overlay: &'a SessionsOverlay, theme: &GhostTheme) -> Element<'a, SessionsMessage> {
    let theme = *theme;
    let font = crate::ui_fonts::primary();
    let mut rows: Vec<Element<'a, SessionsMessage>> = Vec::new();
    for (index, name) in overlay.entries.iter().enumerate() {
        let selected = index == overlay.selected;
        let current = overlay.is_current(name);
        let label = format!("{}{name}", if current { "● " } else { "  " });
        let row = container(row_text(label, font, &theme, selected))
            .width(Length::Fill)
            .padding(Padding::from([4.0, 8.0]))
            .style(move |_| container::Style {
                background: selected.then(|| {
                    Background::Color(Color::from_rgba(1.0, 1.0, 1.0, 0.10))
                }),
                border: Border {
                    radius: 3.0.into(),
                    ..Border::default()
                },
                ..container::Style::default()
            });
        rows.push(
            mouse_area(row)
                .on_press(SessionsMessage::Select(index))
                .into(),
        );
    }

    let hint = if overlay.entries.is_empty() {
        "no sessions saved yet — n starts one".to_string()
    } else {
        "↑/↓ select | Enter switch | n new | r rename | d delete | Esc close".to_string()
    };

    let body = column![
        text(format!("SESSIONS — current: {}", overlay.current))
            .size(13.0)
            .font(font)
            .color(rgb(theme.search_match)),
        scrollable(column(rows).spacing(1.0).width(Length::Fill))
            .width(Length::Fill)
            .height(Length::Fill),
        text(hint).size(12.0).font(font).color(rgb(theme.status_fg)),
    ]
    .spacing(8.0)
    .width(Length::Fill)
    .height(Length::Fill);

    let modal = container(body)
        .width(Length::Fixed(WIDTH))
        .height(Length::Fixed(HEIGHT))
        .padding(Padding::from(12.0))
        .style(move |_| container::Style {
            background: Some(Background::Color(rgb(theme.palette_bg))),
            border: Border {
                color: rgb(theme.frame_active),
                width: 1.5,
                radius: 8.0.into(),
            },
            shadow: Shadow {
                color: Color::from_rgba(0.0, 0.0, 0.0, 0.5),
                offset: Vector::new(0.0, 8.0),
                blur_radius: 20.0,
            },
            ..container::Style::default()
        });

    let backdrop = mouse_area(
        container(Space::new())
            .width(Length::Fill)
            .height(Length::Fill)
            .style(|_| container::Style {
                background: Some(Background::Color(Color::from_rgba(0.0, 0.0, 0.0, 0.65))),
                ..container::Style::default()
            }),
    )
    .on_press(SessionsMessage::Close);

    let centered = container(mouse_area(modal))
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(Horizontal::Center)
        .align_y(Vertical::Center);

    iced::widget::stack![backdrop, centered].into()
}

/// The row's label, highlighted when the entry is the selected one.
fn row_text<'a>(
    label: String,
    font: iced::Font,
    theme: &GhostTheme,
    selected: bool,
) -> iced::widget::Text<'a> {
    let color = if selected {
        theme.search_match
    } else {
        theme.palette_fg
    };
    text(label).size(13.0).font(font).color(rgb(color))
}

/// Builds the modal with a custom message transformation.
pub fn view_mapped<'a, M: 'a>(
    overlay: &'a SessionsOverlay,
    theme: &GhostTheme,
    map: impl Fn(SessionsMessage) -> M + 'a,
) -> Element<'a, M> {
    view(overlay, theme).map(map)
}

#[cfg(test)]
mod tests {
    //! The picker's own behaviour: what a key asks for, and where the selection
    //! ends up. Nothing here touches the workspace or the disk.

    use super::*;
    use iced::keyboard::key::Named;

    fn key(named: Named) -> Key {
        Key::Named(named)
    }

    fn picker() -> SessionsOverlay {
        SessionsOverlay::new(
            "default",
            vec!["default".to_string(), "work".to_string()],
        )
    }

    #[test]
    fn the_current_session_is_listed_and_selected() {
        let overlay = picker();
        assert_eq!(overlay.entries, vec!["default", "work"]);
        assert_eq!(overlay.selected_name(), Some("default"));
        assert!(overlay.is_current("default"));
        assert!(!overlay.is_current("work"));
    }

    #[test]
    fn an_unknown_current_session_is_added_to_the_list() {
        let overlay = SessionsOverlay::new("session-7", vec!["work".to_string()]);
        assert_eq!(overlay.entries, vec!["session-7", "work"]);
        assert_eq!(overlay.selected_name(), Some("session-7"));
    }

    #[test]
    fn keys_ask_for_the_reference_binds() {
        assert_eq!(
            handle_key(&key(Named::Escape), Modifiers::default()),
            Some(SessionsMessage::Close)
        );
        assert_eq!(
            handle_key(&key(Named::Enter), Modifiers::default()),
            Some(SessionsMessage::Activate)
        );
        assert_eq!(
            handle_key(&key(Named::ArrowDown), Modifiers::default()),
            Some(SessionsMessage::Move(1))
        );
        assert_eq!(
            handle_key(&key(Named::ArrowUp), Modifiers::default()),
            Some(SessionsMessage::Move(-1))
        );
        for (c, want) in [
            ("n", SessionsMessage::New),
            ("r", SessionsMessage::Rename),
            ("d", SessionsMessage::Delete),
        ] {
            assert_eq!(handle_key(&Key::Character(c.into()), Modifiers::default()), Some(want));
        }
        assert_eq!(handle_key(&Key::Character("x".into()), Modifiers::default()), None);
        assert_eq!(
            handle_key(&Key::Character("d".into()), Modifiers::CTRL),
            None,
            "a modified key is not a manager bind"
        );
    }

    #[test]
    fn navigation_clamps_and_enter_yields_the_selected_name() {
        let mut overlay = picker();
        assert_eq!(overlay.update(SessionsMessage::Move(-1)), None);
        assert_eq!(overlay.selected, 0, "up at the top stays at the top");
        assert_eq!(overlay.update(SessionsMessage::Move(1)), None);
        assert_eq!(overlay.selected, 1);
        overlay.update(SessionsMessage::Move(1));
        assert_eq!(overlay.selected, 1, "down at the bottom stays at the bottom");
        assert_eq!(
            overlay.update(SessionsMessage::Activate),
            Some(SessionsAction::Switch("work".to_string()))
        );
    }

    #[test]
    fn row_messages_carry_the_selected_name() {
        let mut overlay = picker();
        assert_eq!(overlay.update(SessionsMessage::Select(1)), None);
        assert_eq!(
            overlay.update(SessionsMessage::Delete),
            Some(SessionsAction::Delete("work".to_string()))
        );
        assert_eq!(
            overlay.update(SessionsMessage::Rename),
            Some(SessionsAction::Rename("work".to_string()))
        );
        assert_eq!(overlay.update(SessionsMessage::New), Some(SessionsAction::New));
        assert_eq!(overlay.update(SessionsMessage::Close), Some(SessionsAction::Close));
    }

    #[test]
    fn a_relist_keeps_the_selection_on_the_current_session() {
        let mut overlay = picker();
        overlay.update(SessionsMessage::Select(1));
        overlay.relist(vec!["a".to_string(), "default".to_string()]);
        assert_eq!(overlay.selected_name(), Some("default"));
        assert_eq!(overlay.entries, vec!["a", "default"]);
    }
}
