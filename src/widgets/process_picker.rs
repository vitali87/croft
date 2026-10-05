//! Process picker for "Debug: Attach to Python Process".
//!
//! A centered, selectable list of the attachable CPython 3.14+ processes
//! discovered by [`crate::dap::discovery`]. Mirrors VS Code's "Attach using
//! Process Id" quick-pick. The widget owns only the candidate list and the
//! selection; `App::attach_to_python_target` performs the side effect (spawning
//! `pdb -p` in a PTY). Unlike the Command Palette there is no query line: the
//! attachable set is small, so a plain arrow-key list is enough.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Widget},
};

use crate::dap::discovery::{PyTarget, elide_middle};

/// The picker's owned state.
pub struct ProcessPicker {
    pub targets: Vec<PyTarget>,
    pub selected: usize,
    pub scroll: usize,
    pub last_rect: Rect,
    pub last_inner_height: u16,
}

impl ProcessPicker {
    pub fn new(targets: Vec<PyTarget>) -> Self {
        Self {
            targets,
            selected: 0,
            scroll: 0,
            last_rect: Rect::default(),
            last_inner_height: 0,
        }
    }

    pub fn select_next(&mut self) {
        if self.targets.is_empty() {
            return;
        }
        if self.selected + 1 < self.targets.len() {
            self.selected += 1;
        }
    }

    pub fn select_prev(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    pub fn selected_target(&self) -> Option<&PyTarget> {
        self.targets.get(self.selected)
    }

    /// The target index at screen row `y`, if `y` lands on a visible row.
    /// Unlike the query-line pickers this popup has no prompt or separator:
    /// the list body starts one row below `last_rect.y` (just the top
    /// border) and runs `last_inner_height` rows, in lock-step with
    /// [`render_process_picker`]. Used to map a mouse click to a row.
    pub fn row_index_at(&self, y: u16) -> Option<usize> {
        let list_top = self.last_rect.y.saturating_add(1);
        if y < list_top || y - list_top >= self.last_inner_height {
            return None;
        }
        let idx = self.scroll + (y - list_top) as usize;
        (idx < self.targets.len()).then_some(idx)
    }
}

/// The gutter before an unselected row; a selected one shows `> `.
const ROW_PREFIX: &str = "  ";
/// Blank columns kept between a row's end and the popup's right border.
const ROW_END_GAP: usize = 1;
/// Between a row's PID, version and command line (see `target_label`).
const LABEL_SEPARATOR: &str = "  ·  ";

/// `label` cut to `room` columns (#1283). The command line after the PID
/// and version loses its middle, marked with `…`, so the row keeps its PID
/// and version at the start and the script and arguments at the end, which
/// tell processes apart. Before, the end was cut off with no mark.
fn fit_label(label: &str, room: usize) -> String {
    if label.chars().count() <= room {
        return label.to_string();
    }
    let cmd_at = label
        .match_indices(LABEL_SEPARATOR)
        .nth(1)
        .map(|(at, sep)| at + sep.len());
    match cmd_at {
        Some(at) if label[..at].chars().count() < room => {
            let fixed = label[..at].chars().count();
            format!(
                "{}{}",
                &label[..at],
                elide_middle(&label[at..], room - fixed)
            )
        }
        _ => elide_middle(label, room),
    }
}

pub fn render_process_picker(
    picker: &mut ProcessPicker,
    area: Rect,
    buf: &mut Buffer,
    theme: crate::theme::Theme,
) {
    // Wide enough for the longest row, up to 90% of the terminal (#1283):
    // a fixed 110-column cap cut every command line before its arguments,
    // however wide the window. Short rows keep the usual size.
    let usual = (area.width.saturating_mul(7) / 10).clamp(40, 110);
    let rows_width = picker
        .targets
        .iter()
        .map(|t| t.label.chars().count() + ROW_PREFIX.chars().count() + ROW_END_GAP + 2)
        .max()
        .unwrap_or(0);
    let rows_width = u16::try_from(rows_width).unwrap_or(u16::MAX);
    let width = usual
        .max(rows_width.min(area.width.saturating_mul(9) / 10))
        .min(area.width);
    let height = area.height.saturating_mul(6) / 10;
    let height = height.max(8).min(area.height);
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 4;
    let rect = Rect {
        x,
        y,
        width,
        height,
    };
    picker.last_rect = rect;

    Widget::render(Clear, rect, buf);
    let title = Span::styled(
        " Attach to Python Process — Esc to close, ↑/↓ to navigate, Enter to attach ",
        Style::default()
            .fg(theme.ui(Color::Rgb(0xff, 0xff, 0xff)))
            .add_modifier(Modifier::BOLD),
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.ui(Color::Rgb(0x4e, 0x9a, 0xff))))
        .title(title.clone())
        .style(Style::default().bg(theme.ui(Color::Rgb(0x16, 0x18, 0x1f))));
    let inner = Rect {
        x: rect.x + 1,
        y: rect.y + 1,
        width: rect.width.saturating_sub(2),
        height: rect.height.saturating_sub(2),
    };
    Widget::render(block, rect, buf);
    if theme.gradient() {
        crate::gradient::paint_gradient_box(buf, rect);
        buf.set_span(rect.x + 1, rect.y, &title, title.width() as u16);
    }
    let sel_bg = if theme.gradient() {
        let (r, g, b) = crate::gradient::POPUP_SEL_BG;
        Color::Rgb(r, g, b)
    } else {
        theme.ui(Color::Rgb(0x1e, 0x3a, 0x6e))
    };

    if inner.height == 0 || inner.width == 0 {
        return;
    }
    picker.last_inner_height = inner.height;

    let visible = inner.height as usize;
    let total = picker.targets.len();
    if picker.selected >= picker.scroll + visible {
        picker.scroll = picker.selected + 1 - visible;
    }
    if picker.selected < picker.scroll {
        picker.scroll = picker.selected;
    }
    let end = (picker.scroll + visible).min(total);

    let mut lines: Vec<Line<'static>> = Vec::with_capacity(end - picker.scroll);
    for (offset, target) in picker.targets[picker.scroll..end].iter().enumerate() {
        let row_idx = picker.scroll + offset;
        let is_selected = row_idx == picker.selected;
        let row_style = if is_selected {
            Style::default().bg(sel_bg).fg(theme.ui(Color::White))
        } else {
            Style::default().fg(theme.ui(Color::Rgb(0xec, 0xef, 0xf4)))
        };
        let prefix = if is_selected { "> " } else { ROW_PREFIX };
        let room = (inner.width as usize).saturating_sub(prefix.chars().count() + ROW_END_GAP);
        lines.push(Line::from(vec![
            Span::styled(prefix.to_string(), row_style),
            Span::styled(fit_label(&target.label, room), row_style),
        ]));
    }
    Widget::render(Paragraph::new(lines), inner, buf);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dap::remote_attach::PyVersion;
    use std::path::PathBuf;

    fn target(pid: u32) -> PyTarget {
        PyTarget {
            pid,
            version: PyVersion {
                major: 3,
                minor: 14,
                patch: 2,
            },
            exe: PathBuf::from("/usr/bin/python3.14"),
            label: format!("PID {pid}"),
        }
    }

    #[test]
    fn selection_walks_and_clamps() {
        let mut p = ProcessPicker::new(vec![target(1), target(2), target(3)]);
        assert_eq!(p.selected, 0);
        p.select_prev();
        assert_eq!(p.selected, 0, "clamps at top");
        p.select_next();
        p.select_next();
        assert_eq!(p.selected, 2);
        p.select_next();
        assert_eq!(p.selected, 2, "clamps at bottom");
        assert_eq!(p.selected_target().map(|t| t.pid), Some(3));
    }

    #[test]
    fn empty_picker_has_no_selection() {
        let p = ProcessPicker::new(vec![]);
        assert_eq!(p.selected_target(), None);
    }

    fn labelled(pid: u32, cmd: &str) -> PyTarget {
        PyTarget {
            label: format!("PID {pid}  ·  Python 3.14.0  ·  {cmd}"),
            ..target(pid)
        }
    }

    /// The text of every screen row of a `w`x`h` frame with `p` drawn.
    fn draw(p: &mut ProcessPicker, w: u16, h: u16) -> Vec<String> {
        let area = Rect::new(0, 0, w, h);
        let mut buf = Buffer::empty(area);
        render_process_picker(p, area, &mut buf, crate::theme::Theme::default());
        (0..h)
            .map(|y| (0..w).map(|x| buf[(x, y)].symbol()).collect())
            .collect()
    }

    /// #1283: the popup was capped at 110 columns, so a wider terminal
    /// still cut every row before the arguments that tell processes apart.
    /// It grows with the terminal to fit its rows.
    #[test]
    fn a_wide_terminal_shows_a_long_command_line_whole() {
        let cmd = format!("python3.14 {} --queue emails", "w".repeat(120));
        let mut p = ProcessPicker::new(vec![labelled(1, &cmd)]);
        let rows = draw(&mut p, 250, 30);
        assert!(
            rows.iter().any(|r| r.contains(&format!("{cmd} "))),
            "{rows:#?}"
        );
    }

    /// #1283: a row too long for the popup loses the middle of its command
    /// line, marked with an ellipsis, and keeps the PID and version at the
    /// start and the arguments at the end. Before, the end was cut off
    /// with no mark.
    #[test]
    fn a_row_too_long_for_the_popup_keeps_its_pid_and_its_arguments() {
        let cmd = format!("python3.14 {} -m http.server 8766", "d".repeat(150));
        let mut p = ProcessPicker::new(vec![labelled(10686, &cmd)]);
        let rows = draw(&mut p, 100, 30);
        let row = rows
            .iter()
            .find(|r| r.contains("PID 10686"))
            .expect("the row is drawn");
        assert!(
            row.contains("PID 10686  ·  Python 3.14.0  ·  python3.14 d"),
            "{row}"
        );
        assert!(row.contains("d…d"), "{row}");
        assert!(row.contains("http.server 8766 "), "{row}");
    }

    /// #1283 negative: a row that fits is drawn as is, with no ellipsis,
    /// and a list of short rows keeps the popup at its usual minimum.
    #[test]
    fn a_row_that_fits_is_drawn_unchanged() {
        let mut p = ProcessPicker::new(vec![labelled(7, "python3 manage.py runserver")]);
        let rows = draw(&mut p, 250, 30);
        let row = rows.iter().find(|r| r.contains("PID 7")).expect("drawn");
        assert!(row.contains("PID 7  ·  Python 3.14.0  ·  python3 manage.py runserver "));
        assert!(!rows.iter().any(|r| r.contains('…')), "{rows:#?}");
        assert!(p.last_rect.width <= 110, "{:?}", p.last_rect);
    }

    /// This popup has no prompt or separator, so its click hit-test starts
    /// one row below the popup top (the border), not three like the
    /// query-line pickers. Rows outside the body (borders, or below the
    /// last target) map to nothing.
    #[test]
    fn row_index_at_maps_the_borderless_list_geometry() {
        let mut p = ProcessPicker::new(vec![target(1), target(2), target(3)]);
        p.last_rect = Rect {
            x: 10,
            y: 5,
            width: 40,
            height: 8,
        };
        p.last_inner_height = 6;
        assert_eq!(p.row_index_at(5), None, "top border is not a row");
        assert_eq!(
            p.row_index_at(6),
            Some(0),
            "first row sits under the border"
        );
        assert_eq!(p.row_index_at(8), Some(2));
        assert_eq!(p.row_index_at(9), None, "below the last target is empty");
        p.scroll = 1;
        assert_eq!(p.row_index_at(6), Some(1), "scroll offsets the mapping");
    }
}
