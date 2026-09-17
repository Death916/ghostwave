//! Chrome painter for the iced front-end: tab bar, status bar (mode label,
//! hints, theme name, sync chip, ghostwave visualizer) and toast, drawn in that
//! order back to front after the panes so the chrome sits on top of them.

use ghostwave::app::{STATUS_BAR_H, TAB_BAR_H};
use ghostwave::theme::Rgb;
use ghostwave::theme::Theme as GhostTheme;
use iced::widget::canvas::{Frame, Path, Stroke, Text as CanvasText};
use iced::widget::text;
use iced::{alignment, Color, Font, Pixels, Point, Size};

use crate::measure::{ascent, text_width};

/// Chrome sizes do not scale with `config.font_size`.
const TAB_LABEL_PX: f32 = 14.0;
const STATUS_PX: f32 = 13.0;

/// Tab pitch in px: chip `i` starts at `x = i * TAB_W`, unclamped by the window
/// width, so tabs past the right edge are simply off-screen (no wrap, scroll or
/// ellipsis).
const TAB_W: f32 = 130.0;
/// Active-chip width is `TAB_W - TAB_CHIP_INSET`; the untouched right gap of
/// `tab_bg` reads as the separator. No underline, no border.
const TAB_CHIP_INSET: f32 = 4.0;
/// Tab label x offset from the chip's left edge, in px.
const TAB_LABEL_X: f32 = 10.0;
/// Tab label baseline offset from the tab bar's vertical centre, in px.
const TAB_LABEL_BASELINE_DY: f32 = 4.0;

const MODE_PAD: f32 = 12.0;
const MODE_TEXT_X: f32 = 6.0;
/// Gap between the mode-label box and the hints text, in px.
const HINTS_DX: f32 = 10.0;
const THEME_PAD: f32 = 12.0;
const SYNC_PAD: f32 = 8.0;
/// Sync-chip vertical inset from the status bar's top and bottom edges, in px.
const SYNC_INSET_Y: f32 = 2.0;
/// Sync-chip text x offset from the chip's left edge, in px.
const SYNC_TEXT_DX: f32 = 4.0;
const SYNC_LABEL: &str = " [SYNC] ";

/// Status text baseline inset from the window's bottom edge, in px.
const STATUS_BASELINE_INSET: f32 = 8.0;
/// Clearance the wave keeps from the hints and from the right-hand status
/// items, in px.
const WAVE_MARGIN: f32 = 24.0;
/// The wave is skipped unless it has this much width, in px.
const WAVE_MIN_W: f32 = 50.0;
const WAVE_STEP: f32 = 3.0;

/// Toast box horizontal pad, in px; the label itself already carries a leading
/// and a trailing space.
const TOAST_PAD: f32 = 16.0;
/// Toast box x from the window's left edge, in px.
const TOAST_X: f32 = 8.0;
const TOAST_H: f32 = 24.0;
/// Gap between the toast's bottom edge and the status bar, in px.
const TOAST_BOTTOM_GAP: f32 = 6.0;
const TOAST_TEXT_X: f32 = 16.0;

const RENAME_PAD: f32 = 24.0;
/// Gap between the tab bar and the top of the rename box, in px.
const RENAME_GAP_Y: f32 = 6.0;
const RENAME_H: f32 = 30.0;
/// Rename text x offset from the box's left edge, in px.
const RENAME_TEXT_X: f32 = 12.0;
/// Rename text baseline offset from the tab bar's bottom edge, in px.
const RENAME_BASELINE_DY: f32 = 26.0;

/// Drives the status-bar wave; `None` in [`ChromeState::wave`] disables it.
#[derive(Clone, Copy, Debug)]
pub struct WaveParams {
    /// Wave energy, expected between 0.1 and 2.5. Drives the amplitude and,
    /// through `min(energy / 2.0, 1.0)`, the stroke alpha.
    pub energy: f32,
    /// Phase in radians; the caller advances and wraps it.
    pub phase: f32,
    /// Amplitude scale for the ribbons (1.0 is the default):
    /// `max_amp = sh * 0.35 * amplitude`, clamped to 2..=10 px.
    pub amplitude: f32,
}

/// Holds everything [`paint`] needs to draw one frame.
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

/// Paints the tab bar, status bar (with the wave) and toast. Called LAST, after
/// the panes and their frames, so the chrome sits on top of them; `window` is
/// the whole window size in logical px.
pub fn paint(
    frame: &mut Frame,
    window: Size,
    font: Font,
    theme: &GhostTheme,
    state: &ChromeState<'_>,
) {
    let (w, h) = (window.width, window.height);
    let sh = STATUS_BAR_H;

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

fn paint_tab_bar(
    frame: &mut Frame,
    w: f32,
    font: Font,
    theme: &GhostTheme,
    state: &ChromeState<'_>,
) {
    frame.fill_rectangle(Point::ORIGIN, Size::new(w, TAB_BAR_H), rgb(theme.tab_bg));

    for (i, (label, active)) in state.tabs.iter().enumerate() {
        let x = i as f32 * TAB_W;
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

    frame.fill_rectangle(Point::new(0.0, top), Size::new(w, sh), rgb(theme.status_bg));

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

    draw_text(
        frame,
        state.hints.to_string(),
        Point::new(geom.mode_w + HINTS_DX, h - STATUS_BASELINE_INSET),
        STATUS_PX,
        rgb(theme.status_fg),
        font,
    );

    draw_text(
        frame,
        format!(" {} ", state.theme_name),
        Point::new(w - geom.theme_w, h - STATUS_BASELINE_INSET),
        STATUS_PX,
        rgb(theme.status_fg),
        font,
    );

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
/// between consecutive samples, H1 in full before H2. The ribbon is unclipped
/// and has no background of its own, so it may overflow the status bar.
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
    let hints_w = text_width(state.hints, STATUS_PX);
    let start_x = geom.mode_w + HINTS_DX + hints_w + WAVE_MARGIN;
    let end_x = w - geom.theme_w - if geom.sync { geom.sync_w } else { 0.0 } - WAVE_MARGIN;

    if end_x <= start_x + WAVE_MIN_W {
        return;
    }

    let wave_w = end_x - start_x;
    let center_y = h - sh / 2.0;
    let max_amp = (sh * 0.35 * wave.amplitude).clamp(2.0, 10.0);
    let num_points = (wave_w / WAVE_STEP).max(10.0) as usize;
    let dt = 1.0 / num_points as f32;

    let envelope = |progress: f32| (progress * std::f32::consts::PI).sin();
    let x_at = |progress: f32| start_x + progress * wave_w;

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

/// Butt caps (the `Stroke` default); the colour's alpha is used as given, not
/// premultiplied.
fn stroke_segment(frame: &mut Frame, from: Point, to: Point, width: f32, color: Color) {
    let path = Path::new(|p| {
        p.move_to(from);
        p.line_to(to);
    });
    frame.stroke(&path, Stroke::default().with_color(color).with_width(width));
}

/// Bottom-left toast box: `TOAST_X` from the left edge, its bottom edge
/// `TOAST_BOTTOM_GAP` above the status bar, growing rightwards. One at a time,
/// no border, no icon.
fn paint_toast(frame: &mut Frame, h: f32, sh: f32, font: Font, theme: &GhostTheme, msg: &str) {
    let label = format!(" {msg} ");
    let lw = text_width(&label, STATUS_PX) + TOAST_PAD;
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

/// Paints the tab/pane rename input box, centred `RENAME_GAP_Y` under the tab
/// bar.
///
/// `what` is `"Tab"` or `"Pane"` and `buffer` is the draft name; the label adds
/// the leading space and the literal `_` caret. There is no width cap — the box
/// simply grows past the window edges with the text — so the caller decides
/// whether to paint it at all.
pub fn paint_rename_prompt(
    frame: &mut iced::widget::canvas::Frame,
    window: iced::Size,
    what: &str,
    buffer: &str,
    theme: &GhostTheme,
    font: iced::Font,
) {
    let label = format!(" rename {what}: {buffer}_");
    let lw = text_width(&label, TAB_LABEL_PX) + RENAME_PAD;
    let bx = (window.width - lw) / 2.0;
    let box_y = TAB_BAR_H + RENAME_GAP_Y;

    frame.fill_rectangle(
        Point::new(bx, box_y),
        Size::new(lw, RENAME_H),
        rgb(theme.palette_bg),
    );
    frame.stroke_rectangle(
        Point::new(bx, box_y),
        Size::new(lw, RENAME_H),
        Stroke::default()
            .with_color(rgb(theme.frame_active))
            .with_width(1.0),
    );
    draw_text(
        frame,
        label,
        Point::new(bx + RENAME_TEXT_X, TAB_BAR_H + RENAME_BASELINE_DY),
        TAB_LABEL_PX,
        rgb(theme.palette_fg),
        font,
    );
}

/// Draws one chrome string at `pos`, given as `(x, baseline)`; the baseline is
/// converted to iced's top-left text origin here.
fn draw_text(frame: &mut Frame, content: String, pos: Point, px: f32, color: Color, font: Font) {
    frame.fill_text(CanvasText {
        content,
        position: Point::new(pos.x, pos.y - ascent(px)),
        color,
        size: Pixels(px),
        font,
        align_x: alignment::Horizontal::Left.into(),
        align_y: alignment::Vertical::Top,
        shaping: text::Shaping::Advanced,
        ..CanvasText::default()
    });
}

/// Converts the app crate's theme colour to an opaque iced [`Color`].
fn rgb(c: Rgb) -> Color {
    Color::from_rgb8(c.0, c.1, c.2)
}
