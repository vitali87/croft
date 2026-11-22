use crate::icons;
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
mod listing;
mod navigation;
mod new_and_rename;
mod roots;
mod selection;
mod sticky;
pub use listing::affected_dir_for_event;
#[cfg(test)]
use listing::*;
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
    pub last_area: Rect,
    pub last_scrollbar: Rect,
    /// The sticky band painted this frame: `(screen y, node index)` per
    /// pinned ancestor row. Cleared at render start so the hit test always
    /// describes the painted frame (#103's invariant).
    sticky_rows: Vec<(u16, usize)>,
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

/// The unreviewed-file marker (#345). A middle dot rather than a filled
/// circle: it must read as a small annotation beside a filename, not compete
/// with the file-type icon at the start of the row.
const AGENT_DOT: char = '\u{b7}';

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
            last_area: Rect::default(),
            last_scrollbar: Rect::default(),
            sticky_rows: Vec::new(),
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

/// Decide whether a given node may be deleted via the explorer's right-click
/// menu, returning the path to delete. Refuses for the workspace root and
/// returns `None` if `node` is None (i.e. right-click on empty tree space).
pub fn delete_target_for(node: Option<&Node>, root: &Path) -> Option<PathBuf> {
    let n = node?;
    // Depth 0 is the root-row marker: with multiple workspace roots
    // (#145) every section row is protected, not just the primary.
    if n.depth == 0 || n.path == root {
        return None;
    }
    let canon_root = root.canonicalize().ok();
    let canon_target = n.path.canonicalize().ok();
    if canon_root.is_some() && canon_target == canon_root {
        return None;
    }
    Some(n.path.clone())
}

/// Move `path` to the OS trash (recoverable) rather than unlinking it.
pub fn move_to_trash(path: &Path) -> std::io::Result<()> {
    #[cfg(not(target_os = "android"))]
    {
        trash::delete(path).map_err(|e| std::io::Error::other(format!("{e}")))
    }
    #[cfg(target_os = "android")]
    {
        android_trash::trash_one(path)
    }
}

/// Bulk-trash a batch of paths in a single OS call. On macOS this routes
/// through Finder via one AppleScript so the system trash sound plays once
/// for the whole batch instead of once per file. On Linux/Windows the
/// underlying `trash::delete_all` already groups the request. Android has no
/// OS trash, so each entry is moved into the freedesktop home trash directly.
pub fn move_to_trash_bulk(paths: &[PathBuf]) -> std::io::Result<()> {
    if paths.is_empty() {
        return Ok(());
    }
    #[cfg(target_os = "macos")]
    {
        use trash::macos::{DeleteMethod, TrashContextExtMacos};
        let mut ctx = trash::TrashContext::default();
        ctx.set_delete_method(DeleteMethod::Finder);
        ctx.delete_all(paths)
            .map_err(|e| std::io::Error::other(format!("{e}")))
    }
    #[cfg(not(any(target_os = "macos", target_os = "android")))]
    {
        trash::delete_all(paths).map_err(|e| std::io::Error::other(format!("{e}")))
    }
    #[cfg(target_os = "android")]
    {
        for p in paths {
            android_trash::trash_one(p)?;
        }
        Ok(())
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

/// Suggest a non-colliding destination path for `source` placed inside
/// `dest_dir`. If a sibling with the source's basename already exists,
/// appends ` copy`, ` copy 2`, … until a free name is found, mirroring how
/// macOS Finder de-duplicates pasted names. Returns the resolved path; the
/// caller still has to perform the actual move/copy syscall.
pub fn unique_destination_in(dest_dir: &Path, source: &Path) -> PathBuf {
    let stem = source
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = source.extension().map(|s| s.to_string_lossy().into_owned());
    let original_name = source
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut candidate = dest_dir.join(&original_name);
    if !candidate.exists() {
        return candidate;
    }
    for n in 1.. {
        let suffix = if n == 1 {
            String::from(" copy")
        } else {
            format!(" copy {n}")
        };
        let name = match &ext {
            Some(e) if !stem.is_empty() => format!("{stem}{suffix}.{e}"),
            Some(e) => format!("{suffix}.{e}"),
            None => format!("{stem}{suffix}"),
        };
        candidate = dest_dir.join(name);
        if !candidate.exists() {
            return candidate;
        }
    }
    candidate
}

/// Reject paste/drop operations that would move a directory into itself or
/// a descendant of itself (which would either error or, worse, produce an
/// infinite-recursion copy on filesystems that follow symlinks).
pub fn is_descendant_or_same(target: &Path, source: &Path) -> bool {
    let canon_target = target
        .canonicalize()
        .unwrap_or_else(|_| target.to_path_buf());
    let canon_source = source
        .canonicalize()
        .unwrap_or_else(|_| source.to_path_buf());
    canon_target == canon_source || canon_target.starts_with(&canon_source)
}

/// Whether `source` already sits directly in `dir`, so moving it there moves
/// nothing (#1174). A source that no longer exists (deleted or renamed since
/// it was cut) is not in place: it takes the ordinary path, which reports it
/// missing. A dangling symlink still exists, as the link itself.
pub fn is_directly_in(dir: &Path, source: &Path) -> bool {
    let canon = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    source.symlink_metadata().is_ok()
        && source
            .parent()
            .is_some_and(|parent| canon(parent) == canon(dir))
}

/// Move `source` to a fresh path inside `dest_dir`. Falls back to
/// copy-then-remove when the source and destination live on different
/// filesystems and `std::fs::rename` returns `EXDEV`. Returns the final
/// destination path on success.
pub fn move_into(dest_dir: &Path, source: &Path) -> std::io::Result<PathBuf> {
    if is_descendant_or_same(dest_dir, source) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("cannot move {} into itself", source.display()),
        ));
    }
    // Already there: a fresh name would rename it to "<name> copy" (#1174).
    if is_directly_in(dest_dir, source) {
        return Ok(source.to_path_buf());
    }
    let dest = unique_destination_in(dest_dir, source);
    if std::fs::rename(source, &dest).is_ok() {
        return Ok(dest);
    }
    copy_recursive(source, &dest)?;
    remove_recursive(source)?;
    Ok(dest)
}

/// Recursively copy `source` to `dest`. `dest` must not already exist.
pub fn copy_into(dest_dir: &Path, source: &Path) -> std::io::Result<PathBuf> {
    if is_descendant_or_same(dest_dir, source) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("cannot copy {} into itself", source.display()),
        ));
    }
    let dest = unique_destination_in(dest_dir, source);
    copy_recursive(source, &dest)?;
    Ok(dest)
}

fn copy_recursive(source: &Path, dest: &Path) -> std::io::Result<()> {
    let meta = std::fs::symlink_metadata(source)?;
    let ft = meta.file_type();
    if ft.is_dir() {
        std::fs::create_dir(dest)?;
        for entry in std::fs::read_dir(source)? {
            let entry = entry?;
            let from = entry.path();
            let to = dest.join(entry.file_name());
            copy_recursive(&from, &to)?;
        }
        Ok(())
    } else if ft.is_symlink() {
        let link_target = std::fs::read_link(source)?;
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&link_target, dest)?;
        }
        #[cfg(not(unix))]
        {
            let _ = link_target;
            return Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "symlink copy not supported on this platform",
            ));
        }
        Ok(())
    } else {
        std::fs::copy(source, dest).map(|_| ())
    }
}

fn remove_recursive(path: &Path) -> std::io::Result<()> {
    let meta = std::fs::symlink_metadata(path)?;
    if meta.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    }
}

/// Case-insensitive subsequence (fuzzy) match of `query` against `haystack`,
/// mirroring the VS Code tree filter, which accepts a hit when the query's
/// characters appear in order (not necessarily contiguously). Returns the byte
/// offsets in `haystack` of the matched characters, in order, so the renderer
/// can bold the matched run; `None` when the query is not a subsequence.
///
/// An empty query matches everything with no highlighted offsets.
pub fn subsequence_match(haystack: &str, query: &str) -> Option<Vec<usize>> {
    if query.is_empty() {
        return Some(Vec::new());
    }
    let mut needles = query.chars().flat_map(char::to_lowercase).peekable();
    let mut offsets = Vec::new();
    for (byte_idx, hc) in haystack.char_indices() {
        let Some(&want) = needles.peek() else { break };
        if hc.to_lowercase().eq([want]) {
            offsets.push(byte_idx);
            needles.next();
        }
    }
    needles.peek().is_none().then_some(offsets)
}

/// Foreground for the matched characters of a filtered row name: VS Code's
/// `list.filterMatchForeground` amber, which reads as a clear "this is why this
/// row survived the filter" cue on both the dark and black themes.
const FILTER_MATCH_FG: Color = Color::Rgb(0xe5, 0xa3, 0x4b);

/// Push the file name onto `spans`, bolding the characters that the active
/// filter `query` matched (per [`subsequence_match`]). With an empty query the
/// whole name is pushed as a single span in `base` style, so the unfiltered
/// path is unchanged.
fn push_name_spans(
    spans: &mut Vec<Span<'static>>,
    name: &str,
    query: &str,
    base: Style,
    theme: crate::theme::Theme,
) {
    let Some(offsets) = (!query.is_empty())
        .then(|| subsequence_match(name, query))
        .flatten()
    else {
        spans.push(Span::styled(name.to_string(), base));
        return;
    };
    let match_style = base
        .fg(theme.ui(FILTER_MATCH_FG))
        .add_modifier(Modifier::BOLD);
    let mut cursor = 0usize;
    for &off in &offsets {
        if off > cursor {
            spans.push(Span::styled(name[cursor..off].to_string(), base));
        }
        // The matched character runs from its byte offset to the next char
        // boundary; index by the char's own UTF-8 length.
        let ch_len = name[off..].chars().next().map_or(0, char::len_utf8);
        let next = off + ch_len;
        spans.push(Span::styled(name[off..next].to_string(), match_style));
        cursor = next;
    }
    if cursor < name.len() {
        spans.push(Span::styled(name[cursor..].to_string(), base));
    }
}

impl Widget for &mut FileTree {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let block_style = if self.focused {
            Style::default().fg(self.theme.ui(Color::Rgb(0x4e, 0x9a, 0xff)))
        } else {
            Style::default().fg(Color::DarkGray)
        };
        // Black theme: chipless brand header (panelTitle.activeForeground);
        // Croft Dark keeps the historical white-on-navy chip.
        let label = format!(" {} ", crate::i18n::tr("EXPLORER"));
        let label_width = unicode_width::UnicodeWidthStr::width(label.as_str());
        let tones = crate::widgets::surface::tones(self.theme);
        // Croft Loch: the view title is the one label in the side bar that
        // says where focus is, so it is the strong ink while the tree has
        // the keyboard and recedes to muted otherwise. Never bold: weight
        // is kept for the workspace root below it.
        let title = if let Some(t) = tones {
            Span::styled(
                label,
                Style::default().fg(if self.focused { t.strong() } else { t.muted() }),
            )
        } else if self.focus_gradient {
            Span::styled(
                label,
                Style::default()
                    .fg(crate::gradient::rgb_color(crate::gradient::PANEL_TITLE_FG))
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            Span::styled(
                label,
                Style::default()
                    .fg(self.theme.ui(Color::White))
                    .bg(self.theme.ui(Color::Rgb(0x1e, 0x3a, 0x6e)))
                    .add_modifier(Modifier::BOLD),
            )
        };
        let block =
            crate::widgets::surface::view_block(self.theme, true, block_style).title(title.clone());
        let inner = block.inner(area);
        block.render(area, buf);
        // Black theme: replace the solid focus border with the orange→green
        // gradient (matching the welcome activity box), then re-stamp the
        // title the gradient top edge just overwrote. Croft Loch marks focus
        // with the accent lip, which starts one cell past the title's own
        // padding: the title keeps a breath of space on both sides and the
        // lip's accent end sits right beside the name of the focused view.
        if tones.is_some() {
            if self.focused {
                let lip_x = area
                    .x
                    .saturating_add(1)
                    .saturating_add(u16::try_from(label_width).unwrap_or(u16::MAX))
                    .min(area.right());
                crate::widgets::surface::paint_focus_rule(
                    buf,
                    Rect {
                        x: lip_x,
                        width: area.right() - lip_x,
                        ..area
                    },
                    self.theme,
                );
            }
        } else if crate::widgets::surface::paint_view_focus(
            buf,
            area,
            self.theme,
            self.focused,
            self.focus_gradient,
        ) {
            buf.set_span(area.x + 1, area.y, &title, title.width() as u16);
        }
        // The New File / New Folder / Refresh / Collapse toolbar is painted
        // later, on the root folder row. The "Views and More Actions" (⋯)
        // button sits here on the EXPLORER title line, mirroring VS Code's
        // view-container header. Reset the hit-test rects each frame so a
        // hidden affordance never registers stale clicks.
        self.header_new_file_btn = Rect::default();
        self.header_new_folder_btn = Rect::default();
        self.header_refresh_btn = Rect::default();
        self.header_collapse_btn = Rect::default();
        self.header_views_btn = Rect::default();
        self.last_area = area;
        self.last_scrollbar = Rect::default();
        self.sticky_rows.clear();

        // Paint the ⋯ button at the right end of the title border row. It is
        // always available (not focus-gated) so the view-toggle menu is one
        // click away even when the tree is unfocused, like VS Code. It wears
        // the shared header-action pill — a space-padded `" ⋯ "` inset one cell
        // from the rounded corner — so it matches the terminal `+` button
        // exactly instead of jamming a naked glyph against the corner.
        // Only when the pill clears the (translated) title, which starts one
        // cell in from the corner: `x + 1 + label_width <= x + width - 4`.
        if usize::from(area.width) >= label_width + 5
            && let Some(rect) = crate::widgets::header_pill::render_action(
                buf,
                area.x + area.width - 1,
                area.y,
                crate::widgets::header_pill::MORE_LABEL,
                self.theme,
                self.hover_pointer,
            )
        {
            self.header_views_btn = rect;
        }

        self.last_inner = inner;

        let visible_height = inner.height as usize;
        if visible_height == 0 {
            return;
        }
        // Every node, in tree order — `visible` holds node indices.
        let visible = self.visible_indices();
        // Position of the selected node within the filtered list, so scroll and
        // highlight track the cursor even when intermediate rows are hidden.
        let sel_pos = visible
            .iter()
            .position(|&i| i == self.selected)
            .unwrap_or(0);
        // Scroll is tracked in filtered-row positions. Re-derive it each frame
        // from `self.scroll` (a node index) by mapping to a position, then keep
        // the selection on screen.
        let mut scroll_pos = visible.iter().position(|&i| i >= self.scroll).unwrap_or(0);
        if sel_pos < scroll_pos {
            scroll_pos = sel_pos;
        } else if sel_pos >= scroll_pos + visible_height {
            scroll_pos = sel_pos + 1 - visible_height;
        }
        // Persist the node index at the top of the viewport so the next frame's
        // mapping is stable.
        self.scroll = visible.get(scroll_pos).copied().unwrap_or(0);
        let scrollbar_area = Rect {
            x: inner.x + inner.width.saturating_sub(1),
            y: inner.y,
            width: u16::from(inner.width > 0),
            height: inner.height,
        };
        let scrollbar_metrics =
            scrollbar::vertical_metrics(scrollbar_area, visible.len(), visible_height, scroll_pos);
        if let Some(metrics) = scrollbar_metrics {
            self.last_scrollbar = metrics.area;
        }
        let row_width = inner
            .width
            .saturating_sub(u16::from(scrollbar_metrics.is_some()));

        let pointer = self.hover_pointer;
        // No Explorer filter any more, so no characters are ever highlighted.
        let query = String::new();
        let end = (scroll_pos + visible_height).min(visible.len());
        // Croft Loch's indent guides: the guide of the selected row's parent
        // folder is drawn a step brighter down that folder's children (VS
        // Code's active indent guide), as `(level, first, end)` node indices.
        let active_guide = tones.and_then(|_| self.active_indent_guide());
        for (row, &idx) in visible[scroll_pos..end].iter().enumerate() {
            let node = &self.nodes[idx];
            let is_selected = idx == self.selected;
            let is_marked = self.marked.contains(&node.path);
            let is_drop_target = self.drag_target == Some(idx);
            let y = inner.y + row as u16;

            let mut spans: Vec<Span> = Vec::with_capacity(6);
            match tones {
                // Croft Loch: a thin guide under each ancestor folder's
                // chevron, in the cells the indent already takes. The
                // workspace root's level gets none: every row is under it,
                // so its guide would only be a second left edge.
                Some(t) if node.depth >= 2 => {
                    spans.push(Span::raw("  "));
                    for level in 1..node.depth {
                        let active = active_guide.is_some_and(|(l, first, end)| {
                            l == level && (first..end).contains(&idx)
                        });
                        spans.push(Span::styled(
                            "\u{258f} ",
                            Style::default().fg(if active { t.faint() } else { t.hairline() }),
                        ));
                    }
                }
                _ => spans.push(Span::raw("  ".repeat(node.depth))),
            }

            let name = node.label();

            // Git-ignored and changed rows colour the *name* only — icons
            // keep their color, matching VS Code's git decorations.
            let name_fg = self.name_fg(&node.path);
            // Croft Loch's quiet layer: chevrons, the rolled-up dot, the
            // root badge and note counts all in the muted ink.
            let mark_fg = tones.map_or_else(|| self.theme.ui(Color::Gray), |t| t.muted());

            if node.is_dir {
                let chev = crate::widgets::surface::chevron(self.theme, node.expanded);
                let icon = if node.expanded {
                    icons::FOLDER_OPEN
                } else {
                    icons::FOLDER_CLOSED
                };
                spans.push(Span::styled(
                    format!("{chev} "),
                    Style::default().fg(mark_fg),
                ));
                spans.push(Span::styled(
                    format!("{} ", icon.glyph),
                    Style::default().fg(self.folder_icon_fg(icon.color)),
                ));
                // Bold marks a folder on a boxed theme. Croft Loch keeps
                // weight for the workspace root alone, so the tree reads as
                // one calm list under a single heading.
                let base = if tones.is_some() && node.depth > 0 {
                    Style::default().fg(name_fg)
                } else {
                    Style::default().fg(name_fg).add_modifier(Modifier::BOLD)
                };
                push_name_spans(&mut spans, &name, &query, base, self.theme);
                // A collapsed folder hiding an unreviewed agent write shows a
                // dimmer copy of the file dot (#345), so the lane's changes
                // stay findable without expanding every folder. Only while
                // collapsed: expanded, the file's own dot is on screen.
                if !node.expanded && self.hides_agent_touched(&node.path) {
                    spans.push(Span::styled(
                        format!(" {AGENT_DOT}"),
                        Style::default().fg(mark_fg),
                    ));
                }
                // A lane root wears its branch and its agent after the name
                // (#348), dim so the folder name stays the thing the eye
                // lands on; trailing for the same reason as the agent dot.
                if node.depth == 0
                    && let Some(badge) = self.root_badges.get(&node.path)
                {
                    // `Gray`, the dim the theme maps for light grounds;
                    // `DarkGray` is a named colour `Theme::ui` passes through.
                    spans.push(Span::styled(
                        format!("  {badge}"),
                        Style::default().fg(mark_fg),
                    ));
                }
            } else {
                let suffix = node
                    .path
                    .extension()
                    .map(|e| format!(".{}", e.to_string_lossy()))
                    .unwrap_or_default();
                let icon = icons::for_path(&name, &suffix);
                spans.push(Span::raw("  "));
                spans.push(Span::styled(
                    format!("{} ", icon.glyph),
                    Style::default().fg(self.theme.ui(icon.color)),
                ));
                push_name_spans(
                    &mut spans,
                    &name,
                    &query,
                    Style::default().fg(name_fg),
                    self.theme,
                );
                // The agent-lane dot (#345), AFTER the name rather than
                // before it: a leading marker would shift every name in the
                // tree by a column as writes land, so the eye loses the
                // alignment it reads the tree by. Trailing, it appears and
                // clears without moving anything.
                if self.is_agent_touched(&node.path) {
                    // Croft Loch takes its own palette's yellow rather than
                    // the named colour, which follows the host terminal.
                    let dot = match tones {
                        Some(_) => crate::gradient::rgb_color(self.theme.ansi()[3]),
                        None => self.theme.ui(Color::Yellow),
                    };
                    spans.push(Span::styled(
                        format!(" {AGENT_DOT}"),
                        Style::default().fg(dot),
                    ));
                }
                if let Some(n) = self.note_counts.get(&node.path) {
                    spans.push(Span::styled(
                        format!(" \u{270e}{n}"),
                        Style::default().fg(mark_fg),
                    ));
                }
            }

            let line = Line::from(spans);
            let row_rect = Rect {
                x: inner.x,
                y,
                width: row_width,
                height: 1,
            };
            let drop_bg = self.theme.ui(Color::Rgb(0x2c, 0x60, 0x2e));
            if let Some(t) = tones {
                // Croft Loch: the one selected-row anatomy, the band with the
                // accent bar standing in the pane's left padding column so
                // the names keep their column; greyed while another pane has
                // the keyboard. Marked rows wear a softer tint of the band,
                // hover a neutral lift.
                if is_drop_target {
                    buf.set_style(row_rect, Style::default().bg(drop_bg));
                } else if is_selected {
                    crate::widgets::surface::paint_selected_row(
                        buf,
                        Rect {
                            x: area.x,
                            width: row_width.saturating_add(inner.x - area.x),
                            ..row_rect
                        },
                        t,
                        t.chrome,
                        self.focused,
                    );
                } else if is_marked {
                    let tint = crate::gradient::lerp_rgb(t.chrome, t.accent, 0.12);
                    buf.set_style(
                        row_rect,
                        Style::default().bg(crate::gradient::rgb_color(tint)),
                    );
                } else if let Some(bg) =
                    crate::widgets::hover::row_hover_bg(row_rect, pointer, self.theme)
                {
                    buf.set_style(row_rect, Style::default().bg(bg));
                }
            } else {
                // Black theme: the selected/marked rows wear the brand's
                // muted dark-teal fill (identical to the right-click menu's
                // selection), replacing the legacy bright-blue accent. Croft
                // Dark keeps the historical blue. The drop target stays
                // green either way.
                let (sel_bg, mark_bg) = if self.focus_gradient {
                    (
                        crate::gradient::rgb_color(crate::gradient::POPUP_SEL_BG),
                        self.theme.ui(Color::Rgb(0x18, 0x35, 0x32)),
                    )
                } else {
                    (
                        self.theme.ui(Color::Rgb(0x09, 0x4d, 0x77)),
                        self.theme.ui(Color::Rgb(0x07, 0x33, 0x55)),
                    )
                };
                let line_style = if is_drop_target {
                    Style::default().bg(drop_bg)
                } else if is_selected {
                    Style::default().bg(sel_bg)
                } else if is_marked {
                    Style::default().bg(mark_bg)
                } else if let Some(bg) =
                    crate::widgets::hover::row_hover_bg(row_rect, pointer, self.theme)
                {
                    Style::default().bg(bg)
                } else {
                    Style::default()
                };
                buf.set_style(row_rect, line_style);
            }
            // A changed row's status sits at the right edge (#1513): the
            // file's letter, or a dot on a folder with changes inside. The
            // name is clipped short of it so a long name never covers it.
            let badge = self
                .git_decoration(&node.path)
                .filter(|_| row_width >= 8 && !self.is_ignored(&node.path));
            match badge {
                Some(dec) => {
                    let glyph = if node.is_dir {
                        '\u{25cf}'
                    } else {
                        dec.letter()
                    };
                    buf.set_line(inner.x, y, &line, row_width - 3);
                    buf.set_string(
                        inner.x + row_width - 2,
                        y,
                        glyph.to_string(),
                        Style::default().fg(self.theme.git_decoration_fg(dec)),
                    );
                }
                None => {
                    buf.set_line(inner.x, y, &line, row_width);
                }
            }
        }
        // Explorer header toolbar: New File / New Folder / Refresh / Collapse
        // Folders, right-aligned on the root folder row (the "croft" header),
        // mirroring VS Code's view actions, which live on the workspace-folder
        // header rather than the EXPLORER title. Revealed only while the
        // Explorer pane is focused and the root row sits at the top of the
        // viewport; hidden otherwise (VS Code's hover/focus reveal). Painted
        // after the rows so the pills win over the root row's text/fill.
        if self.focused && self.scroll == 0 && !self.nodes.is_empty() {
            use crate::widgets::header_pill;
            const GLYPHS: [char; 4] = [
                header_pill::NEW_FILE_GLYPH,
                header_pill::NEW_FOLDER_GLYPH,
                header_pill::REFRESH_GLYPH,
                header_pill::COLLAPSE_ALL_GLYPH,
            ];
            // Each pill is a single-cell glyph; `step` adds a one-cell gap so
            // the chips read as four distinct buttons. `right_pad` keeps the
            // block clear of the scrollbar / right border.
            let step: u16 = 2;
            let count = GLYPHS.len() as u16;
            let block_w = count * step - 1;
            let right_pad: u16 = 1;
            // Approximate the root label footprint (chevron + space + folder
            // icon + space + name) so a narrow panel withholds the toolbar
            // rather than overprinting the folder name.
            let root_name = self.nodes[0]
                .path
                .file_name()
                .map(|n| n.to_string_lossy().chars().count())
                .unwrap_or(0) as u16;
            let label_w = 4 + root_name;
            if row_width > label_w + block_w + right_pad + 2 {
                let start_x = inner.x + row_width - right_pad - block_w;
                let y = inner.y;
                for (i, &glyph) in GLYPHS.iter().enumerate() {
                    let x = start_x + i as u16 * step;
                    let rect = Rect {
                        x,
                        y,
                        width: step,
                        height: 1,
                    };
                    let hovered = crate::widgets::hover::contains(rect, self.hover_pointer);
                    header_pill::render(buf, x, y, glyph, self.theme, hovered);
                    match i {
                        0 => self.header_new_file_btn = rect,
                        1 => self.header_new_folder_btn = rect,
                        2 => self.header_refresh_btn = rect,
                        _ => self.header_collapse_btn = rect,
                    }
                }
            }
        }
        // Sticky band (#117): pin the off-screen ancestor directories of the
        // top visible row, overpainting the topmost content rows — the
        // editor sticky band's model, including its guard: never cover the
        // selected row (the scroll clamp above keeps it on screen, so its
        // band-relative row bounds the band's height).
        if scroll_pos > 0 {
            const STICKY_MAX: usize = 3;
            let top_idx = visible[scroll_pos];
            let chain = sticky_ancestors(&self.nodes, top_idx, STICKY_MAX);
            let sel_row = sel_pos.saturating_sub(scroll_pos);
            let band = chain
                .len()
                .min(sel_row)
                .min(visible_height.saturating_sub(1));
            let shown = &chain[chain.len() - band..];
            // Croft Loch lifts the band onto the raised tone: the theme's
            // sticky tint is a hair off the chrome and vanished against it.
            let bg = tones.map_or_else(|| self.theme.sticky_scroll_bg(), |t| t.raised());
            let mark_fg = tones.map_or_else(|| self.theme.ui(Color::Gray), |t| t.muted());
            for (i, &aidx) in shown.iter().enumerate() {
                let y = inner.y + i as u16;
                for x in inner.x..inner.x + row_width {
                    buf[(x, y)].set_symbol(" ");
                    buf[(x, y)].set_style(Style::default().bg(bg));
                }
                let node = &self.nodes[aidx];
                let name = node.label();
                let name_fg = self.name_fg(&node.path);
                let name_style = if tones.is_some() && node.depth > 0 {
                    Style::default().fg(name_fg).bg(bg)
                } else {
                    Style::default()
                        .fg(name_fg)
                        .bg(bg)
                        .add_modifier(Modifier::BOLD)
                };
                let spans = vec![
                    Span::raw("  ".repeat(node.depth)),
                    Span::styled(
                        format!("{} ", crate::widgets::surface::chevron(self.theme, true)),
                        Style::default().fg(mark_fg).bg(bg),
                    ),
                    Span::styled(
                        format!("{} ", icons::FOLDER_OPEN.glyph),
                        Style::default()
                            .fg(self.folder_icon_fg(icons::FOLDER_OPEN.color))
                            .bg(bg),
                    ),
                    Span::styled(name, name_style),
                ];
                buf.set_line(inner.x, y, &Line::from(spans), row_width);
                self.sticky_rows.push((y, aidx));
            }
        }
        if let Some(metrics) = scrollbar_metrics {
            scrollbar::render_vertical_on_chrome(buf, metrics, self.focused, self.theme);
        }
    }
}

#[cfg(test)]
mod tests;
