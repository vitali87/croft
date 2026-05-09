use ignore::WalkBuilder;
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Widget},
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

/// Mode toggles that drive `search_workspace`. Mirror VS Code's three
/// search input toggles, in the same left-to-right order:
///   - `case_sensitive` (Aa)
///   - `whole_word` (ab with underline)
///   - `use_regex` (.*)
/// All-false matches the original case-insensitive substring behaviour.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct SearchOpts {
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub use_regex: bool,
}

/// Search the workspace rooted at `root` for `query` honouring `opts`
/// (case-sensitivity, whole-word boundaries, regex). Honours `.gitignore`
/// (via the `ignore` crate) so generated files don't dominate. Capped at
/// `MAX_HITS`.
pub fn search_workspace(root: &Path, query: &str, opts: SearchOpts) -> Vec<SearchHit> {
    let q = query.trim();
    if q.is_empty() {
        return Vec::new();
    }
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
            Ok(content) => collect_matches_in_text(path, &content, q, opts, &mut hits),
            Err(_) => {} // binary or unreadable; skip silently
        }
    }
    hits
}

/// Split `line` into `(segment, is_match)` runs against `needle` honouring
/// the supplied `opts`, so highlights in result rows / the editor stay
/// consistent with what `collect_matches_in_text` would actually match.
/// Concatenating the segments reproduces the original line byte-for-byte.
/// Empty needle, no match, or invalid regex all return the whole line as
/// a single non-match segment.
pub fn split_for_highlight(line: &str, needle: &str, opts: SearchOpts) -> Vec<(String, bool)> {
    if needle.is_empty() {
        return vec![(line.to_string(), false)];
    }
    if opts.use_regex {
        return split_for_highlight_regex(line, needle, opts);
    }
    let (haystack, search_for): (String, String) = if opts.case_sensitive {
        (line.to_string(), needle.to_string())
    } else {
        (line.to_lowercase(), needle.to_lowercase())
    };
    // If lowercasing changed the byte length (rare Unicode edge case) we
    // can't safely map `haystack` indices back into `line`, so bail out
    // without highlights.
    if haystack.len() != line.len() || search_for.is_empty() {
        return vec![(line.to_string(), false)];
    }
    let mut out: Vec<(String, bool)> = Vec::new();
    let mut last = 0usize;
    let mut start = 0usize;
    while let Some(rel) = haystack[start..].find(&search_for) {
        let abs = start + rel;
        let end = abs + search_for.len();
        if opts.whole_word && !is_whole_word_match(&haystack, abs, end) {
            start = end;
            continue;
        }
        if abs > last {
            out.push((line[last..abs].to_string(), false));
        }
        out.push((line[abs..end].to_string(), true));
        last = end;
        start = end;
    }
    if last < line.len() {
        out.push((line[last..].to_string(), false));
    }
    if out.is_empty() {
        out.push((line.to_string(), false));
    }
    out
}

fn split_for_highlight_regex(line: &str, needle: &str, opts: SearchOpts) -> Vec<(String, bool)> {
    let mut pattern = String::new();
    if !opts.case_sensitive {
        pattern.push_str("(?i)");
    }
    if opts.whole_word {
        pattern.push_str("\\b(?:");
        pattern.push_str(needle);
        pattern.push_str(")\\b");
    } else {
        pattern.push_str(needle);
    }
    let re = match regex::Regex::new(&pattern) {
        Ok(r) => r,
        Err(_) => return vec![(line.to_string(), false)],
    };
    let mut out: Vec<(String, bool)> = Vec::new();
    let mut last = 0usize;
    for m in re.find_iter(line) {
        if m.start() > last {
            out.push((line[last..m.start()].to_string(), false));
        }
        // Empty matches (`.*` against an empty position) would loop
        // forever; advance past them.
        if m.end() == m.start() {
            continue;
        }
        out.push((line[m.start()..m.end()].to_string(), true));
        last = m.end();
    }
    if last < line.len() {
        out.push((line[last..].to_string(), false));
    }
    if out.is_empty() {
        out.push((line.to_string(), false));
    }
    out
}

/// `(query_that_was_run, opts_used, hits)`. The query and opts are echoed
/// back so the receiver can drop stale results when the user has typed
/// past or flipped a toggle since the search started.
pub type SearchResult = (String, SearchOpts, Vec<SearchHit>);

/// Submitted unit of work: the query string and the toggle state at the
/// moment of submission. Bundling them lets the user flip a toggle and
/// re-fire the same query string with new opts.
pub type SearchRequest = (String, SearchOpts);

/// Background worker loop. Reads requests from `rx`, coalesces by always
/// taking the most recent pending request, debounces ~120 ms so a fast
/// typist doesn't trigger a search per keystroke, runs `search_workspace`,
/// and ships `(query, opts, hits)` back via `tx`. Empty queries
/// short-circuit to empty hits without walking the tree. The thread
/// exits cleanly when the channel closes.
pub fn search_worker_loop(
    root: PathBuf,
    rx: std::sync::mpsc::Receiver<SearchRequest>,
    tx: std::sync::mpsc::Sender<SearchResult>,
) {
    use std::time::Duration;
    while let Ok(mut req) = rx.recv() {
        while let Ok(newer) = rx.try_recv() {
            req = newer;
        }
        std::thread::sleep(Duration::from_millis(120));
        while let Ok(newer) = rx.try_recv() {
            req = newer;
        }
        let (query, opts) = req;
        let hits = if query.trim().is_empty() {
            Vec::new()
        } else {
            search_workspace(&root, &query, opts)
        };
        if tx.send((query, opts, hits)).is_err() {
            return;
        }
    }
}

fn is_searchable_size(path: &Path) -> bool {
    std::fs::metadata(path)
        .map(|m| m.len() <= MAX_FILE_BYTES)
        .unwrap_or(false)
}

/// Pure helper used by `search_workspace` and unit-tested directly. Honours
/// the three `SearchOpts` toggles. Regex compilation failure for invalid
/// patterns is silent: returns no matches rather than crashing.
pub fn collect_matches_in_text(
    path: &Path,
    content: &str,
    query: &str,
    opts: SearchOpts,
    out: &mut Vec<SearchHit>,
) {
    let regex = if opts.use_regex {
        let mut pattern = String::new();
        if !opts.case_sensitive {
            pattern.push_str("(?i)");
        }
        if opts.whole_word {
            pattern.push_str("\\b(?:");
            pattern.push_str(query);
            pattern.push_str(")\\b");
        } else {
            pattern.push_str(query);
        }
        match regex::Regex::new(&pattern) {
            Ok(r) => Some(r),
            Err(_) => return, // invalid regex: silently yield zero hits
        }
    } else {
        None
    };
    let lowered_needle = if opts.case_sensitive {
        query.to_string()
    } else {
        query.to_lowercase()
    };
    for (idx, line) in content.lines().enumerate() {
        if out.len() >= MAX_HITS {
            return;
        }
        let matched = if let Some(r) = regex.as_ref() {
            r.is_match(line)
        } else {
            line_contains_needle(line, query, &lowered_needle, opts)
        };
        if matched {
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

/// Literal-mode line match: case-sensitive direct contains when the flag
/// is on, lowercase-folded otherwise. Whole-word check inspects the
/// characters bounding each match position so partial-word hits get
/// filtered out.
fn line_contains_needle(line: &str, query: &str, lowered_needle: &str, opts: SearchOpts) -> bool {
    let (haystack, needle): (String, &str) = if opts.case_sensitive {
        (line.to_string(), query)
    } else {
        (line.to_lowercase(), &lowered_needle[..])
    };
    if needle.is_empty() {
        return false;
    }
    let mut start = 0;
    while let Some(rel) = haystack[start..].find(needle) {
        let abs = start + rel;
        let end = abs + needle.len();
        if !opts.whole_word || is_whole_word_match(&haystack, abs, end) {
            return true;
        }
        start = end;
    }
    false
}

fn is_whole_word_match(haystack: &str, start: usize, end: usize) -> bool {
    let prev_ok = start == 0
        || haystack[..start]
            .chars()
            .last()
            .map(|c| !is_word_char(c))
            .unwrap_or(true);
    let next_ok = end >= haystack.len()
        || haystack[end..]
            .chars()
            .next()
            .map(|c| !is_word_char(c))
            .unwrap_or(true);
    prev_ok && next_ok
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
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
    pub opts: SearchOpts,
    /// Per-toggle absolute screen column captured by the most recent render.
    /// Used by `App::handle_mouse` to map clicks on `Aa`, `ab`, `.*` into
    /// flag flips. Zero means the row was too narrow to render that toggle.
    pub toggle_case_x: u16,
    pub toggle_word_x: u16,
    pub toggle_regex_x: u16,
    pub toggle_y: u16,
    pub paste_button_x: u16,
    pub paste_button_y: u16,
    pub paste_button_w: u16,
    pub selection: Option<(usize, usize)>,
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
            opts: SearchOpts::default(),
            toggle_case_x: 0,
            toggle_word_x: 0,
            toggle_regex_x: 0,
            toggle_y: 0,
            paste_button_x: 0,
            paste_button_y: 0,
            paste_button_w: 0,
            selection: None,
        }
    }

    pub fn paste_button_at(&self, col: u16, row: u16) -> bool {
        if self.paste_button_w == 0 || row != self.paste_button_y {
            return false;
        }
        col >= self.paste_button_x && col < self.paste_button_x + self.paste_button_w
    }

    pub fn selection_range(&self) -> Option<(usize, usize)> {
        self.selection
            .map(|(a, b)| if a <= b { (a, b) } else { (b, a) })
    }

    pub fn select_all_query(&mut self) {
        if self.query.is_empty() {
            self.selection = None;
        } else {
            self.selection = Some((0, self.query.len()));
        }
    }

    pub fn clear_selection(&mut self) {
        self.selection = None;
    }

    pub fn delete_selection(&mut self) -> bool {
        let Some((a, b)) = self.selection_range() else { return false };
        if a == b {
            self.selection = None;
            return false;
        }
        self.query.replace_range(a..b, "");
        self.selection = None;
        true
    }

    pub fn selection_text(&self) -> String {
        match self.selection_range() {
            Some((a, b)) if a < b => self.query[a..b].to_string(),
            _ => String::new(),
        }
    }

    /// Insert `s` at the end of the query, replacing the current selection
    /// (if any) first. Newlines and carriage returns are stripped — search
    /// queries are single-line.
    pub fn insert_str_into_query(&mut self, s: &str) {
        self.delete_selection();
        for c in s.chars() {
            if c != '\n' && c != '\r' {
                self.query.push(c);
            }
        }
    }

    /// Run the current query, store the results, and reset selection.
    pub fn run_query(&mut self) {
        self.hits = search_workspace(&self.root, &self.query, self.opts);
        self.selected = 0;
        self.scroll = 0;
    }

    /// If the cell `(col, row)` falls on one of the three toggle glyphs
    /// (each rendered as a 2-cell pair), return a mutable pointer to the
    /// corresponding flag so the caller can flip it. Returns `None`
    /// otherwise.
    pub fn toggle_at(&self, col: u16, row: u16) -> Option<SearchToggle> {
        if row != self.toggle_y {
            return None;
        }
        for (start, kind) in [
            (self.toggle_case_x, SearchToggle::CaseSensitive),
            (self.toggle_word_x, SearchToggle::WholeWord),
            (self.toggle_regex_x, SearchToggle::UseRegex),
        ] {
            if start != 0 && col >= start && col < start + 2 {
                return Some(kind);
            }
        }
        None
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

    /// Map a click row to a hit index, if any. Hits sit below the cluster:
    /// row 0 = "SEARCH" header, rows 2..=4 = bordered input box, row 5 =
    /// thin separator, row 6 = match-count caption, rows 7+ = results.
    pub fn hit_at_y(&self, y: u16) -> Option<usize> {
        let inner = self.last_inner;
        let results_start = inner.y + 7;
        if y < results_start || y >= inner.y + inner.height {
            return None;
        }
        let row_in_results = (y - results_start) as usize;
        let idx = self.scroll + row_in_results;
        if idx < self.hits.len() {
            Some(idx)
        } else {
            None
        }
    }
}

/// Identifies which of the three search-mode toggles a click landed on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchToggle {
    CaseSensitive,
    WholeWord,
    UseRegex,
}

impl Widget for &mut SearchPanel {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let focus_blue = Color::Rgb(0x4e, 0x9a, 0xff);
        let outer_style = if self.focused {
            Style::default().fg(focus_blue)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        let outer = Block::default().borders(Borders::ALL).border_style(outer_style);
        let inner = outer.inner(area);
        outer.render(area, buf);
        self.last_area = area;
        self.last_inner = inner;
        if inner.height == 0 || inner.width == 0 {
            return;
        }

        // Row 0: "SEARCH" header, top-left, light grey. Replaces the old
        // dark-blue title chip on the outer border, matching the VS Code
        // mockup the user supplied.
        buf.set_string(
            inner.x,
            inner.y,
            "SEARCH",
            Style::default()
                .fg(Color::Rgb(0xb0, 0xb8, 0xc8))
                .add_modifier(Modifier::BOLD),
        );

        if inner.height < 4 {
            return;
        }

        // Geometry for the input cluster sitting at rows 2..=4 of the
        // inner area: a chevron `▾` on the left, a 3-row input box in the
        // middle (thin rounded border, focus-aware), and `Aa │ ab │ .*`
        // toggles to the right of the input. Cursor and content sit on
        // the box's middle row.
        let toggles_inner_w: u16 = 2 + 3 + 2 + 3 + 2; // "Aa" + " │ " + "ab" + " │ " + ".*"
        let chevron_w: u16 = 2; // "▾ "
        // Matching breathing room on both sides of the toggle cluster: one
        // cell between the input box's right border and `Aa`, and the same
        // one cell between `.*` and the panel's outer right border. Without
        // this margin the asterisk reads as crowded into the corner.
        const TOGGLE_GAP: u16 = 1;
        let input_top_y = inner.y + 2;
        let input_bot_y = input_top_y + 2;
        let content_y = input_top_y + 1;
        let chevron_x = inner.x;
        let input_x = chevron_x + chevron_w;
        let toggles_x = inner.x.saturating_add(
            inner
                .width
                .saturating_sub(toggles_inner_w)
                .saturating_sub(TOGGLE_GAP),
        );
        // Input box width is whatever sits between the chevron column and
        // the start of the toggles cluster, with the same TOGGLE_GAP cell
        // separating the box border from `Aa`.
        let input_w = toggles_x
            .saturating_sub(input_x)
            .saturating_sub(TOGGLE_GAP)
            .max(8);
        let input_box = Rect {
            x: input_x,
            y: input_top_y,
            width: input_w,
            height: 3,
        };

        // Chevron, vertically aligned with the input content row.
        let chevron_color = if self.focused { focus_blue } else { Color::DarkGray };
        buf.set_string(
            chevron_x,
            content_y,
            "▾",
            Style::default().fg(chevron_color).add_modifier(Modifier::BOLD),
        );

        // Input box border (rounded so it reads softer than the outer
        // panel border). Style switches to focus blue when the panel is
        // focused; otherwise dim grey, matching the rest of the panel.
        let input_border_style = if self.focused {
            Style::default().fg(focus_blue)
        } else {
            Style::default().fg(Color::Rgb(0x60, 0x68, 0x78))
        };
        let input_block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(input_border_style);
        let input_inner = input_block.inner(input_box);
        input_block.render(input_box, buf);

        // Magnifier glyph on the left of the input content, matching the
        // codicon `search` U+EA6D used in the activity bar.
        let magnifier_glyph = "\u{ea6d}";
        let magnifier_color = if self.focused { focus_blue } else { Color::Rgb(0x9d, 0xa5, 0xb4) };
        let magnifier_w: u16 = 2; // glyph + 1-cell gap
        if input_inner.width > magnifier_w {
            buf.set_string(
                input_inner.x,
                input_inner.y,
                magnifier_glyph,
                Style::default().fg(magnifier_color),
            );
        }
        let typed_x = input_inner.x + magnifier_w;
        let typed_w = input_inner.width.saturating_sub(magnifier_w);

        // Query / placeholder / cursor inside the input box, on its
        // single content row.
        let cursor_span = Span::styled("█", Style::default().fg(focus_blue));
        let placeholder_span = Span::styled(
            "Search",
            Style::default()
                .fg(Color::Rgb(0x6c, 0x7d, 0x9c))
                .add_modifier(Modifier::ITALIC),
        );
        let mut spans: Vec<Span> = Vec::with_capacity(4);
        if self.query.is_empty() {
            if self.focused {
                spans.push(cursor_span);
            }
            spans.push(placeholder_span);
        } else {
            let plain = Style::default().fg(Color::White);
            let selected = Style::default()
                .fg(Color::White)
                .bg(Color::Rgb(0x26, 0x4f, 0x78));
            match self.selection_range() {
                Some((a, b)) if a < b => {
                    if a > 0 {
                        spans.push(Span::styled(self.query[..a].to_string(), plain));
                    }
                    spans.push(Span::styled(self.query[a..b].to_string(), selected));
                    if b < self.query.len() {
                        spans.push(Span::styled(self.query[b..].to_string(), plain));
                    }
                }
                _ => {
                    spans.push(Span::styled(self.query.as_str(), plain));
                }
            }
            if self.focused {
                spans.push(cursor_span);
            }
        }
        buf.set_line(typed_x, input_inner.y, &Line::from(spans), typed_w);

        // Toggles cluster: `Aa │ ab │ .*` aligned with the input content
        // row, with vertical-bar separators between the three glyphs.
        self.paste_button_x = 0;
        self.paste_button_y = content_y;
        self.paste_button_w = 0;

        let active_style = Style::default()
            .fg(Color::Black)
            .bg(Color::Rgb(0xff, 0xd7, 0x4a))
            .add_modifier(Modifier::BOLD);
        let inactive_style = Style::default().fg(Color::Rgb(0x9d, 0xa5, 0xb4));
        let pipe_style = Style::default().fg(Color::Rgb(0x60, 0x68, 0x78));
        let case_x = toggles_x;
        let word_x = case_x + 2 + 3;
        let regex_x = word_x + 2 + 3;
        self.toggle_y = content_y;
        self.toggle_case_x = case_x;
        self.toggle_word_x = word_x;
        self.toggle_regex_x = regex_x;
        for (x, glyph, active) in [
            (case_x, "Aa", self.opts.case_sensitive),
            (word_x, "ab", self.opts.whole_word),
            (regex_x, ".*", self.opts.use_regex),
        ] {
            let style = if active { active_style } else { inactive_style };
            buf.set_string(x, content_y, glyph, style);
        }
        buf.set_string(case_x + 2 + 1, content_y, "│", pipe_style);
        buf.set_string(word_x + 2 + 1, content_y, "│", pipe_style);

        // Thin separator below the input box: light grey horizontal rule
        // running the full inner width.
        let separator_y = input_bot_y + 1;
        if separator_y < inner.y + inner.height {
            let sep_style = Style::default().fg(Color::Rgb(0x40, 0x48, 0x58));
            for x in inner.x..inner.x + inner.width {
                buf.set_string(x, separator_y, "─", sep_style);
            }
        }

        // Match-count caption: live "N matches" line just below the
        // separator. Left blank while waiting for the first result so the
        // panel doesn't flash "0 matches" between keystrokes.
        let caption_y = separator_y + 1;
        let results_start_y = caption_y + 1;
        if caption_y < inner.y + inner.height && !self.query.trim().is_empty() {
            let count = self.hits.len();
            let header = format!("{count} match{}", if count == 1 { "" } else { "es" });
            let caption = Line::from(Span::styled(
                header,
                Style::default()
                    .fg(Color::Rgb(0x9d, 0xa5, 0xb4))
                    .add_modifier(Modifier::ITALIC),
            ));
            buf.set_line(inner.x, caption_y, &caption, inner.width);
        }

        // Results
        if results_start_y >= inner.y + inner.height {
            return;
        }
        let visible = (inner.y + inner.height - results_start_y) as usize;
        if visible == 0 {
            return;
        }
        if self.selected >= self.scroll + visible {
            self.scroll = self.selected + 1 - visible;
        }
        if self.selected < self.scroll {
            self.scroll = self.selected;
        }
        let end = (self.scroll + visible).min(self.hits.len());
        for (row_idx, hit_idx) in (self.scroll..end).enumerate() {
            let y = results_start_y + row_idx as u16;
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
            for (chunk, is_match) in split_for_highlight(&hit.line_text, needle, self.opts) {
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
        collect_matches_in_text(Path::new("a.txt"), content, "hello", SearchOpts::default(), &mut out);
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
        collect_matches_in_text(Path::new("a.txt"), &long_line, "needle", SearchOpts::default(), &mut out);
        assert_eq!(out.len(), 1);
        assert!(out[0].line_text.ends_with('…'));
        assert!(out[0].line_text.chars().count() <= MAX_LINE_LEN + 1);
    }

    #[test]
    fn search_workspace_finds_matches_across_files() {
        let tmp = TempDir::new().unwrap();
        write(&tmp.path().join("a.txt"), "alpha\nbeta\nbananas\n");
        write(&tmp.path().join("b.rs"), "fn beta() {}\nlet bananas = 1;\n");
        let hits = search_workspace(tmp.path(), "bananas", SearchOpts::default());
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
        assert!(search_workspace(tmp.path(), "", SearchOpts::default()).is_empty());
        assert!(search_workspace(tmp.path(), "   ", SearchOpts::default()).is_empty());
    }

    #[test]
    fn search_workspace_skips_binary_or_unreadable_files() {
        let tmp = TempDir::new().unwrap();
        write(&tmp.path().join("text.txt"), "hello world");
        // A non-utf8 file: read_to_string returns Err, we silently skip.
        std::fs::write(tmp.path().join("blob.bin"), [0u8, 159, 146, 150]).unwrap();
        let hits = search_workspace(tmp.path(), "hello", SearchOpts::default());
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
        let hits = search_workspace(tmp.path(), "needle", SearchOpts::default());
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

    /// New layout x-offset for the typed-content cell, relative to
    /// `inner.x`: 2-cell chevron column + 1 input-box left border +
    /// 2-cell magnifier glyph (codicon + 1 cell gap) = 5.
    const INPUT_TYPED_COL: u16 = 5;
    /// New layout y-offset for the input content row, relative to
    /// `inner.y`: header + blank + top border + content = 3.
    const INPUT_CONTENT_ROW: u16 = 3;

    #[test]
    fn cursor_sits_at_input_start_when_query_empty_and_focused() {
        use ratatui::buffer::Buffer;
        let tmp = TempDir::new().unwrap();
        let mut panel = SearchPanel::new(tmp.path().to_path_buf());
        panel.focused = true;
        let area = Rect { x: 0, y: 0, width: 60, height: 12 };
        let mut buf = Buffer::empty(area);
        ratatui::widgets::Widget::render(&mut panel, area, &mut buf);
        let inner_x = panel.last_inner.x;
        let inner_y = panel.last_inner.y;
        assert_eq!(
            buf[(inner_x + INPUT_TYPED_COL, inner_y + INPUT_CONTENT_ROW)].symbol(),
            "█",
            "cursor must sit at the start of the input content area when the query is empty"
        );
    }

    #[test]
    fn cursor_sits_after_typed_text_when_query_non_empty_and_focused() {
        use ratatui::buffer::Buffer;
        let tmp = TempDir::new().unwrap();
        let mut panel = SearchPanel::new(tmp.path().to_path_buf());
        panel.focused = true;
        panel.query = String::from("foo");
        let area = Rect { x: 0, y: 0, width: 60, height: 12 };
        let mut buf = Buffer::empty(area);
        ratatui::widgets::Widget::render(&mut panel, area, &mut buf);
        let inner_x = panel.last_inner.x;
        let inner_y = panel.last_inner.y;
        // INPUT_TYPED_COL + "foo" (3 cells) → cursor sits 3 cells past
        // the start of typed content.
        assert_eq!(
            buf[(inner_x + INPUT_TYPED_COL + 3, inner_y + INPUT_CONTENT_ROW)].symbol(),
            "█",
            "cursor must sit immediately after the typed query"
        );
    }

    fn buffer_to_string(buf: &Buffer) -> String {
        let mut out = String::new();
        for y in buf.area.y..buf.area.y + buf.area.height {
            for x in buf.area.x..buf.area.x + buf.area.width {
                out.push_str(buf[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn header_row_says_search_in_upper_case() {
        // Mockup-driven: an uppercase "SEARCH" header sits at the top-left
        // of the inner area, in light grey — replacing the old dark-blue
        // title bar that used to overlap the outer border.
        use ratatui::buffer::Buffer;
        let tmp = TempDir::new().unwrap();
        let mut panel = SearchPanel::new(tmp.path().to_path_buf());
        let area = Rect { x: 0, y: 0, width: 60, height: 10 };
        let mut buf = Buffer::empty(area);
        ratatui::widgets::Widget::render(&mut panel, area, &mut buf);
        let inner_x = panel.last_inner.x;
        let inner_y = panel.last_inner.y;
        let mut header = String::new();
        for x in inner_x..inner_x + 8 {
            header.push_str(buf[(x, inner_y)].symbol());
        }
        assert!(
            header.starts_with("SEARCH"),
            "first inner row must start with the uppercase SEARCH header; got: {header:?}"
        );
        // Outer-border title row must NOT carry a SEARCH chip — the mockup
        // has no title bar; the header lives inside the panel.
        let mut top_border = String::new();
        for x in area.x..area.x + area.width {
            top_border.push_str(buf[(x, area.y)].symbol());
        }
        assert!(
            !top_border.contains("SEARCH"),
            "outer border row must not carry a SEARCH title chip: {top_border:?}"
        );
    }

    #[test]
    fn input_box_renders_a_thin_focused_border_three_rows_tall() {
        use ratatui::buffer::Buffer;
        let tmp = TempDir::new().unwrap();
        let mut panel = SearchPanel::new(tmp.path().to_path_buf());
        panel.focused = true;
        let area = Rect { x: 0, y: 0, width: 60, height: 12 };
        let mut buf = Buffer::empty(area);
        ratatui::widgets::Widget::render(&mut panel, area, &mut buf);
        let dump = buffer_to_string(&buf);
        // The input box must render a single-line top-left corner glyph
        // somewhere in the upper half of the panel.
        assert!(
            dump.contains('┌') || dump.contains('╭'),
            "input box must have a top-left corner; got buffer:\n{dump}"
        );
        assert!(
            dump.contains('└') || dump.contains('╰'),
            "input box must have a bottom-left corner so the border reads as a 3-row box, not a single rule:\n{dump}"
        );
    }

    #[test]
    fn rendering_at_narrow_widths_does_not_panic() {
        use ratatui::buffer::Buffer;
        let tmp = TempDir::new().unwrap();
        for width in 0u16..40 {
            for height in 0u16..20 {
                let mut panel = SearchPanel::new(tmp.path().to_path_buf());
                let area = Rect { x: 0, y: 0, width, height };
                if area.width == 0 || area.height == 0 {
                    continue;
                }
                let mut buf = Buffer::empty(area);
                ratatui::widgets::Widget::render(&mut panel, area, &mut buf);
            }
        }
    }

    #[test]
    fn asterisk_keeps_breathing_room_against_the_outer_right_border() {
        // User report: the asterisk in `.*` was sitting flush against the
        // outer right border, looking crowded into the corner. The fix
        // mirrors the gap that already exists between the input box and
        // `Aa` so the toggle cluster reads as a contained group on both
        // sides.
        use ratatui::buffer::Buffer;
        let tmp = TempDir::new().unwrap();
        let mut panel = SearchPanel::new(tmp.path().to_path_buf());
        let area = Rect { x: 0, y: 0, width: 60, height: 12 };
        let mut buf = Buffer::empty(area);
        ratatui::widgets::Widget::render(&mut panel, area, &mut buf);
        let inner = panel.last_inner;
        // `.*` toggle is 2 cells wide starting at toggle_regex_x.
        let regex_end = panel.toggle_regex_x + 1;
        let inner_right = inner.x + inner.width - 1;
        let right_gap = inner_right.saturating_sub(regex_end);
        assert!(
            right_gap >= 1,
            "asterisk must not sit flush against the outer right border (right_gap={right_gap})"
        );
    }

    #[test]
    fn input_row_carries_a_chevron_and_pipe_separated_toggles() {
        use ratatui::buffer::Buffer;
        let tmp = TempDir::new().unwrap();
        let mut panel = SearchPanel::new(tmp.path().to_path_buf());
        let area = Rect { x: 0, y: 0, width: 60, height: 10 };
        let mut buf = Buffer::empty(area);
        ratatui::widgets::Widget::render(&mut panel, area, &mut buf);
        let dump = buffer_to_string(&buf);
        // The expand chevron is the only down-arrow on the panel; absence
        // of it means we regressed back to the right-pointing chevron.
        assert!(
            dump.contains('▾') || dump.contains('⌄'),
            "input row must have a down-chevron on the left, like VS Code's expand-to-replace toggle:\n{dump}"
        );
        // Toggles separated by pipe '│' so the trio reads "Aa │ ab │ .*".
        assert!(
            dump.contains("Aa") && dump.contains("ab") && dump.contains(".*"),
            "all three toggle glyphs must render:\n{dump}"
        );
        let pipes = dump.matches('│').count();
        assert!(
            pipes >= 2,
            "at least two pipe separators must sit between the three toggles; got {pipes} pipes in:\n{dump}"
        );
    }

    #[test]
    fn results_start_below_the_input_box_with_a_separator_in_between() {
        use ratatui::buffer::Buffer;
        let tmp = TempDir::new().unwrap();
        write(&tmp.path().join("a.txt"), "needle\n");
        let mut panel = SearchPanel::new(tmp.path().to_path_buf());
        panel.query = String::from("needle");
        panel.run_query();
        assert_eq!(panel.hits.len(), 1);
        let area = Rect { x: 0, y: 0, width: 60, height: 14 };
        let mut buf = Buffer::empty(area);
        ratatui::widgets::Widget::render(&mut panel, area, &mut buf);
        let dump = buffer_to_string(&buf);
        let row_with_needle = dump
            .lines()
            .position(|l| l.contains("needle") && l.contains("a.txt"))
            .expect("result row with needle must render");
        assert!(
            row_with_needle >= 5,
            "result row must sit below the input box (expected row >= 5, got {row_with_needle}):\n{dump}"
        );
    }

    #[test]
    fn search_panel_does_not_render_paste_button_in_input_row() {
        use ratatui::buffer::Buffer;
        let tmp = TempDir::new().unwrap();
        let mut panel = SearchPanel::new(tmp.path().to_path_buf());
        panel.focused = true;
        let area = Rect { x: 0, y: 0, width: 60, height: 5 };
        let mut buf = Buffer::empty(area);
        ratatui::widgets::Widget::render(&mut panel, area, &mut buf);
        assert_eq!(panel.paste_button_w, 0, "paste button should not reserve cells");
        let mut collected = String::new();
        for x in area.x..area.x + area.width {
            collected.push_str(buf[(x, 1)].symbol());
        }
        assert!(
            !collected.to_lowercase().contains("paste"),
            "input row should not visibly contain a Paste button, got: {collected:?}"
        );
    }

    #[test]
    fn paste_button_at_returns_false_when_button_hidden() {
        use ratatui::buffer::Buffer;
        let tmp = TempDir::new().unwrap();
        let mut panel = SearchPanel::new(tmp.path().to_path_buf());
        panel.focused = true;
        let area = Rect { x: 0, y: 0, width: 60, height: 5 };
        let mut buf = Buffer::empty(area);
        ratatui::widgets::Widget::render(&mut panel, area, &mut buf);
        assert_eq!(panel.paste_button_w, 0);
        assert!(!panel.paste_button_at(0, 1));
        assert!(!panel.paste_button_at(40, 1));
    }

    #[test]
    fn split_highlight_returns_whole_line_when_needle_empty() {
        let segs = split_for_highlight("hello world", "", SearchOpts::default());
        assert_eq!(segs, vec![(String::from("hello world"), false)]);
    }

    #[test]
    fn split_highlight_returns_whole_line_when_no_match() {
        let segs = split_for_highlight("hello world", "xyz", SearchOpts::default());
        assert_eq!(segs, vec![(String::from("hello world"), false)]);
    }

    #[test]
    fn split_highlight_marks_each_match_run_case_insensitive() {
        let segs = split_for_highlight("The Quick brown fox jumps over the QUICK fence", "quick", SearchOpts::default());
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
        let segs = split_for_highlight("foo bar foo", "foo", SearchOpts::default());
        let joined: String = segs.iter().map(|(s, _)| s.as_str()).collect();
        assert_eq!(joined, "foo bar foo");
        assert_eq!(segs.first().map(|(_, m)| *m), Some(true), "first seg is match");
        assert_eq!(segs.last().map(|(_, m)| *m), Some(true), "last seg is match");
    }

    #[test]
    fn search_worker_loop_returns_hits_for_a_typed_query() {
        let tmp = TempDir::new().unwrap();
        write(&tmp.path().join("a.txt"), "one\nneedle\nthree\n");
        let (q_tx, q_rx) = std::sync::mpsc::channel::<SearchRequest>();
        let (r_tx, r_rx) = std::sync::mpsc::channel::<SearchResult>();
        let root = tmp.path().to_path_buf();
        let join = std::thread::spawn(move || search_worker_loop(root, q_rx, r_tx));
        q_tx.send(("needle".into(), SearchOpts::default())).unwrap();
        let (q, _opts, hits) = r_rx
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
        let (q_tx, q_rx) = std::sync::mpsc::channel::<SearchRequest>();
        let (r_tx, r_rx) = std::sync::mpsc::channel::<SearchResult>();
        let root = tmp.path().to_path_buf();
        let join = std::thread::spawn(move || search_worker_loop(root, q_rx, r_tx));
        q_tx.send(("o".into(), SearchOpts::default())).unwrap();
        q_tx.send(("on".into(), SearchOpts::default())).unwrap();
        q_tx.send(("one".into(), SearchOpts::default())).unwrap();
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
        let (q, _opts, hits) = last.expect("worker must produce at least one result");
        assert_eq!(q, "one", "coalesce must drop intermediate prefixes");
        assert_eq!(hits.len(), 1);
        drop(q_tx);
        join.join().unwrap();
    }

    #[test]
    fn search_worker_loop_short_circuits_empty_queries() {
        let tmp = TempDir::new().unwrap();
        write(&tmp.path().join("a.txt"), "anything\n");
        let (q_tx, q_rx) = std::sync::mpsc::channel::<SearchRequest>();
        let (r_tx, r_rx) = std::sync::mpsc::channel::<SearchResult>();
        let root = tmp.path().to_path_buf();
        let join = std::thread::spawn(move || search_worker_loop(root, q_rx, r_tx));
        q_tx.send(("".into(), SearchOpts::default())).unwrap();
        let (q, _opts, hits) = r_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        assert_eq!(q, "");
        assert!(hits.is_empty());
        drop(q_tx);
        join.join().unwrap();
    }

    #[test]
    fn search_opts_default_matches_legacy_case_insensitive_substring() {
        let mut out = Vec::new();
        let opts = SearchOpts::default();
        collect_matches_in_text(
            Path::new("a.txt"),
            "Hello World\nfoo bar\nHELLO again\nno match",
            "hello",
            opts,
            &mut out,
        );
        assert_eq!(out.len(), 2, "default opts mirror the original behaviour");
    }

    #[test]
    fn search_opts_case_sensitive_excludes_different_case() {
        let mut out = Vec::new();
        let opts = SearchOpts { case_sensitive: true, ..Default::default() };
        collect_matches_in_text(
            Path::new("a.txt"),
            "Hello World\nhello again\nHELLO loud",
            "hello",
            opts,
            &mut out,
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].line_text, "hello again");
    }

    #[test]
    fn search_opts_whole_word_excludes_partial_matches() {
        let mut out = Vec::new();
        let opts = SearchOpts { whole_word: true, ..Default::default() };
        collect_matches_in_text(
            Path::new("a.txt"),
            "cat sat\ncategory animal\ncat\nbobcat sleeps",
            "cat",
            opts,
            &mut out,
        );
        // Only the lines where 'cat' is a standalone word should match.
        let texts: Vec<&str> = out.iter().map(|h| h.line_text.as_str()).collect();
        assert_eq!(texts, vec!["cat sat", "cat"]);
    }

    #[test]
    fn search_opts_regex_finds_pattern_class() {
        let mut out = Vec::new();
        let opts = SearchOpts { use_regex: true, ..Default::default() };
        collect_matches_in_text(
            Path::new("a.txt"),
            "let x = 42;\nlet y = abc;\nlet z = 13;\nno number",
            r"\d+",
            opts,
            &mut out,
        );
        let texts: Vec<&str> = out.iter().map(|h| h.line_text.as_str()).collect();
        assert_eq!(texts, vec!["let x = 42;", "let z = 13;"]);
    }

    #[test]
    fn search_opts_regex_honours_case_sensitive_flag() {
        let mut out = Vec::new();
        let opts = SearchOpts { use_regex: true, case_sensitive: true, ..Default::default() };
        collect_matches_in_text(
            Path::new("a.txt"),
            "Foo\nfoo\nFOO",
            r"foo",
            opts,
            &mut out,
        );
        let texts: Vec<&str> = out.iter().map(|h| h.line_text.as_str()).collect();
        assert_eq!(texts, vec!["foo"], "regex must respect case-sensitive flag");
    }

    #[test]
    fn search_opts_invalid_regex_returns_empty_quietly() {
        let mut out = Vec::new();
        let opts = SearchOpts { use_regex: true, ..Default::default() };
        collect_matches_in_text(
            Path::new("a.txt"),
            "anything\nat all",
            r"[unclosed",
            opts,
            &mut out,
        );
        assert!(out.is_empty(), "invalid regex must not crash, just yield no hits");
    }

    #[test]
    fn toggle_at_maps_a_click_on_each_glyph_to_the_right_kind() {
        use ratatui::buffer::Buffer;
        let tmp = TempDir::new().unwrap();
        let mut panel = SearchPanel::new(tmp.path().to_path_buf());
        panel.focused = true;
        let area = Rect { x: 0, y: 0, width: 60, height: 12 };
        let mut buf = Buffer::empty(area);
        ratatui::widgets::Widget::render(&mut panel, area, &mut buf);
        let y = panel.toggle_y;
        // Each toggle is 2 cells; clicking either cell on the same row
        // should map to its kind.
        assert_eq!(
            panel.toggle_at(panel.toggle_case_x, y),
            Some(SearchToggle::CaseSensitive)
        );
        assert_eq!(
            panel.toggle_at(panel.toggle_case_x + 1, y),
            Some(SearchToggle::CaseSensitive)
        );
        assert_eq!(
            panel.toggle_at(panel.toggle_word_x, y),
            Some(SearchToggle::WholeWord)
        );
        assert_eq!(
            panel.toggle_at(panel.toggle_regex_x + 1, y),
            Some(SearchToggle::UseRegex)
        );
        // Wrong row → None.
        assert_eq!(panel.toggle_at(panel.toggle_case_x, y + 1), None);
        // Outside the toggle columns → None.
        assert_eq!(panel.toggle_at(0, y), None);
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
