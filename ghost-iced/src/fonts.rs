//! Font stack for the iced front-end: bundled primary face + system fallbacks.
//!
//! iced builds its font system from an *empty* database, so system families are
//! discovered with `fc-list` and scored into a preferred fallback order.

use std::collections::HashSet;
use std::fs;
use std::process::Command;

/// The face the cell metrics are measured from and the primary text font.
pub const PRIMARY_BYTES: &[u8] = include_bytes!("../../assets/JetBrainsMono-Medium.ttf");

/// Returns the family name that resolves to [`PRIMARY_BYTES`] once it is registered.
///
/// iced/cosmic_text matches the typographic family (nameID 16, "JetBrains Mono"),
/// not the file's nameID 1 ("JetBrains Mono Medium").
pub fn primary() -> iced::Font {
    iced::Font::with_name("JetBrains Mono")
}

/// Caps one file's size, bounding memory and parse time. A large variable or
/// collection font still passes it: iced/cosmic_text rasterises collections, so
/// CJK stays renderable at the cost of one face per collection.
const MAX_FONT_BYTES: u64 = 32 * 1024 * 1024;

/// `fc-list` locations, in probe order: `PATH` first, then the NixOS profile
/// paths.
const FC_LIST_BINARIES: [&str; 3] = [
    "fc-list",
    "/run/current-system/sw/bin/fc-list",
    "/usr/bin/fc-list",
];

/// Registered separately, so it must not also be loaded as a fallback.
const PRIMARY_FAMILY: &str = "jetbrains mono";

/// Loads at most `limit` system fallback faces, highest priority first.
///
/// De-duplicated by family (all `JetBrainsMono*` designs collapse into one slot);
/// files that cannot be read, are larger than 32 MiB, or are not a TTF/OTF/TTC
/// are skipped. Logs one summary line to stderr.
pub fn fallback_bytes(limit: usize) -> Vec<(String, Vec<u8>)> {
    let mut out: Vec<(String, Vec<u8>)> = Vec::new();
    if limit == 0 {
        eprintln!("[ghost-iced] fallback fonts: 0");
        return out;
    }

    let mut candidates: Vec<Candidate> = Vec::new();
    let mut seen_paths: HashSet<String> = HashSet::new();
    for (order, (path, family)) in discover_fonts().into_iter().enumerate() {
        let score = score_family(&family);
        if score <= 0 {
            continue;
        }
        if family.to_lowercase() == PRIMARY_FAMILY {
            continue;
        }
        let path_l = path.to_lowercase();
        if path_l.contains("italic") || path_l.contains("oblique") {
            continue;
        }
        if !seen_paths.insert(path.clone()) {
            continue;
        }
        candidates.push(Candidate {
            score,
            order,
            family,
            path,
        });
    }

    candidates.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| is_collection(&a.path).cmp(&is_collection(&b.path)))
            .then_with(|| regularish(&b.path).cmp(&regularish(&a.path)))
            .then_with(|| a.order.cmp(&b.order))
    });

    let mut picked: Vec<&Candidate> = Vec::new();
    let mut picked_keys: HashSet<String> = HashSet::new();
    let mut picked_paths: HashSet<&str> = HashSet::new();
    for c in &candidates {
        if !picked_keys.insert(family_key(&c.family)) {
            continue;
        }
        if !picked_paths.insert(c.path.as_str()) {
            continue;
        }
        picked.push(c);
    }

    for c in picked {
        if out.len() >= limit {
            break;
        }
        let bytes = match load_font_bytes(&c.path) {
            Some(bytes) => bytes,
            None => continue,
        };
        // The same file can appear under several fc-list paths, and a
        // byte-identical face costs ~30 MiB per duplicate collection while
        // adding nothing.
        if out.iter().any(|(_, seen): &(String, Vec<u8>)| seen == &bytes) {
            continue;
        }
        out.push((c.family.clone(), bytes));
    }

    let names: Vec<&str> = out.iter().map(|(f, _)| f.as_str()).collect();
    eprintln!(
        "[ghost-iced] fallback fonts: {} ({})",
        out.len(),
        names.join(", ")
    );
    out
}

struct Candidate {
    score: i32,
    /// Position in `fc-list` output; the final tie-break so ordering is stable.
    order: usize,
    family: String,
    path: String,
}

/// Priority score for a font family (higher = preferred fallback).
///
/// `<= 0` means "do not load": colour bitmap fonts (CBDT/CBLC) score -1000,
/// because the renderer cannot rasterise them.
fn score_family(family: &str) -> i32 {
    let f = family.to_lowercase();
    let has = |s: &str| f.contains(s);
    if has("color") {
        return -1000;
    }
    if f.starts_with("jetbrainsmono nerd font mono") {
        110 // same design as primary + nerd glyphs: identical metrics
    } else if has("jetbrainsmono") && has("nerd") && has("mono") {
        105
    } else if has("dejavu sans mono") {
        100
    } else if has("noto sans mono") {
        90
    } else if has("liberation mono") {
        85
    } else if has("noto sans symbols 2") {
        80
    } else if has("noto sans symbols") {
        75
    } else if has("symbola") {
        70
    } else if has("noto emoji") {
        65
    } else if has("mono") {
        50
    } else {
        10
    }
}

/// Every `JetBrainsMono*` design collapses into one slot.
fn family_key(family: &str) -> String {
    let l = family.to_lowercase();
    if l.contains("jetbrainsmono") {
        "jetbrainsmono".to_string()
    } else {
        l
    }
}

/// The "normal" weight files, preferred on score ties.
fn regularish(path: &str) -> bool {
    let l = path.to_lowercase();
    l.contains("regular") || l.contains("-medium") || l.contains("book")
}

/// `.ttc` files lose score ties to single-face ones, keeping the cheap faces
/// ahead of the multi-face monsters.
fn is_collection(path: &str) -> bool {
    path.to_lowercase().ends_with(".ttc")
}

/// `None` unless it is a plausible TTF/OTF/TTC and within the size cap.
fn load_font_bytes(path: &str) -> Option<Vec<u8>> {
    let meta = fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > MAX_FONT_BYTES {
        return None;
    }
    let bytes = fs::read(path).ok()?;
    if !is_font_bytes(&bytes) {
        return None;
    }
    Some(bytes)
}

/// TrueType/OpenType outline magic, CFF outlines (`OTTO`), or a TrueType
/// collection (`ttcf`).
fn is_font_bytes(bytes: &[u8]) -> bool {
    match bytes.get(..4) {
        Some([0x00, 0x01, 0x00, 0x00]) => true,
        Some(head) => head == b"OTTO" || head == b"ttcf",
        None => false,
    }
}

fn discover_fonts() -> Vec<(String, String)> {
    for bin in FC_LIST_BINARIES {
        match Command::new(bin).output() {
            Ok(o) if o.status.success() => {
                return parse_fc_list(&String::from_utf8_lossy(&o.stdout));
            }
            _ => continue,
        }
    }
    Vec::new()
}

/// The default `fc-list` format is `"/path/font.ttf: Family:style=Regular"`,
/// where the family field may be a comma-separated list (`"Family,Family
/// Variant"`) and the style field may be absent (`"/path/font.ttf: Family"`).
fn parse_fc_list(text: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for line in text.lines() {
        let Some((path, rest)) = line.split_once(':') else {
            continue;
        };
        let path = path.trim();
        let rest = rest.split_once(":style=").map(|(f, _)| f).unwrap_or(rest);
        // A line may also carry a bare `": <style>"` suffix.
        let family = rest.split(':').next().unwrap_or("").trim();
        let family = normalise_family(family.split(',').next().unwrap_or(""));
        if path.is_empty() || family.is_empty() {
            continue;
        }
        if !seen.insert(path.to_string()) {
            continue; // fc-list repeats a file once per style
        }
        out.push((path.to_string(), family));
    }
    out
}

/// Trim and collapse internal whitespace, so `"Noto   Sans Mono "` matches the
/// families the scoring table looks for.
fn normalise_family(family: &str) -> String {
    family.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_fc_list_styles_and_family_lists() {
        let out = parse_fc_list(
            "/x/NotoSansMono.ttf: Noto Sans Mono:style=Bold\n\
             /x/JetBrainsMonoNLNerdFontMono-Light.ttf: JetBrainsMonoNL Nerd Font Mono,JetBrainsMonoNL NFM,JetBrainsMonoNL NFM Light:style=Light,Regular\n\
             /x/Plain.ttf: Plain Family\n\
             : no path\n\
             /x/NotoSansMono.ttf: Noto Sans Mono:style=Regular\n",
        );
        assert_eq!(
            out,
            vec![
                (
                    "/x/NotoSansMono.ttf".to_string(),
                    "Noto Sans Mono".to_string()
                ),
                (
                    "/x/JetBrainsMonoNLNerdFontMono-Light.ttf".to_string(),
                    "JetBrainsMonoNL Nerd Font Mono".to_string()
                ),
                ("/x/Plain.ttf".to_string(), "Plain Family".to_string()),
            ]
        );
    }

    #[test]
    fn scores_match_the_expected_priority() {
        assert_eq!(score_family("JetBrainsMono Nerd Font Mono"), 110);
        assert_eq!(score_family("JetBrainsMonoNL Nerd Font Mono"), 105);
        assert_eq!(score_family("DejaVu Sans Mono"), 100);
        assert_eq!(score_family("Noto Sans Mono"), 90);
        assert_eq!(score_family("Liberation Mono"), 85);
        assert_eq!(score_family("Noto Sans Symbols 2"), 80);
        assert_eq!(score_family("Noto Sans Symbols"), 75);
        assert_eq!(score_family("Symbola"), 70);
        assert_eq!(score_family("Noto Emoji"), 65);
        assert_eq!(score_family("Fira Code"), 10);
        assert!(score_family("Noto Color Emoji") <= 0);
    }

    #[test]
    fn font_magic_is_checked() {
        assert!(is_font_bytes(&[0x00, 0x01, 0x00, 0x00, 0x00]));
        assert!(is_font_bytes(b"OTTO____"));
        assert!(is_font_bytes(b"ttcf____"));
        assert!(!is_font_bytes(b"wOFF____"));
        assert!(!is_font_bytes(b"ttf"));
        assert!(!is_font_bytes(&[]));
    }
}
