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
//! Two invariants come straight from the reference and must not drift:
//! every remote command is wrapped in `sh -c '…'` so a host whose login shell is
//! not POSIX (fish) still parses it, and a remote pane is never auto-connected
//! on restore — see [`crate::pane::Pane::restore_remote`].

use std::collections::HashSet;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use ghostwave::config::Config;
use ghostwave::term::ReconnectState;

use crate::app::Tab;
use crate::pane::PaneEntry;

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

/// How long an ssh command that is not a pane gets, in seconds.
const SSH_CONNECT_TIMEOUT: &str = "5";

/// How long the ssh behind a durable pane gets to connect, in seconds.
const PANE_CONNECT_TIMEOUT: &str = "10";

/// Emits a remote-subsystem trace when `GHOSTWAVE_DEBUG` is set.
///
/// These are the steps that are otherwise invisible until something is wrong
/// (the ssh argv, the upload decisions, the reconnect transitions), which is why
/// the reference kept them behind one switch as well.
fn dbg(msg: impl AsRef<str>) {
    if std::env::var("GHOSTWAVE_DEBUG").map(|v| v == "0").unwrap_or(true) {
        return;
    }
    eprintln!("[ghostwave] {}", msg.as_ref());
}

/// The host string as a filesystem-safe token, for session names.
pub fn sanitize_host(host: &str) -> String {
    host.chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '.' || *c == '-' || *c == '_')
        .collect()
}

/// The remote ghostd session a durable pane binds to: `ghostwave-<host>`.
pub fn remote_session(host: &str) -> String {
    format!("ghostwave-{}", sanitize_host(host))
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
pub fn attach_argv(host: &str, session: &str) -> Vec<String> {
    vec![
        "-t".to_string(),
        "-o".to_string(),
        format!("ConnectTimeout={PANE_CONNECT_TIMEOUT}"),
        host.to_string(),
        attach_cmd(host, session),
    ]
}

/// The remote shell command a durable pane runs: check the cached ghostd binary
/// is present and current, then attach — creating the session when it is gone.
///
/// The version check compares the remote binary against this app's own version,
/// so a rebuilt daemon is re-uploaded rather than reused.
pub fn attach_cmd(host: &str, session: &str) -> String {
    format!(
        "sh -c 'BIN=\"$HOME/.cache/ghostwave/ghostd-$(uname -m)\"; \
         if [ ! -x \"$BIN\" ] || [ \"$($BIN --version 2>/dev/null)\" != \"ghostd {ver}\" ]; then \
         echo \"ghostd missing/stale on {host} — reconnect to upload\"; exit 1; fi; \
         \"$BIN\" attach {session} 2>/dev/null || {{ \"$BIN\" new {session} >/dev/null 2>&1 && exec \"$BIN\" attach {session}; }}'",
        ver = env!("CARGO_PKG_VERSION")
    )
}

/// The remote shell command that kills a durable session's daemon.
///
/// Runs on its own ssh connection, so it works when the pane is dead or the
/// daemon is wedged: the daemon escalates TERM → KILL for itself and the
/// session's process group, and the `pkill` is the last resort for orphans whose
/// pidfile is already gone.
pub fn kill_cmd(host: &str, session: &str) -> String {
    format!(
        "sh -c 'BIN=\"$HOME/.cache/ghostwave/ghostd-$(uname -m)\"; \
         if [ ! -x \"$BIN\" ]; then echo \"no ghostd binary on {host}\"; exit 1; fi; \
         \"$BIN\" kill {session} 2>&1 || true; \
         pkill -9 -f \"ghostd-$(uname -m) new {session}\" 2>/dev/null || true; true'"
    )
}

/// Opens a fresh ssh connection to `host` and kills its `session` daemon.
///
/// Fire and forget: the result is reported by the daemon itself, and the caller
/// is a key press.
pub fn spawn_kill(host: &str, session: &str) {
    let host = host.to_string();
    let session = session.to_string();
    dbg(format!("kill {session} on {host}: spawning ssh"));
    std::thread::spawn(move || {
        let _ = std::process::Command::new("ssh")
            .args([
                "-o",
                &format!("ConnectTimeout={SSH_CONNECT_TIMEOUT}"),
                &host,
                &kill_cmd(&host, &session),
            ])
            .status();
    });
}

/// Runs a short ssh command; true on exit 0.
fn ssh_ok(host: &str, cmd: &str) -> bool {
    std::process::Command::new("ssh")
        .args(["-o", &format!("ConnectTimeout={SSH_CONNECT_TIMEOUT}"), host, cmd])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Runs a short ssh command; its stdout, `None` when it failed.
fn ssh_out(host: &str, cmd: &str) -> Option<String> {
    let out = std::process::Command::new("ssh")
        .args(["-o", &format!("ConnectTimeout={SSH_CONNECT_TIMEOUT}"), host, cmd])
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Makes sure the ghostd binary for `host` is present and current in
/// `~/.cache/ghostwave` on the remote. True when it is, or became, ready.
///
/// Blocking: the caller runs it on the upload thread. False covers every way it
/// cannot be ready — no ssh, an unknown architecture, or a failed upload.
pub fn upload_ghostd(host: &str) -> bool {
    let Some(arch) = ssh_out(host, "uname -m").map(|a| a.trim().to_string()) else {
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
    let check = format!(
        "sh -c 'BIN=\"$HOME/.cache/ghostwave/ghostd-{arch}\"; [ -x \"$BIN\" ] && [ \"$($BIN --version 2>/dev/null)\" = \"ghostd {ver}\" ]'",
        ver = env!("CARGO_PKG_VERSION")
    );
    if ssh_ok(host, &check) {
        dbg(format!("upload {host}: binary current, no upload"));
        return true;
    }
    dbg(format!("upload {host}: uploading {} bytes", bytes.len()));
    let upload = format!(
        "sh -c 'mkdir -p \"$HOME/.cache/ghostwave\" && cat > \"$HOME/.cache/ghostwave/ghostd-{arch}.new\" && chmod +x \"$HOME/.cache/ghostwave/ghostd-{arch}.new\" && mv -f \"$HOME/.cache/ghostwave/ghostd-{arch}.new\" \"$HOME/.cache/ghostwave/ghostd-{arch}\"'"
    );
    let mut child = match std::process::Command::new("ssh")
        .args(["-o", &format!("ConnectTimeout={SSH_CONNECT_TIMEOUT}"), host, &upload])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(_) => return false,
    };
    {
        use std::io::Write;
        if let Some(mut sink) = child.stdin.take() {
            let _ = sink.write_all(bytes);
        }
    }
    child.wait().map(|status| status.success()).unwrap_or(false)
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
/// uploaded yet re-arms 500 ms out instead of burning an attempt.
pub fn reconnect_pass(
    config: &Config,
    ready: &HashSet<String>,
    tabs: &mut [Tab],
    now: Instant,
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

#[cfg(test)]
mod tests {
    //! The command lines, the naming and the backoff schedule — the pure half of
    //! the subsystem. The state machine over real panes is exercised in
    //! [`crate::app`], where the panes live.

    use super::*;

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
    fn the_attach_command_checks_the_binary_then_attaches_with_a_fallback() {
        let cmd = attach_cmd("prod", "ghostwave-prod");
        assert!(cmd.starts_with("sh -c '"), "runs under a POSIX shell: {cmd}");
        assert!(cmd.contains("$HOME/.cache/ghostwave/ghostd-$(uname -m)"));
        assert!(
            cmd.contains(&format!("ghostd {}", env!("CARGO_PKG_VERSION"))),
            "the remote binary is version-checked against this app: {cmd}"
        );
        assert!(cmd.contains("missing/stale on prod"), "the pane reports a stale binary: {cmd}");
        assert!(cmd.contains("attach ghostwave-prod"));
        assert!(
            cmd.contains("new ghostwave-prod") && cmd.contains("exec"),
            "a missing session is created before attaching: {cmd}"
        );
        assert!(cmd.ends_with("'"), "the sh -c wrapper is closed: {cmd}");
    }

    #[test]
    fn the_kill_command_escalates_and_sweeps_orphans() {
        let cmd = kill_cmd("prod", "ghostwave-prod");
        assert!(cmd.starts_with("sh -c '"));
        assert!(cmd.contains("no ghostd binary on prod"), "reports a missing binary: {cmd}");
        assert!(cmd.contains("kill ghostwave-prod"));
        assert!(
            cmd.contains("pkill -9 -f \"ghostd-$(uname -m) new ghostwave-prod\""),
            "orphans whose pidfile is gone are swept by name: {cmd}"
        );
        assert!(cmd.ends_with("true'"), "the kill always reports success: {cmd}");
    }

    #[test]
    fn a_pane_attaches_over_a_tty_connection_with_a_bounded_timeout() {
        let argv = attach_argv("prod", "ghostwave-prod");
        assert_eq!(&argv[..4], ["-t", "-o", "ConnectTimeout=10", "prod"]);
        assert_eq!(argv.len(), 5);
        assert_eq!(argv[4], attach_cmd("prod", "ghostwave-prod"));
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
