use crate::icons;
use crate::widgets::drawn_rows::DrawnRows;
use crate::widgets::scrollbar;
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Widget,
};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

mod decorations;
mod deletion;
mod drawing;
mod filter;
mod listing;
mod move_and_copy;
mod navigation;
mod new_and_rename;
mod roots;
mod selection;
mod sticky;
pub use deletion::delete_target_for;
pub use deletion::move_to_trash;
pub use deletion::move_to_trash_bulk;
#[cfg(test)]
use drawing::*;
pub use filter::subsequence_match;
use filter::*;
pub use listing::affected_dir_for_event;
#[cfg(test)]
use listing::*;
pub use move_and_copy::copy_into;
pub use move_and_copy::is_descendant_or_same;
pub use move_and_copy::is_directly_in;
pub use move_and_copy::move_into;
pub use move_and_copy::unique_destination_in;
pub use new_and_rename::create_file_in;
pub use new_and_rename::create_folder_in;
pub use new_and_rename::create_target_dir_for;
pub use new_and_rename::rename_in;
pub use new_and_rename::rename_target;
pub use new_and_rename::validate_new_name;
#[cfg(test)]
use new_and_rename::*;
use sticky::*;

pub struct Node {
    pub path: PathBuf,
    pub depth: usize,
    pub is_dir: bool,
    pub expanded: bool,
    pub loaded: bool,
    /// Compact folders (#1486), VS Code's `explorer.compactFolders`: how
    /// many folders above `path` this row folds in, each the only entry of
    /// the one above it. 0 for an ordinary row. The row
    /// `src/main/java/com/acme` folds 4, and its `path` is the deepest
    /// folder, `acme`, so New File, paste and drop land there.
    pub folded: usize,
}

impl Node {
    /// Every folder the row shows, deepest first: `path` and the `folded`
    /// folders above it.
    pub fn shown_paths(&self) -> impl Iterator<Item = &Path> {
        self.path.ancestors().take(self.folded + 1)
    }

    /// Whether this row shows `path`, as its own or as a folded-in folder.
    pub fn shows(&self, path: &Path) -> bool {
        self.shown_paths().any(|p| p == path)
    }

    /// The row's name: the entry's own, or a compact row's folder names
    /// joined by `/`, as VS Code labels one on every platform.
    pub fn label(&self) -> String {
        let name = |p: &Path| {
            p.file_name().map_or_else(
                || p.display().to_string(),
                |n| n.to_string_lossy().into_owned(),
            )
        };
        if self.folded == 0 {
            return name(&self.path);
        }
        let shown: Vec<&Path> = self.shown_paths().collect();
        let mut label = String::new();
        for p in shown.into_iter().rev() {
            if !label.is_empty() {
                label.push('/');
            }
            label.push_str(&name(p));
        }
        label
    }
}

pub struct FileTree {
    pub root: PathBuf,
    pub nodes: Vec<Node>,
    pub selected: usize,
    pub scroll: usize,
    pub focused: bool,
    /// True under the Black theme. Draws the orange→green gradient border
    /// (when focused) instead of the solid blue one, and fills the selected
    /// row with the brand's dark-teal instead of the legacy blue. Set by the
    /// app's focus/theme sync.
    pub focus_gradient: bool,
    /// Active color theme. Drives the scrollbar track/thumb colors (which
    /// pre-blend against the theme background). Set by the app's theme sync.
    pub theme: crate::theme::Theme,
    /// Git-ignored paths (absolute; fully-ignored dirs collapsed to one
    /// entry), fed from the git worker's status refresh. Rows whose path is
    /// in the set — or under a dir in the set — render their name in the
    /// theme's dimmed foreground, VS Code's ignored-resource decoration.
    pub ignored: Arc<HashSet<PathBuf>>,
    /// Git status decorations (#1513), fed from the same status refresh as
    /// `ignored`: each changed file and each folder above one, absolute,
    /// with the status it wears. Rows colour their name by it and show its
    /// letter (files) or a dot (folders) at the right edge, as VS Code does.
    pub git_decorations: Arc<HashMap<PathBuf, crate::git::GitDecoration>>,
    /// While the history scrubber sits on a commit (#371): the directory
    /// the listing covers and every path in that commit's tree under it,
    /// absolute, folders included. A row under that directory and absent
    /// from it (submodule contents count as present) did not exist there,
    /// and its name takes the same dimmed foreground as an ignored one.
    /// `None` everywhere else, including at the working tree.
    pub scrub_tree: Option<(PathBuf, Arc<crate::git::CommitTree>)>,
    /// Workspace files an agent changed and the user has not reviewed
    /// (#345), absolute. Rows in this set take a TRAILING dot in the theme's
    /// yellow; it clears when the file is marked reviewed.
    ///
    /// One colour, not one per agent: `AgentLane` carries no colour today,
    /// and inventing one here would put the mapping in the wrong place.
    ///
    /// Membership is EXACT, unlike `ignored`, which covers a directory's
    /// descendants. A directory is not something an agent wrote and not
    /// something "mark reviewed" can clear, so a dot on one would be a mark
    /// no gesture removes. A collapsed folder's dimmer rollup dot is derived
    /// from this set at paint time instead (see
    /// [`Self::hides_agent_touched`]). Fed from `AgentLedger` on the app's
    /// sync.
    pub agent_touched: Arc<HashSet<PathBuf>>,
    /// The project's `files_exclude` globs (#1345): matching entries are
    /// not listed, like VS Code's `files.exclude`.
    pub exclude: crate::widgets::search::PathFilter,
    /// Fold a folder whose only entry is a folder into one row with it
    /// (#1486). On by default; the `disable_compact_folders` setting clears
    /// it through [`Self::set_compact_folders`].
    pub compact_folders: bool,
    /// Text painted after a ROOT row's name (#348): a worktree lane's
    /// branch and the badge of the agent seated in its pane, keyed by the
    /// root path. Swapped in by the app whenever a lane or a seat changes;
    /// a root with no entry paints as before.
    pub root_badges: Arc<HashMap<PathBuf, String>>,
    /// Unresolved sticky notes per file (#367), painted as a count after
    /// the name.
    pub note_counts: Arc<HashMap<PathBuf, usize>>,
    pub last_inner: Rect,
    /// The rows the last frame drew, by path (#2751). A click is matched
    /// against them: the screen shows them until the next frame, while a
    /// wheel step, or a key or click that opened or closed a folder, read in
    /// the same burst has already moved the rows under it.
    drawn: DrawnRows<PathBuf>,
    pub last_area: Rect,
    pub last_scrollbar: Rect,
    /// The sticky band painted this frame, one pinned ancestor per line from
    /// the top, by path like the rows below it. Cleared at render start so
    /// the hit test always describes the painted frame (#103's invariant).
    sticky: DrawnRows<PathBuf>,
    pub anchor: usize,
    pub marked: BTreeSet<PathBuf>,
    /// While the user is mid-drag, the index of the directory row currently
    /// under the pointer (or the parent dir of a hovered file). Drawn with
    /// a highlighted bg so the drop target is unambiguous. Cleared on drop
    /// or cancel.
    pub drag_target: Option<usize>,
    /// Render-time pointer cell, fed from `App::pointer_cell` each frame so a
    /// row under the cursor can lift to the hover background. `None` when the
    /// pointer is outside the panel.
    pub hover_pointer: Option<(u16, u16)>,
    /// Hit-test rects for the Explorer header toolbar buttons, captured each
    /// render on the panel's top border row: VS Code's New File / New Folder /
    /// Refresh / Collapse Folders affordances. `Rect::default()` (zero width)
    /// when the panel is too narrow to paint them.
    pub header_new_file_btn: Rect,
    pub header_new_folder_btn: Rect,
    pub header_refresh_btn: Rect,
    pub header_collapse_btn: Rect,
    /// Hit-test rect for the "Views and More Actions" (⋯) button on the
    /// EXPLORER title line, which opens the menu that toggles the Explorer's
    /// stacked sub-views (Open Editors / Folders / Outline / Timeline / Rust
    /// Dependencies). `Rect::default()` (zero width) when the panel is too
    /// narrow to paint it.
    pub header_views_btn: Rect,
}

impl FileTree {
    pub fn new(root: PathBuf) -> Self {
        let mut tree = Self {
            root: root.clone(),
            nodes: vec![Node {
                path: root,
                depth: 0,
                is_dir: true,
                expanded: true,
                loaded: false,
                folded: 0,
            }],
            selected: 0,
            scroll: 0,
            focused: true,
            focus_gradient: false,
            theme: crate::theme::Theme::default(),
            ignored: Arc::default(),
            git_decorations: Arc::default(),
            scrub_tree: None,
            agent_touched: Arc::default(),
            exclude: Default::default(),
            compact_folders: true,
            note_counts: Arc::default(),
            root_badges: Arc::default(),
            last_inner: Rect::default(),
            drawn: DrawnRows::default(),
            last_area: Rect::default(),
            last_scrollbar: Rect::default(),
            sticky: DrawnRows::default(),
            anchor: 0,
            marked: BTreeSet::new(),
            drag_target: None,
            hover_pointer: None,
            header_new_file_btn: Rect::default(),
            header_new_folder_btn: Rect::default(),
            header_refresh_btn: Rect::default(),
            header_collapse_btn: Rect::default(),
            header_views_btn: Rect::default(),
        };
        tree.load_children(0);
        tree
    }
}

/// Freedesktop.org home-trash implementation for android, where the `trash`
/// crate has no backend (its freedesktop backend is gated out with
/// `not(target_os = "android")`). Moving an entry into
/// `$XDG_DATA_HOME/Trash/files/<name>` plus an `info/<name>.trashinfo` record
/// preserves the same recoverable semantics croft has on macOS/Linux instead
/// of permanently unlinking the file. See the freedesktop Trash spec v1.0.
#[cfg(target_os = "android")]
mod android_trash;

#[cfg(test)]
mod tests;
