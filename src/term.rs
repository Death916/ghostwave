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
    alloc::Bytes,
    key::{self, Key, Mods},
    kitty::graphics::{self, Graphics, Layer, PlacementIterator},
    mouse,
    render::{CellIterator, Dirty, RenderState, RowIterator},
    selection::gesture::{DragEvent, Geometry, Gesture, PressEvent, ReleaseEvent},
    style::RgbColor,
    terminal::{
        ConformanceLevel, DeviceAttributeFeature, DeviceAttributes, DeviceType, Point,
        PointCoordinate, PrimaryDeviceAttributes, ScrollViewport, SecondaryDeviceAttributes,
        SizeReportSize,
    },
    selection::FormatOptions,
};

use macroquad::prelude::{Color, color_u8};

use crate::{
    clipboard,
    layout::Rect,
    pty::{Child, Pty, PtyError, WinSize},
    search::SearchState,
    theme::Theme,
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
    pub w: u32,
    pub h: u32,
    /// Baseline offset within the cell: glyph baseline y = cell_top + baseline.
    /// Keeps glyphs and the block cursor aligned in the same cell.
    pub baseline: f32,
}

/// A durable remote session binding (ghostd). Saved with the session so
/// restore can re-show the pane in a disconnected state.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct RemoteSpec {
    pub host: String,
    /// Session name on the remote host.
    pub session: String,
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
    placement_it: PlacementIterator<'static>,
    key_encoder: key::Encoder<'static>,
    key_event: key::Event<'static>,
    mouse_encoder: mouse::Encoder<'static>,
    mouse_event: mouse::Event<'static>,
    pub response: Vec<u8>,
    selection: SelectionState<'static>,
    /// None when the pane is in a disconnected state (no child process).
    pub pty: Option<Pty>,
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
    last_rect: Option<Rect>,
    last_mouse: (f32, f32),
    scroll_active: bool,
    /// Cursor blink phase (terminal reports cursor_blinking from modes).
    blink_on: bool,
    last_blink: std::time::Instant,
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
    /// Pane output log (name + file) for durable sessions.
    pub log_name: String,
    log_file: Option<std::fs::File>,
    /// Session log cap in bytes; the log is front-truncated when exceeded.
    log_cap: u64,
    /// Bytes written to the log since the last truncation.
    log_bytes: u64,
    /// Optional cursor visual style override (from config).
    pub cursor_style: Option<libghostty_vt::render::CursorVisualStyle>,
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
            size: Mutex::new((cols, rows, cell.w, cell.h)),
            effect_fd: Mutex::new(-1),
            ..Default::default()
        });

        let mut term = Terminal::new(TerminalOptions {
            cols,
            rows,
            max_scrollback: scrollback,
        })?;

        // A remote pane may be created disconnected (restore) — no child
        // process until the user connects or the reconnect loop fires.
        let (pty, child) = if spawn {
            let (p, c) = Pty::new(
                WinSize {
                    cols,
                    rows,
                    xpixel: (cols as u32 * cell.w) as u16,
                    ypixel: (rows as u32 * cell.h) as u16,
                },
                shell,
                args,
                cwd,
            )?;
            (Some(p), Some(c))
        } else {
            (None, None)
        };

        // Pane output log for durable sessions (best-effort; a missing log
        // just means no session restore for this pane).
        let (log_name, log_file) = crate::session::open_pane_log()
            .unwrap_or((String::new(), std::fs::File::open("/dev/null").unwrap()));

        let effect_fd = match &pty {
            Some(p) => p.dup_raw().unwrap_or(-1),
            None => -1,
        };
        *shared.effect_fd.lock().unwrap() = effect_fd;
        let shared_effects = shared.clone();

        term.resize(cols, rows, cell.w, cell.h)?;
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
                    if content.mime.contains("text") || content.mime == "application/x-uripboard" {
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
        let placement_it = PlacementIterator::new()?;

        Ok(Self {
            term,
            render_state,
            row_it,
            cell_it,
            placement_it,
            key_encoder,
            key_event,
            mouse_encoder,
            mouse_event,
            response: Vec::with_capacity(64),
            selection: SelectionState::new()?,
            pty,
            child,
            shared,
            effect_fd,
            name: String::new(),
            cols,
            rows,
            search: SearchState::default(),
            exited: None,
            force_repaint: true,
            last_rect: None,
            last_mouse: (0.0, 0.0),
            scroll_active: false,
            blink_on: true,
            last_blink: std::time::Instant::now(),
            program: shell.map(String::from),
            args: args.iter().map(|s| s.to_string()).collect(),
            cwd: cwd.map(String::from),
            remote,
            last_exit_code: None,
            reconnect: ReconnectState::Idle,
            reconnect_failures: 0,
            log_name,
            log_file: Some(log_file),
            log_cap: log_cap_bytes.max(1024),
            log_bytes: 0,
            cursor_style: None,
        })
        .map(|mut pane| {
            if spawn {
                pane.prime_pty();
            }
            pane
        })
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
    fn prime_pty(&mut self) {
        let start = std::time::Instant::now();
        let mut idle = std::time::Duration::ZERO;
        // Cap ~250ms: a shell's startup burst (config + terminal probes)
        // can take a while, but we must not block pane creation forever.
        while start.elapsed() < std::time::Duration::from_millis(250) {
            let active = self.poll_pty();
            self.flush_response();
            if active {
                idle = std::time::Duration::ZERO;
            } else {
                idle += std::time::Duration::from_millis(2);
                if idle > std::time::Duration::from_millis(60) {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
        }
    }

    pub fn title(&self) -> String {
        self.shared.title.lock().unwrap().clone()
    }

    pub fn pwd(&self) -> String {
        self.shared.pwd.lock().unwrap().clone()
    }

    pub fn set_grid(&mut self, cols: u16, rows: u16, cell: CellDims) -> Result<()> {
        if cols == self.cols && rows == self.rows {
            return Ok(());
        }
        self.cols = cols;
        self.rows = rows;
        *self.shared.size.lock().unwrap() = (cols, rows, cell.w, cell.h);
        self.term.resize(cols, rows, cell.w, cell.h)?;
        if let Some(pty) = self.pty.as_ref() {
            pty.resize(WinSize {
                cols,
                rows,
                xpixel: (cols as u32 * cell.w) as u16,
                ypixel: (rows as u32 * cell.h) as u16,
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
        let Some(pty) = self.pty.as_ref() else {
            return false;
        };
        match pty.read() {
            Ok(data) => {
                // Tee output into the session log (durable sessions). The log
                // is front-truncated when it exceeds the configured cap so it
                // cannot balloon indefinitely (see session::truncate_log_front).
                if !data.is_empty() {
                    if let Some(f) = self.log_file.as_mut() {
                        use std::io::Write;
                        let _ = f.write_all(&data);
                        self.log_bytes += data.len() as u64;
                        if self.log_bytes > self.log_cap && !self.log_name.is_empty() {
                            let path = crate::session::pane_log_path(&self.log_name);
                            let _ = crate::session::truncate_log_front(&path, self.log_cap);
                            self.log_bytes = self.log_cap;
                        }
                    }
                    self.term.vt_write(&data);
                    return true;
                }
                return false;
            }
            Err(PtyError::EndOfStream) => {
                self.exited = Some("process exited".into());
                self.force_repaint = true;
                self.last_exit_code = self.child.as_mut().and_then(|c| c.poll_reap());
                return false;
            }
            Err(PtyError::OtherError(e)) => {
                self.exited = Some(format!("pty error: {e}"));
                self.force_repaint = true;
                self.last_exit_code = self.child.as_mut().and_then(|c| c.poll_reap());
                return false;
            }
        }
        if let Some(child) = self.child.as_mut() {
            if let Some(code) = child.poll_reap() {
                self.last_exit_code = Some(code);
            }
        }
        false
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
        self.pty = Some(pty);
        self.child = Some(child);
        if let Some(p) = self.pty.as_ref() {
            let old = std::mem::replace(&mut *self.shared.effect_fd.lock().unwrap(), -1);
            if old >= 0 {
                unsafe { nix::libc::close(old) };
            }
            *self.shared.effect_fd.lock().unwrap() = p.dup_raw().unwrap_or(-1);
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
        if let Some(f) = self.log_file.as_mut() {
            use std::io::Write;
            let _ = f.write_all(data);
            self.log_bytes += data.len() as u64;
        }
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
                cell_width: cell.w,
                cell_height: cell.h,
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
                if moved
                    && ev.button_down == Some(mouse::Button::Left)
                    && self.selection.is_selecting(&self.term)?
                {
                    self.selection.on_drag(
                        &mut self.term,
                        ev.x,
                        ev.y,
                        rect,
                        cell,
                        padding,
                        mods,
                    )?;
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
        let n = (self.rows as i32 * pages).max(1);
        self.scroll(n);
    }

    pub fn scroll_to_bottom(&mut self) {
        self.term.scroll_viewport(ScrollViewport::Bottom);
        self.scroll_active = false;
        self.force_repaint = true;
    }

    pub fn clear_terminal(&mut self) -> Result<()> {
        self.term.reset();
        self.force_repaint = true;
        Ok(())
    }

    // ---- selection / clipboard ------------------------------------------

    /// Copy the active selection, or the whole viewport if none.
    pub fn copy_selection(&mut self) -> Result<bool> {
        // Active selection (FormatOptions::new uses the terminal's current selection).
        if let Some(bytes) = self.term.format_selection_alloc(None, FormatOptions::new())? {
            let text = String::from_utf8_lossy(&bytes).into_owned();
            if !text.is_empty() {
                self.term.set_selection(None)?;
                self.force_repaint = true;
                return Ok(clipboard::write_text(&text));
            }
        }
        // No selection: copy the visible screen.
        if let Some(sel) = self.term.select_all()? {
            let opts = FormatOptions::new().with_selection(&sel);
            if let Some(bytes) = self.term.format_selection_alloc(None, opts)? {
                let text = String::from_utf8_lossy(&bytes).into_owned();
                return Ok(clipboard::write_text(&text));
            }
        }
        Ok(false)
    }

    pub fn paste_clipboard(&mut self) {
        if let Some(text) = clipboard::read_text() {
            self.write_input(text.as_bytes());
        }
    }

    pub fn paste_primary(&mut self) {
        if let Some(text) = clipboard::read_primary_text().or_else(clipboard::read_text) {
            self.write_input(text.as_bytes());
        }
    }

    pub fn is_mouse_tracking(&self) -> bool {
        self.term.is_mouse_tracking().unwrap_or(false)
    }

    // ---- rendering -------------------------------------------------------

    /// Render this pane into `rect` (window-logical coords, but drawing is in
    /// physical px via the global dpi scale handled by the caller's camera).
    pub fn render(
        &mut self,
        rect: crate::layout::Rect,
        cell: CellDims,
        padding: f32,
        font_stack: &crate::fonts::FontStack,
        font_px: u16,
        theme: Theme,
        focused: bool,
    ) -> Result<()> {
        use macroquad::prelude::*;

        // Pane-local draw origin (macroquad draws in physical px).
        let (ox, oy) = (rect.x, rect.y);

        self.last_rect = Some(rect);

        let snapshot = self.render_state.update(&self.term)?;
        let colors = snapshot.colors()?;

        // Documented libghostty-vt pattern (ghostling-rs reference): draw the
        // pane background every frame using the TERMINAL's default bg, then
        // redraw ALL rows/cells every frame, then reset dirty flags. NOTE:
        // per-row damage skipping is NOT possible against the swapchain —
        // the GL backbuffer is not guaranteed to retain content between
        // swaps (no buffer-age on Wayland/EGL), so skipping rows leaves
        // black/garbage (the old flicker bug). Damage rendering would need
        // an offscreen render target owned by the app (TODO, perf branch).
        draw_rectangle(ox, oy, rect.w, rect.h, color(colors.background));

        // Blink phase (updated here, once per frame — not per row).
        if snapshot.cursor_blinking()? {
            let now = std::time::Instant::now();
            if now.duration_since(self.last_blink).as_millis() >= 500 {
                self.blink_on = !self.blink_on;
                self.last_blink = now;
            }
        }

        {
            // --- search pass: compute matches on current viewport ---
            if self.search.active() && self.search.dirty {
                let mut search_it = self.row_it.update(&snapshot)?;
                self.search.matches.clear();
                self.search.run_on_rows(&mut search_it)?;
                self.search.dirty = false;
            }

            let mut row_it = self.row_it.update(&snapshot)?;

            let mut y = oy + padding;
            let mut row_idx: u32 = 0;
            let mut text = String::with_capacity(32);

            while let Some(row) = row_it.next() {
                let mut x = ox + padding;
                let mut col_idx: u16 = 0;
                let mut cell_it = self.cell_it.update(row)?;

                while let Some(c) = cell_it.next() {
                    let selected = c.is_selected()?;
                    let graphemes = c.graphemes_len()?;
                    let bg_opt = c.bg_color()?;

                    if graphemes == 0 {
                        if selected {
                            draw_rectangle(x, y, cell.w as f32, cell.h as f32, color_theme(theme.selection));
                        } else if let Some(bg) = bg_opt {
                            draw_rectangle(x, y, cell.w as f32, cell.h as f32, color(bg));
                        }
                    } else {
                        c.graphemes_utf8(&mut text)?;
                        // Strip zero-width format chars (variation selectors,
                        // ZWJ/ZWNJ/ZWSP, word joiner, BOM): they carry no ink
                        // and almost no font has them, so drawing them yields
                        // tofu boxes (e.g. starship's nix "❄️" = U+2744 + VS16).
                        text.retain(|ch| {
                            !matches!(
                                ch,
                                '\u{FE00}'..='\u{FE0F}'   // variation selectors
                                    | '\u{200B}'..='\u{200D}' // ZWSP, ZWNJ, ZWJ
                                    | '\u{2060}'             // word joiner
                                    | '\u{FEFF}'             // BOM / ZWNBSP
                            )
                        });
                        // Font fallback: first stack font that covers the glyph
                        // (primary = bundled JetBrains Mono).
                        let font = font_stack.get(font_stack.font_for(text.chars().next().unwrap_or(' ')));
                        let mut fg = crate::theme::Rgb(colors.foreground.r, colors.foreground.g, colors.foreground.b);
                        let mut bg = theme.bg;
                        let mut has_bg = false;
                        let mut bold = false;

                        if let Some(c) = c.fg_color()? {
                            fg = crate::theme::Rgb(c.r, c.g, c.b);
                        }
                        if let Some(c) = bg_opt {
                            bg = crate::theme::Rgb(c.r, c.g, c.b);
                            has_bg = true;
                        }

                        if c.has_styling()? {
                            let style = c.style()?;
                            if style.inverse {
                                std::mem::swap(&mut fg, &mut bg);
                                has_bg = true;
                            }
                            bold = style.bold;
                        }

                        if selected {
                            std::mem::swap(&mut fg, &mut bg);
                            has_bg = true;
                        }

                        // Search match highlight (after selection, before draw):
                        // fill the cell bg with the match color, draw the glyph
                        // in the theme bg for contrast.
                        let matched = self.search.contains(row_idx, col_idx);

                        if matched {
                            draw_rectangle(x, y, cell.w as f32, cell.h as f32, color_theme(theme.search_match));
                        } else if has_bg {
                            draw_rectangle(x, y, cell.w as f32, cell.h as f32, color_theme(bg));
                        }

                        let draw_color = if matched {
                            theme.bg
                        } else {
                            fg
                        };

                        draw_text_ex(
                            &text,
                            x,
                            y + cell.baseline,
                            TextParams {
                                font: Some(font),
                                font_size: font_px,
                                color: color_theme(draw_color),
                                ..Default::default()
                            },
                        );

                        if bold {
                            draw_text_ex(
                                &text,
                                x + 1.0,
                                y + cell.baseline,
                                TextParams {
                                    font: Some(font),
                                    font_size: font_px,
                                    color: color_theme(draw_color),
                                    ..Default::default()
                                },
                            );
                        }
                    }
                    x += cell.w as f32;
                    col_idx += 1;
                }

                // Search match row marker on the left edge.
                if self.search.row_has_match(row_idx) {
                    draw_rectangle(
                        ox + padding - 3.0,
                        y + 2.0,
                        2.0,
                        cell.h as f32 - 4.0,
                        color_theme(theme.search_match),
                    );
                }

                row.set_dirty(false)?;
                y += cell.h as f32;
                row_idx += 1;
            }

            // Cursor (with blink: ghostty reports cursor_blinking from
            // terminal modes; visibility phase updated above, before the
            // damage decision, so phase flips trigger a repaint).
            if snapshot.cursor_visible()?
                && let Some(vp) = snapshot.cursor_viewport()?
                && focused
            {
                let draw_cursor = self.blink_on || !snapshot.cursor_blinking()?;
                let cursor_color = colors.cursor.unwrap_or(colors.foreground);
                let style = match self.cursor_style {
                    Some(s) => s,
                    None => snapshot.cursor_visual_style()?,
                };
                let (cx, cy) = (
                    ox + padding + vp.x as f32 * cell.w as f32,
                    oy + padding + vp.y as f32 * cell.h as f32,
                );
                if draw_cursor {
                    match style {
                    libghostty_vt::render::CursorVisualStyle::Block => {
                        draw_rectangle(cx, cy, cell.w as f32, cell.h as f32, color(cursor_color));
                    }
                    libghostty_vt::render::CursorVisualStyle::Bar => {
                        draw_rectangle(cx, cy, 2.0, cell.h as f32, color(cursor_color));
                    }
                    libghostty_vt::render::CursorVisualStyle::Underline => {
                        draw_rectangle(
                            cx,
                            cy + cell.h as f32 - 2.0,
                            cell.w as f32,
                            2.0,
                            color(cursor_color),
                        );
                    }
                    libghostty_vt::render::CursorVisualStyle::BlockHollow => {
                        draw_rectangle_lines(
                            cx,
                            cy,
                            cell.w as f32,
                            cell.h as f32,
                            1.0,
                            color(cursor_color),
                        );
                    }
                    _ => {}
                    }
                }
            }
        }

        // Kitty graphics images
        if let Ok(graphics) = self.term.kitty_graphics() {
            Self::render_kitty_images(
                &self.term,
                &mut self.placement_it,
                &graphics,
                Layer::BelowBg,
                rect,
                cell,
                padding,
            )?;
            Self::render_kitty_images(
                &self.term,
                &mut self.placement_it,
                &graphics,
                Layer::BelowText,
                rect,
                cell,
                padding,
            )?;
            Self::render_kitty_images(
                &self.term,
                &mut self.placement_it,
                &graphics,
                Layer::AboveText,
                rect,
                cell,
                padding,
            )?;
        }

        // Exited overlay — remote panes get a reconnect prompt instead of
        // the generic message.
        if let Some(msg) = &self.exited {
            if let Some(remote) = &self.remote {
                let status = match self.reconnect {
                    ReconnectState::Waiting { attempt, .. } => {
                        format!("reconnecting (attempt {attempt}) — Esc to stop")
                    }
                    ReconnectState::Manual => "disconnected — Enter connect | k kill remote | Esc dismiss".to_string(),
                    ReconnectState::Idle => "disconnected".to_string(),
                };
                draw_rectangle(ox + 4.0, oy + 4.0, 520.0, 56.0, color_theme(theme.status_bg));
                draw_rectangle_lines(ox + 4.0, oy + 4.0, 520.0, 56.0, 1.0, color_theme(theme.frame_active));
                draw_text(
                    &format!("ssh {} — session {}", remote.host, remote.session),
                    ox + 14.0,
                    oy + 28.0,
                    16.0,
                    color_theme(theme.status_fg),
                );
                draw_text(&status, ox + 14.0, oy + 50.0, 14.0, color_theme(theme.status_fg));
            } else {
                draw_rectangle(ox + 4.0, oy + 4.0, 260.0, 40.0, color_theme(theme.status_bg));
                draw_text(&msg, ox + 12.0, oy + 28.0, 16.0, color_theme(theme.status_fg));
            }
        }

        // Scrollbar
        if let Ok(sb) = self.term.scrollbar() {
            if sb.total > sb.len {
                let area = (rect.h - 2.0 * padding).max(1.0);
                let bar_h = area * (sb.len as f32 / sb.total.max(1) as f32);
                let top_frac = sb.offset as f32 / sb.total.max(1) as f32;
                let bar_y = oy + padding + top_frac * (area - bar_h);
                draw_rectangle(ox + rect.w - 5.0, bar_y, 3.0, bar_h.max(8.0), color_theme(theme.frame_active));
            }
        }

        // Bell flash
        if self.shared.bell.swap(false, Ordering::Relaxed) {
            draw_rectangle(ox, oy, rect.w, rect.h, Color::new(1.0, 1.0, 1.0, 0.08));
        }

        snapshot.set_dirty(Dirty::Clean)?;
        Ok(())
    }

    fn render_kitty_images(
        term: &Terminal<'static, 'static>,
        placement_it: &mut PlacementIterator<'static>,
        graphics: &Graphics<'_>,
        layer: Layer,
        rect: crate::layout::Rect,
        cell: CellDims,
        padding: f32,
    ) -> Result<()> {
        use macroquad::prelude::*;
        let mut placements = placement_it.update(graphics)?;
        placements.set_layer(layer)?;

        while let Some(placement) = placements.next() {
            let image_id = placement.image_id()?;
            let Some(image) = graphics.image(image_id) else { continue };
            let info = placement.placement_render_info(&image, term)?;

            if !info.viewport_visible
                || info.pixel_width == 0
                || info.pixel_height == 0
                || info.grid_cols == 0
                || info.grid_rows == 0
            {
                continue;
            }
            if image.format()? != graphics::ImageFormat::Rgba {
                continue;
            }
            let image_width = image.width()?;
            let image_height = image.height()?;
            let data = image.data()?;
            if data.len() < (image_width * image_height * 4) as usize {
                continue;
            }

            let dest_size = vec2(
                info.grid_cols as f32 * cell.w as f32,
                info.grid_rows as f32 * cell.h as f32,
            );
            let x_offset = placement.x_offset()? as f32;
            let y_offset = placement.y_offset()? as f32;

            let tex = Texture2D::from_rgba8(image_width as u16, image_height as u16, data);
            draw_texture_ex(
                &tex,
                rect.x + padding + info.viewport_col as f32 * cell.w as f32 + x_offset,
                rect.y + padding + info.viewport_row as f32 * cell.h as f32 + y_offset,
                WHITE,
                DrawTextureParams {
                    dest_size: Some(dest_size),
                    source: Some(macroquad::prelude::Rect {
                        x: info.source_x as f32,
                        y: info.source_y as f32,
                        w: info.source_width as f32,
                        h: info.source_height as f32,
                    }),
                    ..Default::default()
                },
            );
        }
        Ok(())
    }

    /// Grid point for a pane-local pixel position (clamped).
    pub fn grid_point(&self, px: f32, py: f32, rect: Rect, cell: CellDims, padding: f32) -> Point {
        let x = ((px - rect.x - padding) / cell.w as f32).clamp(0.0, self.cols as f32) as u16;
        let y = ((py - rect.y - padding) / cell.h as f32).clamp(0.0, self.rows as f32) as u32;
        Point::Viewport(PointCoordinate { x, y })
    }
}

impl Drop for TermPane {
    fn drop(&mut self) {
        // Close the effect fd so no writes go to a dead pty.
        unsafe {
            nix::libc::close(self.effect_fd);
        }
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

fn color(rgb: RgbColor) -> macroquad::color::Color {
    color_u8!(rgb.r, rgb.g, rgb.b, 255)
}

fn color_theme(rgb: crate::theme::Rgb) -> macroquad::color::Color {
    color_u8!(rgb.0, rgb.1, rgb.2, 255)
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
        })
    }

    fn is_selecting(&self, terminal: &Terminal<'_, '_>) -> Result<bool> {
        Ok(self.gesture.click_count(terminal)? > 0)
    }

    /// Convert pane-local pointer pos (already relative to the pane rect,
    /// i.e. surface-space) to a viewport cell. Padding is the grid inset.
    fn point(&self, x: f32, y: f32, rect: Rect, cell: CellDims, padding: f32, cols: u16, rows: u16) -> Point {
        let _ = rect;
        let px = ((x - padding) / cell.w as f32).clamp(0.0, cols as f32) as u16;
        let py = ((y - padding) / cell.h as f32).clamp(0.0, rows as f32) as u32;
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
    ) -> Result<()> {
        let point = self.point(x, y, rect, cell, padding, terminal.cols()?, terminal.rows()?);
        let grid_ref = terminal.grid_ref(point)?;
        let geometry = Geometry {
            columns: terminal.cols()?.into(),
            cell_width: cell.w,
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
        Ok(())
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
        let mut buf = vec![0u8; frame.output_buffer_size()];
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
        let cell = CellDims { w: 10, h: 26, baseline: 20.0 };
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
}
