//! iced-side session state: the tab's split tree, its panes, and pane geometry.
//!
//! Geometry is not re-derived here. `ghostwave::layout` owns the binary split
//! tree and every operation on it (`split_at`, `close_at`, `focus_in_direction`,
//! `stack_at`), exactly as the macroquad front-end uses them, and `Tab::pane_rects`
//! reproduces `app.rs::pane_rects` including the 2px per-pane inset that makes
//! room for the frame strokes. What lives here is only what iced needs: lockable
//! pane handles, per-pane pty bridges, and the cached rects shared by the painter
//! and mouse hit-testing.
//!
//! Pane ids are indices into `Tab::panes` and are **stable**: closing a pane
//! leaves a tombstone instead of compacting the vec (unlike `App::remove_dangling_panes`,
//! which renumbers). Renumbering would move a pane under the message tag its pty
//! bridge already stamped on the stream, so messages would land on the wrong pane.

use std::sync::{Arc, Mutex};

use ghostwave::app::{STATUS_BAR_H, TAB_BAR_H};
use ghostwave::config::Config;
use ghostwave::layout::{self, Dir, Node, Rect, SplitDir};
use ghostwave::term::CellDims;
use ghostwave::theme::{self, Theme};
use iced::{Point, Rectangle, Size};

use crate::pane::{Pane, SharedPane};
use crate::pty_bridge::PaneTag;

/// Inset applied to every pane rect when frames are shown (`app.rs::pane_rects`).
/// Leaves a 4px gutter between neighbours for the 1px/2px frame strokes.
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
    /// Status-bar SYNC chip (zellij-style synchronised input; the flag is here so
    /// the chip renders, the broadcasting itself arrives with the keymap modes).
    pub sync_panes: bool,
}

impl Tab {
    fn new(name: String, pane: SharedPane) -> Self {
        Self {
            name,
            root: Node::Leaf { pane: 0 },
            panes: vec![Some(pane)],
            active: 0,
            sync_panes: false,
        }
    }

    /// Tab-bar label. The reference shows `Tab::name` unless it is empty or still
    /// the generated `"Tab N"`, in which case the ACTIVE pane's OSC title wins
    /// (`app.rs:2961-2969`).
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

    /// Pane rects in content coordinates (`app.rs::pane_rects`).
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
    /// Keymap mode shown in the status bar (M3 gives it the real key handling).
    pub mode: ghostwave::keymap::Mode,
    /// "ghostwave" wave animation state (`app.rs:182-183`, init 0.1 / 0.0).
    pub wave_energy: f32,
    pub wave_phase: f32,
    /// One message at a time, never stacked (`app.rs:147`, `toast()`).
    pub toast: Option<(String, std::time::Instant)>,
    /// Pty output since the last tick — each tick with activity costs +0.12
    /// energy (`app.rs:2906-2908`).
    pub pty_activity: bool,
    last_tick: std::time::Instant,
    window: Size,
    /// Cached rects for the active tab, in canvas coordinates. Shared by the
    /// painter and mouse hit-testing so they can never disagree.
    pub rects: Vec<(usize, Rectangle)>,
}

/// How often the front-end needs to be ticked. The macroquad build paces its
/// whole loop (120 FPS while output flows, 60 FPS active, 8 FPS idle); iced
/// redraws on state change, so an equivalent tick is only needed while
/// something is actually animating.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TickRate {
    /// ~60 FPS: wave is energized, or a toast is on screen.
    Fast,
    /// 8 FPS: the wave idles (it never stops while `visual_wave` is on), matching
    /// the macroquad idle tier.
    Slow,
    /// Nothing animating: no tick, no presents.
    Off,
}

impl AppState {
    pub fn new(config: Config, cell: CellDims, window: Size) -> Result<Self> {
        let theme = theme::by_name(&config.theme);
        let mut pane0 = Pane::spawn(
            PaneTag { tab: 0, pane: 0 },
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
            last_tick: std::time::Instant::now(),
            window,
            rects: Vec::new(),
        };
        state.relayout();

        // Dev knob: `GHOST_ICED_PANES=3` opens that many panes at startup (each
        // split alternating right/down) so multi-pane layout, frames and the
        // per-pane pty bridges can be exercised without typing into the window.
        // Values < 2 are ignored.
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

    // ---- chrome state: wave animation + toast -------------------------------

    /// Advance the wave by the real elapsed time. This is the macroquad build's
    /// per-frame update (`app.rs:2902-2919`) with frame counts replaced by
    /// durations, because iced's tick is not a fixed frame time.
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
        // Decay, floor 0.1: the wave never stops while visual_wave is on.
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

        // Toast lifetime: the reference draws for 2.5 s but only clears at 3 s,
        // leaving half a second of invisible-but-present state. One honest 2.5 s.
        if let Some((_, at)) = &self.toast {
            if at.elapsed().as_secs_f32() >= 2.5 {
                self.toast = None;
            }
        }
    }

    /// What the tick loop should be doing right now.
    pub fn tick_rate(&self) -> TickRate {
        if self.toast.is_some() || (self.config.visual_wave && self.wave_energy > 0.15) {
            TickRate::Fast
        } else if self.config.visual_wave {
            TickRate::Slow
        } else {
            TickRate::Off
        }
    }

    /// Show a toast: one at a time, replacing any current one (`App::toast`).
    /// Nothing raises one yet — the reference's triggers (theme switch, copy,
    /// lock/unlock, session save) arrive with the actions in M3/M4.
    #[allow(dead_code)]
    pub fn toast(&mut self, message: impl Into<String>) {
        self.toast = Some((message.into(), std::time::Instant::now()));
    }

    /// Any key press energizes the wave (`app.rs:2153-2156`).
    pub fn bump_wave(&mut self) {
        if self.config.visual_wave {
            self.wave_energy = (self.wave_energy + 0.25).min(2.5);
        }
    }

    /// Content area: what is left of the window for panes. M2 paints the tab bar
    /// and status bar into the reserved bands; reserving them now (with the app
    /// crate's own constants) means pane rects already match the macroquad build,
    /// and the top row's frame labels have somewhere to be drawn.
    pub fn content(&self) -> Rect {
        Rect {
            x: 0.0,
            y: TAB_BAR_H,
            w: self.window.width.max(1.0),
            h: (self.window.height - TAB_BAR_H - STATUS_BAR_H).max(1.0),
        }
    }

    /// Recompute every pane's rect and grid. Call after any window resize or
    /// tree change (the app's `layout_all`).
    pub fn relayout(&mut self) {
        let content = self.content();
        let frames = self.show_frames;
        let cell = self.cell;
        let pad = self.config.padding;
        let tab = &self.tabs[self.active_tab];

        let rects = tab.pane_rects(content, frames);
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
                // Same formula as the app (`app.rs::layout_all`).
                let cols = ((r.w - 2.0 * pad) / cell.w as f32).max(1.0) as u16;
                let rows = ((r.h - 2.0 * pad) / cell.h as f32).max(1.0) as u16;
                pane.lock().unwrap().resize(cols, rows, cell);
            }
        }
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

    /// Split the focused pane; the new pane becomes active (the app's `split`).
    pub fn split(&mut self, dir: SplitDir) -> Result<usize> {
        let tab_idx = self.active_tab;
        let new_id = self.tabs[tab_idx].panes.len();
        let mut pane = Pane::spawn(
            PaneTag {
                tab: tab_idx,
                pane: new_id,
            },
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
            // Empty tree (no panes in it): the new pane becomes the root.
            tab.root = Node::Leaf { pane: new_id };
        } else {
            layout::split_at(&mut tab.root, &path, dir, new_id);
        }
        tab.panes.push(Some(Arc::new(Mutex::new(pane))));
        tab.active = new_id;

        self.relayout();
        Ok(new_id)
    }

    /// Close the focused pane. The last pane is never closed (that would be a
    /// tab close, which is M4).
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
        // Tombstone the closed pane: the tree no longer references it, dropping
        // the last Arc closes the pty child (SIGHUP) and its bridge thread.
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
}
