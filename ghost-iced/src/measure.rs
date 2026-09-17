//! Text measurement for the chrome's geometry.
//!
//! An iced canvas cannot measure text (the long-open iced#478), but every chrome
//! box in the macroquad build is sized `measure_text(label) + pad` — a fixed
//! width would visibly misplace the right-aligned status items and move the
//! wave's start/end x, which are derived from the hint string's width. So widths
//! are measured here from the same font file with the same fontdue call macroquad
//! uses (`measure_text` sums per-char advances), which makes the numbers match
//! the reference for the monospace face we draw with.
//!
//! Also converts the reference's baselines to iced's top-left text origin:
//! `top = baseline - ascent(px)`.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use fontdue::{Font, FontSettings};

fn font() -> &'static Font {
    static FONT: OnceLock<Font> = OnceLock::new();
    FONT.get_or_init(|| {
        Font::from_bytes(crate::FONT_BYTES, FontSettings::default()).expect("fontdue: parse embedded font")
    })
}

/// Width of `text` at `px`, in the same units as macroquad's `measure_text`.
pub fn text_width(text: &str, px: f32) -> f32 {
    text.chars().map(|c| advance(c, px)).sum()
}

/// Ascent of the face at `px` — the offset from a macroquad baseline (text y) to
/// the top of the text box iced draws from.
pub fn ascent(px: f32) -> f32 {
    static CACHE: OnceLock<Mutex<HashMap<u32, f32>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let key = px.to_bits();
    if let Some(a) = cache.lock().unwrap().get(&key) {
        return *a;
    }
    let ascent = font()
        .horizontal_line_metrics(px)
        .map(|m| m.ascent)
        .unwrap_or(px * 0.8);
    cache.lock().unwrap().insert(key, ascent);
    ascent
}

/// Advance width of one char, cached (the lookup builds metrics per call).
fn advance(c: char, px: f32) -> f32 {
    static CACHE: OnceLock<Mutex<HashMap<(u32, u32), f32>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let key = (c as u32, px.to_bits());
    if let Some(w) = cache.lock().unwrap().get(&key) {
        return *w;
    }
    let w = font().metrics(c, px).advance_width;
    cache.lock().unwrap().insert(key, w);
    w
}
