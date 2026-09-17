//! Chrome painter for the iced front-end: tab bar, status bar (mode label,
//! hints, theme name, sync chip, ghostwave visualizer) and toast.
//!
//! A direct port of `CHROME-REFERENCE.md` §1 (tab bar), §2 (status bar +
//! Wave) and §3 (toasts) — every number here is quoted from the macroquad
//! build at the `main.rs`/`app.rs` lines the spec gives. The reference paints
//! chrome LAST, after panes and their frames, so it sits on top; the iced
//! caller does the same (M1's canvas draws the panes, this runs after).
//!
//! Two conventions differ from the reference and are handled here:
//!
//! * **Baselines.** macroquad's text `y` is a *baseline*; iced draws from the
//!   box's top-left with `align_y: Top`. Every position below is therefore
//!   `Point::new(x, baseline - crate::measure::ascent(px))` via [`draw_text`],
//!   never an eyeballed offset.
//! * **Measured widths.** iced canvases cannot measure text (iced#478), while
//!   every box in the reference is `measure_text(label, px).width + pad`. So
//!   the pads stay literal here but the widths come from
//!   [`crate::measure::text_width`] (same face, same fontdue advance sum), or
//!   the right-aligned status items and the wave's endpoints would drift.

use ghostwave::app::{STATUS_BAR_H, TAB_BAR_H};
use ghostwave::theme::Rgb;
use ghostwave::theme::Theme as GhostTheme;
use iced::widget::canvas::{Frame, Path, Stroke, Text as CanvasText};
use iced::widget::text;
use iced::{alignment, Color, Font, Pixels, Point, Size};

use crate::measure::{ascent, text_width};

// ── §0 Constants ────────────────────────────────────────────────────────────

/// Tab label size (hardcoded 14 px in the reference; chrome sizes do NOT scale
/// with `config.font_size`, §0).
const TAB_LABEL_PX: f32 = 14.0;
/// Status bar / hints / theme name / sync chip / toast size (hardcoded 13 px).
const STATUS_PX: f32 = 13.0;

/// Tab pitch — `main.rs:268` (`let tab_w = 130.0`), `x = i * tab_w`, unclamped:
/// overflow tabs are simply off-screen (no wrap/scroll/ellipsis, §1).
///
/// Quirk (§1, §9.1): the reference's *hit test* uses 120.0 while the *renderer*
/// uses 130.0 (`app.rs:2593,2601` vs `main.rs:268`), so the right 10 px of each
/// chip selects the next tab. The port keeps 130.0 for painting (the spec's
/// instruction); `crate::app` owns the hit test.
const TAB_W: f32 = 130.0;
/// Active-chip width is `tab_w - 4.0` = 126 px; the untouched 4 px right gap of
/// `tab_bg` reads as the separator. No underline, no border (§1).
const TAB_CHIP_INSET: f32 = 4.0;
/// Tab label x offset (§1).
const TAB_LABEL_X: f32 = 10.0;
/// Tab label baseline = `TAB_BAR_H / 2.0 + 4.0` (§0/§1: box centre + 4 px at 14).
const TAB_LABEL_BASELINE_DY: f32 = 4.0;

/// Mode-label box horizontal pad: `mw = measure_text(label, 13).width + 12.0` (§2.2).
const MODE_PAD: f32 = 12.0;
/// Mode-label text x (§2.2).
const MODE_TEXT_X: f32 = 6.0;
/// Hints x = `mw + 10.0` (§2.3).
const HINTS_DX: f32 = 10.0;
/// Theme-name box pad: `rw = measure + 12` (§2.4).
const THEME_PAD: f32 = 12.0;
/// Sync-chip pad: `sw = measure(label, 13).width + 8.0` (§2.5).
const SYNC_PAD: f32 = 8.0;
/// Sync-chip vertical inset: the rect is `sh - 4` tall at `h - sh + 2` (§2.5).
const SYNC_INSET_Y: f32 = 2.0;
/// Sync-chip text x = `sx + 4.0` (§2.5).
const SYNC_TEXT_DX: f32 = 4.0;
/// Chip label, spaces both sides (§2.5).
const SYNC_LABEL: &str = " [SYNC] ";

/// Status text baseline: `h - 8.0` for all three status items (§0: a 13 px
/// baseline is ≈ box centre + 5).
const STATUS_BASELINE_INSET: f32 = 8.0;
/// The wave keeps this much clear of the hints and of the right-hand items
/// (§2 Wave: `+ 24.0` on both `wave_start_x` and `wave_end_x`).
const WAVE_MARGIN: f32 = 24.0;
/// The wave is skipped unless it has this much room (§2 Wave draw guard).
const WAVE_MIN_W: f32 = 50.0;
/// `step = 3.0` px between samples (§2 Wave).
const WAVE_STEP: f32 = 3.0;

/// Toast box pad: `lw = measure_text(label, 13).width + 16.0` (§3); the label
/// itself already carries a leading and a trailing space.
const TOAST_PAD: f32 = 16.0;
/// Toast box x (§3: 8 px from the left edge).
const TOAST_X: f32 = 8.0;
/// Toast box height (§3).
const TOAST_H: f32 = 24.0;
/// The toast's bottom edge sits 6 px above the status bar: `h - sh - 30 + 24` (§3).
const TOAST_BOTTOM_GAP: f32 = 6.0;
/// Toast text x (§3).
const TOAST_TEXT_X: f32 = 16.0;

// ── Public interface ────────────────────────────────────────────────────────

/// Live wave parameters; `None` disables the wave (`config.visual_wave` off or
/// the parent chose not to animate at all).
#[derive(Clone, Copy, Debug)]
pub struct WaveParams {
    /// `app.wave_energy`: floor 0.1, clamped to 2.5. Drives the amplitude and,
    /// through `min(energy / 2.0, 1.0)`, the stroke alpha.
    pub energy: f32,
    /// `app.wave_phase` in radians; the caller advances and wraps it.
    pub phase: f32,
    /// `config.wave_amplitude` (default 1.0): `max_amp = sh * 0.35 * this`,
    /// clamped to 2..=10 px.
    pub amplitude: f32,
}

/// Everything the chrome needs to paint one frame.
///
/// `toast`: the reference keeps `Option<(String, Instant)>`, draws it while
/// `elapsed < 2.5` s and clears it at `>= 3` s (`main.rs:577` vs `app.rs:2886`).
/// Quirk (§3, §9.2): the port-correct lifetime is **2.5 s**, so the caller drops
/// the string then; this painter draws whatever it is handed.
pub struct ChromeState<'a> {
    /// (label, is_active) per tab, in tab order.
    pub tabs: &'a [(String, bool)],
    pub mode_label: &'a str,
    pub hints: &'a str,
    pub theme_name: &'a str,
    pub sync_panes: bool,
    pub wave: Option<WaveParams>,
    pub toast: Option<&'a str>,
}

/// Paint tab bar, status bar (with the wave) and toast. Called LAST, after the
/// panes and their frames, so chrome sits on top — the reference draws it in
/// that order too. `window` is the whole window size in logical px.
pub fn paint(
    frame: &mut Frame,
    window: Size,
    font: Font,
    theme: &GhostTheme,
    state: &ChromeState<'_>,
) {
    let (w, h) = (window.width, window.height);
    // The reference's local `let sh = 26.0_f32` (`main.rs:444`) duplicates
    // `STATUS_BAR_H` (§0, §9.6); use the constant.
    let sh = STATUS_BAR_H;

    // Draw order is the reference's: tab bar → status bar → sync chip → wave →
    // toast (last = topmost; the tab menu, session manager, palette, search and
    // rename overlays are M3 and live outside this module).
    paint_tab_bar(frame, w, font, theme, state);

    let geom = StatusGeom::new(state);
    paint_status_bar(frame, w, h, sh, font, theme, state, &geom);
    if let Some(wave) = state.wave {
        paint_wave(frame, w, h, sh, theme, &geom, state, wave);
    }
    if let Some(toast) = state.toast {
        paint_toast(frame, h, sh, font, theme, toast);
    }
}

// ── §1 Tab bar ──────────────────────────────────────────────────────────────

fn paint_tab_bar(
    frame: &mut Frame,
    w: f32,
    font: Font,
    theme: &GhostTheme,
    state: &ChromeState<'_>,
) {
    // Bar background: `draw_rectangle(0, 0, w, TAB_BAR_H)` in `tab_bg`.
    frame.fill_rectangle(Point::ORIGIN, Size::new(w, TAB_BAR_H), rgb(theme.tab_bg));

    for (i, (label, active)) in state.tabs.iter().enumerate() {
        let x = i as f32 * TAB_W;
        // Active chip `(x, 0, tab_w - 4, TAB_BAR_H)` in `tab_active`; an
        // inactive chip is filled `tab_bg`, i.e. invisible.
        let chip = if *active {
            theme.tab_active
        } else {
            theme.tab_bg
        };
        frame.fill_rectangle(
            Point::new(x, 0.0),
            Size::new(TAB_W - TAB_CHIP_INSET, TAB_BAR_H),
            rgb(chip),
        );

        // `tab.name` when non-empty, else `Tab {i + 1}`. The tab model resolves
        // this upstream (the reference renames empty / `Tab `-prefixed tabs from
        // the active pane's OSC title), so the fallback only guards a blank name.
        let content = if label.is_empty() {
            format!("Tab {}", i + 1)
        } else {
            label.clone()
        };
        let color = if *active {
            theme.fg
        } else {
            theme.tab_inactive_text
        };
        draw_text(
            frame,
            content,
            Point::new(x + TAB_LABEL_X, TAB_BAR_H / 2.0 + TAB_LABEL_BASELINE_DY),
            TAB_LABEL_PX,
            rgb(color),
            font,
        );
    }
}

// ── §2 Status bar (+ §2 Wave) ───────────────────────────────────────────────

/// The status bar's measured box widths. The wave's endpoints depend on all of
/// them (`mw`, `rw` and, when it shows, `sw`), so they are computed once.
struct StatusGeom {
    /// `mw` — mode-label box width, `measure(" {MODE} ", 13) + 12`.
    mode_w: f32,
    /// `rw` — theme-name box width, `measure(" {name} ", 13) + 12`.
    theme_w: f32,
    /// `sw` — sync-chip width, `measure(" [SYNC] ", 13) + 8`; 0 when hidden.
    sync_w: f32,
    /// Whether the chip is drawn (only when the active tab syncs its panes).
    sync: bool,
}

impl StatusGeom {
    fn new(state: &ChromeState<'_>) -> Self {
        let sync = state.sync_panes;
        Self {
            mode_w: text_width(&format!(" {} ", state.mode_label), STATUS_PX) + MODE_PAD,
            theme_w: text_width(&format!(" {} ", state.theme_name), STATUS_PX) + THEME_PAD,
            sync_w: if sync {
                text_width(SYNC_LABEL, STATUS_PX) + SYNC_PAD
            } else {
                0.0
            },
            sync,
        }
    }
}

fn paint_status_bar(
    frame: &mut Frame,
    w: f32,
    h: f32,
    sh: f32,
    font: Font,
    theme: &GhostTheme,
    state: &ChromeState<'_>,
    geom: &StatusGeom,
) {
    let top = h - sh;

    // 1. Background `(0, h - sh, w, sh)` in `status_bg`.
    frame.fill_rectangle(Point::new(0.0, top), Size::new(w, sh), rgb(theme.status_bg));

    // 2. Mode label: text is `" {MODE} "` (spaces both sides), box `(0, h-sh, mw, sh)`
    //    in `frame_active`, text at x = 6, baseline `h - 8`, colour `bg`.
    frame.fill_rectangle(
        Point::new(0.0, top),
        Size::new(geom.mode_w, sh),
        rgb(theme.frame_active),
    );
    draw_text(
        frame,
        format!(" {} ", state.mode_label),
        Point::new(MODE_TEXT_X, h - STATUS_BASELINE_INSET),
        STATUS_PX,
        rgb(theme.bg),
        font,
    );

    // 3. Hints at `mw + 10`, same baseline, `status_fg`.
    draw_text(
        frame,
        state.hints.to_string(),
        Point::new(geom.mode_w + HINTS_DX, h - STATUS_BASELINE_INSET),
        STATUS_PX,
        rgb(theme.status_fg),
        font,
    );

    // 4. Theme name, right-aligned: `" {name} "` at `w - rw`, same baseline,
    //    `status_fg`. (Display quirk, §9.9: the reference shows the configured
    //    name even after a silent theme fallback — the caller decides what string
    //    to hand in, so pass `config.theme` verbatim for parity.)
    draw_text(
        frame,
        format!(" {} ", state.theme_name),
        Point::new(w - geom.theme_w, h - STATUS_BASELINE_INSET),
        STATUS_PX,
        rgb(theme.status_fg),
        font,
    );

    // 5. Sync chip, only when the active tab syncs: `sw` wide, 2 px inset
    //    vertically, at `w - rw - sw - 4`, fill `search_match`, text `bg`, no border.
    if geom.sync {
        let sx = w - geom.theme_w - geom.sync_w - 4.0;
        frame.fill_rectangle(
            Point::new(sx, top + SYNC_INSET_Y),
            Size::new(geom.sync_w, sh - SYNC_INSET_Y * 2.0),
            rgb(theme.search_match),
        );
        draw_text(
            frame,
            SYNC_LABEL.to_string(),
            Point::new(sx + SYNC_TEXT_DX, h - STATUS_BASELINE_INSET),
            STATUS_PX,
            rgb(theme.bg),
            font,
        );
    }
}

/// The "ghostwave" visualizer: two alpha-blended harmonic ribbons stroked
/// between consecutive samples, H1 in full before H2 (§2 Wave). The ribbon is
/// deliberately allowed to overflow the bar — at the defaults `max_amp` is
/// 9.1 px and the peak offset `max_amp * energy` reaches 22.75 px against
/// `sh / 2 = 13` — with no clip and no wave-coloured background.
fn paint_wave(
    frame: &mut Frame,
    w: f32,
    h: f32,
    sh: f32,
    theme: &GhostTheme,
    geom: &StatusGeom,
    state: &ChromeState<'_>,
    wave: WaveParams,
) {
    // `wave_start_x = mw + 10 + hints_w + 24`;
    // `wave_end_x = w - rw - 24`, or `w - rw - sw - 24` when the sync chip shows.
    let hints_w = text_width(state.hints, STATUS_PX);
    let start_x = geom.mode_w + HINTS_DX + hints_w + WAVE_MARGIN;
    let end_x = w - geom.theme_w - if geom.sync { geom.sync_w } else { 0.0 } - WAVE_MARGIN;

    // No wave in narrow windows (§2 Wave draw guard).
    if end_x <= start_x + WAVE_MIN_W {
        return;
    }

    let wave_w = end_x - start_x;
    // `center_y = h - sh / 2.0`: the status bar's vertical centre.
    let center_y = h - sh / 2.0;
    // `max_amp = (sh * 0.35 * config.wave_amplitude).clamp(2.0, 10.0)`.
    let max_amp = (sh * 0.35 * wave.amplitude).clamp(2.0, 10.0);
    // `num_points = (wave_w / step).max(10.0) as usize`; the loop runs
    // `0..=num_points` ⇒ num_points + 1 samples, and `progress = i / num_points`
    // puts the last sample exactly on `wave_end_x`.
    let num_points = (wave_w / WAVE_STEP).max(10.0) as usize;
    let dt = 1.0 / num_points as f32;

    // `envelope = sin(progress * PI)` — 0 at both ends, 1 in the middle. It
    // multiplies the alpha as well as the offset, so each segment carries its
    // own alpha; iced strokes one colour per path, hence one short path per
    // segment (macroquad emits one `draw_line` per segment for the same reason).
    let envelope = |progress: f32| (progress * std::f32::consts::PI).sin();
    let x_at = |progress: f32| start_x + progress * wave_w;

    // H1 — primary ribbon: freq 0.035, stroke 1.5, `frame_active`,
    // `y_off = (sin(x*freq + phase)*0.7 + sin(x*freq*1.8 - phase*1.4)*0.3)
    //          * max_amp * energy * envelope`,
    // `alpha = (0.20 + 0.50 * min(energy/2, 1)) * envelope`.
    let h1_alpha0 = 0.20 + 0.50 * (wave.energy / 2.0).min(1.0);
    let mut prev = {
        let x = x_at(0.0);
        let y_off = ((x * 0.035 + wave.phase).sin() * 0.7
            + (x * 0.035 * 1.8 - wave.phase * 1.4).sin() * 0.3)
            * max_amp
            * wave.energy
            * envelope(0.0);
        Point::new(x, center_y + y_off)
    };
    for i in 0..num_points {
        let progress = i as f32 * dt;
        let next_progress = progress + dt;
        let x = x_at(next_progress);
        let y_off = ((x * 0.035 + wave.phase).sin() * 0.7
            + (x * 0.035 * 1.8 - wave.phase * 1.4).sin() * 0.3)
            * max_amp
            * wave.energy
            * envelope(next_progress);
        let next = Point::new(x, center_y + y_off);
        let alpha = h1_alpha0 * envelope(progress);
        stroke_segment(
            frame,
            prev,
            next,
            1.5,
            Color {
                a: alpha,
                ..rgb(theme.frame_active)
            },
        );
        prev = next;
    }

    // H2 — counter-phase glow: freq 0.022, stroke 1.0, `palette_sel`,
    // `y_off = (sin(x*freq*1.3 - phase*0.8)*0.8 + sin(x*freq*2.2 + phase*1.9)*0.2)
    //          * (max_amp*0.65) * energy * envelope`,
    // `alpha = (0.12 + 0.30 * min(energy/2, 1)) * envelope`.
    let h2_alpha0 = 0.12 + 0.30 * (wave.energy / 2.0).min(1.0);
    let amp_h2 = max_amp * 0.65;
    let mut prev = {
        let x = x_at(0.0);
        let y_off = ((x * 0.022 * 1.3 - wave.phase * 0.8).sin() * 0.8
            + (x * 0.022 * 2.2 + wave.phase * 1.9).sin() * 0.2)
            * amp_h2
            * wave.energy
            * envelope(0.0);
        Point::new(x, center_y + y_off)
    };
    for i in 0..num_points {
        let progress = i as f32 * dt;
        let next_progress = progress + dt;
        let x = x_at(next_progress);
        let y_off = ((x * 0.022 * 1.3 - wave.phase * 0.8).sin() * 0.8
            + (x * 0.022 * 2.2 + wave.phase * 1.9).sin() * 0.2)
            * amp_h2
            * wave.energy
            * envelope(next_progress);
        let next = Point::new(x, center_y + y_off);
        let alpha = h2_alpha0 * envelope(progress);
        stroke_segment(
            frame,
            prev,
            next,
            1.0,
            Color {
                a: alpha,
                ..rgb(theme.palette_sel)
            },
        );
        prev = next;
    }
}

/// Stroke one polyline segment: butt caps (iced's `Stroke` default), which is
/// what the reference's per-segment `draw_line` render does. The alpha is plain
/// non-premultiplied, as in the reference.
fn stroke_segment(frame: &mut Frame, from: Point, to: Point, width: f32, color: Color) {
    let path = Path::new(|p| {
        p.move_to(from);
        p.line_to(to);
    });
    frame.stroke(&path, Stroke::default().with_color(color).with_width(width));
}

// ── §3 Toast ────────────────────────────────────────────────────────────────

/// Bottom-left toast box: 8 px from the left edge, bottom edge 6 px above the
/// status bar, growing rightwards. One at a time, no border, no icon (§3).
fn paint_toast(frame: &mut Frame, h: f32, sh: f32, font: Font, theme: &GhostTheme, msg: &str) {
    let label = format!(" {msg} ");
    let lw = text_width(&label, STATUS_PX) + TOAST_PAD;
    // `h - sh - 30.0`; text baseline `h - sh - 12.0` (18 px below the box top,
    // in a 24 px box) — kept as the reference's baseline, not re-derived.
    let box_y = h - sh - TOAST_H - TOAST_BOTTOM_GAP;

    frame.fill_rectangle(
        Point::new(TOAST_X, box_y),
        Size::new(lw, TOAST_H),
        rgb(theme.palette_sel),
    );
    draw_text(
        frame,
        label,
        Point::new(TOAST_TEXT_X, h - sh - 12.0),
        STATUS_PX,
        rgb(theme.bg),
        font,
    );
}

// ── Shared ──────────────────────────────────────────────────────────────────

/// Draw one chrome string. `pos` is the reference's `(x, baseline)`; the
/// baseline is converted to iced's top-left text origin here.
fn draw_text(frame: &mut Frame, content: String, pos: Point, px: f32, color: Color, font: Font) {
    frame.fill_text(CanvasText {
        content,
        position: Point::new(pos.x, pos.y - ascent(px)),
        color,
        size: Pixels(px),
        font,
        align_x: alignment::Horizontal::Left.into(),
        align_y: alignment::Vertical::Top,
        // Advanced, not Basic: Basic disables font fallback (see pane_canvas.rs),
        // which would tofu any glyph the primary face lacks.
        shaping: text::Shaping::Advanced,
        ..CanvasText::default()
    });
}

/// The app crate's theme colour to an iced colour (opaque).
fn rgb(c: Rgb) -> Color {
    Color::from_rgb8(c.0, c.1, c.2)
}
