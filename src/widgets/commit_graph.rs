//! The Source Control COMMITS section: a repo-wide commit graph with
//! box-drawing lane rails (the tig / lazygit idiom, VS Code's built-in Source
//! Control Graph). The rows are fetched off-thread by the app via
//! [`crate::git::commit_graph`], laid out into rails by [`crate::git::layout_graph`] (pure,
//! so it runs on the fetch thread), and handed here with [`set_rows`]; clicking
//! a row opens that commit's full patch in a read-only editor tab.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Borders, Paragraph, Widget},
};

use crate::git::GraphCommit;
use crate::git::GraphRow;
use crate::theme::Theme;
use crate::widgets::scrollbar;

const COLOR_DIM: Color = Color::Rgb(0x60, 0x68, 0x78);
const COLOR_SUMMARY: Color = Color::Rgb(0xCC, 0xCC, 0xCC);
const COLOR_TAG: Color = Color::Rgb(0xEB, 0xCB, 0x8B);

/// Left indent matching the tree's `Borders::ALL` inset, like TIMELINE.
const CONTENT_INDENT: u16 = 1;

/// Lane colours, cycled by lane index — the same idea as VS Code's
/// `scmGraph.historyItemRefColor` rotation.
const LANE_COLORS: &[Color] = &[
    Color::Rgb(0x4E, 0x9A, 0xFF),
    Color::Rgb(0x8C, 0xC2, 0x65),
    Color::Rgb(0xEB, 0xCB, 0x8B),
    Color::Rgb(0xB4, 0x8E, 0xAD),
    Color::Rgb(0x5F, 0xB4, 0xB4),
    Color::Rgb(0xD0, 0x87, 0x70),
];

pub fn lane_color(lane: usize) -> Color {
    LANE_COLORS[lane % LANE_COLORS.len()]
}

pub struct CommitGraphPanel {
    pub collapsed: bool,
    rows: Vec<GraphRow>,
    /// `true` once a fetch (even an empty one) has landed, so the panel can
    /// show "No commits" instead of spinning on "Loading".
    loaded: bool,
    scroll: usize,
    pub focus_gradient: bool,
    pub theme: Theme,
    pub focused: bool,
    /// COMMITS has the keys (#871): set before each draw from Source
    /// Control's key stop, which holds it. Its selected row then shows the
    /// focused band.
    pub has_keys: bool,
    /// The row the keyboard or a click picked, kept on screen and kept
    /// across a refresh by its commit.
    selected: Option<usize>,
    pub hover_pointer: Option<(u16, u16)>,

    pub last_area: Rect,
    pub last_scrollbar: Rect,
    last_header_row: u16,
    last_header_x: u16,
    last_header_w: u16,
    first_row_y: u16,
    visible_rows: u16,
    viewport_rows: u16,
}

impl CommitGraphPanel {
    pub fn new() -> Self {
        Self {
            // Expanded by default: the graph is the section's whole point,
            // like VS Code shipping its Source Control Graph visible.
            collapsed: false,
            rows: Vec::new(),
            loaded: false,
            scroll: 0,
            focus_gradient: false,
            theme: Theme::default(),
            focused: false,
            has_keys: false,
            selected: None,
            hover_pointer: None,
            last_area: Rect::default(),
            last_scrollbar: Rect::default(),
            last_header_row: 0,
            last_header_x: 0,
            last_header_w: 0,
            first_row_y: 0,
            visible_rows: 0,
            viewport_rows: 0,
        }
    }

    /// Apply a fetched, laid-out graph. Keeps the scroll position when the
    /// newest commit is unchanged (a background refresh), resets it otherwise.
    pub fn set_rows(&mut self, rows: Vec<GraphRow>) {
        let same_tip = match (rows.first(), self.rows.first()) {
            (Some(a), Some(b)) => a.commit.hash == b.commit.hash,
            _ => false,
        };
        let picked = self.selected_commit().map(|c| c.hash.clone());
        self.rows = rows;
        self.loaded = true;
        if !same_tip {
            self.scroll = 0;
        }
        // The picked commit stays picked where it went; a row that left
        // the history (a reset, a rebase) leaves nothing picked.
        self.selected =
            picked.and_then(|hash| self.rows.iter().position(|r| r.commit.hash == hash));
    }

    /// The history row of the commit the keyboard or a click picked.
    pub fn selected_row(&self) -> Option<usize> {
        self.selected
    }

    /// The commit the keyboard or a click picked.
    pub fn selected_commit(&self) -> Option<&GraphCommit> {
        self.selected
            .and_then(|i| self.rows.get(i))
            .map(|r| &r.commit)
    }

    /// Pick row `index` (clamped to the history) and scroll it on screen.
    pub fn select(&mut self, index: usize) {
        let Some(last) = self.rows.len().checked_sub(1) else {
            self.selected = None;
            return;
        };
        let index = index.min(last);
        self.selected = Some(index);
        let page = self.page();
        if index < self.scroll {
            self.scroll = index;
        } else if index >= self.scroll + page {
            self.scroll = index + 1 - page;
        }
    }

    /// Move the pick `delta` rows, from the first row when nothing is picked.
    pub fn move_selection(&mut self, delta: isize) {
        let to = match self.selected {
            None => 0,
            Some(from) => from.saturating_add_signed(delta),
        };
        self.select(to);
    }

    /// How many rows one page moves: the rows the body last showed.
    pub fn page(&self) -> usize {
        usize::from(self.viewport_rows.max(1))
    }

    /// How many commits the history holds.
    pub fn commit_count(&self) -> usize {
        self.rows.len()
    }

    /// The last history row.
    pub fn last_row(&self) -> usize {
        self.rows.len().saturating_sub(1)
    }

    /// The history row drawn at `y`, if any.
    pub fn row_index_at(&self, y: u16) -> Option<usize> {
        if self.collapsed || self.visible_rows == 0 || y < self.first_row_y {
            return None;
        }
        let offset = usize::from(y - self.first_row_y);
        (offset < usize::from(self.visible_rows)).then_some(self.scroll + offset)
    }

    /// Whether a fetched graph has landed since the last [`Self::clear`]
    /// (false = the section still says "Loading").
    pub fn is_loaded(&self) -> bool {
        self.loaded
    }

    pub fn clear(&mut self) {
        self.rows.clear();
        self.loaded = false;
        self.scroll = 0;
        self.selected = None;
    }

    pub fn toggle_collapse(&mut self) {
        self.collapsed = !self.collapsed;
    }

    pub fn scroll_down(&mut self, n: usize) {
        self.scroll = (self.scroll + n).min(self.max_scroll());
    }

    pub fn scroll_up(&mut self, n: usize) {
        self.scroll = self.scroll.saturating_sub(n);
    }

    fn max_scroll(&self) -> usize {
        self.rows
            .len()
            .saturating_sub(self.viewport_rows.max(1) as usize)
    }

    pub fn scroll_to_bar_y(&mut self, y: u16) -> bool {
        let Some(metrics) = scrollbar::vertical_metrics(
            self.last_scrollbar,
            self.rows.len(),
            self.viewport_rows as usize,
            self.scroll,
        ) else {
            return false;
        };
        let target = scrollbar::scroll_for_y(metrics, y);
        let moved = target != self.scroll;
        self.scroll = target;
        moved
    }

    /// Collapsed is the header + top separator; expanded adds one row per
    /// commit, capped at half the Source Control strip so the change list
    /// above always keeps room.
    pub fn desired_height(&self, available: u16) -> u16 {
        const BORDER: u16 = 1;
        if available == 0 {
            return 0;
        }
        let header = 1u16;
        let floor = header + BORDER;
        if self.collapsed {
            return floor.min(available);
        }
        let content = (self.rows.len() as u16).max(1);
        let half = (available / 2).max(floor);
        (header + content + BORDER).min(half)
    }

    pub fn hit_header(&self, x: u16, y: u16) -> bool {
        y == self.last_header_row
            && x >= self.last_header_x
            && x < self.last_header_x.saturating_add(self.last_header_w)
    }

    /// Map a click row to the commit it lands on.
    pub fn commit_at(&self, y: u16) -> Option<&GraphCommit> {
        self.row_index_at(y)
            .and_then(|i| self.rows.get(i))
            .map(|r| &r.commit)
    }
}

impl Default for CommitGraphPanel {
    fn default() -> Self {
        Self::new()
    }
}

/// Strip a decoration down to its badge text: `HEAD -> main` reads `main`,
/// `tag: v1.0` reads `v1.0`. Returns the text plus whether it is a tag and
/// whether it is the HEAD ref.
fn badge_of(decoration: &str) -> (&str, bool, bool) {
    if let Some(name) = decoration.strip_prefix("HEAD -> ") {
        (name, false, true)
    } else if let Some(name) = decoration.strip_prefix("tag: ") {
        (name, true, false)
    } else {
        (decoration, false, false)
    }
}

impl Widget for &mut CommitGraphPanel {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let block = crate::widgets::surface::section_block(
            self.theme,
            Borders::TOP,
            Style::default().fg(self.theme.ui(COLOR_DIM)),
        );
        let inner = block.inner(area);
        block.render(area, buf);
        self.last_area = area;
        self.last_scrollbar = Rect::default();
        self.visible_rows = 0;
        if inner.height == 0 || inner.width == 0 {
            return;
        }
        let inner = Rect {
            x: inner.x + CONTENT_INDENT.min(inner.width),
            width: inner.width.saturating_sub(CONTENT_INDENT),
            ..inner
        };

        let header_y = inner.y;
        let header_row = Rect {
            x: inner.x,
            y: header_y,
            width: inner.width,
            height: 1,
        };
        crate::widgets::surface::paint_section_header(
            buf,
            header_row,
            self.theme,
            !self.collapsed,
            "COMMITS",
            Vec::new(),
            // With the keys but no row to pick, the header shows them.
            crate::widgets::hover::contains(header_row, self.hover_pointer)
                || (self.has_keys && (self.collapsed || self.rows.is_empty())),
        );
        self.last_header_row = header_y;
        self.last_header_x = inner.x;
        self.last_header_w = inner.width;

        if self.collapsed || inner.height < 2 {
            return;
        }

        let body_y = header_y + 1;
        let body_h = inner.height - 1;
        self.first_row_y = body_y;
        self.viewport_rows = body_h;

        if self.rows.is_empty() {
            let msg = if self.loaded { "No commits" } else { "Loading" };
            Paragraph::new(Line::from(Span::styled(
                msg,
                Style::default().fg(self.theme.ui(COLOR_DIM)),
            )))
            .render(
                Rect {
                    x: inner.x,
                    y: body_y,
                    width: inner.width,
                    height: 1,
                },
                buf,
            );
            return;
        }

        self.scroll = self.scroll.min(self.max_scroll());
        let bar = scrollbar::vertical_metrics(
            Rect {
                x: inner.x + inner.width.saturating_sub(1),
                y: body_y,
                width: 1,
                height: body_h,
            },
            self.rows.len(),
            body_h as usize,
            self.scroll,
        );
        let content_w = inner.width.saturating_sub(u16::from(bar.is_some()));

        let visible = (body_h as usize).min(self.rows.len().saturating_sub(self.scroll));
        self.visible_rows = visible as u16;
        // Rails align across rows: pad every row's rail run to the widest
        // visible one so subjects start in one column.
        let rail_w = self
            .rows
            .iter()
            .skip(self.scroll)
            .take(visible)
            .map(|r| r.cells.len())
            .max()
            .unwrap_or(1);

        for row in 0..visible {
            let graph_row = &self.rows[self.scroll + row];
            let y = body_y + row as u16;
            let row_rect = Rect {
                x: inner.x,
                y,
                width: content_w,
                height: 1,
            };
            // The picked row: the change list's selected-row anatomy, the
            // band (greyed while COMMITS has not the keys) on a card, the
            // list blue on a plain panel.
            let picked = self.selected == Some(self.scroll + row);
            let tones = crate::widgets::surface::tones(self.theme);
            if picked {
                match tones {
                    Some(t) => crate::widgets::surface::paint_selected_row(
                        buf,
                        Rect {
                            x: area.x,
                            width: row_rect.right().saturating_sub(area.x),
                            ..row_rect
                        },
                        t,
                        t.chrome,
                        self.focused && self.has_keys,
                    ),
                    None => buf.set_style(
                        row_rect,
                        Style::default().bg(self.theme.ui(Color::Rgb(0x26, 0x4f, 0x78))),
                    ),
                }
            } else if let Some(bg) =
                crate::widgets::hover::row_hover_bg(row_rect, self.hover_pointer, self.theme)
            {
                buf.set_style(row_rect, Style::default().bg(bg));
            }
            let mut spans: Vec<Span> = Vec::new();
            for &(ch, lane) in &graph_row.cells {
                spans.push(Span::styled(
                    ch.to_string(),
                    Style::default().fg(lane_color(lane)),
                ));
            }
            for _ in graph_row.cells.len()..rail_w + 1 {
                spans.push(Span::raw(" "));
            }
            for decoration in &graph_row.commit.refs {
                let (name, is_tag, is_head) = badge_of(decoration);
                let color = if is_tag {
                    self.theme.ui(COLOR_TAG)
                } else {
                    self.theme.accent()
                };
                let mut style = Style::default().fg(color);
                if is_head {
                    style = style.add_modifier(Modifier::BOLD);
                }
                spans.push(Span::styled(format!("\u{f126} {name} "), style));
            }
            spans.push(Span::styled(
                graph_row.commit.summary.clone(),
                Style::default().fg(self.theme.ui(COLOR_SUMMARY)),
            ));
            // Right-aligned dim age, clipped away first when width is tight.
            let age = crate::git::humanize_age(graph_row.commit.age_secs);
            let used: usize = spans.iter().map(|s| s.content.chars().count()).sum();
            let age_cols = age.chars().count() + 1;
            if used + age_cols < content_w as usize {
                let pad = content_w as usize - used - age_cols;
                spans.push(Span::raw(" ".repeat(pad)));
                spans.push(Span::styled(
                    age,
                    Style::default().fg(self.theme.ui(COLOR_DIM)),
                ));
            }
            Paragraph::new(Line::from(spans)).render(row_rect, buf);
        }

        if let Some(metrics) = bar {
            self.last_scrollbar = metrics.area;
            scrollbar::render_vertical_on_chrome(buf, metrics, self.focused, self.theme);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::layout_graph;

    fn commit(hash: &str, parents: &[&str]) -> GraphCommit {
        GraphCommit {
            hash: hash.to_string(),
            short_hash: hash.chars().take(7).collect(),
            parents: parents.iter().map(|p| p.to_string()).collect(),
            refs: Vec::new(),
            summary: format!("commit {hash}"),
            author: "vitali87".to_string(),
            age_secs: 3600,
        }
    }

    #[test]
    fn badge_of_strips_head_and_tag_prefixes() {
        assert_eq!(badge_of("HEAD -> main"), ("main", false, true));
        assert_eq!(badge_of("tag: v0.1.0"), ("v0.1.0", true, false));
        assert_eq!(badge_of("origin/main"), ("origin/main", false, false));
    }

    #[test]
    fn panel_renders_header_rails_summary_and_refs() {
        let mut p = CommitGraphPanel::new();
        let mut c = commit("abc1234", &[]);
        c.refs = vec!["HEAD -> main".to_string()];
        c.summary = "feat: first".to_string();
        p.set_rows(layout_graph(vec![c]));
        let area = Rect {
            x: 0,
            y: 0,
            width: 44,
            height: 6,
        };
        let mut buf = Buffer::empty(area);
        (&mut p).render(area, &mut buf);
        let mut text = String::new();
        for y in 0..area.height {
            for x in 0..area.width {
                text.push_str(buf[(x, y)].symbol());
            }
            text.push('\n');
        }
        assert!(text.contains("COMMITS"), "got:\n{text}");
        assert!(text.contains('●'), "got:\n{text}");
        assert!(text.contains("feat: first"), "got:\n{text}");
        assert!(text.contains("main"), "got:\n{text}");
        assert!(text.contains("hour"), "got:\n{text}");
        // Click mapping: header row is not a commit, the first body row is.
        assert!(p.commit_at(1).is_none(), "y=1 is the header");
        assert_eq!(
            p.commit_at(2).map(|c| c.short_hash.as_str()),
            Some("abc1234")
        );
    }

    #[test]
    fn refresh_with_same_tip_keeps_scroll() {
        let mut p = CommitGraphPanel::new();
        let many: Vec<GraphCommit> = (0..40)
            .map(|i| commit(&format!("c{i}"), &[&format!("c{}", i + 1)]))
            .collect();
        p.set_rows(layout_graph(many.clone()));
        // Render once so the viewport is known, then scroll.
        let area = Rect {
            x: 0,
            y: 0,
            width: 40,
            height: 12,
        };
        let mut buf = Buffer::empty(area);
        (&mut p).render(area, &mut buf);
        p.scroll_down(5);
        assert_eq!(p.scroll, 5);
        p.set_rows(layout_graph(many));
        assert_eq!(p.scroll, 5, "same tip: background refresh keeps place");
        let mut newer = vec![commit("new", &["c0"])];
        newer.extend((0..40).map(|i| commit(&format!("c{i}"), &[&format!("c{}", i + 1)])));
        p.set_rows(layout_graph(newer));
        assert_eq!(p.scroll, 0, "a new tip resets to the newest commit");
    }
}
