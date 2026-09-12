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

/// Debug trace to stdout, ON BY DEFAULT (remote-session bring-up phase —
/// the user runs the debug binary from a terminal and expects to see the
/// steps); set GHOSTWAVE_DEBUG=0 to silence.
pub fn gdbg(msg: impl AsRef<str>) {
    if std::env::var("GHOSTWAVE_DEBUG").map(|v| v == "0").unwrap_or(false) {
        return;
    }
    println!("[ghostwave] {}", msg.as_ref());
}

pub enum Pane {
    Term(TermPane),
    Preview(PreviewPane),
    Ai(AiPane),
    Visual(crate::visualizer::VisualizerPane),
    /// Transient placeholder used during reindexing; never rendered.
    Closed,
}

/// A pane floating above the tab's layout tree (zellij Ctrl+p f). It keeps
/// its own rect and can be moved/resized with Move/Resize modes; `f` again
/// embeds it back at `origin`. A HIDDEN float ("bg pane") is not rendered
/// or hit-tested but keeps running and draining (pane-mode `e` hides,
/// pane-mode `E` cycles floats and shows them again).
pub struct FloatingPane {
    /// Index into `Tab.panes`.
    pub pane: usize,
    /// Overlay rect in logical px (content-area coords).
    pub rect: Rect,
    /// Where the pane came from in the tree (for exact re-embed).
    pub origin: Option<crate::layout::FloatOrigin>,
    /// True = background pane: running but invisible and unclickable.
    pub hidden: bool,
}

pub struct Tab {
    pub name: String,
    pub root: Node,
    pub panes: Vec<Pane>,
    pub active: usize,
    pub zoomed: HashSet<usize>,
    /// Floating panes (z-order: later entries draw on top).
    pub floating: Vec<FloatingPane>,
    pub sync_panes: bool,
}

impl Tab {
    fn new(initial: Pane, name: String) -> Self {
        Self {
            name,
            root: Node::Leaf { pane: 0 },
            panes: vec![initial],
            active: 0,
            zoomed: HashSet::new(),
            floating: Vec::new(),
            sync_panes: false,
        }
    }

    fn from_panes(panes: Vec<Pane>, name: String) -> Self {
        Self {
            name,
            root: Node::Leaf { pane: 0 },
            panes,
            active: 0,
            zoomed: HashSet::new(),
            floating: Vec::new(),
            sync_panes: false,
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

/// A session whose panes are still alive in this process but not visible:
/// parked when the user switches sessions, so pty children keep running and
/// their logs keep growing. Drained every frame in App::update().
pub struct LiveSession {
    pub name: String,
    pub tabs: Vec<Tab>,
    pub active_tab: usize,
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
    /// Session being renamed (current or a parked one) — the manager's `r`
    /// can target a non-current session, so ConfirmRename must not assume
    /// the current session.
    pub rename_target: String,
    pub search_mode_active: bool,
    /// Right-click tab context menu: (tab index, menu origin x, y).
    pub tab_menu: Option<(usize, f32, f32)>,
    /// Name of the session this app instance runs inside.
    pub current_session: String,
    /// Sessions whose panes are STILL ALIVE in this process but not visible
    /// (switched away from). Their pty children keep running and are drained
    /// every frame in update(), so logs stay fresh and switching back swaps
    /// the live tabs in — no shell respawn, no log replay.
    pub parked: Vec<LiveSession>,
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
    /// Living reactive sine-wave animation state (status bar visualizer).
    pub wave_energy: f32,
    pub wave_phase: f32,
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
            rename_target: String::new(),
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
            wave_energy: 0.1,
            wave_phase: 0.0,
            parked: Vec::new(),
            remote_uploads,
            remote_upload_tx,
            binary_ready: Default::default(),
        };

        // Apply the configured theme's fg/bg to the initial terminal.
        app.apply_terminal_colors();

        // Warm up glyph atlas caches for smooth first-frame rendering.
        app.font_stack.warmup(font_px);

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

    /// Tree path of the active pane; when the active pane is floating (no
    /// tree path), fall back to the first tree leaf so tree ops (split,
    /// stack, side panes) never operate on the root.
    fn active_tree_path(&self) -> layout::Path {
        let tab = self.current_tab();
        layout::path_to_pane(&tab.root, tab.active)
            .or_else(|| tab.root.leaves().into_iter().next().map(|(p, _)| p))
            .unwrap_or_default()
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
                // Cycle through tree panes; from a floating pane, jump into
                // the tree (floats aren't part of the DFS cycle).
                let tab = self.current_tab();
                if tab.floating.iter().any(|f| f.pane == tab.active) {
                    if let Some((_, p)) = tab.root.leaves().into_iter().next() {
                        self.current_tab_mut().active = p;
                    }
                } else {
                    let next =
                        layout::next_leaf(&self.current_tab().root, self.active_pane_id());
                    if let Some(n) = next {
                        self.current_tab_mut().active = n;
                    }
                }
            }
            Action::ToggleZoom => self.toggle_zoom(),
            Action::ToggleFloating => self.toggle_floating(),
            Action::ToggleHideFloat => self.toggle_hide_float(),
            Action::CycleFloat => self.cycle_float(),
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
                    t.scroll_half_page(-1);
                }
            }
            Action::HalfPageDown => {
                if let Some(t) = self.active_term_mut() {
                    t.scroll_half_page(1);
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
            Action::KillRemoteSession => self.kill_active_remote_pane(),
            Action::NewAi => self.new_ai_pane(),
            Action::SessionManager => self.open_session_manager(),
            Action::SessionNew => {
                let name = self.next_session_name();
                self.new_session(&name);
            }
            Action::SessionRename => {
                self.mode = Mode::RenameSession;
                self.rename_buf = self.current_session.clone();
                self.rename_target = self.current_session.clone();
                self.repaint_all(); // input box is drawn over content
            }
            Action::SessionDelete => {
                if self.current_session == "default" {
                    self.toast("cannot delete the default session");
                } else {
                    let name = self.current_session.clone();
                    crate::session::delete(&name);
                    // Drop the session's panes (kills their processes) rather
                    // than parking them: its file is gone, and the continuous
                    // save would otherwise resurrect it on quit.
                    self.tabs = Vec::new();
                    self.active_tab = 0;
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
            Action::ToggleSyncPanes => {
                let enabled = {
                    let tab = self.current_tab_mut();
                    tab.sync_panes = !tab.sync_panes;
                    tab.sync_panes
                };
                self.toast(if enabled {
                    "synchronized input ON (broadcasting to all panes)"
                } else {
                    "synchronized input OFF"
                });
                self.repaint_all();
            }
            Action::ToggleScratchpad => self.toggle_scratchpad(),
            Action::Ghostwave => self.open_visualizer(),
            Action::Backspace => match self.mode {
                Mode::EnterSearch => {
                    if let Some(t) = self.active_term_mut() {
                        let mut q = t.search.query.clone();
                        q.pop();
                        t.search.set_query(q);
                    }
                    self.repaint_all();
                }
                Mode::RenameTab | Mode::RenamePane | Mode::RenameSession => {
                    self.rename_buf.pop();
                    self.repaint_all();
                }
                _ => {}
            },
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
                        if !name.is_empty() {
                            let old = std::mem::take(&mut self.rename_target);
                            if old == self.current_session {
                                crate::session::rename(&old, &name);
                                self.current_session = name.clone();
                                crate::session::set_last_session(&name);
                                self.toast(&format!("session renamed: {name}"));
                            } else if !old.is_empty() {
                                crate::session::rename(&old, &name);
                                // The renamed session may be parked (alive):
                                // update its in-memory entry too.
                                if let Some(live) = self.parked.iter_mut().find(|s| s.name == old) {
                                    live.name = name.clone();
                                }
                                self.toast(&format!("session renamed: {name}"));
                            }
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
        let cwd = self.active_term().map(|t| t.pwd()).filter(|s| !s.is_empty());
        let pane = match TermPane::new(
            cols,
            rows,
            self.cell,
            self.config.shell.as_deref(),
            &[],
            cwd.as_deref(),
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
        let cwd = self.active_term().map(|t| t.pwd()).filter(|s| !s.is_empty());
        let id = match self.new_term_pane(cols, rows, None, &[], cwd.as_deref(), None) {
            Ok(id) => id,
            Err(e) => {
                eprintln!("ghostwave: failed to spawn shell: {e}");
                return;
            }
        };
        let path = self.active_tree_path();
        let tab = self.current_tab_mut();
        if matches!(tab.root, Node::Leaf { pane: usize::MAX }) {
            // Tree is empty (the only pane is floating): it becomes the root.
            tab.root = Node::Leaf { pane: id };
        } else if stacked {
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
            // Closing a floating pane: drop it from the float list (its pty
            // child gets SIGHUP'd like any closed pane) and focus the tree.
            if let Some(fi) = tab.floating.iter().position(|f| f.pane == tab.active) {
                let f = tab.floating.remove(fi);
                tab.panes[f.pane] = Pane::Closed;
                let tree_empty = matches!(tab.root, Node::Leaf { pane: usize::MAX })
                    || tab.root.leaves().is_empty();
                if tree_empty {
                    tab.root = Node::Leaf { pane: usize::MAX };
                    tab.active = 0;
                    true
                } else {
                    let next = tab.root.leaves().into_iter().next().map(|(_, p)| p);
                    tab.active = next.unwrap_or(0);
                    false
                }
            } else {
                match layout::path_to_pane(&tab.root, tab.active) {
                    Some(path) => {
                        layout::close_at(&mut tab.root, &path);
                        // close_at marks the root as an empty sentinel when
                        // the last leaf was removed.
                        let empty = matches!(tab.root, Node::Leaf { pane: usize::MAX });
                        if !empty {
                            // Remove the closed pane from the vec (by its
                            // old id).
                            if let Some(id) = layout::path_to_pane(&tab.root, usize::MAX) {
                                let _ = id;
                            }
                        }
                        empty
                    }
                    None => false,
                }
            }
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
    /// then remap the tree (AND floating panes) to the compacted ids.
    fn remove_dangling_panes(&mut self) {
        let tab = self.current_tab_mut();
        // Collect referenced ids (tree leaves + floating panes).
        let mut referenced: HashSet<usize> = HashSet::new();
        collect_tree_ids(&tab.root, &mut referenced);
        for f in &tab.floating {
            referenced.insert(f.pane);
        }
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
        for f in tab.floating.iter_mut() {
            if let Some(n) = remap.get(&f.pane) {
                f.pane = *n;
            }
        }
        tab.panes = new_panes;
        tab.active = remap.get(&tab.active).copied().unwrap_or(0);
        tab.zoomed.retain(|id| remap.contains_key(id));
    }

    pub fn focus(&mut self, dir: Dir) {
        let tab = self.current_tab();
        // Directional focus from a floating pane: floats don't participate
        // in the tree's rect-based navigation — jump to the first tree leaf.
        if tab.floating.iter().any(|f| f.pane == tab.active) {
            if let Some((_, p)) = tab.root.leaves().into_iter().next() {
                self.current_tab_mut().active = p;
            }
            return;
        }
        let content = self.content_rect();
        let rects = self.current_tab().pane_rects(content, self.show_frames);
        let next = layout::focus_in_direction(&rects, self.active_pane_id(), dir);
        if let Some(n) = next {
            self.current_tab_mut().active = n;
        }
    }

    pub fn toggle_zoom(&mut self) {
        // Zoom applies to tree panes; a floating pane can't be zoomed
        // (its rect is already arbitrary). Ctrl+M on a float: tell them.
        let tab = self.current_tab();
        if tab.floating.iter().any(|f| f.pane == tab.active) {
            self.toast("floating pane — embed it (pane-mode f) to zoom");
            return;
        }
        let tab = self.current_tab_mut();
        if !tab.zoomed.insert(tab.active) {
            tab.zoomed.remove(&tab.active);
        }
    }

    /// Zellij float toggle: the focused tree pane floats above the layout
    /// (drawn last, moveable/resizable); the focused floating pane embeds
    /// back into the tree at its original spot.
    pub fn toggle_floating(&mut self) {
        let active = self.active_pane_id();
        // &self reads first (no &mut borrow yet).
        let (float_idx, is_term, tree_path) = {
            let tab = self.current_tab();
            (
                tab.floating.iter().position(|f| f.pane == active),
                matches!(tab.panes.get(active), Some(Pane::Term(_))),
                layout::path_to_pane(&tab.root, active),
            )
        };

        // Embed the active floating pane back into the tree.
        if let Some(fi) = float_idx {
            let tab = self.current_tab_mut();
            let f = tab.floating.remove(fi);
            let pane = f.pane;
            let origin = f.origin;
            let embedded = origin
                .as_ref()
                .map(|o| layout::embed_at(&mut tab.root, pane, o))
                .unwrap_or(false);
            if !embedded {
                // Origin stale (tree changed while it floated): embed next
                // to the tree's active pane (or alone in an empty tree).
                if matches!(tab.root, Node::Leaf { pane: usize::MAX }) {
                    tab.root = Node::Leaf { pane };
                } else {
                    let path = layout::path_to_pane(&tab.root, tab.active)
                        .or_else(|| tab.root.leaves().into_iter().next().map(|(p, _)| p))
                        .unwrap_or_default();
                    layout::split_at(&mut tab.root, &path, layout::SplitDir::Horizontal, pane);
                }
            }
            tab.active = pane;
            self.layout_all();
            self.toast("pane embedded");
            return;
        }

        // Float the active tree pane (Term panes only).
        if !is_term {
            self.toast("only terminal panes can float");
            return;
        }
        let Some(path) = tree_path else {
            return;
        };
        // Grab its current rect BEFORE it leaves the tree.
        let rect = self
            .pane_rect_of(active)
            .unwrap_or_else(|| self.content_rect());
        let tab = self.current_tab_mut();
        let Some((_, origin)) = layout::float_out(&mut tab.root, &path) else {
            return;
        };
        tab.zoomed.remove(&active);
        tab.floating.push(FloatingPane {
            pane: active,
            rect,
            origin: Some(origin),
            hidden: false,
        });
        self.layout_all();
        self.toast("pane floating — f to embed, Ctrl+h move, Ctrl+n resize");
    }

    /// Pane-mode `e`: toggle the active floating pane hidden ("bg pane").
    /// Hidden panes keep running (drained every frame) but are not rendered
    /// or clickable; `E` cycles floats and shows them again.
    pub fn toggle_hide_float(&mut self) {
        let (fi, hidden, first_leaf, tree_has_leaf) = {
            let tab = self.current_tab();
            let Some(fi) = tab.floating.iter().position(|f| f.pane == tab.active) else {
                self.toast("no floating pane to hide (float one with pane-mode f)");
                return;
            };
            let hidden = tab.floating[fi].hidden;
            let leaves = tab.root.leaves();
            let tree_has_leaf = !leaves.is_empty();
            let first_leaf = leaves.into_iter().next().map(|(_, p)| p);
            (fi, hidden, first_leaf, tree_has_leaf)
        };
        let tab = self.current_tab_mut();
        tab.floating[fi].hidden = !hidden;
        if !hidden && tree_has_leaf {
            // Hiding steals focus from a now-invisible pane.
            tab.active = first_leaf.unwrap_or(tab.active);
        }
        self.toast(if !hidden {
            "pane hidden (bg) — Ctrl+p E to show"
        } else {
            "pane shown"
        });
    }

    /// Pane-mode `E`: focus the next floating pane (wrapping), showing it
    /// first if it was hidden. From a tree pane, focuses the first float.
    pub fn cycle_float(&mut self) {
        let (next_idx, was_hidden, len) = {
            let tab = self.current_tab();
            if tab.floating.is_empty() {
                self.toast("no floating panes");
                return;
            }
            let cur = tab.floating.iter().position(|f| f.pane == tab.active);
            let next_idx = match cur {
                Some(i) => (i + 1) % tab.floating.len(),
                None => 0,
            };
            (next_idx, tab.floating[next_idx].hidden, tab.floating.len())
        };
        {
            let tab = self.current_tab_mut();
            tab.floating[next_idx].hidden = false;
            tab.active = tab.floating[next_idx].pane;
        }
        if was_hidden {
            // Grid was not maintained while hidden.
            self.layout_all();
        }
        let msg = if was_hidden {
            format!("float {next_idx}/{len} shown")
        } else {
            format!("float {next_idx}/{len}")
        };
        self.toast(&msg);
    }

    /// Toggle a persistent dropdown floating scratchpad terminal (F12 or Ctrl+~).
    pub fn toggle_scratchpad(&mut self) {
        let content = self.content_rect();
        let pad_w = (content.w * 0.80).clamp(400.0, 1600.0);
        let pad_h = (content.h * 0.70).clamp(250.0, 1000.0);
        let pad_x = content.x + (content.w - pad_w) / 2.0;
        let pad_y = content.y + 12.0;
        let rect = Rect {
            x: pad_x,
            y: pad_y,
            w: pad_w,
            h: pad_h,
        };

        let existing_pos = self.current_tab().floating.iter().position(|f| {
            matches!(self.current_tab().panes.get(f.pane), Some(Pane::Term(t)) if t.name == "scratchpad")
        });

        if let Some(pos) = existing_pos {
            let tab = self.current_tab_mut();
            let is_hidden = tab.floating[pos].hidden;
            if is_hidden {
                tab.floating[pos].hidden = false;
                tab.active = tab.floating[pos].pane;
                self.toast("scratchpad opened (F12 to hide)");
            } else {
                tab.floating[pos].hidden = true;
                if let Some((_, p)) = tab.root.leaves().into_iter().next() {
                    tab.active = p;
                }
                self.toast("scratchpad hidden");
            }
        } else {
            let cols = ((rect.w - 2.0 * self.config.padding) / self.cell.w as f32).max(10.0) as u16;
            let rows = ((rect.h - 2.0 * self.config.padding) / self.cell.h as f32).max(5.0) as u16;
            let cwd = self.active_term().map(|t| t.pwd()).filter(|s| !s.is_empty());
            if let Ok(id) = self.new_term_pane(cols, rows, None, &[], cwd.as_deref(), None) {
                let tab = self.current_tab_mut();
                if let Some(Pane::Term(t)) = tab.panes.get_mut(id) {
                    t.name = "scratchpad".to_string();
                }
                tab.floating.push(FloatingPane {
                    pane: id,
                    rect,
                    origin: None,
                    hidden: false,
                });
                tab.active = id;
                self.toast("scratchpad opened (F12 to hide)");
            }
        }
        self.layout_all();
        self.repaint_all();
    }

    fn cycle_stack(&mut self) {
        let tab = self.current_tab_mut();
        if tab.floating.iter().any(|f| f.pane == tab.active) {
            self.toast("floating panes aren't in a stack");
            return;
        }
        let path = layout::path_to_pane(&tab.root, tab.active).unwrap_or_default();
        if layout::cycle_stack(&mut tab.root, &path) {
            let id = tab.root.pane_id_of_path(&path);
            tab.active = id;
        } else {
            self.toast("not in a stack (create with pane-mode s)");
        }
    }

    pub fn resize(&mut self, dir: Dir, delta: f32) {
        // A floating pane is resized directly (cell steps); the tree is
        // untouched.
        let cell = self.cell;
        let content = self.content_rect();
        let tab = self.current_tab_mut();
        if let Some(fi) = tab.floating.iter().position(|f| f.pane == tab.active) {
            let f = &mut tab.floating[fi];
            let (min_w, min_h) = (2.0 * cell.w as f32, 2.0 * cell.h as f32);
            match dir {
                Dir::Left | Dir::Right => {
                    let step = cell.w as f32 * if delta > 0.0 { 1.0 } else { -1.0 };
                    f.rect.w = (f.rect.w + step).max(min_w);
                }
                Dir::Up | Dir::Down => {
                    let step = cell.h as f32 * if delta > 0.0 { 1.0 } else { -1.0 };
                    f.rect.h = (f.rect.h + step).max(min_h);
                }
            }
            // Keep it inside the content area.
            f.rect.w = f.rect.w.min(content.w - (f.rect.x - content.x));
            f.rect.h = f.rect.h.min(content.h - (f.rect.y - content.y));
            self.layout_all();
            return;
        }
        let path = match layout::path_to_pane(&tab.root, tab.active) {
            Some(p) => p,
            None => return,
        };
        layout::resize_at(&mut tab.root, &path, dir, delta);
    }

    pub fn move_pane(&mut self, dir: Dir) {
        // A floating pane is moved directly (cell steps) — zellij Move mode.
        let cell = self.cell;
        let content = self.content_rect();
        let tab = self.current_tab_mut();
        if let Some(fi) = tab.floating.iter().position(|f| f.pane == tab.active) {
            let f = &mut tab.floating[fi];
            match dir {
                Dir::Left => f.rect.x -= cell.w as f32,
                Dir::Right => f.rect.x += cell.w as f32,
                Dir::Up => f.rect.y -= cell.h as f32,
                Dir::Down => f.rect.y += cell.h as f32,
            }
            f.rect.x = f.rect.x.clamp(content.x, content.x + content.w - f.rect.w);
            f.rect.y = f.rect.y.clamp(content.y, content.y + content.h - f.rect.h);
            self.layout_all();
            return;
        }
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
        // Each plain ssh connection lives in its OWN named session (visible
        // in the session list, no durable marker), so it is identifiable
        // and resumable like any other session.
        let sess = format!("ssh-{}", Self::sanitize_host(host));
        if sess != self.current_session {
            if crate::session::list_sessions().contains(&sess) {
                self.load_session(&sess);
            } else {
                // Park the outgoing session (its panes keep running) and
                // start the ssh session fresh.
                self.park_current();
                self.current_session = sess.clone();
                crate::session::set_last_session(&sess);
            }
        }
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
    /// Host string → filesystem-safe token (used for session names).
    fn sanitize_host(host: &str) -> String {
        host.chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '.' || *c == '-' || *c == '_')
            .collect()
    }

    pub fn remote_session_name(host: &str) -> String {
        format!("ghostwave-{}", Self::sanitize_host(host))
    }

    /// The remote shell command the pane runs: check the cached ghostd
    /// binary, then attach (creating the session if missing).
    fn remote_attach_cmd(host: &str, session: &str) -> String {
        format!(
            "sh -c 'BIN=\"$HOME/.cache/ghostwave/ghostd-$(uname -m)\"; \
             if [ ! -x \"$BIN\" ] || [ \"$($BIN --version 2>/dev/null)\" != \"ghostd {ver}\" ]; then \
             echo \"ghostd missing/stale on {host} — reconnect to upload\"; exit 1; fi; \
             \"$BIN\" attach {session} 2>/dev/null || {{ \"$BIN\" new {session} >/dev/null 2>&1 && exec \"$BIN\" attach {session}; }}'",
            ver = env!("CARGO_PKG_VERSION")
        )
    }

    /// Open a durable remote pane: `ssh -t host` running the ghostd attach
    /// command. The ghostd binary is uploaded in the background on first
    /// use; a stale/missing binary shows an error and the reconnect loop
    /// retries once the upload lands.
    ///
    /// ds sessions live in their OWN local session (`ssh-<host>`) so the
    /// session manager lists them independently and can kill them; a
    /// repeat `ds <host>` focuses/reconnects the existing attach pane
    /// instead of stacking duplicates (the daemon serves one client at a
    /// time anyway).
    pub fn connect_durable_ssh(&mut self, host: &str) {
        let target = format!("ssh-{}", Self::sanitize_host(host));
        if self.current_session != target {
            if crate::session::load(&target).is_some() {
                self.load_session(&target);
            } else {
                // Fresh empty session — no default shell tab; the remote
                // attach pane is added below.
                self.park_current();
                self.tabs = Vec::new();
                self.active_tab = 0;
                self.finish_session_switch(&target);
            }
        }
        // Dedup: an attach pane for this host already exists → focus it
        // and reconnect if it's not live.
        let mut existing: Option<(usize, usize)> = None;
        for (ti, tab) in self.tabs.iter().enumerate() {
            for (pi, pane) in tab.panes.iter().enumerate() {
                if let Pane::Term(t) = pane {
                    if t.remote.as_ref().is_some_and(|r| r.host == host) {
                        existing = Some((ti, pi));
                        break;
                    }
                }
            }
            if existing.is_some() {
                break;
            }
        }
        if let Some((t, p)) = existing {
            self.active_tab = t;
            self.current_tab_mut().active = p;
            let needs_connect = match self.current_tab().panes.get(p) {
                Some(Pane::Term(tp)) => {
                    tp.exited.is_some() || tp.reconnect != crate::term::ReconnectState::Idle
                }
                _ => false,
            };
            if needs_connect {
                self.connect_remote_pane(t, p);
                self.toast(&format!("reconnecting durable ssh: {host}"));
            } else {
                self.toast(&format!("already attached to {host}"));
            }
            return;
        }
        let session = Self::remote_session_name(host);
        let cmd = Self::remote_attach_cmd(host, &session);
        gdbg(format!("ds {host}: session={session} uploading-binary-check"));
        self.ensure_remote_binary(host, &session);
        let cols = 100;
        let rows = 30;
        let args = ["-t", "-o", "ConnectTimeout=10", host, &cmd];
        gdbg(format!("ds {host}: ssh argv: -t -o ConnectTimeout=10 {host} {cmd:?}"));
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
        gdbg(format!("upload thread for {host}: starting"));
        std::thread::spawn(move || {
            let ok = upload_ghostd(&host);
            gdbg(format!("upload thread for {host}: result ok={ok}"));
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
        Self::spawn_remote_kill(&host, &session);
    }

    /// ssh to `host` and kill the ghostd daemon for `dsession`, then pkill
    /// stragglers. Runs on a fresh ssh connection (never the pane's own),
    /// so it works when the local pane is dead or the daemon is wedged.
    /// The remote ghostd escalates TERM→KILL and kills the session's
    /// process group; the pkill is the last resort for orphans whose
    /// pidfile is already gone.
    fn spawn_remote_kill(host: &str, dsession: &str) {
        let host = host.to_string();
        let dsession = dsession.to_string();
        gdbg(format!("kill {dsession} on {host}: spawning ssh"));
        std::thread::spawn(move || {
            let cmd = format!(
                "sh -c 'BIN=\"$HOME/.cache/ghostwave/ghostd-$(uname -m)\"; \
                 if [ ! -x \"$BIN\" ]; then echo \"no ghostd binary on {host}\"; exit 1; fi; \
                 \"$BIN\" kill {dsession} 2>&1 || true; \
                 pkill -9 -f \"ghostd-$(uname -m) new {dsession}\" 2>/dev/null || true; true'"
            );
            let _ = std::process::Command::new("ssh")
                .args(["-o", "ConnectTimeout=5", &host, &cmd])
                .status();
        });
    }

    /// Session manager `k`: kill every durable remote daemon owned by the
    /// selected session (fresh ssh, works when panes are dead/stuck), then
    /// drop the entry — pure ds sessions are deleted wholesale, mixed
    /// sessions keep their local tabs.
    fn kill_session_remotes(&mut self, name: &str) {
        let (specs, pure) = if name == self.current_session {
            (Self::live_remote_specs(&self.tabs), Self::live_is_pure_remote(&self.tabs))
        } else if let Some(p) = self.parked.iter().find(|s| s.name == name) {
            (Self::live_remote_specs(&p.tabs), Self::live_is_pure_remote(&p.tabs))
        } else {
            (crate::session::remote_specs(name), crate::session::is_pure_remote(name))
        };
        if specs.is_empty() {
            self.toast(&format!("no remote sessions in {name}"));
            return;
        }
        // Dedupe by host (one daemon session per host).
        let mut hosts: Vec<(String, String)> = Vec::new();
        for (h, s) in specs {
            if !hosts.iter().any(|(hh, _)| hh == &h) {
                hosts.push((h, s));
            }
        }
        for (host, dsession) in &hosts {
            Self::spawn_remote_kill(host, dsession);
        }
        if name != self.current_session && pure {
            if let Some(idx) = self.parked.iter().position(|s| s.name == name) {
                self.parked.remove(idx);
            }
            crate::session::delete(name);
            let entries = crate::session::list_sessions();
            if let Some((e, sel)) = self.session_list.as_mut() {
                *e = entries;
                if *sel >= e.len() {
                    *sel = e.len().saturating_sub(1);
                }
            }
            self.toast(&format!(
                "killed {} remote session(s) on {}, removed {name}",
                hosts.len(),
                hosts.iter().map(|(h, _)| h.as_str()).collect::<Vec<_>>().join(", ")
            ));
        } else {
            self.toast(&format!(
                "killed {} remote session(s) on {}",
                hosts.len(),
                hosts.iter().map(|(h, _)| h.as_str()).collect::<Vec<_>>().join(", ")
            ));
        }
    }

    /// (host, daemon-session) pairs across live tabs (current or parked).
    fn live_remote_specs(tabs: &[Tab]) -> Vec<(String, String)> {
        let mut v = Vec::new();
        for tab in tabs {
            for pane in &tab.panes {
                if let Pane::Term(t) = pane {
                    if let Some(r) = &t.remote {
                        v.push((r.host.clone(), r.session.clone()));
                    }
                }
            }
        }
        v
    }

    /// True when every live pane is a durable remote pane (a pure ds
    /// session — deleteable after its daemons are killed).
    fn live_is_pure_remote(tabs: &[Tab]) -> bool {
        let mut total = 0;
        let mut remote = 0;
        for tab in tabs {
            for pane in &tab.panes {
                total += 1;
                if matches!(pane, Pane::Term(t) if t.remote.is_some()) {
                    remote += 1;
                }
            }
        }
        total > 0 && total == remote
    }

    /// Palette "Kill Remote Session": kill the remote ghostd session of the
    /// ACTIVE pane (connected or disconnected) and close the local pane —
    /// the only way to fully get rid of a durable remote session.
    fn kill_active_remote_pane(&mut self) {
        let t = self.active_tab;
        let p = self.active_pane_id();
        let Some((host, session)) = self.remote_spec_of(t, p) else {
            self.toast("no remote session in active pane");
            return;
        };
        self.kill_remote_pane(t, p);
        // Close the local pane too (the remote session is gone — the pane
        // is useless; its ssh/attach dies with the daemon anyway).
        self.close_pane();
        self.toast(&format!("killed remote session {session} on {host}"));
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
    /// became ready) — including parked sessions' panes.
    fn respawn_waiting(&mut self, host: &str) {
        Self::respawn_waiting_tabs(&mut self.tabs, host);
        for live in self.parked.iter_mut() {
            Self::respawn_waiting_tabs(&mut live.tabs, host);
        }
    }

    fn respawn_waiting_tabs(tabs: &mut [Tab], host: &str) {
        for tab in tabs.iter_mut() {
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

    pub fn open_visualizer(&mut self) {
        let pane = Pane::Visual(crate::visualizer::VisualizerPane::new());
        self.push_side_pane(pane);
        self.toast("ghostwave visualizer (Space: color mode)");
    }

    /// Add a non-terminal pane as a right split.
    fn push_side_pane(&mut self, pane: Pane) {
        let path = self.active_tree_path();
        let tab = self.current_tab_mut();
        let id = tab.panes.len();
        tab.panes.push(pane);
        if matches!(tab.root, Node::Leaf { pane: usize::MAX }) {
            tab.root = Node::Leaf { pane: id };
        } else {
            layout::split_at(&mut tab.root, &path, SplitDir::Horizontal, id);
        }
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
        let cursor_style = self.config.cursor_style.as_deref().and_then(TermPane::parse_cursor_style);
        for tab in self.tabs.iter_mut() {
            for pane in tab.panes.iter_mut() {
                if let Pane::Term(t) = pane {
                    let _ = t.term.set_default_fg_color(Some(RgbColor { r: fg.0, g: fg.1, b: fg.2 }));
                    let _ = t.term.set_default_bg_color(Some(RgbColor { r: bg.0, g: bg.1, b: bg.2 }));
                    t.cursor_style = cursor_style;
                }
            }
        }
    }

    /// Persist the durable session state (JSON + pane logs). Called on quit.
    pub fn save_session(&self) {
        crate::session::save(
            &self.current_session,
            &Self::session_state_for(&self.tabs, self.cell),
        );
        crate::session::set_last_session(&self.current_session);
    }

    /// Persist every live session (active + parked) — used on quit so a
    /// background session's metadata isn't lost (its pane logs are already
    /// fresh; they grow at drain time even while parked).
    pub fn save_all_sessions(&self) {
        self.save_session();
        for live in &self.parked {
            crate::session::save(&live.name, &Self::session_state_for(&live.tabs, self.cell));
        }
    }

    /// Build the on-disk session state for a set of tabs (terminal panes
    /// only; preview/AI panes are not restored).
    fn session_state_for(tabs: &[Tab], cell: CellDims) -> crate::session::SessionState {
        crate::session::SessionState {
            tabs: tabs
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
                    // Floating panes: remap ids, store overlay rect in cells
                    // (grid-independent across window sizes).
                    let floating: Vec<crate::session::FloatingPaneState> = tab
                        .floating
                        .iter()
                        .filter_map(|f| {
                            let new_id = *map.get(&f.pane)?;
                            Some(crate::session::FloatingPaneState {
                                pane: new_id,
                                x: (f.rect.x / cell.w as f32).max(0.0) as u16,
                                y: ((f.rect.y - TAB_BAR_H) / cell.h as f32).max(0.0) as u16,
                                w: (f.rect.w / cell.w as f32).max(1.0) as u16,
                                h: (f.rect.h / cell.h as f32).max(1.0) as u16,
                                origin: f.origin.clone(),
                                hidden: f.hidden,
                            })
                        })
                        .collect();
                    crate::session::TabState {
                        name: tab.name.clone(),
                        panes,
                        root: Some(root),
                        floating,
                    }
                })
                .collect(),
        }
    }

    /// Save + park the current session: its panes keep RUNNING in the
    /// background (drained every frame in update()) while another session
    /// is shown. The active tab set is cleared; callers must populate it.
    /// An empty session is not parked (nothing to keep alive) and is not
    /// written to disk — a deleted or never-populated session must not be
    /// resurrected by a switch.
    fn park_current(&mut self) {
        if self.tabs.is_empty() {
            return;
        }
        self.save_session();
        self.parked.push(LiveSession {
            name: self.current_session.clone(),
            tabs: std::mem::take(&mut self.tabs),
            active_tab: self.active_tab,
        });
        self.active_tab = 0;
    }

    /// Common tail of load_session/new_session: adopt `name` as the current
    /// session, persist the marker, and refresh colors/layout.
    fn finish_session_switch(&mut self, name: &str) {
        self.current_session = name.to_string();
        crate::session::set_last_session(name);
        self.apply_terminal_colors();
        self.layout_all();
        self.toast(&format!("session: {name}"));
    }

    /// Set every remote pane on `host` to Manual (upload failed) — active
    /// and parked sessions alike.
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
        for live in self.parked.iter_mut() {
            for tab in live.tabs.iter_mut() {
                for pane in tab.panes.iter_mut() {
                    if let Pane::Term(t) = pane {
                        if t.remote.as_ref().is_some_and(|r| r.host == host) {
                            t.reconnect = crate::term::ReconnectState::Manual;
                        }
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
                active: (*active).min(panes.len().saturating_sub(1)),
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
                // Create the terminal WITHOUT spawning, replay history first,
                // THEN start the shell. If the shell started before the
                // replay, its startup terminal probes (fish 4: kitty/DA/
                // XTVERSION/OSC 11) would sit in the pty buffer during the
                // replay and get answered after the shell's read timeout —
                // the responses then get echoed as caret-escaped garbage.
                let Ok(mut pane) = TermPane::new(
                    cols,
                    rows,
                    cell,
                    ps.program.as_deref(),
                    &args,
                    ps.cwd.as_deref(),
                    config.scrollback,
                    config.max_log_mb * 1024 * 1024,
                    false,
                    ps.remote.clone(),
                ) else {
                    continue;
                };
                let seeded = crate::session::replay_pane_log(
                    &ps.log,
                    &mut pane.term,
                    config.max_log_mb * 1024 * 1024,
                );
                pane.log_seed(&seeded);
                if let Some(remote) = &ps.remote {
                    // Remote panes restore DISCONNECTED: history shows, but
                    // the ssh only connects when the user presses Enter (or
                    // an in-session reconnect loop fires — never on restore).
                    pane.exited = Some("disconnected (remote session)".into());
                    pane.last_exit_code = Some(1);
                    pane.reconnect = crate::term::ReconnectState::Manual;
                } else if let Err(e) = pane.spawn_child() {
                    pane.exited = Some(format!("failed to start: {e}"));
                    pane.last_exit_code = Some(1);
                }
                pane.force_repaint = true;
                panes.push(Pane::Term(pane));
            }
            if !panes.is_empty() {
                let mut tab = Tab::from_panes(panes, ts.name.clone());
                // Restore the saved split/stack layout (pane ids already
                // remapped at save time). Old files lack it → single pane.
                if let Some(root) = &ts.root {
                    tab.root = root.clone();
                }
                // Restore floating panes (cells → logical px at this cell
                // size). Skip states referencing panes that failed to build.
                tab.floating = ts
                    .floating
                    .iter()
                    .filter_map(|fs| {
                        if fs.pane >= tab.panes.len() {
                            return None;
                        }
                        if !matches!(tab.panes[fs.pane], Pane::Term(_)) {
                            return None;
                        }
                        Some(FloatingPane {
                            pane: fs.pane,
                            rect: Rect {
                                x: fs.x as f32 * cell.w as f32,
                                y: TAB_BAR_H + fs.y as f32 * cell.h as f32,
                                w: (fs.w as f32 * cell.w as f32).max(1.0),
                                h: (fs.h as f32 * cell.h as f32).max(1.0),
                            },
                            origin: fs.origin.clone(),
                            hidden: fs.hidden,
                        })
                    })
                    .collect();
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
        // If the target session is STILL ALIVE in this process (parked when
        // we switched away), swap the live tab sets — no shell respawn, no
        // log replay; its processes kept running the whole time.
        if let Some(idx) = self.parked.iter().position(|s| s.name == name) {
            let mut live = self.parked.remove(idx);
            self.park_current();
            self.tabs = std::mem::take(&mut live.tabs);
            self.active_tab = live.active_tab;
            self.finish_session_switch(name);
            return;
        }
        // Not alive this run → restore from disk (fresh shells + log replay).
        // Build the incoming session FIRST so a failure leaves the current
        // session untouched (it is only parked once the new one is ready).
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
        self.park_current();
        self.tabs = tabs;
        self.active_tab = 0;
        self.finish_session_switch(name);
    }

    /// Start a fresh (empty) session under `name`.
    pub fn new_session(&mut self, name: &str) {
        let cols: u16 = 100;
        let rows: u16 = 30;
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
                self.toast(&format!("failed to start session: {e}"));
                return;
            }
        };
        self.park_current();
        self.tabs = vec![Tab::new(Pane::Term(pane), "Tab 1".into())];
        self.active_tab = 0;
        self.finish_session_switch(name);
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
        // Floating panes keep their own rects (move/resize adjust them);
        // their grids follow (hidden bg panes skip — no visible rect).
        for f in tab.floating.iter_mut() {
            if f.hidden {
                continue;
            }
            if let Some(Pane::Term(t)) = tab.panes.get_mut(f.pane) {
                let cols = ((f.rect.w - 2.0 * pad) / cell.w as f32).max(1.0) as u16;
                let rows = ((f.rect.h - 2.0 * pad) / cell.h as f32).max(1.0) as u16;
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
        let tab = self.current_tab();
        if let Some(f) = tab.floating.iter().rev().find(|f| f.pane == id && !f.hidden) {
            return Some(f.rect);
        }
        self.active_pane_rects()
            .into_iter()
            .find(|(p, _)| *p == id)
            .map(|(_, r)| r)
    }

    // ---- input -----------------------------------------------------------

    pub fn handle_key(&mut self, kp: KeyPress) {
        let k = kp.key;
        let m = kp.mods;

        // Keystroke energizes the living visual wave
        if self.config.visual_wave {
            self.wave_energy = (self.wave_energy + 0.25).min(2.5);
        }

        // Locked mode: ONLY Ctrl+g unlocks. ALL other keys pass straight to terminal!
        if self.mode == Mode::Locked {
            if kp.key == Key::G && kp.mods.contains(Mods::CTRL) {
                self.mode = Mode::Normal;
                self.toast("unlocked");
            } else if let Some(t) = self.active_term_mut() {
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
            return;
        }

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
                    self.rename_buf = name.clone().unwrap_or_default();
                    self.rename_target = name.unwrap_or_default();
                }
                Key::D => {
                    let name = self
                        .session_list
                        .as_ref()
                        .and_then(|(e, sel)| e.get(*sel).cloned());
                    if let Some(name) = name {
                        if name != self.current_session {
                            crate::session::delete(&name);
                            // If the session is parked (still alive), drop it
                            // too — its processes die, which is the point of
                            // deleting a session.
                            if let Some(idx) = self.parked.iter().position(|s| s.name == name) {
                                self.parked.remove(idx);
                            }
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
                Key::K => {
                    let name = self
                        .session_list
                        .as_ref()
                        .and_then(|(e, sel)| e.get(*sel).cloned());
                    if let Some(name) = name {
                        self.kill_session_remotes(&name);
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

        // Mode entry binds (skipped in Locked mode so passthrough works).
        if self.mode != Mode::Locked {
            if let Some(new_mode) = keymap::entry_bind(k, m) {
                self.mode = if self.mode == new_mode { Mode::Normal } else { new_mode };
                return;
            }
        }

        match keymap::handle(self.mode, &kp) {
            Dispatch::Action(action) => {
                // Modal zellij modes return to Normal after most actions.
                let was_modal = matches!(
                    self.mode,
                    Mode::Pane | Mode::Tab | Mode::Resize | Mode::Session
                );
                self.execute(action);
                if was_modal
                    && matches!(
                        self.mode,
                        Mode::Pane | Mode::Tab | Mode::Resize | Mode::Session
                    )
                {
                    self.mode = Mode::Normal;
                }
            }
            Dispatch::Pass => {
                let tab = self.current_tab_mut();
                let consumed = if kp.text.is_some() && m.contains(Mods::SHIFT) {
                    Mods::SHIFT
                } else {
                    Mods::empty()
                };
                let text = kp.text.map(|c| c.to_string());
                let unshifted = keymap::unshifted_codepoint(k);

                if tab.sync_panes {
                    for pane in &mut tab.panes {
                        if let Pane::Term(t) = pane {
                            let _ = t.handle_key(
                                k,
                                m,
                                consumed,
                                unshifted,
                                text.as_deref(),
                                key::Action::Press,
                            );
                        }
                    }
                } else if let Some(Pane::Visual(v)) = tab.panes.get_mut(tab.active) {
                    if k == Key::Space {
                        v.cycle_mode();
                        self.repaint_all();
                    } else if k == Key::Equal {
                        v.speed_mult = (v.speed_mult + 0.25).min(4.0);
                    } else if k == Key::Minus {
                        v.speed_mult = (v.speed_mult - 0.25).max(0.25);
                    } else if k == Key::Q {
                        self.close_pane();
                    }
                } else if let Some(t) = tab.panes.get_mut(tab.active).and_then(|p| match p {
                    Pane::Term(t) => Some(t),
                    _ => None,
                }) {
                    let _ = t.handle_key(
                        k,
                        m,
                        consumed,
                        unshifted,
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
        if self.mode == Mode::Locked {
            if kp.key == Key::G && kp.mods.contains(Mods::CTRL) {
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
            return;
        }
        match self.mode {
            Mode::Normal => {
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

        let any_pressed = is_mouse_button_down(MouseButton::Left)
            || is_mouse_button_down(MouseButton::Right)
            || is_mouse_button_down(MouseButton::Middle);
        let any_released = is_mouse_button_released(MouseButton::Left)
            || is_mouse_button_released(MouseButton::Right)
            || is_mouse_button_released(MouseButton::Middle);

        // If a mouse drag is in progress on the current tab, capture all mouse events to that pane.
        let captured = if let Some((tab_idx, pane_id)) = self.mouse_pane {
            if tab_idx == self.active_tab && (any_pressed || any_released) {
                self.pane_rect_of(pane_id).map(|r| (pane_id, r))
            } else {
                None
            }
        } else {
            None
        };

        let (pane_id, pane_rect) = if let Some(h) = captured {
            h
        } else {
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
            // Floating panes are hit-tested FIRST (topmost; later floats draw
            // above earlier ones). Tree panes only get the click if no float
            // covers it.
            let float_hit = {
                let tab = self.current_tab();
                tab.floating
                    .iter()
                    .rev()
                    .filter(|f| !f.hidden) // hidden floats are bg panes
                    .find(|f| f.rect.contains(mx, my))
                    .map(|f| (f.pane, f.rect))
            };
            let hit = if let Some(h) = float_hit {
                Some(h)
            } else {
                rects
                    .iter()
                    .find(|(_, r)| r.contains(mx, my))
                    .map(|(id, r)| (*id, *r))
            };
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
            if is_mouse_button_pressed(MouseButton::Left)
                || is_mouse_button_pressed(MouseButton::Right)
                || is_mouse_button_pressed(MouseButton::Middle)
            {
                self.mouse_pane = Some((self.active_tab, pane_id));
            }

            (pane_id, pane_rect)
        };

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

        // Ctrl+Click opens URLs in default browser or local files in Preview pane.
        if is_mouse_button_pressed(MouseButton::Left) && mods.contains(Mods::CTRL) {
            let pad = self.config.padding;
            let cell = self.cell;
            let cell_col = ((lx - pad) / cell.w as f32).max(0.0) as u16;
            let cell_row = ((ly - pad) / cell.h as f32).max(0.0) as u32;
            let link_opt = self.current_tab_mut().panes.get_mut(pane_id).and_then(|p| match p {
                Pane::Term(t) => t.link_at_cell(cell_row, cell_col).map(|l| (l, t.pwd())),
                _ => None,
            });
            if let Some(((link, _, _), pwd)) = link_opt {
                if link.starts_with("http://") || link.starts_with("https://") {
                    let _ = std::process::Command::new("xdg-open").arg(&link).spawn();
                    self.toast(&format!("opened {link}"));
                    return;
                } else {
                    let file_path = link.split(':').next().unwrap_or(&link);
                    let full = if std::path::Path::new(file_path).is_absolute() {
                        std::path::PathBuf::from(file_path)
                    } else {
                        std::path::PathBuf::from(pwd).join(file_path)
                    };
                    if full.exists() {
                        let path_str = full.to_string_lossy().to_string();
                        self.open_file(path_str);
                        return;
                    }
                }
            }
        }

        let tab = self.current_tab_mut();
        let Some(Pane::Term(t)) = tab.panes.get_mut(pane_id) else {
            // Preview/AI panes: wheel scrolls.
            if let Some(Pane::Preview(p)) = tab.panes.get_mut(pane_id) {
                let (wx, wy) = mouse_wheel();
                if wy != 0.0 {
                    if p.kind == crate::preview::PreviewKind::Image {
                        if wy > 0.0 {
                            p.zoom_in();
                        } else {
                            p.zoom_out();
                        }
                    } else {
                        p.scroll_lines(if wy > 0.0 { -3 } else { 3 });
                    }
                }
                if wx != 0.0 && p.kind == crate::preview::PreviewKind::Image {
                    p.pan(wx * 10.0, 0.0);
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
        if is_mouse_button_pressed(MouseButton::Middle) {
            let _ = t.handle_mouse(
                pane_rect,
                cell,
                padding,
                MouseEv {
                    kind: MouseEvKind::Press(libghostty_vt::mouse::Button::Middle),
                    x: lx,
                    y: ly,
                    any_pressed,
                    button_down,
                },
                mods,
            );
            if !t.is_mouse_tracking() {
                t.paste_primary();
            }
        }
        if is_mouse_button_released(MouseButton::Middle) {
            let _ = t.handle_mouse(
                pane_rect,
                cell,
                padding,
                MouseEv {
                    kind: MouseEvKind::Release(libghostty_vt::mouse::Button::Middle),
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

        if !any_pressed {
            self.mouse_pane = None;
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

        // Drain pty output for every pane in every tab — active session AND
        // parked sessions. Parked panes must keep draining or their pty
        // buffers fill and the child processes stall; their logs also grow
        // here, so the on-disk session stays current while hidden.
        self.pty_activity |= drain_tab_panes(&mut self.tabs);
        for live in self.parked.iter_mut() {
            self.pty_activity |= drain_tab_panes(&mut live.tabs);
        }

        // Elapsed time clamped to avoid large jumps across pauses / window minimizes
        let dt = macroquad::time::get_frame_time().clamp(0.001, 0.1);

        // Living reactive sine-wave animation update:
        if self.config.visual_wave {
            if self.pty_activity {
                self.wave_energy = (self.wave_energy + 0.12).min(2.5);
            }
            let decay = (-3.713 * dt).exp();
            self.wave_energy = (self.wave_energy * decay).max(0.1);
            let speed_mult = if self.wave_energy > 0.15 {
                1.0 + self.wave_energy * 1.5
            } else {
                0.35
            };
            self.wave_phase += 2.4 * dt * self.config.wave_speed * speed_mult;
            if self.wave_phase > std::f32::consts::TAU * 100.0 {
                self.wave_phase -= std::f32::consts::TAU * 100.0;
            }
        }

        // Update all visualizer screensaver panes
        for tab in &mut self.tabs {
            for pane in &mut tab.panes {
                if let Pane::Visual(v) = pane {
                    v.update(dt);
                }
            }
        }

        // ---- durable remote sessions ----

        // ghostd upload results: mark the host ready and respawn panes that
        // were waiting for the binary.
        while let Ok((host, _session, ok)) = self.remote_uploads.try_recv() {
            gdbg(format!("upload result {host}: ok={ok}"));
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
        // then drops to manual. Runs over the active session AND parked
        // sessions — a durable remote session keeps reconnecting while you
        // work elsewhere. Panes whose binary isn't uploaded yet wait for the
        // upload signal instead of burning attempts.
        let now = std::time::Instant::now();
        {
            let App { tabs, parked, config, binary_ready, .. } = self;
            remote_reconnect_pass(config, binary_ready, tabs, now);
            for live in parked.iter_mut() {
                remote_reconnect_pass(config, binary_ready, &mut live.tabs, now);
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

/// Drain pty output for every pane in every tab; returns true if any pty
/// produced data (drives the event-paced frame loop). Also flushes each
/// pane's response buffer. Runs for the active session AND parked sessions
/// every frame, so background children never stall on full pty buffers and
/// their durable logs keep growing.
fn drain_tab_panes(tabs: &mut [Tab]) -> bool {
    let mut activity = false;
    for tab in tabs.iter_mut() {
        let mut i = 0;
        while i < tab.panes.len() {
            match &mut tab.panes[i] {
                Pane::Term(t) => {
                    if t.poll_pty() {
                        activity = true;
                    }
                }
                Pane::Ai(a) => a.poll(),
                Pane::Preview(_) => {}
                Pane::Visual(_) => {}
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
    activity
}

/// One reconnect pass over a session's tabs: advance dead remote panes
/// through the bounded auto-reconnect state machine and respawn due ones.
fn remote_reconnect_pass(
    config: &Config,
    binary_ready: &std::collections::HashSet<String>,
    tabs: &mut [Tab],
    now: std::time::Instant,
) {
    let mut to_respawn: Vec<(usize, usize)> = Vec::new();
    let to_manual: Vec<(usize, usize)> = Vec::new();
    for (ti, tab) in tabs.iter_mut().enumerate() {
        for (pi, pane) in tab.panes.iter_mut().enumerate() {
            let Pane::Term(t) = pane else { continue };
            if t.remote.is_none() {
                continue;
            }
            if t.exited.is_some() && t.reconnect == crate::term::ReconnectState::Idle {
                let code = t.last_exit_code.unwrap_or(255);
                let auto = config.remote.auto_reconnect && code != 0;
                if auto && t.reconnect_failures < config.remote.reconnect_attempts {
                    t.reconnect_failures += 1;
                    let delay = config.remote.reconnect_delay_secs.max(1);
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
                        if binary_ready.contains(&h) {
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
        if let Pane::Term(t) = &mut tabs[ti].panes[pi] {
            if !t.respawn() {
                t.reconnect = crate::term::ReconnectState::Manual;
            }
        }
    }
    for (ti, pi) in to_manual {
        if let Pane::Term(t) = &mut tabs[ti].panes[pi] {
            t.reconnect = crate::term::ReconnectState::Manual;
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
        None => {
            gdbg(format!("upload {host}: uname -m failed"));
            return false;
        }
    };
    gdbg(format!("upload {host}: arch={arch}"));
    let bytes: &[u8] = match arch.as_str() {
        "x86_64" => GHOSTD_X86_64,
        "aarch64" => GHOSTD_AARCH64,
        _ => {
            gdbg(format!("upload {host}: unsupported arch {arch}"));
            return false;
        }
    };
    // 2. Already current? (version check on the remote).
    let check = format!(
        "sh -c 'BIN=\"$HOME/.cache/ghostwave/ghostd-{arch}\"; [ -x \"$BIN\" ] && [ \"$($BIN --version 2>/dev/null)\" = \"ghostd {ver}\" ]'",
        ver = env!("CARGO_PKG_VERSION")
    );
    if ssh_ok(host, &check) {
        gdbg(format!("upload {host}: binary current, no upload"));
        return true;
    }
    gdbg(format!("upload {host}: uploading {} bytes", bytes.len()));
    // 3. Upload the binary over stdin.
    let mut child = match std::process::Command::new("ssh")
        .args([
            "-o",
            "ConnectTimeout=5",
            host,
            &format!(
                "sh -c 'mkdir -p \"$HOME/.cache/ghostwave\" && cat > \"$HOME/.cache/ghostwave/ghostd-{arch}.new\" && chmod +x \"$HOME/.cache/ghostwave/ghostd-{arch}.new\" && mv -f \"$HOME/.cache/ghostwave/ghostd-{arch}.new\" \"$HOME/.cache/ghostwave/ghostd-{arch}\"'"
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::Node;
    use std::collections::HashMap;

    #[test]
    fn remap_layout_preserves_stack_active_index() {
        let stack = Node::Stack {
            panes: vec![10, 20],
            active: 1, // active points to panes[1] == 20
        };
        let mut map = HashMap::new();
        // Suppose old pane 10 -> new pane 0, and old pane 20 -> new pane 1.
        // Also map 1 -> 99 to simulate the bug if active (1) were looked up as a pane ID.
        map.insert(10, 0);
        map.insert(20, 1);
        map.insert(1, 99);

        let remapped = App::remap_layout(&stack, &map);
        match remapped {
            Node::Stack { panes, active } => {
                assert_eq!(panes, vec![0, 1]);
                assert_eq!(active, 1, "active index should remain 1 and not be remapped as a pane ID");
                assert_eq!(panes[active], 1);
            }
            _ => panic!("expected Node::Stack"),
        }
    }

    #[test]
    fn locked_mode_prevents_mode_entry_binds() {
        use libghostty_vt::key::{Key, Mods};
        let kp = crate::keymap::KeyPress {
            key: Key::P,
            mods: Mods::CTRL,
            text: None,
        };
        // Verify entry_bind matches Pane
        assert_eq!(crate::keymap::entry_bind(kp.key, kp.mods), Some(Mode::Pane));

        // When locked, keymap::handle produces Pass instead of Pane mode action
        assert!(matches!(
            crate::keymap::handle(Mode::Locked, &kp),
            crate::keymap::Dispatch::Pass
        ));
    }
}