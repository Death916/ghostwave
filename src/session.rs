//! Durable sessions (waveterm/zellij-style): every pane's pty output is
//! tee'd to a log file; on quit the session state (tabs marked durable) is
//! saved as JSON. On startup, durable tabs are recreated and their logs are
//! replayed into fresh terminals, restoring screen AND scrollback.
//!
//! Design decisions:
//! - ALL panes log (cheap; a pane designated durable mid-session keeps its
//!   full history). Only durable tabs are restored.
//! - Logs are raw pty bytes, replayed with vt_write. Exact restoration.
//! - Log cap: MAX_LOG_BYTES per pane; when exceeded the log is truncated at
//!   the front — the restored terminal then shows the tail, and the screen
//!   may start mid-state. (Checkpointing the screen state would require
//!   Format::Full which libghostty-vt 0.2.1 does not expose.)
//! - Old logs (panes that no longer exist) are pruned at save time.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub const MAX_LOG_BYTES: u64 = 32 * 1024 * 1024;

#[derive(Serialize, Deserialize, Default, Clone)]
pub struct SessionState {
    pub tabs: Vec<TabState>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct TabState {
    pub name: String,
    pub durable: bool,
    pub panes: Vec<PaneState>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct PaneState {
    pub program: Option<String>,
    pub args: Vec<String>,
    pub cwd: Option<String>,
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

pub fn state_path() -> PathBuf {
    dir().join("session.json")
}

pub fn pane_log_path(name: &str) -> PathBuf {
    dir().join("panes").join(name)
}

pub fn load() -> Option<SessionState> {
    let bytes = std::fs::read(state_path()).ok()?;
    serde_json::from_slice(&bytes).ok()
}

pub fn save(state: &SessionState) {
    let _ = std::fs::create_dir_all(dir().join("panes"));
    match serde_json::to_string_pretty(state) {
        Ok(json) => {
            let _ = std::fs::write(state_path(), json);
            prune_logs(state);
        }
        Err(e) => eprintln!("ghostwave: session save failed: {e}"),
    }
}

/// Delete pane logs not referenced by the current session.
fn prune_logs(state: &SessionState) {
    let panes_dir = dir().join("panes");
    let Ok(entries) = std::fs::read_dir(&panes_dir) else {
        return;
    };
    let live: std::collections::HashSet<String> = state
        .tabs
        .iter()
        .flat_map(|t| t.panes.iter().map(|p| p.log.clone()))
        .collect();
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
/// screen AND scrollback). Oversized logs are truncated at the front first.
/// Returns the replayed bytes (caller seeds the new pane's log with them so
/// history survives further restarts).
pub fn replay_pane_log(name: &str, term: &mut libghostty_vt::Terminal<'_, '_>) -> Vec<u8> {
    let path = pane_log_path(name);
    let data = match std::fs::read(&path) {
        Ok(d) if d.len() as u64 > MAX_LOG_BYTES => truncate_log_front(&path, MAX_LOG_BYTES),
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
                durable: true,
                panes: vec![PaneState {
                    program: None,
                    args: vec![],
                    cwd: Some("/home/death916".into()),
                    log: "pane-test.log".into(),
                }],
            }],
        };
        save(&state);
        let loaded = load().expect("session should reload");
        assert_eq!(loaded.tabs.len(), 1);
        assert!(loaded.tabs[0].durable);
        assert_eq!(loaded.tabs[0].panes[0].cwd.as_deref(), Some("/home/death916"));
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
