//! Scrollback search bar and overlay widget for the iced front-end.
//!
//! Provides a compact search overlay with query input, match navigation,
//! regular-expression and case-sensitivity toggles, match counters, and
//! synchronization with the terminal pane search state.

use ghostwave::theme::{Rgb, Theme as GhostTheme};
use iced::alignment;
use iced::keyboard::{key::Named, Key, Modifiers};
use iced::widget::{button, container, text, text_input, Row};
use iced::{
    Background, Border, Color, Element, Length, Padding, Shadow, Vector,
};

/// Messages produced by the search overlay widget.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SearchMessage {
    /// The search query text input changed.
    QueryChanged(String),
    /// Toggles case-sensitivity for matching.
    ToggleCaseSensitive,
    /// Toggles regular-expression matching mode.
    ToggleRegex,
    /// Navigates to the previous match in the scrollback.
    PrevMatch,
    /// Navigates to the next match in the scrollback.
    NextMatch,
    /// Closes and dismisses the search overlay.
    Close,
}

/// State for scrollback search query, match navigation, and search options.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchState {
    /// The active search query string.
    pub query: String,
    /// 0-based index of the currently focused match.
    pub match_index: usize,
    /// Total number of matches discovered.
    pub total_matches: usize,
    /// Whether regular-expression matching is active.
    pub regex: bool,
    /// Whether case-sensitive matching is active.
    pub case_sensitive: bool,
}

impl Default for SearchState {
    fn default() -> Self {
        Self {
            query: String::new(),
            match_index: 0,
            total_matches: 0,
            regex: false,
            case_sensitive: false,
        }
    }
}

impl SearchState {
    /// Creates a new inactive search state with empty query and zero matches.
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a search state pre-populated with an initial query string.
    pub fn with_query(query: impl Into<String>) -> Self {
        Self {
            query: query.into(),
            ..Self::default()
        }
    }

    /// Sets the search query string.
    ///
    /// If the new query differs from the current query, resets the match index to 0.
    pub fn set_query(&mut self, query: impl Into<String>) {
        let q = query.into();
        if self.query != q {
            self.query = q;
            self.match_index = 0;
        }
    }

    /// Toggles the regular-expression search flag.
    pub fn toggle_regex(&mut self) {
        self.regex = !self.regex;
    }

    /// Toggles the case-sensitive search flag.
    pub fn toggle_case_sensitive(&mut self) {
        self.case_sensitive = !self.case_sensitive;
    }

    /// Sets the current match index, clamping to available matches.
    pub fn set_match_index(&mut self, index: usize) {
        if self.total_matches == 0 {
            self.match_index = 0;
        } else {
            self.match_index = index.min(self.total_matches - 1);
        }
    }

    /// Sets the total match count, clamping the current match index accordingly.
    pub fn set_total_matches(&mut self, total: usize) {
        self.total_matches = total;
        if total == 0 {
            self.match_index = 0;
        } else if self.match_index >= total {
            self.match_index = total - 1;
        }
    }

    /// Advances to the next match, wrapping to 0 when reaching the end.
    pub fn next_match(&mut self) {
        if self.total_matches > 0 {
            self.match_index = (self.match_index + 1) % self.total_matches;
        }
    }

    /// Moves to the previous match, wrapping to the last match when at 0.
    pub fn prev_match(&mut self) {
        if self.total_matches > 0 {
            self.match_index = if self.match_index == 0 {
                self.total_matches - 1
            } else {
                self.match_index - 1
            };
        }
    }

    /// Returns the formatted match count label (e.g. `"3/15"` or `"No matches"`).
    pub fn match_count_label(&self) -> String {
        if self.total_matches == 0 {
            "No matches".to_string()
        } else {
            let cur = (self.match_index + 1).min(self.total_matches);
            format!("{}/{}", cur, self.total_matches)
        }
    }

    /// Formats the current match position and total matches for display in the chip.
    pub fn format_match_count(&self) -> String {
        self.match_count_label()
    }

    /// Clears the query, resetting matches and navigation index.
    pub fn clear(&mut self) {
        self.query.clear();
        self.match_index = 0;
        self.total_matches = 0;
    }

    /// Returns `true` if the search query is empty.
    pub fn is_empty(&self) -> bool {
        self.query.is_empty()
    }

    /// Synchronizes search parameters to a terminal pane and requests a repaint.
    pub fn sync_to_term(&self, term: &mut ghostwave::term::TermPane) {
        self.sync_to_term_search(&mut term.search);
        term.force_repaint = true;
    }

    /// Synchronizes search parameters to a [`ghostwave::search::SearchState`].
    pub fn sync_to_term_search(&self, search: &mut ghostwave::search::SearchState) {
        if self.query.is_empty() {
            search.stop();
        } else {
            if !search.active {
                search.start();
            }
            if search.query != self.query {
                search.set_query(self.query.clone());
            }
            if search.case_sensitive != self.case_sensitive {
                search.case_sensitive = self.case_sensitive;
                search.dirty = true;
            }
            search.current = self.match_index;
        }
    }

    /// Reads discovered match counts and active match index from a terminal pane.
    pub fn sync_from_term(&mut self, term: &ghostwave::term::TermPane) {
        self.sync_from_term_search(&term.search);
    }

    /// Reads discovered match counts and active match index from a [`ghostwave::search::SearchState`].
    pub fn sync_from_term_search(&mut self, search: &ghostwave::search::SearchState) {
        self.total_matches = search.matches.len();
        if self.total_matches == 0 {
            self.match_index = 0;
        } else {
            self.match_index = search.current.min(self.total_matches - 1);
        }
    }

    /// Advances to the next match and updates the terminal pane.
    pub fn step_next_with_term(&mut self, term: &mut ghostwave::term::TermPane) {
        term.search.next_match();
        self.match_index = term.search.current;
        term.force_repaint = true;
    }

    /// Retreats to the previous match and updates the terminal pane.
    pub fn step_prev_with_term(&mut self, term: &mut ghostwave::term::TermPane) {
        term.search.prev_match();
        self.match_index = term.search.current;
        term.force_repaint = true;
    }

    /// Stops terminal search and clears local search state.
    pub fn close_with_term(&mut self, term: &mut ghostwave::term::TermPane) {
        self.clear();
        term.search.stop();
        term.force_repaint = true;
    }

    /// Applies a [`SearchMessage`] to this state without terminal synchronization.
    pub fn update(&mut self, message: SearchMessage) {
        match message {
            SearchMessage::QueryChanged(q) => self.set_query(q),
            SearchMessage::ToggleCaseSensitive => self.toggle_case_sensitive(),
            SearchMessage::ToggleRegex => self.toggle_regex(),
            SearchMessage::NextMatch => self.next_match(),
            SearchMessage::PrevMatch => self.prev_match(),
            SearchMessage::Close => self.clear(),
        }
    }

    /// Applies a [`SearchMessage`] to this state, optionally synchronizing with a terminal pane.
    pub fn update_with_term(
        &mut self,
        message: SearchMessage,
        term: Option<&mut ghostwave::term::TermPane>,
    ) {
        match message {
            SearchMessage::QueryChanged(q) => {
                self.set_query(q);
                if let Some(t) = term {
                    self.sync_to_term(t);
                }
            }
            SearchMessage::ToggleCaseSensitive => {
                self.toggle_case_sensitive();
                if let Some(t) = term {
                    self.sync_to_term(t);
                }
            }
            SearchMessage::ToggleRegex => {
                self.toggle_regex();
                if let Some(t) = term {
                    self.sync_to_term(t);
                }
            }
            SearchMessage::NextMatch => {
                if let Some(t) = term {
                    self.step_next_with_term(t);
                } else {
                    self.next_match();
                }
            }
            SearchMessage::PrevMatch => {
                if let Some(t) = term {
                    self.step_prev_with_term(t);
                } else {
                    self.prev_match();
                }
            }
            SearchMessage::Close => {
                if let Some(t) = term {
                    self.close_with_term(t);
                } else {
                    self.clear();
                }
            }
        }
    }

    /// Builds the search bar widget for this state.
    pub fn view<'a>(&'a self, theme: &GhostTheme) -> Element<'a, SearchMessage> {
        search_bar(self, theme)
    }

    /// Builds the search overlay container for this state at the given position.
    pub fn view_overlay<'a>(
        &'a self,
        theme: &GhostTheme,
        position: OverlayPosition,
    ) -> Element<'a, SearchMessage> {
        search_overlay(self, theme, position)
    }
}

/// Screen or pane placement for the search overlay container.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum OverlayPosition {
    /// Anchors the search bar at the top-right corner of the pane or window.
    #[default]
    TopRight,
    /// Anchors the search bar at the bottom-right corner of the pane or window.
    BottomRight,
}

/// Translates keyboard events into search actions when the search overlay is active.
///
/// Supported shortcuts:
/// - `Esc`: closes the overlay ([`SearchMessage::Close`])
/// - `Enter`: navigates to the next match ([`SearchMessage::NextMatch`])
/// - `Shift + Enter`: navigates to the previous match ([`SearchMessage::PrevMatch`])
/// - `Up Arrow`: navigates to the previous match ([`SearchMessage::PrevMatch`])
/// - `Down Arrow`: navigates to the next match ([`SearchMessage::NextMatch`])
/// - `Alt + c` or `Ctrl + c`: toggles case sensitivity ([`SearchMessage::ToggleCaseSensitive`])
/// - `Alt + r` or `Ctrl + r`: toggles regex mode ([`SearchMessage::ToggleRegex`])
pub fn handle_key(
    key: &Key,
    modifiers: Modifiers,
) -> Option<SearchMessage> {
    match key.as_ref() {
        Key::Named(Named::Escape) => Some(SearchMessage::Close),
        Key::Named(Named::Enter) => {
            if modifiers.shift() {
                Some(SearchMessage::PrevMatch)
            } else {
                Some(SearchMessage::NextMatch)
            }
        }
        Key::Named(Named::ArrowUp) => Some(SearchMessage::PrevMatch),
        Key::Named(Named::ArrowDown) => Some(SearchMessage::NextMatch),
        Key::Character(c) => {
            let is_alt_or_ctrl = modifiers.alt() || modifiers.control();
            if is_alt_or_ctrl && (c == "c" || c == "C") {
                Some(SearchMessage::ToggleCaseSensitive)
            } else if is_alt_or_ctrl && (c == "r" || c == "R") {
                Some(SearchMessage::ToggleRegex)
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Converts a [`ghostwave::theme::Rgb`] color into an [`iced::Color`].
fn to_color(rgb: Rgb) -> Color {
    Color::from_rgb8(rgb.0, rgb.1, rgb.2)
}

/// Converts a [`ghostwave::theme::Rgb`] color and alpha factor into an [`iced::Color`].
fn with_alpha(rgb: Rgb, alpha: f32) -> Color {
    Color::from_rgba8(rgb.0, rgb.1, rgb.2, alpha)
}

/// Builds the compact search bar widget.
pub fn search_bar<'a>(state: &'a SearchState, theme: &GhostTheme) -> Element<'a, SearchMessage> {
    let theme = *theme;
    let font = crate::fonts::primary();

    let input = text_input("Search scrollback...", &state.query)
        .on_input(SearchMessage::QueryChanged)
        .on_submit(SearchMessage::NextMatch)
        .width(Length::Fixed(190.0))
        .padding(Padding {
            top: 3.0,
            right: 7.0,
            bottom: 3.0,
            left: 7.0,
        })
        .size(13.0)
        .font(font)
        .style(move |_theme, status| {
            let border_color = match status {
                text_input::Status::Focused { .. } => to_color(theme.frame_active),
                text_input::Status::Hovered => to_color(theme.frame_active),
                _ => to_color(theme.frame),
            };
            text_input::Style {
                background: Background::Color(to_color(theme.bg)),
                border: Border {
                    color: border_color,
                    width: 1.0,
                    radius: 3.0.into(),
                },
                icon: to_color(theme.fg),
                placeholder: to_color(theme.tab_inactive_text),
                value: to_color(theme.fg),
                selection: to_color(theme.selection),
            }
        });

    let has_matches = state.total_matches > 0;
    let counter_text_color = if has_matches {
        to_color(theme.palette_fg)
    } else {
        to_color(theme.tab_inactive_text)
    };
    let counter_bg = if has_matches {
        with_alpha(theme.search_match, 0.25)
    } else {
        with_alpha(theme.bg, 0.5)
    };
    let counter_border = if has_matches {
        to_color(theme.search_match)
    } else {
        to_color(theme.frame)
    };

    let counter = container(
        text(state.match_count_label())
            .size(11.0)
            .font(font)
            .color(counter_text_color),
    )
    .padding(Padding {
        top: 3.0,
        right: 6.0,
        bottom: 3.0,
        left: 6.0,
    })
    .style(move |_theme| container::Style {
        background: Some(Background::Color(counter_bg)),
        border: Border {
            color: counter_border,
            width: 1.0,
            radius: 3.0.into(),
        },
        ..container::Style::default()
    });

    let case_btn = button(
        text("Aa")
            .size(11.0)
            .font(font),
    )
    .padding(Padding {
        top: 2.0,
        right: 6.0,
        bottom: 2.0,
        left: 6.0,
    })
    .on_press(SearchMessage::ToggleCaseSensitive)
    .style(move |_theme, status| {
        let is_active = state.case_sensitive;
        let (bg, fg, border_color) = if is_active {
            (
                to_color(theme.frame_active),
                to_color(theme.bg),
                to_color(theme.frame_active),
            )
        } else {
            match status {
                button::Status::Hovered => (
                    with_alpha(theme.frame, 0.3),
                    to_color(theme.fg),
                    to_color(theme.frame_active),
                ),
                _ => (
                    to_color(theme.bg),
                    to_color(theme.tab_inactive_text),
                    to_color(theme.frame),
                ),
            }
        };
        button::Style {
            background: Some(Background::Color(bg)),
            text_color: fg,
            border: Border {
                color: border_color,
                width: 1.0,
                radius: 3.0.into(),
            },
            ..button::Style::default()
        }
    });

    let regex_btn = button(
        text(".*")
            .size(11.0)
            .font(font),
    )
    .padding(Padding {
        top: 2.0,
        right: 6.0,
        bottom: 2.0,
        left: 6.0,
    })
    .on_press(SearchMessage::ToggleRegex)
    .style(move |_theme, status| {
        let is_active = state.regex;
        let (bg, fg, border_color) = if is_active {
            (
                to_color(theme.frame_active),
                to_color(theme.bg),
                to_color(theme.frame_active),
            )
        } else {
            match status {
                button::Status::Hovered => (
                    with_alpha(theme.frame, 0.3),
                    to_color(theme.fg),
                    to_color(theme.frame_active),
                ),
                _ => (
                    to_color(theme.bg),
                    to_color(theme.tab_inactive_text),
                    to_color(theme.frame),
                ),
            }
        };
        button::Style {
            background: Some(Background::Color(bg)),
            text_color: fg,
            border: Border {
                color: border_color,
                width: 1.0,
                radius: 3.0.into(),
            },
            ..button::Style::default()
        }
    });

    let prev_btn = button(
        text("↑")
            .size(12.0)
            .font(font),
    )
    .padding(Padding {
        top: 2.0,
        right: 6.0,
        bottom: 2.0,
        left: 6.0,
    })
    .on_press(SearchMessage::PrevMatch)
    .style(move |_theme, status| {
        let (bg, fg) = match status {
            button::Status::Hovered => (
                with_alpha(theme.frame, 0.4),
                to_color(theme.fg),
            ),
            button::Status::Pressed => (
                to_color(theme.frame_active),
                to_color(theme.bg),
            ),
            _ => (
                to_color(theme.bg),
                to_color(theme.fg),
            ),
        };
        button::Style {
            background: Some(Background::Color(bg)),
            text_color: fg,
            border: Border {
                color: to_color(theme.frame),
                width: 1.0,
                radius: 3.0.into(),
            },
            ..button::Style::default()
        }
    });

    let next_btn = button(
        text("↓")
            .size(12.0)
            .font(font),
    )
    .padding(Padding {
        top: 2.0,
        right: 6.0,
        bottom: 2.0,
        left: 6.0,
    })
    .on_press(SearchMessage::NextMatch)
    .style(move |_theme, status| {
        let (bg, fg) = match status {
            button::Status::Hovered => (
                with_alpha(theme.frame, 0.4),
                to_color(theme.fg),
            ),
            button::Status::Pressed => (
                to_color(theme.frame_active),
                to_color(theme.bg),
            ),
            _ => (
                to_color(theme.bg),
                to_color(theme.fg),
            ),
        };
        button::Style {
            background: Some(Background::Color(bg)),
            text_color: fg,
            border: Border {
                color: to_color(theme.frame),
                width: 1.0,
                radius: 3.0.into(),
            },
            ..button::Style::default()
        }
    });

    let close_btn = button(
        text("✕")
            .size(12.0)
            .font(font),
    )
    .padding(Padding {
        top: 2.0,
        right: 6.0,
        bottom: 2.0,
        left: 6.0,
    })
    .on_press(SearchMessage::Close)
    .style(move |_theme, status| {
        let (bg, fg) = match status {
            button::Status::Hovered => (
                Color::from_rgba8(220, 50, 50, 0.35),
                Color::WHITE,
            ),
            button::Status::Pressed => (
                Color::from_rgba8(220, 50, 50, 0.7),
                Color::WHITE,
            ),
            _ => (
                to_color(theme.bg),
                to_color(theme.tab_inactive_text),
            ),
        };
        button::Style {
            background: Some(Background::Color(bg)),
            text_color: fg,
            border: Border {
                color: to_color(theme.frame),
                width: 1.0,
                radius: 3.0.into(),
            },
            ..button::Style::default()
        }
    });

    container(
        Row::with_children(vec![
            input.into(),
            counter.into(),
            case_btn.into(),
            regex_btn.into(),
            prev_btn.into(),
            next_btn.into(),
            close_btn.into(),
        ])
        .spacing(4.0)
        .align_y(alignment::Vertical::Center),
    )
    .padding(Padding {
        top: 4.0,
        right: 6.0,
        bottom: 4.0,
        left: 6.0,
    })
    .style(move |_theme| container::Style {
        background: Some(Background::Color(to_color(theme.palette_bg))),
        border: Border {
            color: to_color(theme.frame_active),
            width: 1.0,
            radius: 4.0.into(),
        },
        shadow: Shadow {
            color: Color::from_rgba8(0, 0, 0, 0.4),
            offset: Vector::new(0.0, 2.0),
            blur_radius: 6.0,
        },
        ..container::Style::default()
    })
    .into()
}

/// Builds an overlay container positioning the search bar in the active pane or window.
pub fn search_overlay<'a>(
    state: &'a SearchState,
    theme: &GhostTheme,
    position: OverlayPosition,
) -> Element<'a, SearchMessage> {
    let theme = *theme;
    let bar = search_bar(state, &theme);
    let mut c = container(bar).width(Length::Fill).height(Length::Fill);
    match position {
        OverlayPosition::TopRight => {
            c = c
                .align_x(alignment::Horizontal::Right)
                .align_y(alignment::Vertical::Top)
                .padding(Padding {
                    top: 8.0,
                    right: 12.0,
                    bottom: 8.0,
                    left: 12.0,
                });
        }
        OverlayPosition::BottomRight => {
            c = c
                .align_x(alignment::Horizontal::Right)
                .align_y(alignment::Vertical::Bottom)
                .padding(Padding {
                    top: 8.0,
                    right: 12.0,
                    bottom: 8.0,
                    left: 12.0,
                });
        }
    }
    c.into()
}

/// Builds an overlay widget with a custom message transformation.
pub fn search_overlay_mapped<'a, Message: 'a>(
    state: &'a SearchState,
    theme: &GhostTheme,
    position: OverlayPosition,
    map: impl Fn(SearchMessage) -> Message + 'a,
) -> Element<'a, Message> {
    search_overlay(state, theme, position).map(map)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ghostwave::search::Match;
    use ghostwave::theme::THEMES;

    #[test]
    fn search_query_changes() {
        let mut s = SearchState::new();
        assert_eq!(s.query, "");
        assert_eq!(s.match_index, 0);

        s.set_query("error");
        assert_eq!(s.query, "error");
        assert_eq!(s.match_index, 0);

        s.total_matches = 10;
        s.match_index = 4;
        s.set_query("warn");
        assert_eq!(s.query, "warn");
        assert_eq!(s.match_index, 0);

        s.match_index = 2;
        s.set_query("warn");
        assert_eq!(s.match_index, 2);

        s.update(SearchMessage::QueryChanged("info".to_string()));
        assert_eq!(s.query, "info");
        assert_eq!(s.match_index, 0);
    }

    #[test]
    fn regex_toggle() {
        let mut s = SearchState::new();
        assert!(!s.regex);

        s.toggle_regex();
        assert!(s.regex);

        s.toggle_regex();
        assert!(!s.regex);

        s.update(SearchMessage::ToggleRegex);
        assert!(s.regex);

        assert!(!s.case_sensitive);
        s.toggle_case_sensitive();
        assert!(s.case_sensitive);
        s.update(SearchMessage::ToggleCaseSensitive);
        assert!(!s.case_sensitive);
    }

    #[test]
    fn match_count_formatting() {
        let mut s = SearchState::new();
        assert_eq!(s.match_count_label(), "No matches");
        assert_eq!(s.format_match_count(), "No matches");

        s.total_matches = 1;
        s.match_index = 0;
        assert_eq!(s.match_count_label(), "1/1");

        s.total_matches = 15;
        s.match_index = 2;
        assert_eq!(s.match_count_label(), "3/15");

        s.match_index = 0;
        assert_eq!(s.match_count_label(), "1/15");

        s.match_index = 14;
        assert_eq!(s.match_count_label(), "15/15");

        s.match_index = 20;
        assert_eq!(s.match_count_label(), "15/15");

        s.total_matches = 0;
        assert_eq!(s.match_count_label(), "No matches");
    }

    #[test]
    fn navigation_logic() {
        let mut s = SearchState::new();
        s.next_match();
        assert_eq!(s.match_index, 0);
        s.prev_match();
        assert_eq!(s.match_index, 0);

        s.total_matches = 1;
        s.next_match();
        assert_eq!(s.match_index, 0);
        s.prev_match();
        assert_eq!(s.match_index, 0);

        s.total_matches = 3;
        s.match_index = 0;

        s.next_match();
        assert_eq!(s.match_index, 1);
        s.next_match();
        assert_eq!(s.match_index, 2);
        s.next_match();
        assert_eq!(s.match_index, 0);

        s.prev_match();
        assert_eq!(s.match_index, 2);
        s.prev_match();
        assert_eq!(s.match_index, 1);
        s.prev_match();
        assert_eq!(s.match_index, 0);

        s.update(SearchMessage::NextMatch);
        assert_eq!(s.match_index, 1);
        s.update(SearchMessage::PrevMatch);
        assert_eq!(s.match_index, 0);
    }

    #[test]
    fn term_search_state_sync() {
        let mut term_search = ghostwave::search::SearchState::default();
        let mut overlay_state = SearchState {
            query: "fn main".to_string(),
            match_index: 0,
            total_matches: 0,
            regex: false,
            case_sensitive: true,
        };

        overlay_state.sync_to_term_search(&mut term_search);
        assert!(term_search.active);
        assert_eq!(term_search.query, "fn main");
        assert!(term_search.case_sensitive);
        assert!(term_search.dirty);

        term_search.matches = vec![
            Match {
                row: 10,
                col_start: 0,
                col_end: 7,
            },
            Match {
                row: 25,
                col_start: 4,
                col_end: 11,
            },
        ];
        term_search.current = 1;

        overlay_state.sync_from_term_search(&term_search);
        assert_eq!(overlay_state.total_matches, 2);
        assert_eq!(overlay_state.match_index, 1);
        assert_eq!(overlay_state.match_count_label(), "2/2");

        overlay_state.clear();
        overlay_state.sync_to_term_search(&mut term_search);
        assert!(!term_search.active);
        assert!(term_search.query.is_empty());
    }

    #[test]
    fn keyboard_shortcut_routing() {
        assert_eq!(
            handle_key(&Key::Named(Named::Escape), Modifiers::default()),
            Some(SearchMessage::Close)
        );

        assert_eq!(
            handle_key(&Key::Named(Named::Enter), Modifiers::default()),
            Some(SearchMessage::NextMatch)
        );

        assert_eq!(
            handle_key(&Key::Named(Named::Enter), Modifiers::SHIFT),
            Some(SearchMessage::PrevMatch)
        );

        assert_eq!(
            handle_key(&Key::Named(Named::ArrowDown), Modifiers::default()),
            Some(SearchMessage::NextMatch)
        );

        assert_eq!(
            handle_key(&Key::Named(Named::ArrowUp), Modifiers::default()),
            Some(SearchMessage::PrevMatch)
        );

        assert_eq!(
            handle_key(&Key::Character("c".into()), Modifiers::ALT),
            Some(SearchMessage::ToggleCaseSensitive)
        );

        assert_eq!(
            handle_key(&Key::Character("r".into()), Modifiers::CTRL),
            Some(SearchMessage::ToggleRegex)
        );

        assert_eq!(
            handle_key(&Key::Named(Named::Space), Modifiers::default()),
            None
        );
    }

    #[test]
    fn widget_view_tree_construction() {
        let state = SearchState {
            query: "needle".to_string(),
            match_index: 0,
            total_matches: 4,
            regex: true,
            case_sensitive: false,
        };
        let theme = THEMES[0];

        let _bar = search_bar(&state, &theme);
        let _overlay_tr = search_overlay(&state, &theme, OverlayPosition::TopRight);
        let _overlay_br = search_overlay(&state, &theme, OverlayPosition::BottomRight);
    }
}
