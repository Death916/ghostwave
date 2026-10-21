//! The durable remote/SSH pane subsystem: session naming, the command lines a
//! remote pane runs, the ghostd binary bootstrap, and the bounded
//! auto-reconnect pass.
//!
//! This is the port of the reference front-end's `App::remote_attach_cmd` /
//! `ensure_remote_binary` / `connect_remote_pane` / `kill_remote_pane` /
//! `live_remote_specs` subsystem (`macroquad-final`, `src/app.rs`). What is
//! stateless lives here; the state a workspace owns — the staged-pane pool, the
//! per-host upload results, which panes exist — lives on
//! [`crate::app::AppState`], which is also where the pieces are driven from.
//!
//! Three invariants come straight from the reference and must not drift: every
//! remote command is wrapped in `sh -c '…'` so a host whose login shell is not
//! POSIX (fish) still parses it, a remote pane is never auto-connected on
//! restore — see [`crate::pane::Pane::restore_remote`] — and a durable pane's
//! daemon session is named after its host.
//!
//! The contract for the two values that end up on a remote command line is
//! enforced here:
//!
//! * the **host** is only ever an argv element of the local `ssh` call, never
//!   shell source, and it has to pass [`host_is_valid`] before any command is
//!   built for it;
//! * the **session name** is never taken as given: it is rederived with
//!   [`remote_session`], checked with [`binding_is_valid`], and then quoted as
//!   data, so no value out of a session file can become shell or regex syntax;
//! * every `ssh` child runs under an overall deadline and an in-flight cap
//!   ([`SystemSsh`]), because `ConnectTimeout` bounds only the connection.

use std::collections::HashSet;
use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex, OnceLock};
use std::time::Duration;

use ghostwave::config::Config;
use ghostwave::term::ReconnectState;

use crate::app::Tab;
use crate::pane::PaneEntry;

/// The host and session-name rules the front-end and the session store share.
pub use ghostwave::term::{host_is_valid, remote_session, sanitize_host};

/// Embedded ghostd binaries (static musl, built by `scripts/build-ghostd.sh`).
/// The remote's own `uname -m` picks which one is uploaded.
const GHOSTD_X86_64: &[u8] = include_bytes!("../assets/ghostd-x86_64");
const GHOSTD_AARCH64: &[u8] = include_bytes!("../assets/ghostd-aarch64");

/// Ceiling of the auto-reconnect backoff, in seconds (see
/// [`ghostwave::config::RemoteConfig::reconnect_delay_secs`], whose documented
/// contract is an initial delay that caps here).
pub const RECONNECT_DELAY_CAP_SECS: u64 = 15;

/// Delay before the retry that waits for a ghostd upload to land, in ms.
const UPLOAD_WAIT_MS: u64 = 500;

/// How long an ssh command that is not a pane gets to connect, in seconds.
const SSH_CONNECT_TIMEOUT: &str = "5";

/// How long the ssh behind a durable pane gets to connect, in seconds.
const PANE_CONNECT_TIMEOUT: &str = "10";

/// Overall deadline of a short ssh command: connect, run, answer.
const SSH_OP_TIMEOUT: Duration = Duration::from_secs(15);

/// Overall deadline of an upload, which streams a multi-megabyte binary.
const SSH_UPLOAD_TIMEOUT: Duration = Duration::from_secs(120);

/// Ceiling on ssh children running at once, process-wide.
const SSH_MAX_IN_FLIGHT: usize = 4;

/// Emits a remote-subsystem trace when `GHOSTWAVE_DEBUG` is set.
///
/// These are the steps that are otherwise invisible until something is wrong
/// (the ssh argv, the upload decisions, the reconnect transitions), which is why
/// the reference kept them behind one switch as well.
pub(crate) fn dbg(msg: impl AsRef<str>) {
    if std::env::var("GHOSTWAVE_DEBUG").map(|v| v == "0").unwrap_or(true) {
        return;
    }
    eprintln!("[ghostwave] {}", msg.as_ref());
}

/// `s` as one double-quoted shell word, with `"`, `\`, `$` and `` ` `` escaped.
///
/// A session name is quoted even though [`remote_session`] can only produce
/// alphanumerics, `.`, `_` and `-`: these commands are the last stop before a
/// remote login shell, so a name that somehow reached them still cannot expand
/// or close the quoting.
fn dquoted(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        if matches!(c, '"' | '\\' | '$' | '`') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

/// `s` as one single-quoted shell word, with every `'` closed, escaped and
/// reopened (`'\''`).
///
/// Single quotes are the only form that leaves every character alone — `$`,
/// `` ` ``, `\`, `"` and `'` itself — and the helper composes: quoting its own
/// output quotes that *text* as one word for a second shell. That is how the
/// script below is handed to `sh -c`, and it is what makes a session name that
/// contains a `'` unable to end the script early.
fn sq(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        if c == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

/// `s` with every POSIX ERE metacharacter backslash-escaped, so it only matches
/// itself as a literal.
fn regex_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(
            c,
            '.' | '^' | '$' | '*' | '+' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '|' | '\\'
        ) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Whether `session` is the daemon session `host` owns.
///
/// The gate for any binding that did not come from [`remote_session`] in this
/// process — a saved session's `session` field, say. Failing it means the
/// binding is not acted on at all rather than acted on with unvalidated text.
pub fn binding_is_valid(host: &str, session: &str) -> bool {
    host_is_valid(host) && session == remote_session(host)
}

/// The `ssh` argv a plain `ssh <host>` pane runs.
pub fn ssh_argv(host: &str) -> Vec<String> {
    vec![host.to_string()]
}

/// The local session a remote pane lives in: `ssh-<host>`.
///
/// Both a plain `ssh <host>` and a durable `ds <host>` connection live under
/// this one name, so the session list shows where the connection is and a
/// repeated connect focuses it instead of stacking a duplicate.
pub fn ssh_session(host: &str) -> String {
    format!("ssh-{}", sanitize_host(host))
}

/// The `ssh` argv a durable pane runs: `ssh -t -o ConnectTimeout=10 <host> <cmd>`.
///
/// The destination is one argv element and nothing more — the local exec has no
/// shell — and the command is [`attach_cmd`], which carries no host text.
pub fn attach_argv(host: &str, session: &str) -> Vec<String> {
    vec![
        "-t".to_string(),
        "-o".to_string(),
        format!("ConnectTimeout={PANE_CONNECT_TIMEOUT}"),
        host.to_string(),
        attach_cmd(session),
    ]
}

/// The remote shell command a durable pane runs: check the cached ghostd binary
/// is present and current, then attach — creating the session when it is gone.
///
/// The version check compares the remote binary against this app's own version,
/// so a rebuilt daemon is re-uploaded rather than reused. The session name is the
/// only variable text, and it is quoted as data; no host text appears at all,
/// which is what makes a quote-bearing host harmless here. The script itself is
/// one single-quoted word ([`sq`]), so a name carrying a `'` cannot end it.
pub fn attach_cmd(session: &str) -> String {
    let session = dquoted(session);
    let script = format!(
        "BIN=\"$HOME/.cache/ghostwave/ghostd-$(uname -m)\"; \
         if [ ! -x \"$BIN\" ] || [ \"$($BIN --version 2>/dev/null)\" != \"ghostd {ver}\" ]; then \
         echo \"ghostd missing/stale — reconnect to upload\"; exit 1; fi; \
         \"$BIN\" attach {session} 2>/dev/null || {{ \"$BIN\" new {session} >/dev/null 2>&1 && exec \"$BIN\" attach {session}; }}",
        ver = env!("CARGO_PKG_VERSION")
    );
    format!("sh -c {}", sq(&script))
}

/// The `pkill -f` pattern that sweeps a session's orphaned daemons: an actual
/// `ghostd` invocation of `new <session>`, anchored at both ends of that
/// argument.
///
/// `pkill -f` matches its pattern as a regex against the whole command line, so a
/// bare name is both too broad — the `.` in a generated name matches `-`, which
/// would SIGKILL a different session's daemon — and, for a name out of a file,
/// regex syntax. The name is escaped to a literal and the match is pinned to a
/// `ghostd …` command line whose `new <session>` argument ends there or is
/// followed by the next argument, so a session daemon started WITH a command
/// (`ghostd new <name> /bin/bash`) still matches.
fn kill_pattern(session: &str) -> String {
    format!("(^|/)ghostd-[^ ]* new {}($| )", regex_escape(session))
}

/// The orphan sweep as `pkill -f` receives it: [`kill_pattern`] quoted as one
/// shell word, so the shell that runs the sweep cannot take the anchor for a
/// parameter expansion or split the regex.
///
/// Public so a test can assert on the pattern the shell will hand `pkill`,
/// rather than on the quoting that carries it.
pub fn sweep_pattern(session: &str) -> String {
    dquoted(&kill_pattern(session))
}

/// The remote shell command that kills a durable session's daemon.
///
/// Runs on its own ssh connection, so it works when the pane is dead or the
/// daemon is wedged: the daemon escalates TERM → KILL for itself and the
/// session's process group off its own pidfile, and the anchored `pkill` is the
/// last resort for a daemon whose pidfile is already gone. The script is one
/// single-quoted word ([`sq`]), so a session name carrying a `'` cannot end it.
pub fn kill_cmd(session: &str) -> String {
    let script = format!(
        "BIN=\"$HOME/.cache/ghostwave/ghostd-$(uname -m)\"; \
         if [ ! -x \"$BIN\" ]; then echo \"no ghostd binary on this host\"; exit 1; fi; \
         \"$BIN\" kill {session} 2>&1 || true; \
         pkill -9 -f {pattern} 2>/dev/null || true; true",
        session = dquoted(session),
        pattern = sweep_pattern(session),
    );
    format!("sh -c {}", sq(&script))
}

/// What one ssh command produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SshOutcome {
    /// Exit status, `None` when the command never completed.
    pub status: Option<i32>,
    /// Set when the overall deadline killed the command.
    pub timed_out: bool,
    pub stdout: Vec<u8>,
}

impl SshOutcome {
    /// Whether the command ran and exited 0.
    pub fn ok(&self) -> bool {
        !self.timed_out && self.status == Some(0)
    }

    /// Stdout as text, only when the command succeeded.
    pub fn text(&self) -> Option<String> {
        self.ok()
            .then(|| String::from_utf8_lossy(&self.stdout).into_owned())
    }
}

/// The ssh seam: every remote operation goes through one of these.
///
/// It exists so a unit test can exercise the remote logic — which host, which
/// command, which bytes — without dialing a host or killing anything. The real
/// implementation is [`SystemSsh`]; [`testing::FakeSsh`] is the recording double
/// the front-end's tests use.
pub trait SshRunner: Send + Sync + 'static {
    /// Runs `cmd` on `host`, feeding `stdin` when it is given, bounded by an
    /// overall `timeout` (not just a connection timeout).
    fn run(&self, host: &str, cmd: &str, stdin: Option<&[u8]>, timeout: Duration) -> SshOutcome;

    /// The program and args a pane runs to open an ssh connection, given the
    /// `ssh` argv the front-end would run.
    ///
    /// The system runner runs `ssh` itself; a test runner substitutes a local
    /// stand-in, so no unit test starts a real connection.
    fn pane_command(&self, argv: &[String]) -> (String, Vec<String>) {
        ("ssh".to_string(), argv.to_vec())
    }
}

/// The real runner: `ssh`, under an overall deadline and an in-flight cap.
pub struct SystemSsh;

impl SshRunner for SystemSsh {
    fn run(&self, host: &str, cmd: &str, stdin: Option<&[u8]>, timeout: Duration) -> SshOutcome {
        let _slot = slots().acquire();
        let mut command = Command::new("ssh");
        command.args([
            "-o",
            &format!("ConnectTimeout={SSH_CONNECT_TIMEOUT}"),
            host,
            cmd,
        ]);
        run_bounded(&mut command, stdin, timeout)
    }
}

/// Runs `command` to completion, killing it at `timeout`.
///
/// `ConnectTimeout` bounds only the connection: a reachable server can hold an
/// exec channel open indefinitely, which is what retains a detached kill or
/// upload thread. On expiry the child is SIGKILLed and reaped before this
/// returns — it has not been reaped yet when the signal is sent, so its pid
/// cannot be recycled under us.
fn run_bounded(command: &mut Command, stdin_bytes: Option<&[u8]>, timeout: Duration) -> SshOutcome {
    let mut child = match command
        .stdin(if stdin_bytes.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(_) => {
            return SshOutcome {
                status: None,
                timed_out: false,
                stdout: Vec::new(),
            }
        }
    };
    let pid = child.id();
    let sink = child.stdin.take();
    let bytes: Option<Vec<u8>> = stdin_bytes.map(|b| b.to_vec());
    let (tx, rx) = mpsc::channel();
    // The wait runs on its own thread so the deadline can be enforced while the
    // command runs. Dropping the stdin handle is what gives the remote `cat` its
    // EOF, so the writer must not outlive this closure.
    std::thread::spawn(move || {
        if let (Some(mut sink), Some(bytes)) = (sink, bytes) {
            let _ = sink.write_all(&bytes);
        }
        let _ = tx.send(child.wait_with_output().map(|out| (out.status.code(), out.stdout)));
    });
    match rx.recv_timeout(timeout) {
        Ok(Ok((status, stdout))) => SshOutcome {
            status,
            timed_out: false,
            stdout,
        },
        Ok(Err(_)) => SshOutcome {
            status: None,
            timed_out: false,
            stdout: Vec::new(),
        },
        Err(_) => {
            let _ = nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(pid as i32),
                nix::sys::signal::Signal::SIGKILL,
            );
            let _ = rx.recv_timeout(Duration::from_secs(5));
            SshOutcome {
                status: None,
                timed_out: true,
                stdout: Vec::new(),
            }
        }
    }
}

/// A counting semaphore over ssh children.
struct SlotPool {
    free: Mutex<usize>,
    opened: Condvar,
}

impl SlotPool {
    fn new(limit: usize) -> Self {
        Self {
            free: Mutex::new(limit),
            opened: Condvar::new(),
        }
    }

    /// Blocks until a slot is free; the returned guard releases it.
    fn acquire(self: &Arc<Self>) -> Slot {
        let mut free = self.free.lock().unwrap_or_else(|e| e.into_inner());
        while *free == 0 {
            free = self.opened.wait(free).unwrap_or_else(|e| e.into_inner());
        }
        *free -= 1;
        Slot {
            pool: Arc::clone(self),
        }
    }
}

/// One acquired ssh slot; dropping it frees the slot.
struct Slot {
    pool: Arc<SlotPool>,
}

impl Drop for Slot {
    fn drop(&mut self) {
        let mut free = self.pool.free.lock().unwrap_or_else(|e| e.into_inner());
        *free += 1;
        self.pool.opened.notify_one();
    }
}

/// The process-wide ssh pool: repeated kill attempts on a hung host queue
/// instead of piling up processes.
fn slots() -> Arc<SlotPool> {
    static POOL: OnceLock<Arc<SlotPool>> = OnceLock::new();
    Arc::clone(POOL.get_or_init(|| Arc::new(SlotPool::new(SSH_MAX_IN_FLIGHT))))
}

/// Opens a fresh ssh connection to `host` and kills its `session` daemon.
///
/// Fire and forget: the result is reported by the daemon itself, and the caller
/// is a key press. Runs nothing, and returns `false`, for a binding that fails
/// [`binding_is_valid`] — a host that is not an ssh destination, or a session
/// name that is not the one its host owns.
pub fn spawn_kill(ssh: Arc<dyn SshRunner>, host: &str, session: &str) -> bool {
    if !binding_is_valid(host, session) {
        dbg(format!("kill {session} on {host}: refused, not a validated binding"));
        return false;
    }
    let host = host.to_string();
    let session = session.to_string();
    let cmd = kill_cmd(&session);
    dbg(format!("kill {session} on {host}: spawning ssh"));
    std::thread::spawn(move || {
        let _ = ssh.run(&host, &cmd, None, SSH_OP_TIMEOUT);
    });
    true
}

/// A token unique to one upload: this process's id plus a counter, so two
/// uploads — this process's or another one's — can never write the same remote
/// temp file.
fn unique_token() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    format!("{}.{}", std::process::id(), COUNTER.fetch_add(1, Ordering::Relaxed))
}

/// The remote command that reports whether the cached binary is this app's
/// version. `arch` comes from the allowlist in [`upload_ghostd`], so it is a
/// fixed string rather than remote text.
fn version_check(arch: &str) -> String {
    format!(
        "sh -c 'BIN=\"$HOME/.cache/ghostwave/ghostd-{arch}\"; [ -x \"$BIN\" ] && [ \"$($BIN --version 2>/dev/null)\" = \"ghostd {ver}\" ]'",
        ver = env!("CARGO_PKG_VERSION")
    )
}

/// The remote command that installs a streamed binary: write it to a temp path
/// unique to this upload, then rename it over the cached one.
///
/// The rename is the only step that publishes the binary, so a dropped
/// connection leaves the old binary in place and removes the partial temp file;
/// a truncated upload can never look installed.
fn install_cmd(arch: &str, temp: &str) -> String {
    format!(
        "sh -c 'D=\"$HOME/.cache/ghostwave\"; T=\"$D/{temp}\"; \
         mkdir -p \"$D\" && cat > \"$T\" && chmod +x \"$T\" && mv -f \"$T\" \"$D/ghostd-{arch}\" \
         || {{ rm -f \"$T\"; exit 1; }}'"
    )
}

/// Makes sure the ghostd binary for `host` is present and current in
/// `~/.cache/ghostwave` on the remote. True when it is, or became, ready.
///
/// Blocking: the caller runs it on the upload thread. False covers every way it
/// cannot be ready — a host that is not an ssh destination, no ssh, an unknown
/// architecture, a failed or timed-out upload, or an installed binary that does
/// not report the version just sent.
pub fn upload_ghostd(ssh: &dyn SshRunner, host: &str) -> bool {
    if !host_is_valid(host) {
        dbg(format!("upload {host}: not an ssh destination"));
        return false;
    }
    let Some(arch) = ssh
        .run(host, "uname -m", None, SSH_OP_TIMEOUT)
        .text()
        .map(|a| a.trim().to_string())
    else {
        dbg(format!("upload {host}: uname -m failed"));
        return false;
    };
    dbg(format!("upload {host}: arch={arch}"));
    let bytes: &[u8] = match arch.as_str() {
        "x86_64" => GHOSTD_X86_64,
        "aarch64" => GHOSTD_AARCH64,
        _ => {
            dbg(format!("upload {host}: unsupported arch {arch}"));
            return false;
        }
    };
    if ssh.run(host, &version_check(&arch), None, SSH_OP_TIMEOUT).ok() {
        dbg(format!("upload {host}: binary current, no upload"));
        return true;
    }
    let temp = format!("ghostd-{arch}.new.{}", unique_token());
    dbg(format!(
        "upload {host}: uploading {} bytes as {temp}",
        bytes.len()
    ));
    if !ssh
        .run(host, &install_cmd(&arch, &temp), Some(bytes), SSH_UPLOAD_TIMEOUT)
        .ok()
    {
        dbg(format!("upload {host}: install failed"));
        return false;
    }
    // The install counts only once the remote reports the version just sent, so
    // an interleaved upload of a different build is never mistaken for ours.
    let installed = ssh.run(host, &version_check(&arch), None, SSH_OP_TIMEOUT).ok();
    if !installed {
        dbg(format!(
            "upload {host}: installed binary is not ghostd {}",
            env!("CARGO_PKG_VERSION")
        ));
    }
    installed
}

/// The delay before auto-reconnect attempt `attempt` (1-based), from a base of
/// `base_secs`, doubling per attempt and capped at [`RECONNECT_DELAY_CAP_SECS`].
pub fn reconnect_delay(attempt: u32, base_secs: u64) -> Duration {
    let base = base_secs.max(1);
    let doubled = base.saturating_mul(1u64 << attempt.saturating_sub(1).min(8));
    Duration::from_secs(doubled.min(RECONNECT_DELAY_CAP_SECS))
}

/// One (host, daemon-session) pair per host, first occurrence wins: a host runs
/// one daemon session, however many panes point at it.
pub fn dedupe_hosts(specs: Vec<(String, String)>) -> Vec<(String, String)> {
    let mut hosts: Vec<(String, String)> = Vec::new();
    for (host, session) in specs {
        if !hosts.iter().any(|(known, _)| known == &host) {
            hosts.push((host, session));
        }
    }
    hosts
}

/// Whether any pane in `tabs` is a remote pane that still has something to do on
/// its own: about to reconnect, or freshly dead and not yet classified.
///
/// The front-end has to keep ticking while this is true — the reconnect pass is
/// what advances those panes, and an idle front-end ticks at all only while it
/// asks for.
pub fn pending(tabs: &[Tab]) -> bool {
    tabs.iter().any(|tab| {
        tab.panes.iter().flatten().any(|entry| {
            entry.term().is_some_and(|handle| {
                handle.try_lock().map(|pane| remote_pending_pane(&pane)).unwrap_or(false)
            })
        })
    })
}

/// Whether one pane owes the reconnect pass a transition.
fn remote_pending_pane(pane: &crate::pane::Pane) -> bool {
    pane.term.remote.is_some()
        && (pane.term.exited.is_some() || pane.term.reconnect != ReconnectState::Idle)
}

/// Advances every dead remote pane in `tabs` through the bounded auto-reconnect
/// state machine, and respawns the ones that are due.
///
/// A pane that died with a non-zero code enters `Waiting` for attempt 1, waits
/// [`reconnect_delay`] for its next attempt, and drops to `Manual` once
/// `reconnect_attempts` is spent (or when the code was clean, or auto-reconnect
/// is off) — the state `Enter` connects from. A waiting pane whose host is not
/// uploaded yet re-arms 500 ms out instead of burning an attempt; a pane whose
/// stored host is not an ssh destination goes straight to `Manual` rather than
/// retrying a host that can never answer.
pub fn reconnect_pass(
    config: &Config,
    ready: &HashSet<String>,
    tabs: &mut [Tab],
    now: std::time::Instant,
) {
    let mut to_respawn: Vec<(usize, usize)> = Vec::new();
    for (tab_index, tab) in tabs.iter_mut().enumerate() {
        for (pane_index, slot) in tab.panes.iter_mut().enumerate() {
            let Some(handle) = slot.as_ref().and_then(PaneEntry::term) else {
                continue;
            };
            let Ok(mut guard) = handle.try_lock() else {
                continue;
            };
            let pane = &mut *guard;
            if pane.term.remote.is_none() {
                continue;
            }
            if pane.term.exited.is_some() && pane.term.reconnect == ReconnectState::Idle {
                let code = pane.term.last_exit_code.unwrap_or(255);
                let auto = config.remote.auto_reconnect && code != 0;
                if auto && pane.term.reconnect_failures < config.remote.reconnect_attempts {
                    pane.term.reconnect_failures += 1;
                    let delay = reconnect_delay(pane.term.reconnect_failures, config.remote.reconnect_delay_secs);
                    dbg(format!(
                        "reconnect {}: attempt {} in {:?}",
                        pane.term.name, pane.term.reconnect_failures, delay
                    ));
                    pane.term.reconnect = ReconnectState::Waiting {
                        attempt: pane.term.reconnect_failures,
                        next: now + delay,
                    };
                } else {
                    pane.term.reconnect = ReconnectState::Manual;
                }
            }
            if let ReconnectState::Waiting { attempt, next } = pane.term.reconnect {
                if now < next {
                    continue;
                }
                let Some(host) = pane.term.remote.as_ref().map(|remote| remote.host.clone()) else {
                    continue;
                };
                if !host_is_valid(&host) {
                    pane.term.reconnect = ReconnectState::Manual;
                    continue;
                }
                if ready.contains(&host) {
                    to_respawn.push((tab_index, pane_index));
                } else {
                    pane.term.reconnect = ReconnectState::Waiting {
                        attempt,
                        next: now + Duration::from_millis(UPLOAD_WAIT_MS),
                    };
                }
            }
        }
    }
    for (tab_index, pane_index) in to_respawn {
        let Some(handle) = tabs[tab_index]
            .panes
            .get(pane_index)
            .and_then(|slot| slot.as_ref())
            .and_then(PaneEntry::term)
        else {
            continue;
        };
        let Ok(mut guard) = handle.try_lock() else {
            continue;
        };
        if !guard.respawn_remote() {
            guard.term.reconnect = ReconnectState::Manual;
        }
    }
}

/// Collects the upload result of a background [`upload_ghostd`] run, so the
/// transaction is `mpsc::channel`-shaped like the pty bridge's.
pub type UploadResult = (String, bool);

/// The two ends of the ghostd upload channel.
pub fn upload_channel() -> (mpsc::Sender<UploadResult>, mpsc::Receiver<UploadResult>) {
    mpsc::channel()
}

/// Test doubles for the ssh seam.
#[cfg(test)]
pub mod testing {
    //! A runner that records instead of connecting, so a unit test can assert on
    //! what would have been run without dialing a host or killing anything.

    use super::*;
    use std::collections::VecDeque;

    /// An [`SshRunner`] that records every call and answers it from `scripted`
    /// (one outcome per call, in order) or, once that is empty, from the first
    /// `replies` entry whose needle is a substring of the command. The default
    /// answer is a clean exit 0 with no output.
    #[derive(Default)]
    pub struct FakeSsh {
        /// Every (host, command, stdin length) in call order.
        pub calls: Mutex<Vec<(String, String, usize)>>,
        /// Outcomes for the next calls, in order.
        pub scripted: Mutex<VecDeque<SshOutcome>>,
        /// Outcomes picked by command substring once `scripted` is empty.
        pub replies: Mutex<Vec<(String, SshOutcome)>>,
    }

    impl FakeSsh {
        /// Every recorded call, in order.
        pub fn calls(&self) -> Vec<(String, String, usize)> {
            self.calls.lock().unwrap_or_else(|e| e.into_inner()).clone()
        }

        /// The commands run so far, in order.
        pub fn commands(&self) -> Vec<String> {
            self.calls().into_iter().map(|(_, cmd, _)| cmd).collect()
        }

        /// The hosts ssh would have been opened to, in order.
        pub fn hosts(&self) -> Vec<String> {
            self.calls().into_iter().map(|(host, _, _)| host).collect()
        }

        /// Blocks until `n` calls have been recorded, returning what they were —
        /// the front-end runs its kills and uploads on their own threads.
        pub fn wait_calls(&self, n: usize, timeout: Duration) -> Vec<(String, String, usize)> {
            let deadline = std::time::Instant::now() + timeout;
            loop {
                let calls = self.calls();
                if calls.len() >= n || std::time::Instant::now() >= deadline {
                    return calls;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }

    impl SshRunner for FakeSsh {
        fn run(&self, host: &str, cmd: &str, stdin: Option<&[u8]>, _timeout: Duration) -> SshOutcome {
            self.calls
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push((host.to_string(), cmd.to_string(), stdin.map(<[u8]>::len).unwrap_or(0)));
            let scripted = self
                .scripted
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .pop_front();
            if let Some(outcome) = scripted {
                return outcome;
            }
            let replies = self.replies.lock().unwrap_or_else(|e| e.into_inner());
            replies
                .iter()
                .find(|(needle, _)| cmd.contains(needle.as_str()))
                .map(|(_, outcome)| outcome.clone())
                .unwrap_or(SshOutcome {
                    status: Some(0),
                    timed_out: false,
                    stdout: Vec::new(),
                })
        }

        fn pane_command(&self, _argv: &[String]) -> (String, Vec<String>) {
            ("/bin/sh".to_string(), vec!["-c".to_string(), "true".to_string()])
        }
    }
}

#[cfg(test)]
mod tests {
    //! The command lines, the naming, the upload sequence and the bounded child
    //! — the pure half of the subsystem. The state machine over real panes is
    //! exercised in [`crate::app`], where the panes live.
    //!
    //! No test here dials ssh or runs `pkill`: the ssh seam is either the
    //! recording [`testing::FakeSsh`] or a stub command run through a real POSIX
    //! shell, and the orphan sweep's regex is checked with `grep -E`, the same
    //! regex family `pkill -f` matches with.

    use super::*;
    use ghostwave::term::{RemoteSpec, HOST_MAX_LEN};
    use std::collections::VecDeque;
    use std::time::Instant;

    fn outcome(status: i32, stdout: &str) -> SshOutcome {
        SshOutcome {
            status: Some(status),
            timed_out: false,
            stdout: stdout.as_bytes().to_vec(),
        }
    }

    #[test]
    fn a_host_becomes_a_safe_session_token() {
        assert_eq!(sanitize_host("prod-server.example.com"), "prod-server.example.com");
        assert_eq!(sanitize_host("user@host:2222"), "userhost2222");
        assert_eq!(sanitize_host("host name"), "hostname");
    }

    #[test]
    fn the_session_names_are_the_references() {
        assert_eq!(remote_session("prod"), "ghostwave-prod");
        assert_eq!(ssh_session("prod"), "ssh-prod");
        assert_eq!(ssh_session("a@b"), "ssh-ab");
    }

    #[test]
    fn only_ssh_destinations_pass_the_host_boundary() {
        for host in [
            "prod",
            "prod-server.example.com",
            "user@host",
            "user@host:2222",
            "host:22",
            "[::1]:22",
            "user@[fe80::1%eth0]:22",
            "ssh://user@host:2222",
        ] {
            assert!(host_is_valid(host), "{host} is a supported destination form");
        }
        for host in [
            "",
            "host name",
            "host\tname",
            "host\nname",
            "prod'; rm -rf ~; '",
            "prod; rm -rf ~",
            "prod$(id)",
            "prod`id`",
            "prod|id",
            "prod>out",
            "prod&",
            "prod*",
            "prod#x",
            "prod\\x",
            "prod\"x\"",
        ] {
            assert!(!host_is_valid(host), "{host:?} is not an ssh destination");
        }
        let long = "a".repeat(HOST_MAX_LEN + 1);
        assert!(!host_is_valid(&long), "an over-long destination is refused");
    }

    #[test]
    fn a_binding_that_did_not_come_from_a_host_is_refused() {
        assert!(binding_is_valid("prod", &remote_session("prod")));
        assert!(binding_is_valid("user@host:2222", &remote_session("user@host:2222")));
        for (host, session) in [
            ("prod", "ghostwave-prod'; rm -rf ~; '"),
            ("prod", "ghostwave-other"),
            ("prod", "ghostwave-prod; pkill -9 -f ghostd"),
            ("prod", "ghostwave-"),
            ("prod", ""),
            ("prod'; rm -rf ~; '", "ghostwave-prodrm-rf"),
        ] {
            assert!(
                !binding_is_valid(host, session),
                "({host}, {session}) must not be acted on"
            );
        }
    }

    #[test]
    fn a_session_name_out_of_a_file_is_rederived_from_its_host() {
        let stored = r#"{"host":"prod","session":"ghostwave-prod'; rm -rf ~; '"}"#;
        let spec: RemoteSpec = serde_json::from_str(stored).expect("a stored binding parses");
        assert_eq!(spec.host, "prod");
        assert_eq!(spec.session, "ghostwave-prod", "the stored name is replaced");

        let round_trip: RemoteSpec = serde_json::from_str(
            &serde_json::to_string(&RemoteSpec::for_host("user@host:2222")).unwrap(),
        )
        .unwrap();
        assert_eq!(round_trip.session, remote_session("user@host:2222"));

        let missing: RemoteSpec = serde_json::from_str(r#"{"host":"prod"}"#)
            .expect("a stored binding without the derived field is tolerated");
        assert_eq!(missing.session, "ghostwave-prod");

        let restored = RemoteSpec {
            host: "prod".to_string(),
            session: "ghostwave-evil".to_string(),
        };
        assert_eq!(restored.rederived().session, "ghostwave-prod");
    }

    #[test]
    fn the_attach_command_checks_the_binary_then_attaches_with_a_fallback() {
        let cmd = attach_cmd("ghostwave-prod");
        assert!(cmd.starts_with("sh -c '"), "runs under a POSIX shell: {cmd}");
        assert!(cmd.contains("$HOME/.cache/ghostwave/ghostd-$(uname -m)"));
        assert!(
            cmd.contains(&format!("ghostd {}", env!("CARGO_PKG_VERSION"))),
            "the remote binary is version-checked against this app: {cmd}"
        );
        assert!(cmd.contains("missing/stale"), "the pane reports a stale binary: {cmd}");
        assert!(cmd.contains("attach \"ghostwave-prod\""));
        assert!(
            cmd.contains("new \"ghostwave-prod\"") && cmd.contains("exec"),
            "a missing session is created before attaching: {cmd}"
        );
        assert!(cmd.ends_with("'"), "the sh -c wrapper is closed: {cmd}");
    }

    #[test]
    fn no_host_text_reaches_a_remote_command_line() {
        let host = "prod'; rm -rf ~; '";
        let session = remote_session(host);
        let argv = attach_argv(host, &session);
        assert_eq!(argv[3], host, "the destination is one argv element");
        assert_eq!(argv[4], attach_cmd(&session));
        for cmd in [attach_cmd(&session), kill_cmd(&session)] {
            assert!(!cmd.contains("rm -rf"), "host text is not shell source: {cmd}");
            assert!(!cmd.contains(host), "the host is not interpolated at all: {cmd}");
            assert!(!cmd.contains("prod;"), "nor its parts: {cmd}");
        }
    }

    #[test]
    fn the_kill_command_escalates_and_anchors_its_orphan_sweep() {
        let cmd = kill_cmd("ghostwave-prod");
        assert!(cmd.starts_with("sh -c '"));
        assert!(
            cmd.contains("no ghostd binary on this host"),
            "reports a missing binary: {cmd}"
        );
        assert!(cmd.contains("kill \"ghostwave-prod\""), "the pidfile kill is quoted: {cmd}");
        assert!(
            cmd.contains(&format!(
                "pkill -9 -f {}",
                sweep_pattern("ghostwave-prod")
            )),
            "the sweep is anchored and leaves the name a literal: {cmd}"
        );
        assert!(cmd.ends_with("true'"), "the kill always reports success: {cmd}");
        assert!(
            !cmd.contains("pkill -9 -f \"ghostd-$(uname -m) new"),
            "the broad sweep is gone: {cmd}"
        );
    }

    #[test]
    fn the_quoting_helpers_escape_exactly_what_they_must() {
        assert_eq!(dquoted("ghostwave-prod"), "\"ghostwave-prod\"");
        assert_eq!(dquoted(r#"a"b"#), r#""a\"b""#);
        assert_eq!(dquoted(r"a\b"), r#""a\\b""#);
        assert_eq!(dquoted("a$b"), r#""a\$b""#);
        assert_eq!(dquoted("a`b"), r#""a\`b""#);
        assert_eq!(regex_escape("ghostwave-prod.example"), "ghostwave-prod\\.example");
        assert_eq!(regex_escape("a-b_c"), "a-b_c", "only metacharacters are escaped");
        assert_eq!(regex_escape("a;b|$()"), "a;b\\|\\$\\(\\)");
    }

    #[test]
    fn a_session_name_that_is_not_a_literal_still_stays_data() {
        let hostile = "ghostwave-prod'; : > \"$HOME/injected\"; '";
        // The name is double-quoted for argv integrity, then every `'` in that
        // word is escaped because the whole script is one single-quoted word.
        let word = dquoted(hostile).replace('\'', "'\\''");
        for cmd in [kill_cmd(hostile), attach_cmd(hostile)] {
            assert!(
                cmd.contains(&word),
                "the name arrives as one quoted word: {cmd}"
            );
            assert!(
                parses(&cmd),
                "a quote in the name leaves the script syntactically whole: {cmd}"
            );
            assert!(cmd.contains(r"\$HOME"), "even `$` is neutralised: {cmd}");
        }
    }

    /// Whether POSIX `grep -E` finds `pattern` in `line`. `pkill -f` matches its
    /// pattern with the same regex family, so this pins the sweep's semantics
    /// without running `pkill`.
    fn grep_matches(pattern: &str, line: &str) -> bool {
        let Ok(mut child) = Command::new("grep")
            .args(["-E", "-q", pattern])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        else {
            return false;
        };
        if let Some(mut sink) = child.stdin.take() {
            let _ = sink.write_all(line.as_bytes());
        }
        child.wait().map(|s| s.success()).unwrap_or(false)
    }

    /// The text a POSIX shell delivers for the shell word `word` — quote removal
    /// and all — so a test can assert on `pkill`'s argument instead of on the
    /// escaping that carries it.
    fn shell_word(word: &str) -> String {
        let out = Command::new("sh")
            .args(["-c", &format!("printf %s {word}")])
            .output()
            .expect("sh runs");
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// Whether `cmd` looks like one complete command to a POSIX shell — parsed
    /// but not run — which is what a session name breaking out of the script
    /// would destroy.
    fn parses(cmd: &str) -> bool {
        Command::new("sh")
            .args(["-n", "-c", cmd])
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    #[test]
    fn the_orphan_sweep_matches_its_own_session_and_nothing_else() {
        let pattern = kill_pattern("ghostwave-prod.example");
        for own in [
            "/home/u/.cache/ghostwave/ghostd-x86_64 new ghostwave-prod.example",
            "ghostd-aarch64 new ghostwave-prod.example /bin/bash",
        ] {
            assert!(grep_matches(&pattern, own), "the sweep matches {own:?}");
        }
        for other in [
            "/home/u/.cache/ghostwave/ghostd-x86_64 new ghostwave-prod-example",
            "/home/u/.cache/ghostwave/ghostd-x86_64 new ghostwave-prod.example2",
            "/home/u/.cache/ghostwave/ghostd-x86_64 attach ghostwave-prod.example",
            "/home/u/.cache/ghostwave/ghostd-x86_64 list",
            "grep -r new ghostwave-prod.example .",
            "pkill -9 -f \"(^|/)ghostd-[^ ]* new ghostwave-prod.example($| )\"",
            "sh -c 'BIN=x; pkill -9 -f \"(^|/)ghostd-[^ ]* new ghostwave-prod.example($| )\" || true; true'",
        ] {
            assert!(!grep_matches(&pattern, other), "the sweep must not match {other:?}");
        }
        assert!(
            !grep_matches(&pattern, &kill_cmd("ghostwave-prod.example")),
            "the sweep does not match the shell that runs it"
        );
    }

    /// The sweep the shell actually hands `pkill`: the end-of-line anchor must
    /// survive the quoting, or a daemon whose command line ends exactly at the
    /// session name — the orphan this sweep exists for — is never matched.
    #[test]
    fn the_shell_hands_pkill_an_end_anchored_sweep() {
        for session in ["ghostwave-prod", "ghostwave-prod.example"] {
            let pattern = kill_pattern(session);
            let delivered = shell_word(&sweep_pattern(session));

            assert_eq!(
                delivered, pattern,
                "quote removal must leave the regex unchanged"
            );
            assert!(
                !delivered.contains("\\$"),
                "the `$` is the anchor, not a literal dollar: {delivered}"
            );
            assert!(
                grep_matches(&delivered, &format!("/home/u/.cache/ghostwave/ghostd-x86_64 new {session}")),
                "a cmdline that ends exactly at the session name is swept: {delivered}"
            );
            assert!(
                grep_matches(&delivered, &format!("ghostd-aarch64 new {session} /bin/bash")),
                "so is one that starts the session with a command: {delivered}"
            );
            assert!(
                !grep_matches(&delivered, &format!("/home/u/.cache/ghostwave/ghostd-x86_64 new {session}2")),
                "but not a longer session name: {delivered}"
            );
        }
    }

    #[test]
    fn a_pane_attaches_over_a_tty_connection_with_a_bounded_timeout() {
        let argv = attach_argv("prod", "ghostwave-prod");
        assert_eq!(&argv[..4], ["-t", "-o", "ConnectTimeout=10", "prod"]);
        assert_eq!(argv.len(), 5);
        assert_eq!(argv[4], attach_cmd("ghostwave-prod"));
        assert_eq!(ssh_argv("prod"), vec!["prod".to_string()]);
    }

    /// A throwaway HOME with a stub `ghostd-<arch>` in it, so a real POSIX shell
    /// can run one of these commands the way a remote login shell would.
    struct StubRemote {
        home: std::path::PathBuf,
    }

    impl StubRemote {
        /// `version` is what the stub's `--version` prints; the stub records its
        /// argv in `<home>/record` for `attach`, `<home>/record.new` for `new`.
        fn new(version: &str) -> Option<Self> {
            use std::os::unix::fs::PermissionsExt;

            let home = std::env::temp_dir().join(format!("ghostwave-stub-{}", unique_token()));
            let cache = home.join(".cache/ghostwave");
            std::fs::create_dir_all(&cache).ok()?;
            let arch = Command::new("uname").arg("-m").output().ok()?;
            let arch = String::from_utf8_lossy(&arch.stdout).trim().to_string();
            let stub = format!(
                "#!/bin/sh\ncase \"$1\" in\n  --version) echo \"{version}\"; exit 0;;\n  attach) echo \"$*\" > \"$HOME/record\"; exit 0;;\n  new) echo \"$*\" > \"$HOME/record.new\"; exit 0;;\nesac\nexit 1\n"
            );
            let path = cache.join(format!("ghostd-{arch}"));
            std::fs::write(&path, stub).ok()?;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).ok()?;
            Some(Self { home })
        }

        /// Runs `command` the way a remote login shell would: `sh -c <command>`.
        fn run(&self, command: &str) -> std::process::Output {
            Command::new("sh")
                .args(["-c", command])
                .env("HOME", &self.home)
                .output()
                .expect("sh runs")
        }

        fn record(&self, name: &str) -> Option<String> {
            std::fs::read_to_string(self.home.join(name)).ok()
        }

        fn close(&self) {
            let _ = std::fs::remove_dir_all(&self.home);
        }
    }

    #[test]
    fn a_hostile_session_name_stays_one_argument_in_the_attach_script() {
        let stub = StubRemote::new(&format!("ghostd {}", env!("CARGO_PKG_VERSION")))
            .expect("the stub remote is built");
        let hostile = "ghostwave-prod'; : > \"$HOME/injected\"; '";

        let out = stub.run(&attach_cmd(hostile));

        assert!(out.status.success(), "the script ran: {out:?}");
        assert_eq!(
            stub.record("record").as_deref(),
            Some(format!("attach {hostile}\n").as_str()),
            "the injected text arrived as one argument, not as commands"
        );
        assert!(
            stub.record("injected").is_none(),
            "nothing outside the attach call was executed"
        );
        stub.close();
    }

    #[test]
    fn a_stale_binary_stops_the_pane_with_the_reconnect_hint() {
        let stub = StubRemote::new("ghostd 0.0.0").expect("the stub remote is built");

        let out = stub.run(&attach_cmd("ghostwave-prod"));

        assert!(!out.status.success(), "the pane reports the stale binary: {out:?}");
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(text.contains("ghostd missing/stale"), "got {text:?}");
        assert!(stub.record("record").is_none(), "no attach ran");
        stub.close();
    }

    /// The temp path an `install_cmd` writes through.
    fn install_temp(cmd: &str) -> String {
        cmd.split_once("T=\"$D/")
            .and_then(|(_, rest)| rest.split_once('"'))
            .map(|(temp, _)| temp.to_string())
            .expect("the install names a temp path")
    }

    #[test]
    fn an_upload_installs_through_a_unique_temp_then_verifies_the_version() {
        let ssh = testing::FakeSsh::default();
        {
            let mut scripted = ssh.scripted.lock().unwrap();
            for _ in 0..2 {
                scripted.push_back(outcome(0, "aarch64\n"));
                scripted.push_back(outcome(1, "")); // the cached binary is stale
                scripted.push_back(outcome(0, "")); // install
                scripted.push_back(outcome(0, "")); // version check after it
            }
        }

        assert!(upload_ghostd(&ssh, "prod"), "the first upload succeeded");
        assert!(upload_ghostd(&ssh, "prod"), "and so did the second");

        let commands = ssh.commands();
        let installs: Vec<&String> = commands.iter().filter(|cmd| cmd.contains("cat >")).collect();
        assert_eq!(installs.len(), 2, "each upload installed once: {commands:#?}");
        assert!(
            installs.iter().all(|cmd| cmd.contains("mv -f \"$T\" \"$D/ghostd-aarch64\"")),
            "the install publishes by rename: {installs:#?}"
        );
        let temp: Vec<String> = installs.iter().map(|cmd| install_temp(cmd)).collect();
        assert_ne!(temp[0], temp[1], "two uploads never share a temp path");
        assert!(
            temp.iter().all(|t| t.starts_with(&format!(
                "ghostd-aarch64.new.{}.",
                std::process::id()
            ))),
            "the temp path is unique to this process and upload: {temp:?}"
        );
        assert_eq!(
            ssh.calls()[2].2,
            GHOSTD_AARCH64.len(),
            "the binary was streamed to the install"
        );
        assert_eq!(ssh.hosts().iter().filter(|h| *h == "prod").count(), 8);
        assert!(
            commands.last().is_some_and(|cmd| cmd.contains("--version")),
            "the install is verified by version afterwards: {commands:#?}"
        );
    }

    #[test]
    fn an_upload_that_already_has_the_binary_installs_nothing() {
        let ssh = testing::FakeSsh::default();
        {
            let mut scripted = ssh.scripted.lock().unwrap();
            scripted.push_back(outcome(0, "x86_64\n"));
            scripted.push_back(outcome(0, ""));
        }

        assert!(upload_ghostd(&ssh, "prod"));

        let commands = ssh.commands();
        assert_eq!(commands.len(), 2, "no install ran: {commands:#?}");
        assert!(!commands.iter().any(|cmd| cmd.contains("cat >")));
    }

    #[test]
    fn an_upload_that_installs_the_wrong_version_reports_failure() {
        let ssh = testing::FakeSsh::default();
        {
            let mut scripted = ssh.scripted.lock().unwrap();
            scripted.push_back(outcome(0, "aarch64\n"));
            scripted.push_back(outcome(1, ""));
            scripted.push_back(outcome(0, ""));
            scripted.push_back(outcome(1, "")); // the installed binary is not ours
        }

        assert!(!upload_ghostd(&ssh, "prod"), "an unverified install is not ready");
    }

    #[test]
    fn an_upload_to_an_unknown_architecture_installs_nothing() {
        let ssh = testing::FakeSsh::default();
        {
            let mut scripted = ssh.scripted.lock().unwrap();
            scripted.push_back(outcome(0, "armv7l\n"));
        }

        assert!(!upload_ghostd(&ssh, "prod"));

        assert_eq!(ssh.commands(), vec!["uname -m".to_string()]);
    }

    #[test]
    fn an_upload_to_a_host_that_is_not_a_destination_runs_nothing() {
        let ssh = testing::FakeSsh::default();

        assert!(!upload_ghostd(&ssh, "prod; rm -rf ~"));

        assert!(ssh.calls().is_empty(), "no ssh was opened: {:?}", ssh.calls());
    }

    #[test]
    fn a_kill_of_a_binding_that_was_not_derived_runs_nothing() {
        let recorder = Arc::new(testing::FakeSsh::default());
        let ssh: Arc<dyn SshRunner> = recorder.clone();

        assert!(!spawn_kill(ssh, "prod", "ghostwave-evil'; rm -rf ~; '"));

        assert!(recorder.calls().is_empty(), "no ssh was opened: {:?}", recorder.calls());
    }

    #[test]
    fn a_kill_of_a_validated_binding_runs_the_kill_on_that_host() {
        let recorder = Arc::new(testing::FakeSsh::default());
        let ssh: Arc<dyn SshRunner> = recorder.clone();

        assert!(spawn_kill(
            ssh,
            "user@host:2222",
            &remote_session("user@host:2222")
        ));

        let calls = recorder.wait_calls(1, Duration::from_secs(5));
        assert_eq!(calls.len(), 1, "the kill ran once: {calls:?}");
        assert_eq!(calls[0].0, "user@host:2222");
        assert!(calls[0].1.contains("kill \"ghostwave-userhost2222\""));
        assert!(calls[0].1.starts_with("sh -c '"));
        assert_eq!(calls[0].2, 0, "the kill streams nothing on stdin");
    }

    #[test]
    fn a_command_that_never_terminates_is_killed_at_its_deadline() {
        let mut command = Command::new("sh");
        command.args(["-c", "sleep 30"]);
        let started = Instant::now();

        let outcome = run_bounded(&mut command, None, Duration::from_millis(300));

        assert!(outcome.timed_out, "the deadline ended it: {outcome:?}");
        assert!(!outcome.ok());
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the sleep did not run to completion: {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn a_completed_command_reports_its_output_and_feeds_stdin() {
        let mut feeding = Command::new("sh");
        feeding.args(["-c", "wc -c"]);
        let outcome = run_bounded(&mut feeding, Some(b"ninebytes"), Duration::from_secs(10));
        assert!(outcome.ok(), "{outcome:?}");
        assert_eq!(
            outcome.text().as_deref().map(str::trim),
            Some("9"),
            "stdin was streamed and closed"
        );

        let mut failing = Command::new("sh");
        failing.args(["-c", "exit 3"]);
        let outcome = run_bounded(&mut failing, None, Duration::from_secs(10));
        assert!(!outcome.ok(), "a non-zero exit is not ok: {outcome:?}");
        assert_eq!(outcome.status, Some(3));

        let mut missing = Command::new("no-such-ghostwave-binary");
        let outcome = run_bounded(&mut missing, None, Duration::from_secs(10));
        assert!(!outcome.ok() && !outcome.timed_out, "a spawn failure is a failure");
    }

    #[test]
    fn the_slot_pool_caps_commands_in_flight() {
        let pool = Arc::new(SlotPool::new(2));
        let first = pool.acquire();
        let _second = pool.acquire();
        let (tx, rx) = mpsc::channel();
        let waiting = {
            let pool = Arc::clone(&pool);
            std::thread::spawn(move || {
                let _third = pool.acquire();
                let _ = tx.send(());
            })
        };

        assert!(
            rx.recv_timeout(Duration::from_millis(200)).is_err(),
            "the third command waits for a slot"
        );
        drop(first);
        assert!(rx.recv_timeout(Duration::from_secs(5)).is_ok(), "a freed slot admits it");
        waiting.join().expect("the waiter ends");
    }

    #[test]
    fn the_backoff_starts_at_the_configured_delay_and_caps_at_fifteen_seconds() {
        assert_eq!(reconnect_delay(1, 3), Duration::from_secs(3));
        assert_eq!(reconnect_delay(2, 3), Duration::from_secs(6));
        assert_eq!(reconnect_delay(3, 3), Duration::from_secs(12));
        assert_eq!(reconnect_delay(4, 3), Duration::from_secs(15), "capped");
        assert_eq!(reconnect_delay(5, 3), Duration::from_secs(15), "still capped");
        assert_eq!(reconnect_delay(1, 0), Duration::from_secs(1), "a zero base is one second");
        assert_eq!(reconnect_delay(1, 10), Duration::from_secs(10));
        assert_eq!(reconnect_delay(9, 10), Duration::from_secs(15));
    }

    #[test]
    fn a_host_is_killed_once_however_many_panes_point_at_it() {
        let specs = vec![
            ("a".to_string(), "ghostwave-a".to_string()),
            ("b".to_string(), "ghostwave-b".to_string()),
            ("a".to_string(), "ghostwave-a".to_string()),
        ];
        assert_eq!(
            dedupe_hosts(specs),
            vec![
                ("a".to_string(), "ghostwave-a".to_string()),
                ("b".to_string(), "ghostwave-b".to_string()),
            ]
        );
    }
}
