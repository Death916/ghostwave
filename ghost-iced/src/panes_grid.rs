//! iced `pane_grid` pane layer: the pane layer that hands the panes to iced.
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
//! A cell whose pane is not a terminal ([`ContentPane`]) paints that pane's own
//! widget in place of the canvas, clipped to the cell: a preview, an AI chat or
//! the visualizer. Such a cell gets no [`PaneCanvas`] and no mouse reporting, and
//! its label band names its kind.
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
    Background, Border, Element, Fill, Font, Length, Padding, Point, Rectangle, Renderer, Theme,
    alignment, mouse,
};

use crate::Message;
use crate::app::{AppState, GridPane, Tab};
use crate::fonts;
use crate::keys;
use crate::pane::{ContentPane, PaneEntry, PaneId, SharedPane};
use crate::pane_canvas::{self, CanvasState, ChromeData, TerminalCanvas};

/// Gutter between neighbouring cells, in px.
pub const SPACING: f32 = 4.0;

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
    let cols = ((width - 2.0 * padding) / cell.w as f32).max(1.0) as u16;
    let rows = ((height - 2.0 * padding) / cell.h as f32).max(1.0) as u16;
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

/// Gutter width, in px, for a pane layer with or without frames.
pub fn spacing(frames: bool) -> f32 {
    if frames { SPACING } else { 0.0 }
}

/// Inset of a pane's terminal area inside its cell, in px.
pub fn inset(frames: bool) -> f32 {
    if frames { PANE_INSET } else { 0.0 }
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

/// The pane-grid layer's view: the grid over the content area, and the chrome
/// canvas layered above it.
pub fn view<'a>(
    state: &'a AppState,
    mouse_tracking: Vec<usize>,
    chrome: ChromeData,
) -> Element<'a, Message> {
    let chrome_layer = Canvas::new(TerminalCanvas {
        theme: state.theme,
        chrome,
    })
    .width(Fill)
    .height(Fill);

    let layer = container(grid(state, mouse_tracking))
        .padding(Padding {
            top: TAB_BAR_H,
            right: 0.0,
            bottom: STATUS_BAR_H,
            left: 0.0,
        })
        .style(move |_theme| container::Style {
            background: Some(pane_canvas::rgb(state.theme.bg).into()),
            ..container::Style::default()
        });

    iced::widget::Stack::with_children(vec![layer.into(), chrome_layer.into()])
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
    let inset = inset(frames);
    let font = fonts::primary();
    let replace_title = state.config.dynamic_title != crate::config::DynamicTitleMode::Off;

    PaneGrid::new(&tab.grid, move |_cell, payload: &GridPane, _maximized| {
        let index = payload.active_id().and_then(|id| tab.index_of(id));
        let entry = index.and_then(|index| tab.entry(index));
        let focused = index == Some(active);
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
        let body: Element<'a, Message> = match entry {
            Some(PaneEntry::Term(handle)) => container(
                Canvas::new(PaneCanvas {
                    tab: tab_index,
                    pane: index.unwrap_or_default(),
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
            .padding(inset + frame_w)
            .into(),
            Some(PaneEntry::Content(content)) => content_body(content, inset + frame_w, theme),
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
            content = content.title_bar(TitleBar::new(label_band(&label, frame_color, font, theme)));
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
    .spacing(spacing(frames))
    .min_size(MIN_SIZE)
    .on_click(move |pane| {
        let index = click_to_pane_index(&tab.grid, tab, pane).unwrap_or(tab.active);
        Message::FocusPane(index)
    })
    .on_resize(0.0, |event| Message::GridResize {
        split: event.split,
        ratio: event.ratio,
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
fn content_body<'a>(content: &'a ContentPane, inset: f32, theme: GhostTheme) -> Element<'a, Message> {
    let body: Element<'a, Message> = match content {
        ContentPane::Preview { pane, .. } => pane.view::<Message>(&theme),
        ContentPane::Ai { id, pane } => {
            let id = *id;
            pane.view(&Theme::Dark).map(move |msg| match msg {
                crate::ai::AiMessage::InputChanged(_) => Message::Noop,
                msg => Message::Ai { id, msg },
            })
        }
        ContentPane::Visualizer { pane, .. } => Canvas::new(pane).width(Fill).height(Fill).into(),
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
    /// The pane's index into its tab.
    pub pane: usize,
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
                let point = cursor.position_over(bounds)?;
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
        let (cols, rows) = bounds_to_cols_rows(bounds.width, bounds.height, self.cell, self.padding);
        guard.resize(cols, rows, self.cell);

        let origin_offset = Point::new(
            bounds.x.round() - bounds.x,
            bounds.y.round() - bounds.y,
        );
        let pane_rect = Rectangle::new(origin_offset, bounds.size());

        pane_canvas::paint_pane(
            &mut frame,
            &mut guard,
            pane_rect,
            self.cell,
            self.padding,
            self.font_px,
            fonts::primary(),
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
            pane: self.pane,
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

#[cfg(test)]
mod tests {
    use super::*;
    use ghostwave::term::CellDims;
    use iced::widget::pane_grid::{self, Axis};

    #[test]
    fn bounds_to_cols_rows_calculations() {
        let cell = CellDims {
            w: 8,
            h: 16,
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
        assert_eq!(
            format_pane_label("term", 2, "fish", None, true),
            "fish"
        );
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
}
