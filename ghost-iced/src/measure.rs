//! Text measurement for the chrome's geometry: widths come from the embedded
//! font's per-char advances (an iced canvas cannot measure text itself), and
//! baselines convert to iced's top-left origin as `top = baseline - ascent(px)`.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use fontdue::{Font, FontSettings};

fn font() -> &'static Font {
    static FONT: OnceLock<Font> = OnceLock::new();
    FONT.get_or_init(|| {
        Font::from_bytes(crate::FONT_BYTES, FontSettings::default()).expect("fontdue: parse embedded font")
    })
}

/// Returns the width of `text` at `px` in logical px, as the sum of the embedded
/// font's per-char advances.
pub fn text_width(text: &str, px: f32) -> f32 {
    text.chars().map(|c| advance(c, px)).sum()
}

/// Returns the distance from a text baseline to the top of the box iced draws
/// from, at `px`.
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

/// Returns the advance width of one char at `px`, cached because the fontdue
/// lookup builds metrics per call.
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
