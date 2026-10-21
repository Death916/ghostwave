//! iced `pane_grid` pane layer: the pane layer that hands the panes to iced.
//!
//! The tab's own [`pane_grid::State`] is the layout authority ([`crate::app`]),
//! so this module reads it rather than reconstructing it: one [`PaneCanvas`] per
//! cell, painted at the bounds iced gives it, with iced owning the split drags,
//! the pane drags and the focus placement that follow from them.
//!
//! Geometry: neighbouring cells touch, and each paints a frame stroke on its own
//! outermost pixels, so the frames of two neighbours meet on the edge they share
//! and that edge carries one line instead of a band of window background
//! between two frames. A cell's terminal area sits [`inset`] px inside its frame
//! and below a [`TITLE_H`] px label band. Frames off drops the stroke, the inset
//! and the band, so panes touch.
//!
//! Terminal content stays the canvas layer's: one [`PaneCanvas`] per pane paints
//! that pane at its own bounds with the shared painter.
//!
//! A cell whose pane is not a terminal ([`ContentPane`]) paints that pane's own
//! widget in place of the canvas, clipped to the cell: a preview, an AI chat or
//! the visualizer. Such a cell gets no [`PaneCanvas`] and no mouse reporting, and
//! its label band names its kind.
//!
//! A cell holds a list of panes ([`GridPane`]): several of them are a
//! zellij-style stack, of which the active member is drawn and typed into.
//! Floating panes are out of scope.

use ghostwave::layout::SplitDir;
use ghostwave::term::{CellDims, MouseEv, MouseEvKind};
use ghostwave::theme::Theme as GhostTheme;
use iced::widget::canvas::{self, Action, Event, Frame, Geometry};
use iced::widget::pane_grid::{self, Axis, Content, PaneGrid, TitleBar};
use iced::widget::{Canvas, container, text};
use iced::{
    Background, Border, Element, Fill, Font, Length, Padding, Point, Rectangle, Renderer, Theme,
    alignment, mouse,
};

use crate::Message;
use crate::app::{AppState, GridPane, Tab};
use crate::keys;
use crate::pane::{ContentPane, PaneEntry, PaneId, SharedPane};
use crate::pane_canvas::{self, CanvasState};
use crate::ui_fonts;

/// Background kept between the terminal areas of two neighbouring panes, in px.
///
/// Half of it is each pane's own side of the divider, so the terminal areas stay
/// apart without the frames being separated: the cells themselves touch.
pub const PANE_GAP: f32 = 4.0;

/// Inset of a pane's terminal area inside its cell, in px.
pub const PANE_INSET: f32 = 2.0;

/// Height of a pane's label band (its title bar), in px.
pub const TITLE_H: f32 = 16.0;

/// A cell's smallest extent on either axis, in px.
pub const MIN_SIZE: f32 = 50.0;

/// Converts pane bounds in px to cols and rows, clamped to at least 1x1.
pub fn bounds_to_cols_rows(width: f32, height: f32, cell: CellDims, padding: f32) -> (u16, u16) {
    if width.is_nan() || height.is_nan() {
        return (1, 1);
    }
    let cols = ((width - 2.0 * padding) / cell.w).max(1.0) as u16;
    let rows = ((height - 2.0 * padding) / cell.h).max(1.0) as u16;
    (cols.max(1), rows.max(1))
}

/// Maps a clicked grid cell to the tab pane index of that cell's active member.
pub fn cell_active_pane_index(
    grid: &pane_grid::State<GridPane>,
    cell: pane_grid::Pane,
    index_of: impl Fn(PaneId) -> Option<usize>,
) -> Option<usize> {
    grid.get(cell)
        .and_then(GridPane::active_id)
        .and_then(index_of)
}

/// Maps a clicked grid cell to the tab pane index of that cell's active member.
pub fn click_to_pane_index(
    grid: &pane_grid::State<GridPane>,
    tab: &Tab,
    cell: pane_grid::Pane,
) -> Option<usize> {
    cell_active_pane_index(grid, cell, |id| tab.index_of(id))
}

/// Gutter the grid leaves between neighbouring cells, in px.
///
/// None, ever. A frame is drawn on a cell's own outermost pixels, so any gutter
/// iced inserted would sit *between* the frames of two neighbours and show the
/// window background on the edge they share — which reads as a missing frame
/// line exactly where the panes meet. The space that keeps the terminal areas
/// apart is [`inset`]'s, and a split is still grabbable through
/// [`SPLIT_GRAB_W`].
pub const SPACING: f32 = 0.0;

/// Width of the band, in px, in which a split can be grabbed, with frames on.
///
/// The grid has no spacing of its own to grab, so this is handed to iced as the
/// split's leeway; iced makes the hit area `spacing + leeway` wide, centred on
/// the split, which keeps the 4px band the old gutter gave. It is handed over
/// only when frames are on: without them there is nothing to grab between the
/// panes, and a band there would swallow the click that should focus the pane.
pub const SPLIT_GRAB_W: f32 = 4.0;

/// Inset of a pane's terminal area inside its cell, in px.
///
/// [`PANE_GAP`] split over the two panes that share an edge, plus
/// [`PANE_INSET`]; zero without frames, so panes touch.
pub fn inset(frames: bool) -> f32 {
    if frames {
        PANE_INSET + PANE_GAP / 2.0
    } else {
        0.0
    }
}

/// A cell's frame stroke and the inset it puts its terminal area at, in px.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CellMetrics {
    /// Width of the frame stroke drawn on the cell's own edge.
    pub frame: f32,
    /// Inset of the terminal area inside the cell, the stroke included.
    pub padding: f32,
}

/// The frame stroke and terminal inset of one cell.
///
/// The stroke is the focused or unfocused width, or nothing without frames; the
/// terminal area is inset by the stroke plus [`inset`], which is what keeps a
/// pane's content off its own frame and off its neighbour's.
pub fn cell_metrics(frames: bool, focused: bool) -> CellMetrics {
    let frame = if !frames {
        0.0
    } else if focused {
        pane_canvas::FRAME_ACTIVE_W
    } else {
        pane_canvas::FRAME_INACTIVE_W
    };
    CellMetrics {
        frame,
        padding: inset(frames) + frame,
    }
}

/// The rect a pane paints into: the canvas' bounds snapped to whole pixels,
/// without growing past them.
///
/// iced hands the canvas fractional bounds and the painter draws in the canvas'
/// own coordinates, so the origin moves to the nearest pixel and the size gives
/// back whatever it moved. The fill covers the same pixels as before, but its
/// far edge can no longer sit up to half a pixel past the canvas edge — which is
/// the canvas' `theme.bg` band in front of the pane's frame stroke.
fn paint_rect(bounds: Rectangle) -> Rectangle {
    let fx = bounds.x.round() - bounds.x;
    let fy = bounds.y.round() - bounds.y;
    Rectangle {
        x: fx,
        y: fy,
        width: bounds.width - fx,
        height: bounds.height - fy,
    }
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

/// The pane-grid band of the window: the grid, filling whatever the chrome bands
/// do not take. It is laid out in a column with the bars (see `main.rs`), so
/// nothing here reserves space for them and nothing here draws chrome.
pub fn view<'a>(state: &'a AppState, mouse_tracking: Vec<usize>) -> Element<'a, Message> {
    container(grid(state, mouse_tracking))
        .style(move |_theme| container::Style {
            background: Some(pane_canvas::rgb(state.theme.bg).into()),
            ..container::Style::default()
        })
        .width(Fill)
        .height(Fill)
        .into()
}

/// Builds the pane grid from the tab's own state: one pane per cell, each the
/// cell's active pane — a terminal drawn by its own canvas, or a content pane's
/// own widget — under a title bar, and framed by its own container border.
fn grid<'a>(state: &'a AppState, mouse_tracking: Vec<usize>) -> PaneGrid<'a, Message> {
    let tab = state.current();
    let tab_index = state.active_tab;
    let frames = state.show_frames;
    let active = tab.active;
    let theme = state.theme;
    let (cell, font_px, padding) = (state.cell, state.config.font_size, state.config.padding);
    let font = ui_fonts::primary();
    let replace_title = state.config.dynamic_title != crate::ui_config::DynamicTitleMode::Off;

    PaneGrid::new(&tab.grid, move |_cell, payload: &GridPane, _maximized| {
        let index = payload.active_id().and_then(|id| tab.index_of(id));
        let entry = index.and_then(|index| tab.entry(index));
        let focused = index == Some(active);
        let metrics = cell_metrics(frames, focused);
        let frame_w = metrics.frame;
        let frame_color = if focused {
            theme.frame_active
        } else {
            theme.frame
        };
        let body: Element<'a, Message> = match entry {
            Some(PaneEntry::Term(handle)) => container(
                Canvas::new(PaneCanvas {
                    tab: tab_index,
                    pane: index.unwrap_or_default(),
                    pane_id: entry.map(|entry| entry.id()).unwrap_or(PaneId(0)),
                    handle: handle.clone(),
                    cell,
                    font_px,
                    padding,
                    tracking: index.is_some_and(|index| mouse_tracking.contains(&index)),
                    theme,
                })
                .width(Fill)
                .height(Fill),
            )
            .padding(metrics.padding)
            .into(),
            Some(PaneEntry::Content(content)) => content_body(content, metrics.padding, theme),
            None => text("").into(),
        };
        let mut content = Content::new(body);
        if frames {
            let position = index.map(|index| index + 1).unwrap_or(1);
            let stack_info = if payload.panes.len() > 1 {
                Some((payload.active, payload.panes.len()))
            } else {
                None
            };
            let label = match entry {
                Some(PaneEntry::Term(handle)) => {
                    pane_canvas::cell_label(handle, position, stack_info, replace_title)
                }
                Some(PaneEntry::Content(content)) => content.label(),
                None => String::new(),
            };
            content =
                content.title_bar(TitleBar::new(label_band(&label, frame_color, font, theme)));
        }
        content.style(move |_theme| container::Style {
            background: Some(pane_canvas::rgb(theme.bg).into()),
            border: Border {
                color: pane_canvas::rgb(frame_color),
                width: frame_w,
                radius: 0.0.into(),
            },
            ..container::Style::default()
        })
    })
    .spacing(SPACING)
    .min_size(MIN_SIZE)
    .on_click(move |pane| {
        let index = click_to_pane_index(&tab.grid, tab, pane).unwrap_or(tab.active);
        Message::FocusPane(index)
    })
    .on_resize(if frames { SPLIT_GRAB_W } else { 0.0 }, |event| {
        Message::GridResize {
            split: event.split,
            ratio: event.ratio,
        }
    })
    .on_drag(Message::GridDrag)
    .style(move |_theme| grid_style(&theme))
    .width(Fill)
    .height(Fill)
}

/// A content pane's own widget tree, inset inside its cell's frame and clipped
/// to it.
///
/// The pane is not a terminal, so it brings its own widget instead of a
/// [`PaneCanvas`]: the preview's line and image views, the AI chat's cards, or
/// the visualizer's canvas program.
fn content_body<'a>(
    content: &'a ContentPane,
    inset: f32,
    theme: GhostTheme,
) -> Element<'a, Message> {
    let body: Element<'a, Message> = match content {
        ContentPane::Preview { pane, .. } => pane.view::<Message>(&theme),
        ContentPane::Ai { id, pane } => {
            let id = *id;
            pane.view(&Theme::Dark).map(move |msg| match msg {
                crate::ai::AiMessage::InputChanged(_) => Message::Noop,
                msg => Message::Ai { id, msg },
            })
        }
        ContentPane::Visualizer { pane, .. } => pane.view::<Message>(&theme),
    };
    container(body)
        .width(Fill)
        .height(Fill)
        .padding(inset)
        .clip(true)
        .into()
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
    /// The pane's index into its tab, kept for logging and mouse tracking.
    pub pane: usize,
    /// The pane's identity.
    ///
    /// Events are tagged with this rather than the index: the index is only
    /// meaningful for the layout generation that produced it, so a focus change,
    /// close or split between a press and the motion events that follow it would
    /// resolve an index against a different pane.
    pub pane_id: PaneId,
    pub handle: SharedPane,
    pub cell: CellDims,
    pub font_px: f32,
    pub padding: f32,
    /// The pane's application currently tracks the mouse; cursor motion is
    /// delivered to it continuously and only to it.
    pub tracking: bool,
    pub theme: GhostTheme,
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
                let point = cursor.position_over(bounds);
                debug_mouse("press", self.pane, Some(*button), state, bounds, point);
                let point = point?;
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
                let owned = state.any_pressed();
                state.release(*button);
                let point = cursor.position();
                debug_mouse(
                    if owned { "release" } else { "release:ignored" },
                    self.pane,
                    Some(*button),
                    state,
                    bounds,
                    point,
                );
                if !owned {
                    return None;
                }
                let released = pane_canvas::vt_button(*button)?;
                Some(Action::publish(self.mouse_message(
                    clamp_into(bounds, point?),
                    bounds,
                    MouseEvKind::Release(released),
                    state,
                )))
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                let point = cursor.position();
                if state.any_pressed() {
                    debug_mouse("motion:drag", self.pane, None, state, bounds, point);
                    let point = clamp_into(bounds, point?);
                    return Some(Action::publish(self.mouse_message(
                        point,
                        bounds,
                        MouseEvKind::Motion,
                        state,
                    )));
                }
                if !self.tracking || cursor.position_over(bounds).is_none() {
                    debug_mouse("motion:dropped", self.pane, None, state, bounds, point);
                    return None;
                }
                debug_mouse("motion", self.pane, None, state, bounds, point);
                Some(Action::publish(self.mouse_message(
                    point?,
                    bounds,
                    MouseEvKind::Motion,
                    state,
                )))
            }
            Event::Mouse(mouse::Event::WheelScrolled { delta }) => {
                let point = cursor.position_over(bounds)?;
                let line_step = (self.cell.h as f32).max(1.0);
                let dy = match delta {
                    mouse::ScrollDelta::Lines { y, .. } => *y,
                    mouse::ScrollDelta::Pixels { y, .. } => {
                        let notches = *y / line_step;
                        if notches.abs() > 0.0 && notches.abs() < 1.0 {
                            notches.signum()
                        } else {
                            notches
                        }
                    }
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
        let mut frame = Frame::new(renderer, bounds.size());
        let mut guard = self.handle.lock().unwrap();

        guard.last_bounds = Some(bounds);
        let (cols, rows) =
            bounds_to_cols_rows(bounds.width, bounds.height, self.cell, self.padding);
        guard.resize(cols, rows, self.cell);

        let pane_rect = paint_rect(bounds);

        pane_canvas::paint_pane(
            &mut frame,
            &mut guard,
            pane_rect,
            self.cell,
            self.padding,
            self.font_px,
            ui_fonts::primary(),
            &self.theme,
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
            pane: self.pane_id,
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

/// Clamps a window-space point into `bounds`, so a drag that leaves the pane still
/// reports a position inside it.
fn clamp_into(bounds: Rectangle, point: Point) -> Point {
    Point::new(
        point.x.clamp(bounds.x, bounds.x + bounds.width),
        point.y.clamp(bounds.y, bounds.y + bounds.height),
    )
}

/// Logs one canvas mouse event, when `GHOST_ICED_DEBUG` is set.
fn debug_mouse(
    label: &str,
    pane: usize,
    button: Option<mouse::Button>,
    state: &CanvasState,
    bounds: Rectangle,
    point: Option<Point>,
) {
    if std::env::var_os("GHOST_ICED_DEBUG").is_none() {
        return;
    }
    eprintln!(
        "[mouse] {label} pane={pane} button={button:?} any_pressed={} button_down={:?} bounds=({:.1},{:.1} {:.1}x{:.1}) point={point:?}",
        state.any_pressed(),
        state.button_down(),
        bounds.x,
        bounds.y,
        bounds.width,
        bounds.height,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use ghostwave::term::CellDims;
    use iced::widget::pane_grid::{self, Axis};

    #[test]
    fn bounds_to_cols_rows_calculations() {
        let cell = CellDims {
            w: 8.0,
            h: 16.0,
            baseline: 12.0,
        };
        let padding = 6.0;

        let (cols, rows) = bounds_to_cols_rows(800.0, 600.0, cell, padding);
        assert_eq!(cols, 98);
        assert_eq!(rows, 36);

        let (cols, rows) = bounds_to_cols_rows(556.80005, 384.25003, cell, padding);
        assert_eq!(cols, 68);
        assert_eq!(rows, 23);

        assert_eq!(bounds_to_cols_rows(0.0, 0.0, cell, padding), (1, 1));
        assert_eq!(bounds_to_cols_rows(10.0, 10.0, cell, padding), (1, 1));
        assert_eq!(bounds_to_cols_rows(-100.0, -50.0, cell, padding), (1, 1));
        assert_eq!(bounds_to_cols_rows(f32::NAN, 100.0, cell, padding), (1, 1));
        assert_eq!(bounds_to_cols_rows(100.0, f32::NAN, cell, padding), (1, 1));
    }

    #[test]
    fn stack_label_string_formatting() {
        use crate::pane_canvas::format_pane_label;

        assert_eq!(format_pane_label("", 1, "", None, false), "pane 1");
        assert_eq!(
            format_pane_label("term", 2, "fish", None, false),
            "term 2 — fish"
        );
        assert_eq!(
            format_pane_label("pane", 1, "top", Some((0, 1)), false),
            "pane 1 — top"
        );
        assert_eq!(
            format_pane_label("pane", 1, "sh", Some((0, 2)), false),
            "pane 1 [1/2] — sh"
        );
        assert_eq!(
            format_pane_label("pane", 2, "vim", Some((1, 3)), false),
            "pane 2 [2/3] — vim"
        );
        assert_eq!(
            format_pane_label("", 1, "", Some((2, 3)), false),
            "pane 1 [3/3]"
        );

        assert_eq!(format_pane_label("", 1, "", None, true), "pane 1");
        assert_eq!(format_pane_label("term", 2, "fish", None, true), "fish");
        assert_eq!(
            format_pane_label("pane", 1, "sh", Some((0, 2)), true),
            "sh [1/2]"
        );
        assert_eq!(
            format_pane_label("pane", 2, "vim", Some((1, 3)), true),
            "vim [2/3]"
        );
        assert_eq!(
            format_pane_label("", 1, "", Some((2, 3)), true),
            "pane 1 [3/3]"
        );
    }

    #[test]
    fn click_to_pane_index_mapping() {
        let (mut grid, root) = pane_grid::State::new(GridPane::single(PaneId(10)));
        let index_map = |id: PaneId| match id.0 {
            10 => Some(0),
            20 => Some(1),
            30 => Some(2),
            _ => None,
        };

        assert_eq!(cell_active_pane_index(&grid, root, index_map), Some(0));

        let (right, _split) = grid
            .split(Axis::Vertical, root, GridPane::single(PaneId(20)))
            .expect("split succeeds");

        assert_eq!(cell_active_pane_index(&grid, root, index_map), Some(0));
        assert_eq!(cell_active_pane_index(&grid, right, index_map), Some(1));

        let cell = grid.get_mut(root).expect("root exists");
        cell.panes.push(PaneId(30));
        cell.active = 1;

        assert_eq!(cell_active_pane_index(&grid, root, index_map), Some(2));

        let cell = grid.get_mut(root).expect("root exists");
        cell.active = 0;
        assert_eq!(cell_active_pane_index(&grid, root, index_map), Some(0));
    }

    /// Window size the framed-pane cases are laid out at: halves land on whole
    /// pixels, so the arithmetic below is exact.
    const W: f32 = 1200.0;
    /// See [`W`].
    const H: f32 = 800.0;

    /// A side of a cell.
    #[derive(Clone, Copy, Debug)]
    enum Side {
        Left,
        Right,
        Top,
        Bottom,
    }

    /// The cell rects iced lays out for `grid` — the same call `PaneGrid::layout`
    /// makes, so the numbers are the engine's, not this test's.
    fn regions(
        grid: &pane_grid::State<GridPane>,
    ) -> std::collections::BTreeMap<pane_grid::Pane, Rectangle> {
        grid.layout()
            .pane_regions(SPACING, MIN_SIZE, iced::Size::new(W, H))
    }

    /// The band `width` px wide that a cell's frame stroke occupies on `side`.
    fn frame_edge(cell: Rectangle, width: f32, side: Side) -> Rectangle {
        match side {
            Side::Left => Rectangle { width, ..cell },
            Side::Right => Rectangle {
                x: cell.x + cell.width - width,
                width,
                ..cell
            },
            Side::Top => Rectangle {
                height: width,
                ..cell
            },
            Side::Bottom => Rectangle {
                y: cell.y + cell.height - width,
                height: width,
                ..cell
            },
        }
    }

    /// The cell's terminal area: below the label band, inside the frame and the
    /// pane inset.
    fn body(cell: Rectangle, padding: f32, frames: bool) -> Rectangle {
        let title = if frames { TITLE_H } else { 0.0 };
        Rectangle {
            x: cell.x + padding,
            y: cell.y + title + padding,
            width: cell.width - 2.0 * padding,
            height: cell.height - title - 2.0 * padding,
        }
    }

    /// Asserts one pair of neighbouring cells carries its frame on the edge they
    /// share: the two strokes meet on the boundary with no background band
    /// between them, and neither cell's terminal area reaches under a stroke.
    fn assert_framed_shared_edge(
        frames: bool,
        a: Rectangle,
        focused_a: bool,
        b: Rectangle,
        focused_b: bool,
    ) {
        let m_a = cell_metrics(frames, focused_a);
        let m_b = cell_metrics(frames, focused_b);
        let (w_a, w_b) = (m_a.frame, m_b.frame);
        let (pad_a, pad_b) = (m_a.padding, m_b.padding);
        assert!(w_a > 0.0 && w_b > 0.0, "both cells draw a frame stroke");

        // `b` is the neighbour to the right of, or below, `a`.
        let (side_a, side_b, gap, body_a, body_b) = if b.x > a.x + 0.01 {
            (
                Side::Right,
                Side::Left,
                b.x - (a.x + a.width),
                body(a, pad_a, frames),
                body(b, pad_b, frames),
            )
        } else {
            assert!(b.y > a.y + 0.01, "cells {a:?} and {b:?} are not neighbours");
            (
                Side::Bottom,
                Side::Top,
                b.y - (a.y + a.height),
                body(a, pad_a, frames),
                body(b, pad_b, frames),
            )
        };

        assert!(
            gap.abs() <= 0.01,
            "the two frames must meet on the shared edge, but {gap}px of \
             background sits between the cells ({a:?} | {b:?})",
        );

        let edge_a = frame_edge(a, w_a, side_a);
        let edge_b = frame_edge(b, w_b, side_b);
        let stroke_gap = match side_a {
            Side::Right | Side::Left => edge_b.x - (edge_a.x + edge_a.width),
            Side::Top | Side::Bottom => edge_b.y - (edge_a.y + edge_a.height),
        };
        assert!(
            stroke_gap.abs() <= 0.01,
            "the two frame strokes must touch across the shared edge, but they \
             are {stroke_gap}px apart ({edge_a:?} | {edge_b:?})",
        );

        // No cell's terminal area may sit under its neighbour's stroke.
        match side_a {
            Side::Right | Side::Left => {
                assert!(
                    body_b.x >= edge_a.x + edge_a.width - 0.01,
                    "the neighbour's terminal covers this cell's frame: \
                     body {body_b:?} vs stroke {edge_a:?}",
                );
                assert!(
                    body_a.x + body_a.width <= edge_b.x + 0.01,
                    "this cell's terminal covers the neighbour's frame: \
                     body {body_a:?} vs stroke {edge_b:?}",
                );
                assert!(
                    body_a.x + body_a.width <= body_b.x,
                    "the two terminal areas overlap: {body_a:?} | {body_b:?}",
                );
            }
            Side::Top | Side::Bottom => {
                assert!(
                    body_b.y >= edge_a.y + edge_a.height - 0.01,
                    "the neighbour's terminal covers this cell's frame: \
                     body {body_b:?} vs stroke {edge_a:?}",
                );
                assert!(
                    body_a.y + body_a.height <= edge_b.y + 0.01,
                    "this cell's terminal covers the neighbour's frame: \
                     body {body_a:?} vs stroke {edge_b:?}",
                );
                assert!(
                    body_a.y + body_a.height <= body_b.y,
                    "the two terminal areas overlap: {body_a:?} | {body_b:?}",
                );
            }
        }
    }

    /// The frame is drawn on every edge of a cell, the ones it shares with a
    /// neighbour included: with frames on, a cell's stroke and its neighbour's
    /// meet on the boundary, so the shared edge shows a frame line instead of a
    /// band of window background.
    #[test]
    fn framed_panes_carry_their_frame_on_shared_edges() {
        // Two panes, side by side.
        let (mut grid, root) = pane_grid::State::new(GridPane::single(PaneId(1)));
        let (right, _split) = grid
            .split(
                axis(SplitDir::Horizontal),
                root,
                GridPane::single(PaneId(2)),
            )
            .expect("split succeeds");
        let cells = regions(&grid);
        assert_framed_shared_edge(true, cells[&root], true, cells[&right], false);

        // Two panes, stacked.
        let (mut grid, root) = pane_grid::State::new(GridPane::single(PaneId(1)));
        let (bottom, _split) = grid
            .split(axis(SplitDir::Vertical), root, GridPane::single(PaneId(2)))
            .expect("split succeeds");
        let cells = regions(&grid);
        assert_framed_shared_edge(true, cells[&root], true, cells[&bottom], false);

        // Four panes in a cross: every pane has a neighbour on each axis.
        let (mut grid, root) = pane_grid::State::new(GridPane::single(PaneId(1)));
        let (bottom, _split) = grid
            .split(axis(SplitDir::Vertical), root, GridPane::single(PaneId(2)))
            .expect("split succeeds");
        let (top_right, _split) = grid
            .split(
                axis(SplitDir::Horizontal),
                root,
                GridPane::single(PaneId(3)),
            )
            .expect("split succeeds");
        let (bottom_right, _split) = grid
            .split(
                axis(SplitDir::Horizontal),
                bottom,
                GridPane::single(PaneId(4)),
            )
            .expect("split succeeds");
        let cells = regions(&grid);
        assert_framed_shared_edge(true, cells[&root], true, cells[&top_right], false);
        assert_framed_shared_edge(true, cells[&bottom], false, cells[&bottom_right], false);
        assert_framed_shared_edge(true, cells[&root], true, cells[&bottom], false);
        assert_framed_shared_edge(true, cells[&top_right], false, cells[&bottom_right], false);
    }

    /// Frames off is unchanged: no gutter, no inset, no stroke — the panes'
    /// terminal areas touch.
    #[test]
    fn frameless_panes_touch_with_no_gutter() {
        assert_eq!(SPACING, 0.0);
        assert_eq!(inset(false), 0.0);
        assert_eq!(
            cell_metrics(false, true),
            CellMetrics {
                frame: 0.0,
                padding: 0.0
            }
        );
        assert_eq!(
            cell_metrics(false, false),
            CellMetrics {
                frame: 0.0,
                padding: 0.0
            }
        );

        let (mut grid, root) = pane_grid::State::new(GridPane::single(PaneId(1)));
        let (right, _split) = grid
            .split(
                axis(SplitDir::Horizontal),
                root,
                GridPane::single(PaneId(2)),
            )
            .expect("split succeeds");
        let cells = regions(&grid);

        let pad_a = cell_metrics(false, true).padding;
        let pad_b = cell_metrics(false, false).padding;
        let body_a = body(cells[&root], pad_a, false);
        let body_b = body(cells[&right], pad_b, false);

        assert_eq!(
            body_a.x + body_a.width,
            body_b.x,
            "frames off: the two panes' terminal areas touch, with no gutter",
        );
        assert_eq!(body_a.y, body_b.y);
        assert_eq!(body_a.height, body_b.height);
    }

    /// The painted half of the shared edge: what a pane's fill can reach has to
    /// stay clear of both strokes on the edge two cells share.
    ///
    /// The boundary comes from the engine's own layout ([`regions`]), not from
    /// this module's arithmetic, so this fails the day [`SPACING`] stops being
    /// zero, a cell is handed out short or overlapping, or [`cell_metrics`]
    /// stops adding the frame to the inset — the changes that would let a fill
    /// reach a stroke. The reach is the canvas' own edge plus the worst-case
    /// overhang of the paint rect, whose origin may sit half a pixel out.
    #[test]
    fn painted_fill_cannot_reach_a_shared_edge() {
        // Stacked: `a` above `b`, both focus combinations.
        let (mut grid, root) = pane_grid::State::new(GridPane::single(PaneId(1)));
        let (bottom, _split) = grid
            .split(axis(SplitDir::Vertical), root, GridPane::single(PaneId(2)))
            .expect("split succeeds");
        let cells = regions(&grid);
        let (a, b) = (cells[&root], cells[&bottom]);
        assert!(
            (b.y - (a.y + a.height)).abs() <= 0.01,
            "the stacked cells must touch on the shared edge: {a:?} | {b:?}",
        );
        for focused_a in [true, false] {
            let m_a = cell_metrics(true, focused_a);
            let boundary = b.y;
            let fill_reach = a.y + a.height - m_a.padding + 0.5;
            assert!(
                fill_reach <= a.y + a.height - m_a.frame - 3.5 + 0.01,
                "the fill reaches this pane's own stroke: reach {fill_reach}, \
                 stroke starts at {}",
                a.y + a.height - m_a.frame,
            );
            assert!(
                fill_reach <= boundary - (3.5 + m_a.frame) + 0.01,
                "the fill reaches the neighbour's stroke: reach {fill_reach}, \
                 boundary {boundary}",
            );
        }

        // Side by side: `a` left of `b`, both focus combinations.
        let (mut grid, root) = pane_grid::State::new(GridPane::single(PaneId(1)));
        let (right, _split) = grid
            .split(
                axis(SplitDir::Horizontal),
                root,
                GridPane::single(PaneId(2)),
            )
            .expect("split succeeds");
        let cells = regions(&grid);
        let (a, b) = (cells[&root], cells[&right]);
        assert!(
            (b.x - (a.x + a.width)).abs() <= 0.01,
            "the side-by-side cells must touch on the shared edge: {a:?} | {b:?}",
        );
        for focused_a in [true, false] {
            let m_a = cell_metrics(true, focused_a);
            let boundary = b.x;
            let fill_reach = a.x + a.width - m_a.padding + 0.5;
            assert!(
                fill_reach <= a.x + a.width - m_a.frame - 3.5 + 0.01,
                "the fill reaches this pane's own stroke: reach {fill_reach}, \
                 stroke starts at {}",
                a.x + a.width - m_a.frame,
            );
            assert!(
                fill_reach <= boundary - (3.5 + m_a.frame) + 0.01,
                "the fill reaches the neighbour's stroke: reach {fill_reach}, \
                 boundary {boundary}",
            );
        }
    }

    /// The painted rect never grows past its canvas, on any layout phase.
    ///
    /// [`paint_rect`] answers in the canvas' own coordinates, so its result is
    /// put back into window coordinates here before it is compared with the
    /// bounds — the space the canvas' edge and the pane's frame stroke live in.
    /// Snapping the origin to the nearest pixel can move it up to half a pixel
    /// *before* the bounds' origin; what must never happen is the far edge
    /// passing the canvas' far edge, which is what used to let the fill reach
    /// the stroke's side of the canvas.
    #[test]
    fn paint_rect_stays_inside_the_canvas() {
        let cases: [(&str, Rectangle); 4] = [
            (
                "integer",
                Rectangle {
                    x: 592.0,
                    y: 24.0,
                    width: 597.0,
                    height: 640.0,
                },
            ),
            (
                "both fractional",
                Rectangle {
                    x: 592.5,
                    y: 24.25,
                    width: 597.75,
                    height: 640.5,
                },
            ),
            (
                "y rounds up",
                Rectangle {
                    x: 592.25,
                    y: 24.75,
                    width: 597.75,
                    height: 640.5,
                },
            ),
            (
                "worst phase",
                Rectangle {
                    x: 592.999,
                    y: 24.999,
                    width: 597.75,
                    height: 640.5,
                },
            ),
        ];

        for (name, b) in cases {
            let local = paint_rect(b);
            let rect = Rectangle {
                x: b.x + local.x,
                y: b.y + local.y,
                width: local.width,
                height: local.height,
            };
            assert_eq!(
                rect.x.fract(),
                0.0,
                "{name}: x is not on a pixel: {}",
                rect.x
            );
            assert_eq!(
                rect.y.fract(),
                0.0,
                "{name}: y is not on a pixel: {}",
                rect.y
            );
            assert!(
                (rect.x - b.x).abs() <= 0.5 && (rect.y - b.y).abs() <= 0.5,
                "{name}: the origin moved more than half a pixel: {rect:?} vs {b:?}",
            );
            assert!(
                rect.x + rect.width <= b.x + b.width + 1e-3,
                "{name}: the rect ends at {} past the canvas edge {}: {rect:?}",
                rect.x + rect.width,
                b.x + b.width,
            );
            assert!(
                rect.y + rect.height <= b.y + b.height + 1e-3,
                "{name}: the rect ends at {} past the canvas edge {}: {rect:?}",
                rect.y + rect.height,
                b.y + b.height,
            );
        }

        // Whole-pixel bounds map to themselves.
        let b = cases[0].1;
        let local = paint_rect(b);
        let rect = Rectangle {
            x: b.x + local.x,
            y: b.y + local.y,
            width: local.width,
            height: local.height,
        };
        assert_eq!(rect, b, "integer bounds must map to themselves");
    }
}
