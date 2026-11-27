//! Sticky scroll: the folders pinned at the top while their contents scroll, and a
//! click on one.

use super::*;

impl FileTree {
    /// Indices into `self.nodes` painted in tree order — every node, in order,
    /// since the Explorer no longer filters its rows. Kept as a method so the
    /// render's scroll machinery can keep mapping a node index to its on-screen
    /// position uniformly.
    pub fn visible_indices(&self) -> Vec<usize> {
        (0..self.nodes.len()).collect()
    }

    /// Test-only view of the band painted this frame.
    #[cfg(test)]
    pub fn sticky_rows_for_test(&self) -> Vec<(u16, usize)> {
        (self.last_inner.y..).zip(self.sticky.drawn_at()).collect()
    }

    /// Map a click on the sticky band to the pinned directory's node index:
    /// the one the last frame pinned on that line, where it is now (#2751).
    pub fn sticky_row_at(&self, y: u16) -> Option<usize> {
        let line = self.line_at_y(y)?;
        self.sticky.now(line, self.nodes.len(), |i, path| {
            self.nodes[i].path == *path
        })
    }
}

/// The ancestor directory chain of `top_idx`, outermost first, capped at
/// `max` keeping the DEEPEST entries (the closest context matters most —
/// VS Code's tree sticky truncates the outer levels the same way). An
/// ancestor is the nearest earlier node with a strictly smaller depth,
/// walked to the root; a top-level node has none.
pub(crate) fn sticky_ancestors(nodes: &[Node], top_idx: usize, max: usize) -> Vec<usize> {
    let Some(top) = nodes.get(top_idx) else {
        return Vec::new();
    };
    let mut chain = Vec::new();
    let mut want_depth = top.depth;
    for i in (0..top_idx).rev() {
        if want_depth == 0 {
            break;
        }
        if nodes[i].depth < want_depth {
            chain.push(i);
            want_depth = nodes[i].depth;
        }
    }
    chain.reverse(); // outermost first
    let skip = chain.len().saturating_sub(max);
    chain.split_off(skip)
}
