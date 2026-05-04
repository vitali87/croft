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
    let thumb_len = ((viewport_len * track_len as usize) + content_len - 1) / content_len;
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
}
