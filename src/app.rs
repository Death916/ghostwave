//! Application state and logic: tabs, panes, modes, actions, layout.

use std::collections::HashSet;
use std::time::Instant;

use libghostty_vt::key::{self, Key, Mods};

use crate::{
    ai::AiPane,
    config::Config,
    fonts::FontStack,
    keymap::{self, Action, Dispatch, KeyPress, Mode},
    layout::{self, Dir, Node, Rect, SplitDir},
    palette::CommandPalette,
    preview::PreviewPane,
    term::{CellDims, MouseEv, MouseEvKind, TermPane},
    theme::{self, Theme},
};

pub const TAB_BAR_H: f32 = 30.0;
pub const STATUS_BAR_H: f32 = 26.0;

pub enum Pane {
    Term(TermPane),
    Preview(PreviewPane),
    Ai(AiPane),
    /// Transient placeholder used during reindexing; never rendered.
    Closed,
}

pub struct Tab {
    pub name: String,
    pub root: Node,
    pub panes: Vec<Pane>,
    pub active: usize,
    pub zoomed: HashSet<usize>,
}

impl Tab {
    fn new(initial: Pane, name: String) -> Self {
        Self {
            name,
            root: Node::Leaf { pane: 0 },
            panes: vec![initial],
            active: 0,
            zoomed: HashSet::new(),
        }
    }

    fn term(&self, id: usize) -> Option<&TermPane> {
        match self.panes.get(id) {
            Some(Pane::Term(t)) => Some(t),
            _ => None,
        }
    }

    fn term_mut(&mut self, id: usize) -> Option<&mut TermPane> {
        match self.panes.get_mut(id) {
            Some(Pane::Term(t)) => Some(t),
            _ => None,
        }
    }

    /// Pane rects in content area.
    fn pane_rects(&self, content: Rect, frames: bool) -> Vec<(usize, Rect)> {
        let mut rects = layout::layout(&self.root, content);
        if frames {
            for (_, r) in rects.iter_mut() {
                *r = r.inset(2.0);
            }
        }
        rects
    }

    fn active_zoomed(&self) -> bool {
        self.zoomed.contains(&self.active)
    }
}

pub struct App {
    pub tabs: Vec<Tab>,
    pub active_tab: usize,
    pub mode: Mode,
    pub theme: Theme,
    pub theme_name: String,
    pub show_frames: bool,
    pub palette: Option<CommandPalette>,
    pub ai_visible: bool,
    pub toast: Option<(String, Instant)>,
    pub last_tab: Option<usize>,
    pub rename_buf: String,
    pub search_mode_active: bool,
    pub cell: CellDims,
    pub font_px: u16,
    pub config: Config,
    pub font: macroquad::text::Font,
    pub font_stack: FontStack,
    /// Repeat synthesis state: (code, last_press, last_repeat).
    repeats: std::collections::HashMap<macroquad::prelude::KeyCode, (Instant, Instant)>,
    pub quit: bool,
    /// Pane id whose terminal gets the next mouse event (hit test cache).
    mouse_pane: Option<(usize, usize)>, // (tab, pane)
}

impl App {
    pub fn new(config: Config, font: macroquad::text::Font, font_stack: FontStack) -> crate::Result<Self> {
        let theme_name = config.theme.clone();
        let theme = theme::by_name(&theme_name);

        // Cell metrics: width from the font's advance, height from real line
        // metrics (ascent + descent) so ascenders/descenders and the block
        // cursor fit inside the cell. Baseline anchors glyphs in the cell so
        // cursor and text line up (measure_text on "M" returns cap-height and
        // offset_y≈0 — unusable as a baseline, that caused the offset cursor).
        let dpi = macroquad::window::screen_dpi_scale();
        let measured = macroquad::text::measure_text(
            "M",
            Some(&font),
            config.font_size as u16,
            dpi,
        );
        let font_px = (config.font_size * dpi).round() as u16;
        let (ascent, descent) = font_stack.line_metrics(font_px as f32);
        let cell_w = (measured.width.max(1.0)).ceil() as u32;
        let cell_h = (ascent + descent + config.row_gap).max(8.0).ceil() as u32;
        let cell = CellDims { w: cell_w, h: cell_h, baseline: ascent + config.row_gap / 2.0 };

        eprintln!(
            "ghostwave dbg: dpi={dpi} font_size={} font_px={font_px} cell={}x{} baseline={} asc={ascent} desc={descent} screen={}x{}",
            config.font_size,
            cell_w,
            cell_h,
            cell.baseline,
            macroquad::window::screen_width(),
            macroquad::window::screen_height(),
        );

        // Create the initial shell pane BEFORE building the App (there are
        // no tabs yet, so new_term_pane would index an empty vec).
        let cols: u16 = 100;
        let rows: u16 = 30;
        let initial = TermPane::new(
            cols,
            rows,
            cell,
            config.shell.as_deref(),
            &[],
            None,
            config.scrollback,
        )?;

        let mut app = Self {
            tabs: vec![Tab::new(Pane::Term(initial), "Tab 1".into())],
            active_tab: 0,
            mode: Mode::Normal,
            theme,
            theme_name,
            show_frames: true,
            palette: None,
            ai_visible: false,
            toast: None,
            last_tab: None,
            rename_buf: String::new(),
            search_mode_active: false,
            cell,
            font_px,
            config,
            font,
            font_stack,
            repeats: std::collections::HashMap::new(),
            quit: false,
            mouse_pane: None,
        };

        // Apply the configured theme's fg/bg to the initial terminal.
        app.apply_terminal_colors();

        Ok(app)
    }

    // ---- pane creation ---------------------------------------------------

    fn new_term_pane(
        &mut self,
        cols: u16,
        rows: u16,
        program: Option<&str>,
        args: &[&str],
        cwd: Option<&str>,
    ) -> crate::Result<usize> {
        let shell = program.or(self.config.shell.as_deref());
        let pane = TermPane::new(cols, rows, self.cell, shell, args, cwd, self.config.scrollback)?;
        let id = {
            let tab = self.current_tab_mut();
            let id = tab.panes.len();
            tab.panes.push(Pane::Term(pane));
            id
        };
        // Fresh terminals start with libghostty's default colors (near-black);
        // push the theme's default fg/bg so new panes match existing ones.
        self.apply_terminal_colors();
        Ok(id)
    }

    pub fn current_tab(&self) -> &Tab {
        &self.tabs[self.active_tab]
    }

    pub fn current_tab_mut(&mut self) -> &mut Tab {
        &mut self.tabs[self.active_tab]
    }

    pub fn active_pane_id(&self) -> usize {
        self.current_tab().active
    }

    pub fn active_term(&self) -> Option<&TermPane> {
        self.current_tab().term(self.active_pane_id())
    }

    pub fn active_term_mut(&mut self) -> Option<&mut TermPane> {
        let id = self.active_pane_id();
        self.current_tab_mut().term_mut(id)
    }

    // ---- actions ---------------------------------------------------------

    pub fn execute(&mut self, action: Action) {
        match action {
            Action::NewPane | Action::NewPaneDown | Action::NewPaneRight | Action::NewPaneStacked => {
                let dir = match action {
                    Action::NewPaneDown => SplitDir::Vertical,
                    Action::NewPaneRight => SplitDir::Horizontal,
                    _ => SplitDir::Vertical,
                };
                self.split(dir, action == Action::NewPaneStacked);
            }
            Action::ClosePane => self.close_pane(),
            Action::MoveFocus(dir) => self.focus(dir),
            Action::SwitchFocus => {
                let next = layout::next_leaf(&self.current_tab().root, self.active_pane_id());
                if let Some(n) = next {
                    self.current_tab_mut().active = n;
                }
            }
            Action::ToggleZoom => self.toggle_zoom(),
            Action::ToggleFrames => {
                self.show_frames = !self.show_frames;
                self.layout_all();
            }
            Action::RenamePane => {
                self.mode = Mode::RenamePane;
                self.rename_buf.clear();
            }
            Action::CycleStack => self.cycle_stack(),
            Action::NewTab => self.new_tab(),
            Action::CloseTab => self.close_tab(),
            Action::NextTab => self.goto_tab((self.active_tab + 1) % self.tabs.len()),
            Action::PrevTab => {
                let n = self.tabs.len();
                self.goto_tab((self.active_tab + n - 1) % n);
            }
            Action::GoToTab(i) => {
                let n = self.tabs.len();
                if i >= 1 && (i as usize) <= n {
                    self.goto_tab(i as usize - 1);
                }
            }
            Action::ToggleLastTab => {
                if let Some(last) = self.last_tab {
                    if last < self.tabs.len() {
                        self.goto_tab(last);
                    }
                }
            }
            Action::RenameTab => {
                self.mode = Mode::RenameTab;
                self.rename_buf.clear();
            }
            Action::BreakPane => self.break_pane(),
            Action::Resize(dir, increase) => {
                let delta = if increase { 0.06 } else { -0.06 };
                self.resize(dir, delta);
            }
            Action::ResizeAll(increase) => {
                let delta = if increase { 0.06 } else { -0.06 };
                self.resize(Dir::Right, delta);
                self.resize(Dir::Down, delta);
            }
            Action::ScrollUp(n) => {
                if let Some(t) = self.active_term_mut() {
                    t.scroll(-n);
                }
            }
            Action::ScrollDown(n) => {
                if let Some(t) = self.active_term_mut() {
                    t.scroll(n);
                }
            }
            Action::PageUp => {
                if let Some(t) = self.active_term_mut() {
                    t.scroll_page(-1);
                }
            }
            Action::PageDown => {
                if let Some(t) = self.active_term_mut() {
                    t.scroll_page(1);
                }
            }
            Action::HalfPageUp => {
                if let Some(t) = self.active_term_mut() {
                    t.scroll_page(-1);
                }
            }
            Action::HalfPageDown => {
                if let Some(t) = self.active_term_mut() {
                    t.scroll_page(1);
                }
            }
            Action::ScrollToBottom => {
                if let Some(t) = self.active_term_mut() {
                    t.scroll_to_bottom();
                }
            }
            Action::EditScrollback => self.toast("edit-scrollback: not implemented yet"),
            Action::SearchStart => {
                self.mode = Mode::EnterSearch;
                self.search_mode_active = true;
                if let Some(t) = self.active_term_mut() {
                    t.search.start();
                }
            }
            Action::OpenSearch => {
                self.mode = Mode::EnterSearch;
                self.search_mode_active = true;
                if let Some(t) = self.active_term_mut() {
                    t.search.start();
                }
            }
            Action::SearchNext => {
                if let Some(t) = self.active_term_mut() {
                    t.search.next_match();
                    t.force_repaint = true;
                }
            }
            Action::SearchPrev => {
                if let Some(t) = self.active_term_mut() {
                    t.search.prev_match();
                    t.force_repaint = true;
                }
            }
            Action::SearchToggleCase => {
                if let Some(t) = self.active_term_mut() {
                    t.search.case_sensitive = !t.search.case_sensitive;
                    t.search.dirty = true;
                }
            }
            Action::SearchToggleWrap => {
                if let Some(t) = self.active_term_mut() {
                    t.search.wrap = !t.search.wrap;
                }
            }
            Action::SearchToggleWholeWord => {
                if let Some(t) = self.active_term_mut() {
                    t.search.whole_word = !t.search.whole_word;
                    t.search.dirty = true;
                }
            }
            Action::MovePane(dir) => self.move_pane(dir),
            Action::MovePaneNext => self.move_pane_next(),
            Action::MovePanePrev => self.move_pane_prev(),
            Action::Quit => self.quit = true,
            Action::Lock => self.mode = Mode::Locked,
            Action::Unlock => self.mode = Mode::Normal,
            Action::OpenCommandBar => {
                let items = CommandPalette::default_items(&theme::names());
                self.palette = Some(CommandPalette::new(items));
            }
            Action::Copy => {
                if let Some(t) = self.active_term_mut() {
                    let ok = t.copy_selection().unwrap_or(false);
                    self.toast(if ok { "copied" } else { "nothing to copy" });
                }
            }
            Action::Paste => {
                if let Some(t) = self.active_term_mut() {
                    t.paste_clipboard();
                }
            }
            Action::ClearTerminal => {
                if let Some(t) = self.active_term_mut() {
                    let _ = t.clear_terminal();
                }
            }
            Action::OpenFile(path) => self.open_file(path),
            Action::ConnectSsh(host) => self.connect_ssh(&host),
            Action::NewAi => self.new_ai_pane(),
            Action::ToggleAi => self.ai_visible = !self.ai_visible,
            Action::Theme(name) => self.apply_theme(&name),
            Action::FocusPane(i) => {
                let leaves = self.current_tab().root.leaves();
                if let Some((_, pane)) = leaves.get(i as usize) {
                    self.current_tab_mut().active = *pane;
                }
            }
            Action::MoveTab(dir) => {
                let n = self.tabs.len();
                let next = match dir {
                    Dir::Left => (self.active_tab + n - 1) % n,
                    _ => (self.active_tab + 1) % n,
                };
                self.tabs.swap(self.active_tab, next);
                self.active_tab = next;
            }
            Action::Toast(msg) => self.toast(msg),
            Action::ExitMode => self.mode = Mode::Normal,
            Action::ConfirmSearch => {
                // Enter in the search box: switch to Search navigation mode.
                self.mode = Mode::Search;
                if let Some(t) = self.active_term_mut() {
                    t.search.dirty = true;
                }
            }
            Action::ConfirmRename => {
                let name = std::mem::take(&mut self.rename_buf);
                let was_tab = self.mode == Mode::RenameTab;
                self.mode = Mode::Normal;
                if !name.is_empty() {
                    if was_tab {
                        self.current_tab_mut().name = name;
                    } else if let Some(t) = self.active_term_mut() {
                        t.name = name;
                    }
                }
            }
        }
    }

    pub fn toast(&mut self, msg: &str) {
        self.toast = Some((msg.to_string(), Instant::now()));
    }

    // ---- tab ops ---------------------------------------------------------

    pub fn new_tab(&mut self) {
        let cols = 100;
        let rows = 30;
        let pane = match TermPane::new(
            cols,
            rows,
            self.cell,
            self.config.shell.as_deref(),
            &[],
            None,
            self.config.scrollback,
        ) {
            Ok(p) => Pane::Term(p),
            Err(e) => {
                eprintln!("ghostwave: failed to spawn shell: {e}");
                return;
            }
        };
        self.last_tab = Some(self.active_tab);
        let name = format!("Tab {}", self.tabs.len() + 1);
        self.tabs.push(Tab::new(pane, name));
        self.active_tab = self.tabs.len() - 1;
        self.apply_terminal_colors();
        self.layout_all();
    }

    pub fn close_tab(&mut self) {
        if self.tabs.len() <= 1 {
            // Last tab: replace with a fresh shell instead of closing.
            let cols = 100;
            let rows = 30;
            let pane = match TermPane::new(
                cols,
                rows,
                self.cell,
                self.config.shell.as_deref(),
                &[],
                None,
                self.config.scrollback,
            ) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("ghostwave: failed to spawn shell: {e}");
                    return;
                }
            };
            let tab = self.current_tab_mut();
            tab.panes.clear();
            tab.panes.push(Pane::Term(pane));
            tab.root = Node::Leaf { pane: 0 };
            tab.active = 0;
            tab.zoomed.clear();
            self.apply_terminal_colors();
            return;
        }
        self.tabs.remove(self.active_tab);
        if self.active_tab >= self.tabs.len() {
            self.active_tab = self.tabs.len() - 1;
        }
        self.layout_all();
    }

    pub fn goto_tab(&mut self, idx: usize) {
        if idx < self.tabs.len() {
            self.last_tab = Some(self.active_tab);
            self.active_tab = idx;
            self.layout_all();
        }
    }

    fn break_pane(&mut self) {
        // Move the focused pane to a new tab.
        let pane = {
            let tab = self.current_tab_mut();
            let id = tab.active;
            let path = match layout::path_to_pane(&tab.root, id) {
                Some(p) => p,
                None => return,
            };
            layout::close_at(&mut tab.root, &path);
            std::mem::replace(&mut tab.panes[id], Pane::Closed)
        };
        self.remove_dangling_panes();
        self.last_tab = Some(self.active_tab);
        let name = format!("Tab {}", self.tabs.len() + 1);
        self.tabs.push(Tab::new(pane, name));
        self.active_tab = self.tabs.len() - 1;
        self.layout_all();
    }

    // ---- pane ops --------------------------------------------------------

    pub fn split(&mut self, dir: SplitDir, stacked: bool) {
        let cols = 80;
        let rows = 24;
        let id = match self.new_term_pane(cols, rows, None, &[], None) {
            Ok(id) => id,
            Err(e) => {
                eprintln!("ghostwave: failed to spawn shell: {e}");
                return;
            }
        };
        let tab = self.current_tab_mut();
        let path = layout::path_to_pane(&tab.root, tab.active).unwrap_or_default();
        if stacked {
            layout::stack_at(&mut tab.root, &path, id);
        } else {
            layout::split_at(&mut tab.root, &path, dir, id);
        }
        tab.active = id;
        self.layout_all();
    }

    pub fn close_pane(&mut self) {
        let root_empty = {
            let tab = self.current_tab_mut();
            let path = match layout::path_to_pane(&tab.root, tab.active) {
                Some(p) => p,
                None => return,
            };
            layout::close_at(&mut tab.root, &path);
            // close_at marks the root as an empty sentinel when the last
            // leaf was removed.
            let empty = matches!(tab.root, Node::Leaf { pane: usize::MAX });
            if !empty {
                // Remove the closed pane from the vec (by its old id).
                if let Some(id) = layout::path_to_pane(&tab.root, usize::MAX) {
                    let _ = id;
                }
            }
            empty
        };

        if root_empty {
            // No panes left in this tab: spawn a fresh shell.
            let cols = 100;
            let rows = 30;
            let pane = match TermPane::new(
                cols,
                rows,
                self.cell,
                self.config.shell.as_deref(),
                &[],
                None,
                self.config.scrollback,
            ) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("ghostwave: failed to spawn shell: {e}");
                    return;
                }
            };
            let tab = self.current_tab_mut();
            tab.panes.clear();
            tab.panes.push(Pane::Term(pane));
            tab.root = Node::Leaf { pane: 0 };
            tab.active = 0;
            self.apply_terminal_colors();
            self.layout_all();
            return;
        }

        self.remove_dangling_panes();
        self.layout_all();
    }

    /// Remove panes from the vec that are no longer referenced by the tree,
    /// then remap the tree to the compacted ids.
    fn remove_dangling_panes(&mut self) {
        let tab = self.current_tab_mut();
        // Collect referenced ids.
        let mut referenced: HashSet<usize> = HashSet::new();
        collect_tree_ids(&tab.root, &mut referenced);
        // Build new vec of referenced panes in ascending old-id order.
        let mut new_panes: Vec<Pane> = Vec::new();
        let mut remap: std::collections::HashMap<usize, usize> = std::collections::HashMap::new();
        let mut old_ids: Vec<usize> = referenced.into_iter().collect();
        old_ids.sort_unstable();
        for (new_id, old_id) in old_ids.iter().enumerate() {
            remap.insert(*old_id, new_id);
            let pane = std::mem::replace(&mut tab.panes[*old_id], Pane::Closed);
            new_panes.push(pane);
        }
        remap_tree(&mut tab.root, &remap);
        tab.panes = new_panes;
        tab.active = remap.get(&tab.active).copied().unwrap_or(0);
        tab.zoomed.retain(|id| remap.contains_key(id));
    }

    pub fn focus(&mut self, dir: Dir) {
        let content = self.content_rect();
        let rects = self.current_tab().pane_rects(content, self.show_frames);
        let next = layout::focus_in_direction(&rects, self.active_pane_id(), dir);
        if let Some(n) = next {
            self.current_tab_mut().active = n;
        }
    }

    pub fn toggle_zoom(&mut self) {
        let tab = self.current_tab_mut();
        if !tab.zoomed.insert(tab.active) {
            tab.zoomed.remove(&tab.active);
        }
    }

    fn cycle_stack(&mut self) {
        let tab = self.current_tab_mut();
        let path = layout::path_to_pane(&tab.root, tab.active).unwrap_or_default();
        if layout::cycle_stack(&mut tab.root, &path) {
            let id = tab.root.pane_id_of_path(&path);
            tab.active = id;
        } else {
            self.toast("not in a stack (create with pane-mode s)");
        }
    }

    pub fn resize(&mut self, dir: Dir, delta: f32) {
        let tab = self.current_tab_mut();
        let path = match layout::path_to_pane(&tab.root, tab.active) {
            Some(p) => p,
            None => return,
        };
        layout::resize_at(&mut tab.root, &path, dir, delta);
    }

    pub fn move_pane(&mut self, dir: Dir) {
        let content = self.content_rect();
        let rects = self.current_tab().pane_rects(content, self.show_frames);
        let from = self.active_pane_id();
        let Some(target) = layout::focus_in_direction(&rects, from, dir) else {
            return;
        };
        let tab = self.current_tab_mut();
        let (pa, pb) = match (
            layout::path_to_pane(&tab.root, from),
            layout::path_to_pane(&tab.root, target),
        ) {
            (Some(a), Some(b)) => (a, b),
            _ => return,
        };
        layout::swap_panes(&mut tab.root, &pa, &pb);
        tab.active = target;
    }

    pub fn move_pane_next(&mut self) {
        let tab = self.current_tab_mut();
        let from = tab.active;
        let Some(next) = layout::leaf_after(&tab.root, from) else {
            return;
        };
        let (pa, pb) = match (
            layout::path_to_pane(&tab.root, from),
            layout::path_to_pane(&tab.root, next),
        ) {
            (Some(a), Some(b)) => (a, b),
            _ => return,
        };
        layout::swap_panes(&mut tab.root, &pa, &pb);
        tab.active = next;
    }

    pub fn move_pane_prev(&mut self) {
        let tab = self.current_tab_mut();
        let from = tab.active;
        let leaves = tab.root.leaves();
        let idx = leaves.iter().position(|(_, p)| *p == from).unwrap_or(0);
        let prev = if idx == 0 {
            leaves.last().map(|(_, p)| *p)
        } else {
            leaves.get(idx - 1).map(|(_, p)| *p)
        };
        let Some(prev) = prev else { return };
        let (pa, pb) = match (
            layout::path_to_pane(&tab.root, from),
            layout::path_to_pane(&tab.root, prev),
        ) {
            (Some(a), Some(b)) => (a, b),
            _ => return,
        };
        layout::swap_panes(&mut tab.root, &pa, &pb);
        tab.active = prev;
    }

    pub fn open_file(&mut self, path: String) {
        let preview = PreviewPane::open(path.clone());
        self.push_side_pane(Pane::Preview(preview));
        self.toast(&format!("opened {path}"));
    }

    pub fn connect_ssh(&mut self, host: &str) {
        let cols = 100;
        let rows = 30;
        match self.new_term_pane(cols, rows, Some("ssh"), &[host], None) {
            Ok(id) => {
                self.current_tab_mut().active = id;
                self.toast(&format!("connecting to {host}"));
            }
            Err(e) => self.toast(&format!("ssh failed: {e}")),
        }
    }

    pub fn new_ai_pane(&mut self) {
        let pane = Pane::Ai(AiPane::new(self.config.ai.clone()));
        self.push_side_pane(pane);
        self.ai_visible = true;
    }

    /// Add a non-terminal pane as a right split.
    fn push_side_pane(&mut self, pane: Pane) {
        let tab = self.current_tab_mut();
        let id = tab.panes.len();
        tab.panes.push(pane);
        let path = layout::path_to_pane(&tab.root, tab.active).unwrap_or_default();
        layout::split_at(&mut tab.root, &path, SplitDir::Horizontal, id);
        tab.active = id;
        self.layout_all();
    }

    pub fn apply_theme(&mut self, name: &str) {
        if let Some(t) = theme::by_name_opt(name) {
            self.theme = t;
            self.theme_name = name.to_string();
            self.apply_terminal_colors();
            self.toast(&format!("theme: {name}"));
        }
    }

    /// Push the theme's default fg/bg into every pane's terminal so the
    /// grid renders with theme colors (renderer reads colors.background/
    /// colors.foreground from the render-state snapshot).
    pub fn apply_terminal_colors(&mut self) {
        use libghostty_vt::style::RgbColor;
        let (fg, bg) = (self.theme.fg, self.theme.bg);
        for tab in self.tabs.iter_mut() {
            for pane in tab.panes.iter_mut() {
                if let Pane::Term(t) = pane {
                    let _ = t.term.set_default_fg_color(Some(RgbColor { r: fg.0, g: fg.1, b: fg.2 }));
                    let _ = t.term.set_default_bg_color(Some(RgbColor { r: bg.0, g: bg.1, b: bg.2 }));
                }
            }
        }
    }

    // ---- layout ----------------------------------------------------------

    pub fn content_rect(&self) -> Rect {
        let w = macroquad::window::screen_width();
        let h = macroquad::window::screen_height();
        Rect {
            x: 0.0,
            y: TAB_BAR_H,
            w,
            h: (h - TAB_BAR_H - STATUS_BAR_H).max(1.0),
        }
    }

    /// Recompute each terminal pane's grid from its rect. Call on resize/
    /// layout changes.
    pub fn layout_all(&mut self) {
        let content = self.content_rect();
        let frames = self.show_frames;
        let cell = self.cell;
        let pad = self.config.padding;
        let tab = self.current_tab_mut();
        // Zoomed pane gets the full content rect (and a matching grid).
        let mut rects = tab.pane_rects(content, frames);
        if !tab.zoomed.is_empty() {
            rects.retain(|(id, _)| tab.zoomed.contains(id));
            for (_, r) in rects.iter_mut() {
                *r = content;
            }
        }
        for (id, rect) in rects {
            if let Some(Pane::Term(t)) = tab.panes.get_mut(id) {
                let cols = ((rect.w - 2.0 * pad) / cell.w as f32).max(1.0) as u16;
                let rows = ((rect.h - 2.0 * pad) / cell.h as f32).max(1.0) as u16;
                eprintln!("ghostwave dbg: pane {id} rect={rect:?} cols={cols} rows={rows}");
                let _ = t.set_grid(cols, rows, cell);
            }
        }
    }

    /// Rects for the active tab's panes (respecting zoom).
    pub fn active_pane_rects(&self) -> Vec<(usize, Rect)> {
        let content = self.content_rect();
        let tab = self.current_tab();
        let mut rects = tab.pane_rects(content, self.show_frames);
        if tab.active_zoomed() {
            let active = tab.active;
            rects.retain(|(id, _)| *id == active);
            if let Some((_, r)) = rects.iter_mut().find(|(id, _)| *id == active) {
                *r = content;
            }
        }
        rects
    }

    /// Pane rect for a pane id (content-area rect, unzoomed logic).
    pub fn pane_rect_of(&self, id: usize) -> Option<Rect> {
        self.active_pane_rects()
            .into_iter()
            .find(|(p, _)| *p == id)
            .map(|(_, r)| r)
    }

    // ---- input -----------------------------------------------------------

    pub fn handle_key(&mut self, kp: KeyPress) {
        let k = kp.key;
        let m = kp.mods;

        if keymap::is_quit(k, m) {
            self.quit = true;
            return;
        }

        // Palette input takes precedence.
        if let Some(pal) = self.palette.as_mut() {
            match k {
                Key::Escape => self.palette = None,
                Key::Enter => {
                    if let Some(item) = pal.selected() {
                        let action = item.action.clone();
                        self.palette = None;
                        self.mode = Mode::Normal;
                        self.execute(action);
                    }
                }
                Key::ArrowUp => pal.up(),
                Key::ArrowDown => pal.down(),
                Key::Backspace => pal.backspace(),
                _ => {
                    if let Some(c) = kp.text {
                        if c.is_control() {
                            return;
                        }
                        pal.type_char(c);
                    }
                }
            }
            return;
        }

        // Mode entry binds.
        if let Some(new_mode) = keymap::entry_bind(k, m) {
            self.mode = if self.mode == new_mode { Mode::Normal } else { new_mode };
            return;
        }

        match keymap::handle(self.mode, &kp) {
            Dispatch::Action(action) => {
                // Modal zellij modes return to Normal after most actions.
                let was_modal = matches!(
                    self.mode,
                    Mode::Pane | Mode::Tab | Mode::Resize | Mode::Move | Mode::Session
                );
                self.execute(action);
                if was_modal
                    && matches!(
                        self.mode,
                        Mode::Pane | Mode::Tab | Mode::Resize | Mode::Move | Mode::Session
                    )
                {
                    self.mode = Mode::Normal;
                }
            }
            Dispatch::Pass => {
                if let Some(t) = self.active_term_mut() {
                    let consumed = if kp.text.is_some() && m.contains(Mods::SHIFT) {
                        Mods::SHIFT
                    } else {
                        Mods::empty()
                    };
                    let text = kp.text.map(|c| c.to_string());
                    let _ = t.handle_key(
                        k,
                        m,
                        consumed,
                        keymap::unshifted_codepoint(k),
                        text.as_deref(),
                        key::Action::Press,
                    );
                }
            }
            Dispatch::Char(c) => match self.mode {
                Mode::EnterSearch => {
                    if let Some(t) = self.active_term_mut() {
                        t.search.set_query(format!("{}{c}", t.search.query));
                    }
                }
                Mode::RenameTab | Mode::RenamePane => {
                    self.rename_buf.push(c);
                }
                _ => {}
            },
            Dispatch::Ignore => {}
        }
    }

    /// Handle text chars that didn't come with a key event (IME-style).
    pub fn handle_char(&mut self, c: char) {
        if self.palette.is_some() {
            return;
        }
        match self.mode {
            Mode::EnterSearch => {
                if let Some(t) = self.active_term_mut() {
                    t.search.set_query(format!("{}{c}", t.search.query));
                }
            }
            Mode::RenameTab | Mode::RenamePane => self.rename_buf.push(c),
            Mode::Normal | Mode::Locked => {
                if let Some(t) = self.active_term_mut() {
                    t.handle_char(c);
                }
            }
            _ => {}
        }
    }

    pub fn handle_key_release(&mut self, kp: KeyPress) {
        if self.palette.is_some() {
            return;
        }
        match self.mode {
            Mode::Normal | Mode::Locked => {
                // Only forward releases for keys the app did NOT consume on
                // press (bound/entry/quit keys would spam the terminal with
                // stray release events).
                let k = kp.key;
                let m = kp.mods;
                if keymap::is_quit(k, m)
                    || keymap::entry_bind(k, m).is_some()
                    || keymap::global_binds(k, m).is_some()
                {
                    return;
                }
                if let Some(t) = self.active_term_mut() {
                    let _ = t.handle_key(
                        kp.key,
                        kp.mods,
                        Mods::empty(),
                        keymap::unshifted_codepoint(kp.key),
                        None,
                        key::Action::Release,
                    );
                }
            }
            _ => {}
        }
    }

    /// Synthetic key repeat for held keys (terminals expect repeats).
    pub fn handle_repeat(&mut self, code: macroquad::prelude::KeyCode, kp: KeyPress) {
        let now = Instant::now();
        let entry = self.repeats.entry(code).or_insert((now, now));
        if now.duration_since(entry.0).as_millis() < 400 {
            return;
        }
        if now.duration_since(entry.1).as_millis() < 40 {
            return;
        }
        entry.1 = now;
        if self.palette.is_none() {
            if let Some(t) = self.active_term_mut() {
                let text = kp.text.map(|c| c.to_string());
                let consumed = if kp.text.is_some() && kp.mods.contains(Mods::SHIFT) {
                    Mods::SHIFT
                } else {
                    Mods::empty()
                };
                let _ = t.handle_key(
                    kp.key,
                    kp.mods,
                    consumed,
                    keymap::unshifted_codepoint(kp.key),
                    text.as_deref(),
                    key::Action::Repeat,
                );
            }
        }
    }

    pub fn clear_repeat(&mut self, code: macroquad::prelude::KeyCode) {
        self.repeats.remove(&code);
    }

    // ---- mouse -----------------------------------------------------------

    pub fn handle_mouse(&mut self) {
        use macroquad::prelude::*;

        let (mx, my) = mouse_position();
        let cell = self.cell;
        let padding = self.config.padding;

        // Tab bar clicks.
        if my < TAB_BAR_H {
            if is_mouse_button_pressed(MouseButton::Left) {
                let tab_w = 120.0;
                let idx = (mx / tab_w) as usize;
                if idx < self.tabs.len() {
                    self.goto_tab(idx);
                }
            }
            return;
        }

        // Palette click closes.
        if self.palette.is_some() && is_mouse_button_pressed(MouseButton::Left) {
            self.palette = None;
            return;
        }

        let content = self.content_rect();
        if my > content.y + content.h {
            return; // status bar
        }

        let rects = self.active_pane_rects();
        let hit = rects
            .iter()
            .find(|(_, r)| r.contains(mx, my))
            .map(|(id, r)| (*id, *r));
        let Some((pane_id, pane_rect)) = hit else {
            return;
        };
        // Click focuses the pane (hover alone must not steal focus).
        if is_mouse_button_pressed(MouseButton::Left) || is_mouse_button_pressed(MouseButton::Right) {
            if self.current_tab().active != pane_id {
                self.current_tab_mut().active = pane_id;
            }
        }
        self.mouse_pane = Some((self.active_tab, pane_id));

        let mods = keymap::current_mods();
        let any_pressed = is_mouse_button_down(MouseButton::Left)
            || is_mouse_button_down(MouseButton::Right)
            || is_mouse_button_down(MouseButton::Middle);
        let button_down = if is_mouse_button_down(MouseButton::Left) {
            Some(libghostty_vt::mouse::Button::Left)
        } else if is_mouse_button_down(MouseButton::Right) {
            Some(libghostty_vt::mouse::Button::Right)
        } else if is_mouse_button_down(MouseButton::Middle) {
            Some(libghostty_vt::mouse::Button::Middle)
        } else {
            None
        };

        // Local coords.
        let (lx, ly) = (mx - pane_rect.x, my - pane_rect.y);

        let tab = self.current_tab_mut();
        let Some(Pane::Term(t)) = tab.panes.get_mut(pane_id) else {
            // Preview/AI panes: wheel scrolls.
            if let Some(Pane::Preview(p)) = tab.panes.get_mut(pane_id) {
                let (_, wy) = mouse_wheel();
                if wy != 0.0 {
                    p.scroll_lines(if wy > 0.0 { -3 } else { 3 });
                }
            } else if let Some(Pane::Ai(p)) = tab.panes.get_mut(pane_id) {
                let (_, wy) = mouse_wheel();
                if wy != 0.0 {
                    p.scroll_lines(if wy > 0.0 { -3 } else { 3 });
                }
            }
            return;
        };

        if is_mouse_button_pressed(MouseButton::Left) {
            let _ = t.handle_mouse(
                pane_rect,
                cell,
                padding,
                MouseEv {
                    kind: MouseEvKind::Press(libghostty_vt::mouse::Button::Left),
                    x: lx,
                    y: ly,
                    any_pressed,
                    button_down,
                },
                mods,
            );
        }
        if is_mouse_button_released(MouseButton::Left) {
            let _ = t.handle_mouse(
                pane_rect,
                cell,
                padding,
                MouseEv {
                    kind: MouseEvKind::Release(libghostty_vt::mouse::Button::Left),
                    x: lx,
                    y: ly,
                    any_pressed,
                    button_down,
                },
                mods,
            );
        }
        if is_mouse_button_pressed(MouseButton::Right) {
            let _ = t.handle_mouse(
                pane_rect,
                cell,
                padding,
                MouseEv {
                    kind: MouseEvKind::Press(libghostty_vt::mouse::Button::Right),
                    x: lx,
                    y: ly,
                    any_pressed,
                    button_down,
                },
                mods,
            );
        }
        if is_mouse_button_released(MouseButton::Right) {
            let _ = t.handle_mouse(
                pane_rect,
                cell,
                padding,
                MouseEv {
                    kind: MouseEvKind::Release(libghostty_vt::mouse::Button::Right),
                    x: lx,
                    y: ly,
                    any_pressed,
                    button_down,
                },
                mods,
            );
        }

        // Motion (drag / hover). The pane dedups identical positions.
        let _ = (mouse_position(), mouse_position());
        let _ = t.handle_mouse(
            pane_rect,
            cell,
            padding,
            MouseEv {
                kind: MouseEvKind::Motion,
                x: lx,
                y: ly,
                any_pressed,
                button_down,
            },
            mods,
        );

        // Wheel.
        let (_, wy) = mouse_wheel();
        if wy != 0.0 {
            let _ = t.handle_mouse(
                pane_rect,
                cell,
                padding,
                MouseEv {
                    kind: MouseEvKind::Wheel(wy),
                    x: lx,
                    y: ly,
                    any_pressed,
                    button_down,
                },
                mods,
            );
        }
    }

    // ---- per-frame updates ----------------------------------------------

    pub fn update(&mut self) {
        // Drain pty output for every pane in every tab.
        for tab in self.tabs.iter_mut() {
            let mut i = 0;
            while i < tab.panes.len() {
                match &mut tab.panes[i] {
                    Pane::Term(t) => t.poll_pty(),
                    Pane::Ai(a) => a.poll(),
                    Pane::Preview(_) => {}
                    Pane::Closed => {}
                }
                i += 1;
            }
            // Flush response buffers.
            for pane in tab.panes.iter_mut() {
                if let Pane::Term(t) = pane {
                    t.flush_response();
                }
            }
        }

        // Tab label follows the active pane's OSC title (unless renamed).
        let tab = self.current_tab_mut();
        let active = tab.active;
        if let Some(Pane::Term(t)) = tab.panes.get_mut(active) {
            let title = t.title();
            if !title.is_empty() && (tab.name.is_empty() || tab.name.starts_with("Tab ")) {
                tab.name = title;
            }
        }
    }
}

/// Collect all pane ids referenced by the tree.
fn collect_tree_ids(root: &Node, out: &mut HashSet<usize>) {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        match node {
            Node::Split { a, b, .. } => {
                stack.push(a);
                stack.push(b);
            }
            Node::Leaf { pane } => {
                out.insert(*pane);
            }
            Node::Stack { panes, .. } => {
                for p in panes {
                    out.insert(*p);
                }
            }
        }
    }
}

/// Remap pane ids in the tree via `remap` (old -> new).
fn remap_tree(root: &mut Node, remap: &std::collections::HashMap<usize, usize>) {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        match node {
            Node::Split { a, b, .. } => {
                stack.push(a);
                stack.push(b);
            }
            Node::Leaf { pane } => {
                if let Some(n) = remap.get(pane) {
                    *pane = *n;
                }
            }
            Node::Stack { panes, .. } => {
                for p in panes.iter_mut() {
                    if let Some(n) = remap.get(p) {
                        *p = *n;
                    }
                }
            }
        }
    }
}

// Node helper used by cycle_stack
impl Node {
    pub fn pane_id_of_path(&self, path: &[u8]) -> usize {
        self.at(path).pane_id()
    }
}
