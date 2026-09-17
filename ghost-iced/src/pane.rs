//! iced-side pane wrapper.
//!
//! The terminal core stays in the app crate: `ghostwave::term::TermPane` owns
//! the pty, the VT state, scrollback, selection, the pane log and the VT key
//! encoder. iced needs two things the pane keeps to itself:
//!
//!   1. its own libghostty render scratch — the pane's copy is private and bound
//!      to the macroquad draw path, so the iced painter owns a second one;
//!   2. a lockable home for the pane, because `canvas::Program::draw` takes
//!      `&self` while drawing has to mutate the render state.

use std::sync::{Arc, Mutex};

use ghostwave::keymap::KeyPress;
use ghostwave::term::{CellDims, TermPane};
use libghostty_vt::key::{self, Mods};
use libghostty_vt::render::{CellIterator, RenderState, RowIterator};

use crate::pty_bridge::{self, PaneTag, PtyRxSlot};

/// Crate-wide error type (the app crate's `ghostwave::Result` shape).
pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// A pane shared between the iced update loop (drains pty output, writes input)
/// and the canvas painter (reads the screen).
pub type SharedPane = Arc<Mutex<Pane>>;

/// libghostty's per-draw scratch space.
pub struct RenderScratch {
    pub render_state: RenderState<'static>,
    pub row_it: RowIterator<'static>,
    pub cell_it: CellIterator<'static>,
}

impl RenderScratch {
    pub fn new() -> Result<Self> {
        Ok(Self {
            render_state: RenderState::new()?,
            row_it: RowIterator::new()?,
            cell_it: CellIterator::new()?,
        })
    }
}

pub struct Pane {
    pub term: TermPane,
    pub scratch: RenderScratch,
    /// Set when something changed that the canvas has to repaint.
    pub dirty: bool,
    /// This pane's pty output stream, already bridged into iced. It travels with
    /// the pane so a pane can never be separated from its output.
    pub rx: PtyRxSlot,
}

impl Pane {
    /// Spawn a pane whose child is the user's login shell.
    ///
    /// `spawn = true` already forks the pty, starts the reader thread and primes
    /// the shell's startup terminal probes — do *not* call `spawn_child` on top
    /// of it (that would fork a second child and orphan the first).
    ///
    /// `GHOST_ICED_FIXTURE` (like the spike's `SPIKE_FIXTURE`) runs `/bin/sh -c
    /// <cmd>` instead of a shell, for benchmarks and scripted screenshots.
    ///
    /// The pane's pty receiver is moved to a bridge thread here, which is what
    /// turns its output into an iced event (`crate::pty_bridge`); `tag` is the
    /// pane id iced messages will carry.
    pub fn spawn(
        tag: PaneTag,
        cols: u16,
        rows: u16,
        cell: CellDims,
        scrollback_lines: usize,
        log_cap_bytes: u64,
    ) -> Result<Self> {
        let fixture = std::env::var("GHOST_ICED_FIXTURE")
            .ok()
            .filter(|c| !c.is_empty());
        let args: Vec<&str> = match fixture.as_deref() {
            Some(cmd) => vec!["-c", cmd],
            None => Vec::new(),
        };
        let program = if fixture.is_some() {
            Some("/bin/sh")
        } else {
            None
        };

        let mut term = TermPane::new(
            cols,
            rows,
            cell,
            program,
            &args,
            None,
            scrollback_lines,
            log_cap_bytes,
            true,
            None,
        )?;
        let rx = term
            .pty_rx
            .take()
            .expect("pty reader receiver (TermPane::new with spawn = true)");
        Ok(Self {
            term,
            scratch: RenderScratch::new()?,
            dirty: true,
            rx: pty_bridge::spawn(tag, rx),
        })
    }

    /// Apply the configured theme's default fg/bg to this pane — the same call
    /// the macroquad app makes in `app.rs::apply_terminal_colors`. Without it a
    /// fresh pane comes up with libghostty's near-black defaults instead of the
    /// configured theme.
    pub fn apply_theme(&mut self, theme: &ghostwave::theme::Theme) {
        let to_rgb = |c: ghostwave::theme::Rgb| libghostty_vt::style::RgbColor {
            r: c.0,
            g: c.1,
            b: c.2,
        };
        let _ = self
            .term
            .term
            .set_default_fg_color(Some(to_rgb(theme.fg)))
            .and_then(|t| t.set_default_bg_color(Some(to_rgb(theme.bg))));
        self.term.force_repaint = true;
        self.dirty = true;
    }

    /// Feed pty output into the terminal. This mirrors what `TermPane::poll_pty`
    /// does for the macroquad build (log first, then the VT) — the receiver
    /// itself lives on the iced side of the bridge, so `poll_pty` sees nothing.
    pub fn ingest(&mut self, data: &[u8]) {
        if data.is_empty() {
            return;
        }
        if let Some(log) = self.term.log.as_mut() {
            log.write(data);
        }
        self.term.term.vt_write(data);
        self.term.force_repaint = true;
        self.dirty = true;
    }

    /// The child exited or the pty errored out.
    pub fn exited(&mut self, reason: &str) {
        self.term.exited = Some(reason.to_string());
        self.term.last_exit_code = self.term.child.as_mut().and_then(|c| c.poll_reap());
        self.term.force_repaint = true;
        self.dirty = true;
    }

    /// Route a key press through the pane's existing VT key encoder — the same
    /// call the macroquad app makes (`app.rs::handle_key`), so kitty keyboard
    /// protocol, application-cursor mode and friends behave identically.
    pub fn key_press(&mut self, kp: &KeyPress, action: key::Action) {
        let consumed = if kp.text.is_some() && kp.mods.contains(Mods::SHIFT) {
            Mods::SHIFT
        } else {
            Mods::empty()
        };
        let text = kp.text.map(|c| c.to_string());
        let _ = self.term.handle_key(
            kp.key,
            kp.mods,
            consumed,
            ghostwave::keymap::unshifted_codepoint(kp.key),
            text.as_deref(),
            action,
        );
        self.term.flush_response();
        self.dirty = true;
    }

    /// Text input that is not a key press (IME commit, dead keys). Wired up
    /// with the keymap modes in M3 — iced currently delivers typed text inside
    /// the key press event, which `key_press` already handles.
    #[allow(dead_code)]
    pub fn char_input(&mut self, c: char) {
        self.term.handle_char(c);
        self.term.flush_response();
        self.dirty = true;
    }

    /// Grid resize. `TermPane::set_grid` is a no-op when the grid is unchanged.
    pub fn resize(&mut self, cols: u16, rows: u16, cell: CellDims) {
        let _ = self.term.set_grid(cols, rows, cell);
        self.dirty = true;
    }

    /// Wheel scrollback (`TermPane::scroll`, which also leaves the pane in
    /// "scrolled" state so output stops snapping it back to the bottom).
    pub fn scroll(&mut self, lines: i32) {
        self.term.scroll(lines);
        self.dirty = true;
    }
}

/// Cell metrics, computed exactly like the macroquad build (`app.rs`):
/// `cell_w = ceil('M' advance)`, `cell_h = ceil(ascent + descent + row_gap)`,
/// `baseline = ascent + row_gap / 2`. Same font, same fontdue call, so glyph
/// positions line up with the reference build at the same pixel size.
pub fn cell_dims(font_bytes: &[u8], font_px: f32, row_gap: f32) -> CellDims {
    let font = fontdue::Font::from_bytes(font_bytes, fontdue::FontSettings::default())
        .expect("fontdue: parse embedded font");
    let m = font
        .horizontal_line_metrics(font_px)
        .expect("fontdue: horizontal_line_metrics");
    let ascent = m.ascent;
    let descent = -m.descent;
    CellDims {
        w: (font.metrics('M', font_px).advance_width).max(1.0).ceil() as u32,
        h: (ascent + descent + row_gap).max(8.0).ceil() as u32,
        baseline: ascent + row_gap / 2.0,
    }
}
