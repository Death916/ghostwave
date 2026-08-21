//! Command palette (waveterm Ctrl+Shift+P style).

use crate::keymap::Action;

#[derive(Clone, Debug)]
pub struct PaletteItem {
    pub label: String,
    pub action: Action,
}

#[derive(Debug)]
pub struct CommandPalette {
    pub input: String,
    pub cursor: usize,
    pub items: Vec<PaletteItem>,
}

impl CommandPalette {
    pub fn new(items: Vec<PaletteItem>) -> Self {
        Self {
            input: String::new(),
            cursor: 0,
            items,
        }
    }

    /// Filtered items (owned): static commands filtered by input, plus
    /// dynamic "open <path>" / "ssh <host>" entries.
    pub fn filtered(&self) -> Vec<PaletteItem> {
        if let Some(path) = self.input.strip_prefix("open ") {
            let p = path.trim();
            if !p.is_empty() {
                return vec![PaletteItem {
                    label: format!("Open file: {p}"),
                    action: crate::keymap::Action::OpenFile(p.to_string()),
                }];
            }
        }
        if let Some(host) = self.input.strip_prefix("ssh ") {
            let h = host.trim();
            if !h.is_empty() {
                return vec![PaletteItem {
                    label: format!("Connect SSH: {h}"),
                    action: crate::keymap::Action::ConnectSsh(h.to_string()),
                }];
            }
        }
        if let Some(host) = self.input.strip_prefix("ds ") {
            let h = host.trim();
            if !h.is_empty() {
                return vec![PaletteItem {
                    label: format!("Durable SSH (ghostd): {h}"),
                    action: crate::keymap::Action::ConnectDurableSsh(h.to_string()),
                }];
            }
        }
        let q = self.input.to_lowercase();
        let mut v: Vec<PaletteItem> = self
            .items
            .iter()
            .filter(|i| q.is_empty() || i.label.to_lowercase().contains(&q))
            .cloned()
            .collect();
        if self.input.starts_with("theme") {
            v.retain(|i| i.label.starts_with("Theme:"));
        }
        v
    }

    pub fn selected(&self) -> Option<PaletteItem> {
        let f = self.filtered();
        if f.is_empty() {
            return None;
        }
        Some(f[self.cursor.min(f.len() - 1)].clone())
    }

    pub fn type_char(&mut self, c: char) {
        self.input.push(c);
        self.cursor = 0;
    }

    pub fn backspace(&mut self) {
        self.input.pop();
        self.cursor = 0;
    }

    pub fn up(&mut self) {
        let n = self.filtered().len();
        if n > 0 {
            self.cursor = (self.cursor + n - 1) % n;
        }
    }

    pub fn down(&mut self) {
        let n = self.filtered().len();
        if n > 0 {
            self.cursor = (self.cursor + 1) % n;
        }
    }

    /// Static command list (built by the app with theme items).
    pub fn default_items(themes: &[&'static str]) -> Vec<PaletteItem> {
        use crate::keymap::Action::*;
        let mut items = vec![
            PaletteItem { label: "Kill Remote Session".into(), action: KillRemoteSession },
            PaletteItem { label: "New Tab".into(), action: NewTab },
            PaletteItem { label: "New Pane (down)".into(), action: NewPaneDown },
            PaletteItem { label: "New Pane (right)".into(), action: NewPaneRight },
            PaletteItem { label: "New Pane (stacked)".into(), action: NewPaneStacked },
            PaletteItem { label: "Close Pane".into(), action: ClosePane },
            PaletteItem { label: "Close Tab".into(), action: CloseTab },
            PaletteItem { label: "Focus Left".into(), action: MoveFocus(crate::layout::Dir::Left) },
            PaletteItem { label: "Focus Right".into(), action: MoveFocus(crate::layout::Dir::Right) },
            PaletteItem { label: "Focus Up".into(), action: MoveFocus(crate::layout::Dir::Up) },
            PaletteItem { label: "Focus Down".into(), action: MoveFocus(crate::layout::Dir::Down) },
            PaletteItem { label: "Next Pane".into(), action: SwitchFocus },
            PaletteItem { label: "Fullscreen Pane (Window)".into(), action: ToggleZoom },
            PaletteItem { label: "Toggle Pane Frames".into(), action: ToggleFrames },
            PaletteItem { label: "Rename Tab".into(), action: RenameTab },
            PaletteItem { label: "Rename Pane".into(), action: RenamePane },
            PaletteItem { label: "Find in Terminal".into(), action: OpenSearch },
            PaletteItem { label: "Copy".into(), action: Copy },
            PaletteItem { label: "Paste".into(), action: Paste },
            PaletteItem { label: "Clear Terminal".into(), action: ClearTerminal },
            PaletteItem { label: "New AI Chat".into(), action: NewAi },
            PaletteItem { label: "Ghostwave (Screensaver Visualizer)".into(), action: Ghostwave },
            PaletteItem { label: "Toggle Synchronized Panes (Broadcast)".into(), action: ToggleSyncPanes },
            PaletteItem { label: "Toggle Scratchpad Drawer".into(), action: ToggleScratchpad },
            PaletteItem { label: "Quit".into(), action: Quit },
        ];
        for t in themes {
            items.push(PaletteItem {
                label: format!("Theme: {t}"),
                action: Theme(t.to_string()),
            });
        }
        items
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymap::Action;

    fn palette(labels: &[&str]) -> CommandPalette {
        let items = labels.iter().map(|l| PaletteItem { label: l.to_string(), action: Action::Quit }).collect();
        CommandPalette::new(items)
    }

    #[test]
    fn filtered_empty_returns_all() {
        let p = palette(&["Foo", "Bar", "Baz"]);
        let all = p.filtered();
        assert_eq!(all.len(), 3);
        // Must be the original labels, not synthesized entries
        let labels: Vec<_> = all.iter().map(|i| i.label.as_str()).collect();
        assert!(labels.contains(&"Foo") && labels.contains(&"Bar") && labels.contains(&"Baz"));
        assert!(all.iter().all(|i| matches!(i.action, Action::Quit)), "empty filter returns original actions unchanged");
    }

    #[test]
    fn filtered_case_insensitive_substring() {
        let p = {
            let mut c = palette(&["New Tab", "Close Tab", "New Pane"]);
            c.input = "tab".into();
            c
        };
        let out = p.filtered();
        assert_eq!(out.len(), 2);
        assert!(out.iter().any(|i| i.label == "New Tab"));
        assert!(out.iter().any(|i| i.label == "Close Tab"));
    }

    #[test]
    fn filtered_no_match_is_empty() {
        let mut p = palette(&["Foo", "Bar"]);
        p.input = "zzz".into();
        assert!(p.filtered().is_empty());
    }

    #[test]
    fn dynamic_open_prefix() {
        let mut p = palette(&["Foo"]);
        p.input = "open /tmp/file.txt".into();
        let out = p.filtered();
        assert_eq!(out.len(), 1, "open prefix should produce exactly one synthetic entry");
        assert_eq!(out[0].label, "Open file: /tmp/file.txt");
        assert!(matches!(&out[0].action, Action::OpenFile(s) if s == "/tmp/file.txt"), "action should carry the path verbatim");
        // trimmed value: "open   /tmp/a.txt  " should still extract /tmp/a.txt
        p.input = "open   /tmp/a.txt  ".into();
        let out2 = p.filtered();
        assert!(matches!(&out2[0].action, Action::OpenFile(s) if s == "/tmp/a.txt"));
        // bare "open " with no path falls through to normal filter, not a synthetic entry
        p.input = "open ".into();
        let bare = p.filtered();
        assert!(bare.iter().all(|i| !matches!(i.action, Action::OpenFile(_))), "bare 'open ' should not synthesize OpenFile");
    }

    #[test]
    fn dynamic_ssh_prefix() {
        let mut p = palette(&["Foo"]);
        p.input = "ssh myhost".into();
        let out = p.filtered();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].label, "Connect SSH: myhost");
        assert!(matches!(&out[0].action, Action::ConnectSsh(s) if s == "myhost"), "ssh action should carry host verbatim");
        // trims surrounding whitespace
        p.input = "ssh   myhost  ".into();
        let out2 = p.filtered();
        assert!(matches!(&out2[0].action, Action::ConnectSsh(s) if s == "myhost"));
        // bare "ssh " is not a synthetic entry
        p.input = "ssh ".into();
        assert!(p.filtered().iter().all(|i| !matches!(i.action, Action::ConnectSsh(_))));
    }

    #[test]
    fn dynamic_ds_prefix() {
        let mut p = palette(&["Foo"]);
        p.input = "ds myhost".into();
        let out = p.filtered();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].label, "Durable SSH (ghostd): myhost");
        assert!(matches!(&out[0].action, Action::ConnectDurableSsh(s) if s == "myhost"));
        p.input = "ds ".into();
        assert!(p.filtered().iter().all(|i| !matches!(i.action, Action::ConnectDurableSsh(_))));
    }

    #[test]
    fn theme_prefix_filters_to_theme_items() {
        let all = CommandPalette::default_items(&["tokyonight", "dracula"]);
        let non_theme_count = all.iter().filter(|i| !i.label.starts_with("Theme:")).count();
        assert!(non_theme_count > 0, "default items should have non-theme entries to filter out");
        let mut p = CommandPalette::new(all);
        p.input = "theme".into();
        let out = p.filtered();
        assert!(!out.is_empty());
        assert!(out.iter().all(|i| i.label.starts_with("Theme:")), "theme filter must hide non-theme items");
        // And without the prefix those items are not hidden — "new" should find New Tab, not themes
        p.input = "new".into();
        let out2 = p.filtered();
        assert!(out2.iter().any(|i| i.label == "New Tab"));
    }

    #[test]
    fn selected_clamps_cursor() {
        let mut p = palette(&["A", "B", "C"]);
        p.cursor = 99;
        // filtered has 3 items, cursor 99 should clamp to last
        let sel = p.selected().unwrap();
        assert_eq!(sel.label, "C");
    }

    #[test]
    fn selected_none_when_filtered_empty() {
        let mut p = palette(&["A"]);
        p.input = "zzz".into();
        assert!(p.selected().is_none());
    }

    #[test]
    fn nav_up_down_wraps() {
        let mut p = palette(&["A", "B", "C"]);
        assert_eq!(p.cursor, 0);
        p.up();
        assert_eq!(p.cursor, 2, "up from 0 wraps to last");
        p.down();
        assert_eq!(p.cursor, 0, "down from last wraps to 0");
        p.down();
        assert_eq!(p.cursor, 1);
        p.up();
        assert_eq!(p.cursor, 0);
        // single item: up/down are no-ops
        let mut q = palette(&["Only"]);
        q.up();
        assert_eq!(q.cursor, 0);
        q.down();
        assert_eq!(q.cursor, 0);
    }

    #[test]
    fn type_and_backspace_reset_cursor() {
        let mut p = palette(&["A", "B", "C"]);
        p.cursor = 2;
        p.type_char('x');
        assert_eq!(p.cursor, 0);
        p.cursor = 2;
        p.backspace();
        assert_eq!(p.cursor, 0);
    }

    #[test]
    fn default_items_include_theme_entries() {
        let items = CommandPalette::default_items(&["tokyonight", "dracula"]);
        assert!(items.iter().any(|i| i.label == "Theme: tokyonight" && matches!(&i.action, Action::Theme(s) if s == "tokyonight")));
        assert!(items.iter().any(|i| i.label == "Theme: dracula" && matches!(&i.action, Action::Theme(s) if s == "dracula")));
        // theme items carry the correct Action::Theme payload, not Quit or similar
        let theme_actions: Vec<_> = items.iter().filter(|i| i.label.starts_with("Theme:")).collect();
        assert_eq!(theme_actions.len(), 2);
        for t in &theme_actions {
            assert!(matches!(t.action, Action::Theme(_)), "theme label should map to Action::Theme, got {:?}", t.action);
        }
        assert!(items.iter().any(|i| i.action == Action::ToggleSyncPanes));
        assert!(items.iter().any(|i| i.action == Action::ToggleScratchpad));
        assert!(items.iter().any(|i| i.action == Action::Ghostwave));
    }
}
