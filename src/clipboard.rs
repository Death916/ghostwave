//! Clipboard access via wl-copy/wl-paste or xclip/xsel (no extra deps).
//!
//! IMPORTANT: never wait synchronously on these tools from the main thread.
//! wl-paste blocks indefinitely when the clipboard has no owner, and wl-copy
//! stays alive as the clipboard-owner daemon; a main-thread wait freezes the
//! whole app. Children are reaped on background threads and paste reads carry a timeout.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc::{channel, RecvTimeoutError};
use std::time::Duration;

fn find_cmd(names: &[&'static str]) -> Option<PathBuf> {
    let mut search_dirs = Vec::new();
    if let Some(path) = std::env::var_os("PATH") {
        for d in std::env::split_paths(&path) {
            search_dirs.push(d);
        }
    }
    // Fallback standard Unix & Linux system directories if PATH is minimal
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

pub fn write_text(text: &str) -> bool {
    let Some(bin) = find_cmd(&["wl-copy", "xclip", "xsel"]) else {
        return false;
    };
    let is_xclip = bin.file_name().map(|f| f == "xclip").unwrap_or(false);
    let mut c = Command::new(&bin);
    c.stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null());
    if is_xclip {
        c.arg("-selection").arg("clipboard");
    }
    let Ok(mut child) = c.spawn() else {
        return false;
    };
    let text = text.to_string();
    // Feed the payload and immediately close stdin so wl-copy/xclip
    // receives EOF and registers the payload with the clipboard manager.
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

fn read_text_blocking() -> Option<String> {
    for name in ["wl-paste", "xclip", "xsel"] {
        let Some(bin) = find_cmd(&[name]) else { continue };
        let mut c = Command::new(&bin);
        if name == "xclip" {
            c.arg("-selection").arg("clipboard").arg("-o");
        } else if name == "xsel" {
            c.arg("--clipboard").arg("--output");
        }
        let out = c.output().ok()?;
        if out.status.success() && !out.stdout.is_empty() {
            return Some(String::from_utf8_lossy(&out.stdout).into_owned());
        }
    }
    None
}

pub fn read_text() -> Option<String> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let _ = tx.send(read_text_blocking());
    });
    match rx.recv_timeout(Duration::from_millis(500)) {
        Ok(v) => v,
        Err(RecvTimeoutError::Timeout) => None,
        Err(_) => None,
    }
}

fn read_primary_text_blocking() -> Option<String> {
    for name in ["wl-paste", "xclip", "xsel"] {
        let Some(bin) = find_cmd(&[name]) else { continue };
        let mut c = Command::new(&bin);
        if name == "wl-paste" {
            c.arg("--primary");
        } else if name == "xclip" {
            c.arg("-selection").arg("primary").arg("-o");
        } else if name == "xsel" {
            c.arg("--primary").arg("--output");
        }
        let out = c.output().ok()?;
        if out.status.success() && !out.stdout.is_empty() {
            return Some(String::from_utf8_lossy(&out.stdout).into_owned());
        }
    }
    None
}

pub fn read_primary_text() -> Option<String> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let _ = tx.send(read_primary_text_blocking());
    });
    match rx.recv_timeout(Duration::from_millis(500)) {
        Ok(v) => v,
        Err(RecvTimeoutError::Timeout) => None,
        Err(_) => None,
    }
}

