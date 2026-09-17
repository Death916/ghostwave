//! Font stack for the iced front-end: bundled primary face + system fallbacks.
//!
//! iced builds its `cosmic_text::FontSystem` from an *empty* font database plus
//! its own Fira Sans (`iced_graphics/src/text.rs`), so without help it knows no
//! system fonts and no fallback chain: box-drawing, braille, nerd-font and
//! emoji glyphs all come out as tofu. The macroquad front-end (src/fonts.rs in
//! the app crate) solves that by discovering system fonts with `fc-list` and
//! scoring families into a preferred order; this module reproduces that
//! discovery and priority so `main.rs` can hand the bytes to iced:
//!
//! ```ignore
//! let mut app = iced::application(..).font(fonts::PRIMARY_BYTES);
//! for (family, bytes) in fonts::fallback_bytes(12) {
//!     eprintln!("[ghost-iced] font fallback {family}");
//!     app = app.font(bytes); // repeatable: one call per font
//! }
//! ```
//!
//! Discovery is `fc-list` only (authoritative on NixOS, where font files live
//! in `/nix/store` paths fontconfig knows). Colour bitmap fonts (CBDT/CBLC) are
//! deliberately dropped: the renderer cannot rasterise them.
//!
//! Pure `std` on purpose (`std::process::Command`, `std::fs`, `std::collections`):
//! no fontconfig bindings, no parser crates, no extra build dependencies.

use std::collections::HashSet;
use std::fs;
use std::process::Command;

/// The face the cell metrics are measured from and the primary text font.
pub const PRIMARY_BYTES: &[u8] = include_bytes!("../../assets/JetBrainsMono-Medium.ttf");

/// Family name that resolves to [`PRIMARY_BYTES`] once it is registered.
/// (Its nameID 1 is "JetBrains Mono Medium"; nameID 16 is "JetBrains Mono".
/// The typographic family is what iced/cosmic_text resolves.)
pub fn primary() -> iced::Font {
    iced::Font::with_name("JetBrains Mono")
}

/// Skip enormous variable/collection fonts: memory + parse time. This box's
/// `NotoSansMonoCJK-VF.otf.ttc` is 30.4 MiB and *does* pass — iced/cosmic_text
/// can rasterise collections (unlike the macroquad stack), so CJK stays
/// renderable at the cost of one ~30 MiB face per registered collection.
const MAX_FONT_BYTES: u64 = 32 * 1024 * 1024;

/// `fc-list` locations, in probe order (PATH first, then the NixOS profile
/// paths, which is where it actually lives on this box).
const FC_LIST_BINARIES: [&str; 3] = [
    "fc-list",
    "/run/current-system/sw/bin/fc-list",
    "/usr/bin/fc-list",
];

/// Family name of the bundled primary face. It is registered separately, so it
/// must not also be loaded as a fallback.
const PRIMARY_FAMILY: &str = "jetbrains mono";

/// Fallback faces, in priority order, as `(family_name, bytes)`.
///
/// Returns at most `limit` faces, highest priority first, de-duplicated by
/// family (all `JetBrainsMono*` designs collapse into one slot). Files that
/// cannot be read, are larger than 32 MiB, or are not a TTF/OTF/TTC are
/// skipped. Logs one summary line to stderr.
pub fn fallback_bytes(limit: usize) -> Vec<(String, Vec<u8>)> {
    let mut out: Vec<(String, Vec<u8>)> = Vec::new();
    if limit == 0 {
        eprintln!("[ghost-iced] fallback fonts: 0");
        return out;
    }

    // 1) Discover (path, family) pairs, then score + order them.
    let mut candidates: Vec<Candidate> = Vec::new();
    let mut seen_paths: HashSet<String> = HashSet::new();
    for (order, (path, family)) in discover_fonts().into_iter().enumerate() {
        let score = score_family(&family);
        if score <= 0 {
            continue; // "color": bitmap-only, unrenderable
        }
        if family.to_lowercase() == PRIMARY_FAMILY {
            continue; // already registered as `primary()`
        }
        let path_l = path.to_lowercase();
        if path_l.contains("italic") || path_l.contains("oblique") {
            continue; // no style synthesis for fallbacks
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

    // Highest score first; on ties prefer a single-face `.ttf`/`.otf` over a
    // font collection, then a Regular/Medium/Book file, then the first
    // `fc-list` entry. Mirrors `score_family` + the app's tie-breaking.
    candidates.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| is_collection(&a.path).cmp(&is_collection(&b.path)))
            .then_with(|| regularish(&b.path).cmp(&regularish(&a.path)))
            .then_with(|| a.order.cmp(&b.order))
    });

    // 2) Collapse one file per family ("the nerd-font package ships ~18 weights
    // across 6 family NAMES"), keeping the highest-ranked one.
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

    // 3) Read bytes for the first `limit` that survive the sanity checks.
    for c in picked {
        if out.len() >= limit {
            break;
        }
        let bytes = match load_font_bytes(&c.path) {
            Some(bytes) => bytes,
            None => continue,
        };
        // The same file shows up under several fc-list paths (the home-manager
        // profile tree duplicates whole store paths), and registering a
        // byte-identical face twice costs ~30 MiB per duplicate CJK collection
        // while adding nothing: fontdb reads every face in a collection, so one
        // copy already resolves all of its family names.
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

/// A scored font candidate: one file, one family.
struct Candidate {
    score: i32,
    /// Position in `fc-list` output; the final tie-break so ordering is stable.
    order: usize,
    family: String,
    path: String,
}

/// Priority score for a font family (higher = preferred fallback).
/// `<= 0` means "do not load". Same table as the macroquad front-end.
fn score_family(family: &str) -> i32 {
    let f = family.to_lowercase();
    let has = |s: &str| f.contains(s);
    if has("color") {
        return -1000; // CBDT bitmap color fonts: cannot be rasterised
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

/// Grouping key: every `JetBrainsMono*` design collapses into one slot, the way
/// the macroquad stack does — the nerd-font package ships ~18 weights across 6
/// family names and loading them all would starve the symbol/emoji fonts.
fn family_key(family: &str) -> String {
    let l = family.to_lowercase();
    if l.contains("jetbrainsmono") {
        "jetbrainsmono".to_string()
    } else {
        l
    }
}

/// True for the "normal" weight files, preferred on score ties.
fn regularish(path: &str) -> bool {
    let l = path.to_lowercase();
    l.contains("regular") || l.contains("-medium") || l.contains("book")
}

/// True for `TrueTypeCollection` files (`.ttc`). Losing a score tie against a
/// single-face file keeps the cheap faces ahead of the multi-face monsters.
fn is_collection(path: &str) -> bool {
    path.to_lowercase().ends_with(".ttc")
}

/// Read a font file, returning `None` unless it is a plausible TTF/OTF/TTC and
/// within the size cap.
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

/// Cheap sanity check: TrueType/OpenType outline magic, CFF outlines (`OTTO`),
/// or a TrueType collection (`ttcf`). No parser, no dependency.
fn is_font_bytes(bytes: &[u8]) -> bool {
    match bytes.get(..4) {
        Some([0x00, 0x01, 0x00, 0x00]) => true,
        Some(head) => head == b"OTTO" || head == b"ttcf",
        None => false,
    }
}

/// Discover `(path, family)` pairs by running `fc-list`.
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

/// Parse `fc-list` output. The default format is
/// `"/path/font.ttf: Family:style=Regular"`, where the family field may be a
/// comma-separated list (`"Family,Family Variant"`) and the style field may be
/// absent (`"/path/font.ttf: Family"`).
fn parse_fc_list(text: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for line in text.lines() {
        let Some((path, rest)) = line.split_once(':') else {
            continue;
        };
        let path = path.trim();
        let rest = rest.split_once(":style=").map(|(f, _)| f).unwrap_or(rest);
        // Tolerate a bare `": <style>"` suffix as well.
        let family = rest.split(':').next().unwrap_or("").trim();
        let family = normalise_family(family.split(',').next().unwrap_or(""));
        if path.is_empty() || family.is_empty() {
            continue;
        }
        if !seen.insert(path.to_string()) {
            continue; // one entry per file: fc-list repeats it per style
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
    fn scores_match_the_macroquad_stack() {
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
