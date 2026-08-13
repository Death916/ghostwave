//! ghostd — the ghostwave remote session daemon library.
//!
//! Headless, UI-less durable remote sessions (waveterm-style): a session
//! survives the ssh client dying; reattaching replays the ring log then
//! resumes live. This library exposes the shared machinery (state dirs,
//! fd/poll helpers, daemon plumbing, size protocol) so the CLI binary and
//! any embedding host (e.g. the ghostwave app itself) share one
//! implementation.

use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::{
        fd::{AsRawFd, IntoRawFd},
        unix::net::{UnixListener, UnixStream},
        unix::process::CommandExt,
    },
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

use nix::{
    errno::Errno,
    pty::{ForkptyResult, Winsize},
    sys::{
        signal::{self, SigAction, SigHandler, SaFlags, SigSet, Signal},
    },
    unistd::{self, Pid},
};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const DEFAULT_LOG_CAP_MB: u64 = 32;
pub const DEFAULT_FIRST_ATTACH_SECS: u64 = 300;
pub const ATTACH_RETRY_SECS: u64 = 5;

/// Set when SIGTERM arrives (ghostd kill); the accept loop cleans up.
pub static TERM_REQUESTED: AtomicBool = AtomicBool::new(false);

/// Debug trace: eprintln + append to ~/.local/share/ghostwave/debug.log when
/// GHOSTD_DEBUG is set. The daemon's stderr is /dev/null after daemonize, so
/// the file is the only channel there; attach/new run over ssh -t so eprintln
/// also lands in the pane.
pub fn gdbg(tag: &str, msg: &str) {
    if std::env::var_os("GHOSTD_DEBUG").is_none() {
        return;
    }
    eprintln!("[ghostd:{tag}] {msg}");
    if let Ok(home) = std::env::var("HOME") {
        let p = PathBuf::from(home).join(".local/share/ghostwave/debug.log");
        let _ = OpenOptions::new()
            .create(true)
            .append(true)
            .open(p)
            .map(|mut f| {
                let _ = writeln!(f, "[{tag}] {msg}");
            });
    }
}

/// Outcome of probing an existing session's daemon before `new`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Probe {
    /// No reachable daemon (stale socket) — safe to rebind.
    NoSession,
    /// A live daemon answered the SIZE handshake — session is in use.
    Healthy,
    /// A daemon accepted the connection but never answered — wedged.
    Unresponsive,
}

/// Ask an existing session's daemon whether it is alive: send the SIZE
/// handshake and wait (bounded) for the daemon's "OK\n" ack. Daemons
/// before the ack protocol replay the log instead; any byte counts as
/// healthy. Unresponsive = accepted but silent (stuck daemon / dead
/// client-handler) — the caller kills it and rebinds.
pub fn probe_session(name: &str) -> Probe {
    let sock = socket_path(name);
    let Ok(mut s) = UnixStream::connect(&sock) else {
        return Probe::NoSession; // stale socket
    };
    let _ = s.write_all(b"SIZE 100 30\n");
    let _ = set_nonblock(s.as_raw_fd());
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut buf = [0u8; 64];
    loop {
        if Instant::now() >= deadline {
            gdbg("probe", &format!("{name}: no response in 2s -> Unresponsive"));
            return Probe::Unresponsive;
        }
        match fd_poll(&mut [(s.as_raw_fd(), true)], 200) {
            Ok(r) if !r.is_empty() => match fd_read(s.as_raw_fd(), &mut buf) {
                Ok(0) => return Probe::NoSession,
                Ok(n) => {
                    if n > 0 {
                        return Probe::Healthy;
                    }
                }
                Err(Errno::EAGAIN) => {}
                Err(_) => return Probe::NoSession,
            },
            _ => {}
        }
    }
}

/// Terminate a wedged daemon (SIGTERM, then SIGKILL after a 1s grace) and
/// drop its state files so `new` can rebind the session fresh.
pub fn recover_session(name: &str) {
    let pid_file = pid_path(name);
    if let Ok(pid_str) = fs::read_to_string(&pid_file) {
        if let Ok(pid) = pid_str.trim().parse::<i32>() {
            gdbg("recover", &format!("{name}: SIGTERM daemon {pid}"));
            let _ = signal::kill(Pid::from_raw(pid), Signal::SIGTERM);
            for _ in 0..10 {
                if !pid_alive(Pid::from_raw(pid)) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            if pid_alive(Pid::from_raw(pid)) {
                gdbg("recover", &format!("{name}: daemon {pid} stuck, SIGKILL"));
                let _ = signal::kill(Pid::from_raw(pid), Signal::SIGKILL);
            }
        }
    }
    let _ = fs::remove_file(socket_path(name));
    let _ = fs::remove_file(pid_file);
}

pub fn die(msg: &str) -> ! {
    eprintln!("ghostd: {msg}");
    std::process::exit(1);
}

/// Daemon state dir: `$GHOSTWAVE_STATE_DIR` or `~/.local/share/ghostwave`.
pub fn state_dir() -> PathBuf {
    if let Ok(d) = std::env::var("GHOSTWAVE_STATE_DIR") {
        return PathBuf::from(d);
    }
    let home = std::env::var_os("HOME").unwrap_or_default();
    PathBuf::from(home).join(".local/share/ghostwave")
}

pub fn socket_path(name: &str) -> PathBuf {
    state_dir().join(format!("{name}.sock"))
}
pub fn log_path(name: &str) -> PathBuf {
    state_dir().join(format!("{name}.log"))
}
pub fn pid_path(name: &str) -> PathBuf {
    state_dir().join(format!("{name}.pid"))
}

/// Returns (cols, rows) of the terminal on `fd` (TIOCGWINSZ).
pub fn term_size(fd: i32) -> Option<(u16, u16)> {
    nix::ioctl_read_bad!(tiocgwinsz, nix::libc::TIOCGWINSZ, Winsize);
    let mut ws = Winsize { ws_row: 0, ws_col: 0, ws_xpixel: 0, ws_ypixel: 0 };
    unsafe { tiocgwinsz(fd, &mut ws) }.ok()?;
    Some((ws.ws_col, ws.ws_row))
}

pub fn set_term_size(fd: i32, cols: u16, rows: u16) {
    nix::ioctl_write_ptr_bad!(tiocswinsz, nix::libc::TIOCSWINSZ, Winsize);
    let ws = Winsize { ws_col: cols, ws_row: rows, ws_xpixel: 0, ws_ypixel: 0 };
    unsafe { tiocswinsz(fd, &ws) }.ok();
}

pub fn set_nonblock(fd: i32) {
    let flags = unsafe { nix::libc::fcntl(fd, nix::libc::F_GETFL) };
    if flags >= 0 {
        unsafe { nix::libc::fcntl(fd, nix::libc::F_SETFL, flags | nix::libc::O_NONBLOCK) };
    }
}

pub fn fd_read(fd: i32, buf: &mut [u8]) -> Result<usize, Errno> {
    let n = unsafe { nix::libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
    if n < 0 {
        Err(Errno::last())
    } else {
        Ok(n as usize)
    }
}

pub fn fd_write(fd: i32, buf: &[u8]) -> Result<usize, Errno> {
    let n = unsafe { nix::libc::write(fd, buf.as_ptr().cast(), buf.len()) };
    if n < 0 {
        Err(Errno::last())
    } else {
        Ok(n as usize)
    }
}

/// poll() over (fd, want_read) pairs; returns the indices that are ready.
pub fn fd_poll(pairs: &mut [(i32, bool)], timeout_ms: i32) -> Result<Vec<usize>, Errno> {
    let mut fds: Vec<nix::libc::pollfd> = pairs
        .iter()
        .map(|(fd, r)| nix::libc::pollfd {
            fd: *fd,
            events: if *r { nix::libc::POLLIN } else { 0 },
            revents: 0,
        })
        .collect();
    let rc = unsafe { nix::libc::poll(fds.as_mut_ptr(), fds.len() as _, timeout_ms) };
    if rc < 0 {
        return Err(Errno::last());
    }
    Ok(fds
        .iter()
        .enumerate()
        .filter(|(_, f)| f.revents & (nix::libc::POLLIN | nix::libc::POLLHUP | nix::libc::POLLERR) != 0)
        .map(|(i, _)| i)
        .collect())
}

pub fn write_all_blocking(fd: i32, mut data: &[u8]) -> std::io::Result<()> {
    while !data.is_empty() {
        match fd_write(fd, data) {
            Ok(0) => break,
            Ok(n) => data = &data[n..],
            Err(Errno::EINTR) => continue,
            Err(Errno::EAGAIN) => std::thread::sleep(Duration::from_millis(1)),
            Err(e) => return Err(std::io::Error::from(e)),
        }
    }
    Ok(())
}

pub fn pid_alive(pid: Pid) -> bool {
    signal::kill(pid, None).is_ok()
}

// ---------------------------------------------------------------------------
// daemon plumbing

/// Spawn the pty child (the session's program). Returns the non-blocking
/// master fd.
pub fn spawn_child(cmd: &[String]) -> i32 {
    let (shell, args): (PathBuf, Vec<String>) = if !cmd.is_empty() {
        (PathBuf::from(&cmd[0]), cmd[1..].to_vec())
    } else {
        // Login shell via passwd (same policy as the ghostwave pane).
        let s = match unistd::User::from_uid(unistd::getuid()) {
            Ok(Some(u)) if !u.shell.as_os_str().is_empty() => u.shell,
            _ => PathBuf::from("/bin/sh"),
        };
        (s, vec![])
    };
    let ws = Winsize { ws_row: 24, ws_col: 80, ws_xpixel: 0, ws_ypixel: 0 };
    match unsafe { nix::pty::forkpty(&ws, None) } {
        Ok(ForkptyResult::Child) => {
            let arg0 = shell.file_name().unwrap_or(shell.as_os_str()).to_os_string();
            let mut c = Command::new(&shell);
            c.env("TERM", "xterm-256color").arg0(arg0);
            for a in &args {
                c.arg(a);
            }
            _ = c.exec();
            std::process::exit(127);
        }
        Ok(ForkptyResult::Parent { master, .. }) => {
            set_nonblock(master.as_raw_fd());
            master.into_raw_fd()
        }
        Err(e) => {
            eprintln!("ghostd: forkpty failed: {e}");
            std::process::exit(1);
        }
    }
}

/// Double-fork + setsid + stdio detach + SIGHUP-immune, so the daemon
/// survives the ssh session teardown.
pub fn daemonize() {
    match unsafe { unistd::fork() } {
        Ok(unistd::ForkResult::Parent { .. }) => std::process::exit(0),
        Ok(unistd::ForkResult::Child) => {}
        Err(e) => {
            eprintln!("ghostd: fork failed: {e}");
            std::process::exit(1);
        }
    }
    _ = unistd::setsid();
    match unsafe { unistd::fork() } {
        Ok(unistd::ForkResult::Parent { .. }) => std::process::exit(0),
        Ok(unistd::ForkResult::Child) => {}
        Err(_) => std::process::exit(1),
    }
    // Detach stdio.
    if let Ok(null) = OpenOptions::new().read(true).write(true).open("/dev/null") {
        let fd = std::os::fd::AsRawFd::as_raw_fd(&null);
        unsafe {
            nix::libc::dup2(fd, 0);
            nix::libc::dup2(fd, 1);
            nix::libc::dup2(fd, 2);
        }
    }
    // Survive the ssh session teardown.
    unsafe {
        signal::sigaction(
            Signal::SIGHUP,
            &SigAction::new(SigHandler::SigIgn, SaFlags::empty(), SigSet::empty()),
        )
        .ok();
    }
}

pub fn install_term_handler() {
    extern "C" fn on_term(_: i32) {
        TERM_REQUESTED.store(true, Ordering::SeqCst);
    }
    unsafe {
        signal::sigaction(
            Signal::SIGTERM,
            &SigAction::new(SigHandler::Handler(on_term), SaFlags::empty(), SigSet::empty()),
        )
        .ok();
    }
}

/// Front-truncate `path` to keep at most `cap` trailing bytes (ring log).
pub fn truncate_log_front(path: &Path, cap: u64) {
    let Ok(meta) = fs::metadata(path) else { return };
    if meta.len() <= cap {
        return;
    }
    let Ok(mut f) = File::open(path) else { return };
    let skip = meta.len() - cap;
    use std::io::Seek;
    if f.seek(std::io::SeekFrom::Start(skip)).is_err() {
        return;
    }
    let mut tail = Vec::with_capacity(cap as usize);
    if f.read_to_end(&mut tail).is_err() {
        return;
    }
    let _ = fs::write(path, &tail);
}

/// Connect to a session socket, retrying briefly (the daemon may still be
/// binding right after `new`).
pub fn connect_with_retry(name: &str) -> Option<UnixStream> {
    let sock = socket_path(name);
    let deadline = Instant::now() + Duration::from_secs(ATTACH_RETRY_SECS);
    while Instant::now() < deadline {
        match UnixStream::connect(&sock) {
            Ok(s) => return Some(s),
            Err(_) => std::thread::sleep(Duration::from_millis(100)),
        }
    }
    None
}

/// The DCS size message used for mid-session resize (client -> daemon).
/// `\x1bPgw-size <cols> <rows>\x1b\\`
pub fn dcs_size_msg(cols: u16, rows: u16) -> String {
    format!("\x1bPgw-size {cols} {rows}\x1b\\")
}

/// Strip `\x1bPgw-size <c> <r>\x1b\\` sequences from a client→pty byte
/// stream, applying each size to `pty_fd`. Returns the cleaned bytes.
pub fn strip_dcs_sizes(buf: &[u8], pty_fd: i32) -> Vec<u8> {
    const HEAD: &[u8] = b"\x1bPgw-size ";
    let mut out = Vec::with_capacity(buf.len());
    let mut i = 0;
    while i < buf.len() {
        if buf[i..].starts_with(HEAD) {
            // find the terminator ST (ESC \)
            if let Some(rel) = find_seq(&buf[i + HEAD.len()..], b"\x1b\\") {
                let params = &buf[i + HEAD.len()..i + HEAD.len() + rel];
                let spec: String = params.iter().map(|&b| b as char).collect();
                let mut it = spec.split_whitespace();
                if let (Some(c), Some(r)) = (it.next(), it.next()) {
                    if let (Ok(c), Ok(r)) = (c.parse::<u16>(), r.parse::<u16>()) {
                        set_term_size(pty_fd, c, r);
                    }
                }
                i += HEAD.len() + rel + 2; // skip past ST
                continue;
            }
        }
        out.push(buf[i]);
        i += 1;
    }
    out
}

fn find_seq(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Bind the session listener, cleaning up stale files first.
pub fn bind_listener(name: &str) -> std::io::Result<UnixListener> {
    let dir = state_dir();
    fs::create_dir_all(&dir).ok();
    let sock = socket_path(name);
    if sock.exists() {
        // Stale socket from a crashed daemon? If a live daemon holds it,
        // binding fails and the caller reports "session exists".
        if UnixStream::connect(&sock).is_ok() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!("session '{name}' already exists"),
            ));
        }
        fs::remove_file(&sock).ok();
    }
    UnixListener::bind(&sock)
}
