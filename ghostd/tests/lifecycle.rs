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

use ghostd::REPLAY_TAIL_BYTES;

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

    /// Deletes the sandbox's state dir, so the next run sees a host that has never
    /// created one.
    fn remove_state_dir(&self) {
        fs::remove_dir_all(&self.dir).expect("the state dir is removed");
        assert!(!self.dir.exists(), "the state dir is gone");
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

    /// Run `ghostd new <args…>` for `name`, registering the name so the guard
    /// reaps any daemon it created — including one a `--force` created.
    fn run_new(&mut self, args: &[&str], name: &str) -> std::process::Output {
        self.names.push(name.to_string());
        self.run(args)
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

    /// An `ghostd attach <name>` whose stdout is collected as **bytes**, for a test
    /// that has to count a byte budget (`Attach` decodes through
    /// `String::from_utf8_lossy`, which would make the count a guess).
    fn attach_raw(&self, name: &str) -> RawAttach {
        let mut child = Command::new(BIN)
            .args(["attach", name])
            .env("GHOSTWAVE_STATE_DIR", &self.dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("ghostd attach runs");
        let stdin = child.stdin.take();
        let mut stdout = child.stdout.take().expect("stdout is piped");
        let out = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&out);
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            while let Ok(n) = stdout.read(&mut buf) {
                if n == 0 {
                    break;
                }
                sink.lock().unwrap().extend_from_slice(&buf[..n]);
            }
        });
        RawAttach { child, stdin, out }
    }

    /// The session log's bytes, waiting until the file has stopped changing for
    /// `quiet`: a live shell repaints its prompt whenever it becomes ready for
    /// input, so a read taken mid-repaint is not a settle point.
    fn settled_log(&self, name: &str, quiet: Duration, timeout: Duration) -> Vec<u8> {
        let path = self.path(&format!("{name}.log"));
        let deadline = Instant::now() + timeout;
        let mut last = fs::read(&path).expect("the log exists");
        let mut stable_since = Instant::now();
        loop {
            std::thread::sleep(Duration::from_millis(20));
            let now = fs::read(&path).expect("the log exists");
            if now != last {
                last = now;
                stable_since = Instant::now();
            } else if stable_since.elapsed() >= quiet {
                return last;
            }
            assert!(
                Instant::now() < deadline,
                "the log never settled within {timeout:?}"
            );
        }
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

/// A running `ghostd attach` whose output is kept as raw bytes, so a test can
/// count exactly what the daemon replayed instead of a lossy decode.
struct RawAttach {
    child: Child,
    stdin: Option<ChildStdin>,
    out: Arc<Mutex<Vec<u8>>>,
}

impl RawAttach {
    fn write(&mut self, text: &str) {
        let stdin = self.stdin.as_mut().expect("the client's stdin is open");
        stdin
            .write_all(text.as_bytes())
            .expect("the client accepts input");
        stdin.flush().expect("the client's stdin flushes");
    }

    /// Every byte the client has printed so far.
    fn bytes(&self) -> Vec<u8> {
        self.out.lock().unwrap().clone()
    }

    fn len(&self) -> usize {
        self.out.lock().unwrap().len()
    }

    /// The client's bytes, once its buffer has not grown for `quiet`: a replay has
    /// landed when the stream stops moving, not after a fixed sleep.
    fn wait_quiet(&self, quiet: Duration, max: Duration) -> Vec<u8> {
        let deadline = Instant::now() + max;
        let mut last = self.len();
        let mut stable_since = Instant::now();
        loop {
            std::thread::sleep(Duration::from_millis(20));
            let now = self.len();
            if now != last {
                last = now;
                stable_since = Instant::now();
            } else if stable_since.elapsed() >= quiet {
                return self.bytes();
            }
            assert!(
                Instant::now() < deadline,
                "the client's output never settled within {max:?} ({last} bytes)"
            );
        }
    }

    /// The client's bytes once they contain `needle`.
    fn wait_for(&self, needle: &[u8], max: Duration) -> Vec<u8> {
        let deadline = Instant::now() + max;
        loop {
            let seen = self.bytes();
            if seen.windows(needle.len()).any(|w| w == needle) {
                return seen;
            }
            assert!(
                Instant::now() < deadline,
                "waited {max:?} for {:?}, saw {} bytes",
                String::from_utf8_lossy(needle),
                seen.len()
            );
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    /// Ends the client outright and reaps it, so no reader thread is left draining.
    fn kill(mut self) {
        let _ = kill(Pid::from_raw(self.child.id() as i32), Signal::SIGKILL);
        let _ = self.child.wait();
    }

    /// The client's exit status once it has exited, without waiting for it.
    fn exited(&mut self) -> Option<i32> {
        self.child.try_wait().ok().flatten().and_then(|status| status.code())
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

    // A kill is final: the name cannot be recreated until it is un-killed, so the
    // automatic path that follows a killed attach cannot rebuild it.
    let refused = sandbox.run(&["new", "s"]);
    assert!(!refused.status.success(), "a killed name is refused: {refused:?}");
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("unkill"),
        "and says how to undo it: {refused:?}"
    );

    assert!(
        sandbox.run(&["unkill", "s"]).status.success(),
        "the explicit connect clears the tombstone"
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
fn a_kill_leaves_only_its_tombstone_behind() {
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
    assert!(
        sandbox.path("k.killed").exists(),
        "the kill leaves a tombstone in the state dir"
    );

    let refused = sandbox.run(&["new", "k"]);
    assert!(
        !refused.status.success(),
        "a killed name is not recreated by `new`: {refused:?}"
    );
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("unkill"),
        "and the refusal names the way out: {refused:?}"
    );

    assert!(
        sandbox.run(&["unkill", "k"]).status.success(),
        "the name can be released"
    );
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
fn a_killed_session_reports_killed_and_cannot_be_recreated() {
    let mut sandbox = Sandbox::new("killed");
    let out = sandbox.new_session("s");
    assert!(out.status.success(), "the session is created: {out:?}");

    let mut client = sandbox.attach("s");
    client.write("echo ALIVE\n");
    client.expect_output("ALIVE", Duration::from_secs(10));
    drop(client);

    assert!(
        sandbox.run(&["kill", "s"]).status.success(),
        "the session is killed"
    );

    let attach = sandbox.run(&["attach", "s"]);
    assert_eq!(
        attach.status.code(),
        Some(5),
        "a killed session is 5 — terminal, never the 1 that means create me: {attach:?}"
    );
    assert!(
        String::from_utf8_lossy(&attach.stderr).contains("unkill"),
        "and its stderr names unkill: {attach:?}"
    );

    let new = sandbox.run(&["new", "s"]);
    assert!(
        !new.status.success(),
        "a killed name is refused even when attach is not consulted: {new:?}"
    );
    assert!(
        String::from_utf8_lossy(&new.stderr).contains("unkill"),
        "the refusal names unkill: {new:?}"
    );

    assert!(sandbox.path("s.killed").exists(), "the tombstone is there");
    assert!(
        !sandbox.path("s.pid").exists(),
        "and nothing recreated the session behind the refusal"
    );
    let list = sandbox.run(&["list"]);
    assert!(
        !String::from_utf8_lossy(&list.stdout).contains('s'),
        "a killed session is not listed as a session: {list:?}"
    );
}

#[test]
fn new_force_recreates_a_killed_name() {
    let mut sandbox = Sandbox::new("force");
    let out = sandbox.new_session("f");
    assert!(out.status.success(), "the session is created: {out:?}");
    // The daemon publishes its pidfile a moment after `new` returns; a kill that
    // lands first has no owner to signal (pre-existing window, not this change's).
    sandbox
        .wait_for_daemon_pid("f", Duration::from_secs(5))
        .expect("the daemon records its pid");
    assert!(
        sandbox.run(&["kill", "f"]).status.success(),
        "the session is killed"
    );

    let forced = sandbox.run_new(&["new", "--force", "f"], "f");
    assert!(
        forced.status.success(),
        "the explicit override creates the name again: {forced:?}"
    );
    assert!(
        !sandbox.path("f.killed").exists(),
        "and the tombstone is gone"
    );
    sandbox
        .wait_for_daemon_pid("f", Duration::from_secs(5))
        .expect("the new daemon records its pid");

    let mut client = sandbox.attach("f");
    client.write("echo FORCED\n");
    client.expect_output("FORCED", Duration::from_secs(10));
}

#[test]
fn a_kill_of_an_unknown_name_tombstones_it() {
    let mut sandbox = Sandbox::new("unknown");
    let out = sandbox.run_new(&["kill", "no-such"], "no-such");
    assert!(
        out.status.success(),
        "killing a name with no daemon is not an error — there is nothing left to protect: {out:?}"
    );
    assert!(
        sandbox.path("no-such.killed").exists(),
        "and it still tombstones the name"
    );

    let refused = sandbox.run(&["new", "no-such"]);
    assert!(
        !refused.status.success(),
        "so the name cannot be created by an automatic path: {refused:?}"
    );

    assert!(
        sandbox.run(&["unkill", "no-such"]).status.success(),
        "unkill releases it"
    );
    let out = sandbox.new_session("no-such");
    assert!(out.status.success(), "and then it can be created: {out:?}");
}

/// The socket, pidfile and tombstone are all `<state dir>/<name>.*`, so a kill
/// whose name carries a path separator would name files outside the state
/// directory: `kill ../victim` would delete `<state dir>/../victim.sock` and
/// tombstone `<state dir>/../victim.killed`. The name is refused before any of
/// those paths is derived.
#[test]
fn a_kill_of_a_traversing_name_is_refused_before_it_touches_anything() {
    let sandbox = Sandbox::new("traversal");
    let outside = sandbox.dir.parent().expect("the state dir has a parent").to_path_buf();
    let tag = sandbox
        .dir
        .file_name()
        .expect("the state dir has a name")
        .to_string_lossy()
        .to_string();
    let victim = format!("{tag}-victim");
    // The file the unfixed code deletes on its daemon-less path
    // (`fs::remove_file(socket_path(name))`) and the marker it writes next.
    let sock = outside.join(format!("{victim}.sock"));
    let tombstone = outside.join(format!("{victim}.killed"));
    fs::write(&sock, b"do not touch").expect("the sentinel is written");
    let _ = fs::remove_file(&tombstone);

    for name in [format!("../{victim}"), format!("sub/{victim}"), String::new()] {
        let out = sandbox.run(&["kill", &name]);
        assert!(!out.status.success(), "'{name}' is refused: {out:?}");
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("bad session name"),
            "'{name}' is refused by name validation: {out:?}"
        );
    }

    assert_eq!(
        fs::read_to_string(&sock).expect("the sentinel survives"),
        "do not touch",
        "nothing outside the state dir was removed or written"
    );
    assert!(!tombstone.exists(), "and no tombstone was created outside it");
    assert!(
        !sandbox.path(&format!("sub/{victim}.killed")).exists(),
        "nor inside a subdirectory named by the traversal"
    );

    // The same rule guards every verb that derives a path from a name, so the
    // refusal cannot be sidestepped by choosing another one.
    let traversing = format!("../{victim}");
    assert!(!sandbox.run(&["new", &traversing]).status.success(), "new refuses it");
    assert!(!sandbox.run(&["unkill", &traversing]).status.success(), "unkill refuses it");
    assert!(
        fs::read_to_string(&sock).expect("the sentinel still survives") == "do not touch"
            && !tombstone.exists(),
        "and neither verb touched the path outside the state dir"
    );

    let _ = fs::remove_file(&sock);
}

/// A symlink squatting on `<name>.killed` is not a tombstone, so it neither
/// refuses the name nor survives the unkill: the link is unlinked and its target
/// is never followed.
#[test]
fn a_symlink_marker_does_not_pin_a_name_and_unkill_unlinks_it() {
    let mut sandbox = Sandbox::new("marker-link");
    let target = sandbox.path("sibling");
    fs::write(&target, b"keep me").expect("the target is written");
    std::os::unix::fs::symlink(&target, sandbox.path("l.killed")).expect("the link exists");

    let out = sandbox.new_session("l");
    assert!(
        out.status.success(),
        "a symlink marker does not refuse the name: {out:?}"
    );

    let out = sandbox.run(&["unkill", "l"]);
    assert!(out.status.success(), "unkill clears the marker path: {out:?}");
    assert!(!sandbox.path("l.killed").exists(), "the link is gone");
    assert_eq!(
        fs::read_to_string(&target).expect("the target survives"),
        "keep me",
        "and its target was not followed, emptied or removed"
    );
}

/// A host that has never created a state dir is the daemon-less kill path
/// (`kill <unknown name>`): nothing else creates the directory, so the tombstone
/// writer must, or the "stays killed" claim has nothing behind it.
#[test]
fn a_kill_records_its_tombstone_even_when_the_state_dir_does_not_exist_yet() {
    let mut sandbox = Sandbox::new("no-state-dir");
    sandbox.remove_state_dir();

    let out = sandbox.run_new(&["kill", "ghost"], "ghost");
    let tombstoned = sandbox.path("ghost.killed").exists();
    assert!(
        out.status.success() || String::from_utf8_lossy(&out.stderr).contains("may be recreated"),
        "a kill either records the tombstone or says the name may be recreated: {out:?}"
    );
    assert!(
        out.status.success() == tombstoned,
        "a kill that reported success left the tombstone it promises: {out:?}"
    );
    assert!(
        tombstoned,
        "this host can write the tombstone once the dir is created: {out:?}"
    );
    assert!(
        !sandbox.path("ghost.killed.tmp").exists(),
        "and leaves no half-written file behind"
    );

    let refused = sandbox.run(&["new", "ghost"]);
    assert!(
        !refused.status.success(),
        "so the name cannot be recreated by an automatic path: {refused:?}"
    );

    assert!(
        sandbox.run(&["unkill", "ghost"]).status.success(),
        "unkill releases it"
    );
    assert!(!sandbox.path("ghost.killed").exists(), "and the tombstone is gone");
}

/// A tombstone that exists but cannot be removed is a name that is *still*
/// killed: `unkill` must not report it creatable (the explicit-connect script
/// would then attach a name it cannot create).
#[test]
fn an_unkill_whose_tombstone_will_not_clear_fails() {
    use std::os::unix::fs::PermissionsExt;
    if nix::unistd::geteuid().is_root() {
        eprintln!("skipped: a root process can unlink from a read-only directory");
        return;
    }
    let sandbox = Sandbox::new("unkill-stuck");
    fs::write(sandbox.path("stuck.killed"), b"killed 0 0\n").expect("the tombstone exists");
    fs::set_permissions(&sandbox.dir, fs::Permissions::from_mode(0o500))
        .expect("the state dir loses its write bit");

    let out = sandbox.run(&["unkill", "stuck"]);

    fs::set_permissions(&sandbox.dir, fs::Permissions::from_mode(0o700))
        .expect("the write bit is restored so the sandbox can be removed");
    assert!(
        !out.status.success(),
        "a tombstone that will not clear is not reported cleared: {out:?}"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("still killed"),
        "and the message says the name is still killed: {out:?}"
    );
    assert!(sandbox.path("stuck.killed").exists(), "the tombstone is still there");
}

/// A non-tombstone object on the marker path (here a non-empty directory) never
/// killed the name, so it must not be reported as one — but `unkill` must also
/// not claim the path is clear when it could not remove the object.
#[test]
fn an_unkill_of_a_non_tombstone_marker_reports_it_instead_of_claiming_a_clear() {
    let mut sandbox = Sandbox::new("unkill-foreign");
    fs::create_dir_all(sandbox.path("odd.killed")).expect("the obstruction exists");
    fs::write(sandbox.path("odd.killed/inside"), b"x").expect("a file inside it");

    let out = sandbox.run(&["unkill", "odd"]);
    assert!(
        !out.status.success(),
        "a clear that removed nothing is not a success: {out:?}"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("is not killed"),
        "and the message does not call the name killed: {out:?}"
    );
    assert!(
        sandbox.path("odd.killed/inside").exists(),
        "the obstruction was neither removed nor recursed into"
    );

    // The name was never killed, so the automatic path may create it even though
    // the obstruction is still there: nothing is pinned forever.
    let out = sandbox.new_session("odd");
    assert!(
        out.status.success(),
        "a non-tombstone marker does not refuse the name: {out:?}"
    );
}

/// `new --force` clears the marker after the bind, so a marker path that will not
/// clear must stop it before it daemonizes: a daemon running where the next
/// kill's tombstone could not be renamed into place is a session whose kill
/// cannot be recorded.
#[test]
fn a_force_new_whose_tombstone_will_not_clear_does_not_daemonize() {
    let mut sandbox = Sandbox::new("force-stuck");
    fs::create_dir_all(sandbox.path("stuck.killed")).expect("the obstruction exists");
    fs::write(sandbox.path("stuck.killed/inside"), b"x").expect("a file inside it");

    let out = sandbox.run_new(&["new", "--force", "stuck"], "stuck");
    assert!(
        !out.status.success(),
        "the override refuses a name whose marker path will not clear: {out:?}"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("kill marker could not be cleared"),
        "and says why: {out:?}"
    );
    assert!(
        !sandbox.path("stuck.pid").exists(),
        "no daemon was left behind to hold the name"
    );
}

#[test]
fn an_unkill_with_nothing_killed_is_a_no_op() {
    let sandbox = Sandbox::new("unkill-noop");
    let out = sandbox.run(&["unkill", "fresh"]);
    assert!(out.status.success(), "unkill of a live name exits 0: {out:?}");
    assert!(
        !sandbox.path("fresh.killed").exists(),
        "and creates no tombstone"
    );
    assert!(
        !sandbox.path("fresh.pid").exists() && !sandbox.path("fresh.sock").exists(),
        "and touches no session state"
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

    // Quiesce first: the session is live, so the shell repaints its prompt
    // whenever it is ready for input. Snapshot only once the log has settled
    // after a sync command's output, so no repaint is in flight.
    first.write("echo BUSY-QUIESCED\n");
    first.expect_output("BUSY-QUIESCED", Duration::from_secs(10));
    let log_before = sandbox.settled_log("b", Duration::from_millis(300), Duration::from_secs(10));

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
    // A refused attach may not rewrite or truncate the log; appended bytes are
    // tolerated because the live shell's own repaint can still land after the
    // snapshot, and nothing in the log can tell that apart from an append.
    let log_after = sandbox.settled_log("b", Duration::from_millis(300), Duration::from_secs(10));
    assert!(
        log_after.starts_with(&log_before),
        "a refused attach does not touch the log: {} bytes before, {} after\nbefore: {:?}\nafter: {:?}",
        log_before.len(),
        log_after.len(),
        String::from_utf8_lossy(&log_before),
        String::from_utf8_lossy(&log_after)
    );

    // And the session is untouched: the first client still relays both ways.
    first.write("echo STILL-RELAYING\n");
    first.expect_output("STILL-RELAYING", Duration::from_secs(10));
}

#[test]
fn an_attach_replays_only_a_bounded_tail() {
    let mut sandbox = Sandbox::new("tail");
    let out = sandbox.new_session("s");
    assert!(out.status.success(), "the session is created: {out:?}");

    // 1. Fill the log past the bound by driving the session's own shell
    //    (`ghostd new` ignores a command argument).
    let mut first = sandbox.attach_raw("s");
    first.write("seq 1 400000; echo TAIL-MARKER\n");
    first.wait_for(b"TAIL-MARKER", Duration::from_secs(30)); // the line reached the shell
    drop(first); // closing stdin detaches it; the daemon keeps the shell and the log

    // 2. Wait for the fill to finish rather than for a marker count: the log only
    //    stops changing once `seq` is done, and a replay taken mid-stream would
    //    legitimately be bounded but then followed by megabytes of live output.
    let log = sandbox.settled_log("s", Duration::from_millis(500), Duration::from_secs(60));
    assert!(
        log.len() as u64 > 1024 * 1024,
        "the log is far past the bound: {} bytes",
        log.len()
    );
    assert!(
        log.windows(11).any(|window| window == b"TAIL-MARKER"),
        "the fill's output is in the log"
    );

    // 3. A fresh attach replays only the newest, bounded window. The first attempt
    //    can still be refused as busy while the detached client's exit reaches the
    //    daemon, so it is retried rather than assumed.
    let deadline = Instant::now() + Duration::from_secs(20);
    let (mut second, received) = loop {
        let mut client = sandbox.attach_raw("s");
        std::thread::sleep(Duration::from_millis(200));
        if client.exited().is_some() {
            client.kill();
            assert!(
                Instant::now() < deadline,
                "every attach was refused: the detached client never released the session"
            );
            std::thread::sleep(Duration::from_millis(100));
            continue;
        }
        let bytes = client.wait_quiet(Duration::from_millis(300), Duration::from_secs(15));
        if bytes.is_empty() {
            client.kill();
            assert!(
                Instant::now() < deadline,
                "every attach replayed nothing"
            );
            continue;
        }
        break (client, bytes);
    };

    assert!(
        received.len() as u64 <= REPLAY_TAIL_BYTES + 4096,
        "the replay is bounded: {} bytes (bound {REPLAY_TAIL_BYTES})",
        received.len()
    );
    assert!(
        received
            .windows(11)
            .any(|window| window == b"TAIL-MARKER"),
        "the replay keeps the newest bytes, not the oldest: {} bytes starting {:?}",
        received.len(),
        String::from_utf8_lossy(&received[..received.len().min(120)])
    );
    let nl = received
        .iter()
        .position(|&b| b == b'\n')
        .expect("the replay's first line ends with a newline");
    let mut first_line = &received[..nl];
    if first_line.last() == Some(&b'\r') {
        first_line = &first_line[..first_line.len() - 1];
    }
    assert!(
        !first_line.is_empty() && first_line.iter().all(u8::is_ascii_digit),
        "the replay starts on a whole line, not mid-number: {:?}",
        String::from_utf8_lossy(first_line)
    );

    // 4. Live relay still works after the truncated replay.
    second.write("echo LIVE-MARKER\n");
    let seen = second.wait_for(b"LIVE-MARKER", Duration::from_secs(10));
    assert!(
        seen.len() > received.len(),
        "the live output follows the replay: {} then {} bytes",
        received.len(),
        seen.len()
    );
    second.kill();
}
