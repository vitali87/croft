//! How a row looks: its git decoration and name colour, the folder icon's colour, the
//! active indent guide, and whether a path is ignored, gone at the scrubbed commit, or
//! changed by an agent and not yet reviewed.

use super::*;

impl FileTree {
    /// The git decoration `path`'s row wears (#1513). None on a workspace
    /// root row, and none while the history scrubber shows a commit: the
    /// working tree's changes are not that commit's.
    pub fn git_decoration(&self, path: &Path) -> Option<crate::git::GitDecoration> {
        if self.scrub_tree.is_some() || self.is_root_path(path) {
            return None;
        }
        self.git_decorations.get(path).copied()
    }

    /// A row's name colour: dimmed when git ignores it (or it is absent at
    /// the scrubbed commit), its git decoration's colour when it changed,
    /// the plain foreground otherwise.
    pub(super) fn name_fg(&self, path: &Path) -> Color {
        if self.is_ignored(path) || self.is_absent_at_scrub(path) {
            self.theme.ignored_fg()
        } else if let Some(dec) = self.git_decoration(path) {
            self.theme.git_decoration_fg(dec)
        } else if let Some(t) = crate::widgets::surface::tones(self.theme) {
            // Croft Loch's body ink: the named White follows the host
            // terminal's palette and is usually pure #fff.
            t.text()
        } else {
            self.theme.ui(Color::White)
        }
    }

    /// A folder icon's colour: the icon's own tan on a boxed theme, eased
    /// most of the way toward the muted ink under `surfaces`, so folders
    /// read by their shape and a long tree is not a column of gold.
    pub(super) fn folder_icon_fg(&self, color: Color) -> Color {
        match (crate::widgets::surface::tones(self.theme), color) {
            (Some(t), Color::Rgb(r, g, b)) => {
                crate::gradient::rgb_color(crate::gradient::lerp_rgb((r, g, b), t.muted, 0.45))
            }
            _ => self.theme.ui(color),
        }
    }

    /// The active indent guide for Croft Loch's tree: the level of the
    /// selected row's parent folder and the node-index range of that
    /// folder's children, `(level, first, end)`. `None` when the selection
    /// sits directly under a root, whose level draws no guide.
    pub(super) fn active_indent_guide(&self) -> Option<(usize, usize, usize)> {
        let sel = self.nodes.get(self.selected)?;
        if sel.depth < 2 {
            return None;
        }
        let parent = (0..self.selected)
            .rev()
            .find(|&i| self.nodes[i].depth < sel.depth)?;
        let level = self.nodes[parent].depth;
        let end = (parent + 1..self.nodes.len())
            .find(|&i| self.nodes[i].depth <= level)
            .unwrap_or(self.nodes.len());
        Some((level, parent + 1, end))
    }

    /// True when `path` is git-ignored: either listed in the ignored set
    /// itself or a descendant of an ignored directory (the set stores a
    /// fully-ignored dir as one collapsed entry). Walks ancestors up to the
    /// workspace root, so the cost is O(depth) per visible row.
    pub fn is_ignored(&self, path: &Path) -> bool {
        if self.ignored.is_empty() {
            return false;
        }
        let mut p = path;
        loop {
            if self.ignored.contains(p) {
                return true;
            }
            // Stop at whichever workspace root owns the path (#145): the
            // walk must never escape a root into its parent directories.
            if self.is_root_path(p) {
                return false;
            }
            match p.parent() {
                Some(parent) => p = parent,
                None => return false,
            }
        }
    }

    /// True when the scrubber sits on a commit whose tree lacks `path`
    /// (#371). Only paths strictly under the listed directory count: the
    /// root row itself, and rows of another workspace root the listing
    /// never covered, keep their colour.
    pub fn is_absent_at_scrub(&self, path: &Path) -> bool {
        let Some((base, present)) = self.scrub_tree.as_ref() else {
            return false;
        };
        path != base && path.starts_with(base) && !present.contains(path)
    }

    /// Whether `path` is a file an agent changed that is still unreviewed.
    ///
    /// Exact membership, deliberately: see [`Self::agent_touched`].
    pub fn is_agent_touched(&self, path: &Path) -> bool {
        !self.agent_touched.is_empty() && self.agent_touched.contains(path)
    }

    /// Whether a collapsed folder at `dir` hides a file an agent changed and
    /// the user has not reviewed (#345), for its dimmer rollup dot.
    ///
    /// Derived at paint time from [`Self::agent_touched`], never stored: the
    /// dot clears by itself when the last file under it is reviewed, which
    /// is what keeps it from being the mark no gesture removes that the
    /// field's exact membership guards against.
    pub fn hides_agent_touched(&self, dir: &Path) -> bool {
        !self.agent_touched.is_empty()
            && self
                .agent_touched
                .iter()
                .any(|p| p != dir && p.starts_with(dir))
    }
}
