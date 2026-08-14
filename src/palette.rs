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
            PaletteItem { label: "Zoom Pane".into(), action: ToggleZoom },
            PaletteItem { label: "Toggle Pane Frames".into(), action: ToggleFrames },
            PaletteItem { label: "Rename Tab".into(), action: RenameTab },
            PaletteItem { label: "Rename Pane".into(), action: RenamePane },
            PaletteItem { label: "Find in Terminal".into(), action: OpenSearch },
            PaletteItem { label: "Copy".into(), action: Copy },
            PaletteItem { label: "Paste".into(), action: Paste },
            PaletteItem { label: "Clear Terminal".into(), action: ClearTerminal },
            PaletteItem { label: "New AI Chat".into(), action: NewAi },
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
