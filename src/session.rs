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

    #[test]
    fn save_load_roundtrip() {
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
            }],
        };
        save("test-sess", &state);
        let loaded = load("test-sess").expect("session should reload");
        assert_eq!(loaded.tabs.len(), 1);
        assert_eq!(loaded.tabs[0].panes[0].cwd.as_deref(), Some("/home/user"));
        assert!(list_sessions().iter().any(|s| s == "test-sess"));
        delete("test-sess");
        assert!(!session_path("test-sess").exists());
    }

    #[test]
    fn truncate_keeps_tail() {
        let dir = dir();
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("trunc-test.log");
        let big = vec![b'x'; 100_000];
        std::fs::write(&path, &big).unwrap();
        let tail = truncate_log_front(&path, 10_000);
        assert_eq!(tail.len(), 10_000);
        assert!(std::fs::metadata(&path).unwrap().len() <= 10_000);
        let _ = std::fs::remove_file(&path);
    }
}
