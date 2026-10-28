//! Terminal pane: libghostty-vt terminal + rendering + input + selection.
//! Adapted from ghostling-rs (https://github.com/Uzaaft/libghostty-rs).

use serde::{Deserialize, Serialize};

use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

use libghostty_vt::{
    Terminal, TerminalOptions,
    Error as VtError,
    alloc::Bytes,
    key::{self, Key, Mods},
    kitty::graphics::{self},
    mouse,
    render::{CellIterator, RenderState, RowIterator},
    screen::Screen,
    selection::gesture::{DragEvent, Geometry, Gesture, PressEvent, ReleaseEvent},
    terminal::{
        CompressionActivity, CompressionMode, CompressionResult, ConformanceLevel,
        DeviceAttributeFeature, DeviceAttributes, DeviceType, Mode, Point, PointCoordinate,
        PrimaryDeviceAttributes, ScrollViewport, SecondaryDeviceAttributes, SizeReportSize,
    },
    selection::FormatOptions,
};


use crate::{
    clipboard,
    layout::Rect,
    pty::{Child, Pty, WinSize},
    search::SearchState,
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Shared state between the terminal and its effect callbacks.
#[derive(Default)]
struct Shared {
    title: Mutex<String>,
    pwd: Mutex<String>,
    bell: AtomicBool,
    size: Mutex<(u16, u16, u32, u32)>,
    /// Current pty master fd for terminal effect callbacks (VT queries).
    /// Updated on respawn (reconnect) so responses go to the new pty.
    effect_fd: Mutex<i32>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CellDims {
    pub w: f32,
    pub h: f32,
    /// Baseline offset within the cell: glyph baseline y = cell_top + baseline.
    /// Keeps glyphs and the block cursor aligned in the same cell.
    pub baseline: f32,
}

/// Session-name prefix of a remote ghostd session: `ghostwave-<host token>`.
pub const REMOTE_SESSION_PREFIX: &str = "ghostwave-";

/// Longest ssh destination accepted, so a session file cannot feed an unbounded
/// token into a command line.
pub const HOST_MAX_LEN: usize = 255;

/// Whether `host` is a usable ssh destination.
///
/// The accepted characters are exactly what the supported forms need — an alias,
/// `user@host`, `host:port`, `[v6]:port`, `user@[v6]:port` and
/// `ssh://user@host:port` — so a destination can be handed to `ssh` as an argv
/// element and would still be plain data if it ever reached a shell. Whitespace,
/// control characters, quotes and shell metacharacters are all refused at this
/// boundary; the port (`%` for a zone id) is the only punctuation beyond the URI
/// set.
pub fn host_is_valid(host: &str) -> bool {
    !host.is_empty()
        && host.len() <= HOST_MAX_LEN
        && host.chars().all(|c| {
            c.is_ascii_alphanumeric()
                || matches!(c, '.' | '-' | '_' | '@' | ':' | '/' | '[' | ']' | '%')
        })
}

/// The host as a filesystem-safe token, for session names.
pub fn sanitize_host(host: &str) -> String {
    host.chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '.' || *c == '-' || *c == '_')
        .collect()
}

/// The ghostd session a durable pane binds to on `host`: `ghostwave-<host>`.
///
/// This is the only source of a session name — a name stored in a session file
/// is rederived from its host ([`RemoteSpec::rederived`]) rather than trusted.
pub fn remote_session(host: &str) -> String {
    format!("{REMOTE_SESSION_PREFIX}{}", sanitize_host(host))
}

/// A durable remote session binding (ghostd). Saved with the session so
/// restore can re-show the pane in a disconnected state.
///
/// `session` is derived data: it is serialized for readability and backwards
/// compatibility, but deserialization rederives it from `host`, so a crafted
/// session file cannot choose the name that later reaches a remote command line
/// or a `pkill` pattern.
#[derive(Serialize, Clone, Debug)]
pub struct RemoteSpec {
    pub host: String,
    /// Session name on the remote host; always [`remote_session`] of `host`.
    pub session: String,
}

impl RemoteSpec {
    /// The binding for `host`, with the session name derived from it.
    pub fn for_host(host: &str) -> Self {
        Self {
            host: host.to_string(),
            session: remote_session(host),
        }
    }

    /// This binding with the session name rederived from its host.
    ///
    /// Used wherever a binding comes from outside the process — a session file,
    /// a hand-edited config — so nothing downstream has to trust a stored name.
    pub fn rederived(&self) -> Self {
        Self::for_host(&self.host)
    }
}

impl<'de> Deserialize<'de> for RemoteSpec {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Stored {
            host: String,
            /// Accepted from the file format, then ignored: the name is derived.
            #[serde(default)]
            #[allow(dead_code)]
            session: String,
        }
        let stored = Stored::deserialize(deserializer)?;
        Ok(RemoteSpec::for_host(&stored.host))
    }
}

/// Reconnect state machine for remote (ssh/ghostd) panes.
#[derive(Clone, Debug, PartialEq)]
pub enum ReconnectState {
    /// Connected or nothing to do.
    Idle,
    /// Auto-reconnecting: attempt N, retry not before `next`.
    Waiting { attempt: u32, next: std::time::Instant },
    /// Auto-reconnect exhausted/failed; Enter connects manually.
    Manual,
}

/// A single terminal pane (one shell session).
pub struct TermPane {
    pub term: Terminal<'static, 'static>,
    render_state: RenderState<'static>,
    row_it: RowIterator<'static>,
    cell_it: CellIterator<'static>,
    key_encoder: key::Encoder<'static>,
    key_event: key::Event<'static>,
    mouse_encoder: mouse::Encoder<'static>,
    mouse_event: mouse::Event<'static>,
    pub response: Vec<u8>,
    /// Scanner for inbound font queries (`OSC 50`); its replies land in
    /// `response` and leave with `flush_response`.
    font_query: crate::font_query::Scanner,
    /// The family a font query is answered with, captured at construction from
    /// `ui_fonts::active_family()` so a pane answers what the app is actually
    /// configured to use.
    pub font_family: String,
    selection: SelectionState<'static>,
    /// None when the pane is in a disconnected state (no child process).
    pub pty: Option<Pty>,
    pub pty_rx: Option<std::sync::mpsc::Receiver<crate::pty::PtyMsg>>,
    pub child: Option<Child>,
    shared: Arc<Shared>,
    effect_fd: i32,
    pub name: String,
    pub cols: u16,
    pub rows: u16,
    pub search: SearchState,
    /// Process exit message, if the shell exited.
    pub exited: Option<String>,
    /// Force full repaint next frame (selection/search/scroll changes).
    pub force_repaint: bool,
    last_mouse: (f32, f32),
    scroll_active: bool,
    /// Spawn info (for session restore).
    pub program: Option<String>,
    pub args: Vec<String>,
    pub cwd: Option<String>,
    /// Durable remote session binding, if this pane is an ssh+ghostd pane.
    pub remote: Option<RemoteSpec>,
    /// Exit code of the last child (None until reaped).
    pub last_exit_code: Option<i32>,
    /// Reconnect state (remote panes only).
    pub reconnect: ReconnectState,
    /// Consecutive failed connect attempts (reset on manual connect).
    pub reconnect_failures: u32,
    /// Pane output log for durable sessions (managed asynchronously).
    pub log_name: String,
    pub log: Option<crate::session::AsyncPaneLog>,
    /// Optional cursor visual style override (from config).
    pub cursor_style: Option<libghostty_vt::render::CursorVisualStyle>,
    /// Idle scrollback-compression bookkeeping; see [`TermPane::compress_idle_step`].
    compression: IdleCompression,
}

/// How long a terminal must sit unchanged before its scrollback is compressed.
const COMPRESSION_IDLE_DELAY: Duration = Duration::from_secs(2);

/// Schedule state for one terminal's idle scrollback compression.
///
/// The library's activity token is opaque: only equality is meaningful, and it
/// may wrap, so nothing here orders two tokens.
struct IdleCompression {
    /// The token the idle delay is measured from; `None` until first observed.
    token: Option<CompressionActivity>,
    /// When `token` last changed — the start of the idle delay.
    changed_at: Instant,
    /// The token a pass is already complete for: nothing to do until it moves.
    complete_for: Option<CompressionActivity>,
    /// The target told us reclamation is unavailable: stop asking.
    unsupported: bool,
}

impl IdleCompression {
    fn new() -> Self {
        Self {
            token: None,
            changed_at: Instant::now(),
            complete_for: None,
            unsupported: false,
        }
    }
}

impl TermPane {
    pub fn new(
        cols: u16,
        rows: u16,
        cell: CellDims,
        shell: Option<&str>,
        args: &[&str],
        cwd: Option<&str>,
        scrollback: usize,
        log_cap_bytes: u64,
        spawn: bool,
        remote: Option<RemoteSpec>,
    ) -> Result<Self> {
        let shared = Arc::new(Shared {
            size: Mutex::new((cols, rows, cell.w.round() as u32, cell.h.round() as u32)),
            effect_fd: Mutex::new(-1),
            ..Default::default()
        });

        // Ghostty's VT engine measures max_scrollback in BYTES, not lines.
        // Convert line count (< 1,000,000) to bytes (~1024 bytes/line, min 16MB),
        // or accept explicit byte budget if >= 1,000,000.
        let max_scrollback = if scrollback == 0 {
            0
        } else if scrollback < 1_000_000 {
            (scrollback * 1024).max(16 * 1024 * 1024)
        } else {
            scrollback
        };

        let mut term = Terminal::new(TerminalOptions {
            cols,
            rows,
            max_scrollback,
        })?;

        // A remote pane may be created disconnected (restore) — no child
        // process until the user connects or the reconnect loop fires.
        let (pty, child, pty_rx) = if spawn {
            let (p, c) = Pty::new(
                WinSize {
                    cols,
                    rows,
                    xpixel: (cols as f32 * cell.w).round() as u16,
                    ypixel: (rows as f32 * cell.h).round() as u16,
                },
                shell,
                args,
                cwd,
            )?;
            let rx = p.spawn_reader().ok();
            (Some(p), Some(c), rx)
        } else {
            (None, None, None)
        };
        // Pane output log for durable sessions (asynchronously written and truncated).
        let log = crate::session::AsyncPaneLog::open(log_cap_bytes);
        let log_name = log.as_ref().map(|l| l.name.clone()).unwrap_or_default();

        let effect_fd = match &pty {
            Some(p) => p.dup_raw().unwrap_or(-1),
            None => -1,
        };
        *shared.effect_fd.lock().unwrap() = effect_fd;
        let shared_effects = shared.clone();

        term.resize(cols, rows, cell.w.round() as u32, cell.h.round() as u32)?;
        term
            .set_kitty_image_storage_limit(64 * 1024 * 1024)?
            .set_kitty_image_from_file_allowed(true)?
            .set_kitty_image_from_temp_file_allowed(true)?
            .set_kitty_image_from_shared_mem_allowed(true)?;

        term
            .on_pty_write({
                let shared = shared_effects.clone();
                move |_t, data| {
                    let fd = *shared.effect_fd.lock().unwrap();
                    if fd >= 0 {
                        write_raw(fd, data);
                    }
                }
            })?
            .on_size({
                let shared = shared_effects.clone();
                move |_term| {
                    let (cols, rows, cw, ch) = *shared.size.lock().unwrap();
                    Some(SizeReportSize {
                        rows,
                        columns: cols,
                        cell_width: cw,
                        cell_height: ch,
                    })
                }
            })?
            .on_device_attributes(|_term| {
                Some(DeviceAttributes {
                    primary: PrimaryDeviceAttributes::new(
                        ConformanceLevel::VT220,
                        &[
                            DeviceAttributeFeature::COLUMNS_132,
                            DeviceAttributeFeature::SELECTIVE_ERASE,
                            DeviceAttributeFeature::ANSI_COLOR,
                        ],
                    ),
                    secondary: SecondaryDeviceAttributes {
                        device_type: DeviceType::VT220,
                        firmware_version: 1,
                        rom_cartridge: 0,
                    },
                    tertiary: Default::default(),
                })
            })?
            .on_xtversion(|_term| Some("ghostwave"))?
            .on_color_scheme(|_term| None)?
            .on_title_changed({
                let shared = shared_effects.clone();
                move |term| {
                    *shared.title.lock().unwrap() = term.title().unwrap_or("").to_string();
                }
            })?
            .on_pwd_changed({
                let shared = shared_effects.clone();
                move |term| {
                    *shared.pwd.lock().unwrap() = term.pwd().unwrap_or("").to_string();
                }
            })?
            .on_bell({
                let shared = shared_effects.clone();
                move |_term| {
                    shared.bell.store(true, Ordering::Relaxed);
                }
            })?
            .on_clipboard_write(|_term, clip| {
                for content in clip.contents() {
                    if content.mime.contains("text") || content.mime == "application/x-uri-list" {
                        clipboard::write_text(content.data);
                        break;
                    }
                }
                Ok(())
            })?;

        let key_encoder = key::Encoder::new()?;
        let key_event = key::Event::new()?;
        let mouse_encoder = mouse::Encoder::new()?;
        let mouse_event = mouse::Event::new()?;
        let render_state = RenderState::new()?;
        let row_it = RowIterator::new()?;
        let cell_it = CellIterator::new()?;

        Ok(Self {
            term,
            render_state,
            row_it,
            cell_it,
            key_encoder,
            key_event,
            mouse_encoder,
            mouse_event,
            response: Vec::with_capacity(64),
            font_query: crate::font_query::Scanner::new(),
            font_family: crate::ui_fonts::active_family().to_string(),
            selection: SelectionState::new()?,
            pty,
            pty_rx,
            child,
            shared,
            effect_fd,
            name: String::new(),
            cols,
            rows,
            search: SearchState::default(),
            exited: None,
            force_repaint: true,
            last_mouse: (0.0, 0.0),
            scroll_active: false,
            program: shell.map(String::from),
            args: args.iter().map(|s| s.to_string()).collect(),
            cwd: cwd.map(String::from),
            remote,
            last_exit_code: None,
            reconnect: ReconnectState::Idle,
            reconnect_failures: 0,
            log_name,
            log,
            cursor_style: None,
            compression: IdleCompression::new(),
        })
        .map(|mut pane| {
            if spawn {
                pane.prime_pty();
            }
            pane
        })
    }

    /// Offer the library one bounded scrollback-compression step.
    ///
    /// Returns whether a step ran. A step is offered only while the terminal's
    /// activity token is unchanged (i.e. nothing was written) for
    /// [`COMPRESSION_IDLE_DELAY`], and only once per token after a pass reports
    /// [`CompressionResult::Complete`]. `CompressionResult::Unsupported` — the
    /// target cannot reclaim retained mappings — is remembered, so the pane
    /// stops being asked.
    ///
    /// Compression rewrites storage only: it never changes the logical
    /// contents or the scrollback limit, so the caller needs no repaint. The
    /// library requires it to be serialized with every other operation on the
    /// terminal, which the pane's own lock provides — call this with that lock
    /// held, never per frame.
    pub fn compress_idle_step(&mut self) -> bool {
        if self.compression.unsupported {
            return false;
        }
        let Ok(token) = self.term.compression_activity() else {
            return false;
        };
        if self.compression.token != Some(token) {
            // New output since the last offer: restart the idle delay.
            self.compression.token = Some(token);
            self.compression.changed_at = Instant::now();
            return false;
        }
        if self.compression.complete_for == Some(token)
            || self.compression.changed_at.elapsed() < COMPRESSION_IDLE_DELAY
        {
            return false;
        }
        match self.term.compress(CompressionMode::Incremental) {
            Ok(CompressionResult::Pending) => true,
            Ok(CompressionResult::Complete) => {
                self.compression.complete_for = Some(token);
                true
            }
            Ok(CompressionResult::Unsupported) => {
                self.compression.unsupported = true;
                false
            }
            // A failed call is not the target saying "unsupported"; leave the
            // state alone and let the next offer retry.
            Err(_) => false,
        }
    }

    /// Parse a cursor style name string to `CursorVisualStyle`.
    pub fn parse_cursor_style(s: &str) -> Option<libghostty_vt::render::CursorVisualStyle> {
        match s.to_lowercase().trim() {
            "block" => Some(libghostty_vt::render::CursorVisualStyle::Block),
            "bar" | "beam" => Some(libghostty_vt::render::CursorVisualStyle::Bar),
            "underline" => Some(libghostty_vt::render::CursorVisualStyle::Underline),
            "hollow" | "block_hollow" => Some(libghostty_vt::render::CursorVisualStyle::BlockHollow),
            _ => None,
        }
    }

    /// Drain the child's initial output burst right away. Shells (fish 4+)
    /// probe the terminal at startup — kitty keyboard protocol, DA,
    /// XTVERSION, OSC 11 — and read the responses with a short timeout. If
    /// the app doesn't drain promptly (session-log replay at restore, slow
    /// first frames), the responses land AFTER the shell's read window and
    /// the line editor echoes them as caret-escaped garbage
    /// (`^[[?0u^[P>|ghostwave^[\…`). Draining synchronously right after
    /// spawn keeps the queries' responses inside the shell's timeout.
    ///
    /// Blocks for at most `PRIME_DEADLINE`. The burst is treated as over
    /// after `PRIME_IDLE` of silence, but that clock only starts once the
    /// child has actually written something: a child slow to emit its first
    /// byte (fork/exec of a shell on a loaded box) must not be mistaken for
    /// an idle one, or the drain returns before the output it exists to
    /// catch has even been written. Also returns early once the child is
    /// known to have exited.
    fn prime_pty(&mut self) {
        // 1s bound on blocking the caller (pane creation / session restore);
        // 60ms of silence with nothing more coming ends the burst.
        const PRIME_DEADLINE: std::time::Duration = std::time::Duration::from_millis(1000);
        const PRIME_IDLE: std::time::Duration = std::time::Duration::from_millis(60);

        let start = std::time::Instant::now();
        let mut saw_output = false;
        let mut last_output = std::time::Instant::now();
        while start.elapsed() < PRIME_DEADLINE {
            let active = self.poll_pty();
            self.flush_response();
            if active {
                saw_output = true;
                last_output = std::time::Instant::now();
                continue;
            }
            if self.exited.is_some() || (saw_output && last_output.elapsed() > PRIME_IDLE) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }

    pub fn title(&self) -> String {
        self.shared.title.lock().unwrap().clone()
    }

    pub fn pwd(&self) -> String {
        if let Some(child) = &self.child {
            if let Some(pid) = child.pid() {
                if let Ok(target) = std::fs::read_link(format!("/proc/{pid}/cwd")) {
                    let path = target.to_string_lossy().to_string();
                    if !path.is_empty() {
                        return path;
                    }
                }
            }
        }
        let shared = self.shared.pwd.lock().unwrap().clone();
        if !shared.is_empty() {
            return shared;
        }
        self.cwd.clone().unwrap_or_default()
    }

    pub fn set_grid(&mut self, cols: u16, rows: u16, cell: CellDims) -> Result<()> {
        if cols == self.cols && rows == self.rows {
            return Ok(());
        }
        self.cols = cols;
        self.rows = rows;
        *self.shared.size.lock().unwrap() = (cols, rows, cell.w.round() as u32, cell.h.round() as u32);
        self.term.resize(cols, rows, cell.w.round() as u32, cell.h.round() as u32)?;
        if let Some(pty) = self.pty.as_ref() {
            pty.resize(WinSize {
                cols,
                rows,
                xpixel: (cols as f32 * cell.w).round() as u16,
                ypixel: (rows as f32 * cell.h).round() as u16,
            });
        }
        self.force_repaint = true;
        Ok(())
    }

    /// Drain pty output into the terminal. Call every frame. Returns true
    /// when pty data was processed (used for event-driven frame pacing).
    pub fn poll_pty(&mut self) -> bool {
        if self.exited.is_some() {
            if let Some(child) = self.child.as_mut() {
                if let Some(code) = child.poll_reap() {
                    self.last_exit_code = Some(code);
                }
            }
            return false;
        }

        let Some(rx) = self.pty_rx.take() else {
            return false;
        };

        let mut had_activity = false;
        while let Ok(msg) = rx.try_recv() {
            match msg {
                crate::pty::PtyMsg::Data(data) => {
                    if !data.is_empty() {
                        if let Some(log) = self.log.as_mut() {
                            log.write(&data);
                        }
                        self.scan_queries(&data);
                        self.term.vt_write(&data);
                        had_activity = true;
                    }
                }
                crate::pty::PtyMsg::Eof => {
                    self.exited = Some("process exited".into());
                    self.force_repaint = true;
                    self.last_exit_code = self.child.as_mut().and_then(|c| c.poll_reap());
                }
                crate::pty::PtyMsg::Error(e) => {
                    self.exited = Some(format!("pty error: {e}"));
                    self.force_repaint = true;
                    self.last_exit_code = self.child.as_mut().and_then(|c| c.poll_reap());
                }
            }
        }
        self.pty_rx = Some(rx);
        had_activity
    }

    pub fn write_input(&mut self, data: &[u8]) {
        if let Some(pty) = self.pty.as_ref() {
            pty.write(data);
        }
    }

    /// Start the child process (or restart it). program=None → $SHELL.
    /// Resets the exited/reconnect state. No priming (reconnect must not
    /// block the main loop).
    fn start_child(&mut self) -> crate::Result<()> {
        let size = WinSize {
            cols: self.cols,
            rows: self.rows,
            xpixel: 0,
            ypixel: 0,
        };
        let args: Vec<&str> = self.args.iter().map(|s| s.as_str()).collect();
        let (pty, child) = Pty::new(size, self.program.as_deref(), &args, self.cwd.as_deref())?;
        self.pty_rx = pty.spawn_reader().ok();
        self.pty = Some(pty);
        self.child = Some(child);
        if let Some(p) = self.pty.as_ref() {
            let new_fd = p.dup_raw().unwrap_or(-1);
            let old = std::mem::replace(&mut *self.shared.effect_fd.lock().unwrap(), new_fd);
            if old >= 0 {
                unsafe { nix::libc::close(old) };
            }
            self.effect_fd = new_fd;
        }
        self.exited = None;
        self.last_exit_code = None;
        self.reconnect = ReconnectState::Idle;
        self.force_repaint = true;
        Ok(())
    }

    /// Like start_child, then prime the pty so the shell's startup terminal
    /// probes get answered inside its read timeout. Used by the session
    /// restore path (terminal + history replay FIRST, shell second).
    pub fn spawn_child(&mut self) -> crate::Result<()> {
        self.start_child()?;
        self.prime_pty();
        Ok(())
    }

    /// Re-spawn the child (reconnect). Reuses the stored program/args; the
    /// terminal state and log are untouched so history survives.
    pub fn respawn(&mut self) -> bool {
        self.start_child().is_ok()
    }

    /// Seed the session log with replayed history (durable restore) so the
    /// restored scrollback survives further restarts.
    pub fn log_seed(&mut self, data: &[u8]) {
        if let Some(log) = self.log.as_mut() {
            log.seed(data);
        }
    }

    /// Scans inbound pty bytes for a font query and queues the reply in
    /// `response`, which `flush_response` writes back to the pty. The bytes
    /// themselves are not consumed — the VT still receives them unchanged.
    pub fn scan_queries(&mut self, data: &[u8]) {
        let replies = self.font_query.feed(data, &self.font_family);
        self.response.extend_from_slice(&replies);
    }

    pub fn flush_response(&mut self) {
        if !self.response.is_empty() {
            if let Some(pty) = self.pty.as_ref() {
                pty.write(&self.response);
            }
            self.response.clear();
        }
    }

    // ---- input -----------------------------------------------------------

    pub fn handle_key(&mut self, key: Key, mods: Mods, consumed: Mods, unshifted: char, text: Option<&str>, action: key::Action) -> Result<()> {
        self.key_event
            .set_action(action)
            .set_key(key)
            .set_mods(mods)
            .set_consumed_mods(consumed)
            .set_unshifted_codepoint(unshifted)
            .set_utf8(text);
        self.key_encoder
            .set_options_from_terminal(&self.term)
            .encode_to_vec(&self.key_event, &mut self.response)?;
        self.scroll_active = false;
        Ok(())
    }

    pub fn handle_char(&mut self, c: char) {
        if c.is_control() {
            return;
        }
        let mut buf = [0u8; 4];
        self.response.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
        self.scroll_active = false;
    }

    /// Forward a mouse event to the application (if tracking) or update
    /// selection / viewport scrolling. `pos` is pane-local physical px.
    pub fn handle_mouse(
        &mut self,
        rect: Rect,
        cell: CellDims,
        padding: f32,
        ev: MouseEv,
        mods: Mods,
    ) -> Result<()> {
        self.mouse_event.set_mods(mods).set_position(mouse::Position {
            x: ev.x,
            y: ev.y,
        });
        self.mouse_encoder
            .set_options_from_terminal(&self.term)
            .set_size(mouse::EncoderSize {
                screen_width: rect.w as u32,
                screen_height: rect.h as u32,
                cell_width: cell.w.round() as u32,
                cell_height: cell.h.round() as u32,
                padding_top: padding as u32,
                padding_bottom: padding as u32,
                padding_left: padding as u32,
                padding_right: padding as u32,
            })
            .set_any_button_pressed(ev.any_pressed)
            .set_track_last_cell(true);

        match ev.kind {
            MouseEvKind::Press(btn) => {
                if btn == mouse::Button::Left && self.selection_allowed(mods)? {
                    self.selection.on_click(&self.term, ev.x, ev.y, rect, cell, padding)?;
                    self.force_repaint = true;
                    return Ok(());
                }
                self.mouse_event
                    .set_action(mouse::Action::Press)
                    .set_button(Some(btn));
                self.mouse_encoder
                    .encode_to_vec(&self.mouse_event, &mut self.response)?;
            }
            MouseEvKind::Release(btn) => {
                if btn == mouse::Button::Left && self.selection.is_selecting(&self.term)? {
                    self.selection.on_release(&self.term, ev.x, ev.y, rect, cell, padding)?;
                    let opts = FormatOptions::new().with_trim(true).with_unwrap(true);
                    if let Ok(Some(bytes)) = self.term.format_selection_alloc(None, opts) {
                        let text = String::from_utf8_lossy(bytes.as_ref()).into_owned();
                        if !text.is_empty() {
                            clipboard::write_primary_text(&text);
                        }
                    }
                    self.force_repaint = true;
                    return Ok(());
                }
                self.mouse_event
                    .set_action(mouse::Action::Release)
                    .set_button(Some(btn));
                self.mouse_encoder
                    .encode_to_vec(&self.mouse_event, &mut self.response)?;
            }
            MouseEvKind::Motion => {
                let moved = (ev.x - self.last_mouse.0).abs() > 0.5 || (ev.y - self.last_mouse.1).abs() > 0.5;
                // Only drag while the left button is physically held. ghostty's
                // gesture keeps left_click_count > 0 after release (for
                // double-click timing), so gating on click_count alone makes
                // any hover after a click extend the selection.
                let in_autoscroll = ev.y < padding || ev.y > rect.h - padding;
                if (moved || in_autoscroll)
                    && ev.button_down == Some(mouse::Button::Left)
                    && self.selection.is_selecting(&self.term)?
                {
                    let scrolled = self.selection.on_drag(
                        &mut self.term,
                        ev.x,
                        ev.y,
                        rect,
                        cell,
                        padding,
                        mods,
                    )?;
                    if scrolled {
                        self.scroll_active = true;
                    }
                    self.last_mouse = (ev.x, ev.y);
                    self.force_repaint = true;
                    return Ok(());
                }
                if moved {
                    self.mouse_event
                        .set_action(mouse::Action::Motion)
                        .set_button(ev.button_down);
                    self.mouse_encoder
                        .encode_to_vec(&self.mouse_event, &mut self.response)?;
                }
                self.last_mouse = (ev.x, ev.y);
            }
            MouseEvKind::Wheel(dy) => {
                if self.term.is_mouse_tracking()? {
                    let btn = if dy > 0.0 { mouse::Button::Four } else { mouse::Button::Five };
                    self.mouse_event.set_button(Some(btn)).set_action(mouse::Action::Press);
                    self.mouse_encoder.encode_to_vec(&self.mouse_event, &mut self.response)?;
                    self.mouse_event.set_action(mouse::Action::Release);
                    self.mouse_encoder.encode_to_vec(&self.mouse_event, &mut self.response)?;
                } else {
                    let delta: isize = (dy * -2.5) as isize;
                    self.term.scroll_viewport(ScrollViewport::Delta(delta));
                    self.scroll_active = true;
                    self.force_repaint = true;
                }
            }
        }
        Ok(())
    }

    fn selection_allowed(&self, mods: Mods) -> Result<bool> {
        let tracking = self.term.is_mouse_tracking()?;
        Ok(!tracking || mods.contains(Mods::SHIFT))
    }

    pub fn scroll(&mut self, lines: i32) {
        self.term.scroll_viewport(ScrollViewport::Delta(lines as isize));
        self.scroll_active = true;
        self.force_repaint = true;
    }

    pub fn scroll_page(&mut self, pages: i32) {
        let delta = self.rows as i32 * pages;
        let n = if pages < 0 {
            delta.min(-1)
        } else if pages > 0 {
            delta.max(1)
        } else {
            0
        };
        self.scroll(n);
    }

    pub fn scroll_half_page(&mut self, pages: i32) {
        let half = (self.rows as i32 / 2).max(1);
        self.scroll(half * pages);
    }

    pub fn scroll_to_bottom(&mut self) {
        self.term.scroll_viewport(ScrollViewport::Bottom);
        self.scroll_active = false;
        self.force_repaint = true;
    }

    /// Whether the pane is showing the primary screen, the one the scrollback
    /// belongs to.
    ///
    /// Full-screen programs (`less`, `vim`, `htop`) switch to the alternate
    /// screen while they run; the alternate screen keeps no history, so this is
    /// also the answer to "does the pane have scrollback to scroll right now".
    /// A failed read reports `false`: callers that take a key away from the
    /// program use this, and a program that owns the key is safer than one that
    /// silently loses it.
    pub fn on_primary_screen(&self) -> bool {
        matches!(self.term.active_screen(), Ok(Screen::Primary))
    }

    pub fn clear_terminal(&mut self) -> Result<()> {
        self.term.reset();
        self.force_repaint = true;
        Ok(())
    }

    // ---- selection / clipboard ------------------------------------------

    /// Copy the active selection.
    pub fn copy_selection(&mut self) -> Result<bool> {
        let opts = FormatOptions::new().with_trim(true).with_unwrap(true);
        if let Some(bytes) = self.term.format_selection_alloc(None, opts)? {
            let text = String::from_utf8_lossy(bytes.as_ref()).into_owned();
            if !text.is_empty() {
                self.term.set_selection(None)?;
                self.force_repaint = true;
                return Ok(clipboard::write_text(&text));
            }
        }
        Ok(false)
    }

    pub fn paste_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        let is_bracketed = self.term.mode(Mode::BRACKETED_PASTE).unwrap_or(false);
        let mut data = text.as_bytes().to_vec();
        let mut buf = vec![0u8; data.len() * 2 + 64];
        match libghostty_vt::paste::encode(&mut data, is_bracketed, &mut buf) {
            Ok(len) => {
                self.write_input(&buf[..len]);
            }
            Err(libghostty_vt::error::Error::OutOfSpace { required }) => {
                buf.resize(required, 0);
                if let Ok(len) = libghostty_vt::paste::encode(&mut data, is_bracketed, &mut buf) {
                    self.write_input(&buf[..len]);
                } else {
                    self.write_input(text.as_bytes());
                }
            }
            Err(_) => {
                self.write_input(text.as_bytes());
            }
        }
    }

    pub fn paste_clipboard(&mut self) {
        if let Some(text) = clipboard::read_text().or_else(clipboard::read_primary_text) {
            self.paste_text(&text);
        }
    }

    pub fn paste_primary(&mut self) {
        if let Some(text) = clipboard::read_primary_text().or_else(clipboard::read_text) {
            self.paste_text(&text);
        }
    }

    pub fn is_mouse_tracking(&self) -> bool {
        self.term.is_mouse_tracking().unwrap_or(false)
    }

    // ---- rendering -------------------------------------------------------

    /// Grid point for a pane-local pixel position (clamped).
    pub fn grid_point(&self, px: f32, py: f32, rect: Rect, cell: CellDims, padding: f32) -> Point {
        let x = ((px - rect.x - padding) / cell.w).clamp(0.0, self.cols as f32) as u16;
        let y = ((py - rect.y - padding) / cell.h).clamp(0.0, self.rows as f32) as u32;
        Point::Viewport(PointCoordinate { x, y })
    }

    /// Detect if the cell at (row_idx, col_idx) contains a clickable URL or file path.
    /// Returns (target_string, start_col, end_col) if found.
    ///
    /// An OSC 8 hyperlink on the cell wins: its target is the URI the program
    /// declared, which need not appear in the visible text at all (`ls
    /// --hyperlink`, links whose label is a word) and is authoritative across
    /// wide glyphs and grapheme clusters. Bare URLs and paths in the row text
    /// are the fallback, so they stay clickable.
    pub fn link_at_cell(&mut self, row_idx: u32, col_idx: u16) -> Option<(String, u16, u16)> {
        if let Some(hit) = self.osc8_link_at(row_idx, col_idx) {
            return Some(hit);
        }
        let snapshot = self.render_state.update(&self.term).ok()?;
        let mut row_it = self.row_it.update(&snapshot).ok()?;
        let mut cur_row = 0u32;
        while let Some(row) = row_it.next() {
            if cur_row == row_idx {
                let mut cell_it = self.cell_it.update(row).ok()?;
                let mut row_text = String::with_capacity(128);
                let mut char_to_col: Vec<u16> = Vec::with_capacity(128);
                let mut col = 0u16;
                let mut buf = String::with_capacity(8);

                while let Some(c) = cell_it.next() {
                    let graphemes = c.graphemes_len().unwrap_or(0);
                    if graphemes == 0 {
                        row_text.push(' ');
                        char_to_col.push(col);
                    } else {
                        buf.clear();
                        if c.graphemes_utf8(&mut buf).is_ok() {
                            buf.retain(|ch| !matches!(ch, '\u{FE00}'..='\u{FE0F}' | '\u{200B}'..='\u{200D}' | '\u{2060}' | '\u{FEFF}'));
                            let s = if buf.is_empty() { " " } else { &buf };
                            for ch in s.chars() {
                                row_text.push(ch);
                                char_to_col.push(col);
                            }
                        } else {
                            row_text.push(' ');
                            char_to_col.push(col);
                        }
                    }
                    col += 1;
                }

                // Scan row_text for URLs or file paths
                let mut start = 0;
                while start < row_text.len() {
                    let remaining = &row_text[start..];
                    if let Some(pos) = remaining.find(|c: char| !c.is_whitespace()) {
                        let token_start = start + pos;
                        let token_str = &row_text[token_start..];
                        let token_len = token_str.find(|c: char| c.is_whitespace() || c == '"' || c == '\'' || c == '`' || c == '<' || c == '>' || c == '(' || c == ')' || c == '[' || c == ']').unwrap_or(token_str.len());
                        let raw_token = &token_str[..token_len];
                        let clean = raw_token.trim_end_matches(|c: char| c == ',' || c == ';' || c == '.');

                        let is_url = clean.starts_with("http://") || clean.starts_with("https://");
                        let is_path = clean.contains('/') || (clean.contains('.') && !clean.ends_with('.'));

                        if (is_url || is_path) && clean.len() >= 3 {
                            let char_start = row_text[..token_start].chars().count();
                            let char_end = row_text[..token_start + clean.len()].chars().count();
                            let start_col = char_to_col.get(char_start).copied().unwrap_or(0);
                            let end_col = char_to_col.get(char_end.saturating_sub(1)).map(|&c| c + 1).unwrap_or(col);
                            if col_idx >= start_col && col_idx < end_col {
                                return Some((clean.to_string(), start_col, end_col));
                            }
                        }
                        start = token_start + token_len.max(1);
                    } else {
                        break;
                    }
                }
                break;
            }
            cur_row += 1;
        }
        None
    }

    /// The OSC 8 hyperlink covering a viewport cell, with the column range of
    /// the run of cells that shares it.
    fn osc8_link_at(&self, row: u32, col: u16) -> Option<(String, u16, u16)> {
        let uri = self.hyperlink_at(row, col)?;
        let mut start = col;
        while start > 0 && self.hyperlink_at(row, start - 1).as_deref() == Some(uri.as_str()) {
            start -= 1;
        }
        let mut end = col + 1;
        while end < self.cols && self.hyperlink_at(row, end).as_deref() == Some(uri.as_str()) {
            end += 1;
        }
        Some((uri, start, end))
    }

    /// The OSC 8 target a program declared for one viewport cell, if any.
    fn hyperlink_at(&self, row: u32, col: u16) -> Option<String> {
        let point = Point::Viewport(PointCoordinate { x: col, y: row });
        let grid_ref = self.term.grid_ref(point).ok()?;
        let mut buf = vec![0u8; 256];
        loop {
            match grid_ref.hyperlink_uri(&mut buf) {
                Ok(0) => return None,
                Ok(len) => return String::from_utf8(buf[..len].to_vec()).ok(),
                // The URI is longer than the buffer we guessed with; the
                // library reports the size it needs.
                Err(VtError::OutOfSpace { required }) if required > buf.len() => {
                    if required > 64 * 1024 {
                        return None;
                    }
                    buf.resize(required, 0);
                }
                Err(_) => return None,
            }
        }
    }
}

impl Drop for TermPane {
    fn drop(&mut self) {
        // Close the effect fd so no writes go to a dead pty.
        let old = std::mem::replace(&mut *self.shared.effect_fd.lock().unwrap(), -1);
        if old >= 0 {
            unsafe {
                nix::libc::close(old);
            }
        }
        self.effect_fd = -1;
    }
}

fn write_raw(fd: i32, data: &[u8]) {
    use std::os::fd::BorrowedFd;
    let fd = unsafe { BorrowedFd::borrow_raw(fd) };
    let mut remaining = data;
    while !remaining.is_empty() {
        match nix::unistd::write(fd, remaining) {
            Ok(len) => remaining = &remaining[len..],
            Err(nix::errno::Errno::EINTR) => continue,
            Err(_) => break,
        }
    }
}

// ---- mouse events --------------------------------------------------------

#[derive(Clone, Copy, Debug)]
pub enum MouseEvKind {
    Press(mouse::Button),
    Release(mouse::Button),
    Motion,
    Wheel(f32),
}

#[derive(Clone, Copy, Debug)]
pub struct MouseEv {
    pub kind: MouseEvKind,
    pub x: f32,
    pub y: f32,
    pub any_pressed: bool,
    pub button_down: Option<mouse::Button>,
}

// ---- selection -----------------------------------------------------------

struct SelectionState<'alloc> {
    gesture: Gesture<'alloc>,
    press_event: PressEvent<'alloc>,
    release_event: ReleaseEvent<'alloc>,
    drag_event: DragEvent<'alloc>,
    start_time: Instant,
    last_autoscroll: Option<Instant>,
}

impl SelectionState<'_> {
    fn new() -> Result<Self> {
        let mut press_event = PressEvent::new()?;
        press_event.set_repeat_interval(Duration::from_millis(500))?;
        Ok(Self {
            gesture: Gesture::new()?,
            press_event,
            release_event: ReleaseEvent::new()?,
            drag_event: DragEvent::new()?,
            start_time: Instant::now(),
            last_autoscroll: None,
        })
    }

    fn is_selecting(&self, terminal: &Terminal<'_, '_>) -> Result<bool> {
        Ok(self.gesture.click_count(terminal)? > 0)
    }

    /// Convert pane-local pointer pos (already relative to the pane rect,
    /// i.e. surface-space) to a viewport cell. Padding is the grid inset.
    fn point(&self, x: f32, y: f32, rect: Rect, cell: CellDims, padding: f32, cols: u16, rows: u16) -> Point {
        let _ = rect;
        let max_col = cols.saturating_sub(1);
        let max_row = rows.saturating_sub(1);
        let px = ((x - padding) / cell.w).clamp(0.0, max_col as f32) as u16;
        let py = ((y - padding) / cell.h).clamp(0.0, max_row as f32) as u32;
        Point::Viewport(PointCoordinate { x: px, y: py })
    }

    fn on_click(
        &mut self,
        terminal: &Terminal<'_, '_>,
        x: f32,
        y: f32,
        rect: Rect,
        cell: CellDims,
        padding: f32,
    ) -> Result<()> {
        self.last_autoscroll = None;
        let point = self.point(x, y, rect, cell, padding, terminal.cols()?, terminal.rows()?);
        let grid_ref = terminal.grid_ref(point)?;
        let selection = self
            .press_event
            .set_repeat_distance(cell.w.into())?
            .set_time(self.start_time.elapsed())?
            .set_position(x.into(), y.into())?
            .apply(&mut self.gesture, terminal, grid_ref)?;
        terminal.set_selection(selection.as_ref())?;
        Ok(())
    }

    fn on_release(
        &mut self,
        terminal: &Terminal<'_, '_>,
        x: f32,
        y: f32,
        rect: Rect,
        cell: CellDims,
        padding: f32,
    ) -> Result<()> {
        self.last_autoscroll = None;
        let point = self.point(x, y, rect, cell, padding, terminal.cols()?, terminal.rows()?);
        let grid_ref = terminal.grid_ref(point).ok();
        self.release_event.apply(&mut self.gesture, terminal, grid_ref)?;
        Ok(())
    }

    fn on_drag(
        &mut self,
        terminal: &mut Terminal<'_, '_>,
        x: f32,
        y: f32,
        rect: Rect,
        cell: CellDims,
        padding: f32,
        mods: Mods,
    ) -> Result<bool> {
        let mut scrolled = false;
        let top_boundary = padding;
        let bottom_boundary = rect.h - padding;

        if y < top_boundary {
            if self.last_autoscroll.map_or(true, |t| t.elapsed() >= Duration::from_millis(35)) {
                let dist = (top_boundary - y).max(0.0);
                let lines = ((dist / cell.h as f32).ceil() as isize).clamp(1, 5);
                terminal.scroll_viewport(ScrollViewport::Delta(-lines));
                self.last_autoscroll = Some(Instant::now());
                scrolled = true;
            }
        } else if y > bottom_boundary {
            if self.last_autoscroll.map_or(true, |t| t.elapsed() >= Duration::from_millis(35)) {
                let dist = (y - bottom_boundary).max(0.0);
                let lines = ((dist / cell.h as f32).ceil() as isize).clamp(1, 5);
                terminal.scroll_viewport(ScrollViewport::Delta(lines));
                self.last_autoscroll = Some(Instant::now());
                scrolled = true;
            }
        } else {
            self.last_autoscroll = None;
        }

        let point = self.point(x, y, rect, cell, padding, terminal.cols()?, terminal.rows()?);
        let grid_ref = terminal.grid_ref(point)?;
        let geometry = Geometry {
            columns: terminal.cols()?.into(),
            cell_width: cell.w.round() as u32,
            padding_left: padding as u32,
            screen_height: rect.h as u32,
        };
        let rectangle = mods.contains(Mods::ALT)
            && (mods.contains(Mods::CTRL) || mods.contains(Mods::SUPER));
        let selection = self
            .drag_event
            .set_rectangle(rectangle)?
            .set_position(x.into(), y.into())?
            .apply(&mut self.gesture, terminal, grid_ref, geometry)?;
        terminal.set_selection(selection.as_ref())?;
        Ok(scrolled)
    }
}

/// Install a kitty graphics PNG decoder (process-global, once at startup).
/// The crate's own RustPngDecoder has no public constructor (stale docs), so
/// we implement DecodePng ourselves using the png crate.
pub fn install_png_decoder() -> Result<()> {
    graphics::set_png_decoder(Some(Box::new(PngDecoder)))?;
    Ok(())
}

struct PngDecoder;

impl graphics::DecodePng for PngDecoder {
    fn decode_png<'alloc>(
        &mut self,
        alloc: &'alloc libghostty_vt::alloc::Allocator<'_>,
        data: &[u8],
    ) -> Option<graphics::DecodedImage<'alloc>> {
        use png::{Decoder, Transformations};
        use std::io::Cursor;

        let mut decoder = Decoder::new(Cursor::new(data));
        // libghostty only accepts RGBA8: expand palette/grayscale to RGBA,
        // strip 16-bit depth to 8-bit.
        decoder.set_transformations(Transformations::ALPHA | Transformations::STRIP_16);

        let mut frame = decoder.read_info().ok()?;
        // 0.18 answers None when the frame's output size cannot be computed
        // (checked arithmetic), which is an image we cannot decode either.
        let mut buf = vec![0u8; frame.output_buffer_size()?];
        let info = frame.next_frame(&mut buf).ok()?;

        let mut bytes = Bytes::new_with_alloc(alloc, info.buffer_size()).ok()?;
        bytes.copy_from_slice(&buf[..info.buffer_size()]);

        Some(graphics::DecodedImage {
            width: info.width,
            height: info.height,
            data: bytes,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Does the terminal core generate responses for the standard startup
    /// probes? Checks each probe type individually so a single response
    /// can't mask a missing handler.
    #[test]
    fn terminal_responds_to_probes() {
        let mut term =
            Terminal::new(TerminalOptions { cols: 80, rows: 24, max_scrollback: 100 }).unwrap();
        let resp: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
        let r = resp.clone();
        term.on_pty_write(move |_t, data| {
            r.lock().unwrap().extend_from_slice(data);
        })
        .unwrap();
        term.on_xtversion(|_t| Some("ghostwave")).unwrap();
        term.on_device_attributes(|_t| {
            Some(DeviceAttributes {
                primary: PrimaryDeviceAttributes::new(
                    ConformanceLevel::VT220,
                    &[
                        DeviceAttributeFeature::COLUMNS_132,
                        DeviceAttributeFeature::SELECTIVE_ERASE,
                        DeviceAttributeFeature::ANSI_COLOR,
                    ],
                ),
                secondary: SecondaryDeviceAttributes {
                    device_type: DeviceType::VT220,
                    firmware_version: 1,
                    rom_cartridge: 0,
                },
                tertiary: Default::default(),
            })
        })
        .unwrap();

        // Prime bg color: OSC 11 queries only get answered once a color has been set.
        let _ = term.vt_write(b"\x1b]11;rgb:1b/1e/28\x1b\\");

        // Fire each probe separately and assert its response shape, so we
        // verify the *right* handler fired, not just \"something happened\".
        // Kitty keyboard protocol query -> CSI ? <flags> u
        resp.lock().unwrap().clear();
        let _ = term.vt_write(b"\x1b[?u");
        assert!(!resp.lock().unwrap().is_empty(), "kitty query should get a response");

        // XTVERSION (CSI > q) -> DCS > |ghostwave ST
        resp.lock().unwrap().clear();
        let _ = term.vt_write(b"\x1b[>q");
        let got = String::from_utf8_lossy(&resp.lock().unwrap().clone()).into_owned();
        assert!(got.contains("ghostwave"), "XTVERSION should contain ghostwave, got {got:?}");

        // Device Attributes query (CSI c) -> CSI ? 62 ; ... c
        resp.lock().unwrap().clear();
        let _ = term.vt_write(b"\x1b[c");
        let got = String::from_utf8_lossy(&resp.lock().unwrap().clone()).into_owned();
        assert!(got.contains("\x1b["), "DA query should produce CSI response, got {got:?}");

        // OSC 11 query (after bg set) -> OSC 11;rgb:... ST
        resp.lock().unwrap().clear();
        let _ = term.vt_write(b"\x1b]11;?\x1b\\");
        assert!(!resp.lock().unwrap().is_empty(), "OSC 11 query should get a response after bg set");

        // Bulk: all four at once still produces all responses
        resp.lock().unwrap().clear();
        let _ = term.vt_write(
            b"\x1b]11;rgb:1b/1e/28\x1b\\\x1b[?u\x1b[>q\x1b[c\x1b]11;?\x1b\\",
        );
        assert!(!resp.lock().unwrap().is_empty(), "bulk probes should produce responses");
    }

    /// The prime drain must process the child's EARLY output synchronously
    /// during construction — this is what answers fish 4's startup terminal
    /// probes inside its read timeout. Probe with an OSC 0 title (a side
    /// effect we can read back via on_title_changed): the child prints it
    /// immediately, and if priming works the pane's title reflects it by
    /// the time the constructor returns.
    #[test]
    fn prime_drains_early_child_output() {
        let cell = CellDims { w: 10.0, h: 26.0, baseline: 20.0 };
        let pane = TermPane::new(
            80,
            24,
            cell,
            Some("/bin/sh"),
            &["-c", "printf '\\033]0;PRIME-TEST\\033\\\\'; sleep 2"],
            None,
            1000,
            1024 * 1024,
            true,
            None,
        )
        .expect("pane with early-output child");
        let title = pane.title();
        drop(pane);
        assert!(
            title.contains("PRIME-TEST"),
            "prime drain missed early child output (title={title:?})"
        );
    }

    /// A child that is slow to write its FIRST byte must still be drained.
    /// Under load a shell's fork/exec runs far longer than the 60ms idle
    /// break, and a drain that starts counting idle time before any output
    /// has arrived gives up there and loses exactly the startup burst the
    /// prime exists to catch (that is what made
    /// `prime_drains_early_child_output` fail on a loaded box). Deterministic
    /// by construction: the child sleeps 200ms before printing, an order of
    /// magnitude past the idle window whatever the box is doing.
    #[test]
    fn prime_drain_waits_out_a_slow_first_write() {
        let cell = CellDims { w: 10.0, h: 26.0, baseline: 20.0 };
        let pane = TermPane::new(
            80,
            24,
            cell,
            Some("/bin/sh"),
            &["-c", "sleep 0.2; printf '\\033]0;SLOW-PRIME\\033\\\\'"],
            None,
            1000,
            1024 * 1024,
            true,
            None,
        )
        .expect("pane with a slow-starting child");
        let title = pane.title();
        drop(pane);
        assert!(
            title.contains("SLOW-PRIME"),
            "prime drain gave up before the child's first write (title={title:?})"
        );
    }

    /// A font query arriving on the pane's inbound path is answered with the
    /// family the app is configured to use, in the terminator the query used.
    /// Feeds the same `scan_queries` entry point both live hook points
    /// (`TermPane::poll_pty`, `Pane::ingest`) call.
    #[test]
    fn pane_answers_the_font_query() {
        let cell = CellDims { w: 10.0, h: 26.0, baseline: 20.0 };
        let mut pane = TermPane::new(
            80,
            24,
            cell,
            None,
            &[],
            None,
            1000,
            1024 * 1024,
            false,
            None,
        )
        .expect("pane without a child");

        let family = crate::ui_fonts::active_family().to_string();
        pane.response.clear();
        pane.scan_queries(b"\x1b]50;?\x1b\\");

        let mut expected = b"\x1b]50;".to_vec();
        expected.extend_from_slice(family.as_bytes());
        expected.extend_from_slice(b"\x1b\\");
        assert_eq!(
            pane.response, expected,
            "font query should be answered with the configured family"
        );
        assert_eq!(pane.font_family, family, "pane captured the active family");
    }

    #[test]
    fn page_keys_encode_to_the_bytes_a_program_reads() {
        let cell = CellDims { w: 10.0, h: 26.0, baseline: 20.0 };
        let mut pane = TermPane::new(
            80,
            24,
            cell,
            None,
            &[],
            None,
            1000,
            1024 * 1024,
            false,
            None,
        )
        .unwrap();

        pane.handle_key(
            Key::PageUp,
            Mods::empty(),
            Mods::empty(),
            '\0',
            None,
            key::Action::Press,
        )
        .unwrap();
        assert_eq!(pane.response, b"\x1b[5~", "PageUp reaches the program as CSI 5 ~");

        pane.response.clear();
        pane.handle_key(
            Key::PageDown,
            Mods::empty(),
            Mods::empty(),
            '\0',
            None,
            key::Action::Press,
        )
        .unwrap();
        assert_eq!(pane.response, b"\x1b[6~", "PageDown reaches the program as CSI 6 ~");
    }

    #[test]
    fn on_primary_screen_follows_the_alternate_screen_flag() {
        let cell = CellDims { w: 10.0, h: 26.0, baseline: 20.0 };
        let mut pane = TermPane::new(
            80,
            24,
            cell,
            None,
            &[],
            None,
            1000,
            1024 * 1024,
            false,
            None,
        )
        .unwrap();

        assert!(pane.on_primary_screen(), "a fresh terminal is on its primary screen");
        pane.term.vt_write(b"\x1b[?1049h");
        assert!(!pane.on_primary_screen(), "1049h switches a program to the alternate screen");
        pane.term.vt_write(b"\x1b[?1049l");
        assert!(pane.on_primary_screen(), "1049l returns it to the primary screen");
    }

    #[test]
    fn parse_cursor_style_variants() {
        use libghostty_vt::render::CursorVisualStyle;
        assert_eq!(TermPane::parse_cursor_style("block"), Some(CursorVisualStyle::Block));
        assert_eq!(TermPane::parse_cursor_style("BAR"), Some(CursorVisualStyle::Bar));
        assert_eq!(TermPane::parse_cursor_style("beam"), Some(CursorVisualStyle::Bar));
        assert_eq!(TermPane::parse_cursor_style("underline"), Some(CursorVisualStyle::Underline));
        assert_eq!(TermPane::parse_cursor_style("hollow"), Some(CursorVisualStyle::BlockHollow));
        assert_eq!(TermPane::parse_cursor_style("block_hollow"), Some(CursorVisualStyle::BlockHollow));
        assert_eq!(TermPane::parse_cursor_style("unknown"), None);
    }

    #[test]
    fn link_at_cell_detects_urls_and_files() {
        let cell = CellDims { w: 10.0, h: 26.0, baseline: 20.0 };
        let mut pane = TermPane::new(
            80,
            24,
            cell,
            None,
            &[],
            None,
            1000,
            1024 * 1024,
            false,
            None,
        )
        .unwrap();

        pane.term.vt_write(b"Check out https://github.com/ghostty-org and src/main.rs:12:4 for details\r\n");
        // Test URL detection
        let url_hit = pane.link_at_cell(0, 15);
        assert!(url_hit.is_some());
        let (url, start, end) = url_hit.unwrap();
        assert_eq!(url, "https://github.com/ghostty-org");
        assert_eq!(start, 10);
        assert_eq!(end, 40);

        // Test File path detection
        let file_hit = pane.link_at_cell(0, 48);
        assert!(file_hit.is_some());
        let (file, _, _) = file_hit.unwrap();
        assert_eq!(file, "src/main.rs:12:4");
    }

    #[test]
    fn selection_drag_autoscrolls_viewport() {
        let cell = CellDims { w: 10.0, h: 20.0, baseline: 15.0 };
        let mut pane = TermPane::new(
            40,
            10,
            cell,
            None,
            &[],
            None,
            100,
            1024 * 1024,
            false,
            None,
        )
        .unwrap();

        for i in 0..30 {
            pane.term.vt_write(format!("line {}\r\n", i).as_bytes());
        }

        let rect = Rect { x: 0.0, y: 0.0, w: 400.0, h: 200.0 };
        let padding = 5.0;

        pane.handle_mouse(
            rect,
            cell,
            padding,
            MouseEv {
                kind: MouseEvKind::Press(libghostty_vt::mouse::Button::Left),
                x: 50.0,
                y: 100.0,
                any_pressed: true,
                button_down: Some(libghostty_vt::mouse::Button::Left),
            },
            Mods::empty(),
        )
        .unwrap();

        pane.handle_mouse(
            rect,
            cell,
            padding,
            MouseEv {
                kind: MouseEvKind::Motion,
                x: 50.0,
                y: -30.0,
                any_pressed: true,
                button_down: Some(libghostty_vt::mouse::Button::Left),
            },
            Mods::empty(),
        )
        .unwrap();

        assert!(pane.scroll_active, "drag above pane top should activate scroll/autoscroll");
    }

    #[test]
    fn scroll_page_and_half_page_scroll_correct_direction() {
        let cell = CellDims { w: 10.0, h: 26.0, baseline: 20.0 };
        let mut pane = TermPane::new(
            80,
            24,
            cell,
            None,
            &[],
            None,
            10_000,
            1024 * 1024,
            false,
            None,
        )
        .unwrap();

        // Populate with lines so scrollback exists
        for i in 0..100 {
            let line = format!("line {}\r\n", i);
            let _ = pane.term.vt_write(line.as_bytes());
        }

        let sb_before = pane.term.scrollbar().unwrap();
        // Viewport should be at the bottom initially
        assert_eq!(sb_before.offset, sb_before.total - sb_before.len);

        // Page up should scroll upward (offset decreases)
        pane.scroll_page(-1);
        let sb_after_page_up = pane.term.scrollbar().unwrap();
        assert!(
            sb_after_page_up.offset < sb_before.offset,
            "page up must decrease scrollbar offset (scrolled upward)"
        );

        // Half page down should scroll downward (offset increases)
        let offset_before_half = sb_after_page_up.offset;
        pane.scroll_half_page(1);
        let sb_after_half_down = pane.term.scrollbar().unwrap();
        assert!(
            sb_after_half_down.offset > offset_before_half,
            "half page down must increase scrollbar offset (scrolled downward)"
        );
    }

    #[test]
    fn scrollback_line_count_converts_to_bytes_and_retains_history() {
        let cell = CellDims { w: 10.0, h: 26.0, baseline: 20.0 };
        // 10,000 lines should convert to at least 10MB byte budget
        let mut pane = TermPane::new(
            80,
            24,
            cell,
            None,
            &[],
            None,
            10_000,
            32 * 1024 * 1024,
            false,
            None,
        )
        .unwrap();

        // Write 2,000 lines (which previously exceeded the 10KB cap)
        for i in 0..2000 {
            let line = format!("line number {:04} with extra text payload\r\n", i);
            let _ = pane.term.vt_write(line.as_bytes());
        }

        let sb = pane.term.scrollbar().unwrap();
        assert!(
            sb.total >= 2000,
            "scrollbar total rows should retain all 2000 lines, got {}",
            sb.total
        );
    }

    /// A detached pane — no shell spawned — for the link tests.
    fn test_pane() -> TermPane {
        let cell = CellDims { w: 10.0, h: 26.0, baseline: 20.0 };
        TermPane::new(80, 24, cell, None, &[], None, 1000, 1024 * 1024, false, None).unwrap()
    }

    /// Every viewport row as text, top row first.
    fn viewport_text(term: &Terminal<'static, 'static>) -> Vec<String> {
        let mut render_state = RenderState::new().unwrap();
        let snapshot = render_state.update(term).unwrap();
        let mut row_it = RowIterator::new().unwrap();
        let mut rows = row_it.update(&snapshot).unwrap();
        let mut cells = CellIterator::new().unwrap();
        let mut buf = String::new();
        let mut out = Vec::new();
        while let Some(row) = rows.next() {
            let mut line = String::new();
            let mut iter = cells.update(row).unwrap();
            while let Some(cell) = iter.next() {
                buf.clear();
                if cell.graphemes_utf8(&mut buf).is_ok() {
                    line.push_str(&buf);
                } else {
                    line.push(' ');
                }
            }
            out.push(line.trim_end().to_string());
        }
        out
    }

    /// The 0.2.2 shape: an OSC 52 write with an empty payload carries no
    /// representations, and the crate must hand us an empty iterator instead of
    /// a zero-length slice over a NULL pointer (the 0.2.1 UB, upstream #74).
    ///
    /// This pins the behaviour. It cannot demonstrate the absence of UB — that
    /// needs Miri, which is not part of this toolchain.
    #[test]
    fn paste_of_empty_osc52_payload_does_not_panic() {
        let mut term =
            Terminal::new(TerminalOptions { cols: 80, rows: 24, max_scrollback: 100 }).unwrap();
        let writes: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = writes.clone();
        term.on_clipboard_write(move |_term, clip| {
            for content in clip.contents() {
                sink.lock().unwrap().push(content.data.to_string());
            }
            Ok(())
        })
        .unwrap();

        term.vt_write(b"\x1b]52;c;\x07");

        assert!(
            writes.lock().unwrap().is_empty(),
            "an empty OSC 52 payload must not reach the clipboard"
        );
    }

    /// A normal OSC 52 write still arrives at the callback with its payload.
    #[test]
    fn osc52_text_write_still_reaches_the_sink() {
        let mut term =
            Terminal::new(TerminalOptions { cols: 80, rows: 24, max_scrollback: 100 }).unwrap();
        let writes: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = writes.clone();
        term.on_clipboard_write(move |_term, clip| {
            for content in clip.contents() {
                sink.lock().unwrap().push(content.data.to_string());
            }
            Ok(())
        })
        .unwrap();

        // base64("hello")
        term.vt_write(b"\x1b]52;c;aGVsbG8=\x07");

        assert_eq!(writes.lock().unwrap().as_slice(), ["hello".to_string()]);
    }

    /// Compression is supported on this target and does not disturb the logical
    /// contents: the oldest surviving scrollback row and the newest row both
    /// read back unchanged after a full pass. This is the test that proves the
    /// feature works on this build instead of being silently unsupported.
    #[test]
    fn compression_returns_complete_and_keeps_scrollback() {
        let mut term = Terminal::new(TerminalOptions {
            cols: 40,
            rows: 6,
            max_scrollback: 32 * 1024,
        })
        .unwrap();
        // ~2x the scrollback budget, so there is real history to compress and
        // some of it is pruned.
        for i in 0..4000 {
            term.vt_write(format!("line {i:05} ....\r\n").as_bytes());
        }
        term.vt_write(b"NEWEST-MARKER\r\n");
        assert!(
            term.scrollback_rows().unwrap() > 0,
            "the writes must have left the primary screen"
        );

        term.scroll_viewport(ScrollViewport::Top);
        let oldest = viewport_text(&term)
            .into_iter()
            .find(|row| !row.is_empty())
            .expect("the top of the viewport must hold scrollback text");
        assert!(
            oldest.starts_with("line "),
            "the oldest row is scrollback text, got {oldest:?}"
        );

        assert_eq!(
            term.compress(CompressionMode::Full).unwrap(),
            CompressionResult::Complete,
            "aarch64 Linux reclaims retained mappings, so this build must not answer Unsupported"
        );

        term.scroll_viewport(ScrollViewport::Top);
        let oldest_after = viewport_text(&term)
            .into_iter()
            .find(|row| !row.is_empty())
            .expect("the top of the viewport must still hold scrollback text");
        assert_eq!(
            oldest_after, oldest,
            "compression changes storage only, never the logical contents"
        );

        term.scroll_viewport(ScrollViewport::Bottom);
        assert!(
            viewport_text(&term)
                .iter()
                .any(|row| row.contains("NEWEST-MARKER")),
            "the newest row must still be readable after compression"
        );
    }

    /// The scheduling contract the idle stepper relies on: the activity token
    /// moves when new output arrives and holds still when it does not. The token
    /// is opaque and may wrap, so only equality is asserted — never an ordering.
    #[test]
    fn compression_activity_is_a_token() {
        let mut term = Terminal::new(TerminalOptions {
            cols: 40,
            rows: 6,
            max_scrollback: 32 * 1024,
        })
        .unwrap();
        // Fill scrollback first: a short write may land without touching the
        // compression window, and then the token legitimately would not move.
        for i in 0..1000 {
            term.vt_write(format!("line {i:05} ....\r\n").as_bytes());
        }

        let first = term.compression_activity().unwrap();
        assert_eq!(
            term.compression_activity().unwrap(),
            first,
            "reading the token does not change it"
        );

        for i in 0..1000 {
            term.vt_write(format!("more {i:05} ....\r\n").as_bytes());
        }
        let second = term.compression_activity().unwrap();
        assert_ne!(
            second, first,
            "new output must move the activity token; an unchanged token means the idle \
             stepper would never run"
        );
    }

    #[test]
    fn link_at_cell_prefers_the_osc8_uri() {
        let mut pane = test_pane();
        // OSC 8 link over a label that is not the URI and would not match the
        // text scan on its own.
        pane.term
            .vt_write(b"\x1b]8;;https://example.com/x\x1b\\docs\x1b]8;;\x1b\\\r\n");

        let (target, start, end) = pane
            .link_at_cell(0, 0)
            .expect("the cell carries an OSC 8 hyperlink");
        assert_eq!(target, "https://example.com/x");
        assert_eq!((start, end), (0, 4), "the range covers the link's label");
    }

    #[test]
    fn link_at_cell_still_finds_a_bare_url() {
        let mut pane = test_pane();
        pane.term.vt_write(b"see https://example.com/plain now\r\n");

        let (target, start, end) = pane
            .link_at_cell(0, 10)
            .expect("a bare URL stays clickable");
        assert_eq!(target, "https://example.com/plain");
        assert_eq!((start, end), (4, 29));
    }
}
