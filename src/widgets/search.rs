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

        // Input row
        let cursor = if self.focused { "█" } else { " " };
        let input_line = Line::from(vec![
            Span::styled(
                "  ",
                Style::default().fg(Color::Rgb(0xdc, 0xb6, 0x7a)),
            ),
            Span::styled(self.query.as_str(), Style::default().fg(Color::White)),
            Span::styled(cursor, Style::default().fg(Color::Rgb(0x4e, 0x9a, 0xff))),
        ]);
        buf.set_line(inner.x, inner.y, &input_line, inner.width);

        // Results header (row 1)
        if inner.height >= 2 {
            let count = self.hits.len();
            let header = if self.query.trim().is_empty() {
                String::from("type and press Enter to search")
            } else {
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
            let path_display = hit
                .path
                .strip_prefix(&self.root)
                .unwrap_or(hit.path.as_path())
                .display()
                .to_string();
            let header_style = if hit_idx == self.selected {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Rgb(0x4e, 0x9a, 0xff))
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };
            let line = Line::from(vec![
                Span::styled(format!(" {path_display}"), header_style),
                Span::styled(
                    format!(":{}: ", hit.line_no),
                    Style::default().fg(Color::Rgb(0xeb, 0xcb, 0x8b)),
                ),
                Span::styled(
                    hit.line_text.as_str(),
                    Style::default().fg(Color::Gray),
                ),
            ]);
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
