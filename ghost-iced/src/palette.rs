//! Command palette overlay widget for ghostwave's iced front-end.
//!
//! Provides an interactive, fuzzy-searchable modal overlay displaying available actions,
//! keybinding shortcuts, dynamic prefix commands (`open <path>`, `ssh <host>`,
//! `ds <host>`), and theme selection.

use ghostwave::keymap::Action;
use ghostwave::theme::Theme as GhostTheme;
use iced::alignment::{Horizontal, Vertical};
use iced::border::{self, Border};
use iced::widget::{column, container, mouse_area, row, scrollable, text, text_input, Space};
use iced::{Background, Color, Element, Length, Padding, Shadow, Vector};

/// An item in the command palette inventory.
#[derive(Clone, Debug, PartialEq)]
pub struct PaletteItem {
    /// Human-readable display title for the action.
    pub title: String,
    /// Category grouping (e.g. "Pane", "Tab", "Terminal", "Session", "View", "App", "Theme").
    pub category: String,
    /// Optional default keybinding shortcut badge (e.g. "Ctrl+Shift+P").
    pub shortcut: Option<String>,
    /// Underlying action executed when this item is triggered.
    pub action: Action,
}

impl PaletteItem {
    /// Constructs a new [`PaletteItem`].
    pub fn new(
        title: impl Into<String>,
        category: impl Into<String>,
        shortcut: Option<&str>,
        action: Action,
    ) -> Self {
        Self {
            title: title.into(),
            category: category.into(),
            shortcut: shortcut.map(|s| s.to_string()),
            action,
        }
    }

    /// Label accessor matching the legacy palette interface.
    pub fn label(&self) -> &str {
        &self.title
    }
}

/// State for the command palette overlay widget.
#[derive(Clone, Debug)]
pub struct PaletteState {
    /// Current search query string.
    pub query: String,
    /// Selected item index in the filtered action list.
    pub selected: usize,
    /// Full catalog of registered action items.
    pub items: Vec<PaletteItem>,
}

impl Default for PaletteState {
    fn default() -> Self {
        Self::new(default_inventory())
    }
}

impl PaletteState {
    /// Creates a new [`PaletteState`] with the specified items.
    pub fn new(items: Vec<PaletteItem>) -> Self {
        Self {
            query: String::new(),
            selected: 0,
            items,
        }
    }

    /// Returns filtered action items matching the current search query.
    ///
    /// Supports dynamic command prefixes (`open `, `ssh `, `ds `), theme filtering,
    /// and fuzzy/substring matching across title, category, and keybinding shortcuts.
    pub fn filtered_items(&self) -> Vec<PaletteItem> {
        let trimmed = self.query.trim();
        if trimmed.is_empty() {
            return self.items.clone();
        }

        if let Some(path) = self.query.strip_prefix("open ") {
            let p = path.trim();
            if !p.is_empty() {
                return vec![PaletteItem::new(
                    format!("Open file: {p}"),
                    "File",
                    None,
                    Action::OpenFile(p.to_string()),
                )];
            }
        }

        if let Some(host) = self.query.strip_prefix("ssh ") {
            let h = host.trim();
            if !h.is_empty() {
                return vec![PaletteItem::new(
                    format!("Connect SSH: {h}"),
                    "Remote",
                    None,
                    Action::ConnectSsh(h.to_string()),
                )];
            }
        }

        if let Some(host) = self.query.strip_prefix("ds ") {
            let h = host.trim();
            if !h.is_empty() {
                return vec![PaletteItem::new(
                    format!("Durable SSH (ghostd): {h}"),
                    "Remote",
                    None,
                    Action::ConnectDurableSsh(h.to_string()),
                )];
            }
        }

        let is_theme_filter = self.query.starts_with("theme");

        let mut scored: Vec<(i32, PaletteItem)> = self
            .items
            .iter()
            .filter_map(|item| {
                if is_theme_filter
                    && !item.category.eq_ignore_ascii_case("theme")
                    && !item.title.starts_with("Theme:")
                {
                    return None;
                }
                score_item(&self.query, item).map(|score| (score, item.clone()))
            })
            .collect();

        scored.sort_by(|a, b| b.0.cmp(&a.0));
        scored.into_iter().map(|(_, item)| item).collect()
    }

    /// Returns the currently highlighted item, clamped to valid filtered bounds.
    pub fn selected_item(&self) -> Option<PaletteItem> {
        let filtered = self.filtered_items();
        if filtered.is_empty() {
            return None;
        }
        let clamped = self.selected.min(filtered.len().saturating_sub(1));
        Some(filtered[clamped].clone())
    }

    /// Returns the currently highlighted item's action.
    pub fn selected_action(&self) -> Option<Action> {
        self.selected_item().map(|item| item.action)
    }

    /// Sets the search query, resetting the selected index to 0.
    pub fn set_query(&mut self, query: impl Into<String>) {
        self.query = query.into();
        self.selected = 0;
    }

    /// Appends a character to the query, resetting selected index to 0.
    pub fn type_char(&mut self, c: char) {
        self.query.push(c);
        self.selected = 0;
    }

    /// Removes the last character from the query, resetting selected index to 0.
    pub fn backspace(&mut self) {
        self.query.pop();
        self.selected = 0;
    }

    /// Clears the search query and resets the selected index.
    pub fn clear(&mut self) {
        self.query.clear();
        self.selected = 0;
    }

    /// Moves the selection cursor up by one item, wrapping to the end.
    pub fn select_up(&mut self) {
        let n = self.filtered_items().len();
        if n > 0 {
            self.selected = (self.selected + n - 1) % n;
        }
    }

    /// Moves the selection cursor down by one item, wrapping to the start.
    pub fn select_down(&mut self) {
        let n = self.filtered_items().len();
        if n > 0 {
            self.selected = (self.selected + 1) % n;
        }
    }

    /// Sets the selection to a specific index, clamping to available items.
    pub fn select_index(&mut self, index: usize) {
        let n = self.filtered_items().len();
        if n > 0 {
            self.selected = index.min(n - 1);
        } else {
            self.selected = 0;
        }
    }

    /// Confirms the current selection and returns its action.
    pub fn submit(&mut self) -> Option<Action> {
        self.selected_action()
    }

    /// Selects an item by index in the filtered list and returns its action.
    pub fn select_and_execute(&mut self, index: usize) -> Option<Action> {
        let filtered = self.filtered_items();
        if index < filtered.len() {
            self.selected = index;
            Some(filtered[index].action.clone())
        } else {
            None
        }
    }

    /// Handles a palette message, returning an action when execution or closure occurs.
    pub fn update(&mut self, message: PaletteMessage) -> Option<PaletteAction> {
        match message {
            PaletteMessage::QueryChanged(query) => {
                self.set_query(query);
                None
            }
            PaletteMessage::ItemClicked(index) => {
                self.select_and_execute(index).map(PaletteAction::Execute)
            }
            PaletteMessage::Submit => self.submit().map(PaletteAction::Execute),
            PaletteMessage::Close => Some(PaletteAction::Close),
            PaletteMessage::NavigateUp => {
                self.select_up();
                None
            }
            PaletteMessage::NavigateDown => {
                self.select_down();
                None
            }
        }
    }

    /// Handles an iced keyboard event for palette navigation and dismissal.
    pub fn handle_iced_key(
        &mut self,
        key: &iced::keyboard::Key,
        modifiers: &iced::keyboard::Modifiers,
    ) -> Option<PaletteAction> {
        use iced::keyboard::key::Named;
        use iced::keyboard::Key;

        if modifiers.control() {
            if let Key::Character(c) = key {
                let lower = c.to_lowercase();
                if lower == "p" || lower == "k" {
                    self.select_up();
                    return None;
                } else if lower == "n" || lower == "j" {
                    self.select_down();
                    return None;
                }
            }
        }

        match key {
            Key::Named(Named::ArrowUp) => {
                self.select_up();
                None
            }
            Key::Named(Named::ArrowDown) => {
                self.select_down();
                None
            }
            Key::Named(Named::Enter) => self.submit().map(PaletteAction::Execute),
            Key::Named(Named::Escape) => Some(PaletteAction::Close),
            _ => None,
        }
    }

    /// Handles a terminal/keymap [`ghostwave::keymap::KeyPress`] event.
    pub fn handle_keypress(&mut self, kp: &ghostwave::keymap::KeyPress) -> Option<PaletteAction> {
        use libghostty_vt::key::{Key, Mods};

        let k = kp.key;
        let m = kp.mods;

        if m.contains(Mods::CTRL) {
            if k == Key::P || k == Key::K {
                self.select_up();
                return None;
            } else if k == Key::N || k == Key::J {
                self.select_down();
                return None;
            }
        }

        match k {
            Key::ArrowUp => {
                self.select_up();
                None
            }
            Key::ArrowDown => {
                self.select_down();
                None
            }
            Key::Enter => self.submit().map(PaletteAction::Execute),
            Key::Escape => Some(PaletteAction::Close),
            _ => None,
        }
    }
}

/// Messages emitted by command palette UI widgets.
#[derive(Debug, Clone, PartialEq)]
pub enum PaletteMessage {
    /// Search query text was updated in the text input.
    QueryChanged(String),
    /// An action item was clicked at the given filtered index.
    ItemClicked(usize),
    /// Enter was pressed to trigger the active selection.
    Submit,
    /// Modal overlay was cancelled or dismissed.
    Close,
    /// Selection moved up.
    NavigateUp,
    /// Selection moved down.
    NavigateDown,
}

/// Resulting application actions from palette processing.
#[derive(Debug, Clone, PartialEq)]
pub enum PaletteAction {
    /// Execute the selected terminal or application action.
    Execute(Action),
    /// Close the palette modal overlay.
    Close,
}

/// Computes a fuzzy match score between a search query and a target string.
///
/// Returns `Some(score)` if the query matches as a substring or subsequence,
/// with bonuses for prefix matches, word boundaries, and consecutive characters.
pub fn fuzzy_match(pattern: &str, target: &str) -> Option<i32> {
    let p_lower: Vec<char> = pattern.to_lowercase().chars().collect();
    let t_lower: Vec<char> = target.to_lowercase().chars().collect();
    if p_lower.is_empty() {
        return Some(0);
    }

    let target_str = target.to_lowercase();
    let pattern_str = pattern.to_lowercase();

    if let Some(idx) = target_str.find(&pattern_str) {
        let mut score = 1000 - (idx as i32 * 10);
        if idx == 0 {
            score += 500;
        }
        if target_str == pattern_str {
            score += 1000;
        }
        return Some(score);
    }

    let mut p_idx = 0;
    let mut score = 0;
    let mut prev_match_idx: Option<usize> = None;

    for (t_idx, &tc) in t_lower.iter().enumerate() {
        if p_idx < p_lower.len() && tc == p_lower[p_idx] {
            if let Some(prev) = prev_match_idx {
                if prev + 1 == t_idx {
                    score += 50;
                }
            }
            if t_idx == 0
                || t_lower[t_idx - 1] == ' '
                || t_lower[t_idx - 1] == ':'
                || t_lower[t_idx - 1] == '+'
                || t_lower[t_idx - 1] == '-'
                || t_lower[t_idx - 1] == '('
                || t_lower[t_idx - 1] == '/'
            {
                score += 80;
            } else {
                score += 10;
            }
            prev_match_idx = Some(t_idx);
            p_idx += 1;
        }
    }

    if p_idx == p_lower.len() {
        Some(score)
    } else {
        None
    }
}

/// Scores a palette item against the search query across title, shortcut, and category.
fn score_item(query: &str, item: &PaletteItem) -> Option<i32> {
    let q = query.trim();
    if q.is_empty() {
        return Some(0);
    }

    let title_score = fuzzy_match(q, &item.title).map(|s| s * 4);
    let shortcut_score = item
        .shortcut
        .as_deref()
        .and_then(|sc| fuzzy_match(q, sc))
        .map(|s| s * 3);
    let category_score = fuzzy_match(q, &item.category).map(|s| s * 2);

    match (title_score, shortcut_score, category_score) {
        (None, None, None) => None,
        (s1, s2, s3) => {
            let max_val = s1.unwrap_or(0).max(s2.unwrap_or(0)).max(s3.unwrap_or(0));
            Some(max_val)
        }
    }
}

/// Converts a [`ghostwave::theme::Rgb`] to an opaque [`iced::Color`].
fn rgb(c: ghostwave::theme::Rgb) -> Color {
    Color::from_rgb8(c.0, c.1, c.2)
}

/// Converts a [`ghostwave::theme::Rgb`] and alpha value to an [`iced::Color`].
fn rgba(c: ghostwave::theme::Rgb, a: f32) -> Color {
    Color::from_rgba8(c.0, c.1, c.2, a)
}

/// Renders the command palette overlay as an iced widget element.
pub fn view<'a>(state: &'a PaletteState, theme: &GhostTheme) -> Element<'a, PaletteMessage> {
    let theme = *theme;
    let filtered = state.filtered_items();
    let clamped_selected = if filtered.is_empty() {
        0
    } else {
        state.selected.min(filtered.len() - 1)
    };

    let search_input = text_input("Type a command or search...", &state.query)
        .id("palette_search_input")
        .on_input(PaletteMessage::QueryChanged)
        .on_submit(PaletteMessage::Submit)
        .padding(Padding::from([8.0, 12.0]))
        .size(15.0)
        .font(crate::fonts::primary())
        .style(move |_, _| text_input::Style {
            background: Background::Color(rgb(theme.bg)),
            border: border::rounded(4.0).color(rgb(theme.frame_active)).width(1.0),
            icon: rgb(theme.palette_fg),
            placeholder: rgb(theme.tab_inactive_text),
            value: rgb(theme.palette_fg),
            selection: rgb(theme.selection),
        });

    let separator = container(Space::new())
        .width(Length::Fill)
        .height(Length::Fixed(1.0))
        .style(move |_| container::Style {
            background: Some(Background::Color(rgb(theme.frame))),
            ..Default::default()
        });

    let list_content = if filtered.is_empty() {
        column![container(
            text("No matching commands (try: open <path> | ssh <host> | theme <name>)")
                .size(14.0)
                .font(crate::fonts::primary())
                .color(rgb(theme.tab_inactive_text))
        )
        .padding(Padding::from([20.0, 12.0]))
        .width(Length::Fill)
        .align_x(Horizontal::Center)]
        .spacing(2.0)
        .width(Length::Fill)
    } else {
        let mut col = column![].spacing(2.0).width(Length::Fill);

        for (i, item) in filtered.iter().enumerate() {
            let is_selected = i == clamped_selected;

            let title_label = text(item.title.clone())
                .size(14.0)
                .font(crate::fonts::primary())
                .color(if is_selected {
                    rgb(theme.bg)
                } else {
                    rgb(theme.palette_fg)
                });

            let category_badge = container(
                text(item.category.clone())
                    .size(11.0)
                    .font(crate::fonts::primary())
                    .color(if is_selected {
                        rgb(theme.bg)
                    } else {
                        rgb(theme.tab_inactive_text)
                    }),
            )
            .padding(Padding::from([1.0, 5.0]))
            .style(move |_| container::Style {
                background: Some(Background::Color(if is_selected {
                    rgba(theme.bg, 0.15)
                } else {
                    rgba(theme.frame, 0.25)
                })),
                border: Border {
                    color: if is_selected {
                        rgba(theme.bg, 0.3)
                    } else {
                        rgba(theme.frame, 0.4)
                    },
                    width: 1.0,
                    radius: 3.0.into(),
                },
                ..Default::default()
            });

            let left_group = row![title_label, category_badge]
                .spacing(8.0)
                .align_y(Vertical::Center);

            let mut row_children: Vec<Element<'a, PaletteMessage>> = vec![
                left_group.into(),
                Space::new().width(Length::Fill).into(),
            ];

            if let Some(ref sc) = item.shortcut {
                let badge = container(
                    text(sc.clone())
                        .size(11.0)
                        .font(crate::fonts::primary())
                        .color(if is_selected {
                            rgb(theme.bg)
                        } else {
                            rgb(theme.palette_fg)
                        }),
                )
                .padding(Padding::from([2.0, 6.0]))
                .style(move |_| container::Style {
                    background: Some(Background::Color(if is_selected {
                        rgba(theme.bg, 0.25)
                    } else {
                        rgba(theme.frame_active, 0.2)
                    })),
                    border: Border {
                        color: if is_selected {
                            rgba(theme.bg, 0.4)
                        } else {
                            rgb(theme.frame_active)
                        },
                        width: 1.0,
                        radius: 4.0.into(),
                    },
                    ..Default::default()
                });

                row_children.push(badge.into());
            }

            let item_row = iced::widget::Row::with_children(row_children)
                .align_y(Vertical::Center)
                .width(Length::Fill);

            let item_container = container(item_row)
                .width(Length::Fill)
                .padding(Padding::from([6.0, 10.0]))
                .style(move |_| {
                    if is_selected {
                        container::Style {
                            background: Some(Background::Color(rgb(theme.palette_sel))),
                            border: Border {
                                color: rgb(theme.frame_active),
                                width: 1.0,
                                radius: 4.0.into(),
                            },
                            ..Default::default()
                        }
                    } else {
                        container::Style {
                            background: None,
                            border: Border::default(),
                            ..Default::default()
                        }
                    }
                });

            let clickable_item =
                mouse_area(item_container).on_press(PaletteMessage::ItemClicked(i));

            col = col.push(clickable_item);
        }

        col
    };

    let scrollable_list = scrollable(list_content)
        .width(Length::Fill)
        .height(Length::Fill);

    let modal_body = column![search_input, separator, scrollable_list]
        .spacing(8.0)
        .width(Length::Fill)
        .height(Length::Fill);

    let modal_box = container(modal_body)
        .width(Length::Fixed(600.0))
        .height(Length::Fixed(380.0))
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
            ..Default::default()
        });

    let modal_absorber = mouse_area(modal_box);

    let backdrop = mouse_area(
        container(Space::new())
            .width(Length::Fill)
            .height(Length::Fill)
            .style(|_| container::Style {
                background: Some(Background::Color(Color::from_rgba(0.0, 0.0, 0.0, 0.65))),
                ..Default::default()
            }),
    )
    .on_press(PaletteMessage::Close);

    let centered_modal = container(modal_absorber)
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(Horizontal::Center)
        .align_y(Vertical::Center);

    iced::widget::stack![backdrop, centered_modal].into()
}

/// Builds the default action inventory for ghostwave, populated with theme choices.
pub fn default_inventory() -> Vec<PaletteItem> {
    default_items(&ghostwave::theme::names())
}

/// Builds the action inventory for ghostwave with explicitly configured theme choices.
pub fn default_items(themes: &[&str]) -> Vec<PaletteItem> {
    use ghostwave::keymap::Action::*;
    use ghostwave::layout::Dir;

    let mut items = vec![
        PaletteItem::new("New Pane (down)", "Pane", Some("Ctrl+Shift+D"), NewPaneDown),
        PaletteItem::new("New Pane (right)", "Pane", Some("Ctrl+D"), NewPaneRight),
        PaletteItem::new("New Pane (stacked)", "Pane", Some("Ctrl+P S"), NewPaneStacked),
        PaletteItem::new("New Pane", "Pane", Some("Alt+N"), NewPane),
        PaletteItem::new("Close Pane", "Pane", Some("Ctrl+W"), ClosePane),
        PaletteItem::new("Focus Left", "Pane", Some("Alt+H"), MoveFocus(Dir::Left)),
        PaletteItem::new("Focus Right", "Pane", Some("Alt+L"), MoveFocus(Dir::Right)),
        PaletteItem::new("Focus Up", "Pane", Some("Alt+K"), MoveFocus(Dir::Up)),
        PaletteItem::new("Focus Down", "Pane", Some("Alt+J"), MoveFocus(Dir::Down)),
        PaletteItem::new("Next Pane", "Pane", Some("Ctrl+P P"), SwitchFocus),
        PaletteItem::new("Fullscreen Pane (Window)", "Pane", Some("Ctrl+M"), ToggleZoom),
        PaletteItem::new("Toggle Pane Frames", "Pane", Some("Ctrl+P Shift+Z"), ToggleFrames),
        PaletteItem::new("Rename Pane", "Pane", Some("Ctrl+P C"), RenamePane),
        PaletteItem::new("Cycle Stacked Panes", "Pane", Some("Ctrl+P W"), CycleStack),
        PaletteItem::new("Toggle Floating Pane", "Pane", Some("Ctrl+P F"), ToggleFloating),
        PaletteItem::new("Toggle Hide Floating Panes", "Pane", Some("Ctrl+P E"), ToggleHideFloat),
        PaletteItem::new("Cycle Floating Panes", "Pane", Some("Ctrl+P Shift+E"), CycleFloat),
        PaletteItem::new(
            "Toggle Synchronized Panes (Broadcast)",
            "Pane",
            Some("Ctrl+P B"),
            ToggleSyncPanes,
        ),
        PaletteItem::new("Break Pane to New Tab", "Pane", Some("Ctrl+T B"), BreakPane),
        PaletteItem::new("New Tab", "Tab", Some("Ctrl+Shift+T"), NewTab),
        PaletteItem::new("Close Tab", "Tab", Some("Ctrl+Shift+W"), CloseTab),
        PaletteItem::new("Next Tab", "Tab", Some("Ctrl+]"), NextTab),
        PaletteItem::new("Previous Tab", "Tab", Some("Ctrl+["), PrevTab),
        PaletteItem::new("Toggle Last Active Tab", "Tab", Some("Ctrl+T Tab"), ToggleLastTab),
        PaletteItem::new("Rename Tab", "Tab", Some("F2"), RenameTab),
        PaletteItem::new("Find in Terminal", "Terminal", Some("Ctrl+Shift+F"), OpenSearch),
        PaletteItem::new("Copy", "Terminal", Some("Ctrl+Shift+C"), Copy),
        PaletteItem::new("Paste", "Terminal", Some("Ctrl+Shift+V"), Paste),
        PaletteItem::new("Clear Terminal", "Terminal", Some("Ctrl+K"), ClearTerminal),
        PaletteItem::new("Scroll Page Up", "Terminal", Some("Shift+PageUp"), PageUp),
        PaletteItem::new("Scroll Page Down", "Terminal", Some("Shift+PageDown"), PageDown),
        PaletteItem::new("Scroll to Bottom", "Terminal", Some("Shift+End"), ScrollToBottom),
        PaletteItem::new("Edit Scrollback in Editor", "Terminal", Some("Ctrl+S E"), EditScrollback),
        PaletteItem::new("Session Manager", "Session", Some("Ctrl+O W"), SessionManager),
        PaletteItem::new("Detach Session", "Session", Some("Ctrl+O D"), Detach),
        PaletteItem::new("Kill Remote Session", "Session", None, KillRemoteSession),
        PaletteItem::new("Command Palette", "View", Some("Ctrl+Shift+P"), OpenCommandBar),
        PaletteItem::new("Toggle Scratchpad Drawer", "View", Some("F12"), ToggleScratchpad),
        PaletteItem::new(
            "Ghostwave (Screensaver Visualizer)",
            "View",
            None,
            Ghostwave,
        ),
        PaletteItem::new("New AI Chat", "View", None, NewAi),
        PaletteItem::new("Quit", "App", Some("Ctrl+Q"), Quit),
        PaletteItem::new("Lock Terminal", "App", Some("Ctrl+G"), Lock),
    ];

    for t in themes {
        items.push(PaletteItem::new(
            format!("Theme: {t}"),
            "Theme",
            None,
            Theme(t.to_string()),
        ));
    }

    items
}

#[cfg(test)]
mod tests {
    use super::*;
    use ghostwave::keymap::Action;

    fn test_palette(labels: &[&str]) -> PaletteState {
        let items = labels
            .iter()
            .map(|l| PaletteItem::new(*l, "Test", None, Action::Quit))
            .collect();
        PaletteState::new(items)
    }

    #[test]
    fn default_inventory_has_all_categories() {
        let items = default_inventory();
        assert!(!items.is_empty());
        let categories: Vec<&str> = items.iter().map(|i| i.category.as_str()).collect();
        assert!(categories.contains(&"Pane"));
        assert!(categories.contains(&"Tab"));
        assert!(categories.contains(&"Terminal"));
        assert!(categories.contains(&"Session"));
        assert!(categories.contains(&"View"));
        assert!(categories.contains(&"App"));
        assert!(categories.contains(&"Theme"));
    }

    #[test]
    fn filtered_empty_query_returns_all() {
        let p = test_palette(&["Alpha", "Beta", "Gamma"]);
        let out = p.filtered_items();
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].title, "Alpha");
        assert_eq!(out[1].title, "Beta");
        assert_eq!(out[2].title, "Gamma");
    }

    #[test]
    fn filtered_case_insensitive_substring() {
        let mut p = test_palette(&["New Tab", "Close Tab", "New Pane"]);
        p.set_query("tAb");
        let out = p.filtered_items();
        assert_eq!(out.len(), 2);
        assert!(out.iter().any(|i| i.title == "New Tab"));
        assert!(out.iter().any(|i| i.title == "Close Tab"));
    }

    #[test]
    fn filtered_by_category() {
        let items = vec![
            PaletteItem::new("New Tab", "Tab", None, Action::NewTab),
            PaletteItem::new("Close Tab", "Tab", None, Action::CloseTab),
            PaletteItem::new("Split", "Pane", None, Action::NewPaneDown),
        ];
        let mut p = PaletteState::new(items);
        p.set_query("pane");
        let out = p.filtered_items();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].title, "Split");
    }

    #[test]
    fn filtered_by_shortcut() {
        let items = vec![
            PaletteItem::new("Command Bar", "View", Some("Ctrl+Shift+P"), Action::OpenCommandBar),
            PaletteItem::new("Quit", "App", Some("Ctrl+Q"), Action::Quit),
        ];
        let mut p = PaletteState::new(items);
        p.set_query("shift+p");
        let out = p.filtered_items();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].title, "Command Bar");
    }

    #[test]
    fn filtered_fuzzy_subsequence() {
        let mut p = test_palette(&["New Pane Down", "Close Pane", "Copy"]);
        p.set_query("npd");
        let out = p.filtered_items();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].title, "New Pane Down");
    }

    #[test]
    fn filtered_no_match_returns_empty() {
        let mut p = test_palette(&["Foo", "Bar"]);
        p.set_query("zzzzzzzzz");
        assert!(p.filtered_items().is_empty());
    }

    #[test]
    fn dynamic_open_prefix() {
        let mut p = test_palette(&["Unrelated"]);
        p.set_query("open /path/to/demo.rs");
        let out = p.filtered_items();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].title, "Open file: /path/to/demo.rs");
        assert!(matches!(&out[0].action, Action::OpenFile(path) if path == "/path/to/demo.rs"));

        p.set_query("open   /trimmed/path.txt  ");
        let out2 = p.filtered_items();
        assert_eq!(out2[0].title, "Open file: /trimmed/path.txt");

        p.set_query("open ");
        let bare = p.filtered_items();
        assert!(bare.iter().all(|i| !matches!(i.action, Action::OpenFile(_))));
    }

    #[test]
    fn dynamic_ssh_prefix() {
        let mut p = test_palette(&["Unrelated"]);
        p.set_query("ssh prod-server");
        let out = p.filtered_items();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].title, "Connect SSH: prod-server");
        assert!(matches!(&out[0].action, Action::ConnectSsh(h) if h == "prod-server"));

        p.set_query("ssh ");
        let bare = p.filtered_items();
        assert!(bare.iter().all(|i| !matches!(i.action, Action::ConnectSsh(_))));
    }

    #[test]
    fn dynamic_ds_prefix() {
        let mut p = test_palette(&["Unrelated"]);
        p.set_query("ds durable-host");
        let out = p.filtered_items();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].title, "Durable SSH (ghostd): durable-host");
        assert!(matches!(&out[0].action, Action::ConnectDurableSsh(h) if h == "durable-host"));

        p.set_query("ds ");
        let bare = p.filtered_items();
        assert!(bare.iter().all(|i| !matches!(i.action, Action::ConnectDurableSsh(_))));
    }

    #[test]
    fn theme_prefix_filters_to_theme_items() {
        let all = default_items(&["tokyonight", "dracula", "nord"]);
        let mut p = PaletteState::new(all);
        p.set_query("theme");
        let out = p.filtered_items();
        assert!(!out.is_empty());
        assert!(out.iter().all(|i| i.title.starts_with("Theme:")));

        p.set_query("new");
        let out_new = p.filtered_items();
        assert!(out_new.iter().any(|i| i.title.contains("New")));
    }

    #[test]
    fn selected_item_clamps_cursor() {
        let mut p = test_palette(&["A", "B", "C"]);
        p.selected = 100;
        let sel = p.selected_item().unwrap();
        assert_eq!(sel.title, "C");

        p.set_query("nonexistent");
        assert!(p.selected_item().is_none());
        assert!(p.selected_action().is_none());
    }

    #[test]
    fn nav_up_down_wrapping() {
        let mut p = test_palette(&["First", "Second", "Third"]);
        assert_eq!(p.selected, 0);

        p.select_up();
        assert_eq!(p.selected, 2);

        p.select_down();
        assert_eq!(p.selected, 0);

        p.select_down();
        assert_eq!(p.selected, 1);

        p.select_up();
        assert_eq!(p.selected, 0);

        let mut single = test_palette(&["Only"]);
        single.select_up();
        assert_eq!(single.selected, 0);
        single.select_down();
        assert_eq!(single.selected, 0);
    }

    #[test]
    fn query_change_and_typing_resets_cursor() {
        let mut p = test_palette(&["A", "B", "C"]);
        p.selected = 2;
        p.type_char('x');
        assert_eq!(p.selected, 0);

        p.selected = 2;
        p.backspace();
        assert_eq!(p.selected, 0);

        p.selected = 2;
        p.set_query("test");
        assert_eq!(p.selected, 0);
    }

    #[test]
    fn submit_and_execute_by_index() {
        let items = vec![
            PaletteItem::new("Quit", "App", None, Action::Quit),
            PaletteItem::new("Split", "Pane", None, Action::NewPaneDown),
        ];
        let mut p = PaletteState::new(items);
        p.selected = 1;
        assert_eq!(p.submit(), Some(Action::NewPaneDown));

        assert_eq!(p.select_and_execute(0), Some(Action::Quit));
        assert_eq!(p.selected, 0);
        assert_eq!(p.select_and_execute(99), None);
    }

    #[test]
    fn update_messages() {
        let mut p = test_palette(&["First", "Second"]);

        assert_eq!(p.update(PaletteMessage::NavigateDown), None);
        assert_eq!(p.selected, 1);

        assert_eq!(p.update(PaletteMessage::NavigateUp), None);
        assert_eq!(p.selected, 0);

        assert_eq!(
            p.update(PaletteMessage::QueryChanged("First".into())),
            None
        );
        assert_eq!(p.query, "First");

        assert_eq!(
            p.update(PaletteMessage::Submit),
            Some(PaletteAction::Execute(Action::Quit))
        );

        assert_eq!(p.update(PaletteMessage::Close), Some(PaletteAction::Close));

        assert_eq!(
            p.update(PaletteMessage::ItemClicked(0)),
            Some(PaletteAction::Execute(Action::Quit))
        );
    }

    #[test]
    fn handle_iced_keys_navigation_and_actions() {
        use iced::keyboard::key::Named;
        use iced::keyboard::{Key, Modifiers};

        let mut p = test_palette(&["A", "B", "C"]);

        let no_mods = Modifiers::empty();
        let ctrl_mods = Modifiers::CTRL;

        assert_eq!(
            p.handle_iced_key(&Key::Named(Named::ArrowDown), &no_mods),
            None
        );
        assert_eq!(p.selected, 1);

        assert_eq!(
            p.handle_iced_key(&Key::Named(Named::ArrowUp), &no_mods),
            None
        );
        assert_eq!(p.selected, 0);

        assert_eq!(
            p.handle_iced_key(&Key::Character("n".into()), &ctrl_mods),
            None
        );
        assert_eq!(p.selected, 1);

        assert_eq!(
            p.handle_iced_key(&Key::Character("p".into()), &ctrl_mods),
            None
        );
        assert_eq!(p.selected, 0);

        assert_eq!(
            p.handle_iced_key(&Key::Named(Named::Enter), &no_mods),
            Some(PaletteAction::Execute(Action::Quit))
        );

        assert_eq!(
            p.handle_iced_key(&Key::Named(Named::Escape), &no_mods),
            Some(PaletteAction::Close)
        );
    }

    #[test]
    fn handle_ghostty_keypress() {
        use libghostty_vt::key::{Key, Mods};

        let mut p = test_palette(&["A", "B", "C"]);

        let kp_down = ghostwave::keymap::KeyPress {
            key: Key::ArrowDown,
            mods: Mods::empty(),
            text: None,
        };
        assert_eq!(p.handle_keypress(&kp_down), None);
        assert_eq!(p.selected, 1);

        let kp_up = ghostwave::keymap::KeyPress {
            key: Key::ArrowUp,
            mods: Mods::empty(),
            text: None,
        };
        assert_eq!(p.handle_keypress(&kp_up), None);
        assert_eq!(p.selected, 0);

        let kp_ctrl_n = ghostwave::keymap::KeyPress {
            key: Key::N,
            mods: Mods::CTRL,
            text: None,
        };
        assert_eq!(p.handle_keypress(&kp_ctrl_n), None);
        assert_eq!(p.selected, 1);

        let kp_ctrl_p = ghostwave::keymap::KeyPress {
            key: Key::P,
            mods: Mods::CTRL,
            text: None,
        };
        assert_eq!(p.handle_keypress(&kp_ctrl_p), None);
        assert_eq!(p.selected, 0);

        let kp_enter = ghostwave::keymap::KeyPress {
            key: Key::Enter,
            mods: Mods::empty(),
            text: None,
        };
        assert_eq!(
            p.handle_keypress(&kp_enter),
            Some(PaletteAction::Execute(Action::Quit))
        );

        let kp_esc = ghostwave::keymap::KeyPress {
            key: Key::Escape,
            mods: Mods::empty(),
            text: None,
        };
        assert_eq!(p.handle_keypress(&kp_esc), Some(PaletteAction::Close));
    }

    #[test]
    fn fuzzy_matching_scoring() {
        assert!(fuzzy_match("", "anything").is_some());
        assert!(fuzzy_match("abc", "abcdef").is_some());
        assert!(fuzzy_match("abc", "axbycz").is_some());
        assert!(fuzzy_match("xyz", "abc").is_none());

        let prefix_score = fuzzy_match("term", "terminal").unwrap();
        let middle_score = fuzzy_match("term", "my_terminal").unwrap();
        assert!(prefix_score > middle_score);
    }
}
