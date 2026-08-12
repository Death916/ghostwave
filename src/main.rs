//! ghostwave — a waveterm-style terminal built on libghostty-vt (Rust),
//! with zellij-style keybindings.
//!
//! Layout: this file is the macroquad event loop + chrome rendering; the
//! terminal logic lives in `term.rs` (adapted from ghostling-rs), panes/
//! tabs/actions in `app.rs`, keybindings in `keymap.rs`.

mod ai;
mod app;
mod clipboard;
mod config;
mod fonts;
mod keymap;
mod layout;
mod palette;
mod preview;
mod pty;
mod search;
mod session;
mod term;
mod theme;

use std::collections::VecDeque;

use macroquad::prelude::*;

use app::{App, Pane, TAB_BAR_H};
use keymap::Mode;

/// Crate-wide error type (used by term.rs, app.rs, etc.).
pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// All keys we track for press/release/repeat (macroquad KeyCode list).
const ALL_KEYS: &[KeyCode] = &[
    KeyCode::A, KeyCode::B, KeyCode::C, KeyCode::D, KeyCode::E, KeyCode::F, KeyCode::G,
    KeyCode::H, KeyCode::I, KeyCode::J, KeyCode::K, KeyCode::L, KeyCode::M, KeyCode::N,
    KeyCode::O, KeyCode::P, KeyCode::Q, KeyCode::R, KeyCode::S, KeyCode::T, KeyCode::U,
    KeyCode::V, KeyCode::W, KeyCode::X, KeyCode::Y, KeyCode::Z,
    KeyCode::Key0, KeyCode::Key1, KeyCode::Key2, KeyCode::Key3, KeyCode::Key4,
    KeyCode::Key5, KeyCode::Key6, KeyCode::Key7, KeyCode::Key8, KeyCode::Key9,
    KeyCode::Space, KeyCode::Enter, KeyCode::Tab, KeyCode::Backspace, KeyCode::Delete,
    KeyCode::Escape, KeyCode::Up, KeyCode::Down, KeyCode::Left, KeyCode::Right,
    KeyCode::Home, KeyCode::End, KeyCode::PageUp, KeyCode::PageDown, KeyCode::Insert,
    KeyCode::F1, KeyCode::F2, KeyCode::F3, KeyCode::F4, KeyCode::F5, KeyCode::F6,
    KeyCode::F7, KeyCode::F8, KeyCode::F9, KeyCode::F10, KeyCode::F11, KeyCode::F12,
    KeyCode::Minus, KeyCode::Equal, KeyCode::LeftBracket, KeyCode::RightBracket,
    KeyCode::Backslash, KeyCode::Semicolon, KeyCode::Apostrophe, KeyCode::Comma,
    KeyCode::Period, KeyCode::Slash, KeyCode::GraveAccent,
];

/// Keys that produce text characters via the platform char event.
fn key_has_text(code: KeyCode) -> bool {
    matches!(
        code,
        KeyCode::A | KeyCode::B | KeyCode::C | KeyCode::D | KeyCode::E | KeyCode::F
            | KeyCode::G | KeyCode::H | KeyCode::I | KeyCode::J | KeyCode::K | KeyCode::L
            | KeyCode::M | KeyCode::N | KeyCode::O | KeyCode::P | KeyCode::Q | KeyCode::R
            | KeyCode::S | KeyCode::T | KeyCode::U | KeyCode::V | KeyCode::W | KeyCode::X
            | KeyCode::Y | KeyCode::Z | KeyCode::Key0 | KeyCode::Key1 | KeyCode::Key2
            | KeyCode::Key3 | KeyCode::Key4 | KeyCode::Key5 | KeyCode::Key6 | KeyCode::Key7
            | KeyCode::Key8 | KeyCode::Key9 | KeyCode::Space | KeyCode::Minus | KeyCode::Equal
            | KeyCode::LeftBracket | KeyCode::RightBracket | KeyCode::Backslash
            | KeyCode::Semicolon | KeyCode::Apostrophe | KeyCode::Comma | KeyCode::Period
            | KeyCode::Slash | KeyCode::GraveAccent
    )
}

fn handle_input(app: &mut App) {
    // Queue of platform text chars (includes auto-repeat for text keys).
    let mut chars: VecDeque<char> = VecDeque::new();
    while let Some(c) = get_char_pressed() {
        chars.push_back(c);
    }

    for &code in ALL_KEYS {
        if is_key_pressed(code) {
            // Attach the first queued char to the key event (if this key
            // normally produces text). The char is consumed either way.
            let text = if key_has_text(code) { chars.pop_front() } else { None };
            if let Some(kp) = keymap::from_macroquad(code, text) {
                app.handle_key(kp);
                app.clear_repeat(code);
            }
        } else if is_key_down(code) && !key_has_text(code) {
            // Synthetic repeat for non-text keys (arrows, backspace, ...).
            if let Some(kp) = keymap::from_macroquad(code, None) {
                app.handle_repeat(code, kp);
            }
        }
        if is_key_released(code) {
            if let Some(kp) = keymap::from_macroquad(code, None) {
                app.handle_key_release(kp);
            }
            app.clear_repeat(code);
        }
    }

    // Leftover chars (unicode / IME) that didn't map to a tracked key.
    while let Some(c) = chars.pop_front() {
        if !c.is_control() {
            app.handle_char(c);
        }
    }
}

fn color_theme(rgb: crate::theme::Rgb) -> Color {
    color_u8!(rgb.0, rgb.1, rgb.2, 255)
}

fn render(app: &mut App) {
    let (w, h) = (screen_width(), screen_height());
    clear_background(color_theme(app.theme.bg));

    let theme = app.theme;
    let cell = app.cell;
    let padding = app.config.padding;
    let font_px = app.font_px;
    let font = app.font.clone();
    let font_stack = app.font_stack.clone();
    let active_id = app.active_pane_id();
    let show_frames = app.show_frames;

    // ---- panes -----------------------------------------------------------
    let rects = app.active_pane_rects();
    let pane_ids: Vec<usize> = rects.iter().map(|(id, _)| *id).collect();
    for id in pane_ids {
        let rect = rects.iter().find(|(p, _)| *p == id).map(|(_, r)| *r).unwrap_or_default();
        match &mut app.current_tab_mut().panes[id] {
            Pane::Term(t) => {
                set_scissor(rect.x, rect.y, rect.w, rect.h);
                let _ = t.render(
                    rect,
                    cell,
                    padding,
                    &font_stack,
                    font_px,
                    theme,
                    id == active_id,
                );
                reset_scissor();
            }
            Pane::Preview(p) => {
                set_scissor(rect.x, rect.y, rect.w, rect.h);
                p.render(rect, &font, font_px, theme);
                reset_scissor();
            }
            Pane::Ai(a) => {
                set_scissor(rect.x, rect.y, rect.w, rect.h);
                a.render(rect, &font, font_px, theme);
                reset_scissor();
            }
            Pane::Closed => {}
        }
    }

    // ---- pane frames (zellij) --------------------------------------------
    if show_frames {
        for (i, (id, rect)) in rects.iter().enumerate() {
            let col = if *id == active_id { theme.frame_active } else { theme.frame };
            draw_rectangle_lines(rect.x - 2.0, rect.y - 2.0, rect.w + 4.0, rect.h + 4.0, 1.0, color_theme(col));
            // Pane label: index + name (zellij style).
            let name = match &app.current_tab().panes[*id] {
                Pane::Term(t) => {
                    let title = t.title();
                    let n = if !t.name.is_empty() { &t.name } else { "pane" };
                    if title.is_empty() {
                        format!("{n} {}", i + 1)
                    } else {
                        format!("{n} {i} — {title}")
                    }
                }
                Pane::Preview(_) => format!("file {}", i + 1),
                Pane::Ai(_) => format!("ai {}", i + 1),
                Pane::Closed => String::new(),
            };
            let label = format!(" {} ", name);
            let lw = measure_text(&label, Some(&font), 13, 1.0).width + 8.0;
            draw_rectangle(rect.x - 2.0, rect.y - 14.0, lw, 14.0, color_theme(col));
            draw_text_ex(
                &label,
                rect.x,
                rect.y - 3.0,
                TextParams { font: Some(&font), font_size: 13, color: color_theme(theme.tab_bg), ..Default::default() },
            );
        }
    }

    // ---- tab bar ---------------------------------------------------------
    draw_rectangle(0.0, 0.0, w, TAB_BAR_H, color_theme(theme.tab_bg));
    let tab_w = 130.0_f32;
    for (i, tab) in app.tabs.iter().enumerate() {
        let x = i as f32 * tab_w;
        let active = i == app.active_tab;
        draw_rectangle(x, 0.0, tab_w - 4.0, TAB_BAR_H, color_theme(if active { theme.tab_active } else { theme.tab_bg }));
        let label = if tab.name.is_empty() {
            format!("Tab {}", i + 1)
        } else {
            tab.name.clone()
        };
        let col = if active { theme.fg } else { theme.tab_inactive_text };
        draw_text_ex(
            &label,
            x + 10.0,
            TAB_BAR_H / 2.0 + 4.0,
            TextParams { font: Some(&font), font_size: 14, color: color_theme(col), ..Default::default() },
        );
        }

        // ---- tab context menu -------------------------------------------------
        if let Some((tab_idx, mx, my)) = app.tab_menu {
        use app::{TAB_MENU_ITEM_H, TAB_MENU_ITEMS, TAB_MENU_PAD, TAB_MENU_W};
        let menu_h = TAB_MENU_PAD + TAB_MENU_ITEMS as f32 * TAB_MENU_ITEM_H + TAB_MENU_PAD;
        draw_rectangle(mx, my, TAB_MENU_W, menu_h, color_theme(theme.bg));
        draw_rectangle_lines(mx, my, TAB_MENU_W, menu_h, 1.0, color_theme(theme.tab_bg));
        let items = [
            "Rename tab",
            "Close tab",
        ];
        for (i, label) in items.iter().enumerate() {
            let y = my + TAB_MENU_PAD + i as f32 * TAB_MENU_ITEM_H;
            draw_text_ex(
                label,
                mx + 12.0,
                y + TAB_MENU_ITEM_H / 2.0 + 5.0,
                TextParams { font: Some(&font), font_size: 13, color: color_theme(theme.palette_fg), ..Default::default() },
            );
        }
        }

    // ---- session manager overlay ------------------------------------------
    if let Some((entries, selected)) = &app.session_list {
        let w = 460.0_f32;
        let item_h = 24.0_f32;
        let title_h = 30.0_f32;
        let hint_h = 22.0_f32;
        let h = title_h + entries.len() as f32 * item_h + hint_h;
        let x = (screen_width() - w) / 2.0;
        let y = (screen_height() - h) / 3.0;
        draw_rectangle(x, y, w, h, color_theme(theme.bg));
        draw_rectangle_lines(x, y, w, h, 1.0, color_theme(theme.tab_bg));
        draw_text_ex(
            &format!("SESSIONS — current: {}", app.current_session),
            x + 12.0,
            y + 20.0,
            TextParams { font: Some(&font), font_size: 13, color: color_theme(theme.search_match), ..Default::default() },
        );
        for (i, name) in entries.iter().enumerate() {
            let yy = y + title_h + i as f32 * item_h;
            if i == *selected {
                draw_rectangle(x, yy, w, item_h, color_theme(theme.tab_active));
            }
            let cur = *name == app.current_session;
            let label = if cur { format!("● {name}") } else { format!("  {name}") };
            draw_text_ex(
                &label,
                x + 12.0,
                yy + item_h / 2.0 + 5.0,
                TextParams { font: Some(&font), font_size: 13, color: color_theme(theme.palette_fg), ..Default::default() },
            );
        }
        draw_text_ex(
            "↑/↓ select | Enter switch | n new | r rename | d delete | Esc close",
            x + 12.0,
            y + h - 8.0,
            TextParams { font: Some(&font), font_size: 12, color: color_theme(theme.status_fg), ..Default::default() },
        );
    }

    // ---- search input overlay --------------------------------------------
    if app.mode == Mode::EnterSearch {
        if let Some(t) = app.active_term() {
            let total = t.search.matches.len();
            let cur = t.search.current.min(total.saturating_sub(1));
            let q = if t.search.query.is_empty() { "type to search…".to_string() } else { t.search.query.clone() };
            let flags = format!(
                "{} {} {}",
                if t.search.case_sensitive { "[c]ase ✓" } else { "[c]ase" },
                if t.search.wrap { "[w]rap ✓" } else { "[w]rap" },
                if t.search.whole_word { "[o]word ✓" } else { "[o]word" },
            );
            let label = format!(" 🔍 {}   {}/{}   {flags}", if q.is_empty() { "type to search…" } else { &q }, cur + 1, total);
            let lw = measure_text(&label, Some(&font), 14, 1.0).width + 24.0;
            let bx = (w - lw) / 2.0;
            draw_rectangle(bx, TAB_BAR_H + 6.0, lw, 30.0, color_theme(theme.palette_bg));
            draw_rectangle_lines(bx, TAB_BAR_H + 6.0, lw, 30.0, 1.0, color_theme(theme.frame_active));
            draw_text_ex(
                &label,
                bx + 12.0,
                TAB_BAR_H + 26.0,
                TextParams { font: Some(&font), font_size: 14, color: color_theme(theme.palette_fg), ..Default::default() },
            );
        }
    }

    // ---- rename overlay --------------------------------------------------
    if app.mode == Mode::RenameTab || app.mode == Mode::RenamePane {
        let what = if app.mode == Mode::RenameTab { "Tab" } else { "Pane" };
        let label = format!(" rename {what}: {}_", app.rename_buf);
        let lw = measure_text(&label, Some(&font), 14, 1.0).width + 24.0;
        let bx = (w - lw) / 2.0;
        draw_rectangle(bx, TAB_BAR_H + 6.0, lw, 30.0, color_theme(theme.palette_bg));
        draw_rectangle_lines(bx, TAB_BAR_H + 6.0, lw, 30.0, 1.0, color_theme(theme.frame_active));
        draw_text_ex(
            &label,
            bx + 12.0,
            TAB_BAR_H + 26.0,
            TextParams { font: Some(&font), font_size: 14, color: color_theme(theme.palette_fg), ..Default::default() },
        );
    }

    // ---- command palette -------------------------------------------------
    if let Some(pal) = &app.palette {
        let bw = (w * 0.6).min(640.0);
        let bh = 360.0_f32;
        let bx = (w - bw) / 2.0;
        let by = 60.0;
        draw_rectangle(bx, by, bw, bh, color_theme(theme.palette_bg));
        draw_rectangle_lines(bx, by, bw, bh, 1.0, color_theme(theme.frame_active));
        // Input line
        let prompt = format!("> {}", pal.input);
        draw_text_ex(
            &prompt,
            bx + 12.0,
            by + 28.0,
            TextParams { font: Some(&font), font_size: 16, color: color_theme(theme.palette_fg), ..Default::default() },
        );
        draw_line(bx + 6.0, by + 38.0, bx + bw - 6.0, by + 38.0, 1.0, color_theme(theme.frame));
        // Items
        let filtered = pal.filtered();
        let visible = 10.min(filtered.len());
        for i in 0..visible {
            let item = &filtered[i];
            let y = by + 48.0 + i as f32 * 28.0;
            let selected = i == pal.cursor.min(filtered.len().saturating_sub(1));
            if selected {
                draw_rectangle(bx + 4.0, y - 18.0, bw - 8.0, 26.0, color_theme(theme.palette_sel));
            }
            draw_text_ex(
                &item.label,
                bx + 14.0,
                y,
                TextParams { font: Some(&font), font_size: 15, color: color_theme(if selected { theme.bg } else { theme.palette_fg }), ..Default::default() },
            );
        }
        if filtered.is_empty() {
            draw_text_ex(
                "no matching command (try: open <path> | ssh <host> | theme <name>)",
                bx + 14.0,
                by + 66.0,
                TextParams { font: Some(&font), font_size: 14, color: color_theme(theme.tab_inactive_text), ..Default::default() },
            );
        }
    }

    // ---- status bar ------------------------------------------------------
    let sh = 26.0_f32;
    draw_rectangle(0.0, h - sh, w, sh, color_theme(theme.status_bg));
    let mode_label = format!(" {} ", app.mode.label());
    let mw = measure_text(&mode_label, Some(&font), 13, 1.0).width + 12.0;
    draw_rectangle(0.0, h - sh, mw, sh, color_theme(theme.frame_active));
    draw_text_ex(
        &mode_label,
        6.0,
        h - 8.0,
        TextParams { font: Some(&font), font_size: 13, color: color_theme(theme.bg), ..Default::default() },
    );
    let hints = app.mode.hints();
    draw_text_ex(
        hints,
        mw + 10.0,
        h - 8.0,
        TextParams { font: Some(&font), font_size: 13, color: color_theme(theme.status_fg), ..Default::default() },
    );
    let right = format!(" {} ", app.theme_name);
    let rw = measure_text(&right, Some(&font), 13, 1.0).width + 12.0;
    draw_text_ex(
        &right,
        w - rw,
        h - 8.0,
        TextParams { font: Some(&font), font_size: 13, color: color_theme(theme.status_fg), ..Default::default() },
    );

    // ---- toast -----------------------------------------------------------
    if let Some((msg, t0)) = &app.toast {
        if t0.elapsed().as_secs_f32() < 2.5 {
            let label = format!(" {msg} ");
            let lw = measure_text(&label, Some(&font), 13, 1.0).width + 16.0;
            draw_rectangle(8.0, h - sh - 30.0, lw, 24.0, color_theme(theme.palette_sel));
            draw_text_ex(
                &label,
                16.0,
                h - sh - 12.0,
                TextParams { font: Some(&font), font_size: 13, color: color_theme(theme.bg), ..Default::default() },
            );
        }
    }
}

fn reset_scissor() {
    unsafe {
        macroquad::window::get_internal_gl().quad_gl.scissor(None);
    }
}

/// Clip drawing to a logical-pixel rect. macroquad has no public scissor
/// API; quad_gl.scissor works in physical px (Y flipped internally).
fn set_scissor(x: f32, y: f32, w: f32, h: f32) {
    // TEMP: disabled to test whether the scissor clips everything away.
    let _ = (x, y, w, h);
}

fn macroquad_conf() -> macroquad::conf::Conf {
    macroquad::conf::Conf {
        miniquad_conf: miniquad::conf::Conf {
            window_title: "ghostwave".to_owned(),
            window_width: 1280,
            window_height: 800,
            window_resizable: true,
            high_dpi: true,
            platform: miniquad::conf::Platform {
                linux_backend: miniquad::conf::LinuxBackend::WaylandOnly, // TEMP: force wayland to see init error
                ..Default::default()
            },
            ..Default::default()
        },
        ..Default::default()
    }
}

#[macroquad::main(macroquad_conf)]
async fn main() -> Result<()> {
    let _ = term::install_png_decoder();

    let config = config::Config::load();

    // Write a default config file on first run so the user can tweak it.
    if !config::Config::path().exists() {
        if let Some(dir) = config::Config::path().parent() {
            let _ = std::fs::create_dir_all(dir);
            let _ = std::fs::write(
                config::Config::path(),
                "# ghostwave configuration\n# font_size is in points (12 = 12pt, like ghostty/waveterm)\nfont_size = 12.0\ntheme = \"tokyonight\"\nscrollback = 10000\n# per-pane durable-session log cap in MiB (front-truncated when exceeded)\nmax_log_mb = 32\npadding = 6.0\nrow_gap = 4.0\n\n[ai]\nbase_url = \"https://api.minimax.chat/v1\"\nmodel = \"MiniMax-M2\"\napi_key_env = \"MINIMAX_API_KEY\"\n",
            );
        }
    }

    let font = load_ttf_font_from_bytes(include_bytes!("../assets/JetBrainsMono-Medium.ttf"))?;
    let font_stack = fonts::FontStack::new(include_bytes!("../assets/JetBrainsMono-Medium.ttf"), font.clone())?;
    let mut app = App::new(config, font, font_stack)?;

    loop {
        app.update();

        // Recompute pane grids when the window/layout changes (no-op otherwise).
        app.layout_all();

        handle_input(&mut app);
        app.handle_mouse();

        render(&mut app);

        if app.quit {
            app.save_session();
            std::process::exit(0);
        }

        next_frame().await
    }
}
