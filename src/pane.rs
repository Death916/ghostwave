//! iced-side pane wrapper: `ghostwave::term::TermPane` in a lockable home, because
//! `canvas::Program::draw` takes `&self` while drawing mutates render state, plus
//! libghostty's per-draw render scratch.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use ghostwave::config::AiConfig;
use ghostwave::keymap::KeyPress;
use ghostwave::term::{CellDims, ReconnectState, RemoteSpec, TermPane};
use libghostty_vt::key::{self, Mods};
use libghostty_vt::render::{CellIterator, RenderState, RowIterator};

use crate::ai::AiPane;
use crate::ui_config::{DynamicTitleMode, PaneTitleFormat};
use crate::preview::PreviewPane;
use crate::pty_bridge::{self, PtyRxSlot};
use crate::visualizer::VisualizerPane;

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

/// What every live pane was spawned with, kept outside the pane's own lock.
///
/// The session format saves a pane's program, arguments, cwd, remote target and
/// log name. Those never change after the pane is built, so they are recorded
/// here at construction instead of being read back through the pane's mutex:
/// that lock is also taken by the pty path, and a session save must never wait
/// on it.
static PANE_META: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<PaneId, ghostwave::session::PaneState>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

/// Records what a pane was spawned with, for the session format to read later.
fn register_meta(id: PaneId, meta: ghostwave::session::PaneState) {
    if let Ok(mut map) = PANE_META.lock() {
        map.insert(id, meta);
    }
}

/// Forgets a pane's recorded metadata once the pane is gone.
pub fn forget_meta(id: PaneId) {
    if let Ok(mut map) = PANE_META.lock() {
        map.remove(&id);
    }
}

/// Runs `f` over the recorded metadata of `id`, without taking the pane's lock.
pub fn with_pane_meta<R>(id: PaneId, f: impl FnOnce(&ghostwave::session::PaneState) -> R) -> Option<R> {
    PANE_META.lock().ok()?.get(&id).map(f)
}

/// Wraps a built terminal as an iced pane: records what it was spawned with for
/// the session format, and bridges its pty — or an idle slot when it has no child
/// yet, which is how a restored remote pane waits for its reconnect.
fn into_pane(mut term: TermPane) -> Result<Pane> {
    let id = PaneId::next();
    register_meta(
        id,
        ghostwave::session::PaneState {
            program: term.program.clone(),
            args: term.args.clone(),
            cwd: term.cwd.clone(),
            remote: term.remote.clone(),
            log: term.log_name.clone(),
        },
    );
    let rx = match term.pty_rx.take() {
        Some(rx) => pty_bridge::spawn(id, rx),
        None => pty_bridge::idle(id),
    };
    Ok(Pane {
        id,
        term,
        scratch: RenderScratch::new()?,
        dirty: true,
        last_bounds: None,
        rx,
        cached_title: String::new(),
    })
}

impl Pane {
    /// Forks a pty running the user's login shell and starts its reader thread,
    /// priming the shell's startup terminal probes; the pty receiver moves to a bridge
    /// thread that turns its output into an iced event ([`crate::pty_bridge`]).
    ///
    /// `cwd` is the directory the shell starts in; `None` inherits the app's own.
    ///
    /// `GHOST_ICED_FIXTURE` runs `/bin/sh -c <cmd>` in place of a shell.
    pub fn spawn(
        cols: u16,
        rows: u16,
        cell: CellDims,
        scrollback_lines: usize,
        log_cap_bytes: u64,
        cwd: Option<&str>,
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

        let term = TermPane::new(
            cols,
            rows,
            cell,
            program,
            &args,
            cwd,
            scrollback_lines,
            log_cap_bytes,
            true,
            None,
        )?;
        assert!(
            term.pty_rx.is_some(),
            "pty reader receiver (TermPane::new with spawn = true)"
        );
        into_pane(term)
    }

    /// Forks a pty running `program` with `args` — the path a remote pane takes,
    /// where the child is `ssh` and `remote` is the ghostd session it binds to.
    ///
    /// `remote` is recorded for the session format, so the pane comes back
    /// disconnected rather than run at restore (see [`Self::restore_remote`]).
    pub fn spawn_program(
        cols: u16,
        rows: u16,
        cell: CellDims,
        scrollback_lines: usize,
        log_cap_bytes: u64,
        program: Option<&str>,
        args: &[&str],
        cwd: Option<&str>,
        remote: Option<RemoteSpec>,
    ) -> Result<Self> {
        let term = TermPane::new(
            cols,
            rows,
            cell,
            program,
            args,
            cwd,
            scrollback_lines,
            log_cap_bytes,
            true,
            remote,
        )?;
        into_pane(term)
    }

    /// Restores a pane from a saved session: its scrollback log replayed, then a
    /// fresh shell over it.
    ///
    /// The order matters and mirrors the reference: the terminal and its history
    /// are built *before* the child starts. A shell started first would sit with
    /// its startup terminal probes in the pty buffer while the replay ran, and
    /// the answers arriving after its read timeout would be echoed back as
    /// caret-escaped garbage. `log` names the saved log to replay; the replayed
    /// bytes seed this pane's own new log, so the history outlives a second
    /// restart.
    #[allow(clippy::too_many_arguments)]
    pub fn restore(
        cols: u16,
        rows: u16,
        cell: CellDims,
        scrollback_lines: usize,
        log_cap_bytes: u64,
        program: Option<&str>,
        args: &[&str],
        cwd: Option<&str>,
        log: &str,
    ) -> Result<Self> {
        let mut term = TermPane::new(
            cols,
            rows,
            cell,
            program,
            args,
            cwd,
            scrollback_lines,
            log_cap_bytes,
            false,
            None,
        )?;
        let seeded = ghostwave::session::replay_pane_log(log, &mut term.term, log_cap_bytes);
        term.log_seed(&seeded);
        term.spawn_child()?;
        into_pane(term)
    }

    /// Restores a pane whose saved binding is a durable remote session: the
    /// history is replayed over a terminal with NO child process, and the pane is
    /// left DISCONNECTED for the user to connect with Enter (or for the
    /// auto-reconnect loop to pick up).
    ///
    /// A remote pane is never connected at restore, which is the reference's
    /// rule: coming back to a workspace must not silently re-open an ssh
    /// connection to every host it remembers.
    #[allow(clippy::too_many_arguments)]
    pub fn restore_remote(
        cols: u16,
        rows: u16,
        cell: CellDims,
        scrollback_lines: usize,
        log_cap_bytes: u64,
        program: Option<&str>,
        args: &[&str],
        cwd: Option<&str>,
        log: &str,
        remote: RemoteSpec,
    ) -> Result<Self> {
        let mut term = TermPane::new(
            cols,
            rows,
            cell,
            program,
            args,
            cwd,
            scrollback_lines,
            log_cap_bytes,
            false,
            Some(remote),
        )?;
        let seeded = ghostwave::session::replay_pane_log(log, &mut term.term, log_cap_bytes);
        term.log_seed(&seeded);
        term.exited = Some("disconnected (remote session)".to_string());
        term.last_exit_code = Some(1);
        term.reconnect = ReconnectState::Manual;
        into_pane(term)
    }

    /// Reconnects a disconnected remote pane: forks its stored command again and
    /// bridges the new pty, leaving the terminal and its history alone.
    ///
    /// False when the fork failed; the pane is then still disconnected, with the
    /// reconnect state the caller decides.
    pub fn respawn_remote(&mut self) -> bool {
        if !self.term.respawn() {
            return false;
        }
        if let Some(rx) = self.term.pty_rx.take() {
            self.rx = pty_bridge::spawn(self.id, rx);
        }
        self.dirty = true;
        true
    }

    /// The host and daemon session this pane is bound to; `None` for a pane that
    /// is not a remote pane.
    pub fn remote_spec(&self) -> Option<(String, String)> {
        self.term
            .remote
            .as_ref()
            .map(|remote| (remote.host.clone(), remote.session.clone()))
    }

    /// Whether this pane is a remote pane with no live connection.
    pub fn remote_disconnected(&self) -> bool {
        self.term.remote.is_some() && self.term.exited.is_some()
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

    /// Writes pty output to the pane log first, then to the VT. Answers a font
    /// query in the same bytes on the way through — the iced path has no other
    /// response flush, so the reply is written back here.
    pub fn ingest(&mut self, data: &[u8]) {
        if data.is_empty() {
            return;
        }
        if let Some(log) = self.term.log.as_mut() {
            log.write(data);
        }
        self.term.scan_queries(data);
        self.term.term.vt_write(data);
        self.term.flush_response();
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

    /// Recomputes this pane's search matches for the current viewport.
    ///
    /// The core runs this pass inside its own render function, which this
    /// front-end does not call, so the same pass runs here.
    pub fn refresh_search(&mut self) {
        if !self.term.search.active() || !self.term.search.dirty {
            return;
        }
        let Pane { term, scratch, .. } = self;
        let ghostwave::term::TermPane {
            search, term: vt, ..
        } = term;
        let Ok(snapshot) = scratch.render_state.update(vt) else {
            return;
        };
        let Ok(mut rows) = scratch.row_it.update(&snapshot) else {
            return;
        };
        search.matches.clear();
        if search.run_on_rows(&mut rows).is_ok() {
            search.dirty = false;
        }
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

/// A tab slot's occupant: the pane a `Tab::panes` index holds.
pub enum PaneEntry {
    /// A pty-backed terminal pane.
    Term(SharedPane),
    /// A file preview, AI chat or visualizer pane, which has no pty.
    Content(Box<ContentPane>),
}

impl PaneEntry {
    /// This entry's identity, which the grid payload and every id-tagged
    /// message address it by.
    pub fn id(&self) -> PaneId {
        match self {
            Self::Term(pane) => pane.lock().unwrap().id,
            Self::Content(content) => content.id(),
        }
    }

    /// This entry's terminal handle; `None` for a content pane. Every
    /// terminal-only path — the pty subscription, key send, resize, titles, the
    /// mouse-tracking scan and the sync broadcast — goes through this.
    pub fn term(&self) -> Option<&SharedPane> {
        match self {
            Self::Term(pane) => Some(pane),
            Self::Content(_) => None,
        }
    }

    /// Whether this entry is a terminal.
    pub fn is_term(&self) -> bool {
        matches!(self, Self::Term(_))
    }

    /// The content pane this entry holds, if it holds one.
    pub fn content(&self) -> Option<&ContentPane> {
        match self {
            Self::Term(_) => None,
            Self::Content(content) => Some(content),
        }
    }

    /// The content pane this entry holds, if it holds one.
    pub fn content_mut(&mut self) -> Option<&mut ContentPane> {
        match self {
            Self::Term(_) => None,
            Self::Content(content) => Some(content),
        }
    }
}

/// A cell's non-terminal pane: its own widget and state, plus the identity the
/// grid payload and the message routing address it by.
///
/// A content pane's id comes from the same counter the terminals draw on, so the
/// grid's payload and its cells are unchanged by the kind of pane they hold.
pub enum ContentPane {
    /// A file or directory preview.
    Preview {
        /// The pane's process-wide identity.
        id: PaneId,
        /// The previewed target's state and widget.
        pane: PreviewPane,
    },
    /// An AI assistant chat.
    Ai {
        /// The pane's process-wide identity.
        id: PaneId,
        /// The chat's state and widget.
        pane: AiPane,
    },
    /// The synthwave visualizer.
    Visualizer {
        /// The pane's process-wide identity.
        id: PaneId,
        /// The visualizer's state, which is also its canvas program.
        pane: VisualizerPane,
    },
}

impl ContentPane {
    /// A preview pane for `path`, loaded from disk.
    pub fn preview(path: impl Into<PathBuf>) -> Self {
        Self::Preview {
            id: PaneId::next(),
            pane: PreviewPane::open(path),
        }
    }

    /// An AI chat pane configured from `config`.
    pub fn ai(config: AiConfig) -> Self {
        Self::Ai {
            id: PaneId::next(),
            pane: AiPane::new(config),
        }
    }

    /// The synthwave visualizer pane.
    pub fn visualizer() -> Self {
        Self::Visualizer {
            id: PaneId::next(),
            pane: VisualizerPane::new(),
        }
    }

    /// This pane's identity, from the counter the terminals draw on.
    pub fn id(&self) -> PaneId {
        match self {
            Self::Preview { id, .. } | Self::Ai { id, .. } | Self::Visualizer { id, .. } => *id,
        }
    }

    /// The pane's kind: `preview`, `ai` or `visualizer`.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Preview { .. } => "preview",
            Self::Ai { .. } => "ai",
            Self::Visualizer { .. } => "visualizer",
        }
    }

    /// The pane's label-band text: its kind, and for a preview the target's
    /// own name.
    pub fn label(&self) -> String {
        match self {
            Self::Preview { pane, .. } => {
                let name = pane
                    .path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| pane.path.display().to_string());
                format!("preview {name}")
            }
            Self::Ai { .. } => "ai".to_string(),
            Self::Visualizer { .. } => "ghostwave".to_string(),
        }
    }

    /// The preview pane, when this is one.
    pub fn as_preview(&self) -> Option<&PreviewPane> {
        match self {
            Self::Preview { pane, .. } => Some(pane),
            _ => None,
        }
    }

    /// The preview pane, when this is one.
    pub fn as_preview_mut(&mut self) -> Option<&mut PreviewPane> {
        match self {
            Self::Preview { pane, .. } => Some(pane),
            _ => None,
        }
    }

    /// The AI chat pane, when this is one.
    pub fn as_ai(&self) -> Option<&AiPane> {
        match self {
            Self::Ai { pane, .. } => Some(pane),
            _ => None,
        }
    }

    /// The AI chat pane, when this is one.
    pub fn as_ai_mut(&mut self) -> Option<&mut AiPane> {
        match self {
            Self::Ai { pane, .. } => Some(pane),
            _ => None,
        }
    }

    /// The visualizer pane, when this is one.
    pub fn as_visualizer(&self) -> Option<&VisualizerPane> {
        match self {
            Self::Visualizer { pane, .. } => Some(pane),
            _ => None,
        }
    }

    /// The visualizer pane, when this is one.
    pub fn as_visualizer_mut(&mut self) -> Option<&mut VisualizerPane> {
        match self {
            Self::Visualizer { pane, .. } => Some(pane),
            _ => None,
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

/// Cell metrics from the embedded font: `cell_w = 'M' advance`,
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
        w: (font.metrics('M', font_px).advance_width).max(1.0),
        h: (ascent + descent + row_gap).max(8.0).ceil(),
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
