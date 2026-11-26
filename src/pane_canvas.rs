//! iced canvas painter for a tab of ghostwave terminal panes: background,
//! per-cell fg/bg, text, underline/strikethrough, cursor, then the pane frames
//! and their labels. Rects are canvas-local coordinates.

use ghostwave::term::{CellDims, PlacementBox, ReconnectState, placement_boxes};
use ghostwave::theme::Theme as GhostTheme;
use iced::widget::canvas::{Frame, Text as CanvasText};
use iced::widget::text;
use iced::{Color, Font, Pixels, Point, Rectangle, Size, alignment, mouse};

use libghostty_vt::kitty::graphics;
use libghostty_vt::render::{CursorVisualStyle, Dirty};
use libghostty_vt::style::{RgbColor, Underline};

use crate::pane::{Pane, SharedPane};

use libghostty_vt::mouse as vt_mouse;

/// Label font size in px; the pane-grid layer's title band uses it too.
pub(crate) const LABEL_PX: f32 = 13.0;
/// Left/right inset of the label text inside its band, in px.
pub(crate) const LABEL_PAD: f32 = 8.0;
/// Width in px of the focused pane's frame stroke.
pub(crate) const FRAME_ACTIVE_W: f32 = 2.0;
/// Width in px of a frame stroke on a pane that is not focused.
pub(crate) const FRAME_INACTIVE_W: f32 = 1.0;
/// Alpha of the selection band drawn over a selected cell.
const BAND_ALPHA: f32 = 0.7;
/// The glyph every block of colour is built from.
///
/// Cell backgrounds, the selection band, search matches and the cursor are all
/// drawn as text rather than as filled rectangles: a pane's filled rectangles do
/// not reach the screen in every pane, while its glyphs do.
const BLOCK_GLYPH: char = '\u{2588}';
/// The glyph for a search row's left-edge marker.
const MARKER_GLYPH: char = '\u{258F}';
/// The glyph for a bar cursor.
const BAR_GLYPH: char = '\u{258F}';
/// The glyph for an underline cursor and for underlined cells.
const UNDERLINE_GLYPH: char = '\u{2581}';
/// The glyph for struck-through cells.
const STRIKE_GLYPH: char = '\u{2500}';

/// Accumulates the `any_pressed` / `button_down` fields of [`MouseEv`] from the
/// event stream, which is what the app crate's mouse encoder reads.
#[derive(Default)]
pub struct CanvasState {
    /// Buttons currently held, newest last (`button_down` is the most recent
    /// press).
    pressed: Vec<mouse::Button>,
}

impl CanvasState {
    /// Records a press; the newest press is the one `button_down` reports.
    pub(crate) fn press(&mut self, button: mouse::Button) {
        self.pressed.push(button);
    }

    /// Records a release.
    pub(crate) fn release(&mut self, button: mouse::Button) {
        self.pressed.retain(|b| *b != button);
    }

    /// Whether any button is held.
    pub(crate) fn any_pressed(&self) -> bool {
        !self.pressed.is_empty()
    }

    pub(crate) fn button_down(&self) -> Option<vt_mouse::Button> {
        self.pressed.last().copied().and_then(vt_button)
    }
}

/// Only the three real buttons exist in both; the wheel arrives as
/// [`MouseEvKind::Wheel`].
pub(crate) fn vt_button(button: mouse::Button) -> Option<vt_mouse::Button> {
    match button {
        mouse::Button::Left => Some(vt_mouse::Button::Left),
        mouse::Button::Right => Some(vt_mouse::Button::Right),
        mouse::Button::Middle => Some(vt_mouse::Button::Middle),
        _ => None,
    }
}

/// Formats a pane or stack cell label with 1-based index and optional stack info.
pub fn format_pane_label(
    name: &str,
    index: usize,
    title: &str,
    stack: Option<(usize, usize)>,
    replace: bool,
) -> String {
    let base_name = if name.is_empty() { "pane" } else { name };
    let stack_suffix = match stack {
        Some((active, total)) if total > 1 => format!(" [{}/{}]", active + 1, total),
        _ => String::new(),
    };
    if title.is_empty() {
        format!("{base_name} {index}{stack_suffix}")
    } else if replace {
        format!("{title}{stack_suffix}")
    } else {
        format!("{base_name} {index}{stack_suffix} — {title}")
    }
}

/// Returns a cell's label string from its pane handle, 1-based position and optional stack info.
///
/// A remote pane's label also carries its connection state: a disconnected pane
/// shows no output of its own, so its label band is where it asks to be connected.
pub fn cell_label(
    pane: &SharedPane,
    index: usize,
    stack: Option<(usize, usize)>,
    replace: bool,
) -> String {
    let guard = pane.lock().unwrap();
    let label = format_pane_label(&guard.term.name, index, guard.cached_title(), stack, replace);
    match remote_status(&guard) {
        Some(status) => format!("{label} — {status}"),
        None => label,
    }
}

/// The connection state a remote pane shows in its label band; `None` for a pane
/// that is not remote and for one that is connected.
fn remote_status(pane: &Pane) -> Option<String> {
    let remote = pane.term.remote.as_ref()?;
    match (&pane.term.reconnect, pane.term.exited.is_some()) {
        (ReconnectState::Waiting { attempt, .. }, _) => Some(format!(
            "ssh {} — reconnecting (attempt {attempt}) — Esc to stop",
            remote.host
        )),
        (_, true) => Some(format!(
            "ssh {} — disconnected (Enter connect | k kill)",
            remote.host
        )),
        _ => None,
    }
}

/// The x a search row's gutter marker is drawn at: one cell left of the text
/// origin, but never outside the canvas.
///
/// The gutter is only `padding` wide, so a cell wider than the gutter would
/// otherwise put the marker's ink past the canvas edge and onto the pane's own
/// frame stroke. Clamping keeps the marker visible and the stroke intact.
fn marker_x(padding: f32, cell_w: f32) -> f32 {
    (padding - cell_w).max(0.0)
}

/// The canvas-space rectangle a placement's image covers.
///
/// A placement's geometry is anchored at the terminal's text origin, i.e. the
/// pane rect inset by `padding` — carrying that inset here is what keeps image
/// ink off the pane's frame stroke. Takes and returns plain numbers so the pass
/// can be checked without a renderer.
pub(crate) fn placement_rect(
    b: &PlacementBox,
    origin: (f32, f32),
    padding: f32,
) -> (f32, f32, f32, f32) {
    (origin.0 + padding + b.x, origin.1 + padding + b.y, b.w, b.h)
}

/// A pane's rect plus the padding its frame reserves.
///
/// The image pass is clipped to `bounds` inset by `padding`, which is what keeps
/// image ink off the pane's stroke no matter where a placement sits.
#[derive(Clone, Copy)]
pub(crate) struct PaneBody {
    pub bounds: Rectangle,
    pub padding: f32,
}

impl PaneBody {
    /// The part of the canvas an image may cover.
    fn clip(self) -> Rectangle {
        let pad = self.padding;
        Rectangle::new(
            Point::new(self.bounds.x + pad, self.bounds.y + pad),
            Size::new(
                (self.bounds.width - 2.0 * pad).max(0.0),
                (self.bounds.height - 2.0 * pad).max(0.0),
            ),
        )
    }
}

/// Paints the placements one layer at a time, clipped to the pane body.
///
/// Kitty puts a placement either below the background, between background and
/// text, or above text, and the pane has to paint the matching group before or
/// after its cells to preserve that order.
fn paint_placement_layer(
    frame: &mut Frame,
    images: &mut crate::pane::ImageCache,
    term: &libghostty_vt::Terminal<'_, '_>,
    boxes: &[PlacementBox],
    body: PaneBody,
    layer: graphics::Layer,
) {
    let clip = body.clip();
    let origin = (body.bounds.x, body.bounds.y);
    frame.with_clip(clip, |f| {
        for b in boxes.iter().filter(|b| b.layer == layer) {
            let Some(handle) = images.handle(term, b) else {
                continue;
            };
            let (x, y, w, h) = placement_rect(b, origin, body.padding);
            f.draw_image(Rectangle::new(Point::new(x, y), Size::new(w, h)), &handle);
        }
    });
}

/// Paints one pane's terminal content into `rect`, in frame-local coordinates.
pub(crate) fn paint_pane(
    frame: &mut Frame,
    pane: &mut Pane,
    rect: Rectangle,
    cell: CellDims,
    padding: f32,
    font_px: f32,
    font: Font,
    theme: &GhostTheme,
) {
    let pane_id = pane.id;
    let debug = std::env::var_os("GHOST_ICED_DEBUG").is_some();
    let Pane {
        term, scratch, ..
    } = pane;
    let mut selected_cells = 0usize;
    let crate::pane::RenderScratch {
        render_state,
        row_it,
        cell_it,
        images,
    } = scratch;

    let cw = cell.w;
    let ch = cell.h;
    let ox = rect.x + padding;
    let oy = rect.y + padding;

    let snapshot = match render_state.update(&term.term) {
        Ok(s) => s,
        Err(_) => return,
    };
    let colors = match snapshot.colors() {
        Ok(c) => c,
        Err(_) => return,
    };

    let default_bg = rgb(theme.bg);
    let default_fg = to_color(colors.foreground);
    frame.fill_rectangle(
        Point::new(rect.x, rect.y),
        Size::new(rect.width, rect.height),
        default_bg,
    );

    // Kitty images: the two lower layers go under the cells, AboveText goes over
    // them. Everything is clipped to the pane body, so a placement can never
    // paint on the pane's frame stroke.
    let boxes = placement_boxes(&term.term, cell);
    let body = PaneBody { bounds: rect, padding };
    for layer in [graphics::Layer::BelowBg, graphics::Layer::BelowText] {
        paint_placement_layer(frame, images, &term.term, &boxes, body, layer);
    }

    let mut rows = match row_it.update(&snapshot) {
        Ok(it) => it,
        Err(_) => return,
    };

    let mut y = oy;
    let mut row_idx: u32 = 0;
    let mut text_buf = String::with_capacity(128);

    while let Some(row) = rows.next() {
        let mut x = ox;
        let mut col_idx: u16 = 0;
        let mut cells = match cell_it.update(row) {
            Ok(it) => it,
            Err(_) => continue,
        };
        let sel_range = row
            .selection()
            .ok()
            .flatten()
            .map(|s| (s.start_x.min(s.end_x), s.start_x.max(s.end_x)));
        let band = Color {
            a: BAND_ALPHA,
            ..selection_band(theme, default_bg, default_fg)
        };
        let match_band = rgb(theme.search_match);

        while let Some(c) = cells.next() {
            text_buf.clear();

            let mut fg = default_fg;
            let mut cell_bg: Option<Color> = None;
            let mut has_glyphs = false;

            if c.graphemes_len().unwrap_or(0) > 0 {
                let _ = c.graphemes_utf8(&mut text_buf);
                strip_ignorables(&mut text_buf);
                has_glyphs = !text_buf.trim().is_empty();
                if let Ok(Some(fg_c)) = c.fg_color() {
                    fg = to_color(fg_c);
                }
            }

            if let Ok(Some(bg_c)) = c.bg_color() {
                let c_bg = to_color(bg_c);
                if c_bg != default_bg {
                    cell_bg = Some(c_bg);
                }
            }

            let mut underline = false;
            let mut strikethrough = false;
            if c.has_styling().unwrap_or(false)
                && let Ok(style) = c.style()
            {
                if style.inverse {
                    let new_bg = fg;
                    fg = cell_bg.unwrap_or(default_bg);
                    cell_bg = if new_bg == default_bg {
                        None
                    } else {
                        Some(new_bg)
                    };
                }
                underline = style.underline != Underline::None;
                strikethrough = style.strikethrough;
            }

            let selected = sel_range.is_some_and(|(c0, c1)| col_idx >= c0 && col_idx <= c1);
            let matched = term.search.contains(row_idx, col_idx);
            if selected {
                selected_cells += 1;
            }

            let baseline_y = y + cell.baseline;
            if matched {
                draw_glyph(frame, BLOCK_GLYPH, x, baseline_y, match_band, font_px, font);
                fg = rgb(theme.bg);
            } else if selected {
                draw_glyph(frame, BLOCK_GLYPH, x, baseline_y, band, font_px, font);
            } else if let Some(c_bg) = cell_bg {
                draw_glyph(frame, BLOCK_GLYPH, x, baseline_y, c_bg, font_px, font);
            }

            if underline {
                draw_glyph(frame, UNDERLINE_GLYPH, x, baseline_y, fg, font_px, font);
            }
            if strikethrough {
                draw_glyph(frame, STRIKE_GLYPH, x, baseline_y, fg, font_px, font);
            }

            if has_glyphs {
                draw_str(frame, &text_buf, x, baseline_y, fg, font_px, font);
            }

            x += cw;
            col_idx += 1;
        }

        if term.search.row_has_match(row_idx) {
            draw_glyph(
                frame,
                MARKER_GLYPH,
                rect.x + marker_x(padding, cw),
                y + cell.baseline,
                match_band,
                font_px,
                font,
            );
        }

        let _ = row.set_dirty(false);
        y += ch;
        row_idx += 1;
    }

    if snapshot.cursor_visible().unwrap_or(false)
        && let Ok(Some(vp)) = snapshot.cursor_viewport()
    {
        let cx = ox + vp.x as f32 * cw;
        let cy = oy + vp.y as f32 * ch;
        let cursor_color = colors.cursor.map(to_color).unwrap_or(default_fg);
        let style = snapshot
            .cursor_visual_style()
            .unwrap_or(CursorVisualStyle::Block);

        let glyph = match style {
            CursorVisualStyle::Bar => BAR_GLYPH,
            CursorVisualStyle::Underline => UNDERLINE_GLYPH,
            _ => BLOCK_GLYPH,
        };
        draw_glyph(
            frame,
            glyph,
            cx,
            cy + cell.baseline,
            cursor_color,
            font_px,
            font,
        );
    }

    // Placements in the top layer belong over the cells; see the note on the
    // lower layers above.
    paint_placement_layer(
        frame,
        images,
        &term.term,
        &boxes,
        body,
        graphics::Layer::AboveText,
    );

    if debug {
        eprintln!(
            "[paint] pane={pane_id:?} rect=({:.1},{:.1} {:.1}x{:.1}) selected_cells={selected_cells} matches={}",
            rect.x,
            rect.y,
            rect.width,
            rect.height,
            term.search.matches.len()
        );
    }

    let _ = snapshot.set_dirty(Dirty::Clean);
}

fn draw_glyph(
    frame: &mut Frame,
    glyph: char,
    x: f32,
    baseline_y: f32,
    color: Color,
    font_px: f32,
    font: Font,
) {
    let mut buf = [0u8; 4];
    let s = glyph.encode_utf8(&mut buf);
    draw_str(frame, s, x, baseline_y, color, font_px, font);
}

fn draw_str(
    frame: &mut Frame,
    text: &str,
    x: f32,
    baseline_y: f32,
    color: Color,
    font_px: f32,
    font: Font,
) {
    if text.is_empty() {
        return;
    }
    frame.fill_text(CanvasText {
        content: text.to_string(),
        position: Point::new(x, baseline_y - crate::measure::ascent(font_px)),
        color,
        size: Pixels(font_px),
        font,
        align_x: alignment::Horizontal::Left.into(),
        align_y: alignment::Vertical::Top,
        shaping: text::Shaping::Advanced,
        ..CanvasText::default()
    });
}

/// Perceived-luminance gap between two colours, in `0.0..=1.0`.
fn luma_gap(a: Color, b: Color) -> f32 {
    let luma = |c: Color| 0.2126 * c.r + 0.7152 * c.g + 0.0722 * c.b;
    (luma(a) - luma(b)).abs()
}

/// The band painted behind selected cells.
///
/// The theme's `selection` colour is used when it reads against the pane's
/// background. Themes whose selection colour sits almost on top of their
/// background (tokyonight: `#283457` on `#1a1b26`) would give a highlight that
/// is invisible on screen, so the band is mixed toward the foreground until it
/// is legible.
fn selection_band(theme: &GhostTheme, pane_bg: Color, ink: Color) -> Color {
    let band = rgb(theme.selection);
    if luma_gap(band, pane_bg) >= 0.15 {
        return band;
    }
    let mix = 0.35;
    Color {
        r: band.r * (1.0 - mix) + ink.r * mix,
        g: band.g * (1.0 - mix) + ink.g * mix,
        b: band.b * (1.0 - mix) + ink.b * mix,
        a: 1.0,
    }
}

/// Converts a libghostty [`RgbColor`] to an opaque iced [`Color`].
fn to_color(c: RgbColor) -> Color {
    Color::from_rgb8(c.r, c.g, c.b)
}

/// Converts the app crate's theme colour to an opaque iced [`Color`].
pub(crate) fn rgb(c: ghostwave::theme::Rgb) -> Color {
    Color::from_rgb8(c.0, c.1, c.2)
}

/// Drops zero-width / join-control codepoints (variation selectors, ZWSP/ZWJ,
/// word joiner, BOM) from a cell's grapheme cluster: they carry no ink and
/// cosmic-text would advance the cursor for some of them.
fn strip_ignorables(s: &mut String) {
    s.retain(|ch| {
        !matches!(
            ch,
            '\u{FE00}'..='\u{FE0F}' | '\u{200B}'..='\u{200D}' | '\u{2060}' | '\u{FEFF}'
        )
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use ghostwave::term::PlacementBox;
    use libghostty_vt::kitty::graphics::Layer;

    /// The image pass draws inside the pane body, so its ink cannot reach the
    /// frame stroke.
    ///
    /// `paint_pane` gives the pass the pane's rect and the padding the frame
    /// reserves, and a placement's own geometry starts at the text origin — so
    /// the destination has to carry that inset. RED first: `PlacementBox` and
    /// the pass did not exist when this test was written.
    #[test]
    fn an_image_is_drawn_inside_the_pane_body() {
        let pane = (40.0, 12.0);
        let (pane_w, pane_h) = (300.0, 200.0);
        let padding = 6.0;
        let b = PlacementBox {
            image_id: 1,
            x: 0.0,
            y: 0.0,
            w: 20.0,
            h: 10.0,
            source: (0, 0, 20, 10),
            z: 0,
            layer: Layer::AboveText,
        };

        let (x, y, w, h) = placement_rect(&b, pane, padding);
        assert!(
            x >= pane.0 + padding,
            "image ink at x={x} starts left of the body inset {}",
            pane.0 + padding
        );
        assert!(
            y >= pane.1 + padding,
            "image ink at y={y} starts above the body inset {}",
            pane.1 + padding
        );
        assert!(
            x + w <= pane.0 + pane_w - padding,
            "image ink ends past the body's right edge"
        );
        assert!(
            y + h <= pane.1 + pane_h - padding,
            "image ink ends past the body's bottom edge"
        );

        // A placement scrolled partly off the top keeps the same origin: the
        // negative offset shifts it, it does not drop the inset.
        let scrolled = PlacementBox {
            x: -3.0,
            y: -5.0,
            ..b
        };
        let (sx, sy, ..) = placement_rect(&scrolled, pane, padding);
        assert_eq!(sx, pane.0 + padding - 3.0, "the body inset still applies");
        assert_eq!(sy, pane.1 + padding - 5.0, "the body inset still applies");
    }

    /// The marker's ink stays on the canvas, whatever the font makes a cell.
    ///
    /// It is drawn one cell left of the text origin and the gutter is only
    /// `padding` wide, so from `cell_w > padding` on that position is outside
    /// the canvas; from `cell_w > padding + inset` (font_size >= 17 at the
    /// defaults) the ink reaches the pane's frame stroke. The x is canvas-local,
    /// so `0.0` is the canvas edge.
    #[test]
    fn search_marker_stays_inside_the_canvas() {
        // (padding, cell_w) -> where the ink starts, canvas-local.
        let cases: [(f32, f32, f32); 5] = [
            (6.0, 7.2, 0.0),
            (6.0, 9.6, 0.0),
            (6.0, 10.8, 0.0),
            (6.0, 20.0, 0.0),
            (12.0, 7.2, 4.8),
        ];
        for (padding, cell_w, want) in cases {
            let x = marker_x(padding, cell_w);
            assert!(
                (x - want).abs() < 1e-4,
                "marker_x({padding}, {cell_w}) = {x}, want {want}"
            );
            assert!(
                x >= 0.0,
                "marker_x({padding}, {cell_w}) = {x}, left of the canvas edge"
            );
        }

        // No font at the default padding puts the ink past the canvas either.
        let mut cell_w = 1.0;
        while cell_w <= 32.0 {
            let x = marker_x(6.0, cell_w);
            assert!(
                x >= 0.0,
                "marker_x(6.0, {cell_w}) = {x}, left of the canvas edge"
            );
            assert!(
                x + cell_w / 8.0 <= 6.0 + cell_w,
                "marker ink at cell_w {cell_w} runs past the canvas: {x} + {}",
                cell_w / 8.0
            );
            cell_w += 0.125;
        }
    }
}
