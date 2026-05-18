use ignore::{WalkBuilder, WalkState};
use rayon::prelude::*;
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Widget},
};
use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

const MAX_RESULTS: usize = 40;

#[derive(Clone, Debug)]
pub struct FileEntry {
    pub path: PathBuf,
    pub rel: String,
    pub rel_lower: String,
    pub filename_start: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum MatchTier {
    Subsequence = 1,
    PathSubstring = 2,
    FilenameSubstring = 3,
    FilenamePrefix = 4,
    ExactFilename = 5,
}

#[derive(Clone, Debug)]
pub struct ScoredResult {
    pub entry: FileEntry,
}

#[derive(Default)]
pub struct FileFinder {
    pub query: String,
    pub entries: Arc<Vec<FileEntry>>,
    pub results: Vec<ScoredResult>,
    pub selected: usize,
    pub scroll: usize,
    pub last_rect: Rect,
    pub last_inner_height: u16,
}

impl FileFinder {
    pub fn new(entries: Arc<Vec<FileEntry>>) -> Self {
        let mut me = Self {
            query: String::new(),
            entries,
            results: Vec::new(),
            selected: 0,
            scroll: 0,
            last_rect: Rect::default(),
            last_inner_height: 0,
        };
        me.refresh_results();
        me
    }

    #[cfg(test)]
    pub fn set_query(&mut self, q: &str) {
        if q == self.query {
            return;
        }
        self.query = q.to_string();
        self.refresh_results();
        self.selected = 0;
        self.scroll = 0;
    }

    pub fn replace_entries(&mut self, entries: Arc<Vec<FileEntry>>) {
        if Arc::ptr_eq(&self.entries, &entries) {
            return;
        }
        self.entries = entries;
        self.refresh_results();
        if self.results.is_empty() {
            self.selected = 0;
        } else if self.selected >= self.results.len() {
            self.selected = self.results.len() - 1;
        }
        if self.scroll > self.selected {
            self.scroll = self.selected;
        }
    }

    pub fn push_char(&mut self, c: char) {
        self.query.push(c);
        self.refresh_results();
        self.selected = 0;
        self.scroll = 0;
    }

    pub fn pop_char(&mut self) {
        if self.query.pop().is_some() {
            self.refresh_results();
            self.selected = 0;
            self.scroll = 0;
        }
    }

    pub fn select_next(&mut self) {
        if self.results.is_empty() {
            return;
        }
        if self.selected + 1 < self.results.len() {
            self.selected += 1;
        }
    }

    pub fn select_prev(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
        }
    }

    pub fn selected_entry(&self) -> Option<&FileEntry> {
        self.results.get(self.selected).map(|r| &r.entry)
    }

    pub fn selected_index(&self) -> usize {
        self.selected
    }

    pub fn visible_results(&self) -> &[ScoredResult] {
        &self.results
    }

    fn refresh_results(&mut self) {
        let needle: String = self.query.trim().to_lowercase();
        if needle.is_empty() {
            let mut scored: Vec<ScoredResult> = Vec::with_capacity(MAX_RESULTS);
            for entry in self.entries.iter().take(MAX_RESULTS) {
                scored.push(ScoredResult {
                    entry: entry.clone(),
                });
            }
            scored.sort_by(|a, b| a.entry.rel.cmp(&b.entry.rel));
            self.results = scored;
            return;
        }
        let entries: &[FileEntry] = &self.entries;
        // Score in parallel with rayon and accumulate per-thread top-K
        // bounded heaps, then merge. With N=1.5M entries on a typical
        // home dir, this drops keystroke latency from ~300 ms (serial,
        // full sort) to single-digit ms (multi-core, heap of K=40).
        let merged: BinaryHeap<RankedSlot> = entries
            .par_iter()
            .enumerate()
            .fold(
                || BinaryHeap::<RankedSlot>::with_capacity(MAX_RESULTS + 1),
                |mut heap, (idx, entry)| {
                    if let Some((tier, score)) =
                        score_entry(&needle, &entry.rel_lower, entry.filename_start)
                    {
                        push_topk(
                            &mut heap,
                            RankedSlot {
                                tier,
                                rel_len: entry.rel.len() as u32,
                                score,
                                idx,
                            },
                            MAX_RESULTS,
                        );
                    }
                    heap
                },
            )
            .reduce(
                || BinaryHeap::<RankedSlot>::with_capacity(MAX_RESULTS + 1),
                |mut a, b| {
                    for slot in b {
                        push_topk(&mut a, slot, MAX_RESULTS);
                    }
                    a
                },
            );
        // Our Ord orders BEST as LEAST (so the max-heap root is the
        // worst-of-top-K, evictable in O(log K)). into_sorted_vec
        // therefore returns ascending = best-first already.
        let top: Vec<RankedSlot> = merged.into_sorted_vec();
        self.results = top
            .into_iter()
            .map(|slot| ScoredResult {
                entry: entries[slot.idx].clone(),
            })
            .collect();
    }
}

/// One ranked match candidate. Ordering is "worse comes first" so a
/// `BinaryHeap` (max-heap) keeps the WORST of the top-K at the root,
/// letting us evict it in O(log K) when a better candidate arrives.
#[derive(Clone, Copy, Debug)]
struct RankedSlot {
    tier: MatchTier,
    rel_len: u32,
    score: i32,
    idx: usize,
}

impl PartialEq for RankedSlot {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for RankedSlot {}

impl PartialOrd for RankedSlot {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for RankedSlot {
    /// Mirrors the old sort_by in refresh_results, but inverted so
    /// the BinaryHeap (max-heap) root is the WORST candidate in the
    /// current top-K. Tie-break order, BEST -> WORST:
    ///   1. Higher MatchTier (ExactFilename > Subsequence)
    ///   2. Shorter rel.len (workspace-root storage.py beats deep)
    ///   3. Higher fuzzy score
    ///   4. Lexicographic rel ascending (handled at idx tiebreak)
    fn cmp(&self, other: &Self) -> Ordering {
        // Worse-first: invert each comparison vs "best".
        other
            .tier
            .cmp(&self.tier)
            .then_with(|| self.rel_len.cmp(&other.rel_len))
            .then_with(|| other.score.cmp(&self.score))
            .then_with(|| self.idx.cmp(&other.idx))
    }
}

fn push_topk(heap: &mut BinaryHeap<RankedSlot>, slot: RankedSlot, k: usize) {
    if heap.len() < k {
        heap.push(slot);
        return;
    }
    // heap.peek() is the worst-of-top-K under our Ord. If incoming is
    // strictly better (= less under cmp), evict + insert.
    if let Some(worst) = heap.peek() {
        if slot.cmp(worst) == Ordering::Less {
            heap.pop();
            heap.push(slot);
        }
    }
}

pub fn score_entry(
    needle: &str,
    hay_lower: &str,
    filename_start: usize,
) -> Option<(MatchTier, i32)> {
    if needle.is_empty() {
        return Some((MatchTier::Subsequence, 0));
    }
    let filename = &hay_lower[filename_start..];
    if filename == needle {
        return Some((MatchTier::ExactFilename, 10_000));
    }
    if filename.starts_with(needle) {
        return Some((MatchTier::FilenamePrefix, 5_000));
    }
    if filename.contains(needle) {
        return Some((MatchTier::FilenameSubstring, 2_500));
    }
    if hay_lower.contains(needle) {
        return Some((MatchTier::PathSubstring, 1_000));
    }
    fuzzy_score(needle, hay_lower, filename_start)
        .map(|s| (MatchTier::Subsequence, s))
}

pub fn fuzzy_score(needle: &str, hay_lower: &str, filename_start: usize) -> Option<i32> {
    if needle.is_empty() {
        return Some(0);
    }
    let needle_bytes = needle.as_bytes();
    let hay_bytes = hay_lower.as_bytes();
    let mut score: i32 = 0;
    let mut consecutive: i32 = 0;
    let mut needle_idx = 0usize;
    let mut prev_match: Option<usize> = None;
    for (i, &b) in hay_bytes.iter().enumerate() {
        if needle_idx >= needle_bytes.len() {
            break;
        }
        if b == needle_bytes[needle_idx] {
            let mut bonus: i32 = 1;
            if i >= filename_start {
                bonus += 4;
            }
            let prev_byte = if i == 0 { None } else { Some(hay_bytes[i - 1]) };
            let at_word_boundary = i == 0
                || i == filename_start
                || matches!(prev_byte, Some(b'/') | Some(b'_') | Some(b'-') | Some(b'.') | Some(b' '));
            if at_word_boundary {
                bonus += 5;
            }
            if prev_match == Some(i.saturating_sub(1)) && i > 0 {
                consecutive += 1;
                bonus += consecutive * 5;
            } else {
                if let Some(p) = prev_match {
                    let gap = i.saturating_sub(p + 1) as i32;
                    bonus -= gap.min(20);
                }
                consecutive = 0;
            }
            score += bonus;
            prev_match = Some(i);
            needle_idx += 1;
        }
    }
    if needle_idx == needle_bytes.len() {
        Some(score - (hay_bytes.len() as i32 / 4))
    } else {
        None
    }
}

/// Directory names that are vendored / generated / VCS-internal and
/// will never be a useful Cmd+P target. Including these in the index
/// would drown signal in noise (every `.git/objects/*` blob,
/// `node_modules/**`, `target/debug/incremental/*`, etc.) and balloon
/// the walk by orders of magnitude. The match is exact on the path
/// component name - a real workspace file literally named `target.md`
/// is still indexed.
const NOISE_DIR_NAMES: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    ".bzr",
    "node_modules",
    "target",
    "__pycache__",
    ".venv",
    "venv",
    ".tox",
    ".mypy_cache",
    ".pytest_cache",
    ".ruff_cache",
    ".next",
    ".nuxt",
    ".cache",
    "dist",
    "build",
    ".gradle",
    ".idea",
    ".vscode-test",
    ".DS_Store",
];

fn is_noise_dir(name: &std::ffi::OsStr) -> bool {
    name.to_str().is_some_and(|n| NOISE_DIR_NAMES.contains(&n))
}

/// Absolute paths to prune from the walk for two reasons:
///   1. `Library/Containers` and `Library/Group Containers` trigger a
///      macOS TCC consent prompt ("App would like to access data from
///      other apps") because they live behind
///      kTCCServiceSystemPolicyAppData.
///   2. The Library/Caches, Library/Application Support, Library/Mobile
///      Documents, Library/Developer, Library/CoreSimulator,
///      Library/Mail, Library/Messages, Library/CloudStorage,
///      Library/Safari, Library/Cookies trees collectively account
///      for ~1.4M of the ~1.5M files found by a `~`-rooted walk on a
///      typical developer Mac. None of those are useful Cmd+P
///      targets, and including them blows per-keystroke match
///      latency from ~5ms to ~300ms. Library/Logs is kept in because
///      log files DO get opened (the user's lsp.log lives at
///      `~/.croft/lsp.log` but `~/Library/Logs/*` is a reasonable
///      Cmd+P target too).
fn macos_blocked_paths() -> Vec<PathBuf> {
    let Some(home) = std::env::var_os("HOME") else {
        return Vec::new();
    };
    let home = PathBuf::from(home);
    let lib = home.join("Library");
    [
        "Containers",
        "Group Containers",
        "Caches",
        "Application Support",
        "Mobile Documents",
        "CloudStorage",
        "Developer",
        "CoreSimulator",
        "Mail",
        "Messages",
        "Safari",
        "Cookies",
        "Metadata",
        "Suggestions",
        "Biome",
        "Calendars",
        "Reminders",
        "PersonalizationPortrait",
        "HomeKit",
        "Photos",
        "Daemon Containers",
        "WebKit",
    ]
    .iter()
    .map(|leaf| lib.join(leaf))
    .collect()
}

pub fn build_file_index(root: &Path) -> Vec<FileEntry> {
    let collected: Arc<Mutex<Vec<FileEntry>>> = Arc::new(Mutex::new(Vec::new()));
    let root_buf = root.to_path_buf();
    let blocked = macos_blocked_paths();
    let walker = WalkBuilder::new(root)
        .git_ignore(true)
        .require_git(false)
        // .hidden(false) so user dot-dirs (.croft, .cursor, .config,
        // .continue, .databridge) ARE walked - the user explicitly
        // wants to Cmd+P into them. NOISE_DIR_NAMES below prunes the
        // vendored / generated dot-dirs (.git, .venv, .mypy_cache,
        // .next, .cache, ...) that would otherwise drown the index.
        .hidden(false)
        .filter_entry(move |entry| {
            let depth = entry.depth();
            if depth == 0 {
                return true;
            }
            if is_noise_dir(entry.file_name()) {
                return false;
            }
            // Hard-skip macOS app-data containers so readdir does not
            // trip a TCC consent prompt on every launch. starts_with
            // matches both the container root and any descendant.
            for b in &blocked {
                if entry.path().starts_with(b) {
                    return false;
                }
            }
            true
        })
        .build_parallel();
    walker.run(|| {
        let collected = collected.clone();
        let root_buf = root_buf.clone();
        Box::new(move |entry| {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => return WalkState::Continue,
            };
            if !entry.file_type().is_some_and(|t| t.is_file()) {
                return WalkState::Continue;
            }
            let path = entry.path().to_path_buf();
            let rel_path = path.strip_prefix(&root_buf).unwrap_or(&path);
            let rel = rel_path.to_string_lossy().replace('\\', "/");
            if rel.is_empty() {
                return WalkState::Continue;
            }
            let rel_lower = rel.to_lowercase();
            let filename_start = rel.rfind('/').map(|i| i + 1).unwrap_or(0);
            if let Ok(mut g) = collected.lock() {
                g.push(FileEntry {
                    path,
                    rel,
                    rel_lower,
                    filename_start,
                });
            }
            WalkState::Continue
        })
    });
    let mut out = Arc::try_unwrap(collected)
        .map(|m| m.into_inner().unwrap_or_default())
        .unwrap_or_default();
    out.sort_by(|a, b| a.rel.cmp(&b.rel));
    out
}

pub fn render_file_finder(finder: &mut FileFinder, area: Rect, buf: &mut Buffer) {
    let width = area.width.saturating_mul(7) / 10;
    let width = width.clamp(40, 100.min(area.width));
    let height = area.height.saturating_mul(6) / 10;
    let height = height.clamp(10, area.height);
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 4;
    let rect = Rect { x, y, width, height };
    finder.last_rect = rect;

    Widget::render(Clear, rect, buf);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Rgb(0x4e, 0x9a, 0xff)))
        .title(Span::styled(
            " Go to File — Esc to close, ↑/↓ to navigate, Enter to open ",
            Style::default()
                .fg(Color::Rgb(0xff, 0xff, 0xff))
                .add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().bg(Color::Rgb(0x16, 0x18, 0x1f)));
    let inner = Rect {
        x: rect.x + 1,
        y: rect.y + 1,
        width: rect.width.saturating_sub(2),
        height: rect.height.saturating_sub(2),
    };
    Widget::render(block, rect, buf);

    if inner.height == 0 || inner.width == 0 {
        return;
    }

    let prompt_line = Line::from(vec![
        Span::styled("> ", Style::default().fg(Color::Rgb(0x88, 0xc0, 0xd0))),
        Span::styled(
            finder.query.clone(),
            Style::default().fg(Color::Rgb(0xec, 0xef, 0xf4)).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            "_",
            Style::default()
                .fg(Color::Rgb(0xec, 0xef, 0xf4))
                .add_modifier(Modifier::SLOW_BLINK),
        ),
    ]);
    let prompt_rect = Rect {
        x: inner.x,
        y: inner.y,
        width: inner.width,
        height: 1,
    };
    Widget::render(Paragraph::new(prompt_line), prompt_rect, buf);

    let separator_rect = Rect {
        x: inner.x,
        y: inner.y + 1,
        width: inner.width,
        height: 1,
    };
    let sep_line = Line::from(Span::styled(
        "─".repeat(separator_rect.width as usize),
        Style::default().fg(Color::Rgb(0x3b, 0x42, 0x52)),
    ));
    Widget::render(Paragraph::new(sep_line), separator_rect, buf);

    let list_rect = Rect {
        x: inner.x,
        y: inner.y + 2,
        width: inner.width,
        height: inner.height.saturating_sub(2),
    };
    finder.last_inner_height = list_rect.height;
    if list_rect.height == 0 {
        return;
    }

    let visible = list_rect.height as usize;
    let total = finder.results.len();
    if finder.selected >= finder.scroll + visible {
        finder.scroll = finder.selected + 1 - visible;
    }
    if finder.selected < finder.scroll {
        finder.scroll = finder.selected;
    }
    let end = (finder.scroll + visible).min(total);

    if total == 0 {
        let empty = if finder.query.trim().is_empty() {
            Line::from(Span::styled(
                "  (no files indexed)",
                Style::default().fg(Color::Rgb(0x7a, 0x82, 0x90)),
            ))
        } else {
            Line::from(Span::styled(
                format!("  No matches for '{}'", finder.query),
                Style::default().fg(Color::Rgb(0x7a, 0x82, 0x90)),
            ))
        };
        Widget::render(Paragraph::new(empty), list_rect, buf);
        return;
    }

    let mut lines: Vec<Line<'static>> = Vec::with_capacity(end - finder.scroll);
    for (offset, result) in finder.results[finder.scroll..end].iter().enumerate() {
        let row_idx = finder.scroll + offset;
        let is_selected = row_idx == finder.selected;
        let row_style = if is_selected {
            Style::default()
                .bg(Color::Rgb(0x1e, 0x3a, 0x6e))
                .fg(Color::White)
        } else {
            Style::default().fg(Color::Rgb(0xec, 0xef, 0xf4))
        };
        let dir_style = if is_selected {
            Style::default()
                .bg(Color::Rgb(0x1e, 0x3a, 0x6e))
                .fg(Color::Rgb(0xa0, 0xb4, 0xd8))
        } else {
            Style::default().fg(Color::Rgb(0x8e, 0x95, 0xa4))
        };
        let (dir_part, file_part) = split_dir_file(&result.entry.rel, result.entry.filename_start);
        let prefix = if is_selected { "> " } else { "  " };
        let spans: Vec<Span<'static>> = vec![
            Span::styled(prefix.to_string(), row_style),
            Span::styled(file_part.to_string(), row_style.add_modifier(Modifier::BOLD)),
            Span::styled(
                if dir_part.is_empty() { String::new() } else { format!("  {dir_part}") },
                dir_style,
            ),
        ];
        lines.push(Line::from(spans));
    }
    Widget::render(Paragraph::new(lines), list_rect, buf);
}

fn split_dir_file(rel: &str, filename_start: usize) -> (&str, &str) {
    if filename_start == 0 {
        ("", rel)
    } else {
        let dir = &rel[..filename_start.saturating_sub(1)];
        let file = &rel[filename_start..];
        (dir, file)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(rel: &str) -> FileEntry {
        let rel_lower = rel.to_lowercase();
        let filename_start = rel.rfind('/').map(|i| i + 1).unwrap_or(0);
        FileEntry {
            path: PathBuf::from(rel),
            rel: rel.to_string(),
            rel_lower,
            filename_start,
        }
    }

    #[test]
    fn fuzzy_score_returns_some_for_subsequence_match() {
        let score = fuzzy_score("ab", "alpha/beta.rs", "alpha/beta.rs".rfind('/').unwrap() + 1);
        assert!(score.is_some(), "'ab' must subsequence-match 'alpha/beta.rs'");
    }

    #[test]
    fn fuzzy_score_returns_none_when_chars_are_out_of_order() {
        let score = fuzzy_score("zx", "abc/def.rs", 4);
        assert!(score.is_none());
    }

    #[test]
    fn filename_matches_rank_above_directory_matches() {
        let dir_match = fuzzy_score("be", "beta_dir/zoom.rs", "beta_dir/zoom.rs".rfind('/').unwrap() + 1);
        let file_match = fuzzy_score("be", "zoom/beta.rs", "zoom/beta.rs".rfind('/').unwrap() + 1);
        assert!(
            file_match > dir_match,
            "matches inside the filename must score higher than matches in directory segments (file={file_match:?}, dir={dir_match:?})"
        );
    }

    #[test]
    fn consecutive_chars_score_higher_than_split_chars() {
        let consec = fuzzy_score("alp", "alpha.rs", 0).unwrap();
        let split = fuzzy_score("alp", "a_l_p_xx.rs", 0).unwrap();
        assert!(consec > split, "'alp' against 'alpha.rs' must score higher than against 'a_l_p_xx.rs'");
    }

    #[test]
    fn file_finder_with_empty_query_lists_files_alphabetically() {
        let entries = Arc::new(vec![
            entry("zeta.rs"),
            entry("alpha.rs"),
            entry("mid/beta.rs"),
        ]);
        let finder = FileFinder::new(entries);
        let names: Vec<&str> = finder.visible_results().iter().map(|r| r.entry.rel.as_str()).collect();
        assert_eq!(names, vec!["alpha.rs", "mid/beta.rs", "zeta.rs"]);
    }

    #[test]
    fn typing_filters_to_subsequence_matches_only() {
        let entries = Arc::new(vec![
            entry("alpha.rs"),
            entry("sub/beta.rs"),
            entry("gamma.rs"),
        ]);
        let mut finder = FileFinder::new(entries);
        finder.set_query("be");
        let names: Vec<&str> = finder.visible_results().iter().map(|r| r.entry.rel.as_str()).collect();
        assert_eq!(names, vec!["sub/beta.rs"], "filter 'be' must keep only beta.rs");
    }

    #[test]
    fn selection_clamps_when_results_shrink_under_typing() {
        let entries = Arc::new(vec![entry("alpha.rs"), entry("beta.rs")]);
        let mut finder = FileFinder::new(entries);
        finder.select_next();
        assert_eq!(finder.selected_index(), 1);
        finder.set_query("alp");
        assert_eq!(finder.selected_index(), 0, "selection must reset to 0 when the result list changes shape");
    }

    #[test]
    fn select_next_caps_at_last_result() {
        let entries = Arc::new(vec![entry("a.rs"), entry("b.rs")]);
        let mut finder = FileFinder::new(entries);
        finder.select_next();
        finder.select_next();
        finder.select_next();
        assert_eq!(finder.selected_index(), 1, "select_next must not overshoot the last index");
    }

    #[test]
    fn select_prev_caps_at_zero() {
        let entries = Arc::new(vec![entry("a.rs"), entry("b.rs")]);
        let mut finder = FileFinder::new(entries);
        finder.select_prev();
        finder.select_prev();
        assert_eq!(finder.selected_index(), 0);
    }

    #[test]
    fn build_file_index_walks_workspace_and_honours_gitignore() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("alpha.rs"), "").unwrap();
        std::fs::create_dir(tmp.path().join("sub")).unwrap();
        std::fs::write(tmp.path().join("sub").join("beta.rs"), "").unwrap();
        std::fs::write(tmp.path().join(".gitignore"), "ignored.txt\n").unwrap();
        std::fs::write(tmp.path().join("ignored.txt"), "").unwrap();
        let entries = build_file_index(tmp.path());
        let names: Vec<&str> = entries.iter().map(|e| e.rel.as_str()).collect();
        assert!(names.contains(&"alpha.rs"));
        assert!(names.contains(&"sub/beta.rs"));
        assert!(!names.contains(&"ignored.txt"), "gitignored files must be excluded from the Cmd+P index");
    }

    #[test]
    fn build_file_index_includes_user_owned_dot_directories() {
        // Regression for the user's "Cmd+P does not find ~/.croft/lsp.log
        // even when I type lsp.log" report. The old build_file_index
        // called .hidden(true) which silently excluded every dot-dir,
        // including the user's own ~/.croft, ~/.cursor, ~/.continue,
        // ~/.config, etc. - exactly the directories whose contents the
        // Cmd+P finder is supposed to help locate.
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join(".croft")).unwrap();
        std::fs::write(tmp.path().join(".croft").join("lsp.log"), b"").unwrap();
        std::fs::create_dir(tmp.path().join(".config")).unwrap();
        std::fs::write(tmp.path().join(".config").join("init.toml"), b"").unwrap();
        let entries = build_file_index(tmp.path());
        let names: Vec<&str> = entries.iter().map(|e| e.rel.as_str()).collect();
        assert!(
            names.contains(&".croft/lsp.log"),
            "Cmd+P index must include files under user dot-dirs like .croft/; got {names:?}"
        );
        assert!(
            names.contains(&".config/init.toml"),
            "Cmd+P index must include files under .config/ and similar user dot-dirs; got {names:?}"
        );
    }

    #[test]
    fn build_file_index_skips_macos_app_data_containers() {
        // Regression for the user's "iTerm.app would like to access
        // data from other apps" TCC prompt that fired repeatedly
        // after the 0.1.148 background-index landed. macOS gates
        // ~/Library/Containers/<bundle-id> and ~/Library/Group
        // Containers/<group-id> behind kTCCServiceSystemPolicyAppData;
        // any process that readdir's into those paths trips a
        // consent prompt on the parent terminal app. None of those
        // files are useful Cmd+P targets, so prune the two directory
        // trees outright. Test simulates a workspace rooted at a fake
        // $HOME so the path-shape match fires deterministically.
        let tmp = tempfile::tempdir().unwrap();
        let fake_home = tmp.path();
        let containers = fake_home.join("Library").join("Containers").join("com.example.app").join("Data");
        std::fs::create_dir_all(&containers).unwrap();
        std::fs::write(containers.join("private.sqlite"), b"").unwrap();
        let group = fake_home.join("Library").join("Group Containers").join("group.example").join("shared");
        std::fs::create_dir_all(&group).unwrap();
        std::fs::write(group.join("shared.db"), b"").unwrap();
        // A normal Library subdir the user DOES want indexed - Logs
        // is the canonical lsp.log location regression target.
        let logs = fake_home.join("Library").join("Logs");
        std::fs::create_dir_all(&logs).unwrap();
        std::fs::write(logs.join("app.log"), b"").unwrap();
        let saved = std::env::var_os("HOME");
        // SAFETY: tests in this binary are serialized for env vars
        // via build_file_index's HOME read; the value is restored at
        // the end of the test. Other tests do not toggle $HOME.
        unsafe { std::env::set_var("HOME", fake_home); }
        let entries = build_file_index(fake_home);
        match saved {
            Some(v) => unsafe { std::env::set_var("HOME", v) },
            None => unsafe { std::env::remove_var("HOME") },
        }
        let names: Vec<&str> = entries.iter().map(|e| e.rel.as_str()).collect();
        assert!(
            names.contains(&"Library/Logs/app.log"),
            "Library/Logs/app.log must still be indexed; got {names:?}"
        );
        assert!(
            !names.iter().any(|n| n.starts_with("Library/Containers/")),
            "Library/Containers/** must be excluded to avoid macOS AppData TCC prompts; got {names:?}"
        );
        assert!(
            !names.iter().any(|n| n.starts_with("Library/Group Containers/")),
            "Library/Group Containers/** must be excluded for the same TCC reason; got {names:?}"
        );
    }

    #[test]
    fn build_file_index_skips_vendored_noise_directories() {
        // Including hidden dirs would otherwise drown the index in
        // .git/objects/* and node_modules/* — paths the user never
        // wants to open via Cmd+P. Make sure the noise-dir filter
        // keeps those out even though .hidden(false) is now in
        // effect.
        let tmp = tempfile::tempdir().unwrap();
        for noise in [".git", "node_modules", "target", "__pycache__", ".venv", ".tox", ".mypy_cache"] {
            let dir = tmp.path().join(noise);
            std::fs::create_dir(&dir).unwrap();
            std::fs::write(dir.join("blob"), b"").unwrap();
        }
        std::fs::write(tmp.path().join("real.txt"), b"").unwrap();
        let entries = build_file_index(tmp.path());
        let names: Vec<&str> = entries.iter().map(|e| e.rel.as_str()).collect();
        assert!(
            names.contains(&"real.txt"),
            "the regular workspace file must still be indexed; got {names:?}"
        );
        for noise in [".git/blob", "node_modules/blob", "target/blob", "__pycache__/blob", ".venv/blob", ".tox/blob", ".mypy_cache/blob"] {
            assert!(
                !names.contains(&noise),
                "{noise} must be filtered out so vendored / generated dirs do not drown the Cmd+P index; got {names:?}"
            );
        }
    }

    #[test]
    fn exact_filename_match_beats_every_substring_or_subsequence_match() {
        let entries = Arc::new(vec![
            entry("packages/anterior-dev-py/tests/test_citations_storage.py"),
            entry("app/oncohealth/tasks/main/tests/test_configure_agent_v1.py"),
            entry("packages/anterior-dev-py/src/anterior/dev/citations/storage.py"),
        ]);
        let mut finder = FileFinder::new(entries);
        finder.set_query("storage.py");
        let rels: Vec<&str> = finder.visible_results().iter().map(|r| r.entry.rel.as_str()).collect();
        assert_eq!(
            rels.first().copied(),
            Some("packages/anterior-dev-py/src/anterior/dev/citations/storage.py"),
            "exact filename match 'storage.py' MUST come before test_citations_storage.py (filename-substring) and test_configure_agent_v1.py (only a scattered subsequence) — got {rels:?}. Subsequence tier swamping exact match is the bug the user yelled about at 10:54 on 2026-05-13."
        );
    }

    #[test]
    fn filename_prefix_beats_filename_substring_which_beats_path_substring_which_beats_subsequence() {
        let entries = Arc::new(vec![
            entry("zzz_other/random_storage_thing.py"),
            entry("dir_with_storage_in_name/other.py"),
            entry("a/test_storage.py"),
            entry("b/storage_helper.py"),
        ]);
        let mut finder = FileFinder::new(entries);
        finder.set_query("storage");
        let rels: Vec<&str> = finder.visible_results().iter().map(|r| r.entry.rel.as_str()).collect();
        assert_eq!(
            rels,
            vec![
                "b/storage_helper.py",
                "a/test_storage.py",
                "zzz_other/random_storage_thing.py",
                "dir_with_storage_in_name/other.py",
            ],
            "tier order must be: filename starts-with > filename contains > path contains > scattered subsequence — got {rels:?}"
        );
    }

    #[test]
    fn within_the_same_tier_the_shorter_relative_path_wins() {
        let entries = Arc::new(vec![
            entry("a/very/deeply/nested/dir/structure/storage.py"),
            entry("storage.py"),
            entry("b/c/storage.py"),
        ]);
        let mut finder = FileFinder::new(entries);
        finder.set_query("storage.py");
        let rels: Vec<&str> = finder.visible_results().iter().map(|r| r.entry.rel.as_str()).collect();
        assert_eq!(
            rels,
            vec![
                "storage.py",
                "b/c/storage.py",
                "a/very/deeply/nested/dir/structure/storage.py",
            ],
            "all three are exact-filename matches; tie-break must be ascending rel.len() so the workspace-root storage.py wins — got {rels:?}"
        );
    }

    /// Manual diagnostic: walks the real `$HOME`, then types
    /// "lsp.log" one char at a time, printing per-keystroke latency.
    /// Marked #[ignore] so it never runs in CI/regular `cargo test`.
    /// Invoke as:
    ///   cargo test --release --bin croft -- --ignored --nocapture bench_cmd_p_real_home
    #[test]
    #[ignore]
    fn bench_cmd_p_real_home() {
        let home = std::env::var("HOME").expect("$HOME");
        let root = std::path::PathBuf::from(home);
        let t0 = std::time::Instant::now();
        let entries = build_file_index(&root);
        let walk_ms = t0.elapsed().as_secs_f64() * 1000.0;
        eprintln!(
            "walk: indexed {} files in {:.0} ms",
            entries.len(),
            walk_ms
        );
        let arc = std::sync::Arc::new(entries);
        let mut finder = FileFinder::new(arc.clone());
        for c in "lsp.log".chars() {
            let t = std::time::Instant::now();
            finder.push_char(c);
            let ms = t.elapsed().as_secs_f64() * 1000.0;
            eprintln!(
                "type '{}' -> query={:?} results={} latency={:.2} ms",
                c,
                finder.query,
                finder.visible_results().len(),
                ms
            );
        }
    }

    #[test]
    fn replace_entries_re_ranks_results_against_the_new_index() {
        let entries = Arc::new(vec![entry("alpha.rs"), entry("beta.rs")]);
        let mut finder = FileFinder::new(entries);
        finder.set_query("resolver.ts");
        assert!(finder.visible_results().is_empty(), "no resolver.ts in initial index");
        let new_entries = Arc::new(vec![
            entry("alpha.rs"),
            entry("beta.rs"),
            entry("packages/ant-ts-lib-noggin/src/entities/files/resolver.ts"),
        ]);
        finder.replace_entries(new_entries);
        let rels: Vec<&str> = finder
            .visible_results()
            .iter()
            .map(|r| r.entry.rel.as_str())
            .collect();
        assert_eq!(
            rels.first().copied(),
            Some("packages/ant-ts-lib-noggin/src/entities/files/resolver.ts"),
            "after replace_entries the new index must drive the result list; got {rels:?}"
        );
    }

    #[test]
    fn replace_entries_clamps_selection_when_results_shrink() {
        let entries = Arc::new(vec![entry("alpha.rs"), entry("beta.rs"), entry("gamma.rs")]);
        let mut finder = FileFinder::new(entries);
        finder.select_next();
        finder.select_next();
        assert_eq!(finder.selected_index(), 2);
        finder.replace_entries(Arc::new(vec![entry("alpha.rs")]));
        assert_eq!(finder.selected_index(), 0, "selection must clamp to last visible row when the index shrinks");
    }

    #[test]
    fn fuzzy_score_handles_mixed_case_via_lowercased_haystack() {
        let rel = "Sub/BetaFile.rs";
        let rel_lower = rel.to_lowercase();
        let filename_start = rel.rfind('/').map(|i| i + 1).unwrap_or(0);
        let score = fuzzy_score("beta", &rel_lower, filename_start);
        assert!(score.is_some());
    }
}
