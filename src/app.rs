//! iced-side session state: each tab's pane grid, its panes, and their geometry.
//!
//! A tab's [`pane_grid::State`] is its layout authority — splits, ratios, cells,
//! where the focus sits and which pane is zoomed all live there, and every pane's
//! bounds are read back from it rather than derived a second time.
//!
//! Inside a tab a pane is addressed by its index into [`Tab::panes`], which is
//! never reused (a closed pane leaves a tombstone). Anything that can outlive a
//! tab — pty output, exit notices — is addressed by [`PaneId`].

use std::sync::{Arc, Mutex};

use ghostwave::config::Config;
use ghostwave::layout::{Dir, SplitDir};
use ghostwave::term::CellDims;
use ghostwave::theme::{self, Theme};
use iced::Size;
use iced::widget::pane_grid::{self, Axis, Split};

use crate::pane::{ContentPane, Pane, PaneEntry, PaneId, SharedPane};
use crate::panes_grid;

/// Ratios a resize keeps a split within, the same limits the canvas pane layer
/// applies when it moves one.
const RATIO_MIN: f32 = 0.1;
const RATIO_MAX: f32 = 0.9;

/// Grid a pane is born with; the pane's own bounds correct it before the first
/// paint.
const SPAWN_COLS: u16 = 80;
const SPAWN_ROWS: u16 = 24;

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// One cell of a tab's grid: the panes sharing it.
///
/// One pane is a plain pane; several are a zellij-style stack, `active` naming
/// the member that is drawn and typed into.
#[derive(Clone, Debug)]
pub struct GridPane {
    pub panes: Vec<PaneId>,
    pub active: usize,
}

impl GridPane {
    /// A cell holding one pane.
    pub fn single(id: PaneId) -> Self {
        Self {
            panes: vec![id],
            active: 0,
        }
    }

    /// The pane this cell shows.
    pub fn active_id(&self) -> Option<PaneId> {
        self.panes.get(self.active).copied()
    }

    /// Whether `id` is one of the cell's panes.
    pub fn contains(&self, id: PaneId) -> bool {
        self.panes.contains(&id)
    }

    /// Drops the shown pane and leaves the cell on its next member; `None` when
    /// the cell holds a single pane, whose cell closes instead.
    fn pop_active(&mut self) -> Option<PaneId> {
        if self.panes.len() < 2 {
            return None;
        }
        let gone = self.panes.remove(self.active);
        self.active = self.active.min(self.panes.len() - 1);
        Some(gone)
    }
}

pub struct Tab {
    pub name: String,
    /// Index = pane index. `None` = closed (tombstone, index never reused).
    pub panes: Vec<Option<PaneEntry>>,
    /// The tab's layout: one cell per pane or stack.
    pub grid: pane_grid::State<GridPane>,
    /// Index into `panes` of the focused pane, which the grid holds.
    pub active: usize,
    /// Status-bar SYNC chip: synchronised input across the tab's panes.
    pub sync_panes: bool,
}

impl Tab {
    fn new(name: String, entry: PaneEntry) -> Self {
        Self {
            name,
            grid: pane_grid::State::new(GridPane::single(entry.id())).0,
            panes: vec![Some(entry)],
            active: 0,
            sync_panes: false,
        }
    }

    /// A tab restored from a saved session: the panes it kept and the grid their
    /// saved layout tree converts to ([`crate::grid_session`]), with the first
    /// pane focused.
    pub fn restored(name: String, panes: Vec<PaneEntry>, grid: pane_grid::State<GridPane>) -> Self {
        Self {
            name,
            grid,
            panes: panes.into_iter().map(Some).collect(),
            active: 0,
            sync_panes: false,
        }
    }

    /// Tab-bar label: the tab's name, or the active pane's OSC title when the name
    /// is unset or still the generated `"Tab N"`.
    pub fn label(&self, index: usize) -> String {
        let generated = format!("Tab {}", index + 1);
        if !self.name.is_empty() && !self.name.starts_with("Tab ") {
            return self.name.clone();
        }
        if let Some(pane) = self.pane(self.active) {
            let guard = pane.lock().unwrap();
            if !guard.cached_title.is_empty() {
                return guard.cached_title.clone();
            }
        }
        generated
    }

    /// The pane at `index`, whatever kind it is.
    pub fn entry(&self, index: usize) -> Option<&PaneEntry> {
        self.panes.get(index)?.as_ref()
    }

    /// The pane at `index`, whatever kind it is.
    pub fn entry_mut(&mut self, index: usize) -> Option<&mut PaneEntry> {
        self.panes.get_mut(index)?.as_mut()
    }

    /// The content pane at `index`; `None` for a terminal or a tombstone.
    pub fn content(&self, index: usize) -> Option<&ContentPane> {
        self.entry(index)?.content()
    }

    /// The content pane at `index`; `None` for a terminal or a tombstone.
    pub fn content_mut(&mut self, index: usize) -> Option<&mut ContentPane> {
        self.entry_mut(index)?.content_mut()
    }

    /// The terminal at `index` — the accessor every terminal-only path reads.
    /// `None` for a content pane, which has no pty and no VT.
    pub fn pane(&self, id: usize) -> Option<&SharedPane> {
        self.entry(id)?.term()
    }

    /// Every live terminal in the tab, in index order; content panes are not in
    /// it.
    pub fn term_handles(&self) -> Vec<SharedPane> {
        self.panes
            .iter()
            .flatten()
            .filter_map(PaneEntry::term)
            .cloned()
            .collect()
    }

    /// The handle of the terminal with identity `id`, wherever the grid holds it.
    pub fn shared(&self, id: PaneId) -> Option<SharedPane> {
        self.panes
            .iter()
            .flatten()
            .find(|entry| entry.id() == id)
            .and_then(PaneEntry::term)
            .cloned()
    }

    /// The index (into `panes`) of the pane with identity `id`.
    pub fn index_of(&self, id: PaneId) -> Option<usize> {
        self.panes
            .iter()
            .position(|entry| entry.as_ref().is_some_and(|entry| entry.id() == id))
    }

    /// The grid cell holding the pane at `index`.
    pub fn handle(&self, index: usize) -> Option<pane_grid::Pane> {
        let id = self.entry(index)?.id();
        self.grid
            .iter()
            .find(|(_, cell)| cell.contains(id))
            .map(|(cell, _)| *cell)
    }

    /// The grid cell holding the focused pane.
    pub fn focused(&self) -> Option<pane_grid::Pane> {
        self.handle(self.active)
    }

    /// Focuses the pane with identity `id`, when the tab still holds it.
    fn focus_id(&mut self, id: PaneId) {
        if let Some(index) = self.index_of(id) {
            self.active = index;
        }
    }

    /// Leaves the slot of the pane with identity `id` empty.
    ///
    /// Dropping the last handle to a pane closes its pty child and stops its
    /// bridge thread; the slot itself stays, so no later pane takes its index.
    fn tombstone(&mut self, id: PaneId) {
        if let Some(index) = self.index_of(id) {
            self.panes[index] = None;
            // The pane is gone, so its recorded spawn metadata goes too; nothing
            // else reads it once no entry points at it.
            crate::pane::forget_meta(id);
        }
    }

    /// Appends a pane and focuses it; its index is the position it takes.
    fn push_entry(&mut self, entry: PaneEntry) -> usize {
        let index = self.panes.len();
        self.panes.push(Some(entry));
        self.active = index;
        index
    }

    /// Makes a pane the tab's only cell, for a tab left without a live one.
    fn repopulate(&mut self, id: PaneId, entry: PaneEntry) -> usize {
        self.grid = pane_grid::State::new(GridPane::single(id)).0;
        self.push_entry(entry)
    }

    /// Adds a pane beside the focused one, in its own cell, and focuses it.
    ///
    /// The new cell takes the `b` side of the split, which is the side the
    /// direction names: right of a horizontal split, below a vertical one.
    fn add_split(&mut self, dir: SplitDir, id: PaneId, entry: PaneEntry) -> usize {
        let Some(target) = self.focused() else {
            return self.repopulate(id, entry);
        };
        let split = self
            .grid
            .split(panes_grid::axis(dir), target, GridPane::single(id));
        if split.is_none() {
            return self.repopulate(id, entry);
        }
        self.push_entry(entry)
    }

    /// Adds a pane to the focused cell's stack and focuses it.
    fn add_stacked(&mut self, id: PaneId, pane: PaneEntry) -> usize {
        let Some(target) = self.focused() else {
            return self.repopulate(id, pane);
        };
        let Some(cell) = self.grid.get_mut(target) else {
            return self.repopulate(id, pane);
        };
        cell.panes.push(id);
        cell.active = cell.panes.len() - 1;
        self.push_entry(pane)
    }

    /// Drops the focused pane from its cell's stack, if it is in one.
    fn pop_focused_stack(&mut self) -> Option<(PaneId, Option<PaneId>)> {
        let target = self.focused()?;
        if self.grid.get(target)?.panes.len() < 2 {
            return None;
        }
        let gone = self.grid.get_mut(target)?.pop_active()?;
        let next = self.grid.get(target).and_then(GridPane::active_id);
        Some((gone, next))
    }

    /// Closes the focused pane; the last pane of a tab is never closed.
    ///
    /// The panes of a stack share one cell, so a stack loses one member and keeps
    /// its cell. Returns whether the grid changed.
    fn close_focused(&mut self) -> bool {
        if self.panes.iter().filter(|pane| pane.is_some()).count() <= 1 {
            return false;
        }
        if let Some((gone, next)) = self.pop_focused_stack() {
            self.tombstone(gone);
            if let Some(id) = next {
                self.focus_id(id);
            }
            return true;
        }
        let Some(target) = self.focused() else {
            return false;
        };
        let Some((cell, sibling)) = self.grid.close(target) else {
            return false;
        };
        for id in cell.panes {
            self.tombstone(id);
        }
        if let Some(id) = self.grid.get(sibling).and_then(GridPane::active_id) {
            self.focus_id(id);
        }
        true
    }

    /// Takes the focused pane out of the tab: its cell collapses, or — in a
    /// stack — keeps its other members. The vacated slot is tombstoned. `None`
    /// when the tab holds no live pane.
    ///
    /// A tab whose only cell is closed here keeps that cell without a pane, the
    /// same way the layout crate's tree keeps its empty sentinel: the tab stays
    /// until something is split into it.
    fn take_focused(&mut self) -> Option<(PaneId, PaneEntry)> {
        if let Some((gone, next)) = self.pop_focused_stack() {
            if let Some(id) = next {
                self.focus_id(id);
            }
            let index = self.index_of(gone)?;
            return Some((gone, self.panes[index].take()?));
        }
        let target = self.focused()?;
        let gone = self.grid.get(target)?.active_id()?;
        let index = self.index_of(gone)?;
        let pane = self.panes[index].take()?;
        if self.grid.len() > 1 {
            if let Some((_cell, sibling)) = self.grid.close(target) {
                if let Some(id) = self.grid.get(sibling).and_then(GridPane::active_id) {
                    self.focus_id(id);
                }
            }
        }
        Some((gone, pane))
    }

    /// Advances the focused cell's stack and focuses the newly shown pane; false
    /// when the focused pane is not in a stack.
    fn cycle_stack(&mut self) -> bool {
        let Some(target) = self.focused() else {
            return false;
        };
        let Some(cell) = self.grid.get_mut(target) else {
            return false;
        };
        if cell.panes.len() < 2 {
            return false;
        }
        cell.active = (cell.active + 1) % cell.panes.len();
        let next = cell.active_id();
        if let Some(id) = next {
            self.focus_id(id);
        }
        true
    }
}

pub struct AppState {
    pub tabs: Vec<Tab>,
    pub active_tab: usize,
    pub config: Config,
    pub theme: Theme,
    pub cell: CellDims,
    pub show_frames: bool,
    pub mode: ghostwave::keymap::Mode,
    /// Wave animation energy; never falls below 0.1.
    pub wave_energy: f32,
    /// Wave animation phase, in radians.
    pub wave_phase: f32,
    /// The toast message and the instant it was shown; at most one at a time.
    pub toast: Option<(String, std::time::Instant)>,
    /// Set when pty output arrived since the last tick; each tick with activity
    /// adds energy to the wave.
    pub pty_activity: bool,
    /// Set by the quit action; `update` closes the window.
    pub quit: bool,
    pub last_tab: Option<usize>,
    pub rename_buf: String,
    #[allow(dead_code)]
    pub rename_target: String,
    /// The command palette, while it is open. Keys and clicks go here, not to a pane.
    pub palette: Option<crate::palette::PaletteState>,
    /// The scrollback search overlay, while it is open.
    pub search: Option<crate::search_overlay::SearchState>,
    /// The named session this workspace is running inside.
    pub current_session: String,
    /// The named-session picker, while it is open. Keys and clicks go here.
    pub session_picker: Option<crate::sessions_overlay::SessionsOverlay>,
    /// Floating panes and the F12 scratchpad terminal.
    pub scratchpad: crate::scratchpad::ScratchpadState,
    last_tick: std::time::Instant,
    window: Size,
}

/// How often the front-end needs to be ticked: iced redraws on state change, so a
/// tick is only needed while something is animating.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TickRate {
    /// ~60 FPS: the wave is energized or a toast is on screen.
    Fast,
    /// 8 FPS: the wave idles.
    Slow,
    /// Nothing is animating; effectively no ticks (the period is an hour).
    Off,
}

impl AppState {
    pub fn new(config: Config, cell: CellDims, window: Size) -> Result<Self> {
        let theme = theme::by_name(&config.theme);
        let mut state = Self {
            tabs: Vec::new(),
            active_tab: 0,
            config,
            theme,
            cell,
            show_frames: true,
            mode: ghostwave::keymap::Mode::Normal,
            wave_energy: 0.1,
            wave_phase: 0.0,
            toast: None,
            pty_activity: false,
            quit: false,
            last_tab: None,
            rename_buf: String::new(),
            rename_target: String::new(),
            palette: None,
            search: None,
            current_session: ghostwave::session::last_session(),
            session_picker: None,
            scratchpad: crate::scratchpad::ScratchpadState::new(),
            last_tick: std::time::Instant::now(),
            window,
        };
        let (_, pane) = state.spawn_pane()?;
        state.tabs.push(Tab::new("Tab 1".to_string(), PaneEntry::Term(pane)));
        state.relayout();

        if let Ok(count) = std::env::var("GHOST_ICED_PANES").map(|v| v.parse::<usize>().unwrap_or(1))
        {
            for i in 1..count {
                let dir = if i % 2 == 1 {
                    SplitDir::Horizontal
                } else {
                    SplitDir::Vertical
                };
                if state.split(dir).is_err() {
                    eprintln!("[ghostwave] GHOST_ICED_PANES: stopped after {i} panes");
                    break;
                }
            }
        }

        state.refresh_titles();
        Ok(state)
    }

    /// Refreshes dynamic process and directory titles across all panes in all tabs.
    ///
    /// Terminals only: a content pane has no process and no working directory.
    pub fn refresh_titles(&mut self) {
        if self.config.dynamic_title != crate::ui_config::DynamicTitleMode::Off {
            for tab in &self.tabs {
                for entry in tab.panes.iter().flatten() {
                    if let Some(pane) = entry.term() {
                        pane.lock().unwrap().update_title(
                            self.config.dynamic_title,
                            self.config.pane_title_format,
                        );
                    }
                }
            }
        }
    }

    /// The directory a new pane opens in: the focused pane's live working
    /// directory, or `None` when there is none, which leaves the shell in the
    /// app's own directory.
    ///
    /// The pane is read with `try_lock`, so the spawn path never waits on a lock
    /// a caller may already hold.
    pub(crate) fn spawn_cwd(&self) -> Option<String> {
        let tab = self.tabs.get(self.active_tab)?;
        let pane = tab.pane(tab.active)?;
        let dir = pane.try_lock().ok()?.term.pwd();
        let path = std::path::Path::new(&dir);
        (path.is_absolute() && path.is_dir()).then_some(dir)
    }

    /// Forks a pty pane for this front-end, themed but not yet in any tab,
    /// starting in the focused pane's directory.
    pub(crate) fn spawn_pane(&self) -> Result<(PaneId, SharedPane)> {
        let cwd = self.spawn_cwd();
        let mut pane = Pane::spawn(
            SPAWN_COLS,
            SPAWN_ROWS,
            self.cell,
            self.config.scrollback,
            self.config.max_log_mb * 1024 * 1024,
            cwd.as_deref(),
        )?;
        pane.apply_theme(&self.theme);
        let id = pane.id;
        Ok((id, Arc::new(Mutex::new(pane))))
    }

    pub fn current(&self) -> &Tab {
        &self.tabs[self.active_tab]
    }

    /// A tab holding one fresh shell, for a session with nothing saved to
    /// restore.
    pub(crate) fn fresh_tab(&self) -> Result<Tab> {
        let (_, pane) = self.spawn_pane()?;
        Ok(Tab::new("Tab 1".to_string(), PaneEntry::Term(pane)))
    }

    /// Replaces the whole workspace with `tabs`, the first one focused — what
    /// loading a session does.
    ///
    /// The outgoing tabs are dropped, which closes their ptys: this front-end
    /// does not park a session the way the macroquad build does. Floating panes
    /// and the scratchpad go with their tab, so both are reset here rather than
    /// left holding panes that no longer exist.
    pub fn replace_tabs(&mut self, tabs: Vec<Tab>) {
        self.tabs = tabs;
        self.active_tab = 0;
        self.last_tab = None;
        self.scratchpad = crate::scratchpad::ScratchpadState::new();
        self.relayout();
    }

    pub fn set_window(&mut self, size: Size) {
        self.window = size;
        self.relayout();
    }

    /// Advances the wave animation by the real elapsed time since the last tick,
    /// clamped to 1-100 ms.
    pub fn tick(&mut self) {
        let now = std::time::Instant::now();
        let dt = now
            .saturating_duration_since(self.last_tick)
            .as_secs_f32()
            .clamp(0.001, 0.1);
        self.last_tick = now;

        if self.pty_activity {
            self.wave_energy = (self.wave_energy + 0.12).min(2.5);
            self.pty_activity = false;
        }
        self.wave_energy = (self.wave_energy * (-3.713_f32 * dt).exp()).max(0.1);

        let speed_mult = if self.wave_energy > 0.15 {
            1.0 + self.wave_energy * 1.5
        } else {
            0.35
        };
        self.wave_phase += 2.4 * dt * self.config.wave_speed * speed_mult;
        let wrap = std::f32::consts::TAU * 100.0;
        if self.wave_phase > wrap {
            self.wave_phase -= wrap;
        }

        if let Some((_, at)) = &self.toast {
            if at.elapsed().as_secs_f32() >= 2.5 {
                self.toast = None;
            }
        }

        self.tick_content(dt);
        self.refresh_titles();
    }

    /// `Fast` while a toast, an energized wave or a live visualizer is on
    /// screen, `Slow` for an idle wave, dynamic title polling or an AI reply in
    /// flight, otherwise `Off`.
    pub fn tick_rate(&self) -> TickRate {
        if self.toast.is_some() || (self.config.visual_wave && self.wave_energy > 0.15) {
            TickRate::Fast
        } else if self.has_visualizer() {
            TickRate::Fast
        } else if self.config.visual_wave {
            TickRate::Slow
        } else if self.config.dynamic_title != crate::ui_config::DynamicTitleMode::Off
            || self.ai_pending()
        {
            TickRate::Slow
        } else {
            TickRate::Off
        }
    }

    /// Shows `message`, replacing any toast already on screen.
    #[allow(dead_code)]
    pub fn toast(&mut self, message: impl Into<String>) {
        self.toast = Some((message.into(), std::time::Instant::now()));
    }

    /// Energizes the wave; called on every key press.
    pub fn bump_wave(&mut self) {
        if self.config.visual_wave {
            self.wave_energy = (self.wave_energy + 0.25).min(2.5);
        }
    }

    /// Sizes every terminal from its recorded bounds; a no-op when unchanged.
    ///
    /// A content pane has no VT grid and is not resized.
    pub fn relayout(&mut self) {
        let cell = self.cell;
        let pad = self.config.padding;
        for handle in self.current().term_handles() {
            let mut guard = handle.lock().unwrap();
            if let Some(bounds) = guard.last_bounds {
                let (cols, rows) =
                    panes_grid::bounds_to_cols_rows(bounds.width, bounds.height, cell, pad);
                guard.resize(cols, rows, cell);
            }
        }
    }

    /// Applies a split drag: the split keeps the ratio the grid reports, and the
    /// panes drawn at its bounds follow.
    pub fn grid_resize(&mut self, split: Split, ratio: f32) {
        self.tabs[self.active_tab].grid.resize(split, ratio);
        self.relayout();
    }

    /// Applies a pane drag from the pane grid: a pick focuses the pane, a drop
    /// moves it where it was released, a cancel is nothing to do.
    pub fn grid_drag(&mut self, event: pane_grid::DragEvent) {
        let tab_idx = self.active_tab;
        match event {
            pane_grid::DragEvent::Picked { pane } => {
                let picked = self.tabs[tab_idx]
                    .grid
                    .get(pane)
                    .and_then(GridPane::active_id);
                if let Some(id) = picked {
                    self.tabs[tab_idx].focus_id(id);
                }
            }
            pane_grid::DragEvent::Canceled { .. } => {}
            pane_grid::DragEvent::Dropped { pane, target } => {
                self.tabs[tab_idx].grid.drop(pane, target);
                self.relayout();
            }
        }
    }

    /// The focused pane's terminal; `None` when the focused pane is a content
    /// pane. Every terminal-only action reads this.
    pub fn active_pane(&self) -> Option<SharedPane> {
        let tab = self.current();
        tab.pane(tab.active).cloned()
    }

    /// The focused pane, whatever kind it is.
    pub fn focused_entry(&self) -> Option<&PaneEntry> {
        let tab = self.current();
        tab.entry(tab.active)
    }

    /// The focused pane, whatever kind it is.
    pub fn focused_entry_mut(&mut self) -> Option<&mut PaneEntry> {
        let tab = &mut self.tabs[self.active_tab];
        tab.entry_mut(tab.active)
    }

    /// The content pane with identity `id`, wherever it currently lives.
    pub fn content_by_id(&self, id: PaneId) -> Option<&ContentPane> {
        self.tabs
            .iter()
            .flat_map(|tab| tab.panes.iter())
            .flatten()
            .find(|entry| entry.id() == id)
            .and_then(PaneEntry::content)
    }

    /// The content pane with identity `id`, wherever it currently lives.
    pub fn content_by_id_mut(&mut self, id: PaneId) -> Option<&mut ContentPane> {
        self.tabs
            .iter_mut()
            .flat_map(|tab| tab.panes.iter_mut())
            .flatten()
            .find(|entry| entry.id() == id)
            .and_then(PaneEntry::content_mut)
    }

    /// Whether any live content pane is an AI chat with a request in flight,
    /// whose reply the tick has to drain.
    pub fn ai_pending(&self) -> bool {
        self.content_panes()
            .any(|content| content.as_ai().is_some_and(|ai| ai.busy))
    }

    /// Whether a visualizer pane is live, which animates and so needs frames.
    pub fn has_visualizer(&self) -> bool {
        self.content_panes()
            .any(|content| content.as_visualizer().is_some())
    }

    /// Every live content pane, in tab-then-index order.
    fn content_panes(&self) -> impl Iterator<Item = &ContentPane> {
        self.tabs
            .iter()
            .flat_map(|tab| tab.panes.iter())
            .flatten()
            .filter_map(PaneEntry::content)
    }

    /// Advances the content panes that wait on something other than input: the
    /// AI pane drains its worker's reply, the visualizer its own clock.
    pub fn tick_content(&mut self, dt: f32) {
        for tab in self.tabs.iter_mut() {
            for entry in tab.panes.iter_mut().flatten() {
                let Some(content) = entry.content_mut() else {
                    continue;
                };
                match content {
                    ContentPane::Ai { pane, .. } => pane.poll(),
                    ContentPane::Visualizer { pane, .. } => pane.tick(dt),
                    ContentPane::Preview { .. } => {}
                }
            }
        }
    }

    /// Focuses the pane at tab-local index `id`, terminal or content.
    pub fn focus(&mut self, id: usize) {
        let idx = self.active_tab;
        if self.tabs[idx].entry(id).is_some() {
            self.tabs[idx].active = id;
        }
    }

    /// Focuses the pane next to the focused one in `dir`, if the grid has one.
    pub fn focus_dir(&mut self, dir: Dir) {
        let tab = &mut self.tabs[self.active_tab];
        let Some(from) = tab.focused() else { return };
        let Some(next) = tab.grid.adjacent(from, direction(dir)) else {
            return;
        };
        if let Some(id) = tab.grid.get(next).and_then(GridPane::active_id) {
            tab.focus_id(id);
        }
    }

    /// Cycles the focus to the next pane in layout order.
    pub fn focus_next(&mut self) {
        let tab = &mut self.tabs[self.active_tab];
        let order = layout_order(&tab.grid);
        let Some(from) = tab.focused() else { return };
        let at = order.iter().position(|cell| *cell == from).unwrap_or(0);
        let next = order[(at + 1) % order.len()];
        if let Some(id) = tab.grid.get(next).and_then(GridPane::active_id) {
            tab.focus_id(id);
        }
    }

    /// Focuses the `i`-th pane in layout order; `i` is a position, not a pane
    /// index.
    pub fn focus_index(&mut self, i: usize) {
        let tab = &mut self.tabs[self.active_tab];
        let order = layout_order(&tab.grid);
        let target = order
            .get(i)
            .and_then(|cell| tab.grid.get(*cell))
            .and_then(GridPane::active_id);
        if let Some(id) = target {
            tab.focus_id(id);
        }
    }

    /// Splits the focused pane and focuses the new one, which lands on the side
    /// `dir` names.
    pub fn split(&mut self, dir: SplitDir) -> Result<usize> {
        let (id, pane) = self.spawn_pane()?;
        let tab = &mut self.tabs[self.active_tab];
        let index = tab.add_split(dir, id, PaneEntry::Term(pane));
        self.apply_terminal_colors();
        self.relayout();
        Ok(index)
    }

    /// Splits the focused pane with a *stacked* pane sharing its cell and focuses
    /// the new one.
    pub fn split_stacked(&mut self) -> Result<usize> {
        let (id, pane) = self.spawn_pane()?;
        let tab = &mut self.tabs[self.active_tab];
        let index = tab.add_stacked(id, PaneEntry::Term(pane));
        self.apply_terminal_colors();
        self.relayout();
        Ok(index)
    }

    /// Adds a content pane beside the focused one, in its own cell, and focuses
    /// it.
    ///
    /// A content pane has no pty and no VT, so nothing is sized and no terminal
    /// colours are applied; the grid carries it exactly as it carries a
    /// terminal, keyed by the id the content pane took from the same counter.
    pub fn add_content(&mut self, dir: SplitDir, content: ContentPane) -> usize {
        let id = content.id();
        let tab = &mut self.tabs[self.active_tab];
        tab.add_split(dir, id, PaneEntry::Content(Box::new(content)))
    }

    /// Closes the focused pane; the last pane in a tab is never closed.
    pub fn close_active(&mut self) {
        if self.tabs[self.active_tab].close_focused() {
            self.relayout();
        }
    }

    pub fn pane_mut(&self, tab: usize, pane: usize) -> Option<SharedPane> {
        self.tabs.get(tab)?.pane(pane).cloned()
    }

    /// Finds the pane with this identity, wherever it currently lives.
    ///
    /// Pty messages arrive tagged with a [`PaneId`] rather than a location, so a pane
    /// that has moved between tabs still receives its own output.
    pub fn pane_by_id(&self, id: PaneId) -> Option<SharedPane> {
        self.tabs.iter().find_map(|tab| tab.shared(id))
    }

    /// Opens a fresh tab and focuses it.
    pub fn new_tab(&mut self) {
        let (_, pane) = match self.spawn_pane() {
            Ok(pane) => pane,
            Err(e) => {
                eprintln!("ghostwave: failed to spawn shell: {e}");
                return;
            }
        };
        self.last_tab = Some(self.active_tab);
        let name = format!("Tab {}", self.tabs.len() + 1);
        self.tabs.push(Tab::new(name, PaneEntry::Term(pane)));
        self.active_tab = self.tabs.len() - 1;
        self.relayout();
    }

    /// Closes the active tab; the last tab is never removed, it is reset to a single
    /// fresh pane instead.
    pub fn close_tab(&mut self) {
        if self.tabs.len() > 1 {
            self.tabs.remove(self.active_tab);
            if self.active_tab >= self.tabs.len() {
                self.active_tab = self.tabs.len() - 1;
            }
            self.relayout();
            return;
        }
        let (id, pane) = match self.spawn_pane() {
            Ok(pane) => pane,
            Err(e) => {
                eprintln!("ghostwave: failed to spawn shell: {e}");
                return;
            }
        };
        let tab = &mut self.tabs[self.active_tab];
        tab.panes.clear();
        tab.panes.push(Some(PaneEntry::Term(pane)));
        tab.grid = pane_grid::State::new(GridPane::single(id)).0;
        tab.active = 0;
        self.relayout();
    }

    /// Switches to tab `idx` and remembers the tab it came from.
    pub fn goto_tab(&mut self, idx: usize) {
        if idx < self.tabs.len() {
            self.last_tab = Some(self.active_tab);
            self.active_tab = idx;
            self.relayout();
        }
    }

    /// Switches back to the tab that was active before the last switch.
    pub fn toggle_last_tab(&mut self) {
        if let Some(last) = self.last_tab {
            if last < self.tabs.len() {
                self.goto_tab(last);
            }
        }
    }

    /// Swaps the active tab with its neighbour in `dir`; each tab keeps its own
    /// grid, so nothing is re-laid out.
    pub fn move_tab(&mut self, dir: Dir) {
        let n = self.tabs.len();
        let next = match dir {
            Dir::Left => (self.active_tab + n - 1) % n,
            _ => (self.active_tab + 1) % n,
        };
        self.tabs.swap(self.active_tab, next);
        self.active_tab = next;
    }

    /// Toggles the frame strokes; the pane layer's chrome is part of the bounds a
    /// pane is drawn with, so the grids follow the new geometry.
    pub fn toggle_frames(&mut self) {
        self.show_frames = !self.show_frames;
        self.relayout();
    }

    /// Flips the status bar's SYNC chip and returns its new value.
    pub fn toggle_sync(&mut self) -> bool {
        let tab = &mut self.tabs[self.active_tab];
        tab.sync_panes = !tab.sync_panes;
        tab.sync_panes
    }

    /// Maximizes the focused pane to the whole content area, or restores the grid.
    pub fn toggle_zoom(&mut self) {
        let tab = &mut self.tabs[self.active_tab];
        if tab.grid.maximized().is_some() {
            tab.grid.restore();
        } else if let Some(target) = tab.focused() {
            tab.grid.maximize(target);
        }
        self.relayout();
    }

    /// Moves the focused pane's split by `delta`, clamped, so the panes on either
    /// side of it grow and shrink.
    ///
    /// The split moved is the nearest ancestor of the focused pane on the axis
    /// `dir` names; `Dir::Left`/`Dir::Up` grow its `a` side (the left/top pane),
    /// `Dir::Right`/`Dir::Down` its `b` side.
    pub fn resize(&mut self, dir: Dir, delta: f32) {
        let tab = &mut self.tabs[self.active_tab];
        let Some(from) = tab.focused() else { return };
        let Some((split, ratio)) = split_toward(tab.grid.layout(), from, dir) else {
            return;
        };
        let ratio = if dir.grows_a() {
            ratio + delta
        } else {
            ratio - delta
        };
        tab.grid.resize(split, ratio.clamp(RATIO_MIN, RATIO_MAX));
        self.relayout();
    }

    /// Swaps the focused pane with its neighbour in `dir`; the focused pane keeps
    /// its identity and moves into the neighbour's cell.
    pub fn move_pane(&mut self, dir: Dir) {
        let tab = &mut self.tabs[self.active_tab];
        let Some(from) = tab.focused() else { return };
        let Some(to) = tab.grid.adjacent(from, direction(dir)) else {
            return;
        };
        tab.grid.swap(from, to);
        self.relayout();
    }

    /// Swaps the focused pane with the next cell in layout order, wrapping to the
    /// first.
    pub fn move_pane_next(&mut self) {
        let tab = &mut self.tabs[self.active_tab];
        let order = layout_order(&tab.grid);
        let Some(from) = tab.focused() else { return };
        let at = order.iter().position(|cell| *cell == from).unwrap_or(0);
        tab.grid.swap(from, order[(at + 1) % order.len()]);
        self.relayout();
    }

    /// Swaps the focused pane with the previous cell in layout order, wrapping to
    /// the last.
    pub fn move_pane_prev(&mut self) {
        let tab = &mut self.tabs[self.active_tab];
        let order = layout_order(&tab.grid);
        let Some(from) = tab.focused() else { return };
        let at = order.iter().position(|cell| *cell == from).unwrap_or(0);
        tab.grid.swap(from, order[(at + order.len() - 1) % order.len()]);
        self.relayout();
    }

    /// Rotates the stack the focused pane sits in and focuses the newly exposed
    /// pane; stack panes share one cell.
    pub fn cycle_stack(&mut self) {
        if self.tabs[self.active_tab].cycle_stack() {
            self.relayout();
        } else {
            self.toast("not in a stack (create with pane-mode s)");
        }
    }

    /// Moves the focused pane into a brand-new tab.
    ///
    /// The pane keeps its own `Arc` — and with it its pty, its scrollback log and
    /// its bridge receiver, tagged with the pane's own [`PaneId`] — so moving it
    /// between tabs cannot misroute its output. The vacated slot is left as a
    /// tombstone: indices inside a tab are never reused.
    pub fn break_pane(&mut self) {
        let old_tab = self.active_tab;
        let taken = self.tabs[old_tab].take_focused();
        let Some((_, entry)) = taken else { return };

        self.last_tab = Some(old_tab);
        let name = format!("Tab {}", self.tabs.len() + 1);
        self.tabs.push(Tab::new(name, entry));
        self.active_tab = self.tabs.len() - 1;
        self.relayout();
    }

    /// Switches the theme by name, re-pointing every live pane's terminal at the new
    /// palette and storing the choice in the config.
    pub fn apply_theme(&mut self, name: &str) {
        let theme = theme::by_name(name);
        for tab in self.tabs.iter_mut() {
            for handle in tab.term_handles() {
                handle.lock().unwrap().apply_theme(&theme);
            }
        }
        self.theme = theme;
        self.config.theme = name.to_string();
        self.toast(format!("theme: {name}"));
    }

    /// Re-applies the current theme colors across all live terminals.
    pub fn apply_terminal_colors(&self) {
        for tab in &self.tabs {
            for handle in tab.term_handles() {
                handle.lock().unwrap().apply_theme(&self.theme);
            }
        }
    }
}

/// iced's direction for a layout direction.
fn direction(dir: Dir) -> pane_grid::Direction {
    match dir {
        Dir::Left => pane_grid::Direction::Left,
        Dir::Right => pane_grid::Direction::Right,
        Dir::Up => pane_grid::Direction::Up,
        Dir::Down => pane_grid::Direction::Down,
    }
}

/// The cells of `state` in layout order: left to right, top to bottom.
fn layout_order(state: &pane_grid::State<GridPane>) -> Vec<pane_grid::Pane> {
    fn walk(node: &pane_grid::Node, out: &mut Vec<pane_grid::Pane>) {
        match node {
            pane_grid::Node::Split { a, b, .. } => {
                walk(a, out);
                walk(b, out);
            }
            pane_grid::Node::Pane(cell) => out.push(*cell),
        }
    }
    let mut out = Vec::new();
    walk(state.layout(), &mut out);
    out
}

/// The split `dir` moves, with its current ratio: the nearest ancestor of `pane`
/// on `dir`'s axis, which is the boundary between the pane and its neighbour on
/// that side.
fn split_toward(node: &pane_grid::Node, pane: pane_grid::Pane, dir: Dir) -> Option<(Split, f32)> {
    let axis: Axis = panes_grid::axis(dir.axis());
    let mut node = node;
    loop {
        let pane_grid::Node::Split {
            id,
            axis: split_axis,
            ratio,
            a,
            b,
        } = node
        else {
            return None;
        };
        if *split_axis == axis {
            return Some((*id, *ratio));
        }
        node = if holds(a, pane) {
            a
        } else if holds(b, pane) {
            b
        } else {
            return None;
        };
    }
}

/// Whether `node` is the given pane, or holds it somewhere below.
fn holds(node: &pane_grid::Node, pane: pane_grid::Pane) -> bool {
    match node {
        pane_grid::Node::Pane(cell) => *cell == pane,
        pane_grid::Node::Split { a, b, .. } => holds(a, pane) || holds(b, pane),
    }
}
