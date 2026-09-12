//! Native in-process clipboard access via arboard (Wayland + X11) with CLI tool fallback.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc::{channel, RecvTimeoutError};
use std::sync::Mutex;
use std::time::Duration;
use arboard::{Clipboard, GetExtLinux, LinuxClipboardKind, SetExtLinux};

static CLIPBOARD: Mutex<Option<Clipboard>> = Mutex::new(None);
static LAST_COPIED: Mutex<Option<String>> = Mutex::new(None);
static LAST_PRIMARY: Mutex<Option<String>> = Mutex::new(None);

fn with_clipboard<R>(f: impl FnOnce(&mut Clipboard) -> R) -> Option<R> {
    let mut guard = CLIPBOARD.lock().ok()?;
    if guard.is_none() {
        *guard = Clipboard::new().ok();
    }
    guard.as_mut().map(f)
}

pub fn write_text(text: &str) -> bool {
    if let Ok(mut cache) = LAST_COPIED.lock() {
        *cache = Some(text.to_string());
    }
    let _ = with_clipboard(|cb| cb.set_text(text.to_string()));
    write_text_cli(text, false)
}

pub fn write_primary_text(text: &str) -> bool {
    if let Ok(mut cache) = LAST_PRIMARY.lock() {
        *cache = Some(text.to_string());
    }
    let _ = with_clipboard(|cb| {
        cb.set()
            .clipboard(LinuxClipboardKind::Primary)
            .text(text.to_string())
    });
    write_text_cli(text, true)
}

pub fn read_text() -> Option<String> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let res = with_clipboard(|cb| cb.get_text().ok()).flatten();
        let _ = tx.send(res);
    });

    match rx.recv_timeout(Duration::from_millis(50)) {
        Ok(Some(text)) if !text.is_empty() => Some(text),
        _ => {
            // Fallback to in-memory cached copy (prevents Wayland self-paste deadlock)
            if let Ok(guard) = LAST_COPIED.lock() {
                if let Some(cached) = guard.as_ref() {
                    if !cached.is_empty() {
                        return Some(cached.clone());
                    }
                }
            }
            read_text_cli(false)
        }
    }
}

pub fn read_primary_text() -> Option<String> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let res = with_clipboard(|cb| {
            cb.get()
                .clipboard(LinuxClipboardKind::Primary)
                .text()
                .ok()
        })
        .flatten();
        let _ = tx.send(res);
    });

    match rx.recv_timeout(Duration::from_millis(50)) {
        Ok(Some(text)) if !text.is_empty() => Some(text),
        _ => {
            if let Ok(guard) = LAST_PRIMARY.lock() {
                if let Some(cached) = guard.as_ref() {
                    if !cached.is_empty() {
                        return Some(cached.clone());
                    }
                }
            }
            if let Ok(guard) = LAST_COPIED.lock() {
                if let Some(cached) = guard.as_ref() {
                    if !cached.is_empty() {
                        return Some(cached.clone());
                    }
                }
            }
            read_text_cli(true)
        }
    }
}

// ---- CLI tool fallbacks ---------------------------------------------------

fn tool_candidates(is_copy: bool) -> Vec<&'static str> {
    let has_wayland = std::env::var_os("WAYLAND_DISPLAY").is_some();
    if is_copy {
        if has_wayland {
            vec!["wl-copy", "xclip", "xsel"]
        } else {
            vec!["xclip", "xsel", "wl-copy"]
        }
    } else {
        if has_wayland {
            vec!["wl-paste", "xclip", "xsel"]
        } else {
            vec!["xclip", "xsel", "wl-paste"]
        }
    }
}

fn find_cmd(names: &[&'static str]) -> Option<PathBuf> {
    let mut search_dirs = Vec::new();
    if let Some(path) = std::env::var_os("PATH") {
        for d in std::env::split_paths(&path) {
            search_dirs.push(d);
        }
    }
    for fallback in [
        "/usr/local/bin",
        "/usr/bin",
        "/bin",
        "/run/current-system/sw/bin",
    ] {
        let p = PathBuf::from(fallback);
        if !search_dirs.contains(&p) {
            search_dirs.push(p);
        }
    }

    for &name in names {
        for dir in &search_dirs {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

fn write_text_cli(text: &str, primary: bool) -> bool {
    let candidates = tool_candidates(true);
    let Some(bin) = find_cmd(&candidates) else {
        return false;
    };
    let bin_name = bin.file_name().and_then(|f| f.to_str()).unwrap_or("");
    let mut c = Command::new(&bin);
    c.stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null());

    if bin_name == "wl-copy" {
        if primary {
            c.arg("--primary");
        }
    } else if bin_name == "xclip" {
        c.arg("-selection").arg(if primary { "primary" } else { "clipboard" });
    } else if bin_name == "xsel" {
        if primary {
            c.arg("--primary").arg("--input");
        } else {
            c.arg("--clipboard").arg("--input");
        }
    }

    let Ok(mut child) = c.spawn() else {
        return false;
    };
    let text = text.to_string();
    std::thread::spawn(move || {
        if let Some(mut si) = child.stdin.take() {
            use std::io::Write;
            let _ = si.write_all(text.as_bytes());
            let _ = si.flush();
            drop(si);
        }
        let _ = child.wait();
    });
    true
}

fn read_text_cli(primary: bool) -> Option<String> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        for name in tool_candidates(false) {
            let Some(bin) = find_cmd(&[name]) else { continue };
            let mut c = Command::new(&bin);
            if name == "wl-paste" {
                if primary {
                    c.arg("--primary");
                }
                c.arg("--no-newline");
            } else if name == "xclip" {
                c.arg("-selection").arg(if primary { "primary" } else { "clipboard" }).arg("-o");
            } else if name == "xsel" {
                if primary {
                    c.arg("--primary").arg("--output");
                } else {
                    c.arg("--clipboard").arg("--output");
                }
            }
            let Ok(out) = c.output() else { continue };
            if out.status.success() && !out.stdout.is_empty() {
                let _ = tx.send(Some(String::from_utf8_lossy(&out.stdout).into_owned()));
                return;
            }
        }
        let _ = tx.send(None);
    });
    match rx.recv_timeout(Duration::from_millis(500)) {
        Ok(v) => v,
        Err(_) => None,
    }
}



