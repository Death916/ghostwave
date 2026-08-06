//! Clipboard access via wl-copy/wl-paste or xclip/xsel (no extra deps).

use std::process::{Command, Stdio};

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
    let _ = child
        .stdin
        .take()
        .map(|mut si| std::io::Write::write_all(&mut si, text.as_bytes()));
    let _ = child.wait();
    true
}

pub fn read_text() -> Option<String> {
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
