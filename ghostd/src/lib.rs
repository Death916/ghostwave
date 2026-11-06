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

/// `ghostd attach` exit status: there is no session to attach to.
///
/// This is the pre-existing meaning, kept so a daemon that predates the code
/// still works: a caller may create the session. It is also what a session that
/// is *busy* — a client is already attached elsewhere — returns, because the
/// refusal arrives as the handshake's first bytes and never as an `OK\n` ack.
/// A caller must not treat 1 alone as "safe to recreate": it must let `new`'s
/// probe refuse a session that is merely in use.
pub const ATTACH_NO_SESSION: i32 = 1;

/// `ghostd attach` exit status: the daemon holds the session but never answered
/// the handshake (wedged). The caller recovers the daemon and attaches again.
pub const ATTACH_WEDGED: i32 = 3;

/// `ghostd attach` exit status: the session exists but is over — the daemon
/// closed the connection before acking, or the relay ended after the session
/// was acked. A caller must not recreate it: this is the code that stops a
/// duplicate pane from resurrecting a killed session.
pub const ATTACH_ENDED: i32 = 4;

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

/// Terminate a wedged daemon and drop its state so `new` can rebind the session
/// fresh. Returns whether the old daemon is gone.
///
/// The pidfile is the only source of the victim: a session with no pidfile has
/// nothing that names an owner, so this returns `false` and the caller reports
/// it rather than guessing. SIGTERM, a 1 s grace, then SIGKILL, and the verdict
/// is a liveness check after the escalation — a daemon that survives both is
/// left alone, state and all, because binding a second daemon over a live one is
/// the failure this function exists to remove. The session's pty child is killed
/// with the daemon, so a recovered session does not orphan its program.
pub fn recover_session(name: &str) -> bool {
    let Some((daemon, child)) = read_pidfile(name) else {
        gdbg("recover", &format!("{name}: no pidfile names a daemon"));
        return false;
    };
    gdbg("recover", &format!("{name}: SIGTERM daemon {daemon}"));
    let _ = signal::kill(daemon, Signal::SIGTERM);
    for _ in 0..10 {
        if pid_gone(daemon) {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    if !pid_gone(daemon) {
        // A stopped daemon (what a wedged one looks like) does not act on
        // SIGTERM until it is continued, so SIGKILL is the escalation that
        // actually ends it. Give the kernel and pid 1 a moment to finish.
        gdbg("recover", &format!("{name}: daemon {daemon} stuck, SIGKILL"));
        let _ = signal::kill(daemon, Signal::SIGKILL);
        for _ in 0..10 {
            if pid_gone(daemon) {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    if !pid_gone(daemon) {
        return false;
    }
    if let Some(child) = child {
        kill_child_group(child, Duration::from_secs(2));
    }
    remove_state_if_owned(name, daemon);
    true
}

/// The daemon and pty-child pids `<name>.pid` records.
///
/// The file is two lines (daemon, then the session's program) for every daemon
/// that writes it; a file from an older daemon holds one line, so the child is
/// `None`. `None` overall means there is no pidfile or its first line does not
/// parse as a pid — nothing that names an owner.
///
/// Both pids are strictly positive on the way out. A pid of `0` or below is not a
/// pid a signal may be aimed at: `kill(0, sig)` signals the caller's own process
/// group and `kill(-1, sig)` every process the caller may signal, so a stale or
/// corrupt line must never become a victim. An unusable daemon line therefore
/// names no owner at all, while an unusable child line only costs the child: the
/// daemon pid still names a real owner, so the session stays killable and
/// recoverable instead of being pinned by a file we refuse to read.
pub fn read_pidfile(name: &str) -> Option<(Pid, Option<Pid>)> {
    let text = fs::read_to_string(pid_path(name)).ok()?;
    let mut lines = text.lines();
    let daemon = parse_pid(lines.next()?)?;
    let child = lines.next().and_then(parse_pid);
    Some((daemon, child))
}

/// One pidfile line as a pid a signal may be aimed at: strictly positive.
///
/// Anything else — zero, a negative value, a non-number — is `None`.
fn parse_pid(line: &str) -> Option<Pid> {
    let raw = line.trim().parse::<i32>().ok()?;
    (raw > 0).then(|| Pid::from_raw(raw))
}

/// Whether `<name>.pid` still names `daemon` as the session's owner.
///
/// The ownership test every state removal goes through: a daemon may delete only
/// the state it wrote, so a dying daemon cannot unlink its successor's socket and
/// pidfile (which would leave a live daemon unreachable *and* unkillable).
pub fn pidfile_names(name: &str, daemon: Pid) -> bool {
    read_pidfile(name).is_some_and(|(owner, _)| owner == daemon)
}

/// Write `<name>.pid` through `<name>.pid.tmp` and a rename, so a concurrent
/// reader never sees a half-written file and concludes the session has no owner.
pub fn write_pidfile(name: &str, daemon: Pid, child: Option<Pid>) {
    let mut body = daemon.as_raw().to_string();
    if let Some(child) = child {
        body.push('\n');
        body.push_str(&child.as_raw().to_string());
    }
    let tmp = state_dir().join(format!("{name}.pid.tmp"));
    if fs::write(&tmp, body).is_ok() {
        let _ = fs::rename(&tmp, pid_path(name));
    }
}

/// Remove `<name>.sock` and `<name>.pid` only when the pidfile still names
/// `daemon`. Returns whether the state was removed (and therefore whether the
/// caller may also drop the log, which belongs to that same daemon).
pub fn remove_state_if_owned(name: &str, daemon: Pid) -> bool {
    if !pidfile_names(name, daemon) {
        gdbg("state", &format!("{name}: not owned by {daemon}, left alone"));
        return false;
    }
    let _ = fs::remove_file(socket_path(name));
    let _ = fs::remove_file(pid_path(name));
    true
}

/// Whether `pid` can no longer serve anything: gone, or killed and waiting to be
/// reaped. `kill(pid, 0)` alone counts a zombie as alive, which would make every
/// post-SIGKILL verdict wrong.
pub fn pid_gone(pid: Pid) -> bool {
    if !pid_alive(pid) {
        return true;
    }
    match fs::read_to_string(format!("/proc/{}/stat", pid.as_raw())) {
        Ok(stat) => stat
            .rsplit_once(')')
            .map(|(_, rest)| rest.trim_start().starts_with('Z'))
            .unwrap_or(false),
        Err(_) => true,
    }
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

/// Kill the pty child and its whole process group (the child is a session
/// leader, so pgid == pid): SIGTERM, escalate to SIGKILL after `grace`.
///
/// A nonpositive `child` names no group this may signal, and the call returns
/// without touching any process: the group is `-child`, so `Pid(0)` would
/// become `kill(0, ..)` — the caller's own process group — and a negative
/// `child` would be inverted into a positive pid belonging to an unrelated
/// process. (`Pid(i32::MIN)` would also overflow the negation.)
///
/// Safe to call on a dead pid — errors are ignored. Used by the daemon's
/// own cleanup AND by `ghostd kill` so a wedged daemon can't orphan the
/// session's program.
pub fn kill_child_group(child: Pid, grace: Duration) {
    if child.as_raw() <= 0 {
        return;
    }
    let group = Pid::from_raw(-child.as_raw());
    let _ = signal::kill(group, Signal::SIGTERM);
    let deadline = std::time::Instant::now() + grace;
    while std::time::Instant::now() < deadline {
        if !pid_alive(child) {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    if pid_alive(child) {
        gdbg("kill", &format!("child {} stuck, SIGKILL group", child.as_raw()));
        let _ = signal::kill(group, Signal::SIGKILL);
    }
}

// ---------------------------------------------------------------------------
// daemon plumbing

/// Spawn the pty child (the session's program). Returns the non-blocking
/// master fd and the child's pid (== its process group: forkpty makes the
/// child a session leader, so a negative-pid kill reaches its whole group).
pub fn spawn_child(cmd: &[String]) -> (i32, Pid) {
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
        Ok(ForkptyResult::Parent { master, child }) => {
            set_nonblock(master.as_raw_fd());
            (master.into_raw_fd(), child)
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard, OnceLock};

    /// The tests point `GHOSTWAVE_STATE_DIR` at their own directory, so they hold
    /// this lock for their whole run: the variable is process-wide, and a second
    /// test setting it mid-run would aim one test's writes at another's dir.
    fn env_lock() -> MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// A private state dir with `GHOSTWAVE_STATE_DIR` pointed at it; the returned
    /// guard must outlive everything that touches the state.
    fn state_dir_for(tag: &str) -> (MutexGuard<'static, ()>, PathBuf) {
        let guard = env_lock();
        let dir = std::env::temp_dir().join(format!("ghostd-lib-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("a state dir is created");
        // SAFETY: `set_var` is unsafe in edition 2024 because it races any other
        // thread that reads the environment. The caller holds `env_lock` for the
        // rest of the test, and no other thread in this binary reads the variable.
        unsafe { std::env::set_var("GHOSTWAVE_STATE_DIR", &dir) };
        (guard, dir)
    }

    #[test]
    fn pidfile_names_reads_the_two_line_file() {
        let (_guard, dir) = state_dir_for("read");
        let daemon = Pid::from_raw(100);
        let child = Pid::from_raw(200);

        write_pidfile("s", daemon, Some(child));
        assert_eq!(read_pidfile("s"), Some((daemon, Some(child))));
        assert_eq!(
            fs::read_to_string(dir.join("s.pid")).expect("the pidfile is written"),
            "100\n200",
            "daemon first, then the session's program"
        );
        assert!(pidfile_names("s", daemon));
        assert!(!pidfile_names("s", child), "the child is never the owner");

        fs::write(dir.join("s.pid"), "101\n").expect("a one-line file is written");
        assert_eq!(
            read_pidfile("s"),
            Some((Pid::from_raw(101), None)),
            "a file from a daemon that predates the child pid still names its owner"
        );

        fs::write(dir.join("s.pid"), "not a pid\n").expect("a garbage file is written");
        assert_eq!(read_pidfile("s"), None, "garbage names no owner");
        assert!(!pidfile_names("s", Pid::from_raw(101)));
        assert_eq!(read_pidfile("absent"), None, "no pidfile names no owner");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn pidfile_with_a_nonpositive_daemon_names_no_owner() {
        let (_guard, dir) = state_dir_for("bad-daemon");
        // kill(0, ..) signals the caller's own process group and kill(-1, ..)
        // signals everything the caller may signal, so neither may ever come out
        // of the parser as a daemon.
        for raw in ["0", "-1", "-4242"] {
            fs::write(dir.join("s.pid"), format!("{raw}\n200")).expect("a pidfile is written");
            fs::write(dir.join("s.sock"), b"").expect("a socket file is written");

            assert_eq!(
                read_pidfile("s"),
                None,
                "pidfile daemon {raw} names no owner"
            );
            let named = Pid::from_raw(raw.parse::<i32>().expect("the test wrote an i32"));
            assert!(!pidfile_names("s", named), "and does not own the name");
            assert!(
                !remove_state_if_owned("s", named),
                "so no state removal runs for daemon {raw}"
            );
            assert!(
                dir.join("s.sock").exists() && dir.join("s.pid").exists(),
                "the files of an unparseable pidfile are left alone"
            );
        }

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn pidfile_with_a_nonpositive_child_drops_the_child() {
        let (_guard, dir) = state_dir_for("bad-child");
        // The daemon pid is still a real owner, so the session stays recoverable;
        // the child is what must not survive, because kill_child_group would turn
        // it into kill(0, ..) / kill(-1, ..).
        for raw in ["0", "-1", "-4242"] {
            fs::write(dir.join("s.pid"), format!("100\n{raw}")).expect("a pidfile is written");

            assert_eq!(
                read_pidfile("s"),
                Some((Pid::from_raw(100), None)),
                "child {raw} is dropped, the daemon kept"
            );
            assert!(
                pidfile_names("s", Pid::from_raw(100)),
                "the daemon still owns the name"
            );
        }

        fs::write(dir.join("s.pid"), "100\nnot a pid").expect("a garbage child is written");
        assert_eq!(
            read_pidfile("s"),
            Some((Pid::from_raw(100), None)),
            "a malformed child line is not a child"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn invalid_pidfile_drives_no_signal_or_state_removal() {
        let (_guard, dir) = state_dir_for("no-signal");
        fs::write(dir.join("s.pid"), "0\n0").expect("a pidfile is written");
        fs::write(dir.join("s.sock"), b"").expect("a socket file is written");

        // recover_session is the path that SIGTERMs a daemon and then the child's
        // process group: with no owner named it must not reach either.
        assert!(
            !recover_session("s"),
            "a pidfile with no owner recovers nothing"
        );
        assert!(
            dir.join("s.sock").exists() && dir.join("s.pid").exists(),
            "and removes no state"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn state_files_are_removed_only_by_their_owner() {
        let (_guard, dir) = state_dir_for("own");
        let owner = Pid::from_raw(100);
        write_pidfile("s", owner, None);
        fs::write(dir.join("s.sock"), b"").expect("a socket file is written");

        assert!(
            !remove_state_if_owned("s", Pid::from_raw(200)),
            "a daemon that does not own the name removes nothing"
        );
        assert!(dir.join("s.sock").exists() && dir.join("s.pid").exists());

        assert!(
            remove_state_if_owned("s", owner),
            "the owner removes its own state"
        );
        assert!(!dir.join("s.sock").exists() && !dir.join("s.pid").exists());

        let _ = fs::remove_dir_all(&dir);
    }

    /// Pipe end the probe's SIGTERM handler writes to (`-1` until installed).
    static PROBE_WITNESS: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(-1);

    /// Async-signal-safe witness: one byte per SIGTERM that reaches the probe.
    extern "C" fn probe_on_term(_: i32) {
        let fd = PROBE_WITNESS.load(Ordering::SeqCst);
        if fd >= 0 {
            // SAFETY: the probe owns this pipe end until it exits, and write(2)
            // is async-signal-safe.
            unsafe { nix::libc::write(fd, b"!".as_ptr().cast(), 1) };
        }
    }

    /// Fork a probe that leads its own process group, records every SIGTERM it
    /// receives on a pipe, runs `body` and then exits 0.
    ///
    /// Returns only once the probe is ready to be signalled: it has called
    /// `setsid` (so its process group holds nothing but the probe, and a
    /// `kill(0, ..)` inside it can never reach the test harness) and has its
    /// SIGTERM handler in place. Signalling earlier would land on a group that
    /// does not exist yet, or kill the probe before it could witness anything.
    ///
    /// Returns the probe's pid and the read end of its witness pipe.
    fn signal_probe(body: impl FnOnce()) -> (Pid, i32) {
        let mut witness = [0i32; 2];
        let mut ready = [0i32; 2];
        assert_eq!(
            unsafe { nix::libc::pipe(witness.as_mut_ptr()) },
            0,
            "the witness pipe is created"
        );
        assert_eq!(
            unsafe { nix::libc::pipe(ready.as_mut_ptr()) },
            0,
            "the ready pipe is created"
        );
        match unsafe { unistd::fork() }.expect("the probe forks") {
            unistd::ForkResult::Parent { child } => {
                let _ = unsafe { nix::libc::close(witness[1]) };
                let _ = unsafe { nix::libc::close(ready[1]) };
                let mut byte = [0u8; 1];
                let n = unsafe { nix::libc::read(ready[0], byte.as_mut_ptr().cast(), 1) };
                let _ = unsafe { nix::libc::close(ready[0]) };
                assert!(
                    n == 1 && byte[0] == b'r',
                    "the probe is ready with its SIGTERM handler in place: {byte:?}"
                );
                (child, witness[0])
            }
            unistd::ForkResult::Child => {
                let _ = unsafe { nix::libc::close(witness[0]) };
                let _ = unsafe { nix::libc::close(ready[0]) };
                let _ = unistd::setsid();
                PROBE_WITNESS.store(witness[1], Ordering::SeqCst);
                let installed = unsafe {
                    signal::sigaction(
                        Signal::SIGTERM,
                        &SigAction::new(
                            SigHandler::Handler(probe_on_term),
                            SaFlags::empty(),
                            SigSet::empty(),
                        ),
                    )
                };
                let flag = if installed.is_ok() { b"r" } else { b"e" };
                let _ = unsafe { nix::libc::write(ready[1], flag.as_ptr().cast(), 1) };
                let _ = unsafe { nix::libc::close(ready[1]) };
                body();
                unsafe { nix::libc::_exit(0) };
            }
        }
    }

    /// Bytes the probe wrote to its witness pipe. Only called after the probe
    /// was reaped, so a signal that never arrived reads as end-of-file (`0`).
    fn witness_bytes(read_fd: i32) -> usize {
        let mut seen = 0;
        let mut buf = [0u8; 8];
        loop {
            let n = unsafe { nix::libc::read(read_fd, buf.as_mut_ptr().cast(), buf.len()) };
            if n <= 0 {
                break;
            }
            seen += n as usize;
        }
        seen
    }

    #[test]
    fn nonpositive_child_is_never_signalled() {
        // Each of these, without the guard, aims a signal at something the
        // caller never named: 0 at the caller's own process group, -own at the
        // caller itself, -1 at every process it may signal, and i32::MIN at
        // both an unrelated pid and an overflow of the negation. (An arbitrary
        // negative value is the same negation as -own, so it is not repeated
        // here: on a regression it could signal a stranger.)
        let (probe, witness) = signal_probe(|| {
            let own = unistd::getpid();
            for raw in [0, -own.as_raw(), -1, i32::MIN] {
                kill_child_group(Pid::from_raw(raw), Duration::from_millis(100));
            }
        });

        let status = nix::sys::wait::waitpid(probe, None).expect("the probe is reaped");
        let seen = witness_bytes(witness);
        let _ = unsafe { nix::libc::close(witness) };
        assert_eq!(
            seen, 0,
            "no signalled process group or pid reached the probe"
        );
        assert!(
            matches!(status, nix::sys::wait::WaitStatus::Exited(_, 0)),
            "the probe ran to the end of its body: {status:?}"
        );
    }

    #[test]
    fn positive_child_is_still_signalled_with_its_group() {
        // The guard must not change the live path: SIGTERM to the child's group
        // (the probe witnesses it), then SIGKILL when the grace runs out with
        // the child still up.
        let (probe, witness) = signal_probe(|| {
            std::thread::sleep(Duration::from_secs(30));
        });

        kill_child_group(probe, Duration::from_millis(150));

        let status = nix::sys::wait::waitpid(probe, None).expect("the probe is reaped");
        let seen = witness_bytes(witness);
        let _ = unsafe { nix::libc::close(witness) };
        assert_eq!(seen, 1, "the child's process group received the SIGTERM");
        assert!(
            matches!(
                status,
                nix::sys::wait::WaitStatus::Signaled(_, Signal::SIGKILL, _)
            ),
            "and the grace ran out into a SIGKILL of the same group: {status:?}"
        );
    }
}
