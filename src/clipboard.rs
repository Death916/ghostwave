//! Clipboard access via wl-copy/wl-paste or xclip/xsel (no extra deps).
//!
//! IMPORTANT: never wait synchronously on these tools from the main thread.
//! wl-paste blocks indefinitely when the clipboard has no owner, and wl-copy
//! stays alive as the clipboard-owner daemon; a main-thread wait freezes the
//! whole app (observed: Ctrl+Shift+C froze ghostwave). Children are reaped on
//! background threads and paste reads carry a timeout.

use std::process::{Command, Stdio};
use std::sync::mpsc::{channel, RecvTimeoutError};
use std::time::Duration;

fn copy_cmd() -> Option<&'static str> {
    for c in ["wl-copy", "xclip", "xsel"] {
        if std::env::var_os("PATH")
            .map(|p| p.to_string_lossy().split(':').any(|d| {
                std::path::Path::new(d).join(c).exists()
                    || std::path::Path::new(d).join(format!("{c}.exe")).exists()
            }))
            .unwrap_or(false)
        {
            return Some(c);
        }
    }
    None
}

pub fn write_text(text: &str) -> bool {
    let Some(cmd) = copy_cmd() else { return false };
    let mut c = Command::new(cmd);
    c.stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null());
    if cmd == "xclip" {
        c.arg("-selection").arg("clipboard");
    }
    let Ok(mut child) = c.spawn() else { return false };
    // Feed the payload from a background thread (closing stdin finalizes the
    // copy), and reap the child there too — never on the render loop.
    if let Some(mut si) = child.stdin.take() {
        let text = text.to_string();
        std::thread::spawn(move || {
            let _ = std::io::Write::write_all(&mut si, text.as_bytes());
        });
    }
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    true
}

fn read_text_blocking() -> Option<String> {
    for cmd in ["wl-paste", "xclip", "xsel"] {
        let mut c = Command::new(cmd);
        if cmd == "xclip" {
            c.arg("-selection").arg("clipboard").arg("-o");
        } else if cmd == "xsel" {
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
    match rx.recv_timeout(Duration::from_secs(1)) {
        Ok(v) => v,
        Err(RecvTimeoutError::Timeout) => {
            eprintln!("ghostwave: clipboard paste timed out (wl-paste has no data)");
            None
        }
        Err(_) => None,
    }
}
