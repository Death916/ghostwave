//! The daemon's lifecycle, end to end: each test drives the real `ghostd` binary
//! in its own state directory, because everything asserted here — who owns the
//! state files, which exit status an attach reports, whether a new session
//! replays a dead one's log — only exists across processes.
//!
//! Nothing here touches a real session: every child is started with
//! `GHOSTWAVE_STATE_DIR` pointed at a private temporary directory.

use std::{
    fs,
    io::{Read, Write},
    path::PathBuf,
    process::{Child, ChildStdin, Command, Stdio},
    sync::{Arc, Mutex, MutexGuard, OnceLock},
    time::{Duration, Instant},
};

use nix::{
    sys::signal::{kill, Signal},
    unistd::Pid,
};

/// The daemon binary Cargo built for this test target.
const BIN: &str = env!("CARGO_BIN_EXE_ghostd");

/// The tests point the process-wide `GHOSTWAVE_STATE_DIR` at their own
/// directory, so each holds this for its whole run: two tests setting it at once
/// would aim one test's daemon at the other test's state.
fn env_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// A short, unique state directory for one test.
///
/// The session socket lives inside it and a Unix socket path must fit the
/// kernel's `sun_path` (a little over 100 bytes), so the name is short and a
/// `TMPDIR` too deep to leave room — nix-shell's own is ~80 bytes — falls back to
/// the plain system temp dir, which always fits.
fn sandbox_dir(tag: &str, unique: u128) -> PathBuf {
    let name = format!("gd-{tag}-{:010}", unique % 10_000_000_000);
    let preferred = std::env::temp_dir().join(&name);
    if preferred.join("session.sock").as_os_str().len() < 100 {
        preferred
    } else {
        std::path::Path::new("/tmp").join(name)
    }
}

/// One test's private state directory, plus the session names it created so the
/// guard can make sure no daemon outlives the test.
struct Sandbox {
    _lock: MutexGuard<'static, ()>,
    dir: PathBuf,
    names: Vec<String>,
}

impl Sandbox {
    fn new(tag: &str) -> Self {
        let lock = env_lock();
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("the clock is past the epoch")
            .as_nanos();
        let dir = sandbox_dir(tag, unique);
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("a state dir is created");
        // SAFETY: `set_var` is unsafe in edition 2024 because it races any thread
        // that reads the environment. The caller holds `env_lock` for the rest of
        // the test, and this test binary reads the variable from no other thread:
        // the daemons and clients are separate processes and get it via `.env`.
        unsafe { std::env::set_var("GHOSTWAVE_STATE_DIR", &dir) };
        Self {
            _lock: lock,
            dir,
            names: Vec::new(),
        }
    }

    fn path(&self, file: &str) -> PathBuf {
        self.dir.join(file)
    }

    /// Run `ghostd <args>` to completion in this sandbox.
    fn run(&self, args: &[&str]) -> std::process::Output {
        Command::new(BIN)
            .args(args)
            .env("GHOSTWAVE_STATE_DIR", &self.dir)
            .output()
            .expect("ghostd runs")
    }

    fn new_session(&mut self, name: &str) -> std::process::Output {
        self.names.push(name.to_string());
        let out = self.run(&["new", name]);
        if !out.status.success() {
            eprintln!(
                "ghostd new {name} failed in {}: {}",
                self.dir.display(),
                String::from_utf8_lossy(&out.stderr)
            );
        }
        out
    }

    /// An `ghostd attach <name>` client with its stdin held open (that is what
    /// keeps it attached) and its output drained by a reader thread, so a test can
    /// talk to it and watch what it prints while it runs.
    fn attach(&self, name: &str) -> Attach {
        let mut child = Command::new(BIN)
            .args(["attach", name])
            .env("GHOSTWAVE_STATE_DIR", &self.dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("ghostd attach runs");
        let stdin = child.stdin.take();
        let mut stdout = child.stdout.take().expect("stdout is piped");
        let out = Arc::new(Mutex::new(String::new()));
        let sink = Arc::clone(&out);
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            while let Ok(n) = stdout.read(&mut buf) {
                if n == 0 {
                    break;
                }
                sink.lock()
                    .unwrap()
                    .push_str(&String::from_utf8_lossy(&buf[..n]));
            }
        });
        Attach { child, stdin, out }
    }

    /// The daemon pid the pidfile's first line names.
    fn daemon_pid(&self, name: &str) -> Option<i32> {
        fs::read_to_string(self.path(&format!("{name}.pid")))
            .ok()?
            .lines()
            .next()?
            .trim()
            .parse()
            .ok()
    }

    /// The daemon pid, waiting for the daemon to publish its pidfile: `new`
    /// returns when the parent detaches, a moment before the daemon writes it.
    fn wait_for_daemon_pid(&self, name: &str, timeout: Duration) -> Option<i32> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(pid) = self.daemon_pid(name) {
                return Some(pid);
            }
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    /// Every live process running `ghostd new <name>` — what `kill_pattern`
    /// matches on a remote host. A zombie has no cmdline, so a daemon that was
    /// killed but not yet reaped does not count.
    fn daemons_named(&self, name: &str) -> Vec<i32> {
        let mut found = Vec::new();
        let Ok(entries) = fs::read_dir("/proc") else {
            return found;
        };
        for entry in entries.flatten() {
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|file| file.parse::<i32>().ok())
            else {
                continue;
            };
            let Ok(raw) = fs::read(entry.path().join("cmdline")) else {
                continue;
            };
            let args: Vec<String> = raw
                .split(|byte| *byte == 0)
                .map(|arg| String::from_utf8_lossy(arg).into_owned())
                .collect();
            let is_ghostd = args.iter().any(|arg| arg.ends_with("ghostd"));
            let creates_session = args
                .windows(2)
                .any(|pair| pair[0] == "new" && pair[1] == name);
            if is_ghostd && creates_session {
                found.push(pid);
            }
        }
        found.sort_unstable();
        found
    }
}

impl Drop for Sandbox {
    /// Nothing this test started may outlive it: a leaked daemon would hold the
    /// session name, and its pty child would keep the state directory busy.
    fn drop(&mut self) {
        for name in &self.names {
            let mut pids: Vec<i32> = Vec::new();
            if let Ok(text) = fs::read_to_string(self.path(&format!("{name}.pid"))) {
                pids.extend(
                    text.lines()
                        .filter_map(|line| line.trim().parse::<i32>().ok()),
                );
            }
            pids.extend(self.daemons_named(name));
            for pid in pids {
                let _ = kill(Pid::from_raw(pid), Signal::SIGKILL);
                let _ = kill(Pid::from_raw(-pid), Signal::SIGKILL);
            }
        }
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// A running `ghostd attach`: its stdin is a pipe the test writes to, and its
/// stdout is drained into `out` so a test never blocks on a full pipe.
struct Attach {
    child: Child,
    stdin: Option<ChildStdin>,
    out: Arc<Mutex<String>>,
}

impl Attach {
    fn write(&mut self, text: &str) {
        let stdin = self.stdin.as_mut().expect("the client's stdin is open");
        stdin
            .write_all(text.as_bytes())
            .expect("the client accepts input");
        stdin.flush().expect("the client's stdin flushes");
    }

    fn output(&self) -> String {
        self.out.lock().unwrap().clone()
    }

    /// Wait until the client has printed `needle`, then return everything it has
    /// printed so far.
    fn expect_output(&self, needle: &str, timeout: Duration) -> String {
        let deadline = Instant::now() + timeout;
        loop {
            let seen = self.output();
            if seen.contains(needle) {
                return seen;
            }
            assert!(
                Instant::now() < deadline,
                "waited {timeout:?} for {needle:?}, saw: {seen:?}"
            );
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    /// Wait for the client to exit and return its status with its output. The
    /// stdin pipe stays open, so only the daemon can end the client — a client
    /// that exits here exited because the session ended.
    fn wait(mut self) -> (Option<i32>, String) {
        let status = self.child.wait().expect("the client exits");
        std::thread::sleep(Duration::from_millis(50)); // let the reader drain the tail
        (status.code(), self.output())
    }
}

#[test]
fn a_new_session_does_not_replay_a_killed_ones_output() {
    let mut sandbox = Sandbox::new("replay");
    let marker = "MARKER-KILLED-SESSION";
    let out = sandbox.new_session("s");
    assert!(out.status.success(), "the session is created: {out:?}");

    let mut first = sandbox.attach("s");
    first.write(&format!("echo {marker}\n"));
    first.expect_output(marker, Duration::from_secs(10));
    drop(first); // closing stdin detaches the client; the daemon keeps the session

    let log = fs::read_to_string(sandbox.path("s.log")).expect("the live session logs its output");
    assert!(
        log.contains(marker),
        "a live session keeps its history: {log:?}"
    );

    assert!(
        sandbox.run(&["kill", "s"]).status.success(),
        "the session is killed"
    );
    assert!(
        !sandbox.path("s.log").exists(),
        "the killed session's log went with its daemon"
    );

    let out = sandbox.new_session("s");
    assert!(out.status.success(), "a new session is created: {out:?}");

    let mut second = sandbox.attach("s");
    second.write("echo FRESH-SESSION\n");
    let seen = second.expect_output("FRESH-SESSION", Duration::from_secs(10));
    assert!(
        !seen.contains(marker),
        "the new session replays nothing of the killed one: {seen:?}"
    );
    let log = fs::read_to_string(sandbox.path("s.log")).unwrap_or_default();
    assert!(
        !log.contains(marker),
        "and the new log starts empty: {log:?}"
    );
}

#[test]
fn new_recovers_a_wedged_daemon_and_leaves_one_daemon() {
    let mut sandbox = Sandbox::new("wedged");
    let out = sandbox.new_session("w");
    assert!(out.status.success(), "the session is created: {out:?}");
    let old = sandbox
        .wait_for_daemon_pid("w", Duration::from_secs(5))
        .expect("the daemon records its pid");

    // SIGSTOP: the listener still completes connections but the daemon never
    // answers the handshake — exactly what `Probe::Unresponsive` reports.
    kill(Pid::from_raw(old), Signal::SIGSTOP).expect("the daemon is stopped");
    assert_eq!(
        sandbox.daemon_pid("w"),
        Some(old),
        "the stopped daemon still owns the name"
    );

    let out = sandbox.new_session("w");
    assert!(
        out.status.success(),
        "`new` recovered the wedged daemon: {out:?}"
    );

    let recovered = sandbox
        .wait_for_daemon_pid("w", Duration::from_secs(5))
        .expect("the new daemon records its pid");
    assert_ne!(
        old, recovered,
        "the pidfile names the daemon that is actually running"
    );
    assert!(
        kill(Pid::from_raw(old), None).is_err(),
        "the wedged daemon was signalled until it died, not merely forgotten"
    );
    assert_eq!(
        sandbox.daemons_named("w"),
        vec![recovered],
        "exactly one daemon holds the name"
    );

    let mut client = sandbox.attach("w");
    client.write("echo RECOVERED\n");
    client.expect_output("RECOVERED", Duration::from_secs(10));
}

#[test]
fn a_session_that_ended_is_not_reported_as_missing() {
    let mut sandbox = Sandbox::new("ended");
    let out = sandbox.new_session("e");
    assert!(out.status.success(), "the session is created: {out:?}");

    let mut client = sandbox.attach("e");
    client.write("echo ATTACHED\n");
    client.expect_output("ATTACHED", Duration::from_secs(10));

    assert!(
        sandbox.run(&["kill", "e"]).status.success(),
        "the session is killed while a client is attached"
    );
    let (code, seen) = client.wait();
    assert_eq!(
        code,
        Some(4),
        "a session that ended is 4, so no caller recreates it: {seen:?}"
    );

    let missing = sandbox.run(&["attach", "no-such-session"]);
    assert_eq!(
        missing.status.code(),
        Some(1),
        "a name with no session at all is still 1: {missing:?}"
    );
}

#[test]
fn a_kill_leaves_no_state_behind() {
    let mut sandbox = Sandbox::new("clean");
    let marker = "MARKER-BEFORE-KILL";
    let out = sandbox.new_session("k");
    assert!(out.status.success(), "the session is created: {out:?}");

    let mut client = sandbox.attach("k");
    client.write(&format!("echo {marker}\n"));
    client.expect_output(marker, Duration::from_secs(10));
    drop(client);

    let out = sandbox.run(&["kill", "k"]);
    assert!(out.status.success(), "the session is killed: {out:?}");
    for file in ["k.sock", "k.pid", "k.log"] {
        assert!(
            !sandbox.path(file).exists(),
            "{file} outlives the daemon that owned it"
        );
    }

    let out = sandbox.new_session("k");
    assert!(out.status.success(), "the name is reusable: {out:?}");
    let mut client = sandbox.attach("k");
    client.write("echo AFTER-KILL\n");
    let seen = client.expect_output("AFTER-KILL", Duration::from_secs(10));
    assert!(
        !seen.contains(marker),
        "the reused name starts with an empty log: {seen:?}"
    );
}

#[test]
fn a_busy_attach_is_not_reported_as_ended() {
    let mut sandbox = Sandbox::new("busy");
    let out = sandbox.new_session("b");
    assert!(out.status.success(), "the session is created: {out:?}");

    // The first client holds the session open: its stdin stays open, which is
    // what keeps the daemon's `client_tx` occupied and the session `busy`.
    let mut first = sandbox.attach("b");
    first.write("echo FIRST-ATTACHED\n");
    first.expect_output("FIRST-ATTACHED", Duration::from_secs(10));

    let pid_before = fs::read(sandbox.path("b.pid")).expect("the pidfile exists");
    let log_before = fs::read(sandbox.path("b.log")).expect("the log exists");

    // The second client keeps its stdin open too (as a pane's tty does): with
    // stdin at EOF it would exit 0 for "my client went away" and never report
    // what the daemon said.
    let second = sandbox.attach("b");
    let (code, seen) = second.wait();
    assert_eq!(
        code,
        Some(1),
        "a busy session is 1 — the caller retries — and never 4, which means ended: {seen:?}"
    );

    assert_eq!(
        fs::read(sandbox.path("b.pid")).expect("the pidfile survives"),
        pid_before,
        "a refused attach does not touch the pidfile"
    );
    assert_eq!(
        fs::read(sandbox.path("b.log")).expect("the log survives"),
        log_before,
        "a refused attach does not touch the log"
    );

    // And the session is untouched: the first client still relays both ways.
    first.write("echo STILL-RELAYING\n");
    first.expect_output("STILL-RELAYING", Duration::from_secs(10));
}
