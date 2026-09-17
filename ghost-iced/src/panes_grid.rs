//! iced `pane_grid` pane layer: the alternative to the hand-rolled canvas pane
//! layer ([`crate::pane_canvas`]), selected by `GHOST_ICED_PANEGRID`.
//!
//! `ghostwave::layout::Node` stays the single source of truth: the
//! [`pane_grid::State`] held in [`GridCache`] is derived from the active tab's
//! tree, and every grid interaction is written back into that tree — a click
//! focuses the pane under it, a split drag moves the split's ratio, a pane drag
//! re-arranges the tree. The keymap layer therefore keeps operating on one tree
//! and both pane layers describe the same session.
//!
//! Geometry mirrors the canvas layer while frames are shown: a [`SPACING`] px
//! gutter between panes with the window background showing through it, each
//! pane's terminal area inset by [`PANE_INSET`] px inside its grid region (half
//! the gutter on either side, so the frame stroke lands in the gutter), a
//! [`TITLE_H`] px label band as the pane's title bar, and a frame stroke in
//! `theme.frame_active` (2 px, focused pane) or `theme.frame` (1 px). Frames off
//! mirrors the canvas layer's touching panes: no gutter, no inset, no band.
//!
//! Terminal content stays the canvas layer's: one [`PaneCanvas`] per pane paints
//! that pane at its own bounds with the shared painter.
//!
//! Quirks: `ghostwave::layout::Node::Stack` has no `pane_grid` equivalent, so a
//! stack contributes its active pane only. Floating panes are out of scope.

use std::collections::BTreeMap;

use ghostwave::app::{STATUS_BAR_H, TAB_BAR_H};
use ghostwave::layout::{Dir, Node, Rect, SplitDir};
use ghostwave::term::{CellDims, MouseEv, MouseEvKind};
use ghostwave::theme::Theme as GhostTheme;
use iced::widget::canvas::{self, Action, Event, Frame, Geometry};
use iced::widget::pane_grid::{self, Axis, Configuration, Content, Pane, PaneGrid, Split, TitleBar};
use iced::widget::{Canvas, container, text};
use iced::{
    Background, Border, Element, Fill, Font, Length, Padding, Point, Rectangle, Renderer, Size,
    Theme,
    alignment, mouse,
};

use crate::Message;
use crate::app::AppState;
use crate::fonts;
use crate::keys;
use crate::pane::SharedPane;
use crate::pane_canvas::{self, CanvasState, ChromeData, TerminalCanvas};

/// Environment variable selecting this layer; unset, empty or `0` keeps the
/// canvas layer.
const ENV: &str = "GHOST_ICED_PANEGRID";

/// Gutter between neighbouring panes, in px.
pub const SPACING: f32 = 4.0;

/// Inset of a pane's terminal area inside its grid region, in px.
pub const PANE_INSET: f32 = 2.0;

/// Height of a pane's label band (its title bar), in px.
pub const TITLE_H: f32 = 14.0;

/// A pane's smallest extent on either axis, in px.
pub const MIN_SIZE: f32 = 50.0;

/// Ratio limits the derived state is built with, mirroring the clamps
/// `ghostwave::layout` applies when it paints a tree.
const RATIO_MIN: f32 = 0.05;
const RATIO_MAX: f32 = 0.95;

/// Whether the pane layer is iced's `pane_grid`.
pub fn enabled() -> bool {
    std::env::var(ENV).is_ok_and(|v| !v.is_empty() && v != "0")
}

/// Gutter width, in px, for a pane layer with or without frames.
pub fn spacing(frames: bool) -> f32 {
    if frames { SPACING } else { 0.0 }
}

/// Inset of a pane's terminal area inside its grid region, in px.
pub fn inset(frames: bool) -> f32 {
    if frames { PANE_INSET } else { 0.0 }
}

/// Height of a pane's label band, in px; zero when frames are off.
pub fn title_h(frames: bool) -> f32 {
    if frames { TITLE_H } else { 0.0 }
}

/// The `pane_grid` state derived from one tab's split tree, plus what is needed
/// to write a split drag back into that tree.
pub struct GridCache {
    /// Tab this state was derived from.
    pub tab: usize,
    /// `AppState::tree_revision` this state was derived from.
    pub revision: u64,
    /// One grid pane per terminal pane; `T` is the pane's index into
    /// `Tab::panes`. Pane and split ids are a function of the tree's shape only,
    /// so they stay valid across a rebuild.
    pub state: pane_grid::State<usize>,
    /// The tree path of every split, so a split drag can move the same split in
    /// `Tab::root`.
    pub splits: BTreeMap<Split, Vec<u8>>,
}

impl GridCache {
    /// Derives the grid state from `root`.
    pub fn derive(tab: usize, revision: u64, root: &Node) -> Self {
        let state = pane_grid::State::with_configuration(config(root));
        let mut splits = BTreeMap::new();
        collect_splits(state.layout(), &mut Vec::new(), &mut splits);
        Self {
            tab,
            revision,
            state,
            splits,
        }
    }

    /// Whether this state was derived from `tab` at `revision`.
    pub fn is_fresh(&self, tab: usize, revision: u64) -> bool {
        self.tab == tab && self.revision == revision
    }
}

/// `ghostwave::layout::Node` → `pane_grid` configuration. A stack contributes its
/// active pane only.
fn config(node: &Node) -> Configuration<usize> {
    match node {
        Node::Leaf { pane } => Configuration::Pane(*pane),
        Node::Stack { panes, active } => Configuration::Pane(panes[*active]),
        Node::Split { dir, ratio, a, b } => Configuration::Split {
            axis: axis(*dir),
            // Matches what the tree painter would draw for the same tree.
            ratio: ratio.clamp(RATIO_MIN, RATIO_MAX),
            a: Box::new(config(a)),
            b: Box::new(config(b)),
        },
    }
}

/// The grid axis a tree split direction splits along. `SplitDir::Horizontal` is
/// side-by-side, which is iced's vertical axis: the two enums are crossed.
pub fn axis(dir: SplitDir) -> Axis {
    match dir {
        SplitDir::Horizontal => Axis::Vertical,
        SplitDir::Vertical => Axis::Horizontal,
    }
}

/// Records every split id against its tree path. Ids are handed out in the same
/// depth-first order [`config`] walks the tree in, so the walk here pairs them up.
fn collect_splits(node: &pane_grid::Node, path: &mut Vec<u8>, out: &mut BTreeMap<Split, Vec<u8>>) {
    if let pane_grid::Node::Split { id, a, b, .. } = node {
        path.push(0);
        collect_splits(a, path, out);
        path.pop();
        path.push(1);
        collect_splits(b, path, out);
        path.pop();
        out.insert(*id, path.clone());
    }
}

/// Pane rects in content coordinates, from the derived state's own layout:
/// `pane_grid` lays the panes out with its own `pane_regions`, so deriving them
/// from the tree instead would let the ptys and the painted panes drift apart.
///
/// Each rect is a pane's terminal area: inside the frame stroke and below the
/// label band.
pub fn pane_rects(cache: &GridCache, content: Rect, frames: bool) -> Vec<(usize, Rect)> {
    let (spacing, inset, title) = (spacing(frames), inset(frames), title_h(frames));
    let regions = cache.state.layout().pane_regions(
        spacing,
        MIN_SIZE,
        Size::new(content.w.max(1.0), content.h.max(1.0)),
    );
    regions
        .into_iter()
        .filter_map(|(pane, region)| {
            let id = *cache.state.get(pane)?;
            Some((
                id,
                Rect {
                    x: content.x + region.x + inset,
                    y: content.y + region.y + title + inset,
                    w: (region.width - 2.0 * inset).max(1.0),
                    h: (region.height - title - 2.0 * inset).max(1.0),
                },
            ))
        })
        .collect()
}

/// Converts a `pane_grid` split ratio into the tree's ratio for the same split.
///
/// A grid ratio is a position inside the split's region, which spans both panes
/// plus the gutter; a tree ratio is the share of that region given to the `a`
/// side (`ghostwave::layout::SplitDir::Horizontal`'s left pane, `Vertical`'s top
/// pane). This mirrors `pane_grid::Axis::split`, so a drag that is written back
/// does not shift the split by the rounding difference.
pub fn tree_ratio(
    state: &pane_grid::State<usize>,
    split: Split,
    ratio: f32,
    spacing: f32,
    bounds: Size,
) -> Option<f32> {
    let regions = state.layout().split_regions(spacing, MIN_SIZE, bounds);
    let (axis, region, _) = regions.get(&split)?;
    let share = match axis {
        Axis::Horizontal => {
            let top = (region.height * ratio - spacing / 2.0)
                .round()
                .clamp(MIN_SIZE, (region.height - MIN_SIZE - spacing).max(MIN_SIZE));
            top / region.height.max(1.0)
        }
        Axis::Vertical => {
            let left = (region.width * ratio - spacing / 2.0)
                .round()
                .clamp(MIN_SIZE, (region.width - MIN_SIZE - spacing).max(MIN_SIZE));
            left / region.width.max(1.0)
        }
    };
    Some(share.clamp(RATIO_MIN, RATIO_MAX))
}

/// Where a dragged pane was dropped, in this crate's terms.
#[derive(Clone, Copy, Debug)]
pub enum DropTarget {
    /// An edge of the whole grid: move the pane to that side of the tab.
    Grid(Dir),
    /// Another pane; `dir` is the edge it was dropped on, `None` its centre,
    /// which trades the two panes' places.
    OnPane { pane: usize, dir: Option<Dir> },
}

/// Translates a grid drop target; `None` when the target pane is not in the
/// state.
pub fn drop_target(
    target: pane_grid::Target,
    state: &pane_grid::State<usize>,
) -> Option<DropTarget> {
    Some(match target {
        pane_grid::Target::Edge(edge) => DropTarget::Grid(dir_of(edge)),
        pane_grid::Target::Pane(pane, region) => {
            let pane = *state.get(pane)?;
            match region {
                pane_grid::Region::Center => DropTarget::OnPane { pane, dir: None },
                pane_grid::Region::Edge(edge) => DropTarget::OnPane {
                    pane,
                    dir: Some(dir_of(edge)),
                },
            }
        }
    })
}

/// The terminal pane a grid pane holds.
pub fn pane_of(state: &pane_grid::State<usize>, pane: Pane) -> Option<usize> {
    state.get(pane).copied()
}

/// iced's edge as this tree's direction.
fn dir_of(edge: pane_grid::Edge) -> Dir {
    match edge {
        pane_grid::Edge::Top => Dir::Up,
        pane_grid::Edge::Bottom => Dir::Down,
        pane_grid::Edge::Left => Dir::Left,
        pane_grid::Edge::Right => Dir::Right,
    }
}

/// The pane-grid layer's view: the chrome canvas at the back, the grid over the
/// content area.
///
/// The canvas paints the tab bar, the status bar and the window background the
/// gutters show through, and keeps the pane rects empty so that panes, their
/// frames and their mouse handling belong to the grid alone.
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

    let Some(cache) = state
        .grid
        .as_ref()
        .filter(|cache| cache.is_fresh(state.active_tab, state.tree_revision))
    else {
        // Without a derived state there is no grid to show; the canvas layer's
        // chrome still is.
        return chrome_layer.into();
    };

    let layer = container(grid(cache, panes, mouse_tracking, state)).padding(Padding {
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

/// Builds the pane grid from the derived state: one pane per terminal pane, each
/// its own canvas under a title bar, framed by its own container border.
fn grid<'a>(
    cache: &'a GridCache,
    panes: Vec<(usize, SharedPane)>,
    mouse_tracking: Vec<usize>,
    state: &'a AppState,
) -> PaneGrid<'a, Message> {
    let frames = state.show_frames;
    let active = state.current().active;
    let theme = state.theme;
    let (cell, font_px, padding) = (state.cell, state.config.font_size, state.config.padding);
    let tab = state.active_tab;
    let inset = inset(frames);
    let font = fonts::primary();

    PaneGrid::new(&cache.state, move |pane, &pane_id, _maximized| {
        let handle = panes
            .iter()
            .find(|(id, _)| *id == pane_id)
            .map(|(_, handle)| handle.clone());
        let focused = pane_id == active;
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
        let body: Element<'a, Message> = match handle.clone() {
            Some(handle) => container(
                Canvas::new(PaneCanvas {
                    tab,
                    pane: pane_id,
                    active,
                    handle,
                    cell,
                    font_px,
                    padding,
                    tracking: mouse_tracking.contains(&pane_id),
                })
                .width(Fill)
                .height(Fill),
            )
            .padding(inset + frame_w)
            .into(),
            None => text("").into(),
        };
        let frame_color = if focused {
            theme.frame_active
        } else {
            theme.frame
        };
        let mut content = Content::new(body);
        if frames {
            // The pane's 1-based position in the tab, which is the order the grid
            // hands its panes out in.
            let index = cache
                .state
                .iter()
                .position(|(other, _)| *other == pane)
                .map(|i| i + 1)
                .unwrap_or(pane_id + 1);
            let label = handle
                .as_ref()
                .map(|handle| pane_canvas::pane_label(handle, index))
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
    /// The pane's id (index into `Tab::panes`).
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
