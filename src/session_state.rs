use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// One open editor tab, captured so a re-exec into a freshly-installed
/// croft binary can reopen the file at the same cursor + scroll position.
/// `unsaved_text` is `Some` only when the buffer is dirty, so an in-place
/// update never silently drops uncommitted edits. `path` is `None` for an
/// untitled buffer, which is carried only when it has unsaved text.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpenTabState {
    pub path: Option<PathBuf>,
    pub cursor_row: usize,
    pub cursor_col: usize,
    pub scroll: usize,
    pub scroll_col: usize,
    pub dirty: bool,
    pub unsaved_text: Option<String>,
    /// An xlsx grid's unsaved cells (#1593): (worksheet name, body row,
    /// column, value), applied over the workbook as it opens. A CSV/TSV grid keeps
    /// its edits as `unsaved_text` instead, the text its save would write.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sheet_cells: Vec<crate::sheet::SheetCellEdit>,
}

/// The slice of running-croft state worth carrying across a self-re-exec.
///
/// Terminal panes are deliberately ABSENT, and that is not an oversight: the
/// per-workspace terminal store (`save_terminal_session` /
/// `restore_terminal_session`) already records pane cwds AND names, is written
/// before the re-exec, and is replayed at startup ahead of this handoff.
/// Duplicating panes here would append a second copy on top of the ones that
/// store already restored (#249 review).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionState {
    pub workspace_root: PathBuf,
    pub tabs: Vec<OpenTabState>,
    pub active_tab: usize,
    pub sidebar_view: String,
    pub sidebar_width: u16,
    pub terminal_height: Option<u16>,
    pub focus_editor: bool,
    /// Every file's bookmarks (1-based lines), the closed files' included,
    /// so a file's marks come back when it is opened again.
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub bookmarks: std::collections::HashMap<PathBuf, std::collections::BTreeSet<usize>>,
    /// What each bookmarked line read when the session was saved, by file
    /// and line, so marks on a file changed since come back on their text
    /// (see [`SessionState::relocate_bookmarks_on_disk`]).
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub bookmark_text:
        std::collections::HashMap<PathBuf, std::collections::BTreeMap<usize, String>>,
}

impl SessionState {
    pub fn save(&self, path: &Path) -> Result<()> {
        let json = serde_json::to_string(self).context("serializing session state")?;
        write_private_atomically(path, json.as_bytes())
    }

    pub fn load(path: &Path) -> Result<Self> {
        let json =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        serde_json::from_str(&json).context("parsing session state")
    }

    /// Put each bookmark back on its line's text in the file as it is on
    /// disk now. A file saved over since the session was (before a crash,
    /// or by another tool) has moved its lines, and its old numbers would
    /// mark other text. A mark whose line still reads the same stays; one
    /// whose line changed goes to the nearest line that reads what it did,
    /// and stays on its number when no line does. Reads the marked files,
    /// so it runs off the UI thread.
    pub fn relocate_bookmarks_on_disk(&mut self) {
        for (path, text) in &self.bookmark_text {
            let Some(set) = self.bookmarks.get_mut(path) else {
                continue;
            };
            let Ok(bytes) = std::fs::read(path) else {
                continue;
            };
            // A byte-order mark names a UTF-16 file's encoding; anything
            // else reads as UTF-8, and a line that does not decode the same
            // simply matches nothing.
            let encoding = encoding_rs::Encoding::for_bom(&bytes)
                .map_or(encoding_rs::UTF_8, |(encoding, _)| encoding);
            let (decoded, _, _) = encoding.decode(&bytes);
            let lines: Vec<&str> = decoded.lines().collect();
            *set = relocate_marks(set, text, &lines);
        }
    }

    /// Whether this session was saved for the workspace at `root`.
    pub fn is_of(&self, root: &Path) -> bool {
        crate::hot_exit::canonical(&self.workspace_root) == crate::hot_exit::canonical(root)
    }

    /// What a clean exit keeps for the next launch: the files open, where
    /// their carets were, the layout and the bookmarks, but no unsaved text.
    /// That is hot exit's to keep (#862): a copy here would come back over
    /// a later save, or a later discard, after a crash.
    pub fn without_unsaved_text(mut self) -> Self {
        let active = self.tabs.get(self.active_tab).and_then(|t| t.path.clone());
        // A tab with no file has nothing to reopen, and a path that is not
        // UTF-8 cannot be written as JSON: one would fail the whole save.
        self.tabs
            .retain(|t| t.path.as_deref().and_then(Path::to_str).is_some());
        for tab in &mut self.tabs {
            tab.dirty = false;
            tab.unsaved_text = None;
            tab.sheet_cells.clear();
        }
        self.active_tab = active
            .and_then(|p| self.tabs.iter().position(|t| t.path.as_ref() == Some(&p)))
            .unwrap_or(0);
        self
    }
}

/// `marks` (1-based lines) moved onto `lines` by the text each read when
/// it was set (`text`): kept where its line still reads the same, else on
/// the nearest line that does, else on its number. A blank line is no
/// anchor, since any blank line would match it.
fn relocate_marks(
    marks: &std::collections::BTreeSet<usize>,
    text: &std::collections::BTreeMap<usize, String>,
    lines: &[&str],
) -> std::collections::BTreeSet<usize> {
    let reads = |line: usize, want: &str| {
        line.checked_sub(1)
            .and_then(|i| lines.get(i))
            .is_some_and(|l| l.trim_end() == want)
    };
    marks
        .iter()
        .map(|&mark| {
            let Some(want) = text
                .get(&mark)
                .map(|t| t.trim_end())
                .filter(|t| !t.trim().is_empty())
            else {
                return mark;
            };
            if reads(mark, want) {
                return mark;
            }
            (1..=lines.len().max(mark))
                .flat_map(|d| [mark.checked_sub(d), Some(mark + d)])
                .flatten()
                .find(|&line| reads(line, want))
                .unwrap_or(mark)
        })
        .collect()
}

/// Write `bytes` to `path` owner-only and written aside, then renamed over:
/// the files that go through here (the relaunch handoff, the hot-exit
/// backup) carry unsaved text, and a crash mid-write must not leave half.
/// Creates `path`'s directory as needed.
pub(crate) fn write_private_atomically(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    use std::io::Write;
    #[cfg(unix)]
    use std::os::unix::fs::OpenOptionsExt;
    let tmp = path.with_extension(format!(
        "json.{}.{:?}.tmp",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_file(&tmp);
    let write = || -> std::io::Result<()> {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        opts.mode(0o600);
        let mut f = opts.open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)
    };
    write().map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        anyhow::Error::new(e).context(format!("writing {}", path.display()))
    })
}

/// Where the session saved at a clean exit is kept for the workspace at
/// `root`: one file per workspace under `dir`, named like its hot-exit
/// folder (a digest of the canonical root), so a launch from any directory
/// finds its own and two workspaces never share one.
pub fn saved_path(dir: &Path, root: &Path) -> PathBuf {
    crate::hot_exit::workspace_dir(dir, root).with_extension("json")
}

/// Per-pid scratch path for the session handoff file. Pid-scoping keeps
/// two concurrent croft sessions on the same remote from clobbering each
/// other's restore file.
pub fn handoff_path() -> PathBuf {
    let base = dirs_cache_croft();
    base.join(format!("session-{}.json", std::process::id()))
}

#[cfg(test)]
thread_local! {
    /// Where `dirs_cache_croft` points for the current test thread only.
    pub(crate) static CACHE_DIR_FOR_THIS_TEST_THREAD: std::cell::RefCell<Option<PathBuf>> =
        const { std::cell::RefCell::new(None) };
}

/// Points `dirs_cache_croft` at a dir on this test thread until dropped.
#[cfg(test)]
pub(crate) struct CacheDirForThisTestThread;

#[cfg(test)]
impl CacheDirForThisTestThread {
    pub(crate) fn set(dir: &Path) -> Self {
        CACHE_DIR_FOR_THIS_TEST_THREAD.with(|d| *d.borrow_mut() = Some(dir.to_path_buf()));
        Self
    }
}

#[cfg(test)]
impl Drop for CacheDirForThisTestThread {
    fn drop(&mut self) {
        CACHE_DIR_FOR_THIS_TEST_THREAD.with(|d| *d.borrow_mut() = None);
    }
}

pub(crate) fn dirs_cache_croft() -> PathBuf {
    // A test that redirects croft's cache must not reach the real one. The
    // redirect is per thread so tests running alongside keep their own.
    #[cfg(test)]
    if let Some(dir) = CACHE_DIR_FOR_THIS_TEST_THREAD.with(|d| d.borrow().clone()) {
        return dir;
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".cache").join("croft")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn marks(lines: &[usize]) -> std::collections::BTreeSet<usize> {
        lines.iter().copied().collect()
    }

    fn text(pairs: &[(usize, &str)]) -> std::collections::BTreeMap<usize, String> {
        pairs.iter().map(|&(l, t)| (l, t.to_string())).collect()
    }

    #[test]
    fn a_mark_whose_line_reads_the_same_stays() {
        let lines = ["a", "b", "c"];
        assert_eq!(
            relocate_marks(&marks(&[2]), &text(&[(2, "b")]), &lines),
            marks(&[2])
        );
    }

    #[test]
    fn a_mark_moves_to_the_nearest_line_that_reads_what_its_did() {
        // "b" moved down two lines, and another "b" sits further away.
        let lines = ["x", "y", "a", "b", "c", "d", "e", "b"];
        assert_eq!(
            relocate_marks(&marks(&[2]), &text(&[(2, "b")]), &lines),
            marks(&[4])
        );
        // Moved up, past the end of the file it now has.
        let lines = ["b"];
        assert_eq!(
            relocate_marks(&marks(&[3]), &text(&[(3, "b")]), &lines),
            marks(&[1])
        );
    }

    #[test]
    fn a_mark_with_no_line_reading_its_text_or_a_blank_one_keeps_its_number() {
        let lines = ["a", "", "c"];
        assert_eq!(
            relocate_marks(&marks(&[2]), &text(&[(2, "gone")]), &lines),
            marks(&[2])
        );
        let lines = ["", "a", ""];
        assert_eq!(
            relocate_marks(&marks(&[2]), &text(&[(2, "  ")]), &lines),
            marks(&[2]),
            "a blank line is no anchor"
        );
        assert_eq!(
            relocate_marks(&marks(&[2]), &text(&[]), &lines),
            marks(&[2]),
            "a mark saved without its text"
        );
    }

    #[test]
    fn round_trips_through_disk() {
        let dir = std::env::temp_dir().join(format!("croft-session-test-{}", std::process::id()));
        let path = dir.join("state.json");
        let state = SessionState {
            workspace_root: PathBuf::from("/work/repo"),
            tabs: vec![
                OpenTabState {
                    path: Some(PathBuf::from("/work/repo/src/main.rs")),
                    cursor_row: 12,
                    cursor_col: 4,
                    scroll: 3,
                    scroll_col: 0,
                    dirty: false,
                    unsaved_text: None,
                    sheet_cells: Vec::new(),
                },
                OpenTabState {
                    path: Some(PathBuf::from("/work/repo/README.md")),
                    cursor_row: 0,
                    cursor_col: 0,
                    scroll: 0,
                    scroll_col: 0,
                    dirty: true,
                    unsaved_text: Some(String::from("edited\nbody")),
                    sheet_cells: Vec::new(),
                },
            ],
            active_tab: 1,
            sidebar_view: String::from("Explorer"),
            sidebar_width: 32,
            terminal_height: Some(14),
            focus_editor: true,
            bookmarks: Default::default(),
            bookmark_text: Default::default(),
        };
        state.save(&path).expect("save");
        let loaded = SessionState::load(&path).expect("load");
        assert_eq!(state, loaded);
        // It carries unsaved text: owner-only.
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn handoff_path_is_pid_scoped_under_cache_croft() {
        let p = handoff_path();
        assert!(p.to_string_lossy().contains(".cache/croft"));
        assert!(
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("session-")
        );
    }

    /// #249: the handoff must NOT carry terminal panes. The per-workspace
    /// store already restores them (with names) and runs first at startup, so
    /// panes here would be appended on top of those — 2 panes becoming 3.
    /// Serialising the struct and asserting the key is absent pins that.
    #[test]
    fn the_handoff_does_not_carry_terminal_panes() {
        let state = SessionState {
            workspace_root: PathBuf::from("/work/repo"),
            tabs: Vec::new(),
            active_tab: 0,
            sidebar_view: String::from("Explorer"),
            sidebar_width: 32,
            terminal_height: None,
            focus_editor: true,
            bookmarks: Default::default(),
            bookmark_text: Default::default(),
        };
        let json = serde_json::to_string(&state).expect("serialize");
        assert!(
            !json.contains("terminals"),
            "panes belong to the per-workspace store, not the handoff: {json}"
        );
    }
}
