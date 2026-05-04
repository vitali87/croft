use ignore::WalkBuilder;
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Widget},
};
use std::path::{Path, PathBuf};

const MAX_HITS: usize = 200;
const MAX_LINE_LEN: usize = 200;
const MAX_FILE_BYTES: u64 = 5 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchHit {
    pub path: PathBuf,
    /// 1-indexed line number, the way humans / editors talk about lines.
    pub line_no: usize,
    /// The matched line, with surrounding whitespace trimmed and length capped.
    pub line_text: String,
}

/// Search the workspace rooted at `root` for `query` and return up to
/// `MAX_HITS` matches.  Substring match, case-insensitive.  Honours
/// `.gitignore` (via the `ignore` crate) so generated files don't dominate.
pub fn search_workspace(root: &Path, query: &str) -> Vec<SearchHit> {
    let q = query.trim();
    if q.is_empty() {
        return Vec::new();
    }
    let needle = q.to_lowercase();
    let mut hits: Vec<SearchHit> = Vec::new();
    let walker = WalkBuilder::new(root)
        .git_ignore(true)
        .hidden(true)
        .build();
    for entry in walker.flatten() {
        if hits.len() >= MAX_HITS {
            break;
        }
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        if !is_searchable_size(path) {
            continue;
        }
        match std::fs::read_to_string(path) {
            Ok(content) => collect_matches_in_text(path, &content, &needle, &mut hits),
            Err(_) => {} // binary or unreadable; skip silently
        }
    }
    hits
}

/// Split `line` into `(segment, is_match)` runs against a case-insensitive
/// `needle`. Concatenating the segments reproduces the original line
/// byte-for-byte. Empty needle or no match returns the whole line as a
/// single non-match segment. Used by the result-row renderer to highlight
/// every occurrence of the user's query inside the matched line.
pub fn split_for_highlight(line: &str, needle: &str) -> Vec<(String, bool)> {
    if needle.is_empty() {
        return vec![(line.to_string(), false)];
    }
    let lower_line = line.to_lowercase();
    let lower_needle = needle.to_lowercase();
    // If lowercasing changed the byte length (rare: e.g. some Unicode
    // titlecase forms), the lower_line ↔ line index mapping no longer
    // holds; bail out without highlights rather than risk slicing into
    // the middle of a UTF-8 codepoint.
    if lower_line.len() != line.len() {
        return vec![(line.to_string(), false)];
    }
    let mut out: Vec<(String, bool)> = Vec::new();
    let mut last = 0usize;
    let mut start = 0usize;
    while let Some(rel) = lower_line[start..].find(&lower_needle) {
        let abs = start + rel;
        if abs > last {
            out.push((line[last..abs].to_string(), false));
        }
        let end = abs + lower_needle.len();
        out.push((line[abs..end].to_string(), true));
        last = end;
        start = end;
        if lower_needle.is_empty() {
            break;
        }
    }
    if last < line.len() {
        out.push((line[last..].to_string(), false));
    }
    if out.is_empty() {
        out.push((line.to_string(), false));
    }
    out
}

/// `(query_that_was_run, hits)`. The query is echoed back so the receiver
/// can drop stale results when the user has typed past the query that
/// produced them.
pub type SearchResult = (String, Vec<SearchHit>);

/// Background worker loop. Reads queries from `rx`, coalesces by always
/// taking the most recent pending query, debounces ~120 ms so a fast typist
/// doesn't trigger a search per keystroke, runs `search_workspace`, and
/// ships `(query, hits)` back via `tx`. Empty queries short-circuit to
/// empty hits without walking the tree. The thread exits cleanly when the
/// query channel closes (App dropped).
pub fn search_worker_loop(
    root: PathBuf,
    rx: std::sync::mpsc::Receiver<String>,
    tx: std::sync::mpsc::Sender<SearchResult>,
) {
    use std::time::Duration;
    while let Ok(mut query) = rx.recv() {
        while let Ok(newer) = rx.try_recv() {
            query = newer;
        }
        std::thread::sleep(Duration::from_millis(120));
        while let Ok(newer) = rx.try_recv() {
            query = newer;
        }
        let hits = if query.trim().is_empty() {
            Vec::new()
        } else {
            search_workspace(&root, &query)
        };
        if tx.send((query, hits)).is_err() {
            return;
        }
    }
}

fn is_searchable_size(path: &Path) -> bool {
    std::fs::metadata(path)
        .map(|m| m.len() <= MAX_FILE_BYTES)
        .unwrap_or(false)
}

/// Pure helper used by `search_workspace` and unit-tested directly.
pub fn collect_matches_in_text(
    path: &Path,
    content: &str,
    lowercase_needle: &str,
    out: &mut Vec<SearchHit>,
) {
    for (idx, line) in content.lines().enumerate() {
        if out.len() >= MAX_HITS {
            return;
        }
        if line.to_lowercase().contains(lowercase_needle) {
            let trimmed = line.trim_start().trim_end();
            let cut: String = if trimmed.len() > MAX_LINE_LEN {
                let mut s: String = trimmed.chars().take(MAX_LINE_LEN).collect();
                s.push('…');
                s
            } else {
                trimmed.to_string()
            };
            out.push(SearchHit {
                path: path.to_path_buf(),
                line_no: idx + 1,
                line_text: cut,
            });
        }
    }
}

/// Side-panel widget shown when the active sidebar view is "Search".
pub struct SearchPanel {
    pub query: String,
    pub hits: Vec<SearchHit>,
    pub selected: usize,
    pub scroll: usize,
    pub focused: bool,
    pub last_inner: Rect,
    pub last_area: Rect,
    pub root: PathBuf,
}

impl SearchPanel {
    pub fn new(root: PathBuf) -> Self {
        Self {
            query: String::new(),
            hits: Vec::new(),
            selected: 0,
            scroll: 0,
            focused: false,
            last_inner: Rect::default(),
            last_area: Rect::default(),
            root,
        }
    }

    /// Run the current query, store the results, and reset selection.
    pub fn run_query(&mut self) {
        self.hits = search_workspace(&self.root, &self.query);
        self.selected = 0;
        self.scroll = 0;
    }

    pub fn move_up(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
        }
    }

    pub fn move_down(&mut self) {
        if self.selected + 1 < self.hits.len() {
            self.selected += 1;
        }
    }

    pub fn selected_hit(&self) -> Option<&SearchHit> {
        self.hits.get(self.selected)
    }

    /// Map a click row to a hit index, if any.
    pub fn hit_at_y(&self, y: u16) -> Option<usize> {
        let inner = self.last_inner;
        // First inner row is the input box, second row is a separator/blank,
        // hits start at row 2.
        if y < inner.y + 2 || y >= inner.y + inner.height {
            return None;
        }
        let row_in_results = (y - (inner.y + 2)) as usize;
        let idx = self.scroll + row_in_results;
        if idx < self.hits.len() {
            Some(idx)
        } else {
            None
        }
    }
}

impl Widget for &mut SearchPanel {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let block_style = if self.focused {
            Style::default().fg(Color::Rgb(0x4e, 0x9a, 0xff))
        } else {
            Style::default().fg(Color::DarkGray)
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(block_style)
            .title(Span::styled(
                " SEARCH ",
                Style::default()
                    .fg(Color::White)
                    .bg(Color::Rgb(0x1e, 0x3a, 0x6e))
                    .add_modifier(Modifier::BOLD),
            ));
        let inner = block.inner(area);
        block.render(area, buf);
        self.last_area = area;
        self.last_inner = inner;
        if inner.height == 0 {
            return;
        }

        // Input row: chevron prefix, query (or italic placeholder when empty),
        // a software-cursor block when focused, and a right-aligned cluster
        // of `Aa ab .*` mode glyphs (cosmetic for now; actual case / whole-
        // word / regex toggles arrive in a follow-up).
        let chevron_color = if self.focused {
            Color::Rgb(0x4e, 0x9a, 0xff)
        } else {
            Color::DarkGray
        };
        let toggles = "Aa ab .*";
        let toggles_w = toggles.chars().count() as u16;
        let toggles_x = inner
            .x
            .saturating_add(inner.width.saturating_sub(toggles_w));
        let chevron = Span::styled(
            "› ",
            Style::default().fg(chevron_color).add_modifier(Modifier::BOLD),
        );
        let mut spans: Vec<Span> = vec![chevron];
        if self.query.is_empty() {
            spans.push(Span::styled(
                "Search",
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::ITALIC),
            ));
        } else {
            spans.push(Span::styled(
                self.query.as_str(),
                Style::default().fg(Color::White),
            ));
        }
        if self.focused {
            spans.push(Span::styled(
                "█",
                Style::default().fg(Color::Rgb(0x4e, 0x9a, 0xff)),
            ));
        }
        let typed_w = inner.width.saturating_sub(toggles_w + 1);
        buf.set_line(inner.x, inner.y, &Line::from(spans), typed_w);
        buf.set_line(
            toggles_x,
            inner.y,
            &Line::from(Span::styled(
                toggles,
                Style::default().fg(Color::Rgb(0x6c, 0x7d, 0x9c)),
            )),
            toggles_w,
        );

        // Status row: live match count. Left blank while waiting for the
        // first result of a non-empty query so the panel doesn't flash
        // "0 matches" between keystrokes.
        if inner.height >= 2 {
            let header = if self.query.trim().is_empty() {
                String::new()
            } else {
                let count = self.hits.len();
                format!("{count} match{}", if count == 1 { "" } else { "es" })
            };
            let line = Line::from(Span::styled(
                header,
                Style::default().fg(Color::DarkGray).add_modifier(Modifier::ITALIC),
            ));
            buf.set_line(inner.x, inner.y + 1, &line, inner.width);
        }

        // Results
        if inner.height < 3 {
            return;
        }
        let visible = (inner.height as usize).saturating_sub(2);
        if self.selected >= self.scroll + visible {
            self.scroll = self.selected + 1 - visible;
        }
        if self.selected < self.scroll {
            self.scroll = self.selected;
        }
        let end = (self.scroll + visible).min(self.hits.len());
        for (row_idx, hit_idx) in (self.scroll..end).enumerate() {
            let y = inner.y + 2 + row_idx as u16;
            let hit = &self.hits[hit_idx];
            // Show just the basename so the matched line itself has room
            // to render inside a narrow side panel — without this, deep
            // monorepo paths consumed the whole row and the highlight fell
            // off the right edge.
            let path_display = hit
                .path
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| {
                    hit.path
                        .strip_prefix(&self.root)
                        .unwrap_or(hit.path.as_path())
                        .display()
                        .to_string()
                });
            let header_style = if hit_idx == self.selected {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Rgb(0x4e, 0x9a, 0xff))
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };
            let needle = self.query.trim();
            // High-contrast yellow background like ripgrep / VS Code's
            // editor.findMatchHighlightBackground. Different treatment when
            // the row is selected so the highlight stays readable against
            // the blue row bg instead of fighting it.
            let highlight_style = Style::default()
                .fg(Color::Black)
                .bg(Color::Rgb(0xff, 0xd7, 0x4a))
                .add_modifier(Modifier::BOLD);
            let plain_style = Style::default().fg(Color::Gray);
            let mut spans: Vec<Span> = vec![
                Span::styled(format!(" {path_display}"), header_style),
                Span::styled(
                    format!(":{}: ", hit.line_no),
                    Style::default().fg(Color::Rgb(0xeb, 0xcb, 0x8b)),
                ),
            ];
            for (chunk, is_match) in split_for_highlight(&hit.line_text, needle) {
                spans.push(Span::styled(
                    chunk,
                    if is_match { highlight_style } else { plain_style },
                ));
            }
            let line = Line::from(spans);
            buf.set_line(inner.x, y, &line, inner.width);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn write(p: &Path, s: &str) {
        fs::write(p, s).unwrap();
    }

    #[test]
    fn collect_matches_substring_case_insensitive() {
        let mut out = Vec::new();
        let content = "Hello World\nfoo bar\nHELLO again\nno match here";
        collect_matches_in_text(Path::new("a.txt"), content, "hello", &mut out);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].line_no, 1);
        assert_eq!(out[0].line_text, "Hello World");
        assert_eq!(out[1].line_no, 3);
        assert_eq!(out[1].line_text, "HELLO again");
    }

    #[test]
    fn collect_matches_truncates_very_long_lines() {
        let mut out = Vec::new();
        let long_line = "x".repeat(MAX_LINE_LEN + 50) + "needle";
        collect_matches_in_text(Path::new("a.txt"), &long_line, "needle", &mut out);
        assert_eq!(out.len(), 1);
        assert!(out[0].line_text.ends_with('…'));
        assert!(out[0].line_text.chars().count() <= MAX_LINE_LEN + 1);
    }

    #[test]
    fn search_workspace_finds_matches_across_files() {
        let tmp = TempDir::new().unwrap();
        write(&tmp.path().join("a.txt"), "alpha\nbeta\nbananas\n");
        write(&tmp.path().join("b.rs"), "fn beta() {}\nlet bananas = 1;\n");
        let hits = search_workspace(tmp.path(), "bananas");
        assert_eq!(hits.len(), 2);
        let names: Vec<String> = hits
            .iter()
            .map(|h| h.path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert!(names.contains(&"a.txt".to_string()));
        assert!(names.contains(&"b.rs".to_string()));
    }

    #[test]
    fn search_workspace_returns_empty_for_blank_query() {
        let tmp = TempDir::new().unwrap();
        write(&tmp.path().join("a.txt"), "anything");
        assert!(search_workspace(tmp.path(), "").is_empty());
        assert!(search_workspace(tmp.path(), "   ").is_empty());
    }

    #[test]
    fn search_workspace_skips_binary_or_unreadable_files() {
        let tmp = TempDir::new().unwrap();
        write(&tmp.path().join("text.txt"), "hello world");
        // A non-utf8 file: read_to_string returns Err, we silently skip.
        std::fs::write(tmp.path().join("blob.bin"), [0u8, 159, 146, 150]).unwrap();
        let hits = search_workspace(tmp.path(), "hello");
        assert_eq!(hits.len(), 1);
        assert!(hits[0].path.ends_with("text.txt"));
    }

    #[test]
    fn search_workspace_caps_at_max_hits() {
        let tmp = TempDir::new().unwrap();
        // 5 files, each with 100 matches → 500 lines. Cap is 200.
        for i in 0..5 {
            let many = (0..100).map(|_| "needle\n").collect::<String>();
            write(&tmp.path().join(format!("f{i}.txt")), &many);
        }
        let hits = search_workspace(tmp.path(), "needle");
        assert_eq!(hits.len(), MAX_HITS);
    }

    #[test]
    fn search_panel_run_query_populates_hits_and_resets_selection() {
        let tmp = TempDir::new().unwrap();
        write(&tmp.path().join("x.txt"), "one\ntwo\nthree\n");
        let mut panel = SearchPanel::new(tmp.path().to_path_buf());
        panel.query = "two".into();
        panel.run_query();
        assert_eq!(panel.hits.len(), 1);
        assert_eq!(panel.selected, 0);
    }

    #[test]
    fn split_highlight_returns_whole_line_when_needle_empty() {
        let segs = split_for_highlight("hello world", "");
        assert_eq!(segs, vec![(String::from("hello world"), false)]);
    }

    #[test]
    fn split_highlight_returns_whole_line_when_no_match() {
        let segs = split_for_highlight("hello world", "xyz");
        assert_eq!(segs, vec![(String::from("hello world"), false)]);
    }

    #[test]
    fn split_highlight_marks_each_match_run_case_insensitive() {
        let segs = split_for_highlight("The Quick brown fox jumps over the QUICK fence", "quick");
        // Two runs of "quick" (mixed case), three non-match tails.
        let matches: Vec<&String> =
            segs.iter().filter_map(|(s, m)| if *m { Some(s) } else { None }).collect();
        assert_eq!(matches.len(), 2);
        // Original-case slice must be preserved in the matched segment.
        assert_eq!(matches[0], "Quick");
        assert_eq!(matches[1], "QUICK");
        // Concatenating all segments must reproduce the original line.
        let joined: String = segs.iter().map(|(s, _)| s.as_str()).collect();
        assert_eq!(joined, "The Quick brown fox jumps over the QUICK fence");
    }

    #[test]
    fn split_highlight_handles_match_at_start_and_end() {
        let segs = split_for_highlight("foo bar foo", "foo");
        let joined: String = segs.iter().map(|(s, _)| s.as_str()).collect();
        assert_eq!(joined, "foo bar foo");
        assert_eq!(segs.first().map(|(_, m)| *m), Some(true), "first seg is match");
        assert_eq!(segs.last().map(|(_, m)| *m), Some(true), "last seg is match");
    }

    #[test]
    fn search_worker_loop_returns_hits_for_a_typed_query() {
        let tmp = TempDir::new().unwrap();
        write(&tmp.path().join("a.txt"), "one\nneedle\nthree\n");
        let (q_tx, q_rx) = std::sync::mpsc::channel::<String>();
        let (r_tx, r_rx) = std::sync::mpsc::channel::<SearchResult>();
        let root = tmp.path().to_path_buf();
        let join = std::thread::spawn(move || search_worker_loop(root, q_rx, r_tx));
        q_tx.send("needle".into()).unwrap();
        let (q, hits) = r_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("worker should ship a result");
        assert_eq!(q, "needle");
        assert_eq!(hits.len(), 1);
        drop(q_tx);
        join.join().unwrap();
    }

    #[test]
    fn search_worker_loop_coalesces_a_burst_of_keystrokes() {
        // Simulate a fast typist sending o, on, one. Worker must coalesce
        // and only run the latest, returning hits for "one" — not for the
        // intermediate prefixes.
        let tmp = TempDir::new().unwrap();
        write(&tmp.path().join("a.txt"), "alpha one beta\nzeta\n");
        let (q_tx, q_rx) = std::sync::mpsc::channel::<String>();
        let (r_tx, r_rx) = std::sync::mpsc::channel::<SearchResult>();
        let root = tmp.path().to_path_buf();
        let join = std::thread::spawn(move || search_worker_loop(root, q_rx, r_tx));
        q_tx.send("o".into()).unwrap();
        q_tx.send("on".into()).unwrap();
        q_tx.send("one".into()).unwrap();
        let mut last: Option<SearchResult> = None;
        while let Ok(r) = r_rx.recv_timeout(std::time::Duration::from_secs(2)) {
            last = Some(r);
            if r_rx
                .recv_timeout(std::time::Duration::from_millis(200))
                .is_err()
            {
                break;
            }
        }
        let (q, hits) = last.expect("worker must produce at least one result");
        assert_eq!(q, "one", "coalesce must drop intermediate prefixes");
        assert_eq!(hits.len(), 1);
        drop(q_tx);
        join.join().unwrap();
    }

    #[test]
    fn search_worker_loop_short_circuits_empty_queries() {
        let tmp = TempDir::new().unwrap();
        write(&tmp.path().join("a.txt"), "anything\n");
        let (q_tx, q_rx) = std::sync::mpsc::channel::<String>();
        let (r_tx, r_rx) = std::sync::mpsc::channel::<SearchResult>();
        let root = tmp.path().to_path_buf();
        let join = std::thread::spawn(move || search_worker_loop(root, q_rx, r_tx));
        q_tx.send("".into()).unwrap();
        let (q, hits) = r_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        assert_eq!(q, "");
        assert!(hits.is_empty());
        drop(q_tx);
        join.join().unwrap();
    }

    #[test]
    fn search_panel_navigation_clamps() {
        let tmp = TempDir::new().unwrap();
        write(
            &tmp.path().join("x.txt"),
            "a needle\nb needle\nc needle\n",
        );
        let mut panel = SearchPanel::new(tmp.path().to_path_buf());
        panel.query = "needle".into();
        panel.run_query();
        assert_eq!(panel.hits.len(), 3);
        panel.move_up();
        assert_eq!(panel.selected, 0);
        panel.move_down();
        panel.move_down();
        panel.move_down(); // would go to 3, but clamped to 2
        assert_eq!(panel.selected, 2);
    }
}
