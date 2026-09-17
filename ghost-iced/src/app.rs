//! iced-side session state: the tab's split tree, its panes, and pane geometry.
//!
//! Pane ids index `Tab::panes` and are never reused: closing a pane leaves a
//! tombstone, so pty output cannot be routed into the pane that took its index.

use std::sync::{Arc, Mutex};

use ghostwave::app::{STATUS_BAR_H, TAB_BAR_H};
use ghostwave::config::Config;
use ghostwave::layout::{self, Dir, Node, Rect, SplitDir};
use ghostwave::term::CellDims;
use ghostwave::theme::{self, Theme};
use iced::widget::pane_grid::{self, Split};
use iced::{Point, Rectangle, Size};

use crate::pane::{Pane, PaneId, SharedPane};
use crate::panes_grid::{self, DropTarget, GridCache};

/// Inset applied to every pane rect when frames are shown, leaving a 4px gutter
/// between neighbours for the frame strokes.
const FRAME_INSET: f32 = 2.0;

/// Grid a pane is born with; `relayout` corrects it before the first paint.
const SPAWN_COLS: u16 = 80;
const SPAWN_ROWS: u16 = 24;

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

pub struct Tab {
    pub name: String,
    pub root: Node,
    /// Index = pane id. `None` = closed (tombstone, id never reused).
    pub panes: Vec<Option<SharedPane>>,
    pub active: usize,
    /// Panes pinned to the full content rect; holds at most the active pane.
    pub zoomed: std::collections::HashSet<usize>,
    /// Status-bar SYNC chip: synchronised input across the tab's panes.
    pub sync_panes: bool,
}

impl Tab {
    fn new(name: String, pane: SharedPane) -> Self {
        Self {
            name,
            root: Node::Leaf { pane: 0 },
            panes: vec![Some(pane)],
            active: 0,
            zoomed: std::collections::HashSet::new(),
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
            let title = pane.lock().unwrap().term.title();
            if !title.is_empty() {
                return title;
            }
        }
        generated
    }

    pub fn pane(&self, id: usize) -> Option<&SharedPane> {
        self.panes.get(id).and_then(|p| p.as_ref())
    }

    /// Pane rects in content coordinates, inset when `frames` is set.
    pub fn pane_rects(&self, content: Rect, frames: bool) -> Vec<(usize, Rect)> {
        let mut rects = layout::layout(&self.root, content);
        if frames {
            for (_, r) in rects.iter_mut() {
                *r = r.inset(FRAME_INSET);
            }
        }
        rects
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
    /// `GHOST_ICED_PANEGRID`: the pane layer is iced's `pane_grid` rather than the
    /// hand-rolled canvas.
    pub pane_grid: bool,
    /// The revision of the active tab's split tree, bumped by every
    /// [`AppState::relayout`]. The pane-grid layer re-derives its state whenever it
    /// lags this counter, and a re-derivation is safe mid-drag because pane and
    /// split ids are a function of the tree's shape, not of its history.
    pub tree_revision: u64,
    /// The pane-grid state derived from the active tab's tree; kept only in the
    /// pane-grid layer.
    pub grid: Option<GridCache>,
    last_tick: std::time::Instant,
    window: Size,
    /// Cached rects for the active tab, in canvas coordinates, shared by the
    /// painter and mouse hit-testing.
    pub rects: Vec<(usize, Rectangle)>,
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
        let mut pane0 = Pane::spawn(
            SPAWN_COLS,
            SPAWN_ROWS,
            cell,
            config.scrollback,
            config.max_log_mb * 1024 * 1024,
        )?;
        pane0.apply_theme(&theme);

        let mut state = Self {
            tabs: vec![Tab::new("Tab 1".to_string(), Arc::new(Mutex::new(pane0)))],
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
            pane_grid: panes_grid::enabled(),
            tree_revision: 0,
            grid: None,
            last_tick: std::time::Instant::now(),
            window,
            rects: Vec::new(),
        };
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
                    eprintln!("[ghost-iced] GHOST_ICED_PANES: stopped after {i} panes");
                    break;
                }
            }
        }

        Ok(state)
    }

    pub fn current(&self) -> &Tab {
        &self.tabs[self.active_tab]
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
        // Decay, floored at 0.1: the wave never stops while visual_wave is on.
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
            // Toast lifetime: 2.5 s.
            if at.elapsed().as_secs_f32() >= 2.5 {
                self.toast = None;
            }
        }
    }

    /// `Fast` while a toast or an energized wave is on screen, `Slow` for an idle
    /// wave, otherwise `Off`.
    pub fn tick_rate(&self) -> TickRate {
        if self.toast.is_some() || (self.config.visual_wave && self.wave_energy > 0.15) {
            TickRate::Fast
        } else if self.config.visual_wave {
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

    /// The content area: the window minus the tab-bar and status-bar bands.
    pub fn content(&self) -> Rect {
        Rect {
            x: 0.0,
            y: TAB_BAR_H,
            w: self.window.width.max(1.0),
            h: (self.window.height - TAB_BAR_H - STATUS_BAR_H).max(1.0),
        }
    }

    /// Recomputes every pane's rect and grid; must be called after any window resize
    /// or tree change.
    ///
    /// Also the tree's revision point: the pane-grid layer derives its state from the
    /// tree as soon as it sees a revision it has not derived yet.
    pub fn relayout(&mut self) {
        let content = self.content();
        let frames = self.show_frames;
        let cell = self.cell;
        let pad = self.config.padding;
        self.tree_revision = self.tree_revision.wrapping_add(1);

        let mut rects = if self.pane_grid {
            // The grid's own layout decides where panes are, so the ptys are sized
            // from it rather than from the tree's rects.
            self.sync_grid();
            match &self.grid {
                Some(cache) => panes_grid::pane_rects(cache, content, frames),
                None => Vec::new(),
            }
        } else {
            self.tabs[self.active_tab].pane_rects(content, frames)
        };

        // Zoom: the active pane takes the whole content rect; the others are not laid out.
        let tab = &self.tabs[self.active_tab];
        let active = tab.active;
        if tab.zoomed.contains(&active) {
            rects.retain(|(id, _)| *id == active);
            for (_, r) in rects.iter_mut() {
                *r = content;
            }
        }
        self.rects = rects
            .iter()
            .map(|(id, r)| {
                (
                    *id,
                    Rectangle::new(Point::new(r.x, r.y), Size::new(r.w.max(1.0), r.h.max(1.0))),
                )
            })
            .collect();

        for (id, r) in &rects {
            if let Some(pane) = tab.pane(*id) {
                let cols = ((r.w - 2.0 * pad) / cell.w as f32).max(1.0) as u16;
                let rows = ((r.h - 2.0 * pad) / cell.h as f32).max(1.0) as u16;
                pane.lock().unwrap().resize(cols, rows, cell);
            }
        }
    }

    /// Re-derives the pane-grid layer's state when the tree has moved on since the
    /// last derivation; a no-op in the canvas layer.
    pub fn sync_grid(&mut self) {
        if !self.pane_grid {
            return;
        }
        let (tab, revision) = (self.active_tab, self.tree_revision);
        let fresh = self
            .grid
            .as_ref()
            .is_some_and(|cache| cache.is_fresh(tab, revision));
        if !fresh {
            self.grid = Some(GridCache::derive(tab, revision, &self.tabs[tab].root));
        }
    }

    /// Writes a split drag back into the tree, and moves the same split in the
    /// derived state so what is drawn follows the drag.
    pub fn grid_resize(&mut self, split: Split, ratio: f32) {
        let content = self.content();
        let bounds = Size::new(content.w.max(1.0), content.h.max(1.0));
        let spacing = panes_grid::spacing(self.show_frames);
        let (tab, revision) = (self.active_tab, self.tree_revision);

        let mut write_back = None;
        if let Some(cache) = self.grid.as_mut() {
            if !cache.is_fresh(tab, revision) {
                return;
            }
            let Some(share) = panes_grid::tree_ratio(&cache.state, split, ratio, spacing, bounds)
            else {
                return;
            };
            cache.state.resize(split, ratio);
            write_back = cache.splits.get(&split).cloned().map(|path| (path, share));
        }
        if let Some((path, share)) = write_back {
            set_split_ratio(&mut self.tabs[tab].root, &path, share);
        }
        self.relayout();
    }

    /// Applies a pane drag from the pane grid: a pick focuses the pane, a drop moves
    /// it, a cancel is nothing to do.
    pub fn grid_drag(&mut self, event: pane_grid::DragEvent) {
        if std::env::var_os("GHOST_ICED_DEBUG").is_some() {
            eprintln!("[msg] GridDrag {event:?} active={}", self.current().active);
        }
        match event {
            pane_grid::DragEvent::Picked { pane } => {
                let picked = self
                    .grid
                    .as_ref()
                    .and_then(|cache| panes_grid::pane_of(&cache.state, pane));
                if let Some(pane) = picked {
                    self.focus(pane);
                }
            }
            pane_grid::DragEvent::Canceled { .. } => {}
            pane_grid::DragEvent::Dropped { pane, target } => {
                let moved = self
                    .grid
                    .as_ref()
                    .and_then(|cache| panes_grid::pane_of(&cache.state, pane));
                let drop = match (&self.grid, target) {
                    (Some(cache), target) => panes_grid::drop_target(target, &cache.state),
                    (None, _) => None,
                };
                if let (Some(moved), Some(drop)) = (moved, drop) {
                    self.move_pane_to(moved, drop);
                }
            }
        }
    }

    /// Moves `from` to `target` in the active tab's tree, keeping the pane's
    /// identity, and focuses it where it landed.
    ///
    /// The tree's shape changes, which is what makes the pane-grid layer re-derive
    /// its state; the pane's `Arc` — and with it its pty, scrollback and label — is
    /// untouched, so output cannot be misrouted.
    pub fn move_pane_to(&mut self, from: usize, target: DropTarget) {
        let tab_idx = self.active_tab;
        if self.tabs[tab_idx].panes.iter().filter(|p| p.is_some()).count() < 2 {
            return;
        }
        let tab = &mut self.tabs[tab_idx];
        let Some(path) = layout::path_to_pane(&tab.root, from) else {
            return;
        };
        match target {
            DropTarget::OnPane { pane, dir: None } => {
                if pane == from {
                    return;
                }
                let Some(other) = layout::path_to_pane(&tab.root, pane) else {
                    return;
                };
                layout::swap_panes(&mut tab.root, &path, &other);
            }
            DropTarget::Grid(dir) => {
                // Leave the cell first: the tree collapses, so the pane can take a
                // side of what is left.
                layout::close_at(&mut tab.root, &path);
                if matches!(tab.root, Node::Leaf { pane: usize::MAX }) {
                    tab.root = Node::Leaf { pane: from };
                } else {
                    let rest = std::mem::replace(&mut tab.root, Node::Leaf { pane: from });
                    tab.root = split_around(rest, from, dir);
                }
            }
            DropTarget::OnPane {
                pane,
                dir: Some(dir),
            } => {
                layout::close_at(&mut tab.root, &path);
                let Some(other) = layout::path_to_pane(&tab.root, pane) else {
                    return;
                };
                split_with_pane(&mut tab.root, &other, from, dir);
            }
        }
        tab.active = from;
        self.relayout();
    }

    pub fn active_pane(&self) -> Option<SharedPane> {
        let tab = self.current();
        tab.pane(tab.active).cloned()
    }

    pub fn focus(&mut self, id: usize) {
        let idx = self.active_tab;
        if self.tabs[idx].pane(id).is_some() {
            self.tabs[idx].active = id;
        }
    }

    pub fn focus_dir(&mut self, dir: Dir) {
        let tab = self.current();
        let from = tab.active;
        let leaves: Vec<(usize, Rect)> = tab
            .pane_rects(self.content(), self.show_frames)
            .into_iter()
            .collect();
        if let Some(next) = layout::focus_in_direction(&leaves, from, dir) {
            self.focus(next);
        }
    }

    /// Splits the focused pane and focuses the new one.
    pub fn split(&mut self, dir: SplitDir) -> Result<usize> {
        let tab_idx = self.active_tab;
        let new_id = self.tabs[tab_idx].panes.len();
        let mut pane = Pane::spawn(
            SPAWN_COLS,
            SPAWN_ROWS,
            self.cell,
            self.config.scrollback,
            self.config.max_log_mb * 1024 * 1024,
        )?;
        pane.apply_theme(&self.theme);

        let tab = &mut self.tabs[tab_idx];
        let path = layout::path_to_pane(&tab.root, tab.active).unwrap_or_default();
        if matches!(tab.root, Node::Leaf { pane: usize::MAX }) {
            // Empty tree: the new pane becomes the root.
            tab.root = Node::Leaf { pane: new_id };
        } else {
            layout::split_at(&mut tab.root, &path, dir, new_id);
        }
        tab.panes.push(Some(Arc::new(Mutex::new(pane))));
        tab.active = new_id;

        self.relayout();
        Ok(new_id)
    }

    /// Splits the focused pane with a *stacked* pane sharing its cell and focuses the
    /// new one.
    pub fn split_stacked(&mut self) -> Result<usize> {
        let tab_idx = self.active_tab;
        let new_id = self.tabs[tab_idx].panes.len();
        let mut pane = Pane::spawn(
            SPAWN_COLS,
            SPAWN_ROWS,
            self.cell,
            self.config.scrollback,
            self.config.max_log_mb * 1024 * 1024,
        )?;
        pane.apply_theme(&self.theme);

        let tab = &mut self.tabs[tab_idx];
        let path = layout::path_to_pane(&tab.root, tab.active).unwrap_or_default();
        if matches!(tab.root, Node::Leaf { pane: usize::MAX }) {
            // Empty tree: a stack of one is just a leaf.
            tab.root = Node::Leaf { pane: new_id };
        } else {
            layout::stack_at(&mut tab.root, &path, new_id);
        }
        tab.panes.push(Some(Arc::new(Mutex::new(pane))));
        tab.active = new_id;

        self.relayout();
        Ok(new_id)
    }

    /// Closes the focused pane; the last pane in a tab is never closed.
    pub fn close_active(&mut self) {
        let tab_idx = self.active_tab;
        if self.tabs[tab_idx].panes.iter().filter(|p| p.is_some()).count() <= 1 {
            return;
        }
        {
            let tab = &mut self.tabs[tab_idx];
            if let Some(path) = layout::path_to_pane(&tab.root, tab.active) {
                layout::close_at(&mut tab.root, &path);
            }
        }
        // Tombstone it: dropping the last Arc closes the pty child and its bridge thread.
        let active = self.tabs[tab_idx].active;
        self.tabs[tab_idx].panes[active] = None;

        let tab = &mut self.tabs[tab_idx];
        if let Some((_, next)) = tab.root.leaves().into_iter().next() {
            tab.active = next;
        }
        self.relayout();
    }

    pub fn pane_mut(&self, tab: usize, pane: usize) -> Option<SharedPane> {
        self.tabs.get(tab)?.pane(pane).cloned()
    }

    /// Finds the pane with this identity, wherever it currently lives.
    ///
    /// Pty messages arrive tagged with a [`PaneId`] rather than a location, so a pane
    /// that has moved between tabs still receives its own output.
    pub fn pane_by_id(&self, id: PaneId) -> Option<SharedPane> {
        self.tabs
            .iter()
            .flat_map(|tab| tab.panes.iter().flatten())
            .find(|pane| pane.lock().unwrap().id == id)
            .cloned()
    }

    /// Opens a fresh tab and focuses it; pane ids are per-tab, so its first pane is
    /// id 0.
    pub fn new_tab(&mut self) {
        let mut pane = match Pane::spawn(
            SPAWN_COLS,
            SPAWN_ROWS,
            self.cell,
            self.config.scrollback,
            self.config.max_log_mb * 1024 * 1024,
        ) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("ghostwave: failed to spawn shell: {e}");
                return;
            }
        };
        pane.apply_theme(&self.theme);

        self.last_tab = Some(self.active_tab);
        let name = format!("Tab {}", self.tabs.len() + 1);
        self.tabs.push(Tab::new(name, Arc::new(Mutex::new(pane))));
        self.active_tab = self.tabs.len() - 1;
        self.relayout();
    }

    /// Closes the active tab; the last tab is never removed, it is reset to a single
    /// fresh pane instead.
    pub fn close_tab(&mut self) {
        if self.tabs.len() <= 1 {
            let mut pane = match Pane::spawn(
                SPAWN_COLS,
                SPAWN_ROWS,
                self.cell,
                self.config.scrollback,
                self.config.max_log_mb * 1024 * 1024,
            ) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("ghostwave: failed to spawn shell: {e}");
                    return;
                }
            };
            pane.apply_theme(&self.theme);

            // Dropping the last Arc closes each pty child.
            let tab = &mut self.tabs[self.active_tab];
            tab.panes.clear();
            tab.panes.push(Some(Arc::new(Mutex::new(pane))));
            tab.root = Node::Leaf { pane: 0 };
            tab.active = 0;
            tab.zoomed.clear();
            self.relayout();
            return;
        }
        // Surviving tabs shift down; pane output is tagged by identity, so it still routes.
        self.tabs.remove(self.active_tab);
        if self.active_tab >= self.tabs.len() {
            self.active_tab = self.tabs.len() - 1;
        }
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

    /// Swaps the active tab with its neighbour in `dir`; no relayout, since the tab
    /// keeps its own rects.
    pub fn move_tab(&mut self, dir: Dir) {
        let n = self.tabs.len();
        let next = match dir {
            Dir::Left => (self.active_tab + n - 1) % n,
            _ => (self.active_tab + 1) % n,
        };
        self.tabs.swap(self.active_tab, next);
        self.active_tab = next;
    }

    /// Toggles the frame strokes; every pane rect is inset when frames are shown, so
    /// the grids follow the new geometry.
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

    /// Pins the active pane to the full content rect, or unpins it.
    ///
    /// The pane-grid layer has no such rect — its geometry is the grid's — so there it
    /// says so instead of sizing the ptys for a rect that will not be drawn.
    pub fn toggle_zoom(&mut self) {
        if self.pane_grid {
            self.toast("zoom: not available in the pane grid layer");
            return;
        }
        let tab = &mut self.tabs[self.active_tab];
        if !tab.zoomed.insert(tab.active) {
            tab.zoomed.remove(&tab.active);
        }
        self.relayout();
    }

    /// Cycles focus to the next pane in DFS order.
    pub fn focus_next(&mut self) {
        let tab = &mut self.tabs[self.active_tab];
        if let Some(next) = layout::next_leaf(&tab.root, tab.active) {
            tab.active = next;
        }
    }

    /// Focuses the `i`-th leaf in DFS order; `i` is a leaf index, not a pane id.
    pub fn focus_index(&mut self, i: usize) {
        let tab = &mut self.tabs[self.active_tab];
        if let Some((_, pane)) = tab.root.leaves().get(i) {
            tab.active = *pane;
        }
    }

    /// Grows or shrinks the active pane by moving the nearest ancestor split on
    /// `dir`'s axis.
    pub fn resize(&mut self, dir: Dir, delta: f32) {
        let path = {
            let tab = &self.tabs[self.active_tab];
            match layout::path_to_pane(&tab.root, tab.active) {
                Some(p) => p,
                None => return,
            }
        };
        // Ratios decide rects, so the panes are re-set before the next paint.
        layout::resize_at(&mut self.tabs[self.active_tab].root, &path, dir, delta);
        self.relayout();
    }

    /// Swaps the active pane with its neighbour in `dir`; the panes trade places in the
    /// tree and the focus follows the moved pane.
    pub fn move_pane(&mut self, dir: Dir) {
        let (from, rects) = {
            let tab = &self.tabs[self.active_tab];
            (tab.active, tab.pane_rects(self.content(), self.show_frames))
        };
        let Some(target) = layout::focus_in_direction(&rects, from, dir) else {
            return;
        };
        let tab = &mut self.tabs[self.active_tab];
        let (pa, pb) = match (
            layout::path_to_pane(&tab.root, from),
            layout::path_to_pane(&tab.root, target),
        ) {
            (Some(a), Some(b)) => (a, b),
            _ => return,
        };
        layout::swap_panes(&mut tab.root, &pa, &pb);
        tab.active = target;
        self.relayout();
    }

    /// Swaps the active pane with the next leaf in DFS order.
    pub fn move_pane_next(&mut self) {
        let tab = &mut self.tabs[self.active_tab];
        let from = tab.active;
        let next = match layout::leaf_after(&tab.root, from) {
            Some(n) => n,
            None => return,
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
        self.relayout();
    }

    /// Swaps the active pane with the previous leaf in DFS order, wrapping to the last
    /// leaf.
    pub fn move_pane_prev(&mut self) {
        let tab = &mut self.tabs[self.active_tab];
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
        self.relayout();
    }

    /// Rotates the stack the active pane sits in and focuses the newly exposed pane;
    /// stack panes share one cell.
    pub fn cycle_stack(&mut self) {
        let tab_idx = self.active_tab;
        let cycled = {
            let tab = &mut self.tabs[tab_idx];
            let path = layout::path_to_pane(&tab.root, tab.active).unwrap_or_default();
            if layout::cycle_stack(&mut tab.root, &path) {
                // The stack is the same but its active pane is not: read the id back.
                tab.active = tab.root.at(&path).pane_id();
                true
            } else {
                false
            }
        };
        if cycled {
            self.relayout();
        } else {
            self.toast("not in a stack (create with pane-mode s)");
        }
    }

    /// Moves the focused pane into a brand-new tab.
    ///
    /// The pane keeps its own `Arc` — and with it its pty, its scrollback log and its
    /// bridge receiver, tagged with the pane's own [`PaneId`] — so moving it between
    /// tabs cannot misroute its output. The vacated slot is left as a tombstone: ids
    /// inside a tab are never reused.
    pub fn break_pane(&mut self) {
        let old_tab = self.active_tab;
        let pane = {
            let tab = &mut self.tabs[old_tab];
            let id = tab.active;
            let path = match layout::path_to_pane(&tab.root, id) {
                Some(p) => p,
                None => return,
            };
            layout::close_at(&mut tab.root, &path);
            // Take the handle out, leave the tombstone: nothing may shift into this id.
            tab.panes[id].take()
        };
        let Some(pane) = pane else { return };

        self.last_tab = Some(old_tab);
        let name = format!("Tab {}", self.tabs.len() + 1);
        self.tabs.push(Tab::new(name, pane));
        self.active_tab = self.tabs.len() - 1;
        self.relayout();
    }

    /// Switches the theme by name, re-pointing every live pane's terminal at the new
    /// palette and storing the choice in the config.
    pub fn apply_theme(&mut self, name: &str) {
        let theme = theme::by_name(name);
        for tab in self.tabs.iter_mut() {
            for pane in tab.panes.iter_mut().flatten() {
                pane.lock().unwrap().apply_theme(&theme);
            }
        }
        self.theme = theme;
        self.config.theme = name.to_string();
        self.toast(format!("theme: {name}"));
    }
}

/// Sets the ratio of the split at `path`, walking the tree rather than indexing it:
/// a stale pane-grid path then changes nothing instead of panicking.
fn set_split_ratio(root: &mut Node, path: &[u8], ratio: f32) {
    let mut node = root;
    for &step in path {
        node = match node {
            Node::Split { a, b, .. } => {
                if step == 0 {
                    a
                } else {
                    b
                }
            }
            _ => return,
        };
    }
    if let Node::Split { ratio: r, .. } = node {
        *r = ratio;
    }
}

/// Splits the node at `path` between itself and a new leaf holding `pane`, on the
/// side `dir` names: the node keeps its cell and the pane lands beside it.
fn split_with_pane(root: &mut Node, path: &[u8], pane: usize, dir: Dir) {
    let mut node = root;
    for &step in path {
        node = match node {
            Node::Split { a, b, .. } => {
                if step == 0 {
                    a
                } else {
                    b
                }
            }
            _ => return,
        };
    }
    let rest = std::mem::replace(node, Node::Leaf { pane });
    *node = split_around(rest, pane, dir);
}

/// A split of `rest` with a fresh leaf for `pane`; the direction places the pane on
/// the side it names ([`Dir::grows_a`]: `Left`/`Up` are the `a` side).
fn split_around(rest: Node, pane: usize, dir: Dir) -> Node {
    let leaf = Box::new(Node::Leaf { pane });
    let rest = Box::new(rest);
    let (a, b) = if dir.grows_a() {
        (leaf, rest)
    } else {
        (rest, leaf)
    };
    Node::Split {
        dir: dir.axis(),
        ratio: 0.5,
        a,
        b,
    }
}
