use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Style},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VerticalScrollbar {
    pub area: Rect,
    pub max_scroll: usize,
    thumb_start: u16,
    thumb_len: u16,
}

pub fn vertical_metrics(
    area: Rect,
    content_len: usize,
    viewport_len: usize,
    scroll: usize,
) -> Option<VerticalScrollbar> {
    if area.width == 0 || area.height == 0 || viewport_len == 0 || content_len <= viewport_len {
        return None;
    }

    // Pattern: proportional range mapping over bounded intervals.
    // Why: scrollbar thumbs map a content scroll range onto a smaller track range.
    // Model: coordinate compression from [0, max_scroll] into [0, track_travel].
    let track_len = area.height;
    let max_scroll = content_len.saturating_sub(viewport_len);
    let thumb_len = (viewport_len * track_len as usize).div_ceil(content_len);
    let thumb_len = thumb_len.clamp(1, track_len as usize) as u16;
    let travel = track_len.saturating_sub(thumb_len);
    let thumb_start = if max_scroll == 0 || travel == 0 {
        0
    } else {
        ((scroll.min(max_scroll) * travel as usize + max_scroll / 2) / max_scroll) as u16
    };

    Some(VerticalScrollbar {
        area,
        max_scroll,
        thumb_start,
        thumb_len,
    })
}

pub fn scroll_for_y(metrics: VerticalScrollbar, y: u16) -> usize {
    if metrics.max_scroll == 0 || metrics.area.height == 0 {
        return 0;
    }
    let track_row = y
        .saturating_sub(metrics.area.y)
        .min(metrics.area.height.saturating_sub(1));
    let travel = metrics.area.height.saturating_sub(metrics.thumb_len);
    if travel == 0 {
        return 0;
    }
    let thumb_top = track_row.saturating_sub(metrics.thumb_len / 2).min(travel);
    (thumb_top as usize * metrics.max_scroll + travel as usize / 2) / travel as usize
}

pub fn render_vertical(buf: &mut Buffer, metrics: VerticalScrollbar, focused: bool) {
    let track = Style::default().bg(Color::Rgb(0x2b, 0x33, 0x46));
    let thumb_color = if focused {
        Color::Rgb(0x4e, 0x9a, 0xff)
    } else {
        Color::Rgb(0x6b, 0x73, 0x86)
    };
    let thumb = Style::default().bg(thumb_color);
    for row in 0..metrics.area.height {
        let is_thumb = row >= metrics.thumb_start
            && row < metrics.thumb_start.saturating_add(metrics.thumb_len);
        let cell = &mut buf[(metrics.area.x, metrics.area.y + row)];
        cell.set_symbol(" ");
        if is_thumb {
            cell.set_style(thumb);
        } else {
            cell.set_style(track);
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HorizontalScrollbar {
    pub area: Rect,
    pub max_scroll: usize,
    thumb_start: u16,
    thumb_len: u16,
}

/// Mirror of [`vertical_metrics`] for a one-row track laid along the X axis.
/// `content_len`/`viewport_len`/`scroll` are all in CHARACTER columns so the
/// editor's `scroll_col` maps straight onto the thumb. Returns `None` when the
/// content fits, so callers can skip reserving the bottom row entirely.
pub fn horizontal_metrics(
    area: Rect,
    content_len: usize,
    viewport_len: usize,
    scroll: usize,
) -> Option<HorizontalScrollbar> {
    if area.width == 0 || area.height == 0 || viewport_len == 0 || content_len <= viewport_len {
        return None;
    }

    let track_len = area.width;
    let max_scroll = content_len.saturating_sub(viewport_len);
    let thumb_len = (viewport_len * track_len as usize).div_ceil(content_len);
    let thumb_len = thumb_len.clamp(1, track_len as usize) as u16;
    let travel = track_len.saturating_sub(thumb_len);
    let thumb_start = if max_scroll == 0 || travel == 0 {
        0
    } else {
        ((scroll.min(max_scroll) * travel as usize + max_scroll / 2) / max_scroll) as u16
    };

    Some(HorizontalScrollbar {
        area,
        max_scroll,
        thumb_start,
        thumb_len,
    })
}

pub fn scroll_for_x(metrics: HorizontalScrollbar, x: u16) -> usize {
    if metrics.max_scroll == 0 || metrics.area.width == 0 {
        return 0;
    }
    let track_col = x
        .saturating_sub(metrics.area.x)
        .min(metrics.area.width.saturating_sub(1));
    let travel = metrics.area.width.saturating_sub(metrics.thumb_len);
    if travel == 0 {
        return 0;
    }
    let thumb_left = track_col.saturating_sub(metrics.thumb_len / 2).min(travel);
    (thumb_left as usize * metrics.max_scroll + travel as usize / 2) / travel as usize
}

pub fn render_horizontal(buf: &mut Buffer, metrics: HorizontalScrollbar, focused: bool) {
    // Paint a lower-half block (`▄`) coloured via the foreground rather than a
    // full-cell background. Terminal cells are about twice as tall as they are
    // wide, so a full-row bar reads twice as thick as the 1-column vertical
    // bar; the half block matches its visual weight and hugs the bottom edge.
    let track_color = Color::Rgb(0x2b, 0x33, 0x46);
    let thumb_color = if focused {
        Color::Rgb(0x4e, 0x9a, 0xff)
    } else {
        Color::Rgb(0x6b, 0x73, 0x86)
    };
    for col in 0..metrics.area.width {
        let is_thumb = col >= metrics.thumb_start
            && col < metrics.thumb_start.saturating_add(metrics.thumb_len);
        let color = if is_thumb { thumb_color } else { track_color };
        let cell = &mut buf[(metrics.area.x + col, metrics.area.y)];
        cell.set_symbol("\u{2584}");
        cell.set_style(Style::default().fg(color));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vertical_metrics_hidden_without_overflow() {
        let area = Rect {
            x: 0,
            y: 0,
            width: 1,
            height: 10,
        };
        assert!(vertical_metrics(area, 10, 10, 0).is_none());
        assert!(vertical_metrics(area, 5, 10, 0).is_none());
    }

    #[test]
    fn scroll_for_y_maps_track_extremes_to_scroll_extremes() {
        let area = Rect {
            x: 0,
            y: 5,
            width: 1,
            height: 10,
        };
        let metrics = vertical_metrics(area, 100, 20, 0).unwrap();
        assert_eq!(scroll_for_y(metrics, 5), 0);
        assert_eq!(scroll_for_y(metrics, 14), 80);
    }

    #[test]
    fn render_vertical_uses_full_cell_backgrounds() {
        let area = Rect {
            x: 2,
            y: 0,
            width: 1,
            height: 5,
        };
        let mut buf = Buffer::empty(Rect {
            x: 0,
            y: 0,
            width: 4,
            height: 5,
        });
        let metrics = vertical_metrics(area, 100, 20, 0).unwrap();
        render_vertical(&mut buf, metrics, true);
        assert_eq!(buf[(2, 0)].symbol(), " ");
        assert_eq!(buf[(2, 0)].bg, Color::Rgb(0x4e, 0x9a, 0xff));
        assert_eq!(buf[(2, 4)].bg, Color::Rgb(0x2b, 0x33, 0x46));
    }

    #[test]
    fn horizontal_metrics_hidden_without_overflow() {
        let area = Rect {
            x: 0,
            y: 0,
            width: 10,
            height: 1,
        };
        assert!(horizontal_metrics(area, 10, 10, 0).is_none());
        assert!(horizontal_metrics(area, 5, 10, 0).is_none());
    }

    #[test]
    fn scroll_for_x_maps_track_extremes_to_scroll_extremes() {
        let area = Rect {
            x: 5,
            y: 0,
            width: 10,
            height: 1,
        };
        let metrics = horizontal_metrics(area, 100, 20, 0).unwrap();
        assert_eq!(scroll_for_x(metrics, 5), 0);
        assert_eq!(scroll_for_x(metrics, 14), 80);
    }

    #[test]
    fn render_horizontal_uses_half_height_block() {
        let area = Rect {
            x: 0,
            y: 2,
            width: 5,
            height: 1,
        };
        let mut buf = Buffer::empty(Rect {
            x: 0,
            y: 0,
            width: 5,
            height: 3,
        });
        let metrics = horizontal_metrics(area, 100, 20, 0).unwrap();
        render_horizontal(&mut buf, metrics, true);
        // Half-height block coloured via the foreground keeps the bar thin.
        assert_eq!(buf[(0, 2)].symbol(), "\u{2584}");
        assert_eq!(buf[(0, 2)].fg, Color::Rgb(0x4e, 0x9a, 0xff));
        assert_eq!(buf[(4, 2)].fg, Color::Rgb(0x2b, 0x33, 0x46));
    }
}
