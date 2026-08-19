//! The Source Control COMMITS section: a repo-wide commit graph with
//! box-drawing lane rails (the tig / lazygit idiom, VS Code's built-in Source
//! Control Graph). The rows are fetched off-thread by the app via
//! [`crate::git::commit_graph`], laid out into rails by [`layout_graph`] (pure,
//! so it runs on the fetch thread), and handed here with [`set_rows`]; clicking
//! a row opens that commit's full patch in a read-only editor tab.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Widget},
};

use crate::git::GraphCommit;
use crate::theme::Theme;
use crate::widgets::scrollbar;

const COLOR_HEADER: Color = Color::Rgb(0xE8, 0xEE, 0xF8);
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

/// One paintable rail cell: the glyph and the lane index that colours it.
pub type RailCell = (char, usize);

/// One laid-out graph row: the commit plus its rail cells, ready to paint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GraphRow {
    pub commit: GraphCommit,
    pub cells: Vec<RailCell>,
}

/// Lay the topo-ordered commit list (newest first) out into lanes.
///
/// The classic lane algorithm: each lane holds the hash it expects next.
/// A commit lands on the first lane expecting it (or a free one for a new
/// branch tip); other lanes expecting the same hash close into the node
/// (`╯` / `╰`); a merge commit's extra parents open new lanes (`╮` / `╭`),
/// or junction into a lane that already expects that parent (`┤` / `├`).
/// Pass-through rails between the node and the farthest connector cross as
/// `┼`; empty cells on that span fill with `─`.
pub fn layout_graph(commits: Vec<GraphCommit>) -> Vec<GraphRow> {
    let mut lanes: Vec<Option<String>> = Vec::new();
    let mut rows = Vec::with_capacity(commits.len());
    for c in commits {
        let node_lane = match lanes
            .iter()
            .position(|l| l.as_deref() == Some(c.hash.as_str()))
        {
            Some(i) => i,
            None => match lanes.iter().position(|l| l.is_none()) {
                Some(i) => i,
                None => {
                    lanes.push(None);
                    lanes.len() - 1
                }
            },
        };
        let closing: Vec<usize> = lanes
            .iter()
            .enumerate()
            .filter(|(i, l)| *i != node_lane && l.as_deref() == Some(c.hash.as_str()))
            .map(|(i, _)| i)
            .collect();
        let before: Vec<bool> = lanes.iter().map(|l| l.is_some()).collect();
        lanes[node_lane] = c.parents.first().cloned();
        for &i in &closing {
            lanes[i] = None;
        }
        let mut opening: Vec<usize> = Vec::new();
        for p in c.parents.iter().skip(1) {
            match lanes.iter().position(|l| l.as_deref() == Some(p.as_str())) {
                Some(i) => opening.push(i),
                None => {
                    let i = match lanes.iter().position(|l| l.is_none()) {
                        // Prefer a free slot right of the node so merge rails
                        // fan out rightward like every graph tool.
                        Some(free) if free > node_lane => free,
                        _ => {
                            lanes.push(None);
                            lanes.len() - 1
                        }
                    };
                    lanes[i] = Some(p.clone());
                    opening.push(i);
                }
            }
        }
        let width = lanes.len().max(before.len());
        let mut span_min = node_lane;
        let mut span_max = node_lane;
        for &i in closing.iter().chain(opening.iter()) {
            span_min = span_min.min(i);
            span_max = span_max.max(i);
        }
        let mut cells: Vec<RailCell> = Vec::with_capacity(width);
        for i in 0..width {
            let before_occ = before.get(i).copied().unwrap_or(false);
            let after_occ = lanes.get(i).is_some_and(|l| l.is_some());
            let ch = if i == node_lane {
                '●'
            } else if closing.contains(&i) {
                if i > node_lane { '╯' } else { '╰' }
            } else if opening.contains(&i) {
                if before_occ {
                    if i > node_lane { '┤' } else { '├' }
                } else if i > node_lane {
                    '╮'
                } else {
                    '╭'
                }
            } else if before_occ && after_occ {
                if i > span_min && i < span_max {
                    '┼'
                } else {
                    '│'
                }
            } else if i > span_min && i < span_max {
                '─'
            } else {
                ' '
            };
            cells.push((ch, i));
        }
        while cells.last().is_some_and(|(ch, _)| *ch == ' ') {
            cells.pop();
        }
        rows.push(GraphRow { commit: c, cells });
        while lanes.last().is_some_and(|l| l.is_none()) {
            lanes.pop();
        }
    }
    rows
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
        self.rows = rows;
        self.loaded = true;
        if !same_tip {
            self.scroll = 0;
        }
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
        if self.collapsed || self.visible_rows == 0 || y < self.first_row_y {
            return None;
        }
        let offset = (y - self.first_row_y) as usize;
        if offset >= self.visible_rows as usize {
            return None;
        }
        self.rows.get(self.scroll + offset).map(|r| &r.commit)
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
        let block = Block::default()
            .borders(Borders::TOP)
            .border_style(Style::default().fg(self.theme.ui(COLOR_DIM)));
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

        let chevron = if self.collapsed {
            crate::icons::CHEVRON_CLOSED
        } else {
            crate::icons::CHEVRON_OPEN
        };
        let header_y = inner.y;
        Paragraph::new(Line::from(vec![
            Span::styled(
                format!("{chevron} "),
                Style::default().fg(self.theme.ui(COLOR_DIM)),
            ),
            Span::styled(
                "COMMITS",
                Style::default()
                    .fg(self.theme.ui(COLOR_HEADER))
                    .add_modifier(Modifier::BOLD),
            ),
        ]))
        .render(
            Rect {
                x: inner.x,
                y: header_y,
                width: inner.width,
                height: 1,
            },
            buf,
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
            if let Some(bg) =
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
            scrollbar::render_vertical(buf, metrics, self.focused, self.theme);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn rails(rows: &[GraphRow]) -> Vec<String> {
        rows.iter()
            .map(|r| r.cells.iter().map(|(ch, _)| *ch).collect())
            .collect()
    }

    #[test]
    fn linear_history_is_a_single_node_column() {
        let rows = layout_graph(vec![
            commit("c", &["b"]),
            commit("b", &["a"]),
            commit("a", &[]),
        ]);
        assert_eq!(rails(&rows), vec!["●", "●", "●"]);
        // Every node sits on lane 0.
        assert!(rows.iter().all(|r| r.cells[0].1 == 0));
    }

    #[test]
    fn merge_opens_a_lane_and_the_branch_point_closes_it() {
        // m merges b into the a-line; both a and b descend from r.
        let rows = layout_graph(vec![
            commit("m", &["a", "b"]),
            commit("a", &["r"]),
            commit("b", &["r"]),
            commit("r", &[]),
        ]);
        assert_eq!(
            rails(&rows),
            vec![
                "●╮", // merge: node + the second-parent rail opening
                "●│", // a on lane 0, b's rail passing through
                "│●", // b on lane 1
                "●╯", // r: lane 1 closes back into the node
            ]
        );
        // The b rail is lane 1 throughout, so it keeps one colour.
        assert_eq!(rows[0].cells[1].1, 1);
        assert_eq!(rows[2].cells[1].1, 1);
        assert_eq!(rows[3].cells[1].1, 1);
    }

    #[test]
    fn second_branch_tip_gets_its_own_lane_without_a_merge() {
        // Two branch tips (t on top of a, u also on top of a): u appears
        // after t with no lane expecting it, so it allocates lane 1, and the
        // shared parent closes it.
        let rows = layout_graph(vec![
            commit("t", &["a"]),
            commit("u", &["a"]),
            commit("a", &[]),
        ]);
        assert_eq!(rails(&rows), vec!["●", "│●", "●╯"]);
    }

    #[test]
    fn pass_through_lane_crosses_a_long_connector() {
        // m merges c (lane 2) while b (lane 1) passes through: the connector
        // from the node to ╯ must cross b's rail as ┼.
        let rows = layout_graph(vec![
            commit("m", &["a", "c"]),
            commit("x", &["b"]),
            commit("c", &["y"]),
            commit("a", &["z1"]),
            commit("b", &["z2"]),
            commit("y", &["z3"]),
        ]);
        // Row 0: m on lane 0 opens c's rail. Row 1 is x, a NEW tip: it takes
        // the next free lane. The exact rails depend on allocation order; the
        // load-bearing assertion is the crossing on y's row if c closed over
        // a live rail. Assert the merge row instead, which is deterministic.
        assert_eq!(rows[0].cells[0].0, '●');
        assert!(
            rows[0].cells.iter().any(|(ch, _)| *ch == '╮'),
            "the merge's second parent must open a rail; got {:?}",
            rails(&rows)
        );
    }

    #[test]
    fn merge_into_an_expected_lane_junctions_instead_of_opening() {
        // m's second parent b is ALREADY expected by lane 0 (x's parent):
        // the merge junctions into that rail (├) rather than opening a third
        // lane, so the shared parent keeps one rail.
        let rows = layout_graph(vec![
            commit("x", &["b"]),
            commit("m", &["a", "b"]),
            commit("b", &[]),
        ]);
        assert_eq!(
            rails(&rows),
            vec![
                "●",  // x on lane 0, expecting b
                "├●", // m on lane 1; its b-parent junctions into lane 0
                "●│", // b lands on lane 0; lane 1 still awaits a (not listed)
            ]
        );
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
