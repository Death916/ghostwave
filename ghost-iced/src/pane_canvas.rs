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
use crate::keys;
use crate::pane::{Pane, SharedPane};

use ghostwave::term::{MouseEv, MouseEvKind};
use libghostty_vt::mouse as vt_mouse;

const LABEL_PX: f32 = 13.0;
/// Advance of the label face at `LABEL_PX`, in px. Used only to size the label's
/// background band, since the canvas cannot measure text.
const LABEL_ADVANCE: f32 = 7.8;
const LABEL_PAD: f32 = 8.0;
const FRAME_ACTIVE_W: f32 = 2.0;
const FRAME_INACTIVE_W: f32 = 1.0;
/// Label band height in px above a pane rect.
const LABEL_H: f32 = 14.0;

/// Paints one tab's terminal panes, their frames and the chrome.
pub struct TerminalCanvas {
    /// Tab these panes belong to, so mouse events can be tagged.
    pub tab: usize,
    pub panes: Vec<(usize, SharedPane)>,
    /// Pane rects (the active tab's), in canvas-local coordinates.
    pub rects: Vec<(usize, Rectangle)>,
    pub active: usize,
    pub cell: CellDims,
    pub font_px: f32,
    pub padding: f32,
    pub show_frames: bool,
    pub theme: GhostTheme,
    /// Content area (window minus the tab bar and status bar), in canvas
    /// coordinates. Pane frames and labels are clipped to it: iced renders every
    /// text item above every shape in a `Frame`, so without the clip a top-row
    /// pane label would float over the tab bar instead of being covered by it.
    pub content: Rectangle,
    /// Panes whose application currently tracks the mouse.
    ///
    /// Cursor motion is delivered by iced continuously; forwarding every move
    /// would mean locking every pane on every pixel, so motion is only handed to
    /// a pane that either has a button down or is tracking the mouse. The list is
    /// snapshotted when the view is rebuilt.
    pub mouse_tracking: Vec<usize>,
    /// Everything the chrome needs, owned: the canvas is rebuilt every view.
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
    fn button_down(&self) -> Option<vt_mouse::Button> {
        self.pressed.last().copied().and_then(vt_button)
    }
}

/// Only the three real buttons exist in both; the wheel arrives as
/// [`MouseEvKind::Wheel`].
fn vt_button(button: mouse::Button) -> Option<vt_mouse::Button> {
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
        state: &mut Self::State,
        event: &Event,
        _bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<Action<Message>> {
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(button)) => {
                let point = cursor.position()?;
                // The tab bar is drawn over the panes, so it gets first claim on
                // a press.
                if let Some(index) = self.tab_at(point) {
                    return Some(Action::publish(Message::FocusTab(index)));
                }
                let (pane, rect) = self.pane_at(point)?;
                let pressed = vt_button(*button)?;
                // Track the iced button, so release matching and the vt mapping
                // stay one list.
                state.pressed.push(*button);
                Some(Action::publish(self.mouse_message(
                    pane,
                    rect,
                    point,
                    MouseEvKind::Press(pressed),
                    state,
                )))
            }
            Event::Mouse(mouse::Event::ButtonReleased(button)) => {
                let point = cursor.position()?;
                let (pane, rect) = self.pane_at(point)?;
                let released = vt_button(*button)?;
                state.pressed.retain(|b| b != button);
                Some(Action::publish(self.mouse_message(
                    pane,
                    rect,
                    point,
                    MouseEvKind::Release(released),
                    state,
                )))
            }
            // Motion is the hot path: iced reports every pixel, so it only goes
            // to a pane that is tracking the mouse or has a button down.
            Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                let point = cursor.position()?;
                let (pane, rect) = self.pane_at(point)?;
                if state.pressed.is_empty() && !self.mouse_tracking.contains(&pane) {
                    return None;
                }
                Some(Action::publish(self.mouse_message(
                    pane,
                    rect,
                    point,
                    MouseEvKind::Motion,
                    state,
                )))
            }
            Event::Mouse(mouse::Event::WheelScrolled { delta }) => {
                let point = cursor.position()?;
                let (pane, rect) = self.pane_at(point)?;
                // libghostty takes the wheel as a positive-up delta and decides
                // for itself whether that is application mouse reporting or a
                // viewport scroll; the app's own scale is one notch per line, so
                // pixel deltas are divided down.
                let dy = match delta {
                    mouse::ScrollDelta::Lines { y, .. } => *y,
                    mouse::ScrollDelta::Pixels { y, .. } => *y / 53.0,
                };
                if dy == 0.0 {
                    return None;
                }
                Some(Action::publish(self.mouse_message(
                    pane,
                    rect,
                    point,
                    MouseEvKind::Wheel(dy),
                    state,
                )))
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

        // Window background: it shows through everywhere the panes do not cover,
        // including the frame gutters. It is drawn inside its own region because
        // a `Frame`'s own primitives are flushed after every draft, so a direct
        // fill here would paint over the panes and their frames; `with_clip` is
        // the only way to draw beneath them.
        frame.with_clip(bounds, |f| {
            f.fill_rectangle(
                Point::new(bounds.x, bounds.y),
                Size::new(bounds.width, bounds.height),
                rgb(self.theme.bg),
            );
        });

        for (id, rect) in &self.rects {
            let Some(pane) = self.panes.iter().find(|(p, _)| p == id).map(|(_, p)| p) else {
                continue;
            };
            let mut guard = pane.lock().unwrap();
            let pane: &mut Pane = &mut guard;
            let rect = *rect;
            let cell = self.cell;
            let padding = self.padding;
            let font_px = self.font_px;
            frame.with_clip(rect, |f| {
                paint_pane(f, pane, rect, cell, padding, font_px, font);
            });
            guard.dirty = false;
        }

        if self.show_frames {
            // Clipped to the content area, for the reason given on that field.
            let content = self.content;
            let rects = self.rects.clone();
            let active = self.active;
            let theme = self.theme;
            let labels: Vec<String> = rects.iter().map(|(id, _)| self.label_for(*id)).collect();
            frame.with_clip(content, |f| {
                for ((id, rect), label) in rects.iter().zip(labels.iter()) {
                    paint_frame(f, *rect, *id == active, &theme, label, font);
                }
            });
        }

        // Chrome last, so it paints over the pane content; the rename prompt is
        // drawn with it, geometrically clear of both bars.
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

impl TerminalCanvas {
    /// Returns the pane id and rect under `point`, in canvas-local coordinates.
    fn pane_at(&self, point: Point) -> Option<(usize, Rectangle)> {
        self.rects
            .iter()
            .find(|(_, rect)| rect.contains(point))
            .map(|(id, rect)| (*id, *rect))
    }

    fn mouse_message(
        &self,
        pane: usize,
        rect: Rectangle,
        point: Point,
        kind: MouseEvKind,
        state: &CanvasState,
    ) -> Message {
        Message::Mouse {
            tab: self.tab,
            pane,
            // A click focuses what it lands on, and the same event is still fed
            // to the pane under the cursor.
            focus: pane != self.active,
            ev: MouseEv {
                kind,
                // Pane-local logical px, which is what the mouse encoder takes.
                x: point.x - rect.x,
                y: point.y - rect.y,
                any_pressed: !state.pressed.is_empty(),
                button_down: state.button_down(),
            },
            mods: keys::current_mods(),
        }
    }

    /// Returns the pane label as `name index — title`, with the title omitted
    /// when empty.
    fn label_for(&self, id: usize) -> String {
        let Some(pane) = self.panes.iter().find(|(p, _)| *p == id).map(|(_, p)| p) else {
            return String::new();
        };
        let guard = pane.lock().unwrap();
        let t = &guard.term;
        let name = if t.name.is_empty() {
            "pane".to_string()
        } else {
            t.name.clone()
        };
        let title = t.title();
        // 1-based position in the tab's rect list.
        let index = self
            .rects
            .iter()
            .position(|(p, _)| *p == id)
            .map(|i| i + 1)
            .unwrap_or(id);
        if title.is_empty() {
            format!("{name} {index}")
        } else {
            format!("{name} {index} — {title}")
        }
    }
}

/// Paints one pane's terminal content into `rect`, in canvas-local coordinates.
fn paint_pane(
    frame: &mut Frame,
    pane: &mut Pane,
    rect: Rectangle,
    cell: CellDims,
    padding: f32,
    font_px: f32,
    font: Font,
) {
    // Disjoint field borrows: `MutexGuard` derefs through a trait, so borrowing
    // `term` and `scratch` at once only works after destructuring.
    let Pane {
        term, scratch, ..
    } = pane;
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

    let default_bg = to_color(colors.background);
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
    let mut text_buf = String::with_capacity(128);

    while let Some(row) = rows.next() {
        let mut x = ox;
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

            // A cell with no explicit bg, or one that is the terminal default,
            // needs no rectangle — the pane background already covers it.
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
                    // Inverse swaps the cell's own colours.
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

            // Text: extend the current run when colour and baseline match,
            // otherwise flush and start a new one. A cell without a glyph (a
            // wide character's continuation, an empty cell) BREAKS the run: the
            // glyphs after it sit at their own grid column, not at
            // `run start + advance`.
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

            if let Some(c_bg) = cell_bg {
                frame.fill_rectangle(Point::new(x, y), Size::new(cw, ch), c_bg);
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
        }

        if let Some(r) = run.take() {
            draw_run(frame, &r, font_px, font);
        }

        let _ = row.set_dirty(false);
        y += ch;
    }

    // Cursor: honour the style the terminal reports (DECSCUSR), block by default.
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
            // Block, BlockHollow and any future variant: solid block.
            _ => frame.fill_rectangle(Point::new(cx, cy), Size::new(cw, ch), cursor_color),
        }
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

/// Draws a pane's frame and label: the stroke sits 2 px outside the pane rect,
/// so it lands in the inset gutter, and is 2 px wide when the pane is focused
/// and 1 px otherwise, with a filled label band above it.
fn paint_frame(
    frame: &mut Frame,
    rect: Rectangle,
    active: bool,
    theme: &GhostTheme,
    label: &str,
    font: Font,
) {
    if label.is_empty() {
        return;
    }
    let color = rgb(if active {
        theme.frame_active
    } else {
        theme.frame
    });
    let width = if active {
        FRAME_ACTIVE_W
    } else {
        FRAME_INACTIVE_W
    };

    frame.stroke_rectangle(
        Point::new(rect.x - 2.0, rect.y - 2.0),
        Size::new(rect.width + 4.0, rect.height + 4.0),
        canvas::Stroke::default().with_color(color).with_width(width),
    );

    let text = format!(" {label} ");
    let band_w = text.chars().count() as f32 * LABEL_ADVANCE + LABEL_PAD;
    let band_y = rect.y - LABEL_H;
    frame.fill_rectangle(Point::new(rect.x - 2.0, band_y), Size::new(band_w, LABEL_H), color);
    frame.fill_text(CanvasText {
        content: text,
        position: Point::new(rect.x, band_y + 2.0),
        color: rgb(theme.tab_bg),
        size: Pixels(LABEL_PX),
        font,
        align_x: alignment::Horizontal::Left.into(),
        align_y: alignment::Vertical::Top,
        shaping: text::Shaping::Advanced,
        ..CanvasText::default()
    });
}

fn draw_run(frame: &mut Frame, run: &TextRun, font_px: f32, font: Font) {
    if run.text.is_empty() {
        return;
    }
    frame.fill_text(CanvasText {
        content: run.text.clone(),
        position: Point::new(run.x, run.baseline_y),
        color: run.color,
        size: Pixels(font_px),
        font,
        align_x: alignment::Horizontal::Left.into(),
        align_y: alignment::Vertical::Top,
        // Advanced, not Basic: `Basic` shapes with no font fallback, so every
        // glyph missing from the primary face — prompt glyphs, braille,
        // private-use icons — would be tofu.
        shaping: text::Shaping::Advanced,
        ..CanvasText::default()
    });
}

/// Converts a libghostty [`RgbColor`] to an opaque iced [`Color`].
fn to_color(c: RgbColor) -> Color {
    Color::from_rgb8(c.r, c.g, c.b)
}

/// Converts the app crate's theme colour to an opaque iced [`Color`].
fn rgb(c: ghostwave::theme::Rgb) -> Color {
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
