//! Font stack: bundled primary font + system-font fallback chain.
//!
//! macroquad rasterizes glyphs via fontdue but keeps the parsed font private,
//! so glyph coverage is checked here with our own fontdue copies of the same
//! bytes. Each cell picks the first font in the stack that has the character;
//! cells draw with that font (macroquad instance), glyph positions share the
//! same baseline so mixed-font lines stay aligned.
//!
//! Discovery: fc-list when available (authoritative on NixOS, where font
//! files live in /nix/store paths fontconfig knows), else walks the usual
//! font directories. Color bitmap fonts (CBDT/CBLC) are skipped — fontdue
//! cannot rasterize them and macroquad would render nothing.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use macroquad::text::{load_ttf_font_from_bytes, Font};

use crate::Result;

/// Loaded fallback fonts (max), beyond the bundled primary.
const MAX_FALLBACKS: usize = 8;
/// Skip enormous variable/collection fonts (memory + parse time).
const MAX_FONT_BYTES: u64 = 6 * 1024 * 1024;

struct Inner {
    /// fontdue coverage parsers, parallel to `fonts` in FontStack.
    coverage: Vec<fontdue::Font>,
    ascii_coverage: [u8; 128],
    /// char -> index into the stack (0 = primary). Computed lazily for non-ASCII.
    cache: Mutex<HashMap<char, usize>>,
}

/// Primary + fallback fonts. Cheap to clone (Font is an Arc, inner is an Arc).
#[derive(Clone)]
pub struct FontStack {
    fonts: Vec<Font>,
    inner: Arc<Inner>,
}

impl FontStack {
    /// Build the stack from the bundled primary font bytes (already loaded as
    /// `primary` for rendering) plus discovered system fallbacks.
    pub fn new(primary_bytes: &[u8], primary: Font) -> Result<Self> {
        let mut fonts = vec![primary];
        let mut coverage = vec![fontdue::Font::from_bytes(
            primary_bytes,
            fontdue::FontSettings::default(),
        )?];

        let mut candidates: Vec<(i32, String, PathBuf)> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        for (path, family) in discover_fonts() {
            let score = score_family(&family);
            if score <= 0 {
                continue;
            }
            let path_l = path.to_lowercase();
            if path_l.contains("italic") || path_l.contains("oblique") {
                continue; // no style synthesis for fallbacks
            }
            if !seen.insert(path.clone()) {
                continue;
            }
            candidates.push((score, family, PathBuf::from(path)));
        }
        // One font per family (the nerd-font package ships ~18 weights of the
        // same design across 6 family NAMES — "JetBrainsMono Nerd Font",
        // "JetBrainsMonoNL Nerd Font Mono", "…Propo", etc. — loading them all
        // wastes slots and starves symbol/emoji fonts like Noto Sans Symbols
        // 2, so all JetBrains variants collapse to one slot). Highest score
        // first; prefer Regular-ish files on ties.
        let mut by_family: HashMap<String, (i32, String, PathBuf)> = HashMap::new();
        let regularish = |p: &PathBuf| {
            let l = p.to_string_lossy().to_lowercase();
            l.contains("regular") || l.contains("-medium") || l.contains("book")
        };
        let family_key = |f: &str| {
            let l = f.to_lowercase();
            if l.contains("jetbrainsmono") {
                "jetbrainsmono".to_string()
            } else {
                l
            }
        };
        for c in candidates {
            let key = family_key(&c.1);
            let better = match by_family.get(&key) {
                Some(existing) => {
                    c.0 > existing.0
                        || (c.0 == existing.0 && regularish(&c.2) && !regularish(&existing.2))
                }
                None => true,
            };
            if better {
                by_family.insert(key, c);
            }
        }
        let mut candidates: Vec<(i32, String, PathBuf)> = by_family.into_values().collect();
        candidates.sort_by(|a, b| b.0.cmp(&a.0).then(a.2.cmp(&b.2)));

        let mut picked: Vec<(i32, String, PathBuf)> =
            candidates.iter().filter(|c| c.0 >= 50).take(MAX_FALLBACKS).cloned().collect();
        if picked.len() < 3 {
            for c in candidates.iter().filter(|c| c.0 < 50) {
                if picked.len() >= 3 {
                    break;
                }
                picked.push(c.clone());
            }
        }

        for (_, family, path) in picked {
            let bytes = match std::fs::read(&path) {
                Ok(b) if b.len() as u64 <= MAX_FONT_BYTES => b,
                _ => continue,
            };
            let mq = match load_ttf_font_from_bytes(&bytes) {
                Ok(f) => f,
                Err(_) => continue,
            };
            let fd = match fontdue::Font::from_bytes(bytes, fontdue::FontSettings::default()) {
                Ok(f) => f,
                Err(_) => continue,
            };
            eprintln!("ghostwave: font fallback {} ({family})", path.display());
            fonts.push(mq);
            coverage.push(fd);
        }

        let mut ascii_coverage = [0u8; 128];
        for b in 0..128u8 {
            let c = b as char;
            let idx = coverage.iter().position(|f| f.has_glyph(c)).unwrap_or(0);
            ascii_coverage[b as usize] = idx as u8;
        }

        Ok(Self {
            fonts,
            inner: Arc::new(Inner {
                coverage,
                ascii_coverage,
                cache: Mutex::new(HashMap::new()),
            }),
        })
    }

    /// Index of the first font in the stack that has `c` (0 = primary).
    /// Unknown characters fall back to the primary (renders as tofu, as
    /// before — no worse than today).
    #[inline]
    pub fn font_for(&self, c: char) -> usize {
        let cp = c as u32;
        if cp < 128 {
            return self.inner.ascii_coverage[cp as usize] as usize;
        }
        let mut cache = self.inner.cache.lock().unwrap();
        if let Some(&idx) = cache.get(&c) {
            return idx;
        }
        let idx = self
            .inner
            .coverage
            .iter()
            .position(|f| f.has_glyph(c))
            .unwrap_or(0);
        cache.insert(c, idx);
        idx
    }

    pub fn get(&self, idx: usize) -> &Font {
        &self.fonts[idx.min(self.fonts.len() - 1)]
    }

    /// Pre-warm font glyph caches by rasterizing printable ASCII into macroquad's atlas.
    pub fn warmup(&self, font_size: u16) {
        use macroquad::text::measure_text;
        let ascii_str: String = (0x20..=0x7Eu8).map(|b| b as char).collect();
        for font in &self.fonts {
            let _ = measure_text(&ascii_str, Some(font), font_size, 1.0);
        }
    }

    pub fn len(&self) -> usize {
        self.fonts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.fonts.is_empty()
    }

    /// (ascent, descent) of the primary font at `px` physical size.
    /// descent is returned positive. Falls back to rough 0.8/0.2 split.
    pub fn line_metrics(&self, px: f32) -> (f32, f32) {
        self.inner
            .coverage
            .first()
            .and_then(|f| f.horizontal_line_metrics(px))
            .map(|m| (m.ascent, -m.descent))
            .unwrap_or((px * 0.8, px * 0.2))
    }
}

/// Priority score for a font family (higher = preferred fallback).
/// <= 0 means "do not load".
fn score_family(family: &str) -> i32 {
    let f = family.to_lowercase();
    let has = |s: &str| f.contains(s);
    if has("color") {
        return -1000; // CBDT bitmap color fonts: fontdue can't rasterize
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

/// Discover (path, family) pairs from fc-list, falling back to walking
/// fontconfig's configured directories.
fn discover_fonts() -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    // 1) fc-list (authoritative; knows /nix/store font dirs on NixOS).
    let fc = ["fc-list", "/run/current-system/sw/bin/fc-list", "/usr/bin/fc-list"];
    for bin in fc {
        match std::process::Command::new(bin).output() {
            Ok(o) if o.status.success() => {
                let text = String::from_utf8_lossy(&o.stdout);
                for line in text.lines() {
                    // "/path/font.ttf: Family:style=Regular" (family may be
                    // comma-separated: "A,B:style=...").
                    let Some((path, rest)) = line.split_once(':') else {
                        continue;
                    };
                    let family = rest
                        .split_once(":style=")
                        .map(|(fam, _)| fam)
                        .unwrap_or(rest)
                        .split(',')
                        .next()
                        .unwrap_or("")
                        .trim();
                    if path.ends_with(".ttc") || path.ends_with(".TTC") {
                        continue;
                    }
                    if !family.is_empty() && seen.insert(path.to_string()) {
                        out.push((path.to_string(), family.to_string()));
                    }
                }
                return out;
            }
            _ => {}
        }
    }

    // 2) Directory walk (non-NixOS or fc-list missing).
    for dir in font_dirs() {
        let mut stack: Vec<PathBuf> = vec![dir];
        let mut depth = 0;
        while let Some(d) = stack.pop() {
            let Ok(rd) = std::fs::read_dir(&d) else { continue };
            for entry in rd.flatten() {
                let p = entry.path();
                if p.is_dir() && depth < 5 {
                    stack.push(p);
                } else if let Some(ext) = p.extension().and_then(|e| e.to_str()) {
                    let ext = ext.to_lowercase();
                    if (ext == "ttf" || ext == "otf") && seen.insert(p.to_string_lossy().into_owned())
                    {
                        let fam = p
                            .file_stem()
                            .map(|s| s.to_string_lossy().replace(['-', '_'], " "))
                            .unwrap_or_default();
                        out.push((p.to_string_lossy().into_owned(), fam));
                    }
                }
            }
            depth += 1;
            if depth > 5 {
                break;
            }
        }
    }
    out
}

/// Directories to scan for fonts when fc-list is unavailable.
fn font_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Ok(x) = std::env::var("XDG_DATA_HOME") {
        if !x.is_empty() {
            dirs.push(PathBuf::from(x).join("fonts"));
        }
    }
    if let Ok(x) = std::env::var("XDG_DATA_DIRS") {
        for d in x.split(':') {
            if !d.is_empty() {
                dirs.push(PathBuf::from(d).join("fonts"));
            }
        }
    }
    if let Ok(h) = std::env::var("HOME") {
        if !h.is_empty() {
            dirs.push(PathBuf::from(&h).join(".fonts"));
            dirs.push(PathBuf::from(&h).join(".local/share/fonts"));
        }
    }
    for d in [
        "/usr/share/fonts",
        "/usr/local/share/fonts",
        "/run/current-system/sw/share/fonts",
        "/etc/profiles/per-user/share/fonts",
    ] {
        dirs.push(PathBuf::from(d));
    }
    if let Ok(u) = std::env::var("USER") {
        if !u.is_empty() {
            dirs.push(PathBuf::from(format!("/etc/profiles/per-user/{u}/share/fonts")));
        }
    }
    // <dir>/abs/path</dir> entries from fontconfig config files.
    for conf in ["/etc/fonts/fonts.conf", "/etc/fonts/fonts.d"] {
        let p = Path::new(conf);
        if p.is_dir() {
            for f in std::fs::read_dir(p).into_iter().flatten().flatten() {
                collect_conf_dirs(&f.path(), &mut dirs);
            }
        } else if p.is_file() {
            collect_conf_dirs(p, &mut dirs);
        }
    }
    dirs
}

/// Extract absolute <dir> entries from a fontconfig XML config.
fn collect_conf_dirs(path: &Path, out: &mut Vec<PathBuf>) {
    let Ok(text) = std::fs::read_to_string(path) else { return };
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("<dir>") {
            if let Some(dir) = rest.strip_suffix("</dir>") {
                let dir = dir.trim();
                if dir.starts_with('/') && !dir.contains("..") {
                    out.push(PathBuf::from(dir));
                }
            }
        }
    }
}
