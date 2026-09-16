//! Throwaway iced 0.14 spike: one libghostty-vt terminal pane on a Canvas.
//! Answers: present/s idle vs streaming, CPU/GPU cost vs macroquad build.

use std::{
    os::fd::{AsFd, AsRawFd, IntoRawFd, OwnedFd},
    os::unix::process::CommandExt,
    path::PathBuf,
    process::Command,
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc, Arc, Mutex,
    },
    time::{Duration, Instant},
};

use iced::{
    keyboard::{self, key::Named, Key, Modifiers},
    mouse,
    widget::canvas::{self, Canvas, Cache, Frame, Geometry, Text as CanvasText},
    widget::text,
    alignment, Color, Element, Fill, Pixels, Point, Rectangle, Renderer, Subscription, Task,
    Theme,
};

use libghostty_vt::{
    render::{CellIterator, Dirty, RenderState, RowIterator},
    Terminal, TerminalOptions,
};

use nix::{
    errno::Errno,
    fcntl::{self, OFlag},
    pty::ForkptyResult,
    sys::{signal, wait},
    unistd::{self, Pid},
};

// ---------------------------------------------------------------------------
// Font bytes (embedded)
// ---------------------------------------------------------------------------
const FONT_BYTES: &[u8] = include_bytes!("../../assets/JetBrainsMono-Medium.ttf");

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------
const FONT_PX: u16 = 16;
const COLS: u16 = 80;
const ROWS: u16 = 24;
const PADDING: f32 = 4.0;

// ---------------------------------------------------------------------------
// PTY helpers
// ---------------------------------------------------------------------------
#[derive(Clone, Copy)]
struct WinSize {
    cols: u16,
    rows: u16,
    xpixel: u16,
    ypixel: u16,
}

impl WinSize {
    fn to_winsize(self) -> nix::pty::Winsize {
        nix::pty::Winsize {
            ws_col: self.cols,
            ws_row: self.rows,
            ws_xpixel: self.xpixel,
            ws_ypixel: self.ypixel,
        }
    }
}

#[derive(Clone, Debug)]
enum PtyMsg {
    Data(Vec<u8>),
    Eof,
}

struct Pty(OwnedFd);

impl Pty {
    fn new(size: WinSize) -> std::io::Result<(Self, Child)> {
        let fixture = std::env::var("SPIKE_FIXTURE").ok();
        let child_result = unsafe { nix::pty::forkpty(&size.to_winsize(), None)? };
        match child_result {
            ForkptyResult::Child => {
                if let Some(cmd) = fixture {
                    let _ = Command::new("/bin/sh")
                        .arg("-c")
                        .arg(&cmd)
                        .env("TERM", "xterm-256color")
                        .exec();
                } else {
                    let shell = match unistd::User::from_uid(unistd::getuid()) {
                        Ok(Some(user)) if !user.shell.as_os_str().is_empty() => user.shell,
                        _ => std::env::var_os("SHELL")
                            .filter(|s| !s.is_empty())
                            .map(PathBuf::from)
                            .unwrap_or_else(|| PathBuf::from("/bin/sh")),
                    };
                    let arg0 = shell.file_name().unwrap_or(shell.as_os_str());
                    let _ = Command::new(&shell)
                        .arg0(arg0)
                        .env("TERM", "xterm-256color")
                        .exec();
                }
                std::process::exit(127);
            }
            ForkptyResult::Parent { child, master: fd } => {
                let raw = fcntl::fcntl(&fd, fcntl::F_GETFL)?;
                let flags = OFlag::from_bits_retain(raw) | OFlag::O_NONBLOCK;
                let _ = fcntl::fcntl(&fd, fcntl::F_SETFL(flags))?;
                Ok((Self(fd), Child::Active(child)))
            }
        }
    }

    /// Reader thread: pumps the pty into an iced-futures channel, so the iced
    /// subscription can stream it as messages (no polling, no timer).
    fn spawn_reader(
        &self,
        tx: iced::futures::channel::mpsc::UnboundedSender<PtyMsg>,
    ) -> std::io::Result<()> {
        let fd = self.0.try_clone()?;
        std::thread::Builder::new()
            .name("spike-pty-reader".into())
            .spawn(move || {
                use nix::poll::{poll, PollFd, PollFlags, PollTimeout};
                let mut buf = [0u8; 65536];
                let mut pfds = [PollFd::new(
                    fd.as_fd(),
                    PollFlags::POLLIN | PollFlags::POLLHUP | PollFlags::POLLERR,
                )];
                loop {
                    match poll(&mut pfds, PollTimeout::NONE) {
                        Ok(_) => {
                            let revents = pfds[0].revents().unwrap_or(PollFlags::empty());
                            if revents.contains(PollFlags::POLLIN) {
                                match nix::unistd::read(&fd, &mut buf) {
                                    Ok(0) => {
                                        let _ = tx.unbounded_send(PtyMsg::Eof);
                                        break;
                                    }
                                    Ok(len) => {
                                        if tx.unbounded_send(PtyMsg::Data(buf[..len].to_vec())).is_err() {
                                            break;
                                        }
                                    }
                                    Err(Errno::EINTR) | Err(Errno::EAGAIN) => continue,
                                    Err(_) => {
                                        let _ = tx.unbounded_send(PtyMsg::Eof);
                                        break;
                                    }
                                }
                            } else if revents.contains(PollFlags::POLLHUP)
                                || revents.contains(PollFlags::POLLERR)
                            {
                                let _ = tx.unbounded_send(PtyMsg::Eof);
                                break;
                            }
                        }
                        Err(Errno::EINTR) => continue,
                        Err(_) => {
                            let _ = tx.unbounded_send(PtyMsg::Eof);
                            break;
                        }
                    }
                }
            })?;
        Ok(())
    }

    fn write(&self, data: &[u8]) {
        let mut rem = data;
        while !rem.is_empty() {
            match nix::unistd::write(&self.0, rem) {
                Ok(len) => rem = &rem[len..],
                Err(Errno::EINTR) => continue,
                Err(_) => break,
            }
        }
    }

    fn dup_raw(&self) -> std::io::Result<i32> {
        let dup = nix::unistd::dup(&self.0)?;
        Ok(dup.into_raw_fd())
    }

    fn resize(&self, size: WinSize) {
        nix::ioctl_write_ptr_bad!(tiocswinsz, nix::libc::TIOCSWINSZ, nix::pty::Winsize);
        let _ = unsafe { tiocswinsz(self.0.as_raw_fd(), &size.to_winsize()) };
    }
}

enum Child {
    Active(Pid),
    Exited(Pid),
    Reaped(wait::WaitStatus),
}

impl Drop for Child {
    fn drop(&mut self) {
        match *self {
            Child::Active(pid) | Child::Exited(pid) => {
                let _ = signal::kill(pid, signal::SIGHUP);
                let _ = wait::waitpid(pid, None);
            }
            Child::Reaped(_) => {}
        }
    }
}

// ---------------------------------------------------------------------------
// Cell metrics (from fontdue)
// ---------------------------------------------------------------------------
#[derive(Clone, Copy)]
struct CellDims {
    w: f32,
    h: f32,
    baseline: f32,
}

fn compute_cell_dims() -> CellDims {
    let font = fontdue::Font::from_bytes(FONT_BYTES, fontdue::FontSettings::default())
        .expect("fontdue parse");
    let metrics = font
        .horizontal_line_metrics(FONT_PX as f32)
        .expect("horizontal_line_metrics");
    let ascent = metrics.ascent;
    let descent = -metrics.descent;
    let row_gap = 0.0;
    let cell_h = ascent + descent + row_gap;
    let cell_w = font.metrics('M', FONT_PX as f32).width as f32;
    let baseline = ascent + row_gap / 2.0;
    CellDims {
        w: cell_w,
        h: cell_h,
        baseline,
    }
}

// ---------------------------------------------------------------------------
// Measurement state
// ---------------------------------------------------------------------------
struct Measurements {
    frame_count: AtomicU64,
    last_snapshot: Mutex<Instant>,
    last_count: Mutex<u64>,
    presents_per_sec: Mutex<f64>,
}

impl Measurements {
    fn new() -> Self {
        Self {
            frame_count: AtomicU64::new(0),
            last_snapshot: Mutex::new(Instant::now()),
            last_count: Mutex::new(0),
            presents_per_sec: Mutex::new(0.0),
        }
    }

    fn tick(&self) {
        self.frame_count.fetch_add(1, Ordering::Relaxed);
        let now = Instant::now();
        let mut last = self.last_snapshot.lock().unwrap();
        let elapsed = now.duration_since(*last);
        if elapsed >= Duration::from_secs(1) {
            let count = self.frame_count.load(Ordering::Relaxed);
            let prev = *self.last_count.lock().unwrap();
            let diff = count - prev;
            let pps = diff as f64 / elapsed.as_secs_f64();
            *self.presents_per_sec.lock().unwrap() = pps;
            *self.last_count.lock().unwrap() = count;
            *last = now;
            eprintln!("[spike] presents/s: {:.1} (cumul: {})", pps, count);
        }
    }
}

// ---------------------------------------------------------------------------
// Terminal state
// ---------------------------------------------------------------------------
struct TermState {
    term: Terminal<'static, 'static>,
    render_state: RenderState<'static>,
    row_it: RowIterator<'static>,
    cell_it: CellIterator<'static>,
    cell: CellDims,
    dirty: bool,
    effect_fd: i32,
}

// ---------------------------------------------------------------------------
// iced messages
// ---------------------------------------------------------------------------
#[derive(Debug)]
enum Message {
    PtyMsg(PtyMsg),
    KeyPress(Key, Modifiers),
}

// ---------------------------------------------------------------------------
// iced application
// ---------------------------------------------------------------------------
struct App {
    term_state: Arc<Mutex<TermState>>,
    measurements: Arc<Measurements>,
    pty: Arc<Pty>,
    rx_slot: PtyRxSlot,
    child: Option<Child>,
    cell: CellDims,
    cols: u16,
    rows: u16,
    exited: bool,
}

/// The pty receiver handed to the iced subscription. Wrapped in an Arc so the
/// subscription can own it while `App::new` keeps a handle for priming, and
/// Hashable by pointer so iced treats the subscription identity as stable.
#[derive(Clone)]
struct PtyRxSlot(Arc<Mutex<Option<iced::futures::channel::mpsc::UnboundedReceiver<PtyMsg>>>>);

impl std::hash::Hash for PtyRxSlot {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        (Arc::as_ptr(&self.0) as usize).hash(state);
    }
}

/// Built once by iced when the subscription starts: the pty stream itself.
/// Returns a pending stream if the receiver was already taken (so a restarted
/// subscription is a no-op instead of a panic).
fn pty_stream(slot: &PtyRxSlot) -> std::pin::Pin<Box<dyn iced::futures::Stream<Item = Message> + Send>> {
    use iced::futures::StreamExt;
    match slot.0.lock().unwrap().take() {
        Some(rx) => Box::pin(rx.map(Message::PtyMsg)),
        None => Box::pin(iced::futures::stream::pending::<Message>()),
    }
}

/// Keyboard events arrive as ignored runtime events (the canvas doesn't capture
/// them), so this is the whole input path: key press -> bytes -> pty.
fn kb_listen(
    event: iced::Event,
    _status: iced::event::Status,
    _window: iced::window::Id,
) -> Option<Message> {
    match event {
        iced::Event::Keyboard(keyboard::Event::KeyPressed {
            key, modifiers, ..
        }) => Some(Message::KeyPress(key, modifiers)),
        _ => None,
    }
}

impl App {
    fn new() -> (Self, Task<Message>) {
        let cell = compute_cell_dims();
        let cols = COLS;
        let rows = ROWS;

        eprintln!(
            "[spike] cell: {:.1}x{:.1} px, {} cols x {} rows",
            cell.w, cell.h, cols, rows
        );

        // Terminal
        let mut term = Terminal::new(TerminalOptions {
            cols,
            rows,
            max_scrollback: (1000 * 1024).max(16 * 1024 * 1024),
        })
        .expect("Terminal::new");

        // PTY
        let (pty, child) = Pty::new(WinSize {
            cols,
            rows,
            xpixel: (cols as u32 * cell.w as u32) as u16,
            ypixel: (rows as u32 * cell.h as u32) as u16,
        })
        .expect("Pty::new");

        let effect_fd = pty.dup_raw().unwrap_or(-1);

        // Terminal effects
        term.on_pty_write({
            let fd = effect_fd;
            move |_t, data| {
                if fd >= 0 {
                    use std::os::fd::BorrowedFd;
                    let fd = unsafe { BorrowedFd::borrow_raw(fd) };
                    let mut rem = data;
                    while !rem.is_empty() {
                        match nix::unistd::write(fd, rem) {
                            Ok(len) => rem = &rem[len..],
                            Err(Errno::EINTR) => continue,
                            Err(_) => break,
                        }
                    }
                }
            }
        })
        .expect("on_pty_write");

        term.on_size({
            let (c, r, cw, ch) = (cols, rows, cell.w as u32, cell.h as u32);
            move |_term| {
                Some(libghostty_vt::terminal::SizeReportSize {
                    rows: r,
                    columns: c,
                    cell_width: cw,
                    cell_height: ch,
                })
            }
        })
        .expect("on_size");

        term.on_title_changed(|_term| {})
            .expect("on_title_changed");
        term.on_pwd_changed(|_term| {}).expect("on_pwd_changed");
        term.on_bell(|_term| {}).expect("on_bell");
        term.on_xtversion(|_term| Some("ghost-iced-spike"))
            .expect("on_xtversion");
        term.on_color_scheme(|_term| None)
            .expect("on_color_scheme");
        term.on_device_attributes(|_term| None)
            .expect("on_device_attributes");
        term.on_clipboard_write(|_term, _clip| Ok(()))
            .expect("on_clipboard_write");

        term.resize(cols, rows, cell.w as u32, cell.h as u32)
            .expect("resize");

        let render_state = RenderState::new().expect("RenderState::new");
        let row_it = RowIterator::new().expect("RowIterator::new");
        let cell_it = CellIterator::new().expect("CellIterator::new");

        let measurements = Arc::new(Measurements::new());

        let term_state = Arc::new(Mutex::new(TermState {
            term,
            render_state,
            row_it,
            cell_it,
            cell,
            dirty: true,
            effect_fd,
        }));

        // PTY -> iced: the reader thread feeds an iced-futures channel and the
        // subscription streams it. No timer, no polling, no writer thread.
        let (pty_tx, mut pty_rx) = iced::futures::channel::mpsc::unbounded::<PtyMsg>();
        pty.spawn_reader(pty_tx).expect("spawn_reader");

        // Prime: drain initial output (shell startup probes)
        {
            let start = Instant::now();
            let mut idle = Duration::ZERO;
            while start.elapsed() < Duration::from_millis(250) {
                match pty_rx.try_recv() {
                    Ok(PtyMsg::Data(d)) => {
                        let mut ts = term_state.lock().unwrap();
                        ts.term.vt_write(&d);
                        idle = Duration::ZERO;
                    }
                    Ok(PtyMsg::Eof) => break,
                    // Empty or disconnected: keep waiting for output until the
                    // 60ms idle threshold (or the 250ms priming budget).
                    Err(_) => {
                        idle += Duration::from_millis(2);
                        if idle > Duration::from_millis(60) {
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(2));
                    }
                }
            }
        }

        eprintln!("[spike] terminal primed, launching iced window...");

        let app = App {
            term_state,
            measurements,
            pty: Arc::new(pty),
            rx_slot: PtyRxSlot(Arc::new(Mutex::new(Some(pty_rx)))),
            child: Some(child),
            cell,
            cols,
            rows,
            exited: false,
        };

        (app, Task::none())
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::PtyMsg(PtyMsg::Data(data)) => {
                let mut ts = self.term_state.lock().unwrap();
                ts.term.vt_write(&data);
                ts.dirty = true;
                drop(ts);
                self.measurements.tick();
                Task::none()
            }
            Message::PtyMsg(PtyMsg::Eof) => {
                self.exited = true;
                eprintln!("[spike] pty eof");
                Task::none()
            }
            Message::KeyPress(key, mods) => {
                let bytes = encode_key_simple(&key, &mods);
                if !bytes.is_empty() {
                    self.pty.write(&bytes);
                }
                Task::none()
            }
        }
    }

    fn view(&self) -> Element<'_, Message> {
        let program = TerminalProgram {
            term_state: self.term_state.clone(),
            cell: self.cell,
            measurements: self.measurements.clone(),
        };
        Canvas::new(program).width(Fill).height(Fill).into()
    }

    fn subscription(&self) -> Subscription<Message> {
        // Input: ignored runtime events carry keyboard presses.
        let kb = iced::event::listen_with(kb_listen);

        // PTY data via channel-in-closure pattern (iced docs recommendation)
        // The closure creates a channel, bridges from the std mpsc pty reader,
        // and returns the receiver. iced wraps it in a Stream and delivers items.
        let pty_sub = Subscription::run_with(self.rx_slot.clone(), pty_stream);

        Subscription::batch([kb, pty_sub])
    }
}

// ---------------------------------------------------------------------------
// Canvas program
// ---------------------------------------------------------------------------
struct TerminalProgram {
    term_state: Arc<Mutex<TermState>>,
    cell: CellDims,
    measurements: Arc<Measurements>,
}

impl canvas::Program<Message> for TerminalProgram {
    type State = ();

    fn draw(
        &self,
        _tree: &Self::State,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        self.measurements.tick();

        let mut guard = self.term_state.lock().unwrap();
        // Split the guard into disjoint field borrows: MutexGuard derefs through
        // a trait, so render_state.update(&term) would fight the borrow checker
        // until the fields are destructured.
        let TermState {
            term,
            render_state,
            row_it,
            cell_it,
            ..
        } = &mut *guard;

        let ox = bounds.x + PADDING;
        let oy = bounds.y + PADDING;

        let mut frame = Frame::new(renderer, bounds.size());

        // Snapshot + colors
        let snapshot = match render_state.update(term) {
            Ok(s) => s,
            Err(_) => return vec![frame.into_geometry()],
        };
        let colors = match snapshot.colors() {
            Ok(c) => c,
            Err(_) => return vec![frame.into_geometry()],
        };

        // Background
        let bg = Color {
            r: colors.background.r as f32 / 255.0,
            g: colors.background.g as f32 / 255.0,
            b: colors.background.b as f32 / 255.0,
            a: 1.0,
        };
        frame.fill_rectangle(
            Point::new(bounds.x, bounds.y),
            iced::Size::new(bounds.width, bounds.height),
            bg,
        );

        // Rows
        let mut rows = match row_it.update(&snapshot) {
            Ok(it) => it,
            Err(_) => return vec![frame.into_geometry()],
        };

        let mut y = oy;
        let mut text_buf = String::with_capacity(128);

        while let Some(row) = rows.next() {
            let mut x = ox;
            let mut cells = match cell_it.update(row) {
                Ok(it) => it,
                Err(_) => continue,
            };

            while let Some(c) = cells.next() {
                text_buf.clear();
                let graphemes = c.graphemes_len().unwrap_or(0);

                let fg_color = if graphemes == 0 {
                    Color {
                        r: colors.foreground.r as f32 / 255.0,
                        g: colors.foreground.g as f32 / 255.0,
                        b: colors.foreground.b as f32 / 255.0,
                        a: 1.0,
                    }
                } else {
                    let _ = c.graphemes_utf8(&mut text_buf);
                    text_buf.retain(|ch| {
                        !matches!(
                            ch,
                            '\u{FE00}'..='\u{FE0F}'
                                | '\u{200B}'..='\u{200D}'
                                | '\u{2060}'
                                | '\u{FEFF}'
                        )
                    });

                    if let Ok(Some(fg)) = c.fg_color() {
                        Color {
                            r: fg.r as f32 / 255.0,
                            g: fg.g as f32 / 255.0,
                            b: fg.b as f32 / 255.0,
                            a: 1.0,
                        }
                    } else {
                        Color {
                            r: colors.foreground.r as f32 / 255.0,
                            g: colors.foreground.g as f32 / 255.0,
                            b: colors.foreground.b as f32 / 255.0,
                            a: 1.0,
                        }
                    }
                };

                // Cell background
                if let Ok(Some(bg_c)) = c.bg_color() {
                    let cell_bg = Color {
                        r: bg_c.r as f32 / 255.0,
                        g: bg_c.g as f32 / 255.0,
                        b: bg_c.b as f32 / 255.0,
                        a: 1.0,
                    };
                    frame.fill_rectangle(
                        Point::new(x, y),
                        iced::Size::new(self.cell.w, self.cell.h),
                        cell_bg,
                    );
                }

                // Text
                if !text_buf.is_empty() {
                    frame.fill_text(CanvasText {
                        content: text_buf.clone(),
                        position: Point::new(x, y + self.cell.baseline),
                        color: fg_color,
                        size: Pixels(FONT_PX as f32),
                        align_x: alignment::Horizontal::Left.into(),
                        align_y: alignment::Vertical::Top,
                        shaping: text::Shaping::Basic,
                        ..CanvasText::default()
                    });
                }

                x += self.cell.w;
            }

            let _ = row.set_dirty(false);
            y += self.cell.h;
        }

        // Cursor
        if let Ok(Some(vp)) = snapshot.cursor_viewport() {
            let cx = ox + vp.x as f32 * self.cell.w;
            let cy = oy + vp.y as f32 * self.cell.h;
            let cursor_color = colors
                .cursor
                .map(|cc| Color {
                    r: cc.r as f32 / 255.0,
                    g: cc.g as f32 / 255.0,
                    b: cc.b as f32 / 255.0,
                    a: 1.0,
                })
                .unwrap_or_else(|| Color {
                    r: colors.foreground.r as f32 / 255.0,
                    g: colors.foreground.g as f32 / 255.0,
                    b: colors.foreground.b as f32 / 255.0,
                    a: 1.0,
                });
            frame.fill_rectangle(
                Point::new(cx, cy),
                iced::Size::new(self.cell.w, self.cell.h),
                cursor_color,
            );
        }

        let _ = snapshot.set_dirty(Dirty::Clean);

        vec![frame.into_geometry()]
    }
}

// ---------------------------------------------------------------------------
// Simple key encoder
// ---------------------------------------------------------------------------
fn encode_key_simple(key: &Key, mods: &Modifiers) -> Vec<u8> {
    let ctrl = mods.contains(Modifiers::CTRL);

    match key {
        Key::Named(Named::Enter) => vec![b'\r'],
        Key::Named(Named::Backspace) => vec![0x08],
        Key::Named(Named::Tab) => vec![b'\t'],
        Key::Named(Named::Escape) => vec![0x1b],
        Key::Named(Named::ArrowUp) => {
            if ctrl {
                vec![0x1b, b'[', b'1', b';', b'5', b'A']
            } else {
                vec![0x1b, b'[', b'A']
            }
        }
        Key::Named(Named::ArrowDown) => {
            if ctrl {
                vec![0x1b, b'[', b'1', b';', b'5', b'B']
            } else {
                vec![0x1b, b'[', b'B']
            }
        }
        Key::Named(Named::ArrowRight) => {
            if ctrl {
                vec![0x1b, b'[', b'1', b';', b'5', b'C']
            } else {
                vec![0x1b, b'[', b'C']
            }
        }
        Key::Named(Named::ArrowLeft) => {
            if ctrl {
                vec![0x1b, b'[', b'1', b';', b'5', b'D']
            } else {
                vec![0x1b, b'[', b'D']
            }
        }
        Key::Character(c) => {
            let ch = c.as_str();
            if ctrl {
                if let Some(byte) = ch.bytes().next() {
                    if byte >= b'a' && byte <= b'z' {
                        return vec![byte - b'a' + 1];
                    }
                }
            }
            ch.as_bytes().to_vec()
        }
        _ => vec![],
    }
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------
fn main() -> iced::Result {
    iced::application(App::new, App::update, App::view)
        .subscription(App::subscription)
        .run()
}
