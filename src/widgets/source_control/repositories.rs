//! The multi-root REPOSITORIES overview: one row per workspace folder that is a git
//! repository, drawing them, and the click that picks one.

use super::*;

/// One row of the multi-root REPOSITORIES overview (#161): a workspace
/// folder that is a git repository, as the App's per-folder workers see
/// it each frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepoRow {
    pub label: String,
    pub branch: Option<String>,
    pub changes: usize,
    pub active: bool,
    pub workspace_root: std::path::PathBuf,
}

impl SourceControlPanel {
    /// Replace the overview's rows; the keys leave REPOSITORIES when it
    /// hides, and its keyboard row stays on the list.
    pub fn set_repositories(&mut self, rows: Vec<RepoRow>) {
        self.repositories = rows;
        self.settle_keys();
    }

    /// REPOSITORIES takes the keys (#871), on the shown repository's row.
    pub fn focus_repositories(&mut self) {
        self.keys = KeyStop::Repositories;
        self.repo_cursor = self.repositories.iter().position(|r| r.active).unwrap_or(0);
    }

    /// Move REPOSITORIES' keyboard row `delta` rows, stopping at the ends.
    pub fn move_repo_cursor(&mut self, delta: isize) {
        let last = self.repositories.len().saturating_sub(1);
        self.repo_cursor = self.repo_cursor.saturating_add_signed(delta).min(last);
    }

    /// The repository on REPOSITORIES' keyboard row.
    pub fn repo_under_cursor(&self) -> Option<&RepoRow> {
        self.repositories.get(self.repo_cursor)
    }

    /// Paint the multi-root REPOSITORIES overview (#161) starting at `y`,
    /// returning the y below it (unchanged when hidden). One row per
    /// repository: label, branch (dim), change count (right-aligned), the
    /// shown repository's label in bold. The selection background sits on
    /// the shown repository's row, or on the keyboard's row while
    /// REPOSITORIES has the keys (#871), and wears the focus colour only
    /// then, as the change list's does.
    pub(super) fn render_repositories_section(
        &mut self,
        inner: Rect,
        y: u16,
        buf: &mut Buffer,
    ) -> u16 {
        if self.repositories.len() < 2 {
            return y;
        }
        let bottom = inner.y + inner.height;
        let mut y = y;
        if y >= bottom {
            return y;
        }
        let tones = crate::widgets::surface::tones(self.theme);
        buf.set_stringn(
            inner.x + 1,
            y,
            "REPOSITORIES",
            inner.width.saturating_sub(1) as usize,
            Style::default()
                .fg(tones.map_or_else(
                    || self.theme.ui(Color::Rgb(0x8a, 0x92, 0xa4)),
                    |t| t.muted(),
                ))
                .add_modifier(Modifier::BOLD),
        );
        y += 1;
        let has_keys = self.keys == KeyStop::Repositories;
        for (i, row) in self.repositories.iter().enumerate() {
            if y >= bottom {
                break;
            }
            let picked = if has_keys {
                i == self.repo_cursor
            } else {
                row.active
            };
            let rect = Rect {
                x: inner.x,
                y,
                width: inner.width,
                height: 1,
            };
            // Croft Loch: rows stay on the chrome (the default background
            // painted black bars across it), the active repository wears the
            // selected-row band and bar, the label is body text and the
            // branch muted.
            if let Some(t) = tones {
                let hovered = crate::widgets::hover::contains(rect, self.hover_pointer);
                // Clear the row onto the chrome first: in the no-repo state
                // the overview paints over the empty-state card's top.
                for x in rect.left()..rect.right() {
                    buf[(x, y)]
                        .set_symbol(" ")
                        .set_style(Style::default().bg(t.chrome()));
                }
                if picked {
                    crate::widgets::surface::paint_selected_row(
                        buf,
                        Rect {
                            x: rect.x.saturating_sub(1),
                            width: rect.width + u16::from(rect.x > 0),
                            ..rect
                        },
                        t,
                        t.chrome,
                        self.focused && has_keys,
                    );
                } else if hovered {
                    buf.set_style(rect, Style::default().bg(t.hover_on(t.chrome)));
                }
                let branch = row.branch.as_deref().unwrap_or("—");
                let weight = if row.active {
                    Modifier::BOLD
                } else {
                    Modifier::empty()
                };
                let line = Line::from(vec![
                    Span::styled(
                        format!(" {}", row.label),
                        Style::default().fg(t.text()).add_modifier(weight),
                    ),
                    Span::styled(format!("  {branch}"), Style::default().fg(t.muted())),
                ]);
                buf.set_line(rect.x, y, &line, rect.width);
                let left_w = line.width() as u16;
                if row.changes > 0 {
                    let count = row.changes.to_string();
                    let cx = (rect.x + rect.width).saturating_sub(count.len() as u16 + 1);
                    if cx > rect.x + left_w {
                        buf.set_string(
                            cx,
                            y,
                            &count,
                            Style::default().fg(self.theme.ui(Color::Rgb(0xe0, 0x9a, 0x4e))),
                        );
                    }
                }
                self.repo_row_areas.push((rect, row.workspace_root.clone()));
                y += 1;
                continue;
            }
            let hover_bg =
                crate::widgets::hover::row_hover_bg(rect, self.hover_pointer, self.theme);
            let (bg, fg) = if picked {
                (self.theme.selection(), self.theme.ui(Color::White))
            } else if let Some(h) = hover_bg {
                (h, self.theme.ui(Color::Gray))
            } else {
                (Color::Reset, self.theme.ui(Color::Gray))
            };
            for x in rect.x..rect.x + rect.width {
                buf[(x, y)].set_bg(bg);
            }
            let branch = row.branch.as_deref().unwrap_or("—");
            let left = format!(" {}  {branch}", row.label);
            buf.set_stringn(
                rect.x,
                y,
                &left,
                rect.width as usize,
                Style::default().bg(bg).fg(fg).add_modifier(if row.active {
                    Modifier::BOLD
                } else {
                    Modifier::empty()
                }),
            );
            if row.changes > 0 {
                let count = row.changes.to_string();
                let cx = (rect.x + rect.width).saturating_sub(count.len() as u16 + 1);
                if cx > rect.x + left.len() as u16 {
                    buf.set_string(
                        cx,
                        y,
                        &count,
                        Style::default()
                            .bg(bg)
                            .fg(self.theme.ui(Color::Rgb(0xe0, 0x9a, 0x4e))),
                    );
                }
            }
            self.repo_row_areas.push((rect, row.workspace_root.clone()));
            y += 1;
        }
        // One blank spacer so the branch row does not crowd the section.
        y = (y + 1).min(bottom);
        y
    }

    /// Map a click to the repositories-overview row it landed on (#161).
    pub fn click_repository(&self, x: u16, y: u16) -> Option<std::path::PathBuf> {
        self.repo_row_areas
            .iter()
            .find(|(r, _)| x >= r.x && x < r.x + r.width && y == r.y)
            .map(|(_, root)| root.clone())
    }
}
