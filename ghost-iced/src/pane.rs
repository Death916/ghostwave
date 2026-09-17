//! iced-side pane wrapper: `ghostwave::term::TermPane` in a lockable home, because
//! `canvas::Program::draw` takes `&self` while drawing mutates render state, plus
//! libghostty's per-draw render scratch.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use ghostwave::keymap::KeyPress;
use ghostwave::term::{CellDims, TermPane};
use libghostty_vt::key::{self, Mods};
use libghostty_vt::render::{CellIterator, RenderState, RowIterator};

use crate::config::{DynamicTitleMode, PaneTitleFormat};
use crate::pty_bridge::{self, PtyRxSlot};

/// Crate-wide alias for results that box their error.
pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// A pane handle shared by the update loop (drains pty output, writes input) and the
/// canvas painter (reads the screen).
pub type SharedPane = Arc<Mutex<Pane>>;

/// Reusable libghostty render state and iterators, kept across draws.
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

/// A pane's process-wide identity, independent of its position in a tab.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PaneId(pub usize);

impl PaneId {
    fn next() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }
}

pub struct Pane {
    pub id: PaneId,
    pub term: TermPane,
    pub scratch: RenderScratch,
    pub dirty: bool,
    /// Last bounds laid out and painted by iced.
    pub last_bounds: Option<iced::Rectangle>,
    /// The pane's pty output stream, already bridged into iced.
    pub rx: PtyRxSlot,
    /// Cached dynamic title updated on tick/pty events.
    pub cached_title: String,
}

impl Pane {
    /// Forks a pty running the user's login shell and starts its reader thread,
    /// priming the shell's startup terminal probes; the pty receiver moves to a bridge
    /// thread that turns its output into an iced event ([`crate::pty_bridge`]).
    ///
    /// `GHOST_ICED_FIXTURE` runs `/bin/sh -c <cmd>` in place of a shell.
    pub fn spawn(
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
        let id = PaneId::next();
        Ok(Self {
            id,
            term,
            scratch: RenderScratch::new()?,
            dirty: true,
            last_bounds: None,
            rx: pty_bridge::spawn(id, rx),
            cached_title: String::new(),
        })
    }

    /// Sets this pane's default foreground and background from `theme`.
    pub fn apply_theme(&mut self, theme: &ghostwave::theme::Theme) {
        let to_rgb = |c: ghostwave::theme::Rgb| libghostty_vt::style::RgbColor {
            r: c.0,
            g: c.1,
            b: c.2,
        };
        let _ = self.term.term.set_default_fg_color(Some(to_rgb(theme.fg)));
        let _ = self.term.term.set_default_bg_color(Some(to_rgb(theme.bg)));
        self.term.force_repaint = true;
        self.dirty = true;
    }

    /// Writes pty output to the pane log first, then to the VT.
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

    /// Records that the child exited or the pty errored, reaping it for an exit code.
    pub fn exited(&mut self, reason: &str) {
        self.term.exited = Some(reason.to_string());
        self.term.last_exit_code = self.term.child.as_mut().and_then(|c| c.poll_reap());
        self.term.force_repaint = true;
        self.dirty = true;
    }

    /// Encodes and sends `kp` to the child, then flushes any VT response.
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

    /// Text input that is not a key press (IME commit, dead keys).
    #[allow(dead_code)]
    pub fn char_input(&mut self, c: char) {
        self.term.handle_char(c);
        self.term.flush_response();
        self.dirty = true;
    }

    /// Resizes the grid; a resize to the current size is a no-op.
    pub fn resize(&mut self, cols: u16, rows: u16, cell: CellDims) {
        let _ = self.term.set_grid(cols, rows, cell);
        self.dirty = true;
    }

    /// Scrolls the viewport by `lines` and leaves the pane in scrolled state.
    pub fn scroll(&mut self, lines: i32) {
        self.term.scroll(lines);
        self.dirty = true;
    }

    /// Scrolls the viewport by whole pages, a page being the grid's row count.
    pub fn scroll_page(&mut self, pages: i32) {
        self.term.scroll_page(pages);
        self.dirty = true;
    }

    pub fn scroll_half_page(&mut self, pages: i32) {
        self.term.scroll_half_page(pages);
        self.dirty = true;
    }

    pub fn scroll_to_bottom(&mut self) {
        self.term.scroll_to_bottom();
        self.dirty = true;
    }

    /// Resets the terminal, clearing the screen and the scrollback.
    pub fn clear_terminal(&mut self) -> Result<()> {
        let r = self.term.clear_terminal();
        self.dirty = true;
        r
    }

    /// Copies the active selection to the clipboard; `Ok(false)` when there is nothing
    /// to copy.
    pub fn copy_selection(&mut self) -> Result<bool> {
        self.term.copy_selection()
    }

    /// Pastes the clipboard into the pane.
    pub fn paste_clipboard(&mut self) {
        self.term.paste_clipboard();
        self.term.flush_response();
        self.dirty = true;
    }

    /// Whether the application in this pane tracks the mouse, which it asks for with a
    /// VT sequence.
    pub fn mouse_tracking(&self) -> bool {
        self.term.term.is_mouse_tracking().unwrap_or(false)
    }

    /// Handles a mouse event inside this pane: forwarded to the application when it
    /// tracks the mouse, otherwise it drives selection and viewport scrolling. `rect`
    /// and `ev.x`/`ev.y` are pane-local logical px.
    pub fn mouse_event(
        &mut self,
        rect: ghostwave::layout::Rect,
        cell: CellDims,
        padding: f32,
        ev: ghostwave::term::MouseEv,
        mods: Mods,
    ) {
        let _ = self.term.handle_mouse(rect, cell, padding, ev, mods);
        self.term.flush_response();
        self.dirty = true;
    }

    /// Returns the foreground process command running in this pane, if detectable.
    pub fn foreground_process(&self) -> Option<String> {
        let pty = self.term.pty.as_ref()?;
        let fd = pty.as_raw_fd();
        if fd < 0 {
            return None;
        }
        let borrowed = unsafe { std::os::fd::BorrowedFd::borrow_raw(fd) };
        let fg_pid = nix::unistd::tcgetpgrp(borrowed).ok()?;
        if fg_pid.as_raw() <= 0 {
            return None;
        }
        let shell_pid = self.term.child.as_ref().and_then(|c| c.pid());
        if shell_pid.is_some_and(|sp| sp == fg_pid) {
            return None;
        }

        let comm_path = format!("/proc/{}/comm", fg_pid.as_raw());
        let comm = std::fs::read_to_string(&comm_path).ok()?;
        let comm = comm.trim();
        if comm.is_empty() {
            None
        } else {
            Some(comm.to_string())
        }
    }

    /// Detects current working directory of this pane, with tilde replacement for $HOME.
    pub fn current_dir(&self) -> Option<String> {
        let osc_pwd = self.term.pwd();
        let path = if !osc_pwd.is_empty() {
            Some(osc_pwd)
        } else if let Some(pid) = self.term.child.as_ref().and_then(|c| c.pid()) {
            std::fs::read_link(format!("/proc/{}/cwd", pid.as_raw()))
                .ok()
                .map(|p| p.to_string_lossy().to_string())
        } else {
            None
        }?;

        Some(shorten_home(&path))
    }

    /// Returns the cached dynamic title for this pane.
    pub fn cached_title(&self) -> &str {
        &self.cached_title
    }

    /// Recomputes and caches the dynamic title for this pane.
    pub fn update_title(&mut self, mode: DynamicTitleMode, format: PaneTitleFormat) {
        if mode == DynamicTitleMode::Off {
            self.cached_title.clear();
            return;
        }

        let osc_title = self.term.title();
        let fg_cmd = if mode == DynamicTitleMode::Process {
            self.foreground_process()
        } else {
            None
        };
        let dir = self.current_dir();

        self.cached_title = match format {
            PaneTitleFormat::Process => {
                if let Some(cmd) = fg_cmd {
                    cmd
                } else if !osc_title.is_empty() {
                    osc_title
                } else {
                    dir.unwrap_or_default()
                }
            }
            PaneTitleFormat::Pwd => {
                dir.unwrap_or_else(|| {
                    if let Some(cmd) = fg_cmd {
                        cmd
                    } else {
                        osc_title
                    }
                })
            }
            PaneTitleFormat::Both => match (fg_cmd, dir) {
                (Some(cmd), Some(d)) => format!("{cmd} ({d})"),
                (Some(cmd), None) => cmd,
                (None, Some(d)) => {
                    if !osc_title.is_empty() && osc_title != d {
                        format!("{osc_title} — {d}")
                    } else {
                        d
                    }
                }
                (None, None) => osc_title,
            },
        };
    }

    /// Dynamic title computed on demand; prefer [`Self::cached_title`] in rendering paths.
    #[allow(dead_code)]
    pub fn display_title(&self) -> String {
        if !self.cached_title.is_empty() {
            return self.cached_title.clone();
        }
        let osc_title = self.term.title();
        let fg_cmd = self.foreground_process();
        let dir = self.current_dir();

        match (fg_cmd, dir) {
            (Some(cmd), Some(d)) => format!("{cmd} ({d})"),
            (Some(cmd), None) => cmd,
            (None, Some(d)) => {
                if !osc_title.is_empty() && osc_title != d {
                    format!("{osc_title} — {d}")
                } else {
                    d
                }
            }
            (None, None) => osc_title,
        }
    }
}

/// Condenses a path starting with `$HOME` into `~/...`.
pub fn shorten_home(path: &str) -> String {
    if let Ok(home) = std::env::var("HOME") {
        if path == home {
            return "~".to_string();
        } else if let Some(rel) = path.strip_prefix(&home) {
            if rel.starts_with('/') {
                return format!("~{rel}");
            }
        }
    }
    path.to_string()
}

/// Cell metrics from the embedded font: `cell_w = ceil('M' advance)`,
/// `cell_h = ceil(ascent + descent + row_gap)`, `baseline = ascent + row_gap / 2`.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_shorten_home() {
        if let Ok(home) = std::env::var("HOME") {
            assert_eq!(shorten_home(&home), "~");
            assert_eq!(shorten_home(&format!("{home}/foo/bar")), "~/foo/bar");
            assert_eq!(shorten_home("/tmp/foo"), "/tmp/foo");
        }
    }
}
