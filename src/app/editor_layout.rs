//! The editor group layout tree.
//!
//! Croft hoists the *active* editor group into `App::editor` so the hundreds of
//! `self.editor` call sites always operate on the focused group. This module
//! holds everything else: the spatial TREE of groups and the operations that
//! split, move focus between, and collapse them. Exactly one leaf in the tree
//! is the ACTIVE placeholder — its `EditorTabs` is `None` because those tabs
//! live, hoisted, in `App::editor`. Every other leaf owns its `EditorTabs`.
//!
//! The layout maths (`leaf_rects`) and the structural operations are pure and
//! unit-tested here, independent of `App` and rendering.

use ratatui::layout::Rect;

use crate::widgets::editor::EditorTabs;

/// Orientation of a split node: side-by-side columns or stacked rows.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SplitDir {
    /// Children laid out left-to-right (VS Code "Split Left/Right").
    Horizontal,
    /// Children laid out top-to-bottom (VS Code "Split Up/Down").
    Vertical,
}

/// One of the four screen directions a Split or Move targets (VS Code's
/// Split/Move Up/Down/Left/Right).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Dir {
    Up,
    Down,
    Left,
    Right,
}

impl Dir {
    /// The split axis this direction lives on: Left/Right tile horizontally,
    /// Up/Down vertically.
    pub fn split_dir(self) -> SplitDir {
        match self {
            Self::Left | Self::Right => SplitDir::Horizontal,
            Self::Up | Self::Down => SplitDir::Vertical,
        }
    }

    /// Whether a new group created in this direction goes AFTER the existing one
    /// (Right / Down) rather than before it (Left / Up).
    pub fn places_new_after(self) -> bool {
        matches!(self, Self::Right | Self::Down)
    }
}

/// A node in the layout tree: either a single group (leaf) or a split of
/// child nodes along one direction.
pub enum LayoutNode {
    /// A group. `None` marks the single ACTIVE leaf whose tabs are hoisted
    /// into `App::editor`; `Some` leaves own their tabs.
    Leaf(Option<EditorTabs>),
    /// A split of two-or-more children along `dir`, each carrying a relative
    /// `weight` that apportions the available length.
    Split {
        dir: SplitDir,
        children: Vec<LayoutChild>,
    },
}

/// A child of a [`LayoutNode::Split`]: the sub-node plus its relative size
/// weight (larger = wider/taller share of the parent's length).
pub struct LayoutChild {
    pub node: LayoutNode,
    pub weight: u16,
}

/// A draggable boundary between two adjacent groups in the rendered grid. One
/// per internal child boundary of every split node, so a grid of any depth has
/// every seam independently hit-testable and draggable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Seam {
    /// The owning split's orientation. `Horizontal` → a VERTICAL seam line the
    /// user drags left/right; `Vertical` → a HORIZONTAL seam dragged up/down.
    pub dir: SplitDir,
    /// Screen coordinate of the seam line: `x` for a horizontal split, `y` for
    /// a vertical split.
    pub pos: u16,
    /// Perpendicular extent `[cross_start, cross_end)` the seam spans (rows for
    /// a vertical line, columns for a horizontal line) — so seams in different
    /// regions of a grid don't claim each other's cells.
    pub cross_start: u16,
    pub cross_end: u16,
    /// Along-axis start of the owning split node and its total length, plus the
    /// child boundary index — enough to repartition the two adjacent groups on
    /// a drag without disturbing the rest of the grid.
    pub node_axis_start: u16,
    pub node_axis_len: u16,
    /// Path of child indices from the root to the owning split node.
    pub path: Vec<usize>,
    /// The seam sits between child `boundary` and `boundary + 1`.
    pub boundary: usize,
}

/// Lay every leaf out within `area`, returning one [`Rect`] per leaf in
/// depth-first (left-to-right / top-to-bottom) order — the same order
/// [`LayoutNode`] iteration visits leaves. Splits apportion the available
/// length by child weight, clamped so no child falls below `min`.
pub fn leaf_rects(node: &LayoutNode, area: Rect, min: u16) -> Vec<Rect> {
    match node {
        LayoutNode::Leaf(_) => vec![area],
        LayoutNode::Split { dir, children } => {
            let weights: Vec<u16> = children.iter().map(|c| c.weight).collect();
            let horizontal = *dir == SplitDir::Horizontal;
            let total = if horizontal { area.width } else { area.height };
            let lengths = apportion(total, &weights, min);
            let mut out = Vec::new();
            let mut offset = if horizontal { area.x } else { area.y };
            for (child, len) in children.iter().zip(lengths) {
                let sub = if horizontal {
                    Rect::new(offset, area.y, len, area.height)
                } else {
                    Rect::new(area.x, offset, area.width, len)
                };
                out.extend(leaf_rects(&child.node, sub, min));
                offset = offset.saturating_add(len);
            }
            out
        }
    }
}

/// Split `total` cells among children by relative `weights` (largest-remainder
/// rounding for an exact tiling), then raise any child below `min` to `min` by
/// taking cells from the largest sibling. Best-effort when `total` can't seat
/// every child at `min` (a degenerate tiny pane).
fn apportion(total: u16, weights: &[u16], min: u16) -> Vec<u16> {
    let n = weights.len();
    if n == 0 {
        return Vec::new();
    }
    if n == 1 {
        return vec![total];
    }
    let total_i = total as i64;
    let sum_w: i64 = weights.iter().map(|&w| w.max(1) as i64).sum::<i64>().max(1);
    let mut alloc: Vec<i64> = Vec::with_capacity(n);
    let mut remainders: Vec<(i64, usize)> = Vec::with_capacity(n);
    let mut used: i64 = 0;
    for (i, &w) in weights.iter().enumerate() {
        let exact = total_i * w.max(1) as i64;
        alloc.push(exact / sum_w);
        remainders.push((exact % sum_w, i));
        used += exact / sum_w;
    }
    // Hand out the rounding leftover to the largest fractional remainders.
    remainders.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    let mut leftover = total_i - used;
    let mut k = 0usize;
    while leftover > 0 && !remainders.is_empty() {
        alloc[remainders[k % n].1] += 1;
        leftover -= 1;
        k += 1;
    }
    // Enforce the minimum, stealing from the largest surplus sibling.
    let min_i = min as i64;
    while let Some(d) = alloc.iter().position(|&a| a < min_i) {
        let need = min_i - alloc[d];
        match (0..n)
            .filter(|&i| i != d && alloc[i] > min_i)
            .max_by_key(|&i| alloc[i])
        {
            Some(donor) => {
                let take = need.min(alloc[donor] - min_i);
                if take == 0 {
                    break;
                }
                alloc[donor] -= take;
                alloc[d] += take;
            }
            None => {
                alloc[d] = min_i;
                break;
            }
        }
    }
    alloc.into_iter().map(|a| a.max(0) as u16).collect()
}

/// The editor group layout: the tree root, with the active group hoisted out
/// into `App::editor` (the unique `Leaf(None)`). All structural mutations go
/// through here so the "exactly one active leaf" invariant is centralised.
pub struct EditorLayout {
    root: LayoutNode,
}

impl Default for EditorLayout {
    fn default() -> Self {
        Self::single()
    }
}

impl EditorLayout {
    /// A fresh, unsplit layout: a single active leaf (its tabs live in
    /// `App::editor`).
    pub fn single() -> Self {
        Self {
            root: LayoutNode::Leaf(None),
        }
    }

    /// True when more than one group exists (the editor pane is split).
    pub fn is_split(&self) -> bool {
        self.leaf_count() > 1
    }

    /// Total number of groups (leaves) in the tree.
    pub fn leaf_count(&self) -> usize {
        count_leaves(&self.root)
    }

    /// Lay out every group within `area`, one [`Rect`] per leaf in depth-first
    /// order (matching [`Self::leaf_count`] / active indexing).
    pub fn leaf_rects(&self, area: Rect, min: u16) -> Vec<Rect> {
        leaf_rects(&self.root, area, min)
    }

    /// Every draggable seam in the grid laid out within `area`, one per internal
    /// child boundary of every split node (depth-first).
    pub fn seams(&self, area: Rect, min: u16) -> Vec<Seam> {
        let mut out = Vec::new();
        let mut path = Vec::new();
        collect_seams(&self.root, area, min, &mut path, &mut out);
        out
    }

    /// Apply a seam drag: move the boundary to `pointer` (a screen `x` for a
    /// horizontal split, `y` for a vertical split) by repartitioning ONLY the
    /// two groups the seam separates, leaving the rest of the grid fixed. The
    /// owning split's child weights are first normalised to their current cell
    /// lengths so adjusting two never disturbs the others.
    pub fn drag_seam(&mut self, seam: &Seam, pointer: u16, min: u16) {
        let Some(node) = node_at_path(&mut self.root, &seam.path) else {
            return;
        };
        let LayoutNode::Split { children, .. } = node else {
            return;
        };
        if seam.boundary + 1 >= children.len() {
            return;
        }
        // Normalise to current cell lengths so only the dragged pair moves.
        let weights: Vec<u16> = children.iter().map(|c| c.weight).collect();
        let lengths = apportion(seam.node_axis_len, &weights, min);
        for (c, len) in children.iter_mut().zip(&lengths) {
            c.weight = (*len).max(1);
        }
        let b = seam.boundary;
        let combined = lengths[b].saturating_add(lengths[b + 1]);
        let first_start =
            seam.node_axis_start + lengths[..b].iter().copied().map(u32::from).sum::<u32>() as u16;
        let max_first = combined.saturating_sub(min).max(min);
        let new_first = pointer
            .saturating_sub(first_start)
            .clamp(min.min(combined), max_first);
        children[b].weight = new_first.max(1);
        children[b + 1].weight = combined.saturating_sub(new_first).max(1);
    }

    /// Depth-first index of the active leaf (the hoisted group), i.e. how many
    /// leaves precede it left-to-right / top-to-bottom. `0` when unsplit.
    pub fn active_dfs_index(&self) -> usize {
        let mut idx = 0;
        let mut found = 0;
        visit_leaves(&self.root, &mut |is_active| {
            if is_active {
                found = idx;
            }
            idx += 1;
        });
        found
    }

    /// Split the active leaf in two: the currently focused group's `existing`
    /// tabs settle into a new sibling leaf, and the active placeholder moves to
    /// the freshly focused group (whose tabs the caller swaps into
    /// `App::editor`). `new_after` puts the new active group after the existing
    /// one (right / below) — the "Split Editor Right" default.
    pub fn split_active(&mut self, existing: EditorTabs, dir: SplitDir, new_after: bool) {
        let mut existing = Some(existing);
        split_active_rec(&mut self.root, &mut existing, dir, new_after);
    }

    /// Move focus to the leaf at depth-first index `target`. The currently
    /// focused group's `hoisted` tabs settle into the old active leaf; the
    /// target leaf's tabs are returned for the caller to hoist into
    /// `App::editor`.
    pub fn refocus_to_dfs(&mut self, hoisted: EditorTabs, target: usize) -> EditorTabs {
        set_active_tabs(&mut self.root, hoisted);
        let mut idx = 0;
        take_leaf_at(&mut self.root, target, &mut idx)
            .expect("target dfs index must point at a real leaf")
    }

    /// Collapse the (empty) active leaf away and promote a surviving group to
    /// become the new active group, whose tabs are returned. Used when the
    /// focused group's last tab closes.
    pub fn collapse_active(&mut self) -> EditorTabs {
        remove_active(&mut self.root);
        activate_first(&mut self.root).expect("a surviving leaf must remain to promote")
    }

    /// Mutable references to every non-active group, for fan-out work the app
    /// must apply to all groups (LSP tokens, diagnostics, theme, focus flags).
    pub fn inactive_groups_mut(&mut self) -> Vec<&mut EditorTabs> {
        let mut out = Vec::new();
        collect_inactive_mut(&mut self.root, &mut out);
        out
    }

    /// Shared references to every non-active group, for read-only fan-out
    /// (e.g. collecting the set of open paths across all groups).
    pub fn inactive_groups(&self) -> Vec<&EditorTabs> {
        let mut out = Vec::new();
        collect_inactive(&self.root, &mut out);
        out
    }

    /// Reference to the group at depth-first index `dfs`, if that leaf is an
    /// inactive (owned) group. Returns `None` for the active placeholder leaf
    /// (whose tabs live in `App::editor`) or an out-of-range index.
    pub fn group_ref_at_dfs(&self, dfs: usize) -> Option<&EditorTabs> {
        let mut idx = 0;
        group_ref_at(&self.root, dfs, &mut idx)
    }

    /// Depth-first index of the inactive group whose most-recent render area
    /// contains the cell `(col, row)`, if any. Used to route a click in a
    /// non-focused group so focus moves there first. The active group is
    /// excluded (its area is tracked on `App::editor`, outside the tree).
    pub fn inactive_dfs_index_at(&self, col: u16, row: u16) -> Option<usize> {
        let mut idx = 0;
        inactive_index_at(&self.root, col, row, &mut idx)
    }

    /// Orientation of the root split, or `None` when unsplit (a single leaf).
    /// A test-only accessor for asserting which split a directional op built;
    /// production code reads the full grid via [`Self::seams`]/[`Self::leaf_rects`].
    #[cfg(test)]
    pub fn root_split_dir(&self) -> Option<SplitDir> {
        match &self.root {
            LayoutNode::Split { dir, .. } => Some(*dir),
            LayoutNode::Leaf(_) => None,
        }
    }

    /// Depth-first index of the inactive group spatially adjacent to the active
    /// group across its `dir` edge, if any. Adjacency = a shared edge with a
    /// nonzero perpendicular overlap; the neighbour with the largest overlap
    /// wins. Used by Move <dir> to find the destination group.
    pub fn neighbor_leaf_in_dir(&self, area: Rect, min: u16, dir: Dir) -> Option<usize> {
        let rects = self.leaf_rects(area, min);
        let active = self.active_dfs_index();
        let a = *rects.get(active)?;
        let mut best: Option<(usize, u16)> = None;
        for (i, r) in rects.iter().enumerate() {
            if i == active {
                continue;
            }
            let (adjacent, overlap) = match dir {
                Dir::Left => (r.x + r.width == a.x, vertical_overlap(*r, a)),
                Dir::Right => (a.x + a.width == r.x, vertical_overlap(*r, a)),
                Dir::Up => (r.y + r.height == a.y, horizontal_overlap(*r, a)),
                Dir::Down => (a.y + a.height == r.y, horizontal_overlap(*r, a)),
            };
            if adjacent && overlap > 0 && best.is_none_or(|(_, o)| overlap > o) {
                best = Some((i, overlap));
            }
        }
        best.map(|(i, _)| i)
    }

    /// Drop every inactive group for which `is_blank` holds (an emptied source
    /// after a Move), collapsing any split left with a single child. The active
    /// (hoisted) leaf is never touched.
    pub fn prune_blank_inactive(&mut self, is_blank: impl Fn(&EditorTabs) -> bool) {
        prune_blank_rec(&mut self.root, &is_blank);
    }
}

/// Cells of vertical overlap between two rects (0 when they don't share rows).
fn vertical_overlap(p: Rect, q: Rect) -> u16 {
    let top = p.y.max(q.y);
    let bottom = (p.y + p.height).min(q.y + q.height);
    bottom.saturating_sub(top)
}

/// Cells of horizontal overlap between two rects (0 when they share no columns).
fn horizontal_overlap(p: Rect, q: Rect) -> u16 {
    let left = p.x.max(q.x);
    let right = (p.x + p.width).min(q.x + q.width);
    right.saturating_sub(left)
}

fn prune_blank_rec<F: Fn(&EditorTabs) -> bool>(node: &mut LayoutNode, is_blank: &F) {
    if let LayoutNode::Split { children, .. } = node {
        for c in children.iter_mut() {
            prune_blank_rec(&mut c.node, is_blank);
        }
        children.retain(|c| !matches!(&c.node, LayoutNode::Leaf(Some(t)) if is_blank(t)));
        if children.len() == 1 {
            let only = children.remove(0);
            *node = only.node;
        }
    }
}

fn group_ref_at<'a>(node: &'a LayoutNode, dfs: usize, idx: &mut usize) -> Option<&'a EditorTabs> {
    match node {
        LayoutNode::Leaf(slot) => {
            let here = *idx;
            *idx += 1;
            if here == dfs { slot.as_ref() } else { None }
        }
        LayoutNode::Split { children, .. } => {
            for c in children {
                if let Some(found) = group_ref_at(&c.node, dfs, idx) {
                    return Some(found);
                }
            }
            None
        }
    }
}

/// Emit one [`Seam`] per internal child boundary of every split under `node`,
/// laid out within `area` (mirroring [`leaf_rects`]'s tiling), depth-first.
fn collect_seams(
    node: &LayoutNode,
    area: Rect,
    min: u16,
    path: &mut Vec<usize>,
    out: &mut Vec<Seam>,
) {
    let LayoutNode::Split { dir, children } = node else {
        return;
    };
    let weights: Vec<u16> = children.iter().map(|c| c.weight).collect();
    let horizontal = *dir == SplitDir::Horizontal;
    let total = if horizontal { area.width } else { area.height };
    let lengths = apportion(total, &weights, min);
    let node_axis_start = if horizontal { area.x } else { area.y };
    let (cross_start, cross_end) = if horizontal {
        (area.y, area.y.saturating_add(area.height))
    } else {
        (area.x, area.x.saturating_add(area.width))
    };
    let mut offset = node_axis_start;
    let mut child_rects = Vec::with_capacity(children.len());
    for (i, len) in lengths.iter().enumerate() {
        let sub = if horizontal {
            Rect::new(offset, area.y, *len, area.height)
        } else {
            Rect::new(area.x, offset, area.width, *len)
        };
        child_rects.push(sub);
        offset = offset.saturating_add(*len);
        if i + 1 < children.len() {
            out.push(Seam {
                dir: *dir,
                pos: offset, // start of child i+1 == the seam line
                cross_start,
                cross_end,
                node_axis_start,
                node_axis_len: total,
                path: path.clone(),
                boundary: i,
            });
        }
    }
    for (i, child) in children.iter().enumerate() {
        path.push(i);
        collect_seams(&child.node, child_rects[i], min, path, out);
        path.pop();
    }
}

/// Descend `path` (child indices from the root) to a node for mutation.
fn node_at_path<'a>(mut node: &'a mut LayoutNode, path: &[usize]) -> Option<&'a mut LayoutNode> {
    for &i in path {
        let LayoutNode::Split { children, .. } = node else {
            return None;
        };
        node = &mut children.get_mut(i)?.node;
    }
    Some(node)
}

fn rect_contains(r: Rect, col: u16, row: u16) -> bool {
    col >= r.x
        && col < r.x.saturating_add(r.width)
        && row >= r.y
        && row < r.y.saturating_add(r.height)
}

fn inactive_index_at(node: &LayoutNode, col: u16, row: u16, idx: &mut usize) -> Option<usize> {
    match node {
        LayoutNode::Leaf(Some(tabs)) => {
            let here = *idx;
            *idx += 1;
            rect_contains(tabs.last_full_area, col, row).then_some(here)
        }
        LayoutNode::Leaf(None) => {
            *idx += 1;
            None
        }
        LayoutNode::Split { children, .. } => {
            for c in children {
                if let Some(found) = inactive_index_at(&c.node, col, row, idx) {
                    return Some(found);
                }
            }
            None
        }
    }
}

/// Count the leaves under `node`.
fn count_leaves(node: &LayoutNode) -> usize {
    match node {
        LayoutNode::Leaf(_) => 1,
        LayoutNode::Split { children, .. } => children.iter().map(|c| count_leaves(&c.node)).sum(),
    }
}

/// Visit every leaf in depth-first order, reporting whether each is the active
/// (hoisted) leaf.
fn visit_leaves(node: &LayoutNode, f: &mut impl FnMut(bool)) {
    match node {
        LayoutNode::Leaf(tabs) => f(tabs.is_none()),
        LayoutNode::Split { children, .. } => {
            for c in children {
                visit_leaves(&c.node, f);
            }
        }
    }
}

/// Replace an active (`Leaf(None)`) `node` with a fresh split of `dir`,
/// seating the `existing` tabs in the sibling leaf and keeping the active
/// placeholder. `new_after` puts the active placeholder after the existing
/// group (Split Right / Down) or before it (Split Left / Up).
fn wrap_active_leaf(
    node: &mut LayoutNode,
    existing: &mut Option<EditorTabs>,
    dir: SplitDir,
    new_after: bool,
) {
    let existing_leaf = LayoutChild {
        node: LayoutNode::Leaf(Some(existing.take().expect("existing tabs"))),
        weight: 1,
    };
    let active_leaf = LayoutChild {
        node: LayoutNode::Leaf(None),
        weight: 1,
    };
    let children = if new_after {
        vec![existing_leaf, active_leaf]
    } else {
        vec![active_leaf, existing_leaf]
    };
    *node = LayoutNode::Split { dir, children };
}

fn split_active_rec(
    node: &mut LayoutNode,
    existing: &mut Option<EditorTabs>,
    dir: SplitDir,
    new_after: bool,
) -> bool {
    match node {
        // A bare active leaf (only the root reaches here): wrap it in a split.
        LayoutNode::Leaf(None) => {
            wrap_active_leaf(node, existing, dir, new_after);
            true
        }
        LayoutNode::Leaf(Some(_)) => false,
        LayoutNode::Split {
            dir: node_dir,
            children,
        } => {
            // Is the active placeholder a DIRECT child of this split?
            if let Some(p) = children
                .iter()
                .position(|c| matches!(c.node, LayoutNode::Leaf(None)))
            {
                if *node_dir == dir {
                    // Same axis as VS Code: insert the existing group as a flat
                    // sibling next to the active placeholder rather than nesting.
                    let existing_child = LayoutChild {
                        node: LayoutNode::Leaf(Some(existing.take().expect("existing tabs"))),
                        weight: 1,
                    };
                    // new_after keeps the active placeholder to the right/below
                    // the existing group, so the existing group lands before it.
                    let insert_at = if new_after { p } else { p + 1 };
                    children.insert(insert_at, existing_child);
                } else {
                    // Cross axis: wrap just the active child in a nested split.
                    wrap_active_leaf(&mut children[p].node, existing, dir, new_after);
                }
                true
            } else {
                children
                    .iter_mut()
                    .any(|c| split_active_rec(&mut c.node, existing, dir, new_after))
            }
        }
    }
}

fn set_active_tabs(node: &mut LayoutNode, tabs: EditorTabs) -> Option<EditorTabs> {
    match node {
        LayoutNode::Leaf(slot @ None) => {
            *slot = Some(tabs);
            None
        }
        LayoutNode::Leaf(Some(_)) => Some(tabs),
        LayoutNode::Split { children, .. } => {
            let mut carry = Some(tabs);
            for c in children {
                carry = set_active_tabs(&mut c.node, carry.take().unwrap());
                if carry.is_none() {
                    break;
                }
            }
            carry
        }
    }
}

fn take_leaf_at(node: &mut LayoutNode, target: usize, idx: &mut usize) -> Option<EditorTabs> {
    match node {
        LayoutNode::Leaf(slot) => {
            let hit = *idx == target;
            *idx += 1;
            if hit { slot.take() } else { None }
        }
        LayoutNode::Split { children, .. } => {
            for c in children {
                if let Some(tabs) = take_leaf_at(&mut c.node, target, idx) {
                    return Some(tabs);
                }
            }
            None
        }
    }
}

/// Remove the active (`Leaf(None)`) leaf and unwrap any split left with a
/// single child.
fn remove_active(node: &mut LayoutNode) -> bool {
    let LayoutNode::Split { children, .. } = node else {
        return false;
    };
    if let Some(pos) = children
        .iter()
        .position(|c| matches!(c.node, LayoutNode::Leaf(None)))
    {
        children.remove(pos);
    } else {
        for c in children.iter_mut() {
            if remove_active(&mut c.node) {
                break;
            }
        }
    }
    if children.len() == 1 {
        let only = children.pop().expect("len checked");
        *node = only.node;
    }
    true
}

/// Mark the first depth-first leaf active (taking its tabs) — used after
/// [`remove_active`] leaves the tree with no active placeholder.
fn activate_first(node: &mut LayoutNode) -> Option<EditorTabs> {
    match node {
        LayoutNode::Leaf(slot) => slot.take(),
        LayoutNode::Split { children, .. } => {
            for c in children {
                if let Some(tabs) = activate_first(&mut c.node) {
                    return Some(tabs);
                }
            }
            None
        }
    }
}

fn collect_inactive<'a>(node: &'a LayoutNode, out: &mut Vec<&'a EditorTabs>) {
    match node {
        LayoutNode::Leaf(Some(tabs)) => out.push(tabs),
        LayoutNode::Leaf(None) => {}
        LayoutNode::Split { children, .. } => {
            for c in children {
                collect_inactive(&c.node, out);
            }
        }
    }
}

fn collect_inactive_mut<'a>(node: &'a mut LayoutNode, out: &mut Vec<&'a mut EditorTabs>) {
    match node {
        LayoutNode::Leaf(Some(tabs)) => out.push(tabs),
        LayoutNode::Leaf(None) => {}
        LayoutNode::Split { children, .. } => {
            for c in children {
                collect_inactive_mut(&mut c.node, out);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaf() -> LayoutNode {
        LayoutNode::Leaf(None)
    }

    fn area(w: u16, h: u16) -> Rect {
        Rect::new(0, 0, w, h)
    }

    #[test]
    fn single_leaf_fills_the_whole_area() {
        let rects = leaf_rects(&leaf(), area(100, 40), 10);
        assert_eq!(rects, vec![area(100, 40)]);
    }

    #[test]
    fn horizontal_split_with_equal_weights_halves_the_width() {
        let node = LayoutNode::Split {
            dir: SplitDir::Horizontal,
            children: vec![
                LayoutChild {
                    node: leaf(),
                    weight: 1,
                },
                LayoutChild {
                    node: leaf(),
                    weight: 1,
                },
            ],
        };
        let rects = leaf_rects(&node, area(100, 40), 10);
        assert_eq!(rects.len(), 2);
        // Left half then right half; together they tile the area with no gap
        // and no overlap.
        assert_eq!(rects[0], Rect::new(0, 0, 50, 40));
        assert_eq!(rects[1], Rect::new(50, 0, 50, 40));
    }

    #[test]
    fn vertical_split_with_equal_weights_halves_the_height() {
        let node = LayoutNode::Split {
            dir: SplitDir::Vertical,
            children: vec![
                LayoutChild {
                    node: leaf(),
                    weight: 1,
                },
                LayoutChild {
                    node: leaf(),
                    weight: 1,
                },
            ],
        };
        let rects = leaf_rects(&node, area(100, 40), 10);
        assert_eq!(rects.len(), 2);
        assert_eq!(rects[0], Rect::new(0, 0, 100, 20));
        assert_eq!(rects[1], Rect::new(0, 20, 100, 20));
    }

    #[test]
    fn weights_apportion_length_proportionally() {
        let node = LayoutNode::Split {
            dir: SplitDir::Horizontal,
            children: vec![
                LayoutChild {
                    node: leaf(),
                    weight: 3,
                },
                LayoutChild {
                    node: leaf(),
                    weight: 1,
                },
            ],
        };
        let rects = leaf_rects(&node, area(100, 40), 10);
        assert_eq!(rects[0], Rect::new(0, 0, 75, 40));
        assert_eq!(rects[1], Rect::new(75, 0, 25, 40));
    }

    #[test]
    fn a_starved_child_is_clamped_to_the_minimum() {
        // Weight would give the right child 4 cells; min forces 10, and the
        // left child gives up the difference. Tiling is preserved.
        let node = LayoutNode::Split {
            dir: SplitDir::Horizontal,
            children: vec![
                LayoutChild {
                    node: leaf(),
                    weight: 24,
                },
                LayoutChild {
                    node: leaf(),
                    weight: 1,
                },
            ],
        };
        let rects = leaf_rects(&node, area(100, 40), 10);
        assert_eq!(rects[1].width, 10);
        assert_eq!(rects[0].width, 90);
        assert_eq!(rects[0].x, 0);
        assert_eq!(rects[1].x, 90);
    }

    #[test]
    fn split_active_makes_a_two_leaf_horizontal_split_with_the_new_group_active_after() {
        let mut layout = EditorLayout::single();
        assert_eq!(layout.leaf_count(), 1);
        assert!(!layout.is_split());
        assert_eq!(layout.active_dfs_index(), 0);

        layout.split_active(EditorTabs::default(), SplitDir::Horizontal, true);

        assert_eq!(layout.leaf_count(), 2);
        assert!(layout.is_split());
        // New focused group is the right (after) leaf → depth-first index 1.
        assert_eq!(layout.active_dfs_index(), 1);
        match &layout.root {
            LayoutNode::Split { dir, children } => {
                assert_eq!(*dir, SplitDir::Horizontal);
                assert_eq!(children.len(), 2);
                // Existing tabs settle on the left; the active placeholder is right.
                assert!(matches!(children[0].node, LayoutNode::Leaf(Some(_))));
                assert!(matches!(children[1].node, LayoutNode::Leaf(None)));
            }
            _ => panic!("expected a split at the root"),
        }
    }

    #[test]
    fn refocus_to_dfs_moves_the_active_marker_to_the_target_leaf() {
        let mut layout = EditorLayout::single();
        layout.split_active(EditorTabs::default(), SplitDir::Horizontal, true);
        assert_eq!(layout.active_dfs_index(), 1); // active is the right leaf

        let _moved = layout.refocus_to_dfs(EditorTabs::default(), 0);

        assert_eq!(layout.leaf_count(), 2);
        assert_eq!(layout.active_dfs_index(), 0); // active is now the left leaf
        match &layout.root {
            LayoutNode::Split { children, .. } => {
                assert!(matches!(children[0].node, LayoutNode::Leaf(None)));
                assert!(matches!(children[1].node, LayoutNode::Leaf(Some(_))));
            }
            _ => panic!("expected a split"),
        }
    }

    #[test]
    fn collapse_active_promotes_the_sibling_back_to_a_single_leaf() {
        let mut layout = EditorLayout::single();
        layout.split_active(EditorTabs::default(), SplitDir::Horizontal, true);
        assert!(layout.is_split());

        let _promoted = layout.collapse_active();

        assert_eq!(layout.leaf_count(), 1);
        assert!(!layout.is_split());
        assert_eq!(layout.active_dfs_index(), 0);
        assert!(matches!(&layout.root, LayoutNode::Leaf(None)));
    }

    #[test]
    fn same_axis_split_inserts_a_flat_sibling_not_a_nested_split() {
        let mut layout = EditorLayout::single();
        layout.split_active(EditorTabs::default(), SplitDir::Horizontal, true);
        // Split the active (right) leaf along the SAME axis again.
        layout.split_active(EditorTabs::default(), SplitDir::Horizontal, true);
        match &layout.root {
            LayoutNode::Split { dir, children } => {
                assert_eq!(*dir, SplitDir::Horizontal);
                assert_eq!(children.len(), 3, "VS Code flattens same-axis splits");
                assert!(
                    children
                        .iter()
                        .all(|c| matches!(c.node, LayoutNode::Leaf(_))),
                    "all three children are leaves, no nesting"
                );
            }
            _ => panic!("expected one flat 3-way split"),
        }
        assert_eq!(layout.leaf_count(), 3);
        assert_eq!(layout.active_dfs_index(), 2, "newest group is rightmost");
    }

    #[test]
    fn cross_axis_split_wraps_the_active_leaf_in_a_nested_split() {
        let mut layout = EditorLayout::single();
        layout.split_active(EditorTabs::default(), SplitDir::Horizontal, true);
        // Split the active (right) leaf along the CROSS axis.
        layout.split_active(EditorTabs::default(), SplitDir::Vertical, true);
        match &layout.root {
            LayoutNode::Split { dir, children } => {
                assert_eq!(*dir, SplitDir::Horizontal);
                assert_eq!(children.len(), 2);
                assert!(matches!(children[0].node, LayoutNode::Leaf(Some(_))));
                assert!(matches!(
                    children[1].node,
                    LayoutNode::Split {
                        dir: SplitDir::Vertical,
                        ..
                    }
                ));
            }
            _ => panic!("expected the root to stay a 2-way horizontal split"),
        }
        assert_eq!(layout.leaf_count(), 3);
        assert_eq!(layout.active_dfs_index(), 2, "newest group is lower-right");
    }

    #[test]
    fn seams_lists_one_draggable_boundary_per_internal_split_edge() {
        let mut layout = EditorLayout::single();
        layout.root = LayoutNode::Split {
            dir: SplitDir::Horizontal,
            children: vec![
                LayoutChild {
                    node: leaf(),
                    weight: 1,
                },
                LayoutChild {
                    node: leaf(),
                    weight: 1,
                },
                LayoutChild {
                    node: leaf(),
                    weight: 1,
                },
            ],
        };
        let seams = layout.seams(area(99, 40), 10);
        assert_eq!(seams.len(), 2, "two internal boundaries → two seams");
        assert!(seams.iter().all(|s| s.dir == SplitDir::Horizontal));
        assert_eq!(seams[0].pos, 33); // 1/3 column
        assert_eq!(seams[1].pos, 66); // 2/3 column
        assert_eq!((seams[0].cross_start, seams[0].cross_end), (0, 40));
        assert_eq!(seams[0].boundary, 0);
        assert_eq!(seams[1].boundary, 1);
    }

    #[test]
    fn seams_includes_nested_split_boundaries_scoped_to_their_region() {
        // H[ leaf | V[leaf, leaf] ]: one outer vertical seam (full height) and
        // one inner horizontal seam confined to the right column.
        let mut layout = EditorLayout::single();
        layout.root = LayoutNode::Split {
            dir: SplitDir::Horizontal,
            children: vec![
                LayoutChild {
                    node: leaf(),
                    weight: 1,
                },
                LayoutChild {
                    node: LayoutNode::Split {
                        dir: SplitDir::Vertical,
                        children: vec![
                            LayoutChild {
                                node: leaf(),
                                weight: 1,
                            },
                            LayoutChild {
                                node: leaf(),
                                weight: 1,
                            },
                        ],
                    },
                    weight: 1,
                },
            ],
        };
        let seams = layout.seams(area(100, 40), 10);
        assert_eq!(seams.len(), 2);
        let outer = seams
            .iter()
            .find(|s| s.dir == SplitDir::Horizontal)
            .unwrap();
        assert_eq!(outer.pos, 50);
        assert_eq!((outer.cross_start, outer.cross_end), (0, 40));
        assert_eq!(outer.path, Vec::<usize>::new());
        let inner = seams.iter().find(|s| s.dir == SplitDir::Vertical).unwrap();
        assert_eq!(inner.pos, 20);
        assert_eq!(
            (inner.cross_start, inner.cross_end),
            (50, 100),
            "the inner seam spans only the right column"
        );
        assert_eq!(inner.path, vec![1]);
    }

    #[test]
    fn dragging_a_seam_repartitions_only_its_two_groups() {
        let mut layout = EditorLayout::single();
        layout.root = LayoutNode::Split {
            dir: SplitDir::Horizontal,
            children: vec![
                LayoutChild {
                    node: leaf(),
                    weight: 1,
                },
                LayoutChild {
                    node: leaf(),
                    weight: 1,
                },
                LayoutChild {
                    node: leaf(),
                    weight: 1,
                },
            ],
        };
        let seams = layout.seams(area(99, 40), 10);
        // Drag the SECOND seam (between columns 1 and 2) to x = 50.
        layout.drag_seam(&seams[1], 50, 10);
        let rects = layout.leaf_rects(area(99, 40), 10);
        assert_eq!(rects[0].x, 0);
        assert_eq!(rects[0].width, 33, "the first boundary did not move");
        assert_eq!(rects[2].x, 50, "only the dragged second boundary moved");
    }

    #[test]
    fn collapse_active_in_a_nested_grid_unwraps_the_emptied_split() {
        let mut layout = EditorLayout::single();
        layout.split_active(EditorTabs::default(), SplitDir::Horizontal, true); // H[A, active]
        layout.split_active(EditorTabs::default(), SplitDir::Vertical, true); // H[A, V[B, active]]
        assert_eq!(layout.leaf_count(), 3);
        let _ = layout.collapse_active(); // close the lower-right group
        assert_eq!(layout.leaf_count(), 2);
        match &layout.root {
            LayoutNode::Split { dir, children } => {
                assert_eq!(*dir, SplitDir::Horizontal, "the outer split survives");
                assert_eq!(children.len(), 2);
                assert!(
                    children
                        .iter()
                        .all(|c| matches!(c.node, LayoutNode::Leaf(_))),
                    "the emptied inner V-split unwrapped to its surviving leaf"
                );
            }
            _ => panic!("expected the root H-split to remain"),
        }
        assert!(layout.is_split());
    }

    #[test]
    fn nested_splits_lay_out_depth_first() {
        // Root H-split: left leaf | right is a V-split of two leaves.
        let node = LayoutNode::Split {
            dir: SplitDir::Horizontal,
            children: vec![
                LayoutChild {
                    node: leaf(),
                    weight: 1,
                },
                LayoutChild {
                    node: LayoutNode::Split {
                        dir: SplitDir::Vertical,
                        children: vec![
                            LayoutChild {
                                node: leaf(),
                                weight: 1,
                            },
                            LayoutChild {
                                node: leaf(),
                                weight: 1,
                            },
                        ],
                    },
                    weight: 1,
                },
            ],
        };
        let rects = leaf_rects(&node, area(100, 40), 10);
        assert_eq!(rects.len(), 3);
        assert_eq!(rects[0], Rect::new(0, 0, 50, 40)); // left column
        assert_eq!(rects[1], Rect::new(50, 0, 50, 20)); // right-top
        assert_eq!(rects[2], Rect::new(50, 20, 50, 20)); // right-bottom
    }

    #[test]
    fn neighbor_leaf_in_dir_finds_the_group_across_each_edge() {
        // Two side-by-side columns; the active group is the right one (dfs 1).
        let mut layout = EditorLayout::single();
        layout.split_active(EditorTabs::default(), SplitDir::Horizontal, true);
        assert_eq!(layout.active_dfs_index(), 1);
        // Its only spatial neighbour is the left column, reached by going Left.
        assert_eq!(
            layout.neighbor_leaf_in_dir(area(100, 40), 10, Dir::Left),
            Some(0)
        );
        assert_eq!(
            layout.neighbor_leaf_in_dir(area(100, 40), 10, Dir::Right),
            None
        );
        assert_eq!(
            layout.neighbor_leaf_in_dir(area(100, 40), 10, Dir::Up),
            None
        );
        assert_eq!(
            layout.neighbor_leaf_in_dir(area(100, 40), 10, Dir::Down),
            None
        );
    }

    #[test]
    fn dir_maps_to_split_axis_and_placement() {
        assert_eq!(Dir::Right.split_dir(), SplitDir::Horizontal);
        assert_eq!(Dir::Left.split_dir(), SplitDir::Horizontal);
        assert_eq!(Dir::Down.split_dir(), SplitDir::Vertical);
        assert_eq!(Dir::Up.split_dir(), SplitDir::Vertical);
        assert!(Dir::Right.places_new_after() && Dir::Down.places_new_after());
        assert!(!Dir::Left.places_new_after() && !Dir::Up.places_new_after());
    }

    #[test]
    fn prune_blank_inactive_drops_an_emptied_group_and_collapses_the_split() {
        let mut layout = EditorLayout::single();
        layout.split_active(EditorTabs::default(), SplitDir::Horizontal, true);
        // Mark the inactive (left) group blank so prune should drop it, leaving
        // a single active leaf.
        assert_eq!(layout.leaf_count(), 2);
        layout.prune_blank_inactive(|_t| true);
        assert_eq!(layout.leaf_count(), 1);
        assert!(!layout.is_split());
        assert_eq!(layout.active_dfs_index(), 0);
    }
}
