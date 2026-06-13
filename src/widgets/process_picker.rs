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

use crate::dap::discovery::PyTarget;

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
}

pub fn render_process_picker(
    picker: &mut ProcessPicker,
    area: Rect,
    buf: &mut Buffer,
    gradient: bool,
) {
    let width = area.width.saturating_mul(7) / 10;
    let width = width.clamp(40, 110.min(area.width));
    let height = area.height.saturating_mul(6) / 10;
    let height = height.clamp(8, area.height);
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
            .fg(Color::Rgb(0xff, 0xff, 0xff))
            .add_modifier(Modifier::BOLD),
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Rgb(0x4e, 0x9a, 0xff)))
        .title(title.clone())
        .style(Style::default().bg(Color::Rgb(0x16, 0x18, 0x1f)));
    let inner = Rect {
        x: rect.x + 1,
        y: rect.y + 1,
        width: rect.width.saturating_sub(2),
        height: rect.height.saturating_sub(2),
    };
    Widget::render(block, rect, buf);
    if gradient {
        crate::gradient::paint_gradient_box(buf, rect);
        buf.set_span(rect.x + 1, rect.y, &title, title.width() as u16);
    }
    let sel_bg = if gradient {
        let (r, g, b) = crate::gradient::POPUP_SEL_BG;
        Color::Rgb(r, g, b)
    } else {
        Color::Rgb(0x1e, 0x3a, 0x6e)
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
            Style::default().bg(sel_bg).fg(Color::White)
        } else {
            Style::default().fg(Color::Rgb(0xec, 0xef, 0xf4))
        };
        let prefix = if is_selected { "> " } else { "  " };
        lines.push(Line::from(vec![
            Span::styled(prefix.to_string(), row_style),
            Span::styled(target.label.clone(), row_style),
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
}
