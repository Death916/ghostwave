//! Durable sessions (waveterm/zellij-style): every pane's pty output is
//! tee'd to a log file; sessions are NAMED and saved as JSON under
//! ~/.local/share/ghostwave/sessions/<name>.json. The app runs inside one
//! session (default: "default"); Ctrl+o → session mode → `w` opens the
//! session manager (switch/new/rename/delete). Switching sessions replaces
//! the current tabs and replays the saved pane logs, restoring screen AND
//! scrollback.
//!
//! Design decisions:
//! - ALL panes log (cheap; the whole session is saved, no per-tab opt-in).
//! - Logs are raw pty bytes, replayed with vt_write. Exact restoration.
//! - Log cap: config max_log_mb per pane; when exceeded the log is truncated
//!   at the front — the restored terminal then shows the tail, and the screen
//!   may start mid-state. (Checkpointing the screen state would require
//!   Format::Full which libghostty-vt 0.2.1 does not expose.)
//! - Old logs (panes that no longer exist in ANY session) are pruned at save.
//! - Legacy single session.json is migrated to sessions/default.json.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Default, Clone)]
pub struct SessionState {
    pub tabs: Vec<TabState>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct TabState {
    pub name: String,
    pub panes: Vec<PaneState>,
    /// Layout tree (pane ids reference `panes`). Absent in old files →
    /// single-pane layout on restore.
    pub root: Option<crate::layout::Node>,
    /// Floating panes (zellij style). Absent in old files → none.
    #[serde(default)]
    pub floating: Vec<FloatingPaneState>,
}

/// A floating pane's persisted state: pane id (remapped at save), overlay
/// position/size in CELLS (grid-independent), its tree origin, and whether
/// it is hidden (bg pane).
#[derive(Serialize, Deserialize, Clone)]
pub struct FloatingPaneState {
    pub pane: usize,
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
    #[serde(default)]
    pub origin: Option<crate::layout::FloatOrigin>,
    #[serde(default)]
    pub hidden: bool,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct PaneState {
    pub program: Option<String>,
    pub args: Vec<String>,
    pub cwd: Option<String>,
    /// Durable remote binding (ssh+ghostd). Present = restore the pane
    /// DISCONNECTED (no auto-connect; the user connects or reconnects).
    #[serde(default)]
    pub remote: Option<crate::term::RemoteSpec>,
    /// Log filename (relative to the session dir).
    pub log: String,
}

/// ~/.local/share/ghostwave (XDG_DATA_HOME aware).
pub fn dir() -> PathBuf {
    let base = std::env::var_os("XDG_DATA_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let home = std::env::var_os("HOME").unwrap_or_default();
            PathBuf::from(home).join(".local/share")
        });
    base.join("ghostwave")
}

pub fn sessions_dir() -> PathBuf {
    dir().join("sessions")
}

pub fn session_path(name: &str) -> PathBuf {
    // Names come from user input; sanitize to a safe filename.
    let safe: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    sessions_dir().join(format!("{safe}.json"))
}

/// Last-used session name marker.
pub fn last_session_path() -> PathBuf {
    dir().join("last_session")
}

pub fn last_session() -> String {
    std::fs::read_to_string(last_session_path())
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "default".into())
}

pub fn set_last_session(name: &str) {
    let _ = std::fs::create_dir_all(dir());
    let _ = std::fs::write(last_session_path(), name);
}

/// Sorted names of saved sessions (excluding empty).
pub fn list_sessions() -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(sessions_dir()) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if let Some(stem) = name.strip_suffix(".json") {
                out.push(stem.to_string());
            }
        }
    }
    out.sort();
    out
}

pub fn pane_log_path(name: &str) -> PathBuf {
    dir().join("panes").join(name)
}

/// Migrate the legacy single session.json → sessions/default.json.
fn migrate_legacy() {
    let legacy = dir().join("session.json");
    if legacy.exists() && !session_path("default").exists() {
        let _ = std::fs::create_dir_all(sessions_dir());
        if let Ok(bytes) = std::fs::read(&legacy) {
            let _ = std::fs::write(session_path("default"), bytes);
        }
        let _ = std::fs::remove_file(&legacy);
    }
}

pub fn load(name: &str) -> Option<SessionState> {
    migrate_legacy();
    let bytes = std::fs::read(session_path(name)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Whether any pane in the session is a durable remote (ssh) connection —
/// used by the session manager to show an "ssh" marker.
pub fn has_remote(name: &str) -> bool {
    load(name).is_some_and(|s| {
        s.tabs.iter().any(|t| t.panes.iter().any(|p| p.remote.is_some()))
    })
}

/// All durable remote (host, daemon-session) pairs in a saved session —
/// used by the session manager's kill action.
pub fn remote_specs(name: &str) -> Vec<(String, String)> {
    let Some(s) = load(name) else { return vec![] };
    let mut v = Vec::new();
    for t in &s.tabs {
        for p in &t.panes {
            if let Some(r) = &p.remote {
                v.push((r.host.clone(), r.session.clone()));
            }
        }
    }
    v
}

/// True when EVERY pane in the saved session is a durable remote pane —
/// a pure `ds` session, safe to delete wholesale after killing its
/// daemons (mixed sessions keep their local tabs).
pub fn is_pure_remote(name: &str) -> bool {
    load(name).is_some_and(|s| {
        !s.tabs.is_empty()
            && s.tabs.iter().all(|t| {
                !t.panes.is_empty() && t.panes.iter().all(|p| p.remote.is_some())
            })
    })
}

pub fn save(name: &str, state: &SessionState) {
    let _ = std::fs::create_dir_all(sessions_dir());
    match serde_json::to_string_pretty(state) {
        Ok(json) => {
            let _ = std::fs::write(session_path(name), json);
            prune_logs();
        }
        Err(e) => eprintln!("ghostwave: session save failed: {e}"),
    }
}

pub fn delete(name: &str) {
    let _ = std::fs::remove_file(session_path(name));
    if last_session() == name {
        let _ = std::fs::remove_file(last_session_path());
    }
    prune_logs();
}

/// Rename a saved session (preserving its pane log references).
pub fn rename(old: &str, new: &str) {
    if old == new {
        return;
    }
    if let Some(state) = load(old) {
        save(new, &state);
    }
    let _ = std::fs::remove_file(session_path(old));
    if last_session() == old {
        set_last_session(new);
    }
}

/// Delete pane logs not referenced by ANY saved session.
fn prune_logs() {
    migrate_legacy();
    let panes_dir = dir().join("panes");
    let Ok(entries) = std::fs::read_dir(&panes_dir) else {
        return;
    };
    let mut live: std::collections::HashSet<String> = std::collections::HashSet::new();
    for name in list_sessions() {
        if let Some(state) = load(&name) {
            for tab in &state.tabs {
                for p in &tab.panes {
                    live.insert(p.log.clone());
                }
            }
        }
    }
    for entry in entries.flatten() {
        if let Some(name) = entry.file_name().to_str().map(String::from) {
            if !live.contains(&name) {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
}

/// Open (create) a pane log file with a unique name.
pub fn open_pane_log() -> std::io::Result<(String, std::fs::File)> {
    let panes_dir = dir().join("panes");
    std::fs::create_dir_all(&panes_dir)?;
    for _ in 0..100 {
        let name = format!(
            "pane-{}-{}.log",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        let path = panes_dir.join(&name);
        match std::fs::OpenOptions::new().create_new(true).append(true).open(&path) {
            Ok(f) => return Ok((name, f)),
            Err(_) => continue, // collision; retry
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "could not allocate pane log name",
    ))
}

#[derive(Debug)]
enum LogMsg {
    Write(Vec<u8>),
    Seed(Vec<u8>),
    Truncate(u64),
    Flush,
}

/// Asynchronous background worker for durable session logging.
/// Eliminates all disk write and truncation latency from the terminal UI thread.
pub struct AsyncPaneLog {
    pub name: String,
    tx: std::sync::mpsc::SyncSender<LogMsg>,
    cap: u64,
    written_bytes: u64,
}

impl AsyncPaneLog {
    pub fn open(log_cap_bytes: u64) -> Option<Self> {
        let (name, file) = open_pane_log().ok()?;
        let (tx, rx) = std::sync::mpsc::sync_channel(512);
        let path = pane_log_path(&name);

        std::thread::Builder::new()
            .name(format!("pane-log-{}", name))
            .spawn(move || {
                use std::io::Write;
                let mut writer = std::io::BufWriter::with_capacity(65536, file);
                let mut bytes_since_trunc = 0u64;

                while let Ok(msg) = rx.recv() {
                    match msg {
                        LogMsg::Write(data) => {
                            let _ = writer.write_all(&data);
                            bytes_since_trunc += data.len() as u64;
                        }
                        LogMsg::Seed(data) => {
                            let _ = writer.write_all(&data);
                            let _ = writer.flush();
                            bytes_since_trunc += data.len() as u64;
                        }
                        LogMsg::Truncate(cap) => {
                            let _ = writer.flush();
                            truncate_log_front(&path, cap);
                            if let Ok(f) = std::fs::OpenOptions::new().append(true).open(&path) {
                                writer = std::io::BufWriter::with_capacity(65536, f);
                            }
                            bytes_since_trunc = cap;
                        }
                        LogMsg::Flush => {
                            let _ = writer.flush();
                        }
                    }
                }
                let _ = writer.flush();
            })
            .ok()?;

        Some(Self {
            name,
            tx,
            cap: log_cap_bytes.max(1024),
            written_bytes: 0,
        })
    }

    #[inline]
    pub fn write(&mut self, data: &[u8]) {
        self.written_bytes += data.len() as u64;
        let _ = self.tx.try_send(LogMsg::Write(data.to_vec()));
        const HYSTERESIS_BYTES: u64 = 2 * 1024 * 1024;
        if self.written_bytes > self.cap.saturating_add(HYSTERESIS_BYTES) {
            let _ = self.tx.send(LogMsg::Truncate(self.cap));
            self.written_bytes = self.cap;
        }
    }

    pub fn seed(&mut self, data: &[u8]) {
        self.written_bytes += data.len() as u64;
        let _ = self.tx.send(LogMsg::Seed(data.to_vec()));
    }

    pub fn flush(&self) {
        let _ = self.tx.send(LogMsg::Flush);
    }
}

/// Truncate a log from the front (keeps only the last `keep` bytes).
/// Returns the bytes to replay (the retained tail).
pub fn truncate_log_front(path: &PathBuf, keep: u64) -> Vec<u8> {
    let Ok(meta) = std::fs::metadata(path) else {
        return Vec::new();
    };
    let len = meta.len();
    if len <= keep {
        return Vec::new();
    }
    // Read the tail, rewrite the file with just the tail.
    let mut f = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };
    use std::io::{Read, Seek, SeekFrom};
    let _ = f.seek(SeekFrom::Start(len - keep));
    let mut tail = Vec::with_capacity(keep as usize);
    let _ = f.read_to_end(&mut tail);
    let _ = std::fs::write(path, &tail);
    tail
}

/// Replay a pane's logged pty output into a fresh terminal (restores the
/// screen AND scrollback). Oversized logs are truncated at the front first
/// (cap in bytes). Returns the replayed bytes (caller seeds the new pane's
/// log with them so history survives further restarts).
pub fn replay_pane_log(
    name: &str,
    term: &mut libghostty_vt::Terminal<'_, '_>,
    cap_bytes: u64,
) -> Vec<u8> {
    let path = pane_log_path(name);
    let data = match std::fs::read(&path) {
        Ok(d) if d.len() as u64 > cap_bytes => truncate_log_front(&path, cap_bytes),
        Ok(d) => d,
        Err(_) => return Vec::new(),
    };
    for chunk in data.chunks(16384) {
        let _ = term.vt_write(chunk);
    }
    data
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static SESS_COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn unique_sess(prefix: &str) -> String {
        let n = SESS_COUNTER.fetch_add(1, Ordering::SeqCst);
        format!("{prefix}-{}-{}", std::process::id(), n)
    }

    #[test]
    fn save_load_roundtrip() {
        let name = unique_sess("test-sess-rt");
        let state = SessionState {
            tabs: vec![TabState {
                name: "work".into(),
                panes: vec![PaneState {
                    program: None,
                    args: vec![],
                    cwd: Some("/home/user".into()),
                    remote: None,
                    log: "pane-test.log".into(),
                }],
                root: Some(crate::layout::Node::Leaf { pane: 0 }),
                floating: vec![],
            }],
        };
        save(&name, &state);
        let loaded = load(&name).expect("session should reload");
        assert_eq!(loaded.tabs.len(), 1);
        assert_eq!(loaded.tabs[0].name, "work");
        assert_eq!(loaded.tabs[0].panes[0].cwd.as_deref(), Some("/home/user"));
        assert_eq!(loaded.tabs[0].panes[0].log, "pane-test.log");
        assert!(loaded.tabs[0].root.is_some(), "layout root should survive roundtrip");
        assert!(list_sessions().iter().any(|s| s == &name), "saved session should appear in list");
        // multi-tab roundtrip
        let name2 = unique_sess("test-sess-multi");
        let state2 = SessionState {
            tabs: vec![
                TabState { name: "a".into(), panes: vec![PaneState { program: Some("/bin/sh".into()), args: vec!["-c".into()], cwd: None, remote: None, log: "l1".into() }], root: None, floating: vec![] },
                TabState { name: "b".into(), panes: vec![PaneState { program: None, args: vec![], cwd: None, remote: None, log: "l2".into() }], root: None, floating: vec![] },
            ],
        };
        save(&name2, &state2);
        let loaded2 = load(&name2).unwrap();
        assert_eq!(loaded2.tabs.len(), 2);
        assert_eq!(loaded2.tabs[1].name, "b");
        delete(&name);
        delete(&name2);
        assert!(!session_path(&name).exists(), "deleted session file should be gone");
        assert!(!session_path(&name2).exists());
    }

    #[test]
    fn truncate_keeps_tail() {
        let dir = dir();
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(format!("trunc-test-{}-{}.log", std::process::id(), SESS_COUNTER.fetch_add(1, Ordering::SeqCst)));
        let big = vec![b'x'; 100_000];
        std::fs::write(&path, &big).unwrap();
        let tail = truncate_log_front(&path, 10_000);
        assert_eq!(tail.len(), 10_000);
        assert!(tail.iter().all(|&b| b == b'x'), "tail should be all x's");
        assert!(std::fs::metadata(&path).unwrap().len() <= 10_000);
        assert_eq!(std::fs::read(&path).unwrap(), tail, "file should now equal the returned tail");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn truncate_under_cap_is_noop() {
        let dir = dir();
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(format!("trunc-noop-{}-{}.log", std::process::id(), SESS_COUNTER.fetch_add(1, Ordering::SeqCst)));
        let small = b"hello".to_vec();
        std::fs::write(&path, &small).unwrap();
        let tail = truncate_log_front(&path, 10_000);
        assert!(tail.is_empty(), "under-cap should return empty (no truncation needed)");
        assert_eq!(std::fs::read(&path).unwrap(), small, "file should be untouched");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn session_path_sanitizes() {
        // Slashes and dots become underscores, so path traversal is blocked
        let p2 = session_path("a/b");
        assert!(p2.file_name().unwrap().to_string_lossy().contains("a_b"), "slash should become underscore, got {:?}", p2);
        assert!(p2.to_string_lossy().ends_with(".json"));
        let p3 = session_path("../../etc/passwd");
        let fname = p3.file_name().unwrap().to_string_lossy();
        assert!(!fname.contains('/'), "filename should not contain slash, got {fname:?}");
        assert!(!fname.contains(".."), "filename should not contain .. traversal");
        // stays under sessions_dir
        assert!(p3.starts_with(sessions_dir()), "sanitized path should stay under sessions dir, got {p3:?}");
    }

    #[test]
    fn load_missing_returns_none() {
        let name = unique_sess("does-not-exist-zzz");
        assert!(load(&name).is_none(), "missing session should be None");
    }
}
