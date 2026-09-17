//! Floating pane management and persistent scratchpad overlay.
//!
//! Provides data structures and layout management for floating terminal panes
//! positioned over the tiled grid, including the F12 / Ctrl+~ dropdown scratchpad.
//! Floating panes can be detached from the grid, moved, resized, hidden to the background,
//! cycled (Zellij-style `e`/`E`), and re-embedded back into their original layout position.

use std::sync::{Arc, Mutex};

use ghostwave::app::{STATUS_BAR_H, TAB_BAR_H};
pub use ghostwave::layout::FloatOrigin;
use ghostwave::layout::SplitDir;
use iced::{Point, Rectangle, Size};

use crate::app::{AppState, GridPane};
use crate::pane::{Pane, PaneEntry, PaneId, SharedPane};

/// Default width ratio for scratchpad overlay (80% of window width).
pub const DEFAULT_SCRATCHPAD_WIDTH_RATIO: f32 = 0.80;

/// Default height ratio for scratchpad overlay (70% of available height).
pub const DEFAULT_SCRATCHPAD_HEIGHT_RATIO: f32 = 0.70;

/// Default top offset in pixels below the tab bar for top-drawer overlays.
pub const DEFAULT_TOP_OFFSET: f32 = 12.0;

/// Minimum width for a floating pane in pixels.
pub const MIN_FLOAT_WIDTH: f32 = 100.0;

/// Minimum height for a floating pane in pixels.
pub const MIN_FLOAT_HEIGHT: f32 = 60.0;

/// A floating terminal pane rendered as an overlay on top of the tiled grid.
#[derive(Clone, Debug)]
pub struct FloatingPane {
    /// Process-wide pane identity.
    pub pane: PaneId,
    /// Bounding rectangle in window coordinates.
    pub rect: Rectangle,
    /// Origin location where the pane was detached from, if any.
    pub origin: Option<FloatOrigin>,
    /// Whether the pane is hidden in the background.
    pub hidden: bool,
    /// Stacking order layer (higher z-indices render on top).
    pub z_index: usize,
}

impl FloatingPane {
    /// Creates a new visible floating pane at the specified bounds.
    pub fn new(pane: PaneId, rect: Rectangle, origin: Option<FloatOrigin>, z_index: usize) -> Self {
        Self {
            pane,
            rect,
            origin,
            hidden: false,
            z_index,
        }
    }

    /// Returns the identity of the floating pane.
    pub fn id(&self) -> PaneId {
        self.pane
    }

    /// Whether this pane is currently hidden from view.
    pub fn is_hidden(&self) -> bool {
        self.hidden
    }

    /// Returns the top-left position of the floating pane.
    pub fn position(&self) -> Point {
        Point::new(self.rect.x, self.rect.y)
    }

    /// Returns the size of the floating pane.
    pub fn size(&self) -> Size {
        Size::new(self.rect.width, self.rect.height)
    }
}

/// Geometry placement preset for the scratchpad overlay.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ScratchpadGeometry {
    /// Centered overlay within the window bounds.
    Centered {
        /// Ratio of window width (e.g. 0.80 for 80%).
        width_ratio: f32,
        /// Ratio of window height (e.g. 0.70 for 70%).
        height_ratio: f32,
    },
    /// Top-drawer overlay attached below the tab bar.
    TopDrawer {
        /// Ratio of window width (e.g. 0.80 for 80%).
        width_ratio: f32,
        /// Ratio of window height (e.g. 0.70 for 70%).
        height_ratio: f32,
        /// Top margin in pixels below the tab bar.
        top_offset: f32,
    },
    /// Fixed pixel bounds.
    Custom(Rectangle),
}

impl Default for ScratchpadGeometry {
    fn default() -> Self {
        Self::TopDrawer {
            width_ratio: DEFAULT_SCRATCHPAD_WIDTH_RATIO,
            height_ratio: DEFAULT_SCRATCHPAD_HEIGHT_RATIO,
            top_offset: DEFAULT_TOP_OFFSET,
        }
    }
}

impl ScratchpadGeometry {
    /// Creates a centered geometry preset.
    pub fn centered(width_ratio: f32, height_ratio: f32) -> Self {
        Self::Centered {
            width_ratio,
            height_ratio,
        }
    }

    /// Creates a top-drawer geometry preset.
    pub fn top_drawer(width_ratio: f32, height_ratio: f32, top_offset: f32) -> Self {
        Self::TopDrawer {
            width_ratio,
            height_ratio,
            top_offset,
        }
    }

    /// Computes the overlay rectangle within the specified window dimensions.
    pub fn compute_rect(&self, window: Size) -> Rectangle {
        match *self {
            Self::Centered {
                width_ratio,
                height_ratio,
            } => {
                let w = (window.width * width_ratio.clamp(0.1, 1.0))
                    .clamp(MIN_FLOAT_WIDTH, window.width.max(MIN_FLOAT_WIDTH));
                let h = (window.height * height_ratio.clamp(0.1, 1.0))
                    .clamp(MIN_FLOAT_HEIGHT, window.height.max(MIN_FLOAT_HEIGHT));
                let x = ((window.width - w) / 2.0).max(0.0);
                let y = ((window.height - h) / 2.0).max(0.0);
                Rectangle {
                    x,
                    y,
                    width: w,
                    height: h,
                }
            }
            Self::TopDrawer {
                width_ratio,
                height_ratio,
                top_offset,
            } => {
                let top = TAB_BAR_H + top_offset.max(0.0);
                let avail_h = (window.height - top - STATUS_BAR_H).max(MIN_FLOAT_HEIGHT);
                let w = (window.width * width_ratio.clamp(0.1, 1.0))
                    .clamp(MIN_FLOAT_WIDTH, window.width.max(MIN_FLOAT_WIDTH));
                let h = (avail_h * height_ratio.clamp(0.1, 1.0)).clamp(MIN_FLOAT_HEIGHT, avail_h);
                let x = ((window.width - w) / 2.0).max(0.0);
                let y = top;
                clamp_rect_to_window(
                    Rectangle {
                        x,
                        y,
                        width: w,
                        height: h,
                    },
                    window,
                )
            }
            Self::Custom(rect) => clamp_rect_to_window(rect, window),
        }
    }
}

/// The id of the pane the active tab has focused, read without locking it.
fn active_pane_id(app: &AppState) -> Option<PaneId> {
    let tab = app.tabs.get(app.active_tab)?;
    let entry = tab.panes.get(tab.active)?.as_ref()?;
    Some(entry.id())
}

/// State and manager for floating panes and the F12 scratchpad overlay.
#[derive(Clone, Debug)]
pub struct ScratchpadState {
    /// Dedicated scratchpad terminal pane ID, if spawned.
    pub scratchpad_pane: Option<PaneId>,
    /// Whether the scratchpad dropdown is currently open.
    pub is_open: bool,
    /// Geometry preset for scratchpad placement.
    pub geometry: ScratchpadGeometry,
    /// Active floating panes managed by this state.
    pub floating: Vec<FloatingPane>,
    /// Monotonically increasing counter for z-index ordering.
    pub next_z_index: usize,
    /// Index in `floating` of the currently focused floating pane.
    pub focused_float: Option<usize>,
    /// Cached window dimensions.
    pub last_window_size: Option<Size>,
}

impl Default for ScratchpadState {
    fn default() -> Self {
        Self::new()
    }
}

impl ScratchpadState {
    /// Creates a new closed scratchpad manager with default geometry.
    pub fn new() -> Self {
        Self {
            scratchpad_pane: None,
            is_open: false,
            geometry: ScratchpadGeometry::default(),
            floating: Vec::new(),
            next_z_index: 1,
            focused_float: None,
            last_window_size: None,
        }
    }

    /// Whether the scratchpad is currently open.
    pub fn is_open(&self) -> bool {
        self.is_open
    }

    /// Returns the scratchpad pane ID, if allocated.
    pub fn pane(&self) -> Option<PaneId> {
        self.scratchpad_pane
    }

    /// Returns all registered floating panes.
    pub fn floating_panes(&self) -> &[FloatingPane] {
        &self.floating
    }

    /// Returns an iterator over all non-hidden floating panes.
    pub fn visible_floats(&self) -> impl Iterator<Item = &FloatingPane> {
        self.floating.iter().filter(|f| !f.hidden)
    }

    /// Finds a floating pane by its pane ID.
    pub fn find_float(&self, pane: PaneId) -> Option<&FloatingPane> {
        self.floating.iter().find(|f| f.pane == pane)
    }

    /// Finds a mutable floating pane by its pane ID.
    pub fn find_float_mut(&mut self, pane: PaneId) -> Option<&mut FloatingPane> {
        self.floating.iter_mut().find(|f| f.pane == pane)
    }

    /// Whether the specified pane is currently floating.
    pub fn is_floating(&self, pane: PaneId) -> bool {
        self.floating.iter().any(|f| f.pane == pane)
    }

    /// Whether the specified pane is the dedicated scratchpad terminal.
    pub fn is_scratchpad(&self, pane: PaneId) -> bool {
        self.scratchpad_pane == Some(pane)
    }

    /// Returns the pane ID of the currently focused floating pane.
    pub fn focused_float_id(&self) -> Option<PaneId> {
        self.focused_float
            .and_then(|idx| self.floating.get(idx))
            .map(|f| f.pane)
    }

    /// Records the current window size for overlay computations.
    pub fn set_window_size(&mut self, size: Size) {
        self.last_window_size = Some(size);
    }

    /// Resolves the current window dimensions from recorded state or pane bounds.
    pub fn window_size(&self, app: &AppState) -> Size {
        if let Some(size) = self.last_window_size {
            return size;
        }
        let mut max_w: f32 = 0.0;
        let mut max_h: f32 = 0.0;
        for handle in app.current().term_handles() {
            if let Ok(guard) = handle.lock() {
                if let Some(b) = guard.last_bounds {
                    max_w = max_w.max(b.x + b.width);
                    max_h = max_h.max(b.y + b.height);
                }
            }
        }
        if max_w > 100.0 && max_h > 100.0 {
            Size::new(max_w, max_h + STATUS_BAR_H)
        } else {
            Size::new(1200.0, 800.0)
        }
    }

    /// Toggles the open/closed state of the scratchpad terminal.
    ///
    /// If the scratchpad has not yet been spawned, forks a fresh pty pane,
    /// formats its geometry, adds it to the current tab's pane storage without
    /// attaching it to the tiled grid, and reveals it. If already spawned, toggles
    /// its visibility and updates focus.
    pub fn toggle(&mut self, app: &mut AppState) {
        if let Some(id) = self.scratchpad_pane {
            if app.pane_by_id(id).is_some() {
                let open = self.toggle_state();
                if open {
                    if let Some(pos) = self.floating.iter().position(|f| f.pane == id) {
                        self.focused_float = Some(pos);
                    }
                    if let Some(tab) = app.tabs.get_mut(app.active_tab) {
                        if let Some(idx) = tab.index_of(id) {
                            tab.active = idx;
                        }
                    }
                    app.toast("scratchpad opened (F12 to hide)");
                } else {
                    let tab = &mut app.tabs[app.active_tab];
                    if let Some(cell) = tab.grid.iter().next().map(|(c, _)| *c) {
                        if let Some(grid_pane) = tab.grid.get(cell) {
                            if let Some(first_id) = grid_pane.active_id() {
                                if let Some(idx) = tab.index_of(first_id) {
                                    tab.active = idx;
                                }
                            }
                        }
                    }
                    app.toast("scratchpad hidden");
                }
                return;
            }
            self.scratchpad_pane = None;
            self.floating.retain(|f| f.pane != id);
        }

        let win_size = self.window_size(app);
        let rect = self.geometry.compute_rect(win_size);
        let (cols, rows) = crate::panes_grid::bounds_to_cols_rows(
            rect.width,
            rect.height,
            app.cell,
            app.config.padding,
        );
        let cols = cols.max(10);
        let rows = rows.max(5);

        let spawn_res = Pane::spawn(
            cols,
            rows,
            app.cell,
            app.config.scrollback,
            app.config.max_log_mb * 1024 * 1024,
        );

        match spawn_res {
            Ok(mut pane) => {
                pane.cached_title = "scratchpad".to_string();
                pane.apply_theme(&app.theme);
                let id = pane.id;
                let shared: SharedPane = Arc::new(Mutex::new(pane));
                let tab = &mut app.tabs[app.active_tab];
                let idx = tab.panes.len();
                tab.panes.push(Some(PaneEntry::Term(shared)));
                tab.active = idx;
                self.scratchpad_pane = Some(id);
                self.to_float(id, rect, None);
                self.is_open = true;
                app.toast("scratchpad opened (F12 to hide)");
            }
            Err(e) => {
                app.toast(format!("failed to spawn scratchpad: {e}"));
            }
        }
    }

    /// Toggles the open/closed state flag and updates the associated floating pane.
    ///
    /// Returns the new open status.
    pub fn toggle_state(&mut self) -> bool {
        self.is_open = !self.is_open;
        if let Some(id) = self.scratchpad_pane {
            if let Some(float) = self.floating.iter_mut().find(|f| f.pane == id) {
                float.hidden = !self.is_open;
                if self.is_open {
                    self.next_z_index += 1;
                    float.z_index = self.next_z_index;
                }
            }
        }
        self.is_open
    }

    /// Detaches a pane into a float with the given bounds and origin tracking.
    pub fn to_float(&mut self, pane: PaneId, bounds: Rectangle, origin: Option<FloatOrigin>) {
        self.next_z_index += 1;
        let z = self.next_z_index;
        if let Some(existing) = self.floating.iter_mut().find(|f| f.pane == pane) {
            existing.rect = bounds;
            existing.origin = origin;
            existing.hidden = false;
            existing.z_index = z;
        } else {
            self.floating.push(FloatingPane {
                pane,
                rect: bounds,
                origin,
                hidden: false,
                z_index: z,
            });
        }
        self.focused_float = Some(self.floating.len() - 1);
    }

    /// Floats the focused pane out of its grid cell, or embeds it back if it is
    /// already floating.
    ///
    /// A floated pane keeps its place in `tab.panes` but loses its grid cell, so
    /// the grid stops painting it and the float layer takes over — the same split
    /// of duties the scratchpad uses. Embedding reverses it: the float is dropped
    /// and the pane splits back in beside the focused cell. The origin a float was
    /// created from is not kept, which matches the reference's behaviour when its
    /// recorded origin has gone stale.
    pub fn toggle_float_pane(&mut self, app: &mut AppState) {
        let tab_index = app.active_tab;
        let Some(active) = active_pane_id(app) else {
            return;
        };
        if self.floating.iter().any(|float| float.pane == active) {
            self.floating.retain(|float| float.pane != active);
            self.focused_float = None;
            {
                let tab = &mut app.tabs[tab_index];
                if let Some(target) = tab.focused() {
                    tab.grid.split(
                        iced::widget::pane_grid::Axis::Vertical,
                        target,
                        crate::app::GridPane::single(active),
                    );
                }
            }
            app.toast("floating pane embedded");
            return;
        }
        let rect = self.geometry.compute_rect(self.window_size(app));
        {
            let tab = &mut app.tabs[tab_index];
            if let Some(index) = tab.index_of(active) {
                if let Some(cell) = tab.handle(index) {
                    let _ = tab.grid.close(cell);
                }
            }
        }
        self.to_float(active, rect, None);
        app.toast("floating pane (the same bind embeds it again)");
    }

    /// Hides or shows the focused floating pane.
    pub fn toggle_hide_float(&mut self, app: &mut AppState) {
        let Some(active) = active_pane_id(app) else {
            return;
        };
        match self.floating.iter_mut().find(|float| float.pane == active) {
            Some(float) => {
                float.hidden = !float.hidden;
                let hidden = float.hidden;
                app.toast(if hidden {
                    "floating pane hidden"
                } else {
                    "floating pane shown"
                });
            }
            None => app.toast("no floating pane here (float one first)"),
        }
    }

    /// Focuses the next floating pane of this tab, showing it if it was hidden.
    pub fn cycle_float(&mut self, app: &mut AppState) {
        if self.floating.is_empty() {
            app.toast("no floating panes");
            return;
        }
        let current =
            active_pane_id(app).and_then(|id| self.floating.iter().position(|f| f.pane == id));
        let next = match current {
            Some(index) => (index + 1) % self.floating.len(),
            None => 0,
        };
        self.floating[next].hidden = false;
        self.focused_float = Some(next);
        let pane = self.floating[next].pane;
        let index = app.tabs[app.active_tab].index_of(pane);
        if let Some(index) = index {
            app.focus_index(index);
        }
        app.toast(format!("floating pane {}", next + 1));
    }

    /// Re-attaches a floating pane back into the pane grid, returning its stored layout origin if tracked.
    pub fn embed(&mut self, pane: PaneId) -> Option<FloatOrigin> {
        let pos = self.floating.iter().position(|f| f.pane == pane)?;
        let removed = self.floating.remove(pos);
        if self.focused_float == Some(pos) {
            self.focused_float = if self.floating.is_empty() {
                None
            } else {
                Some(pos.min(self.floating.len() - 1))
            };
        } else if let Some(idx) = self.focused_float {
            if idx > pos {
                self.focused_float = Some(idx - 1);
            }
        }
        removed.origin
    }

    /// Embeds a floating pane back into the application's active tab grid.
    pub fn embed_into(&mut self, pane: PaneId, app: &mut AppState) -> bool {
        let origin = self.embed(pane);
        let tab = &mut app.tabs[app.active_tab];
        if tab.shared(pane).is_none() {
            return false;
        }
        if let Some(target) = tab.focused() {
            let dir = match &origin {
                Some(FloatOrigin::Split { dir, .. }) => *dir,
                _ => SplitDir::Horizontal,
            };
            let axis = crate::panes_grid::axis(dir);
            let _ = tab.grid.split(axis, target, GridPane::single(pane));
        } else {
            tab.grid = iced::widget::pane_grid::State::new(GridPane::single(pane)).0;
        }
        if let Some(idx) = tab.index_of(pane) {
            tab.active = idx;
        }
        app.toast("pane embedded");
        app.relayout();
        true
    }

    /// Cycles through hidden/shown floating panes in forward sequence (Zellij-style `E`).
    ///
    /// Reveals the selected floating pane if it was hidden, raises its z-index, and
    /// returns its `PaneId`.
    pub fn cycle_floats(&mut self) -> Option<PaneId> {
        if self.floating.is_empty() {
            return None;
        }
        let next_idx = match self.focused_float {
            Some(idx) => (idx + 1) % self.floating.len(),
            None => 0,
        };
        self.floating[next_idx].hidden = false;
        self.next_z_index += 1;
        self.floating[next_idx].z_index = self.next_z_index;
        self.focused_float = Some(next_idx);
        Some(self.floating[next_idx].pane)
    }

    /// Cycles through hidden/shown floating panes in reverse sequence.
    pub fn cycle_floats_prev(&mut self) -> Option<PaneId> {
        if self.floating.is_empty() {
            return None;
        }
        let len = self.floating.len();
        let next_idx = match self.focused_float {
            Some(idx) => (idx + len - 1) % len,
            None => 0,
        };
        self.floating[next_idx].hidden = false;
        self.next_z_index += 1;
        self.floating[next_idx].z_index = self.next_z_index;
        self.focused_float = Some(next_idx);
        Some(self.floating[next_idx].pane)
    }

    /// Toggles the hidden state of a floating pane (Zellij-style `e`).
    ///
    /// Returns the updated hidden state, or `None` if the pane is not floating.
    pub fn toggle_hide(&mut self, pane: PaneId) -> Option<bool> {
        let float = self.floating.iter_mut().find(|f| f.pane == pane)?;
        float.hidden = !float.hidden;
        Some(float.hidden)
    }

    /// Hides the currently focused floating pane.
    pub fn hide_focused(&mut self) -> Option<PaneId> {
        let idx = self.focused_float?;
        let float = self.floating.get_mut(idx)?;
        float.hidden = true;
        Some(float.pane)
    }

    /// Reveals a specific floating pane by clearing its hidden flag and raising its z-index.
    pub fn reveal(&mut self, pane: PaneId) -> bool {
        if let Some(pos) = self.floating.iter().position(|f| f.pane == pane) {
            self.floating[pos].hidden = false;
            self.next_z_index += 1;
            self.floating[pos].z_index = self.next_z_index;
            self.focused_float = Some(pos);
            true
        } else {
            false
        }
    }

    /// Translates a floating pane by `(delta_x, delta_y)`, clamping it within window dimensions.
    pub fn move_float(&mut self, pane: PaneId, delta_x: f32, delta_y: f32, window: Size) {
        if let Some(float) = self.find_float_mut(pane) {
            float.rect.x += delta_x;
            float.rect.y += delta_y;
            float.rect = clamp_rect_to_window(float.rect, window);
        }
    }

    /// Resizes a floating pane by `(delta_w, delta_h)`, clamping it within window dimensions.
    pub fn resize_float(&mut self, pane: PaneId, delta_w: f32, delta_h: f32, window: Size) {
        if let Some(float) = self.find_float_mut(pane) {
            float.rect.width = (float.rect.width + delta_w).max(MIN_FLOAT_WIDTH);
            float.rect.height = (float.rect.height + delta_h).max(MIN_FLOAT_HEIGHT);
            float.rect = clamp_rect_to_window(float.rect, window);
        }
    }
}

/// Clamps a rectangle to fit completely within bounding limits.
pub fn clamp_rect(rect: Rectangle, bounds: Rectangle) -> Rectangle {
    let min_w = MIN_FLOAT_WIDTH.min(bounds.width);
    let min_h = MIN_FLOAT_HEIGHT.min(bounds.height);
    let width = rect.width.clamp(min_w, bounds.width.max(min_w));
    let height = rect.height.clamp(min_h, bounds.height.max(min_h));
    let max_x = (bounds.x + bounds.width - width).max(bounds.x);
    let max_y = (bounds.y + bounds.height - height).max(bounds.y);
    let x = rect.x.clamp(bounds.x, max_x);
    let y = rect.y.clamp(bounds.y, max_y);
    Rectangle {
        x,
        y,
        width,
        height,
    }
}

/// Clamps a floating pane rectangle within window dimensions.
pub fn clamp_rect_to_window(rect: Rectangle, window: Size) -> Rectangle {
    clamp_rect(
        rect,
        Rectangle {
            x: 0.0,
            y: 0.0,
            width: window.width,
            height: window.height,
        },
    )
}

/// Clamps a floating pane rectangle within the content area (between tab bar and status bar).
pub fn clamp_rect_to_content(rect: Rectangle, window: Size) -> Rectangle {
    let top = TAB_BAR_H;
    let bottom = STATUS_BAR_H;
    let height = (window.height - top - bottom).max(0.0);
    clamp_rect(
        rect,
        Rectangle {
            x: 0.0,
            y: top,
            width: window.width,
            height,
        },
    )
}

/// Computes overlay coordinates for a given geometry preset and window size.
pub fn compute_overlay_rect(geometry: &ScratchpadGeometry, window: Size) -> Rectangle {
    geometry.compute_rect(window)
}

/// Structural equality comparison helper for [`FloatOrigin`].
pub fn float_origins_eq(a: &FloatOrigin, b: &FloatOrigin) -> bool {
    match (a, b) {
        (FloatOrigin::Root, FloatOrigin::Root) => true,
        (FloatOrigin::Stack { path: p1 }, FloatOrigin::Stack { path: p2 }) => p1 == p2,
        (
            FloatOrigin::Split {
                parent: p1,
                side: s1,
                dir: d1,
                ratio: r1,
            },
            FloatOrigin::Split {
                parent: p2,
                side: s2,
                dir: d2,
                ratio: r2,
            },
        ) => p1 == p2 && s1 == s2 && d1 == d2 && (r1 - r2).abs() < 1e-4,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_scratchpad_toggle_state() {
        let mut state = ScratchpadState::new();
        assert!(!state.is_open());
        assert_eq!(state.pane(), None);

        let pane = PaneId(42);
        let rect = Rectangle {
            x: 50.0,
            y: 50.0,
            width: 500.0,
            height: 400.0,
        };
        state.to_float(pane, rect, None);
        state.scratchpad_pane = Some(pane);

        let opened = state.toggle_state();
        assert!(opened);
        assert!(state.is_open());
        assert!(!state.find_float(pane).unwrap().hidden);

        let closed = state.toggle_state();
        assert!(!closed);
        assert!(!state.is_open());
        assert!(state.find_float(pane).unwrap().hidden);

        let opened_again = state.toggle_state();
        assert!(opened_again);
        assert!(state.is_open());
        assert!(!state.find_float(pane).unwrap().hidden);
    }

    #[test]
    fn test_floating_rect_clamping_within_window_bounds() {
        let win = Size::new(800.0, 600.0);

        let neg_rect = Rectangle {
            x: -120.0,
            y: -50.0,
            width: 300.0,
            height: 200.0,
        };
        let clamped_neg = clamp_rect_to_window(neg_rect, win);
        assert_eq!(clamped_neg.x, 0.0);
        assert_eq!(clamped_neg.y, 0.0);
        assert_eq!(clamped_neg.width, 300.0);
        assert_eq!(clamped_neg.height, 200.0);

        let overflow_rect = Rectangle {
            x: 700.0,
            y: 500.0,
            width: 300.0,
            height: 200.0,
        };
        let clamped_overflow = clamp_rect_to_window(overflow_rect, win);
        assert_eq!(clamped_overflow.x, 500.0);
        assert_eq!(clamped_overflow.y, 400.0);
        assert_eq!(clamped_overflow.width, 300.0);
        assert_eq!(clamped_overflow.height, 200.0);

        let small_rect = Rectangle {
            x: 100.0,
            y: 100.0,
            width: 20.0,
            height: 10.0,
        };
        let clamped_small = clamp_rect_to_window(small_rect, win);
        assert!(clamped_small.width >= MIN_FLOAT_WIDTH);
        assert!(clamped_small.height >= MIN_FLOAT_HEIGHT);

        let content_rect = Rectangle {
            x: 0.0,
            y: 0.0,
            width: 300.0,
            height: 200.0,
        };
        let clamped_content = clamp_rect_to_content(content_rect, win);
        assert_eq!(clamped_content.y, TAB_BAR_H);
        assert!(clamped_content.y + clamped_content.height <= win.height - STATUS_BAR_H);
    }

    #[test]
    fn test_float_hiding_and_revealing() {
        let mut state = ScratchpadState::new();
        let p1 = PaneId(1);
        let p2 = PaneId(2);
        let rect = Rectangle {
            x: 10.0,
            y: 10.0,
            width: 200.0,
            height: 150.0,
        };

        state.to_float(p1, rect, None);
        state.to_float(p2, rect, None);

        assert!(!state.find_float(p1).unwrap().hidden);
        assert!(!state.find_float(p2).unwrap().hidden);

        assert_eq!(state.toggle_hide(p1), Some(true));
        assert!(state.find_float(p1).unwrap().hidden);
        assert!(!state.find_float(p2).unwrap().hidden);

        assert_eq!(state.toggle_hide(p1), Some(false));
        assert!(!state.find_float(p1).unwrap().hidden);

        state.toggle_hide(p1);
        assert!(state.find_float(p1).unwrap().hidden);

        let focused = state.cycle_floats();
        assert!(focused.is_some());
        assert!(!state.find_float(p1).unwrap().hidden || !state.find_float(p2).unwrap().hidden);

        state.toggle_hide(p1);
        assert!(state.find_float(p1).unwrap().hidden);
        assert!(state.reveal(p1));
        assert!(!state.find_float(p1).unwrap().hidden);
    }

    #[test]
    fn test_reembed_origin_tracking() {
        let mut state = ScratchpadState::new();
        let rect = Rectangle {
            x: 20.0,
            y: 20.0,
            width: 400.0,
            height: 300.0,
        };

        let p1 = PaneId(101);
        state.to_float(p1, rect, Some(FloatOrigin::Root));
        assert!(state.is_floating(p1));
        let origin1 = state.embed(p1);
        assert!(matches!(origin1, Some(FloatOrigin::Root)));
        assert!(!state.is_floating(p1));

        let p2 = PaneId(102);
        let split_origin = FloatOrigin::Split {
            parent: vec![0, 1],
            side: 1,
            dir: SplitDir::Vertical,
            ratio: 0.45,
        };
        state.to_float(p2, rect, Some(split_origin.clone()));
        let origin2 = state.embed(p2);
        assert!(origin2.is_some());
        assert!(float_origins_eq(&origin2.unwrap(), &split_origin));
        assert!(!state.is_floating(p2));

        let p3 = PaneId(103);
        let stack_origin = FloatOrigin::Stack {
            path: vec![1, 0, 1],
        };
        state.to_float(p3, rect, Some(stack_origin.clone()));
        let origin3 = state.embed(p3);
        assert!(origin3.is_some());
        assert!(float_origins_eq(&origin3.unwrap(), &stack_origin));

        assert!(state.embed(PaneId(999)).is_none());
    }

    #[test]
    fn test_geometry_overlay_computation() {
        let win = Size::new(1000.0, 800.0);

        let centered = ScratchpadGeometry::Centered {
            width_ratio: 0.8,
            height_ratio: 0.6,
        };
        let r1 = compute_overlay_rect(&centered, win);
        assert!((r1.width - 800.0).abs() < 0.01);
        assert!((r1.height - 480.0).abs() < 0.01);
        assert!((r1.x - 100.0).abs() < 0.01);
        assert!((r1.y - 160.0).abs() < 0.01);

        let top_drawer = ScratchpadGeometry::TopDrawer {
            width_ratio: 0.9,
            height_ratio: 0.5,
            top_offset: 20.0,
        };
        let r2 = compute_overlay_rect(&top_drawer, win);
        assert!((r2.width - 900.0).abs() < 0.01);
        assert!((r2.height - 362.0).abs() < 0.01);
        assert!((r2.x - 50.0).abs() < 0.01);
        assert!((r2.y - 50.0).abs() < 0.01);
    }

    #[test]
    fn test_cycle_floats_ordering_and_z_index() {
        let mut state = ScratchpadState::new();
        let p1 = PaneId(10);
        let p2 = PaneId(20);
        let rect = Rectangle::new(Point::new(10.0, 10.0), Size::new(200.0, 150.0));

        state.to_float(p1, rect, None);
        state.to_float(p2, rect, None);

        let initial_z = state.find_float(p1).unwrap().z_index;

        let next1 = state.cycle_floats();
        assert!(next1.is_some());
        let z1 = state.find_float(next1.unwrap()).unwrap().z_index;
        assert!(z1 > initial_z);

        let next2 = state.cycle_floats();
        assert!(next2.is_some());
        let z2 = state.find_float(next2.unwrap()).unwrap().z_index;
        assert!(z2 > z1);
    }

    #[test]
    fn test_move_and_resize_float_clamping() {
        let mut state = ScratchpadState::new();
        let win = Size::new(1000.0, 800.0);
        let pane = PaneId(1);
        let rect = Rectangle {
            x: 100.0,
            y: 100.0,
            width: 300.0,
            height: 200.0,
        };
        state.to_float(pane, rect, None);

        state.move_float(pane, 50.0, 30.0, win);
        let f = state.find_float(pane).unwrap();
        assert_eq!(f.rect.x, 150.0);
        assert_eq!(f.rect.y, 130.0);

        state.move_float(pane, 2000.0, 2000.0, win);
        let f = state.find_float(pane).unwrap();
        assert_eq!(f.rect.x, 700.0);
        assert_eq!(f.rect.y, 600.0);

        state.resize_float(pane, 50.0, 50.0, win);
        let f = state.find_float(pane).unwrap();
        assert!(f.rect.width <= win.width);
        assert!(f.rect.height <= win.height);
    }
}
