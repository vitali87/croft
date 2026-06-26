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
    /// Drives the seam's drag axis and tells the tab menu which split exists.
    pub fn root_split_dir(&self) -> Option<SplitDir> {
        match &self.root {
            LayoutNode::Split { dir, .. } => Some(*dir),
            LayoutNode::Leaf(_) => None,
        }
    }

    /// Set the two child weights of the sole root split, so a seam drag pins
    /// the columns to `(first, second)` cells at the current width. A no-op
    /// unless the root is a two-child split (the only shape this stage drags).
    pub fn set_root_split_weights(&mut self, first: u16, second: u16) {
        if let LayoutNode::Split { children, .. } = &mut self.root
            && children.len() == 2
        {
            children[0].weight = first.max(1);
            children[1].weight = second.max(1);
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

fn split_active_rec(
    node: &mut LayoutNode,
    existing: &mut Option<EditorTabs>,
    dir: SplitDir,
    new_after: bool,
) -> bool {
    match node {
        LayoutNode::Leaf(None) => {
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
            true
        }
        LayoutNode::Leaf(Some(_)) => false,
        LayoutNode::Split { children, .. } => children
            .iter_mut()
            .any(|c| split_active_rec(&mut c.node, existing, dir, new_after)),
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
}
