//! OSC 7501 inbox: what every pane's programs say they are doing.
//!
//! Three surfaces read this module:
//!
//! - [`StatusChip`] is the one-line summary a pane's title band and a tab's
//!   label show, so a state is visible without focusing the pane.
//! - [`StatusInbox`] is the list the user opens: every live record, with the
//!   terminal it came from.
//! - [`state_color`] is the palette both use.
//!
//! # Untrusted text
//!
//! `msg` and `title` come from a program that already controls its own screen.
//! [`StatusRow::from_record`] runs them through
//! [`ghostwave::program_status::sanitize`], which replaces the bidirectional
//! overrides and invisible formatting that would let a message reorder or hide
//! the text around it, and both are capped in length here. Everything is drawn
//! with [`iced::widget::text`], which paints glyphs and never interprets markup.
//!
//! Nothing in this module writes to a program, plays a sound or raises a
//! desktop notification: a record's only effect is text in ghostwave's own
//! chrome, so there is nothing for a rate limit to protect.

use ghostwave::program_status::{Kind, Record, State, Table};
use ghostwave::theme::{Rgb, Theme as GhostTheme};
use iced::alignment::{Horizontal, Vertical};
use iced::keyboard::{Key, Modifiers, key::Named};
use iced::widget::{column, container, mouse_area, row, scrollable, text, Space};
use iced::{Background, Border, Color, Element, Length, Padding, Shadow, Vector};

use crate::pane::PaneId;

/// Width of the inbox modal, in px.
const WIDTH: f32 = 720.0;

/// Height of the inbox modal, in px.
const HEIGHT: f32 = 420.0;

/// Longest `msg` shown, in characters. The spec lets a terminal shorten it.
const MSG_CHARS: usize = 240;

/// Longest `title` shown, in characters.
const TITLE_CHARS: usize = 80;

/// Most rows the inbox builds; the rest are reported as a count.
pub const MAX_ROWS: usize = 200;

/// The one-line state a pane's chrome shows.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StatusChip {
    pub state: State,
    pub kind: Option<Kind>,
    pub progress: Option<u8>,
}

impl StatusChip {
    /// From a record, taking the fields a chip has room for.
    pub fn from_record(record: &Record) -> Self {
        Self {
            state: record.state,
            kind: record.kind,
            progress: record.progress,
        }
    }

    /// The chip as text: a glyph, the state, what a `blocked` record waits for,
    /// and progress when the program reported one.
    pub fn label(self) -> String {
        let mut out = format!("{} {}", self.state.glyph(), self.state.as_str());
        if let Some(kind) = self.kind {
            out.push_str(&format!(" ({})", kind.as_str()));
        }
        if let Some(progress) = self.progress {
            out.push_str(&format!(" {progress}%"));
        }
        out
    }
}

/// One live record, ready to draw.
#[derive(Clone, Debug, PartialEq)]
pub struct StatusRow {
    /// The terminal the record came from, named by tab and pane.
    pub source: String,
    /// The pane's identity, for "go to the program that said this".
    pub pane: Option<PaneId>,
    /// The record's id; empty for the root record. Ids are `[A-Za-z0-9_.+-]`
    /// only, so they need no disarming.
    pub id: String,
    pub chip: StatusChip,
    /// The `app` to group by, inherited from the nearest ancestor when the
    /// record's own report did not carry one. Disarmed and capped.
    pub app: Option<String>,
    pub title: Option<String>,
    pub msg: Option<String>,
    /// When the record was last updated on its pane; the list is ordered by it,
    /// it is never shown.
    pub updated: u64,
}

impl StatusRow {
    /// A row for `record`, taken from `table` so `app` can be inherited.
    ///
    /// `msg` and `title` are disarmed and shortened; nothing else is touched.
    pub fn from_record(source: String, pane: Option<PaneId>, table: &Table, record: &Record) -> Self {
        Self {
            source,
            pane,
            id: record.id.clone(),
            chip: StatusChip::from_record(record),
            app: table
                .app_of(&record.id)
                .map(|app| ghostwave::program_status::sanitize(app)),
            title: record
                .title
                .as_deref()
                .map(|text| shorten(&ghostwave::program_status::sanitize(text), TITLE_CHARS)),
            msg: record
                .msg
                .as_deref()
                .map(|text| shorten(&ghostwave::program_status::sanitize(text), MSG_CHARS)),
            updated: record.updated,
        }
    }

    /// What the row says before the source, for tests and for the pane's own
    /// chrome: `app — title — msg`, with the pieces a record does not have left
    /// out.
    pub fn summary(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if let Some(app) = &self.app {
            parts.push(app.clone());
        }
        if let Some(title) = &self.title {
            parts.push(title.clone());
        }
        if let Some(msg) = &self.msg {
            parts.push(msg.clone());
        }
        parts.join(" — ")
    }

    /// The row's label: the state chip, then the id when it has one, then the
    /// summary, then where it came from.
    pub fn label(&self) -> String {
        let mut out = self.chip.label();
        if !self.id.is_empty() {
            out.push_str(&format!("  {}", self.id));
        }
        let summary = self.summary();
        if !summary.is_empty() {
            out.push_str(&format!("  {summary}"));
        }
        out.push_str(&format!("   [{}]", self.source));
        out
    }
}

/// Caps `text` at `chars` characters, marking the cut.
fn shorten(text: &str, chars: usize) -> String {
    if text.chars().count() <= chars {
        return text.to_string();
    }
    let mut out: String = text.chars().take(chars).collect();
    out.push('…');
    out
}

/// Messages the inbox's keys and rows produce.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StatusMessage {
    /// Moves the selection by one row.
    Move(i32),
    /// Selects the row a click landed on.
    Select(usize),
    /// Goes to the pane the selected row came from.
    Focus,
    /// Dismisses the inbox.
    Close,
}

/// What a message asks the workspace to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StatusAction {
    /// Focus the pane a record came from.
    Focus(PaneId),
    /// Close the inbox.
    Close,
}

/// The inbox: the rows built from the live panes and the row the keyboard acts
/// on. The rows are rebuilt every frame, because a program changes state as
/// fast as it can write; only the selection is kept.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StatusInbox {
    pub rows: Vec<StatusRow>,
    pub selected: usize,
    /// Rows the pane tables held beyond [`MAX_ROWS`].
    pub hidden: usize,
    /// Records evicted to stay within a pane's capacity, summed over panes.
    pub evicted: u64,
}

impl StatusInbox {
    /// The row the keyboard acts on.
    pub fn selected_row(&self) -> Option<&StatusRow> {
        self.rows.get(self.selected)
    }

    /// Replaces the rows, keeping the selection inside the new list.
    pub fn relist(&mut self, rows: Vec<StatusRow>, hidden: usize, evicted: u64) {
        self.rows = rows;
        self.hidden = hidden;
        self.evicted = evicted;
        if self.selected >= self.rows.len() {
            self.selected = self.rows.len().saturating_sub(1);
        }
    }

    /// Applies a message: navigation moves the selection, the rest name the
    /// row's pane or ask to close.
    pub fn update(&mut self, message: StatusMessage) -> Option<StatusAction> {
        match message {
            StatusMessage::Move(delta) => {
                self.step(delta);
                None
            }
            StatusMessage::Select(index) => {
                if index < self.rows.len() {
                    self.selected = index;
                }
                None
            }
            StatusMessage::Focus => self
                .selected_row()
                .and_then(|row| row.pane)
                .map(StatusAction::Focus),
            StatusMessage::Close => Some(StatusAction::Close),
        }
    }

    /// Moves the selection by `delta`, clamped to the list.
    pub fn step(&mut self, delta: i32) {
        let last = self.rows.len().saturating_sub(1) as i32;
        self.selected = (self.selected as i32 + delta).clamp(0, last) as usize;
    }
}

/// Translates a key into an inbox message while the inbox owns the keyboard.
pub fn handle_key(key: &Key, modifiers: Modifiers) -> Option<StatusMessage> {
    match key.as_ref() {
        Key::Named(Named::Escape) => Some(StatusMessage::Close),
        Key::Named(Named::ArrowUp) => Some(StatusMessage::Move(-1)),
        Key::Named(Named::ArrowDown) => Some(StatusMessage::Move(1)),
        Key::Named(Named::Enter) => Some(StatusMessage::Focus),
        Key::Character(c) if !modifiers.control() && !modifiers.alt() => {
            match c.to_lowercase().as_str() {
                "j" => Some(StatusMessage::Move(1)),
                "k" => Some(StatusMessage::Move(-1)),
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

/// The colour a state is drawn in: fixed colours for the states that mean
/// something to a human (blocked, error, done), the theme's accent while work is
/// running, and the theme's muted text for idle.
pub fn state_color(state: State, theme: &GhostTheme) -> Color {
    match state {
        State::Blocked => rgb(Rgb(0xe0, 0xaf, 0x68)),
        State::Error => rgb(Rgb(0xf7, 0x76, 0x8e)),
        State::Working => rgb(theme.frame_active),
        State::Done => rgb(Rgb(0x9e, 0xce, 0x6a)),
        State::Idle => rgb(theme.tab_inactive_text),
    }
}

/// Builds the modal: the records, blocked first, each saying where it came from.
pub fn view<'a>(inbox: &'a StatusInbox, theme: &GhostTheme) -> Element<'a, StatusMessage> {
    let theme = *theme;
    let font = crate::ui_fonts::primary();
    let mut rows: Vec<Element<'a, StatusMessage>> = Vec::new();
    for (index, entry) in inbox.rows.iter().enumerate() {
        let selected = index == inbox.selected;
        let color = state_color(entry.chip.state, &theme);
        let body = row![
            text(entry.chip.label())
                .size(12.0)
                .font(font)
                .color(color)
                .width(Length::Fixed(180.0)),
            text(entry.summary())
                .size(12.0)
                .font(font)
                .color(rgb(theme.palette_fg))
                .width(Length::Fill),
            text(format!("[{}]", entry.source))
                .size(11.0)
                .font(font)
                .color(rgb(theme.tab_inactive_text))
                .width(Length::Shrink),
        ]
        .spacing(8.0)
        .align_y(Vertical::Center);

        let line = container(body)
            .width(Length::Fill)
            .padding(Padding::from([3.0, 6.0]))
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
            mouse_area(line)
                .on_press(StatusMessage::Select(index))
                .into(),
        );
    }

    let title = if inbox.rows.is_empty() {
        "PROGRAM STATUS — nothing reported".to_string()
    } else {
        let mut title = format!("PROGRAM STATUS — {} record(s)", inbox.rows.len());
        if inbox.hidden > 0 {
            title.push_str(&format!(" and {} more not shown", inbox.hidden));
        }
        if inbox.evicted > 0 {
            title.push_str(&format!(" — {} evicted at capacity", inbox.evicted));
        }
        title
    };

    let hint =
        "↑/↓ select | Enter go to the terminal | Esc close — text is shown as the program sent it";

    let body = column![
        text(title).size(13.0).font(font).color(rgb(theme.search_match)),
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
    .on_press(StatusMessage::Close);

    let centered = container(mouse_area(modal))
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(Horizontal::Center)
        .align_y(Vertical::Center);

    iced::widget::stack![backdrop, centered].into()
}

/// Builds the modal with a custom message transformation.
pub fn view_mapped<'a, M: 'a>(
    inbox: &'a StatusInbox,
    theme: &GhostTheme,
    map: impl Fn(StatusMessage) -> M + 'a,
) -> Element<'a, M> {
    view(inbox, theme).map(map)
}

#[cfg(test)]
mod tests {
    //! The inbox's own behaviour: what a key asks for, what a row says, and
    //! that untrusted text is disarmed before it is drawn.

    use super::*;
    use ghostwave::program_status::Table;

    fn table_with(body: &[&str]) -> Table {
        let mut table = Table::new();
        for report in body {
            table.apply(report.as_bytes());
        }
        table
    }

    #[test]
    fn a_chip_names_the_state_kind_and_progress() {
        let table = table_with(&["state=blocked:kind=permission:progress=10"]);
        let chip = StatusChip::from_record(table.get(Table::ROOT).unwrap());
        assert_eq!(chip.label(), "▲ blocked (permission) 10%");

        let table = table_with(&["state=working:progress=40:app=brew"]);
        let chip = StatusChip::from_record(table.get(Table::ROOT).unwrap());
        assert_eq!(chip.label(), "● working 40%");

        let table = table_with(&["state=done:app=brew"]);
        let chip = StatusChip::from_record(table.get(Table::ROOT).unwrap());
        assert_eq!(chip.label(), "▣ done");
    }

    #[test]
    fn a_row_says_where_it_came_from_and_inherits_app() {
        let table = table_with(&[
            "state=working:app=deploy",
            "state=blocked:kind=permission:id=eu-west:title=RVUgV2VzdA==:msg=QXBwcm92ZSBkZXBsb3k=",
        ]);
        let record = table.get("eu-west").unwrap();
        let row = StatusRow::from_record("Tab 2 / pane 1".to_string(), Some(PaneId(7)), &table, record);
        assert_eq!(row.app.as_deref(), Some("deploy"), "inherited from the root");
        assert_eq!(row.title.as_deref(), Some("EU West"));
        assert_eq!(row.msg.as_deref(), Some("Approve deploy"));
        assert_eq!(row.source, "Tab 2 / pane 1");
        assert_eq!(row.pane, Some(PaneId(7)));
        let label = row.label();
        assert!(label.starts_with("▲ blocked (permission)"), "{label}");
        assert!(label.contains("eu-west"), "{label}");
        assert!(label.contains("deploy — EU West — Approve deploy"), "{label}");
        assert!(label.ends_with("[Tab 2 / pane 1]"), "{label}");
    }

    #[test]
    fn a_row_disarms_bidi_text_from_a_record() {
        // "safe\u{202e}gnp.exe" — the override would reorder the line it is
        // drawn in, so it never reaches the widget.
        let table = table_with(&["state=blocked:msg=c2FmZSLuZ25wLmV4ZQ=="]);
        let record = table.get(Table::ROOT).unwrap();
        assert!(
            record.msg.as_deref().unwrap().contains('\u{202e}'),
            "the record keeps what the program sent"
        );
        let row = StatusRow::from_record("Tab 1 / pane 0".to_string(), None, &table, record);
        let msg = row.msg.as_deref().unwrap();
        assert!(!msg.contains('\u{202e}'), "the row does not: {msg:?}");
        assert!(msg.starts_with("safe"));
    }

    #[test]
    fn a_long_message_is_shortened() {
        let long = "m".repeat(400);
        let mut table = Table::new();
        table.apply(format!("state=working:msg={}", base64(&long)).as_bytes());
        let record = table.get(Table::ROOT).unwrap();
        let row = StatusRow::from_record("Tab 1".to_string(), None, &table, record);
        let msg = row.msg.as_deref().unwrap();
        assert_eq!(msg.chars().count(), MSG_CHARS + 1, "capped and marked");
        assert!(msg.ends_with('…'));
    }

    #[test]
    fn navigation_clamps_and_enter_yields_the_pane() {
        let mut inbox = StatusInbox::default();
        let table = table_with(&["state=working"]);
        let record = table.get(Table::ROOT).unwrap();
        inbox.relist(
            vec![
                StatusRow::from_record("Tab 1".to_string(), Some(PaneId(1)), &table, record),
                StatusRow::from_record("Tab 2".to_string(), Some(PaneId(2)), &table, record),
            ],
            0,
            0,
        );
        assert_eq!(inbox.update(StatusMessage::Move(-1)), None);
        assert_eq!(inbox.selected, 0);
        inbox.update(StatusMessage::Move(1));
        assert_eq!(inbox.selected, 1);
        inbox.update(StatusMessage::Move(1));
        assert_eq!(inbox.selected, 1, "clamped at the end");
        assert_eq!(
            inbox.update(StatusMessage::Focus),
            Some(StatusAction::Focus(PaneId(2)))
        );
        assert_eq!(inbox.update(StatusMessage::Close), Some(StatusAction::Close));
    }

    #[test]
    fn relist_keeps_the_selection_inside_the_list() {
        let table = table_with(&["state=working"]);
        let record = table.get(Table::ROOT).unwrap();
        let row = |source: &str| StatusRow::from_record(source.to_string(), None, &table, record);
        let mut inbox = StatusInbox::default();
        inbox.relist(vec![row("a"), row("b"), row("c")], 0, 0);
        inbox.update(StatusMessage::Select(2));
        inbox.relist(vec![row("a")], 0, 0);
        assert_eq!(inbox.selected, 0, "the selection follows the shorter list");
        inbox.relist(Vec::new(), 5, 3);
        assert!(inbox.selected_row().is_none());
        assert_eq!(inbox.hidden, 5);
        assert_eq!(inbox.evicted, 3);
        assert_eq!(inbox.update(StatusMessage::Focus), None, "nothing to focus");
    }

    #[test]
    fn keys_are_the_inbox_binds() {
        assert_eq!(
            handle_key(&Key::Named(Named::Escape), Modifiers::default()),
            Some(StatusMessage::Close)
        );
        assert_eq!(
            handle_key(&Key::Named(Named::Enter), Modifiers::default()),
            Some(StatusMessage::Focus)
        );
        assert_eq!(
            handle_key(&Key::Character("j".into()), Modifiers::default()),
            Some(StatusMessage::Move(1))
        );
        assert_eq!(
            handle_key(&Key::Character("k".into()), Modifiers::default()),
            Some(StatusMessage::Move(-1))
        );
        assert_eq!(
            handle_key(&Key::Character("j".into()), Modifiers::CTRL),
            None,
            "a modified key is not an inbox bind"
        );
    }

    #[test]
    fn related_states_have_distinct_colours() {
        let theme = ghostwave::theme::by_name("tokyonight");
        let colors: Vec<[u8; 4]> = [
            State::Blocked,
            State::Error,
            State::Working,
            State::Done,
            State::Idle,
        ]
        .iter()
        .map(|state| {
            let c = state_color(*state, &theme);
            [c.r, c.g, c.b, c.a].map(|v| (v * 255.0).round() as u8)
        })
        .collect();
        let mut unique = colors.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), colors.len(), "each state its own colour");
    }

    fn base64(input: &str) -> String {
        const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let bytes = input.as_bytes();
        let mut out = String::new();
        for chunk in bytes.chunks(3) {
            let b = [
                chunk[0],
                *chunk.get(1).unwrap_or(&0),
                *chunk.get(2).unwrap_or(&0),
            ];
            let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
            out.push(ALPHABET[(n >> 18) as usize & 63] as char);
            out.push(ALPHABET[(n >> 12) as usize & 63] as char);
            out.push(if chunk.len() > 1 {
                ALPHABET[(n >> 6) as usize & 63] as char
            } else {
                '='
            });
            out.push(if chunk.len() > 2 {
                ALPHABET[n as usize & 63] as char
            } else {
                '='
            });
        }
        out
    }
}
