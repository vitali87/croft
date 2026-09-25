//! Painting the SARIF viewer tab: a header, the tab strip with level chips
//! and the filter box, the grouped result list beside a details pane, and a
//! key hint row. Frame truth for the list is written back into the view so
//! mouse clicks hit the rows the user sees.

use super::semantics::{BaselineState, Level, SuppressionState};
use super::view::{Row, SarifView, Tab};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use std::path::Path;

/// VS Code's `problemsErrorIcon` / `WarningIcon` / `InfoIcon` foregrounds.
pub fn level_color(level: Level) -> Color {
    match level {
        Level::Error => Color::Rgb(0xf1, 0x4c, 0x4c),
        Level::Warning => Color::Rgb(0xcc, 0xa7, 0x00),
        Level::Note => Color::Rgb(0x37, 0x94, 0xff),
        Level::None => Color::Rgb(0x8b, 0x94, 0x9e),
    }
}

pub fn level_glyph(level: Level) -> &'static str {
    match level {
        Level::Error => "●",
        Level::Warning => "▲",
        Level::Note => "◆",
        Level::None => "○",
    }
}

fn baseline_label(b: BaselineState) -> &'static str {
    match b {
        BaselineState::New => "new",
        BaselineState::Unchanged => "unchanged",
        BaselineState::Updated => "updated",
        BaselineState::Absent => "absent",
        BaselineState::Unspecified => "no baseline",
    }
}

fn suppression_label(s: SuppressionState) -> &'static str {
    match s {
        SuppressionState::Unknown | SuppressionState::NotSuppressed => "not suppressed",
        SuppressionState::UnderReview => "suppression under review",
        SuppressionState::Suppressed => "suppressed",
    }
}

pub fn render(
    view: &mut SarifView,
    path: Option<&Path>,
    inner: Rect,
    buf: &mut Buffer,
    bg: Color,
    theme: crate::theme::Theme,
) {
    let bg_style = Style::default().bg(bg);
    for y in inner.y..inner.y + inner.height {
        for x in inner.x..inner.x + inner.width {
            buf[(x, y)].set_style(bg_style);
            buf[(x, y)].set_symbol(" ");
        }
    }
    view.rows_top = 0;
    view.rows_visible = 0;
    view.list_width = 0;
    if inner.height < 5 || inner.width < 30 {
        return;
    }
    let w = inner.width as usize;
    let text = Style::default().fg(theme.ui(Color::Gray)).bg(bg);
    let dim = Style::default().fg(Color::DarkGray).bg(bg);

    // Header.
    let name = path
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| String::from("(sarif)"));
    let n = view.entries.len();
    let tools = view.tools().join(", ");
    let header = format!(
        " {name} · {} · {n} result{} ",
        if tools.is_empty() { "SARIF" } else { &tools },
        if n == 1 { "" } else { "s" }
    );
    buf.set_stringn(
        inner.x,
        inner.y,
        &header,
        w,
        Style::default()
            .fg(Color::White)
            .bg(theme.ui(Color::Rgb(0x09, 0x4d, 0x77)))
            .add_modifier(Modifier::BOLD),
    );

    // Tab strip, level chips, filter box.
    let bar_y = inner.y + 1;
    let mut x = inner.x + 1;
    for (tab, label) in [
        (Tab::Locations, "Locations"),
        (Tab::Rules, "Rules"),
        (Tab::Logs, "Logs"),
    ] {
        let style = if view.tab == tab {
            Style::default()
                .fg(theme.accent_contrast_fg())
                .bg(theme.accent())
                .add_modifier(Modifier::BOLD)
        } else {
            text
        };
        let cell = format!(" {label} ");
        if (x - inner.x) as usize + cell.chars().count() >= w {
            break;
        }
        buf.set_stringn(x, bar_y, &cell, w, style);
        x += cell.chars().count() as u16 + 1;
    }
    x += 1;
    for (level, count) in view.level_counts() {
        let hidden = view.filters.hidden_levels.contains(&level);
        let chip = format!("{} {count} ", level_glyph(level));
        if (x - inner.x) as usize + chip.chars().count() >= w {
            break;
        }
        let style = if hidden {
            dim.add_modifier(Modifier::CROSSED_OUT)
        } else {
            Style::default().fg(level_color(level)).bg(bg)
        };
        buf.set_stringn(x, bar_y, &chip, w, style);
        x += chip.chars().count() as u16 + 1;
    }
    let filter = if view.editing_query {
        format!("/ {}▏", view.query_text)
    } else if view.query_text.is_empty() {
        String::from("/ filter")
    } else {
        format!("/ {}", view.query_text)
    };
    let used = (x - inner.x) as usize;
    if used + 4 < w {
        let style = if view.editing_query || !view.query_text.is_empty() {
            text.add_modifier(Modifier::BOLD)
        } else {
            dim
        };
        buf.set_stringn(x + 1, bar_y, &filter, w - used - 1, style);
    }

    // Body: list, and details beside it when there is room.
    let body_top = inner.y + 2;
    let body_h = inner.height - 3;
    let (list_w, detail_x) = if inner.width >= 90 {
        let lw = inner.width * 58 / 100;
        (lw, Some(inner.x + lw + 1))
    } else {
        (inner.width, None)
    };
    view.rows_top = body_top;
    view.rows_visible = body_h;
    view.list_x = inner.x;
    view.list_width = list_w;

    let rows = view.rows();
    if !rows.is_empty() {
        view.selected = view.selected.min(rows.len() - 1);
    }
    let visible = body_h as usize;
    if view.selected < view.scroll {
        view.scroll = view.selected;
    } else if view.selected >= view.scroll + visible {
        view.scroll = view.selected + 1 - visible;
    }
    view.scroll = view
        .scroll
        .min(rows.len().saturating_sub(visible.min(rows.len())));

    if rows.is_empty() {
        let msg = if view.entries.is_empty() {
            " No results in this log."
        } else {
            " No results match the filters · x: clear filters"
        };
        buf.set_stringn(inner.x, body_top, msg, list_w as usize, dim);
    }
    for (r, row) in rows.iter().enumerate().skip(view.scroll).take(visible) {
        let y = body_top + (r - view.scroll) as u16;
        let selected = r == view.selected;
        let sel_style = Style::default()
            .fg(theme.accent_contrast_fg())
            .bg(theme.accent());
        match row {
            Row::Group {
                label,
                count,
                collapsed,
                ..
            } => {
                let chevron = if *collapsed {
                    crate::icons::CHEVRON_CLOSED
                } else {
                    crate::icons::CHEVRON_OPEN
                };
                let count_s = format!(" {count} ");
                let label_w = (list_w as usize).saturating_sub(count_s.len() + 3);
                let line = format!("{chevron} {label:<label_w$}{count_s}");
                let style = if selected {
                    sel_style.add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                        .fg(theme.ui(Color::White))
                        .bg(bg)
                        .add_modifier(Modifier::BOLD)
                };
                buf.set_stringn(inner.x, y, &line, list_w as usize, style);
            }
            Row::Item { entry } => {
                let e = &view.entries[*entry];
                let pos = if e.line > 0 {
                    format!("{}:{}", e.line, e.column.max(1))
                } else {
                    String::from("—")
                };
                let base = if selected { sel_style } else { text };
                buf.set_stringn(inner.x, y, "   ", list_w as usize, base);
                let glyph_style = if selected {
                    sel_style
                } else {
                    Style::default().fg(level_color(e.level)).bg(bg)
                };
                buf.set_stringn(inner.x + 2, y, level_glyph(e.level), 1, glyph_style);
                let rest = format!(" {pos:<8} {}", e.message);
                let rest_w = (list_w as usize).saturating_sub(3);
                let padded = format!("{rest:<rest_w$}");
                buf.set_stringn(inner.x + 3, y, &padded, rest_w, base);
            }
        }
    }

    if let Some(dx) = detail_x {
        for y in body_top..body_top + body_h {
            buf.set_stringn(dx - 1, y, "│", 1, dim);
        }
        let dw = (inner.x + inner.width).saturating_sub(dx + 1) as usize;
        render_details(view, dx + 1, body_top, dw, body_h, buf, text, dim, bg);
    }

    let hint = if view.editing_query {
        " type to filter · terms AND · a|b OR · -x exclude · rule: file: level: tag: tool: msg: · Enter done · Esc clear "
    } else {
        " ↑↓ move · ←→ fold · Enter open · / filter · Tab view · s sort · 1-4 levels · u suppressed · a absent · x clear "
    };
    buf.set_stringn(
        inner.x,
        inner.y + inner.height - 1,
        format!("{hint:<w$}"),
        w,
        Style::default()
            .fg(theme.ui(Color::Gray))
            .bg(theme.ui(Color::Rgb(0x07, 0x33, 0x55))),
    );
}

#[allow(clippy::too_many_arguments)]
fn render_details(
    view: &SarifView,
    x: u16,
    top: u16,
    w: usize,
    h: u16,
    buf: &mut Buffer,
    text: Style,
    dim: Style,
    bg: Color,
) {
    if w < 10 {
        return;
    }
    let Some(e) = view.selected_entry() else {
        buf.set_stringn(x, top, "Select a result to see its details.", w, dim);
        return;
    };
    let mut lines: Vec<(String, Style)> = Vec::new();
    let rule = if e.rule_name.is_empty() {
        e.rule_id.clone()
    } else {
        format!("{} · {}", e.rule_id, e.rule_name)
    };
    lines.push((rule, text.add_modifier(Modifier::BOLD)));
    lines.push((
        format!(
            "{} · {} · {}",
            e.level.as_str(),
            baseline_label(e.baseline),
            suppression_label(e.suppression)
        ),
        Style::default().fg(level_color(e.level)).bg(bg),
    ));
    lines.push((String::new(), text));
    for wrapped in wrap(&e.message, w) {
        lines.push((wrapped, text));
    }
    lines.push((String::new(), text));
    if !e.file.is_empty() {
        let at = if e.line > 0 {
            format!("at {}:{}:{}", e.file, e.line, e.column.max(1))
        } else {
            format!("at {}", e.file)
        };
        lines.push((at, text));
    }
    if !e.tool.is_empty() {
        lines.push((format!("tool {}", e.tool), dim));
    }
    if !e.tags.is_empty() {
        lines.push((format!("tags {}", e.tags.join(", ")), dim));
    }
    for (i, (line, style)) in lines.into_iter().take(h as usize).enumerate() {
        buf.set_stringn(x, top + i as u16, &line, w, style);
    }
}

/// Greedy word wrap to `width` columns; a word longer than a line is split.
fn wrap(s: &str, width: usize) -> Vec<String> {
    let mut out = Vec::new();
    for para in s.lines() {
        let mut line = String::new();
        for word in para.split_whitespace() {
            let need = line.chars().count() + usize::from(!line.is_empty()) + word.chars().count();
            if need > width && !line.is_empty() {
                out.push(std::mem::take(&mut line));
            }
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(word);
            while line.chars().count() > width {
                let head: String = line.chars().take(width).collect();
                let tail: String = line.chars().skip(width).collect();
                out.push(head);
                line = tail;
            }
        }
        out.push(line);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrap_breaks_on_words_and_splits_long_ones() {
        assert_eq!(wrap("aa bb cc", 5), vec!["aa bb", "cc"]);
        assert_eq!(wrap("abcdefgh", 3), vec!["abc", "def", "gh"]);
        assert_eq!(wrap("one\ntwo", 10), vec!["one", "two"]);
    }
}
