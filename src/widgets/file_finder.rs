use ignore::{WalkBuilder, WalkState};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Widget},
};
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
    pub tier: MatchTier,
    pub score: i32,
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

    pub fn set_query(&mut self, q: &str) {
        if q == self.query {
            return;
        }
        self.query = q.to_string();
        self.refresh_results();
        self.selected = 0;
        self.scroll = 0;
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
                    tier: MatchTier::Subsequence,
                    score: 0,
                });
            }
            scored.sort_by(|a, b| a.entry.rel.cmp(&b.entry.rel));
            self.results = scored;
            return;
        }
        let mut top: Vec<(MatchTier, i32, usize)> = Vec::new();
        for (idx, entry) in self.entries.iter().enumerate() {
            if let Some((tier, score)) =
                score_entry(&needle, &entry.rel_lower, entry.filename_start)
            {
                top.push((tier, score, idx));
            }
        }
        let entries = &self.entries;
        top.sort_by(|a, b| {
            b.0.cmp(&a.0)
                .then_with(|| entries[a.2].rel.len().cmp(&entries[b.2].rel.len()))
                .then_with(|| b.1.cmp(&a.1))
                .then_with(|| entries[a.2].rel.cmp(&entries[b.2].rel))
        });
        top.truncate(MAX_RESULTS);
        self.results = top
            .into_iter()
            .map(|(tier, score, idx)| ScoredResult {
                entry: self.entries[idx].clone(),
                tier,
                score,
            })
            .collect();
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

pub fn build_file_index(root: &Path) -> Vec<FileEntry> {
    let collected: Arc<Mutex<Vec<FileEntry>>> = Arc::new(Mutex::new(Vec::new()));
    let root_buf = root.to_path_buf();
    let walker = WalkBuilder::new(root)
        .git_ignore(true)
        .require_git(false)
        .hidden(true)
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

    #[test]
    fn fuzzy_score_handles_mixed_case_via_lowercased_haystack() {
        let rel = "Sub/BetaFile.rs";
        let rel_lower = rel.to_lowercase();
        let filename_start = rel.rfind('/').map(|i| i + 1).unwrap_or(0);
        let score = fuzzy_score("beta", &rel_lower, filename_start);
        assert!(score.is_some());
    }
}
