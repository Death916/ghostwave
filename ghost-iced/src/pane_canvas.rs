//! iced canvas painter for a tab of ghostwave terminal panes: background,
//! per-cell fg/bg, text, underline/strikethrough, cursor, then the pane frames
//! and their labels. Rects are canvas-local coordinates.

use ghostwave::app::TAB_BAR_H;
use ghostwave::term::CellDims;
use ghostwave::theme::Theme as GhostTheme;
use iced::widget::canvas::{self, Action, Event, Frame, Geometry, Text as CanvasText};
use iced::widget::text;
use iced::{Color, Font, Pixels, Point, Rectangle, Renderer, Size, Theme, alignment, mouse};

use libghostty_vt::render::{CursorVisualStyle, Dirty};
use libghostty_vt::style::{RgbColor, Underline};

use crate::Message;
use crate::chrome::{self, ChromeState, WaveParams};
use crate::fonts;
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

/// Paints the window chrome: tab bar, status bar, wave, toast and overlays.
pub struct TerminalCanvas {
    pub theme: GhostTheme,
    pub chrome: ChromeData,
}

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

/// Holds an owned snapshot of the chrome's state, turned into a borrowed
/// [`ChromeState`] at [`chrome::paint`] time.
pub struct ChromeData {
    /// (label, is_active), in tab order.
    pub tabs: Vec<(String, bool)>,
    pub mode_label: String,
    pub hints: String,
    pub theme_name: String,
    pub sync_panes: bool,
    pub wave: Option<WaveParams>,
    pub toast: Option<String>,
    /// `(what, buffer)` while a rename mode is active — "Tab" or "Pane", and the
    /// text so far.
    pub rename: Option<(String, String)>,
}

impl TerminalCanvas {
    /// Returns the tab whose `TAB_W` px chip contains `point.x`; `None` outside
    /// the bar or past the last tab.
    fn tab_at(&self, point: Point) -> Option<usize> {
        const TAB_W: f32 = 130.0;
        if point.y >= TAB_BAR_H || point.x < 0.0 {
            return None;
        }
        let index = (point.x / TAB_W) as usize;
        (index < self.chrome.tabs.len()).then_some(index)
    }
}

impl canvas::Program<Message> for TerminalCanvas {
    type State = CanvasState;

    fn update(
        &self,
        _state: &mut Self::State,
        event: &Event,
        _bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<Action<Message>> {
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                let point = cursor.position()?;
                if let Some(index) = self.tab_at(point) {
                    Some(Action::publish(Message::FocusTab(index)))
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    fn draw(
        &self,
        _state: &Self::State,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let font = fonts::primary();

        if let Some((what, buffer)) = &self.chrome.rename {
            chrome::paint_rename_prompt(&mut frame, bounds.size(), what, buffer, &self.theme, font);
        }
        chrome::paint(
            &mut frame,
            bounds.size(),
            font,
            &self.theme,
            &ChromeState {
                tabs: &self.chrome.tabs,
                mode_label: &self.chrome.mode_label,
                hints: &self.chrome.hints,
                theme_name: &self.chrome.theme_name,
                sync_panes: self.chrome.sync_panes,
                wave: self.chrome.wave,
                toast: self.chrome.toast.as_deref(),
            },
        );

        vec![frame.into_geometry()]
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
pub fn cell_label(
    pane: &SharedPane,
    index: usize,
    stack: Option<(usize, usize)>,
    replace: bool,
) -> String {
    let guard = pane.lock().unwrap();
    let title = guard.cached_title();
    format_pane_label(&guard.term.name, index, title, stack, replace)
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
    } = scratch;

    let cw = cell.w as f32;
    let ch = cell.h as f32;
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
        let mut run: Option<TextRun> = None;

        while let Some(c) = cells.next() {
            text_buf.clear();

            let mut fg = default_fg;
            let mut cell_bg: Option<Color> = None;
            let mut has_glyphs = false;

            if c.graphemes_len().unwrap_or(0) > 0 {
                let _ = c.graphemes_utf8(&mut text_buf);
                strip_ignorables(&mut text_buf);
                has_glyphs = !text_buf.is_empty();
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

            let selected = c.is_selected().unwrap_or(false);
            if selected {
                selected_cells += 1;
            }
            if term.search.contains(row_idx, col_idx) {
                frame.fill_rectangle(Point::new(x, y), Size::new(cw, ch), rgb(theme.search_match));
                fg = rgb(theme.bg);
            } else {
                if let Some(c_bg) = cell_bg {
                    frame.fill_rectangle(Point::new(x, y), Size::new(cw, ch), c_bg);
                }

                if selected {
                    frame.fill_rectangle(Point::new(x, y), Size::new(cw, ch), rgb(theme.selection));
                }
            }

            let baseline_y = y + cell.baseline;
            let extends = match &run {
                Some(r) => has_glyphs && r.color == fg && r.baseline_y == baseline_y,
                None => false,
            };
            if extends {
                if let Some(r) = run.as_mut() {
                    r.text.push_str(&text_buf);
                }
            } else {
                if let Some(r) = run.take() {
                    draw_run(frame, &r, font_px, font);
                }
                if has_glyphs {
                    run = Some(TextRun {
                        x,
                        baseline_y,
                        color: fg,
                        text: text_buf.clone(),
                    });
                }
            }

            if underline {
                frame.fill_rectangle(Point::new(x, y + ch - 1.0), Size::new(cw, 1.0), fg);
            }
            if strikethrough {
                frame.fill_rectangle(
                    Point::new(x, y + (ch / 2.0).floor()),
                    Size::new(cw, 1.0),
                    fg,
                );
            }

            x += cw;
            col_idx += 1;
        }

        if let Some(r) = run.take() {
            draw_run(frame, &r, font_px, font);
        }

        if term.search.row_has_match(row_idx) {
            frame.fill_rectangle(
                Point::new(ox - 3.0, y + 2.0),
                Size::new(2.0, ch - 4.0),
                rgb(theme.search_match),
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

        match style {
            CursorVisualStyle::Bar => {
                frame.fill_rectangle(Point::new(cx, cy), Size::new(2.0, ch), cursor_color)
            }
            CursorVisualStyle::Underline => {
                frame.fill_rectangle(Point::new(cx, cy + ch - 1.0), Size::new(cw, 1.0), cursor_color)
            }
            _ => frame.fill_rectangle(Point::new(cx, cy), Size::new(cw, ch), cursor_color),
        }
    }

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

/// A horizontal run of cells sharing one foreground colour, drawn as a single
/// text item so glyphs advance by the font's own advance instead of being pinned
/// to the rounded-up cell width.
struct TextRun {
    x: f32,
    baseline_y: f32,
    color: Color,
    text: String,
}


fn draw_run(frame: &mut Frame, run: &TextRun, font_px: f32, font: Font) {
    if run.text.is_empty() {
        return;
    }
    frame.fill_text(CanvasText {
        content: run.text.clone(),
        position: Point::new(run.x, run.baseline_y - crate::measure::ascent(font_px)),
        color: run.color,
        size: Pixels(font_px),
        font,
        align_x: alignment::Horizontal::Left.into(),
        align_y: alignment::Vertical::Top,
        shaping: text::Shaping::Advanced,
        ..CanvasText::default()
    });
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
