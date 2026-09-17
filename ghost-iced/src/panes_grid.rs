//! iced `pane_grid` pane layer: the pane layer that hands the panes to iced,
//! selected by `GHOST_ICED_PANEGRID`.
//!
//! The tab's own [`pane_grid::State`] is the layout authority ([`crate::app`]),
//! so this module reads it rather than reconstructing it: one [`PaneCanvas`] per
//! cell, painted at the bounds iced gives it, with iced owning the split drags,
//! the pane drags and the focus placement that follow from them.
//!
//! Geometry: a [`SPACING`] px gutter between cells with the window background
//! showing through it, each cell's terminal area inset by [`PANE_INSET`] px and
//! sitting below a [`TITLE_H`] px label band, framed by a stroke in
//! `theme.frame_active` (focused pane) or `theme.frame`. Frames off drops the
//! gutter, the inset and the band, so panes touch.
//!
//! Terminal content stays the canvas layer's: one [`PaneCanvas`] per pane paints
//! that pane at its own bounds with the shared painter.
//!
//! A cell holds a list of panes ([`GridPane`]): several of them are a
//! zellij-style stack, of which the active member is drawn and typed into.
//! Floating panes are out of scope.

use ghostwave::app::{STATUS_BAR_H, TAB_BAR_H};
use ghostwave::layout::SplitDir;
use ghostwave::term::{CellDims, MouseEv, MouseEvKind};
use ghostwave::theme::Theme as GhostTheme;
use iced::widget::canvas::{self, Action, Event, Frame, Geometry};
use iced::widget::pane_grid::{self, Axis, Content, PaneGrid, TitleBar};
use iced::widget::{Canvas, container, text};
use iced::{
    Background, Border, Element, Fill, Font, Length, Padding, Point, Rectangle, Renderer, Size,
    Theme, alignment, mouse,
};

use crate::Message;
use crate::app::{AppState, GridPane};
use crate::fonts;
use crate::keys;
use crate::pane::SharedPane;
use crate::pane_canvas::{self, CanvasState, ChromeData, TerminalCanvas};

/// Environment variable selecting this layer; unset, empty or `0` keeps the
/// canvas layer.
const ENV: &str = "GHOST_ICED_PANEGRID";

/// Gutter between neighbouring cells, in px.
pub const SPACING: f32 = 4.0;

/// Inset of a pane's terminal area inside its cell, in px.
pub const PANE_INSET: f32 = 2.0;

/// Height of a pane's label band (its title bar), in px.
pub const TITLE_H: f32 = 14.0;

/// A cell's smallest extent on either axis, in px.
pub const MIN_SIZE: f32 = 50.0;

/// Whether the pane layer is iced's `pane_grid`.
pub fn enabled() -> bool {
    std::env::var(ENV).is_ok_and(|v| !v.is_empty() && v != "0")
}

/// Gutter width, in px, for a pane layer with or without frames.
pub fn spacing(frames: bool) -> f32 {
    if frames { SPACING } else { 0.0 }
}

/// Inset of a pane's terminal area inside its cell, in px.
pub fn inset(frames: bool) -> f32 {
    if frames { PANE_INSET } else { 0.0 }
}

/// Height of a pane's label band, in px; zero when frames are off.
pub fn title_h(frames: bool) -> f32 {
    if frames { TITLE_H } else { 0.0 }
}

/// The grid axis a split direction splits along. `SplitDir::Horizontal` is
/// side-by-side, which is iced's vertical axis: the two enums are crossed.
pub fn axis(dir: SplitDir) -> Axis {
    match dir {
        SplitDir::Horizontal => Axis::Vertical,
        SplitDir::Vertical => Axis::Horizontal,
    }
}

/// The split direction a grid axis splits along; the inverse of [`axis`].
pub fn dir_of(axis: Axis) -> SplitDir {
    match axis {
        Axis::Vertical => SplitDir::Horizontal,
        Axis::Horizontal => SplitDir::Vertical,
    }
}

/// The pane-grid layer's view: the chrome canvas at the back, the grid over the
/// content area.
///
/// The canvas paints the tab bar, the status bar and the window background the
/// gutters show through, and keeps the pane rects empty so that panes, their
/// frames and their mouse handling belong to the grid alone. `panes` are the
/// active tab's live panes, keyed by their index in the tab.
pub fn view<'a>(
    state: &'a AppState,
    panes: Vec<(usize, SharedPane)>,
    mouse_tracking: Vec<usize>,
    chrome: ChromeData,
) -> Element<'a, Message> {
    let content = state.content();
    let chrome_layer = Canvas::new(TerminalCanvas {
        tab: state.active_tab,
        panes: Vec::new(),
        rects: Vec::new(),
        active: state.current().active,
        cell: state.cell,
        font_px: state.config.font_size,
        padding: state.config.padding,
        show_frames: false,
        paint_background: false,
        theme: state.theme,
        content: Rectangle::new(
            Point::new(content.x, content.y),
            Size::new(content.w.max(1.0), content.h.max(1.0)),
        ),
        chrome,
        mouse_tracking: Vec::new(),
    })
    .width(Fill)
    .height(Fill);

    let layer = container(grid(state, panes, mouse_tracking)).padding(Padding {
        top: TAB_BAR_H,
        right: 0.0,
        bottom: STATUS_BAR_H,
        left: 0.0,
    });

    // The chrome layer is last so it draws over the grid: a canvas inside a
    // `Stack` loses its text pass when it sits underneath an opaque sibling.
    iced::widget::Stack::with_children(vec![layer.into(), chrome_layer.into()])
        .width(Fill)
        .height(Fill)
        .into()
}

/// Builds the pane grid from the tab's own state: one pane per cell, each the
/// cell's active pane drawn by its own canvas under a title bar, and framed by
/// its own container border.
fn grid<'a>(
    state: &'a AppState,
    panes: Vec<(usize, SharedPane)>,
    mouse_tracking: Vec<usize>,
) -> PaneGrid<'a, Message> {
    let tab = state.current();
    let tab_index = state.active_tab;
    let frames = state.show_frames;
    let active = tab.active;
    let theme = state.theme;
    let (cell, font_px, padding) = (state.cell, state.config.font_size, state.config.padding);
    let inset = inset(frames);
    let font = fonts::primary();

    PaneGrid::new(&tab.grid, move |_cell, payload: &GridPane, _maximized| {
        // The cell's payload names panes by identity; the pane layer works with
        // the index the tab addresses them by.
        let index = payload.active_id().and_then(|id| tab.index_of(id));
        let handle = index.and_then(|index| {
            panes
                .iter()
                .find(|(pane, _)| *pane == index)
                .map(|(_, handle)| handle.clone())
        });
        let focused = index == Some(active);
        // `Content` has no padding of its own, so the frame width is part of the
        // canvas inset: without it the opaque canvas covers the pane's own
        // border and only the gutters show it.
        let frame_w = if !frames {
            0.0
        } else if focused {
            pane_canvas::FRAME_ACTIVE_W
        } else {
            pane_canvas::FRAME_INACTIVE_W
        };
        let frame_color = if focused {
            theme.frame_active
        } else {
            theme.frame
        };
        let body: Element<'a, Message> = match handle.clone() {
            Some(handle) => container(
                Canvas::new(PaneCanvas {
                    tab: tab_index,
                    pane: index.unwrap_or_default(),
                    active,
                    handle,
                    cell,
                    font_px,
                    padding,
                    tracking: index.is_some_and(|index| mouse_tracking.contains(&index)),
                })
                .width(Fill)
                .height(Fill),
            )
            .padding(inset + frame_w)
            .into(),
            None => text("").into(),
        };
        let mut content = Content::new(body);
        if frames {
            // The pane's 1-based position in the tab.
            let position = index.map(|index| index + 1).unwrap_or(1);
            let label = handle
                .as_ref()
                .map(|handle| pane_canvas::pane_label(handle, position))
                .unwrap_or_default();
            content = content.title_bar(TitleBar::new(label_band(&label, frame_color, font, theme)));
        }
        content.style(move |_theme| container::Style {
            // Also the pane's own margin, so the frame stroke reads as one.
            background: Some(pane_canvas::rgb(theme.bg).into()),
            border: Border {
                color: pane_canvas::rgb(frame_color),
                width: frame_w,
                radius: 0.0.into(),
            },
            ..container::Style::default()
        })
    })
    .spacing(spacing(frames))
    .min_size(MIN_SIZE)
    // The grab area of a split is the gutter itself, which is dead space: panes
    // keep every click inside their terminal area.
    .on_resize(0.0, |event| Message::GridResize {
        split: event.split,
        ratio: event.ratio,
    })
    .on_drag(Message::GridDrag)
    .style(move |_theme| grid_style(&theme))
    .width(Fill)
    .height(Fill)
}

/// The pane's label as a filled band, the canvas layer's chip turned into a title
/// bar; a long label is clipped rather than wrapped, so the band keeps its height.
fn label_band<'a>(
    label: &str,
    frame_color: ghostwave::theme::Rgb,
    font: Font,
    theme: GhostTheme,
) -> Element<'a, Message> {
    container(
        text(label.to_string())
            .size(pane_canvas::LABEL_PX)
            .font(font)
            .color(pane_canvas::rgb(theme.tab_bg))
            .wrapping(text::Wrapping::None),
    )
    .width(Length::Shrink)
    .height(TITLE_H)
    .padding(Padding {
        top: 0.0,
        right: pane_canvas::LABEL_PAD,
        bottom: 0.0,
        left: pane_canvas::LABEL_PAD,
    })
    .align_y(alignment::Vertical::Center)
    .style(move |_theme| container::Style {
        background: Some(pane_canvas::rgb(frame_color).into()),
        text_color: Some(pane_canvas::rgb(theme.tab_bg)),
        ..container::Style::default()
    })
    .into()
}

/// The grid's own highlights in this front-end's palette: the drag target is a
/// tint of the focused frame colour, the split lines are the frame colours.
fn grid_style(theme: &GhostTheme) -> pane_grid::Style {
    pane_grid::Style {
        hovered_region: pane_grid::Highlight {
            background: Background::Color(iced::Color {
                a: 0.25,
                ..pane_canvas::rgb(theme.frame_active)
            }),
            border: Border {
                color: pane_canvas::rgb(theme.frame_active),
                width: pane_canvas::FRAME_ACTIVE_W,
                radius: 0.0.into(),
            },
        },
        picked_split: pane_grid::Line {
            color: pane_canvas::rgb(theme.frame_active),
            width: pane_canvas::FRAME_ACTIVE_W,
        },
        hovered_split: pane_grid::Line {
            color: pane_canvas::rgb(theme.frame),
            width: pane_canvas::FRAME_INACTIVE_W,
        },
    }
}

/// One pane's terminal, painted at the canvas's own bounds: the pane-grid layer's
/// equivalent of the canvas layer's per-pane region.
///
/// Mouse events are forwarded into the pane in its own coordinates, exactly as
/// the canvas layer forwards them. `bounds` is the canvas's own rect in window
/// coordinates, so the pane-local position is the cursor offset into it.
pub struct PaneCanvas {
    /// Tab these panes belong to, so forwarded events can be tagged.
    pub tab: usize,
    /// The pane's index into its tab.
    pub pane: usize,
    /// The tab's focused pane, so a click knowledgeably focuses what it hit.
    pub active: usize,
    pub handle: SharedPane,
    pub cell: CellDims,
    pub font_px: f32,
    pub padding: f32,
    /// The pane's application currently tracks the mouse; cursor motion is
    /// delivered to it continuously and only to it.
    pub tracking: bool,
}

impl canvas::Program<Message> for PaneCanvas {
    type State = CanvasState;

    fn update(
        &self,
        state: &mut Self::State,
        event: &Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<Action<Message>> {
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(button)) => {
                let point = cursor.position()?;
                let pressed = pane_canvas::vt_button(*button)?;
                state.press(*button);
                Some(Action::publish(self.mouse_message(
                    point,
                    bounds,
                    MouseEvKind::Press(pressed),
                    state,
                )))
            }
            Event::Mouse(mouse::Event::ButtonReleased(button)) => {
                let point = cursor.position()?;
                let released = pane_canvas::vt_button(*button)?;
                state.release(*button);
                Some(Action::publish(self.mouse_message(
                    point,
                    bounds,
                    MouseEvKind::Release(released),
                    state,
                )))
            }
            // Motion is the hot path: iced reports every pixel, so it only goes
            // to a pane that is tracking the mouse or has a button down.
            Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                if !state.any_pressed() && !self.tracking {
                    return None;
                }
                let point = cursor.position()?;
                Some(Action::publish(self.mouse_message(
                    point,
                    bounds,
                    MouseEvKind::Motion,
                    state,
                )))
            }
            Event::Mouse(mouse::Event::WheelScrolled { delta }) => {
                let point = cursor.position()?;
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
                    point,
                    bounds,
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
        // The frame covers this pane only: iced clips the canvas's geometry to
        // its own layer, so the pane rect is the whole frame.
        let mut frame = Frame::new(renderer, bounds.size());
        let mut guard = self.handle.lock().unwrap();
        pane_canvas::paint_pane(
            &mut frame,
            &mut guard,
            Rectangle::new(Point::ORIGIN, bounds.size()),
            self.cell,
            self.padding,
            self.font_px,
            fonts::primary(),
        );
        guard.dirty = false;
        vec![frame.into_geometry()]
    }
}

impl PaneCanvas {
    /// The mouse event for `point`, in the pane's own coordinates.
    fn mouse_message(
        &self,
        point: Point,
        bounds: Rectangle,
        kind: MouseEvKind,
        state: &CanvasState,
    ) -> Message {
        Message::Mouse {
            tab: self.tab,
            pane: self.pane,
            // A click focuses what it lands on, and the same event is still fed
            // to the pane under the cursor.
            focus: self.pane != self.active,
            ev: MouseEv {
                kind,
                x: point.x - bounds.x,
                y: point.y - bounds.y,
                any_pressed: state.any_pressed(),
                button_down: state.button_down(),
            },
            mods: keys::current_mods(),
        }
    }
}
