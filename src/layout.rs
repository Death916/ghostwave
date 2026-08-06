//! Pane layout: a binary split tree with zellij-style stacked panes.
//!
//! Each tab owns a `Node` tree. Leaves reference panes by index into the
//! tab's `Vec<Pane>`. Stacked panes (zellij `s`) share one cell; the active
//! one is rendered.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SplitDir {
    /// Side-by-side (new pane to the right).
    Horizontal,
    /// One above the other (new pane below).
    Vertical,
}

/// A direction (zellij semantics: Left/Up grow the a-side).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    Left,
    Right,
    Up,
    Down,
}

impl Dir {
    pub fn axis(self) -> SplitDir {
        match self {
            Dir::Left | Dir::Right => SplitDir::Horizontal,
            Dir::Up | Dir::Down => SplitDir::Vertical,
        }
    }
    pub fn grows_a(self) -> bool {
        matches!(self, Dir::Left | Dir::Up)
    }
}

#[derive(Debug)]
pub enum Node {
    Split {
        dir: SplitDir,
        /// Fraction of the rect given to `a` (0.0..1.0).
        ratio: f32,
        a: Box<Node>,
        b: Box<Node>,
    },
    Leaf {
        pane: usize,
    },
    Stack {
        panes: Vec<usize>,
        active: usize,
    },
}

/// Path to a node: 0 = left/top child, 1 = right/bottom child.
pub type Path = Vec<u8>;

impl Node {
    /// Leaf or stack (a pane-holding node).
    pub fn is_leaf_like(&self) -> bool {
        matches!(self, Node::Leaf { .. } | Node::Stack { .. })
    }

    /// The pane id this node resolves to (leaf pane, or stack's active pane).
    pub fn pane_id(&self) -> usize {
        match self {
            Node::Leaf { pane } => *pane,
            Node::Stack { panes, active } => panes[*active],
            Node::Split { .. } => unreachable!("pane_id on split"),
        }
    }

    /// Walk to the node at `path`.
    pub fn at<'a>(&'a self, path: &[u8]) -> &'a Node {
        let mut node = self;
        for &c in path {
            node = match node {
                Node::Split { a, b, .. } => if c == 0 { a } else { b },
                _ => panic!("path into leaf"),
            };
        }
        node
    }

    pub fn at_mut<'a>(&'a mut self, path: &[u8]) -> &'a mut Node {
        let mut node = self;
        for &c in path {
            node = match node {
                Node::Split { a, b, .. } => if c == 0 { a } else { b },
                _ => panic!("path into leaf"),
            };
        }
        node
    }

    pub fn set_pane_at(&mut self, path: &[u8], pane: usize) {
        let node = self.at_mut(path);
        match node {
            Node::Leaf { pane: p } => *p = pane,
            Node::Stack { panes, active } => panes[*active] = pane,
            _ => {}
        }
    }

    /// All (path, pane_id) leaves in DFS order.
    pub fn leaves(&self) -> Vec<(Path, usize)> {
        let mut out = Vec::new();
        self.collect_leaves(&mut vec![], &mut out);
        out
    }

    fn collect_leaves(&self, path: &mut Path, out: &mut Vec<(Path, usize)>) {
        match self {
            Node::Split { a, b, .. } => {
                path.push(0);
                a.collect_leaves(path, out);
                path.pop();
                path.push(1);
                b.collect_leaves(path, out);
                path.pop();
            }
            Node::Leaf { pane } => out.push((path.clone(), *pane)),
            Node::Stack { panes, active } => out.push((path.clone(), panes[*active])),
        }
    }
}

/// Replace the leaf at `path` with a split containing the old pane + a new pane.
pub fn split_at(root: &mut Node, path: &[u8], dir: SplitDir, new_pane: usize) {
    let node = root.at_mut(path);
    match node {
        Node::Leaf { pane } => {
            let old = *pane;
            *node = Node::Split {
                dir,
                ratio: 0.5,
                a: Box::new(Node::Leaf { pane: old }),
                b: Box::new(Node::Leaf { pane: new_pane }),
            };
        }
        // Splitting inside a stack: wrap the whole stack, new pane becomes sibling.
        Node::Stack { .. } => {
            let stack = std::mem::replace(node, Node::Leaf { pane: 0 });
            *node = Node::Split {
                dir,
                ratio: 0.5,
                a: Box::new(stack),
                b: Box::new(Node::Leaf { pane: new_pane }),
            };
        }
        _ => unreachable!("split_at on split"),
    }
}

/// Push a new pane into the stack at `path` (zellij stacked panes).
pub fn stack_at(root: &mut Node, path: &[u8], new_pane: usize) {
    let node = root.at_mut(path);
    match node {
        Node::Stack { panes, active } => {
            panes.push(new_pane);
            *active = panes.len() - 1;
        }
        Node::Leaf { pane } => {
            let old = *pane;
            *node = Node::Stack {
                panes: vec![old, new_pane],
                active: 1,
            };
        }
        _ => unreachable!("stack_at on split"),
    }
}

/// Remove the pane at `path`. Returns the removed pane id (or None when a
/// stack shrank but survives). The tree is collapsed where possible.
pub fn close_at(root: &mut Node, path: &[u8]) -> Option<usize> {
    let remove_from_stack = |stack: &mut Node| -> Option<usize> {
        if let Node::Stack { panes, active } = stack {
            if panes.len() > 1 {
                panes.remove(*active);
                if *active >= panes.len() {
                    *active = panes.len() - 1;
                }
                None
            } else {
                Some(panes[0])
            }
        } else {
            None
        }
    };

    if path.is_empty() {
        let removed = match root {
            Node::Leaf { pane } => Some(*pane),
            Node::Stack { .. } => remove_from_stack(root),
            _ => None,
        };
        if removed.is_some() {
            *root = Node::Leaf { pane: usize::MAX }; // mark empty; app fixes
        }
        return removed;
    }

    let parent_path = &path[..path.len() - 1];
    let side = *path.last().unwrap();
    let removed = match root.at_mut(parent_path) {
        Node::Split { a, b, .. } => {
            let target = if side == 0 { a } else { b };
            match &mut **target {
                Node::Leaf { pane } => Some(*pane),
                Node::Stack { .. } => remove_from_stack(target),
                _ => None,
            }
        }
        _ => None,
    };

    if removed.is_some() {
        // Collapse the parent split if the surviving child is leaf-like.
        let parent = root.at_mut(parent_path);
        if let Node::Split { a, b, .. } = parent {
            let survivor = if side == 0 { b } else { a };
            if survivor.is_leaf_like() {
                let replacement =
                    std::mem::replace(survivor, Box::new(Node::Leaf { pane: usize::MAX }));
                *parent = *replacement;
            }
        }
    }
    removed
}

/// Resize the nearest ancestor split whose axis matches `dir`'s axis.
/// Left/Up grow the `a` side, Right/Down grow `b`.
pub fn resize_at(root: &mut Node, path: &[u8], dir: Dir, delta: f32) {
    let axis = dir.axis();
    let grow_a = dir.grows_a();
    let mut p: &[u8] = path;
    while !p.is_empty() {
        let parent_path = &p[..p.len() - 1];
        let axis_ok = match root.at(parent_path) {
            Node::Split { dir: d, .. } => *d == axis,
            _ => false,
        };
        if axis_ok {
            if let Node::Split { ratio, .. } = root.at_mut(parent_path) {
                let new = if grow_a { *ratio + delta } else { *ratio - delta };
                *ratio = new.clamp(0.1, 0.9);
            }
            return;
        }
        p = parent_path;
    }
}

/// Find the leaf path for a pane id.
pub fn path_to_pane(root: &Node, pane: usize) -> Option<Path> {
    root.leaves()
        .into_iter()
        .find(|(_, p)| *p == pane)
        .map(|(p, _)| p)
}

/// Directional focus move: pick the leaf whose rect is closest in `dir`.
pub fn focus_in_direction(leaves: &[(usize, Rect)], from_pane: usize, dir: Dir) -> Option<usize> {
    let from = leaves.iter().find(|(p, _)| *p == from_pane)?.1;
    let mut best: Option<(f32, usize)> = None;
    for (p, r) in leaves {
        if *p == from_pane {
            continue;
        }
        let (gap, center_dist) = match dir {
            Dir::Left => {
                if r.x + r.w > from.x + 0.5 {
                    continue;
                }
                (from.x - (r.x + r.w), (r.center().1 - from.center().1).abs())
            }
            Dir::Right => {
                if r.x < from.x + from.w - 0.5 {
                    continue;
                }
                (r.x - (from.x + from.w), (r.center().1 - from.center().1).abs())
            }
            Dir::Up => {
                if r.y + r.h > from.y + 0.5 {
                    continue;
                }
                (from.y - (r.y + r.h), (r.center().0 - from.center().0).abs())
            }
            Dir::Down => {
                if r.y < from.y + from.h - 0.5 {
                    continue;
                }
                (r.y - (from.y + from.h), (r.center().0 - from.center().0).abs())
            }
        };
        let score = gap + center_dist * 0.5;
        if best.map_or(true, |(bs, _)| score < bs) {
            best = Some((score, *p));
        }
    }
    best.map(|(_, p)| p)
}

/// Cycle to the next leaf in DFS order (zellij SwitchFocus).
pub fn next_leaf(root: &Node, from_pane: usize) -> Option<usize> {
    let leaves = root.leaves();
    let idx = leaves.iter().position(|(_, p)| *p == from_pane)?;
    Some(leaves[(idx + 1) % leaves.len()].1)
}

/// Find the leaf that comes after the pane in DFS order (for move mode n).
pub fn leaf_after(root: &Node, from_pane: usize) -> Option<usize> {
    let leaves = root.leaves();
    let idx = leaves.iter().position(|(_, p)| *p == from_pane)?;
    if idx + 1 < leaves.len() {
        Some(leaves[idx + 1].1)
    } else {
        Some(leaves[0].1)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn inset(&self, d: f32) -> Rect {
        Rect {
            x: self.x + d,
            y: self.y + d,
            w: (self.w - 2.0 * d).max(0.0),
            h: (self.h - 2.0 * d).max(0.0),
        }
    }
    pub fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && px < self.x + self.w && py >= self.y && py < self.y + self.h
    }
    pub fn center(&self) -> (f32, f32) {
        (self.x + self.w / 2.0, self.y + self.h / 2.0)
    }
}

/// Compute pixel rects for every leaf pane (stack active pane gets full rect).
pub fn layout(root: &Node, rect: Rect) -> Vec<(usize, Rect)> {
    let mut out = Vec::new();
    layout_node(root, rect, &mut out);
    out
}

fn layout_node(node: &Node, rect: Rect, out: &mut Vec<(usize, Rect)>) {
    match node {
        Node::Leaf { pane } => out.push((*pane, rect)),
        Node::Stack { panes, active } => out.push((panes[*active], rect)),
        Node::Split { dir, ratio, a, b } => {
            let r = (*ratio).clamp(0.05, 0.95);
            match dir {
                SplitDir::Horizontal => {
                    let wa = rect.w * r;
                    layout_node(a, Rect { x: rect.x, y: rect.y, w: wa, h: rect.h }, out);
                    layout_node(
                        b,
                        Rect { x: rect.x + wa, y: rect.y, w: rect.w - wa, h: rect.h },
                        out,
                    );
                }
                SplitDir::Vertical => {
                    let ha = rect.h * r;
                    layout_node(a, Rect { x: rect.x, y: rect.y, w: rect.w, h: ha }, out);
                    layout_node(
                        b,
                        Rect { x: rect.x, y: rect.y + ha, w: rect.w, h: rect.h - ha },
                        out,
                    );
                }
            }
        }
    }
}

/// Swap the panes at two paths (zellij move mode).
pub fn swap_panes(root: &mut Node, path_a: &[u8], path_b: &[u8]) {
    let a = root.at(path_a).pane_id();
    let b = root.at(path_b).pane_id();
    root.set_pane_at(path_a, b);
    root.set_pane_at(path_b, a);
}

/// Cycle a stack's active pane at `path` (if it is a stack).
pub fn cycle_stack(root: &mut Node, path: &[u8]) -> bool {
    if let Node::Stack { panes, active } = root.at_mut(path) {
        *active = (*active + 1) % panes.len();
        true
    } else {
        false
    }
}

/// Is the node at `path` a stack?
pub fn is_stack(root: &Node, path: &[u8]) -> bool {
    matches!(root.at(path), Node::Stack { .. })
}
