//! Session persistence: the grid a live tab runs on and the split tree a session
//! file stores, converted in one place.
//!
//! `ghostwave::layout::Node` stays the on-disk shape, so the macroquad
//! front-end keeps reading the same files. A session stores a pane by its index
//! inside its tab, while a live tab addresses it by [`PaneId`]; the caller
//! supplies that mapping in both directions.

use ghostwave::layout::Node;
use iced::widget::pane_grid::{self, Configuration};

use crate::app::GridPane;
use crate::pane::PaneId;
use crate::panes_grid;

/// The grid for a session tree: one cell per leaf, a stack's panes sharing one
/// cell.
///
/// `id_of` maps an index in the session to the identity of the pane that index
/// names.
pub fn state_from_node(node: &Node, id_of: impl Fn(usize) -> PaneId) -> pane_grid::State<GridPane> {
    pane_grid::State::with_configuration(configuration(node, &id_of))
}

/// The session tree for a grid, as its layout reads today.
///
/// `index_of` maps a pane's identity back to its index in the session. Zoom is a
/// view state and has no tree of its own, so it is not persisted.
pub fn node_from_state(
    state: &pane_grid::State<GridPane>,
    index_of: impl Fn(PaneId) -> usize,
) -> Node {
    node_of(state, state.layout(), &index_of)
}

/// The grid configuration for a tree node.
///
/// A split keeps its ratio as written: a tree ratio is the share of the region
/// given to its `a` side, which is the ratio the grid stores for the same split.
fn configuration(node: &Node, id_of: &impl Fn(usize) -> PaneId) -> Configuration<GridPane> {
    match node {
        Node::Leaf { pane } => Configuration::Pane(GridPane::single(id_of(*pane))),
        Node::Stack { panes, active } => Configuration::Pane(GridPane {
            panes: panes.iter().map(|pane| id_of(*pane)).collect(),
            active: *active,
        }),
        Node::Split { dir, ratio, a, b } => Configuration::Split {
            axis: panes_grid::axis(*dir),
            ratio: *ratio,
            a: Box::new(configuration(a, id_of)),
            b: Box::new(configuration(b, id_of)),
        },
    }
}

/// The tree node for a grid node; a cell with one pane is a leaf, one with
/// several is a stack.
fn node_of(
    state: &pane_grid::State<GridPane>,
    node: &pane_grid::Node,
    index_of: &impl Fn(PaneId) -> usize,
) -> Node {
    match node {
        pane_grid::Node::Pane(cell) => match state.get(*cell) {
            Some(cell) if cell.panes.len() == 1 => Node::Leaf {
                pane: index_of(cell.panes[0]),
            },
            Some(cell) => Node::Stack {
                panes: cell.panes.iter().map(|pane| index_of(*pane)).collect(),
                active: cell.active,
            },
            None => Node::Leaf { pane: usize::MAX },
        },
        pane_grid::Node::Split {
            axis, ratio, a, b, ..
        } => Node::Split {
            dir: panes_grid::dir_of(*axis),
            ratio: *ratio,
            a: Box::new(node_of(state, a, index_of)),
            b: Box::new(node_of(state, b, index_of)),
        },
    }
}

#[cfg(test)]
mod tests {
    //! A session tree survives a trip through the grid a tab runs on: out to the
    //! grid, back to the tree, unchanged in shape, ratios and stacking.

    use super::*;
    use ghostwave::layout::SplitDir;

    /// An unsaved session addresses its panes by their index, as these do.
    fn id_of(index: usize) -> PaneId {
        PaneId(index)
    }

    fn index_of(id: PaneId) -> usize {
        id.0
    }

    fn leaf(pane: usize) -> Node {
        Node::Leaf { pane }
    }

    fn split(dir: SplitDir, ratio: f32, a: Node, b: Node) -> Node {
        Node::Split {
            dir,
            ratio,
            a: Box::new(a),
            b: Box::new(b),
        }
    }

    fn round_trip(node: &Node) -> Node {
        node_from_state(&state_from_node(node, id_of), index_of)
    }

    /// `Node` has no `PartialEq`, so trees are compared by their rendering, which
    /// distinguishes every field these trees carry.
    fn assert_same(before: &Node, after: &Node) {
        assert_eq!(
            format!("{before:?}"),
            format!("{after:?}"),
            "the tree changed on the way through the grid"
        );
    }

    #[test]
    fn single_pane_round_trips() {
        assert_same(&leaf(0), &round_trip(&leaf(0)));
    }

    #[test]
    fn right_split_round_trips() {
        let tree = split(SplitDir::Horizontal, 0.35, leaf(0), leaf(1));
        assert_same(&tree, &round_trip(&tree));
    }

    #[test]
    fn down_split_round_trips() {
        let tree = split(SplitDir::Vertical, 0.5, leaf(0), leaf(1));
        assert_same(&tree, &round_trip(&tree));
    }

    #[test]
    fn nested_splits_round_trip() {
        let tree = split(
            SplitDir::Horizontal,
            0.5,
            split(SplitDir::Vertical, 0.25, leaf(0), leaf(1)),
            split(
                SplitDir::Vertical,
                0.75,
                leaf(2),
                split(SplitDir::Horizontal, 0.5, leaf(3), leaf(4)),
            ),
        );
        assert_same(&tree, &round_trip(&tree));
    }

    #[test]
    fn stack_round_trips_with_its_active_pane() {
        let tree = Node::Stack {
            panes: vec![0, 1],
            active: 1,
        };
        let state = state_from_node(&tree, id_of);
        let cell = state
            .iter()
            .map(|(_, cell)| cell)
            .next()
            .expect("a stack is one cell");
        assert_eq!(cell.panes, vec![PaneId(0), PaneId(1)]);
        assert_eq!(cell.active, 1, "the stack's active pane is kept");
        assert_eq!(cell.active_id(), Some(PaneId(1)));
        assert_same(&tree, &round_trip(&tree));
    }
}
