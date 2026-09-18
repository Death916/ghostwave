//! Text measurement for the pane painter: a canvas cannot measure text itself, so
//! baselines convert to iced's top-left origin as `top = baseline - ascent(px)`.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use fontdue::{Font, FontSettings};

fn font() -> &'static Font {
    static FONT: OnceLock<Font> = OnceLock::new();
    FONT.get_or_init(|| {
        Font::from_bytes(crate::ui_fonts::primary_bytes(), FontSettings::default())
            .expect("fontdue: parse primary font")
    })
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
