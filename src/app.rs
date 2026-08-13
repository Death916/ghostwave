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

/// Tab context menu geometry (shared by app hit-testing and main.rs render).
pub const TAB_MENU_W: f32 = 230.0;
pub const TAB_MENU_ITEM_H: f32 = 24.0;
pub const TAB_MENU_PAD: f32 = 8.0;
pub const TAB_MENU_ITEMS: usize = 2;

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

    fn from_panes(panes: Vec<Pane>, name: String) -> Self {
        Self {
            name,
            root: Node::Leaf { pane: 0 },
            panes,
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
    pub toast: Option<(String, Instant)>,
    pub last_tab: Option<usize>,
    pub rename_buf: String,
    pub search_mode_active: bool,
    /// Right-click tab context menu: (tab index, menu origin x, y).
    pub tab_menu: Option<(usize, f32, f32)>,
    /// Name of the session this app instance runs inside.
    pub current_session: String,
    /// Session manager overlay: (session names, selected index).
    pub session_list: Option<(Vec<String>, usize)>,
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
    /// Last time the durable session was persisted (continuous save).
    last_session_save: std::time::Instant,
    /// True when the last update() drained pty data (event-driven pacing:
    /// the main loop skips its frame sleep while there's backlog).
    pub pty_activity: bool,
    /// ghostd upload results: (host, session, ok). Polled each frame.
    remote_uploads: std::sync::mpsc::Receiver<(String, String, bool)>,
    remote_upload_tx: std::sync::mpsc::Sender<(String, String, bool)>,
    /// Hosts whose ghostd binary is known-good this session (skip re-upload).
    binary_ready: std::collections::HashSet<String>,
}

impl App {
    pub fn new(config: Config, font: macroquad::text::Font, font_stack: FontStack) -> crate::Result<Self> {
        let theme_name = config.theme.clone();
        let theme = theme::by_name(&theme_name);

        // Cell metrics. macroquad's draw_text_ex/measure_text treat font_size
        // in LOGICAL pixels (miniquad's internal dpi_scale is 1.0 on this
        // Wayland setup even though screen_dpi_scale() reports the wl buffer
        // scale, e.g. 2.0). font_size is configured in POINTS like ghostty/
        // waveterm (12pt = 16px at 96dpi); convert to logical px here.
        let dpi = 1.0; // logical-px drawing; do NOT scale by wl buffer scale
        let font_px = (config.font_size * 96.0 / 72.0).round().max(6.0) as u16;
        let measured = macroquad::text::measure_text(
            "M",
            Some(&font),
            font_px,
            1.0,
        );
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

        // Restore the last-used named session if it has durable tabs;
        // otherwise create the initial shell pane (no tabs exist yet, so
        // new_term_pane would index an empty vec).
        let current_session = crate::session::last_session();
        crate::session::set_last_session(&current_session);
        let cols: u16 = 100;
        let rows: u16 = 30;
        let tabs = match crate::session::load(&current_session) {
            Some(sess) => Self::tabs_from_session(&sess, cols, rows, cell, &config)?,
            None => Vec::new(),
        };
        let mut tabs = tabs;
        if tabs.is_empty() {
            let initial = TermPane::new(
                cols,
                rows,
                cell,
                config.shell.as_deref(),
                &[],
                None,
                config.scrollback,
                config.max_log_mb * 1024 * 1024,
                true,
                None,
            )?;
            tabs.push(Tab::new(Pane::Term(initial), "Tab 1".into()));
        }

        let (remote_upload_tx, remote_uploads) = std::sync::mpsc::channel();

        let mut app = Self {
            tabs,
            current_session,
            active_tab: 0,
            mode: Mode::Normal,
            theme,
            theme_name,
            show_frames: true,
            palette: None,
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
            tab_menu: None,
            session_list: None,
            last_session_save: std::time::Instant::now(),
            pty_activity: false,
            remote_uploads,
            remote_upload_tx,
            binary_ready: Default::default(),
        };

        // Apply the configured theme's fg/bg to the initial terminal.
        app.apply_terminal_colors();

        Ok(app)
    }

    // ---- pane creation ---------------------------------------------------

    /// Create a pane WITHOUT inserting it into a tab (for new-tab flows).
    fn make_term_pane(
        &self,
        cols: u16,
        rows: u16,
        program: Option<&str>,
        args: &[&str],
        cwd: Option<&str>,
        remote: Option<crate::term::RemoteSpec>,
    ) -> crate::Result<TermPane> {
        let shell = program.or(self.config.shell.as_deref());
        let pane = TermPane::new(
            cols,
            rows,
            self.cell,
            shell,
            args,
            cwd,
            self.config.scrollback,
            self.config.max_log_mb * 1024 * 1024,
            true,
            remote,
        )?;
        Ok(pane)
    }

    /// Push a new tab containing `pane` and switch to it.
    fn push_new_tab(&mut self, pane: Pane, name: &str) {
        self.tabs.push(Tab::new(pane, name.to_string()));
        self.active_tab = self.tabs.len() - 1;
        self.last_tab = None;
        self.apply_terminal_colors();
        self.layout_all();
    }

    fn new_term_pane(
        &mut self,
        cols: u16,
        rows: u16,
        program: Option<&str>,
        args: &[&str],
        cwd: Option<&str>,
        remote: Option<crate::term::RemoteSpec>,
    ) -> crate::Result<usize> {
        let pane = self.make_term_pane(cols, rows, program, args, cwd, remote)?;
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
                self.repaint_all(); // frame lines are drawn over pane edges
            }
            Action::RenamePane => {
                self.mode = Mode::RenamePane;
                self.rename_buf.clear();
                self.repaint_all(); // input box is drawn over content
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
                self.repaint_all(); // input box is drawn over content
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
                self.repaint_all(); // input box + match highlights over content
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
                self.repaint_all(); // palette is drawn over content
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
            Action::ConnectDurableSsh(host) => self.connect_durable_ssh(&host),
            Action::NewAi => self.new_ai_pane(),
            Action::SessionManager => self.open_session_manager(),
            Action::SessionNew => {
                let name = self.next_session_name();
                self.new_session(&name);
            }
            Action::SessionRename => {
                self.mode = Mode::RenameSession;
                self.rename_buf = self.current_session.clone();
                self.repaint_all(); // input box is drawn over content
            }
            Action::SessionDelete => {
                if self.current_session == "default" {
                    self.toast("cannot delete the default session");
                } else {
                    let name = self.current_session.clone();
                    crate::session::delete(&name);
                    self.current_session = "default".into();
                    self.load_session("default");
                    self.toast(&format!("deleted session: {name}"));
                }
            }
            Action::Detach => {
                self.save_session();
                self.toast(&format!("session {} saved — quitting", self.current_session));
                self.quit = true;
            }
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
            Action::ExitMode => {
                self.mode = Mode::Normal;
                self.repaint_all(); // search/rename overlays disappear
            }
            Action::ConfirmSearch => {
                // Enter in the search box: switch to Search navigation mode.
                self.mode = Mode::Search;
                if let Some(t) = self.active_term_mut() {
                    t.search.dirty = true;
                }
            }
            Action::ConfirmRename => {
                let name = std::mem::take(&mut self.rename_buf);
                match self.mode {
                    Mode::RenameTab => {
                        self.mode = Mode::Normal;
                        if !name.is_empty() {
                            self.current_tab_mut().name = name;
                        }
                    }
                    Mode::RenamePane => {
                        self.mode = Mode::Normal;
                        if !name.is_empty() {
                            if let Some(t) = self.active_term_mut() {
                                t.name = name;
                            }
                        }
                    }
                    Mode::RenameSession => {
                        self.mode = Mode::Normal;
                        if !name.is_empty() && name != self.current_session {
                            let old = self.current_session.clone();
                            crate::session::rename(&old, &name);
                            self.current_session = name.clone();
                            crate::session::set_last_session(&name);
                            self.toast(&format!("session renamed: {name}"));
                        }
                    }
                    _ => self.mode = Mode::Normal,
                }
            }
        }
    }

    pub fn toast(&mut self, msg: &str) {
        self.toast = Some((msg.to_string(), Instant::now()));
        // The toast is drawn over pane content: the covered cells need a
        // full repaint when it appears (and on expiry, handled in update).
        self.repaint_all();
    }

    /// Force a full repaint of every terminal pane (anything drawn over
    /// pane content — overlays, toasts, focus moves, theme changes — must
    /// repaint the covered cells; damage-based rendering skips clean panes).
    pub fn repaint_all(&mut self) {
        for tab in &mut self.tabs {
            for pane in &mut tab.panes {
                if let Pane::Term(t) = pane {
                    t.force_repaint = true;
                }
            }
        }
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
            self.config.max_log_mb * 1024 * 1024,
            true,
            None,
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
                self.config.max_log_mb * 1024 * 1024,
                true,
                None,
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
        let id = match self.new_term_pane(cols, rows, None, &[], None, None) {
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
                self.config.max_log_mb * 1024 * 1024,
                true,
                None,
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
        match self.make_term_pane(cols, rows, Some("ssh"), &[host], None, None) {
            Ok(pane) => {
                self.push_new_tab(Pane::Term(pane), &format!("ssh {host}"));
                self.toast(&format!("connecting to {host}"));
            }
            Err(e) => self.toast(&format!("ssh failed: {e}")),
        }
    }

    // ---- durable remote sessions (ghostd) --------------------------------

    /// Remote session name for a host: `ghostwave-<host>` (sanitized).
    pub fn remote_session_name(host: &str) -> String {
        let h: String = host
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '.' || *c == '-' || *c == '_')
            .collect();
        format!("ghostwave-{h}")
    }

    /// The remote shell command the pane runs: check the cached ghostd
    /// binary, then attach (creating the session if missing).
    fn remote_attach_cmd(host: &str, session: &str) -> String {
        format!(
            "BIN=\"$HOME/.cache/ghostwave/ghostd-$(uname -m)\"; \
             if [ ! -x \"$BIN\" ] || [ \"$($BIN --version 2>/dev/null)\" != \"ghostd {ver}\" ]; then \
             echo \"ghostd missing/stale on {host} — reconnect to upload\"; exit 1; fi; \
             exec \"$BIN\" attach {session} 2>/dev/null || {{ \"$BIN\" new {session} >/dev/null 2>&1 && exec \"$BIN\" attach {session}; }}",
            ver = env!("CARGO_PKG_VERSION")
        )
    }

    /// Open a durable remote pane: `ssh -t host` running the ghostd attach
    /// command. The ghostd binary is uploaded in the background on first
    /// use; a stale/missing binary shows an error and the reconnect loop
    /// retries once the upload lands.
    pub fn connect_durable_ssh(&mut self, host: &str) {
        let session = Self::remote_session_name(host);
        let cmd = Self::remote_attach_cmd(host, &session);
        self.ensure_remote_binary(host, &session);
        let cols = 100;
        let rows = 30;
        let args = ["-t", "-o", "ConnectTimeout=10", host, &cmd];
        match self.make_term_pane(
            cols,
            rows,
            Some("ssh"),
            &args,
            None,
            Some(crate::term::RemoteSpec {
                host: host.to_string(),
                session,
            }),
        ) {
            Ok(pane) => {
                self.push_new_tab(Pane::Term(pane), &format!("ssh {host}"));
                self.toast(&format!("durable ssh: {host}"));
            }
            Err(e) => self.toast(&format!("ssh failed: {e}")),
        }
    }

    /// Background thread: check/upload the ghostd binary for `host`; the
    /// result arrives on `remote_uploads` and respawns waiting panes.
    fn ensure_remote_binary(&mut self, host: &str, session: &str) {
        if self.binary_ready.contains(host) {
            return;
        }
        let tx = self.remote_upload_tx.clone();
        let host = host.to_string();
        let session = session.to_string();
        std::thread::spawn(move || {
            let ok = upload_ghostd(&host);
            let _ = tx.send((host, session, ok));
        });
    }

    /// Enter on a disconnected remote pane: connect now (upload first if
    /// needed, then respawn — or wait for the upload signal).
    fn connect_remote_pane(&mut self, tab_idx: usize, pane_idx: usize) {
        let Some((host, session)) = self.remote_spec_of(tab_idx, pane_idx) else {
            return;
        };
        let ready = self.binary_ready.contains(&host);
        if !ready {
            self.ensure_remote_binary(&host, &session);
        }
        if let Pane::Term(t) = &mut self.tabs[tab_idx].panes[pane_idx] {
            t.reconnect_failures = 0;
            if ready {
                if !t.respawn() {
                    t.reconnect = crate::term::ReconnectState::Manual;
                    self.toast("reconnect failed");
                }
            } else {
                t.reconnect = crate::term::ReconnectState::Waiting {
                    attempt: 1,
                    next: std::time::Instant::now(),
                };
            }
            self.repaint_all();
        }
    }

    /// k on a disconnected remote pane: kill the remote session (ghostd
    /// kill on the host). The daemon cleans up its socket/pid/log.
    fn kill_remote_pane(&mut self, tab_idx: usize, pane_idx: usize) {
        let Some((host, session)) = self.remote_spec_of(tab_idx, pane_idx) else {
            return;
        };
        self.toast(&format!("killing remote session {session} on {host}"));
        std::thread::spawn(move || {
            let cmd = format!(
                "BIN=\"$HOME/.cache/ghostwave/ghostd-$(uname -m)\"; \"$BIN\" kill {session} 2>&1 || echo \"no ghostd binary on {host}\""
            );
            let _ = std::process::Command::new("ssh")
                .args(["-o", "ConnectTimeout=5", &host, &cmd])
                .status();
        });
    }

    fn remote_spec_of(&self, tab_idx: usize, pane_idx: usize) -> Option<(String, String)> {
        match self.tabs.get(tab_idx)?.panes.get(pane_idx)? {
            Pane::Term(t) => t
                .remote
                .as_ref()
                .map(|r| (r.host.clone(), r.session.clone())),
            _ => None,
        }
    }

    /// Respawn every pane waiting to reconnect to `host` (binary just
    /// became ready).
    fn respawn_waiting(&mut self, host: &str) {
        for tab in self.tabs.iter_mut() {
            for pane in tab.panes.iter_mut() {
                if let Pane::Term(t) = pane {
                    let is_host = t.remote.as_ref().is_some_and(|r| r.host == host);
                    if is_host && matches!(t.reconnect, crate::term::ReconnectState::Waiting { .. }) {
                        let _ = t.respawn();
                    }
                }
            }
        }
    }

    pub fn new_ai_pane(&mut self) {
        let pane = Pane::Ai(AiPane::new(self.config.ai.clone()));
        self.push_side_pane(pane);
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

    /// Persist the durable session state (JSON + pane logs). Called on quit.
    pub fn save_session(&self) {
        let state = crate::session::SessionState {
            tabs: self
                .tabs
                .iter()
                .map(|tab| {
                    // Terminal panes only (preview/AI panes are not restored);
                    // remap the layout tree to the kept pane ids.
                    let mut kept: Vec<usize> = Vec::new();
                    let panes: Vec<crate::session::PaneState> = tab
                        .panes
                        .iter()
                        .enumerate()
                        .filter_map(|(id, p)| match p {
                            Pane::Term(t) => {
                                kept.push(id);
                                Some(crate::session::PaneState {
                                    program: t.program.clone(),
                                    args: t.args.clone(),
                                    cwd: t.cwd.clone(),
                                    remote: t.remote.clone(),
                                    log: t.log_name.clone(),
                                })
                            }
                            _ => None, // preview/AI panes are not restored
                        })
                        .collect();
                    let map: std::collections::HashMap<usize, usize> = kept
                        .iter()
                        .enumerate()
                        .map(|(new_id, old_id)| (*old_id, new_id))
                        .collect();
                    let root = Self::remap_layout(&tab.root, &map);
                    crate::session::TabState {
                        name: tab.name.clone(),
                        panes,
                        root: Some(root),
                    }
                })
                .collect(),
        };
        crate::session::save(&self.current_session, &state);
        crate::session::set_last_session(&self.current_session);
    }

    /// Set every remote pane on `host` to Manual (upload failed).
    fn set_remote_manual(&mut self, host: &str) {
        for tab in self.tabs.iter_mut() {
            for pane in tab.panes.iter_mut() {
                if let Pane::Term(t) = pane {
                    if t.remote.as_ref().is_some_and(|r| r.host == host) {
                        t.reconnect = crate::term::ReconnectState::Manual;
                    }
                }
            }
        }
    }

    // ---- tab context menu -------------------------------------------------

    // ---- sessions ---------------------------------------------------------

    /// Remap pane ids in a layout tree through `map` (old id → new id).
    /// Dropped ids (preview/AI panes) fall back to pane 0.
    fn remap_layout(node: &crate::layout::Node, map: &std::collections::HashMap<usize, usize>) -> crate::layout::Node {
        use crate::layout::Node;
        match node {
            Node::Split { dir, ratio, a, b } => Node::Split {
                dir: *dir,
                ratio: *ratio,
                a: Box::new(Self::remap_layout(a, map)),
                b: Box::new(Self::remap_layout(b, map)),
            },
            Node::Leaf { pane } => Node::Leaf {
                pane: *map.get(pane).unwrap_or(&0),
            },
            Node::Stack { panes, active } => Node::Stack {
                panes: panes.iter().map(|p| *map.get(p).unwrap_or(&0)).collect(),
                active: *map.get(active).unwrap_or(&0),
            },
        }
    }

    /// Build tabs (with replayed scrollback) from a saved session state.
    fn tabs_from_session(
        sess: &crate::session::SessionState,
        cols: u16,
        rows: u16,
        cell: CellDims,
        config: &Config,
    ) -> crate::Result<Vec<Tab>> {
        let mut tabs: Vec<Tab> = Vec::new();
        for ts in sess.tabs.iter().filter(|t| !t.panes.is_empty()) {
            let mut panes: Vec<Pane> = Vec::new();
            for ps in &ts.panes {
                let args: Vec<&str> = ps.args.iter().map(String::as_str).collect();
                // Remote panes restore DISCONNECTED: history shows, but the
                // ssh only connects when the user presses Enter (or an
                // in-session reconnect loop fires — never on restore).
                let spawn = ps.remote.is_none();
                let Ok(pane) = TermPane::new(
                    cols,
                    rows,
                    cell,
                    ps.program.as_deref(),
                    &args,
                    ps.cwd.as_deref(),
                    config.scrollback,
                    config.max_log_mb * 1024 * 1024,
                    spawn,
                    ps.remote.clone(),
                ) else {
                    continue;
                };
                let mut pane = pane;
                let seeded = crate::session::replay_pane_log(
                    &ps.log,
                    &mut pane.term,
                    config.max_log_mb * 1024 * 1024,
                );
                pane.log_seed(&seeded);
                if !spawn {
                    pane.exited = Some("disconnected (remote session)".into());
                    pane.last_exit_code = Some(1);
                    pane.reconnect = crate::term::ReconnectState::Manual;
                    pane.force_repaint = true;
                }
                panes.push(Pane::Term(pane));
            }
            if !panes.is_empty() {
                let mut tab = Tab::from_panes(panes, ts.name.clone());
                // Restore the saved split/stack layout (pane ids already
                // remapped at save time). Old files lack it → single pane.
                if let Some(root) = &ts.root {
                    tab.root = root.clone();
                }
                tabs.push(tab);
            }
        }
        Ok(tabs)
    }

    /// Switch the whole window to a saved session (replacing tabs).
    pub fn load_session(&mut self, name: &str) {
        if name == self.current_session {
            self.toast(&format!("already in session: {name}"));
            return;
        }
        self.save_session(); // persist the outgoing session
        let cols: u16 = 100;
        let rows: u16 = 30;
        let tabs = match crate::session::load(name) {
            Some(sess) => Self::tabs_from_session(&sess, cols, rows, self.cell, &self.config)
                .unwrap_or_default(),
            None => Vec::new(),
        };
        let mut tabs = tabs;
        if tabs.is_empty() {
            match TermPane::new(
                cols,
                rows,
                self.cell,
                self.config.shell.as_deref(),
                &[],
                None,
                self.config.scrollback,
                self.config.max_log_mb * 1024 * 1024,
                true,
                None,
            ) {
                Ok(p) => tabs.push(Tab::new(Pane::Term(p), "Tab 1".into())),
                Err(e) => {
                    self.toast(&format!("failed to open session {name}: {e}"));
                    return;
                }
            }
        }
        self.tabs = tabs;
        self.active_tab = 0;
        self.current_session = name.to_string();
        crate::session::set_last_session(name);
        self.apply_terminal_colors();
        self.layout_all();
        self.toast(&format!("session: {name}"));
    }

    /// Start a fresh (empty) session under `name`.
    pub fn new_session(&mut self, name: &str) {
        self.save_session();
        let cols: u16 = 100;
        let rows: u16 = 30;
        match TermPane::new(
            cols,
            rows,
            self.cell,
            self.config.shell.as_deref(),
            &[],
            None,
            self.config.scrollback,
            self.config.max_log_mb * 1024 * 1024,
            true,
            None,
        ) {
            Ok(p) => {
                self.tabs = vec![Tab::new(Pane::Term(p), "Tab 1".into())];
                self.active_tab = 0;
                self.current_session = name.to_string();
                crate::session::set_last_session(name);
                self.apply_terminal_colors();
                self.layout_all();
                self.toast(&format!("new session: {name}"));
            }
            Err(e) => self.toast(&format!("failed to start session: {e}")),
        }
    }

    /// Next free auto-generated session name (session-1, session-2, …).
    fn next_session_name(&self) -> String {
        let existing = crate::session::list_sessions();
        let mut i = 1;
        loop {
            let name = format!("session-{i}");
            if !existing.iter().any(|e| e == &name) {
                return name;
            }
            i += 1;
        }
    }

    /// Open the session manager overlay (list of saved sessions).
    pub fn open_session_manager(&mut self) {
        let mut entries = crate::session::list_sessions();
        if !entries.iter().any(|e| e == &self.current_session) {
            entries.push(self.current_session.clone());
            entries.sort();
        }
        let selected = entries
            .iter()
            .position(|e| e == &self.current_session)
            .unwrap_or(0);
        self.session_list = Some((entries, selected));
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

        // Disconnected durable-remote pane: Enter connects, k kills the
        // remote session, Esc stops auto-reconnect. All other keys are
        // swallowed (there is no process to send them to).
        if self.mode == Mode::Normal {
            let active = self.current_tab().active;
            let is_remote_dead = matches!(
                self.current_tab().panes.get(active),
                Some(Pane::Term(t)) if t.remote.is_some() && t.exited.is_some()
            );
            if is_remote_dead {
                let tab_idx = self.active_tab;
                let pane_idx = active;
                match k {
                    Key::Enter => {
                        self.connect_remote_pane(tab_idx, pane_idx);
                        return;
                    }
                    Key::K => {
                        self.kill_remote_pane(tab_idx, pane_idx);
                        return;
                    }
                    Key::Escape => {
                        if let Pane::Term(t) = &mut self.tabs[tab_idx].panes[pane_idx] {
                            t.reconnect = crate::term::ReconnectState::Manual;
                        }
                        self.toast("reconnect stopped — Enter to connect");
                        return;
                    }
                    _ => return,
                }
            }
        }

        // Tab context menu swallows keys; Esc closes it.
        if self.tab_menu.is_some() {
            if k == Key::Escape {
                self.tab_menu = None;
                self.repaint_all();
            }
            return;
        }

        // Palette input takes precedence.
        if let Some(pal) = self.palette.as_mut() {
            match k {
                Key::Escape => {
                    self.palette = None;
                    self.repaint_all();
                }
                Key::Enter => {
                    if let Some(item) = pal.selected() {
                        let action = item.action.clone();
                        self.palette = None;
                        self.mode = Mode::Normal;
                        self.repaint_all(); // palette closes before executing
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

        // Session manager overlay.
        if self.session_list.is_some() {
            match k {
                Key::Escape => {
                    self.session_list = None;
                    self.repaint_all();
                }
                Key::ArrowUp => {
                    if let Some((_, sel)) = self.session_list.as_mut() {
                        *sel = sel.saturating_sub(1);
                    }
                }
                Key::ArrowDown => {
                    if let Some((e, sel)) = self.session_list.as_mut() {
                        *sel = (*sel + 1).min(e.len().saturating_sub(1));
                    }
                }
                Key::Enter => {
                    let name = self
                        .session_list
                        .take()
                        .and_then(|(e, sel)| e.get(sel).cloned());
                    self.mode = Mode::Normal;
                    if let Some(name) = name {
                        self.load_session(&name);
                    }
                }
                Key::R => {
                    let name = self
                        .session_list
                        .take()
                        .and_then(|(e, sel)| e.get(sel).cloned());
                    self.mode = Mode::RenameSession;
                    self.rename_buf = name.unwrap_or_default();
                }
                Key::D => {
                    let name = self
                        .session_list
                        .as_ref()
                        .and_then(|(e, sel)| e.get(*sel).cloned());
                    if let Some(name) = name {
                        if name != self.current_session {
                            crate::session::delete(&name);
                            let entries = crate::session::list_sessions();
                            if let Some((e, sel)) = self.session_list.as_mut() {
                                *e = entries;
                                if *sel >= e.len() {
                                    *sel = e.len().saturating_sub(1);
                                }
                            }
                            self.toast(&format!("deleted session: {name}"));
                        } else {
                            self.toast("cannot delete the current session");
                        }
                    }
                }
                Key::N => {
                    let name = self.next_session_name();
                    self.session_list = None;
                    self.mode = Mode::Normal;
                    self.new_session(&name);
                }
                _ => {}
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
                Mode::RenameTab | Mode::RenamePane | Mode::RenameSession => {
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
            Mode::RenameTab | Mode::RenamePane | Mode::RenameSession => self.rename_buf.push(c),
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

        // Tab context menu clicks (before the tab-bar check: the menu box
        // opens at the cursor and extends below the tab bar).
        if let Some((tab_idx, mx0, my0)) = self.tab_menu {
            if is_mouse_button_pressed(MouseButton::Left) {
                let menu_h = TAB_MENU_PAD + TAB_MENU_ITEMS as f32 * TAB_MENU_ITEM_H + TAB_MENU_PAD;
                if mx >= mx0 && mx <= mx0 + TAB_MENU_W && my >= my0 && my <= my0 + menu_h {
                    let item = ((my - my0 - TAB_MENU_PAD) / TAB_MENU_ITEM_H) as usize;
                    match item {
                        0 => {
                            self.goto_tab(tab_idx);
                            self.mode = Mode::RenameTab;
                        }
                        1 => {
                            self.goto_tab(tab_idx);
                            self.close_tab();
                        }
                        _ => {}
                    }
                }
                self.tab_menu = None;
                self.repaint_all(); // menu box covered pane content
                return;
            }
        }

        // Tab bar clicks.
        if my < TAB_BAR_H {
            if is_mouse_button_pressed(MouseButton::Right) {
                let tab_w = 120.0;
                let idx = (mx / tab_w) as usize;
                if idx < self.tabs.len() {
                    self.tab_menu = Some((idx, mx, my));
                    self.repaint_all(); // menu box covers pane content
                }
            }
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
            self.repaint_all(); // palette box covered pane content
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
                self.repaint_all(); // cursor moved to another pane
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
        self.pty_activity = false;

        // Persist the durable session every 2s so a crash/kill loses at most
        // a couple of seconds (quit also saves explicitly).
        if self.last_session_save.elapsed().as_secs() >= 2 {
            self.save_session();
            self.last_session_save = std::time::Instant::now();
        }

        // Toast expiry: repaint when it disappears (it's drawn over content).
        if let Some((_, t)) = self.toast {
            if t.elapsed().as_secs() >= 3 {
                self.toast = None;
                self.repaint_all();
            }
        }

        // Drain pty output for every pane in every tab.
        for tab in self.tabs.iter_mut() {
            let mut i = 0;
            while i < tab.panes.len() {
                match &mut tab.panes[i] {
                    Pane::Term(t) => {
                        if t.poll_pty() {
                            self.pty_activity = true;
                        }
                    }
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

        // ---- durable remote sessions ----

        // ghostd upload results: mark the host ready and respawn panes that
        // were waiting for the binary.
        while let Ok((host, _session, ok)) = self.remote_uploads.try_recv() {
            if ok {
                self.binary_ready.insert(host.clone());
                self.respawn_waiting(&host);
            } else {
                self.set_remote_manual(&host);
                self.toast(&format!("ghostd setup failed on {host}"));
            }
            self.repaint_all();
        }

        // Reconnect loop: a dead remote pane enters auto-reconnect (bounded),
        // then drops to manual. Panes whose binary isn't uploaded yet wait
        // for the upload signal instead of burning attempts.
        let now = std::time::Instant::now();
        let mut to_respawn: Vec<(usize, usize)> = Vec::new();
        let mut to_manual: Vec<(usize, usize)> = Vec::new();
        for (ti, tab) in self.tabs.iter_mut().enumerate() {
            for (pi, pane) in tab.panes.iter_mut().enumerate() {
                let Pane::Term(t) = pane else { continue };
                if t.remote.is_none() {
                    continue;
                }
                if t.exited.is_some() && t.reconnect == crate::term::ReconnectState::Idle {
                    let code = t.last_exit_code.unwrap_or(255);
                    let auto = self.config.remote.auto_reconnect && code != 0;
                    if auto && t.reconnect_failures < self.config.remote.reconnect_attempts {
                        t.reconnect_failures += 1;
                        let delay = self.config.remote.reconnect_delay_secs.max(1);
                        t.reconnect = crate::term::ReconnectState::Waiting {
                            attempt: t.reconnect_failures,
                            next: now + std::time::Duration::from_secs(delay),
                        };
                    } else {
                        t.reconnect = crate::term::ReconnectState::Manual;
                    }
                }
                if let crate::term::ReconnectState::Waiting { attempt, next } = t.reconnect {
                    if now >= next {
                        let host = t.remote.as_ref().map(|r| r.host.clone());
                        if let Some(h) = host {
                            if self.binary_ready.contains(&h) {
                                to_respawn.push((ti, pi));
                            } else {
                                t.reconnect = crate::term::ReconnectState::Waiting {
                                    attempt,
                                    next: now + std::time::Duration::from_millis(500),
                                };
                            }
                        }
                    }
                }
            }
        }
        for (ti, pi) in to_respawn {
            if let Pane::Term(t) = &mut self.tabs[ti].panes[pi] {
                if !t.respawn() {
                    t.reconnect = crate::term::ReconnectState::Manual;
                }
            }
        }
        for (ti, pi) in to_manual {
            if let Pane::Term(t) = &mut self.tabs[ti].panes[pi] {
                t.reconnect = crate::term::ReconnectState::Manual;
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

// ---- durable remote (ghostd) bootstrap -----------------------------------

/// Embedded ghostd binaries (built as static musl; see assets/). Selected
/// by the REMOTE arch (uname -m) during the upload.
const GHOSTD_X86_64: &[u8] = include_bytes!("../assets/ghostd-x86_64");
const GHOSTD_AARCH64: &[u8] = include_bytes!("../assets/ghostd-aarch64");

/// Run a short ssh command; true on exit 0.
fn ssh_ok(host: &str, cmd: &str) -> bool {
    std::process::Command::new("ssh")
        .args(["-o", "ConnectTimeout=5", host, cmd])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Run a short ssh command; its stdout (None on failure).
fn ssh_out(host: &str, cmd: &str) -> Option<String> {
    let out = std::process::Command::new("ssh")
        .args(["-o", "ConnectTimeout=5", host, cmd])
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Ensure the ghostd binary for `host` is present and current in
/// ~/.cache/ghostwave on the remote. True when it is (or became) ready.
fn upload_ghostd(host: &str) -> bool {
    // 1. Remote arch decides which embedded binary to ship.
    let arch = match ssh_out(host, "uname -m") {
        Some(a) => a.trim().to_string(),
        None => return false,
    };
    let bytes: &[u8] = match arch.as_str() {
        "x86_64" => GHOSTD_X86_64,
        "aarch64" => GHOSTD_AARCH64,
        _ => return false,
    };
    // 2. Already current? (version check on the remote).
    let check = format!(
        "BIN=\"$HOME/.cache/ghostwave/ghostd-{arch}\"; [ -x \"$BIN\" ] && [ \"$($BIN --version 2>/dev/null)\" = \"ghostd {ver}\" ]",
        ver = env!("CARGO_PKG_VERSION")
    );
    if ssh_ok(host, &check) {
        return true;
    }
    // 3. Upload the binary over stdin.
    let mut child = match std::process::Command::new("ssh")
        .args([
            "-o",
            "ConnectTimeout=5",
            host,
            &format!(
                "mkdir -p \"$HOME/.cache/ghostwave\" && cat > \"$HOME/.cache/ghostwave/ghostd-{arch}\" && chmod +x \"$HOME/.cache/ghostwave/ghostd-{arch}\""
            ),
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    {
        use std::io::Write;
        if let Some(mut si) = child.stdin.take() {
            let _ = si.write_all(bytes);
        }
    }
    child.wait().map(|s| s.success()).unwrap_or(false)
}