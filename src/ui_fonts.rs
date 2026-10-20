//! Font stack for the iced front-end: bundled primary face + system fallbacks.
//!
//! iced builds its font system from an *empty* database, so system families are
//! discovered with `fc-list` and scored into a preferred fallback order.

use std::collections::HashSet;
use std::fs;
use std::process::Command;
use std::sync::OnceLock;

/// The face the cell metrics are measured from and the primary text font.
pub const PRIMARY_BYTES: &[u8] = include_bytes!("../assets/JetBrainsMono-Medium.ttf");

static ACTIVE_FAMILY: OnceLock<String> = OnceLock::new();
static ACTIVE_BYTES: OnceLock<Option<&'static [u8]>> = OnceLock::new();

/// Initializes the primary font family and bytes from user config if provided.
/// Falls back to bundled JetBrains Mono if omitted or not found.
pub fn init(preferred_family: Option<&str>) {
    if let Some(target) = preferred_family {
        let target_clean = target.trim();
        if !target_clean.is_empty() {
            if let Some((family, bytes)) = resolve_family(target_clean) {
                eprintln!("{}", custom_font_message(&family));
                let leaked_bytes: &'static [u8] = Box::leak(bytes.into_boxed_slice());
                let _ = ACTIVE_FAMILY.set(family);
                let _ = ACTIVE_BYTES.set(Some(leaked_bytes));
                return;
            } else {
                eprintln!(
                    "[ghostwave] font '{}' not found on system; falling back to bundled JetBrains Mono",
                    loggable(target_clean)
                );
            }
        }
    }
    let _ = ACTIVE_FAMILY.set("JetBrains Mono".to_string());
    let _ = ACTIVE_BYTES.set(None);
}

/// The family the cells are measured from and the primary text is drawn in —
/// i.e. the answer to a font query.
///
/// Falls back to the bundled face's family when [`init`] has not run or found
/// no configured family on the system.
pub fn active_family() -> &'static str {
    ACTIVE_FAMILY.get().map(|s| s.as_str()).unwrap_or("JetBrains Mono")
}

/// Returns the active primary font for iced canvas and widget text.
pub fn primary() -> iced::Font {
    let family = ACTIVE_FAMILY.get().map(|s| s.as_str()).unwrap_or("JetBrains Mono");
    if family == "JetBrains Mono" {
        iced::Font {
            family: iced::font::Family::Name("JetBrains Mono"),
            weight: iced::font::Weight::Medium,
            stretch: iced::font::Stretch::Normal,
            style: iced::font::Style::Normal,
        }
    } else {
        iced::Font {
            family: iced::font::Family::Name(Box::leak(family.to_string().into_boxed_str())),
            weight: iced::font::Weight::Normal,
            stretch: iced::font::Stretch::Normal,
            style: iced::font::Style::Normal,
        }
    }
}

/// Returns the bytes of the primary font for cell dimension calculation.
pub fn primary_bytes() -> &'static [u8] {
    ACTIVE_BYTES.get().copied().flatten().unwrap_or(PRIMARY_BYTES)
}

/// Returns custom font bytes if a non-bundled font was loaded, so iced can register it.
pub fn custom_bytes() -> Option<&'static [u8]> {
    ACTIVE_BYTES.get().copied().flatten()
}

/// Resolves a font family name against `fc-list` output, matching case-insensitively.
pub fn resolve_family(target: &str) -> Option<(String, Vec<u8>)> {
    let target_lower = target.to_lowercase();
    let fonts = discover_fonts();
    // Try exact case-insensitive match first
    for (path, family) in &fonts {
        if family.to_lowercase() == target_lower {
            if let Some(bytes) = load_font_bytes(path) {
                return Some((family.clone(), bytes));
            }
        }
    }
    // Try substring/prefix match second
    for (path, family) in &fonts {
        let fam_lower = family.to_lowercase();
        if fam_lower.starts_with(&target_lower) || fam_lower.contains(&target_lower) {
            if let Some(bytes) = load_font_bytes(path) {
                return Some((family.clone(), bytes));
            }
        }
    }
    None
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
        eprintln!("[ghostwave] fallback fonts: 0");
        return out;
    }

    let mut candidates: Vec<Candidate> = Vec::new();
    let mut seen_paths: HashSet<String> = HashSet::new();
    for (order, (path, family)) in discover_fonts().into_iter().enumerate() {
        let score = score_family(&family);
        if score <= 0 {
            continue;
        }
        let fam_lower = family.to_lowercase();
        if fam_lower == PRIMARY_FAMILY || ACTIVE_FAMILY.get().is_some_and(|a| a.to_lowercase() == fam_lower) {
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
        if out.iter().any(|(_, seen): &(String, Vec<u8>)| seen == &bytes) {
            continue;
        }
        out.push((c.family.clone(), bytes));
    }

    let names: Vec<&str> = out.iter().map(|(f, _)| f.as_str()).collect();
    eprintln!("[ghostwave] fallback fonts: {}", fallback_summary(&names));
    out
}

/// Renders untrusted text safe for one terminal line.
///
/// Every Unicode control character — C0 (including ESC and BEL), DEL and the C1
/// block U+0080–U+009F — and every bidirectional formatting control (U+061C,
/// U+200E–U+200F, U+202A–U+202E, U+2066–U+2069) is replaced by its `\u{...}`
/// escape, so Fontconfig metadata cannot inject terminal escape sequences into
/// the inherited stderr, nor reorder the diagnostic to spoof its own family and
/// list boundaries in a bidi-capable terminal. Printable text passes through
/// unchanged, so the diagnostic stays readable.
fn loggable(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        if ch.is_control() || is_bidi_control(ch) {
            out.push_str(&format!("\\u{{{:x}}}", ch as u32));
        } else {
            out.push(ch);
        }
    }
    out
}

/// Whether `ch` is a Unicode `Bidi_Control` property character.
///
/// These are category `Cf`, not `Cc`, so [`char::is_control`] misses them, yet
/// they are invisible and can visually reorder a line in a bidi-capable
/// terminal — including the overrides U+202D/U+202E and the isolates
/// U+2066–U+2069. They must be escaped like any other control.
fn is_bidi_control(ch: char) -> bool {
    matches!(
        ch,
        '\u{061c}'                  // ARABIC LETTER MARK
            | '\u{200e}'            // LEFT-TO-RIGHT MARK
            | '\u{200f}'            // RIGHT-TO-LEFT MARK
            | '\u{202a}'..='\u{202e}' // LRE, RLE, PDF, LRO, RLO
            | '\u{2066}'..='\u{2069}' // LRI, RLI, FSI, PDI
    )
}

/// The fallback summary line, e.g. `3 (DejaVu Sans Mono, Noto Sans Symbols 2)`.
///
/// Family names come straight from `fc-list`, so they go through [`loggable`].
fn fallback_summary(names: &[&str]) -> String {
    let escaped: Vec<String> = names.iter().map(|n| loggable(n)).collect();
    format!("{} ({})", names.len(), escaped.join(", "))
}

/// The diagnostic logged when a configured family resolves to a system face.
///
/// The family is Fontconfig metadata, so it goes through [`loggable`].
fn custom_font_message(family: &str) -> String {
    format!("[ghostwave] custom font configured: {}", loggable(family))
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
        110
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
    } else if has("mono")
        || has("code")
        || has("iosevka")
        || has("hack")
        || has("inconsolata")
        || has("courier")
        || has("consolas")
    {
        50
    } else {
        0
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
        let family = rest.split(':').next().unwrap_or("").trim();
        let family = normalise_family(family.split(',').next().unwrap_or(""));
        if path.is_empty() || family.is_empty() {
            continue;
        }
        if !seen.insert(path.to_string()) {
            continue;
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
        assert_eq!(score_family("Fira Code"), 50);
        assert_eq!(score_family("Fira Sans"), 0);
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

    /// ESC, BEL, DEL and C1 must not survive into a log line; plain text must.
    #[test]
    fn loggable_escapes_control_bytes() {
        let hostile = "Mono\u{1b}]0;pwned\u{7}\u{9b}2J\u{7f}\u{0}";
        assert_eq!(
            loggable(hostile),
            "Mono\\u{1b}]0;pwned\\u{7}\\u{9b}2J\\u{7f}\\u{0}"
        );
        assert_eq!(loggable("DejaVu Sans Mono"), "DejaVu Sans Mono");
    }

    /// Bidi formatting controls are `Cf`, so `char::is_control` misses them and
    /// they would otherwise reorder the line in a bidi-capable terminal.
    #[test]
    fn loggable_escapes_bidi_controls() {
        let hostile = "Mono\u{202e}emans\u{202d}\u{202c}\u{2066}\u{2069}\u{200f}\u{200e}\u{061c}";
        assert_eq!(
            loggable(hostile),
            "Mono\\u{202e}emans\\u{202d}\\u{202c}\\u{2066}\\u{2069}\\u{200f}\\u{200e}\\u{61c}"
        );
        assert_no_hostile_controls(&loggable(hostile));
        assert_eq!(loggable("Noto Sans Mono"), "Noto Sans Mono");
        assert_eq!(loggable("JetBrains Mono"), "JetBrains Mono");
    }

    /// The whole `fc-list` → scoring → summary path stays inert for hostile
    /// family metadata, while the selected name is still recognisable.
    #[test]
    fn fallback_summary_is_terminal_safe() {
        let parsed = parse_fc_list(
            "/x/evil.ttf: Mono\u{1b}[31m\u{7} Pwn Mono:style=Regular\n\
             /x/DejaVuSansMono.ttf: DejaVu Sans Mono:style=Regular\n",
        );
        let names: Vec<&str> = parsed.iter().map(|(_, f)| f.as_str()).collect();
        assert!(
            names.iter().any(|n| score_family(n) == 50),
            "hostile family must still score as a selectable fallback: {names:?}"
        );

        let summary = fallback_summary(&names);
        assert!(
            !summary.chars().any(char::is_control),
            "summary leaked a control character: {summary:?}"
        );
        assert!(summary.starts_with("2 ("), "{summary:?}");
        assert!(summary.contains("\\u{1b}[31m\\u{7}"), "{summary:?}");
        assert!(summary.contains("DejaVu Sans Mono"), "{summary:?}");
    }

    /// Bidi controls in an `fc-list` family reach neither `stderr` raw via the
    /// fallback summary nor move the neighbouring printable text.
    #[test]
    fn fallback_summary_escapes_bidi_controls() {
        let parsed = parse_fc_list(
            "/x/evil.ttf: Mono\u{202e}Pwn\u{2066}\u{2069} Mono:style=Regular\n\
             /x/DejaVuSansMono.ttf: DejaVu Sans Mono:style=Regular\n",
        );
        let names: Vec<&str> = parsed.iter().map(|(_, f)| f.as_str()).collect();
        assert_eq!(names.len(), 2, "{names:?}");

        let summary = fallback_summary(&names);
        assert_no_hostile_controls(&summary);
        assert!(summary.contains("\\u{202e}"), "{summary:?}");
        assert!(summary.contains("\\u{2066}\\u{2069}"), "{summary:?}");
        assert!(summary.contains("DejaVu Sans Mono"), "{summary:?}");
        let tail = summary.rsplit(", ").next().unwrap();
        assert_eq!(
            tail.trim_end_matches(')'),
            "DejaVu Sans Mono",
            "{summary:?}"
        );
    }

    /// The custom-font diagnostic takes the same treatment.
    #[test]
    fn custom_font_message_is_terminal_safe() {
        let msg = custom_font_message("Evil\u{1b}[2J\u{7} Mono");
        assert_eq!(
            msg,
            "[ghostwave] custom font configured: Evil\\u{1b}[2J\\u{7} Mono"
        );
        assert!(!msg.chars().any(char::is_control), "{msg:?}");
        assert_eq!(
            custom_font_message("DejaVu Sans Mono"),
            "[ghostwave] custom font configured: DejaVu Sans Mono"
        );
    }

    /// A configured family carrying a right-to-left override must be escaped in
    /// the same diagnostic, with ordinary family text left untouched.
    #[test]
    fn custom_font_message_escapes_bidi_controls() {
        let msg = custom_font_message("\u{202e}Mono\u{2067}\u{61c}");
        assert_eq!(
            msg,
            "[ghostwave] custom font configured: \\u{202e}Mono\\u{2067}\\u{61c}"
        );
        assert_no_hostile_controls(&msg);
        assert_eq!(
            custom_font_message("Noto Sans Mono"),
            "[ghostwave] custom font configured: Noto Sans Mono"
        );
    }

    /// Asserts that no raw Cc, DEL/C1 or bidi formatting control survived.
    fn assert_no_hostile_controls(text: &str) {
        let raw: Vec<char> = text
            .chars()
            .filter(|c| c.is_control() || is_bidi_control(*c))
            .collect();
        assert!(
            raw.is_empty(),
            "raw control(s) {raw:?} survived in {text:?}"
        );
    }
}
