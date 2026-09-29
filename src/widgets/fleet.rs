//! The FLEET tab (#363): one tile per host a fleet run reached, with an
//! exit-status dot and the time it took, under a `7 identical · 2 differ ·
//! 1 failed` summary. In diff mode a tile shows only how its output differs
//! from the reference: the plurality output, or the tile the user picked.

use crate::fleet::HostResult;
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
};

/// The narrowest a tile gets before the grid drops a column.
const TILE_MIN_WIDTH: u16 = 36;

#[derive(Debug, Clone)]
pub struct FleetView {
    pub command: String,
    pub results: Vec<HostResult>,
    /// The tile a user chose as the reference, overriding the plurality.
    pub reference_host: Option<String>,
    pub selected: usize,
    pub diff_mode: bool,
    /// Frame truth for mouse hits: each tile's rect, in `results` order.
    pub tile_rects: Vec<Rect>,
    /// Columns in the last frame's grid, for Up and Down.
    pub columns: usize,
}

/// How a tile's host fared, which picks its dot's colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TileState {
    Same,
    Differs,
    Failed,
    TimedOut,
}

impl FleetView {
    pub fn new(command: String, results: Vec<HostResult>) -> FleetView {
        FleetView {
            command,
            results,
            reference_host: None,
            selected: 0,
            // A comparison is the point of a fleet run; start in it.
            diff_mode: true,
            tile_rects: Vec::new(),
            columns: 1,
        }
    }

    /// The output every other tile is compared with: the chosen tile's,
    /// else the plurality of the hosts that succeeded (none on a tie).
    pub fn reference(&self) -> Option<&str> {
        match &self.reference_host {
            Some(host) => self
                .results
                .iter()
                .find(|r| &r.host == host)
                .map(|r| r.output.as_str()),
            None => crate::fleet::reference_output(&self.results),
        }
    }

    pub fn state(&self, r: &HostResult) -> TileState {
        if r.exit.is_none() {
            TileState::TimedOut
        } else if !r.ok() {
            TileState::Failed
        } else if Some(r.output.as_str()) == self.reference() {
            TileState::Same
        } else {
            TileState::Differs
        }
    }

    /// The summary line over the grid.
    pub fn summary(&self) -> String {
        crate::fleet::summarise(&self.results, self.reference()).line()
    }

    /// The lines a tile shows: its output, or in diff mode only how it
    /// differs from the reference (`- ` missing, `+ ` extra).
    pub fn tile_lines(&self, r: &HostResult) -> Vec<String> {
        match (self.diff_mode, self.reference(), self.state(r)) {
            (true, Some(reference), TileState::Differs) => {
                crate::fleet::differing_lines(reference, &r.output)
            }
            (true, Some(_), TileState::Same) => vec![String::from("(same as the reference)")],
            _ => r.output.lines().map(str::to_string).collect(),
        }
    }

    /// Move the selection by `delta` tiles, clamped.
    pub fn move_selection(&mut self, delta: isize) {
        let last = self.results.len().saturating_sub(1);
        self.selected = self.selected.saturating_add_signed(delta).min(last);
    }

    pub fn selected_result(&self) -> Option<&HostResult> {
        self.results.get(self.selected)
    }

    /// Make the selected tile the reference, or go back to the plurality
    /// when it already is.
    pub fn toggle_reference(&mut self) {
        let Some(host) = self.selected_result().map(|r| r.host.clone()) else {
            return;
        };
        self.reference_host = if self.reference_host.as_deref() == Some(host.as_str()) {
            None
        } else {
            Some(host)
        };
    }

    /// The run as text, for saving: the command, the summary, then each
    /// host's full output under its heading.
    pub fn capture(&self) -> String {
        let mut out = format!("$ {}\n{}\n", self.command, self.summary());
        for r in &self.results {
            let status = match r.exit {
                Some(code) => format!("exit {code}"),
                None => String::from("timed out"),
            };
            out.push_str(&format!(
                "\n== {} ({status}, {:.1}s)\n{}\n",
                r.host,
                r.elapsed.as_secs_f64(),
                r.output.trim_end()
            ));
        }
        out
    }

    /// The tile under screen cell (`x`, `y`), from the last frame.
    pub fn tile_at(&self, x: u16, y: u16) -> Option<usize> {
        self.tile_rects
            .iter()
            .position(|r| x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height)
    }
}

pub fn render(
    view: &mut FleetView,
    area: Rect,
    buf: &mut Buffer,
    bg: Color,
    theme: crate::theme::Theme,
) {
    view.tile_rects.clear();
    if area.width < 10 || area.height < 4 {
        return;
    }
    let dim = Style::default()
        .fg(theme.ui(Color::Rgb(0x9a, 0xa4, 0xb2)))
        .bg(bg);
    let head = Style::default()
        .fg(theme.ui(Color::Rgb(0xe8, 0xee, 0xf8)))
        .bg(bg)
        .add_modifier(Modifier::BOLD);
    buf.set_stringn(
        area.x + 1,
        area.y,
        format!("$ {}", view.command),
        area.width as usize - 2,
        head,
    );
    let mode = if view.diff_mode {
        "diff"
    } else {
        "full output"
    };
    let reference = match &view.reference_host {
        Some(h) => format!("reference: {h}"),
        None => String::from("reference: most common output"),
    };
    buf.set_stringn(
        area.x + 1,
        area.y + 1,
        format!("{} · {mode} · {reference}", view.summary()),
        area.width as usize - 2,
        dim,
    );
    buf.set_stringn(
        area.x + 1,
        area.y + area.height - 1,
        "←→↑↓ select · d diff · r reference · Enter shell here · s save · Esc close",
        area.width as usize - 2,
        dim,
    );
    let grid = Rect {
        x: area.x,
        y: area.y + 2,
        width: area.width,
        height: area.height.saturating_sub(3),
    };
    let n = view.results.len();
    if n == 0 || grid.height < 3 {
        return;
    }
    let cols = ((grid.width / TILE_MIN_WIDTH).max(1) as usize).min(n);
    let rows = n.div_ceil(cols);
    view.columns = cols;
    let tile_w = grid.width / cols as u16;
    let tile_h = (grid.height / rows as u16).max(3);
    for (i, r) in view.results.iter().enumerate() {
        let (col, row) = ((i % cols) as u16, (i / cols) as u16);
        let rect = Rect {
            x: grid.x + col * tile_w,
            y: grid.y + row * tile_h,
            width: tile_w,
            height: tile_h,
        };
        if rect.y + rect.height > grid.y + grid.height {
            break;
        }
        view.tile_rects.push(rect);
        let state = view.state(r);
        let dot = match state {
            TileState::Same => theme.git_added(),
            TileState::Differs => theme.ui(Color::Rgb(0xe5, 0xc0, 0x7b)),
            TileState::Failed | TileState::TimedOut => theme.git_deleted(),
        };
        let selected = i == view.selected;
        let border = Style::default()
            .fg(if selected {
                theme.accent()
            } else if matches!(state, TileState::Failed | TileState::TimedOut) {
                theme.git_deleted()
            } else {
                theme.ui(Color::Rgb(0x3a, 0x41, 0x50))
            })
            .bg(bg);
        let block = ratatui::widgets::Block::default()
            .borders(ratatui::widgets::Borders::ALL)
            .border_style(border);
        let inner = block.inner(rect);
        ratatui::widgets::Widget::render(block, rect, buf);
        let status = match (state, r.exit) {
            (TileState::TimedOut, _) => String::from("timed out"),
            (_, Some(code)) if code != 0 => format!("exit {code}"),
            _ => String::new(),
        };
        let title = format!(" {} {:.1}s {status}", r.host, r.elapsed.as_secs_f64());
        buf.set_string(
            rect.x + 1,
            rect.y,
            "\u{25cf}",
            Style::default().fg(dot).bg(bg),
        );
        buf.set_stringn(
            rect.x + 2,
            rect.y,
            &title,
            rect.width.saturating_sub(3) as usize,
            border.add_modifier(Modifier::BOLD),
        );
        for (k, line) in view
            .tile_lines(r)
            .iter()
            .take(inner.height as usize)
            .enumerate()
        {
            let fg = if view.diff_mode && line.starts_with("- ") {
                theme.git_deleted()
            } else if view.diff_mode && line.starts_with("+ ") {
                theme.git_added()
            } else {
                theme.ui(Color::Rgb(0xcc, 0xcc, 0xcc))
            };
            buf.set_stringn(
                inner.x,
                inner.y + k as u16,
                line,
                inner.width as usize,
                Style::default().fg(fg).bg(bg),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn r(host: &str, output: &str, exit: Option<i32>) -> HostResult {
        HostResult {
            host: host.into(),
            output: output.into(),
            exit,
            elapsed: Duration::from_millis(200),
        }
    }

    pub fn sample() -> FleetView {
        FleetView::new(
            String::from("uname -r"),
            vec![
                r("a", "6.8.0-45", Some(0)),
                r("b", "6.8.0-45", Some(0)),
                r("c", "6.8.0-31", Some(0)),
                r("slow", "timed out", None),
            ],
        )
    }

    #[test]
    fn tiles_compare_with_the_plurality_or_the_chosen_reference() {
        let mut v = sample();
        let states: Vec<_> = v.results.iter().map(|r| v.state(r)).collect();
        assert_eq!(
            states,
            [
                TileState::Same,
                TileState::Same,
                TileState::Differs,
                TileState::TimedOut
            ]
        );
        assert_eq!(
            v.tile_lines(&v.results[2].clone()),
            ["- 6.8.0-45", "+ 6.8.0-31"]
        );
        assert_eq!(v.summary(), "2 identical · 1 differ · 1 failed");
        // Diff against the first-listed odd host instead.
        v.selected = 2;
        v.toggle_reference();
        assert_eq!(v.state(&v.results[0].clone()), TileState::Differs);
        assert_eq!(v.state(&v.results[2].clone()), TileState::Same);
        v.toggle_reference();
        assert_eq!(
            v.reference_host, None,
            "a second press goes back to the plurality"
        );
        v.diff_mode = false;
        assert_eq!(v.tile_lines(&v.results[2].clone()), ["6.8.0-31"]);
    }

    #[test]
    fn the_grid_draws_one_tile_per_host_and_marks_the_timeout() {
        let mut v = sample();
        let area = Rect::new(0, 0, 120, 20);
        let mut buf = Buffer::empty(area);
        render(
            &mut v,
            area,
            &mut buf,
            Color::Black,
            crate::theme::Theme::default(),
        );
        assert_eq!(v.tile_rects.len(), 4);
        assert_eq!(v.columns, 3);
        let text: String = (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
                    + "\n"
            })
            .collect();
        assert!(text.contains("2 identical · 1 differ · 1 failed"), "{text}");
        assert!(text.contains("slow 0.2s timed out"), "{text}");
        assert!(text.contains("+ 6.8.0-31"), "{text}");
        assert_eq!(
            v.tile_at(v.tile_rects[2].x + 1, v.tile_rects[2].y + 1),
            Some(2)
        );
    }

    #[test]
    fn a_capture_holds_every_hosts_full_output() {
        let c = sample().capture();
        assert!(
            c.starts_with("$ uname -r\n2 identical · 1 differ · 1 failed\n"),
            "{c}"
        );
        assert!(c.contains("== c (exit 0, 0.2s)\n6.8.0-31\n"));
        assert!(c.contains("== slow (timed out, 0.2s)"));
    }
}
