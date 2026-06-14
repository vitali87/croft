use std::collections::VecDeque;

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    symbols::Marker,
    text::{Line, Span},
    widgets::{
        Block, Borders, Paragraph, Widget,
        canvas::{Canvas, Line as CanvasLine},
    },
};

use crate::sysmon::SystemSample;

const HISTORY_LEN: usize = 240;

const COLOR_HEADER: Color = Color::Rgb(0xE8, 0xEE, 0xF8);
const COLOR_DIM: Color = Color::Rgb(0x60, 0x68, 0x78);
const COLOR_CPU: Color = Color::Rgb(0x5E, 0xC4, 0xFF);
const COLOR_MEM: Color = Color::Rgb(0xC7, 0x8C, 0xFF);
const COLOR_NET: Color = Color::Rgb(0x57, 0xE3, 0x89);
const COLOR_DISK: Color = Color::Rgb(0xF9, 0xE6, 0x4F);
const COLOR_TEMP: Color = Color::Rgb(0xFF, 0x6F, 0x6F);

pub const COLLAPSED_HEIGHT: u16 = 2;
pub const EXPANDED_HEIGHT: u16 = 7;

const MIN_ACTIVITY_ROWS: u16 = 14;

/// Left indent for panel content, matching the `Borders::ALL` inset of the
/// activity widget rendered directly above so the two left edges align.
const CONTENT_INDENT: u16 = 1;

pub struct SystemPanel {
    user_override: Option<bool>,
    pub hidden: bool,
    pub last_effective_collapsed: bool,
    pub latest: Option<SystemSample>,
    pub last_area: Rect,
    pub last_header_row: u16,
    pub last_header_x: u16,
    pub last_header_w: u16,
    cpu: VecDeque<u64>,
    mem: VecDeque<u64>,
    net: VecDeque<u64>,
    disk: VecDeque<u64>,
    temp: VecDeque<u64>,
}

impl SystemPanel {
    pub fn new() -> Self {
        Self {
            // Collapsed by default (VS Code parity): show only the SYSTEM header
            // until the user expands it, so the live metrics never distract and
            // sampling stays idle until asked for.
            user_override: Some(true),
            hidden: false,
            last_effective_collapsed: true,
            latest: None,
            last_area: Rect::default(),
            last_header_row: 0,
            last_header_x: 0,
            last_header_w: 0,
            cpu: VecDeque::with_capacity(HISTORY_LEN),
            mem: VecDeque::with_capacity(HISTORY_LEN),
            net: VecDeque::with_capacity(HISTORY_LEN),
            disk: VecDeque::with_capacity(HISTORY_LEN),
            temp: VecDeque::with_capacity(HISTORY_LEN),
        }
    }

    pub fn set_hidden(&mut self, h: bool) {
        self.hidden = h;
    }

    pub fn desired_height(&self, available: u16) -> u16 {
        if self.hidden || available < COLLAPSED_HEIGHT {
            return 0;
        }
        let effective_collapsed = self
            .user_override
            .unwrap_or(available < EXPANDED_HEIGHT.saturating_add(MIN_ACTIVITY_ROWS));
        let want = if effective_collapsed {
            COLLAPSED_HEIGHT
        } else {
            EXPANDED_HEIGHT
        };
        match self.user_override {
            Some(_) => want.min(available.saturating_sub(1)),
            None => {
                if available >= want.saturating_add(MIN_ACTIVITY_ROWS) {
                    want
                } else {
                    0
                }
            }
        }
    }

    pub fn apply_sample(&mut self, s: SystemSample) {
        push(&mut self.cpu, s.cpu_pct as u64);
        push(&mut self.mem, s.mem_pct as u64);
        push(&mut self.net, s.net_bps);
        push(&mut self.disk, s.disk_pct as u64);
        push(&mut self.temp, s.temp_c.unwrap_or(0) as u64);
        self.latest = Some(s);
    }

    pub fn toggle_collapse(&mut self) {
        self.user_override = Some(!self.last_effective_collapsed);
    }

    pub fn hit_header(&self, x: u16, y: u16) -> bool {
        y == self.last_header_row
            && x >= self.last_header_x
            && x < self.last_header_x.saturating_add(self.last_header_w)
    }
}

impl Default for SystemPanel {
    fn default() -> Self {
        Self::new()
    }
}

fn push(buf: &mut VecDeque<u64>, v: u64) {
    if buf.len() == HISTORY_LEN {
        buf.pop_front();
    }
    buf.push_back(v);
}

fn fmt_bps(bps: u64) -> String {
    const K: f64 = 1024.0;
    const M: f64 = K * 1024.0;
    const G: f64 = M * 1024.0;
    let b = bps as f64;
    if b >= G {
        format!("{:.1} GB/s", b / G)
    } else if b >= M {
        format!("{:.1} MB/s", b / M)
    } else if b >= K {
        format!("{:.1} KB/s", b / K)
    } else {
        format!("{} B/s", bps)
    }
}

fn fmt_temp(t: Option<u8>) -> String {
    match t {
        Some(v) => format!("{}°C", v),
        None => String::from("--°C"),
    }
}

impl Widget for &mut SystemPanel {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let block = Block::default()
            .borders(Borders::BOTTOM)
            .border_style(Style::default().fg(COLOR_DIM));
        let inner = block.inner(area);
        block.render(area, buf);
        self.last_area = area;

        if inner.height == 0 || inner.width == 0 {
            return;
        }

        // The activity widget directly above is drawn with `Borders::ALL`,
        // so its content sits one column right of the shared column edge.
        // This panel only has a bottom separator, so indent its content by
        // the same column to keep the labels and sparklines aligned with the
        // box above. The bottom border (rendered by `block`) keeps full width.
        let inner = Rect {
            x: inner.x + CONTENT_INDENT.min(inner.width),
            width: inner.width.saturating_sub(CONTENT_INDENT),
            ..inner
        };

        let can_show_expanded = inner.height > 5;
        let effective_collapsed = self.user_override.unwrap_or(!can_show_expanded);
        let effective_collapsed = effective_collapsed || !can_show_expanded;
        self.last_effective_collapsed = effective_collapsed;
        let chevron = if effective_collapsed { '▸' } else { '▾' };
        let header_y = inner.y;

        if effective_collapsed {
            let mut spans = vec![
                Span::styled(format!("{chevron} "), Style::default().fg(COLOR_DIM)),
                Span::styled(
                    "SYSTEM ",
                    Style::default()
                        .fg(COLOR_HEADER)
                        .add_modifier(Modifier::BOLD),
                ),
            ];
            spans.extend(collapsed_summary(self.latest.as_ref()));
            let area1 = Rect {
                x: inner.x,
                y: header_y,
                width: inner.width,
                height: 1,
            };
            Paragraph::new(Line::from(spans)).render(area1, buf);
            self.last_header_row = header_y;
            self.last_header_x = inner.x;
            self.last_header_w = inner.width;
            return;
        }

        let header = Paragraph::new(Line::from(vec![
            Span::styled(format!("{chevron} "), Style::default().fg(COLOR_DIM)),
            Span::styled(
                "SYSTEM",
                Style::default()
                    .fg(COLOR_HEADER)
                    .add_modifier(Modifier::BOLD),
            ),
        ]));
        let header_area = Rect {
            x: inner.x,
            y: header_y,
            width: inner.width,
            height: 1,
        };
        header.render(header_area, buf);
        self.last_header_row = header_y;
        self.last_header_x = inner.x;
        self.last_header_w = inner.width;

        let latest = self.latest;
        let net_dyn_max = self.net.iter().copied().max().unwrap_or(0).max(1);
        let rows: [(Color, &str, &VecDeque<u64>, u64, String); 5] = [
            (
                COLOR_CPU,
                "CPU",
                &self.cpu,
                100,
                latest
                    .map(|s| format!("{}%", s.cpu_pct))
                    .unwrap_or_else(|| "--".into()),
            ),
            (
                COLOR_MEM,
                "MEM",
                &self.mem,
                100,
                latest
                    .map(|s| format!("{}%", s.mem_pct))
                    .unwrap_or_else(|| "--".into()),
            ),
            (
                COLOR_NET,
                "NET",
                &self.net,
                net_dyn_max,
                latest
                    .map(|s| fmt_bps(s.net_bps))
                    .unwrap_or_else(|| "--".into()),
            ),
            (
                COLOR_DISK,
                "DISK",
                &self.disk,
                100,
                latest
                    .map(|s| format!("{}%", s.disk_pct))
                    .unwrap_or_else(|| "--".into()),
            ),
            (
                COLOR_TEMP,
                "TEMP",
                &self.temp,
                100,
                latest
                    .map(|s| fmt_temp(s.temp_c))
                    .unwrap_or_else(|| "--".into()),
            ),
        ];

        for (y, (color, label, hist, scale_max, value)) in (header_y + 1..).zip(rows) {
            if y >= inner.y + inner.height {
                break;
            }
            render_metric_row(
                buf,
                Rect {
                    x: inner.x,
                    y,
                    width: inner.width,
                    height: 1,
                },
                color,
                label,
                hist,
                scale_max,
                &value,
            );
        }
    }
}

fn render_metric_row(
    buf: &mut Buffer,
    area: Rect,
    color: Color,
    label: &str,
    hist: &VecDeque<u64>,
    scale_max: u64,
    value: &str,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let bold = Style::default().fg(color).add_modifier(Modifier::BOLD);
    let label_w = label.chars().count() as u16 + 1;
    let value_w = value.chars().count() as u16 + 1;

    if label_w + value_w >= area.width {
        let mut spans = vec![Span::styled(label.to_string(), bold)];
        if (label.chars().count() as u16) + (value.chars().count() as u16) < area.width {
            let pad = area.width - label.chars().count() as u16 - value.chars().count() as u16;
            spans.push(Span::raw(" ".repeat(pad as usize)));
            spans.push(Span::styled(value.to_string(), bold));
        }
        Paragraph::new(Line::from(spans)).render(area, buf);
        return;
    }

    buf.set_string(area.x, area.y, format!("{label} "), bold);
    buf.set_string(
        area.x + area.width - (value.chars().count() as u16) - 1,
        area.y,
        format!(" {value}"),
        bold,
    );

    let spark_x = area.x + label_w;
    let spark_w = area.width - label_w - value_w;
    render_inline_spark(buf, spark_x, area.y, spark_w, hist, scale_max, color);
}

fn render_inline_spark(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    width: u16,
    hist: &VecDeque<u64>,
    scale_max: u64,
    color: Color,
) {
    if width < 2 || hist.len() < 2 {
        return;
    }
    let denom = scale_max.max(1) as f64;
    let target_pixels = (width as usize).saturating_mul(2);
    let visible = hist.len().min(target_pixels);
    let start = hist.len() - visible;
    let slice: Vec<u64> = hist.iter().skip(start).copied().collect();
    let bounds_max = (target_pixels.saturating_sub(1).max(1)) as f64;

    let area = Rect {
        x,
        y,
        width,
        height: 1,
    };
    Canvas::default()
        .marker(Marker::Braille)
        .background_color(Color::Reset)
        .x_bounds([0.0, bounds_max])
        .y_bounds([0.0, denom])
        .paint(|ctx| {
            for i in 0..(slice.len() - 1) {
                ctx.draw(&CanvasLine {
                    x1: i as f64,
                    y1: slice[i] as f64,
                    x2: (i + 1) as f64,
                    y2: slice[i + 1] as f64,
                    color,
                });
            }
        })
        .render(area, buf);
}

fn collapsed_summary(latest: Option<&SystemSample>) -> Vec<Span<'static>> {
    let Some(s) = latest else {
        return vec![Span::styled("…", Style::default().fg(COLOR_DIM))];
    };
    vec![
        Span::styled(format!("{}%", s.cpu_pct), Style::default().fg(COLOR_CPU)),
        Span::raw(" "),
        Span::styled(format!("{}%", s.mem_pct), Style::default().fg(COLOR_MEM)),
        Span::raw(" "),
        Span::styled(fmt_bps(s.net_bps), Style::default().fg(COLOR_NET)),
        Span::raw(" "),
        Span::styled(format!("{}%", s.disk_pct), Style::default().fg(COLOR_DISK)),
        Span::raw(" "),
        Span::styled(fmt_temp(s.temp_c), Style::default().fg(COLOR_TEMP)),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_toggle_flips_relative_to_last_rendered_state() {
        let mut p = SystemPanel::new();
        p.last_effective_collapsed = false;
        p.toggle_collapse();
        assert_eq!(p.desired_height(60), COLLAPSED_HEIGHT);
        p.last_effective_collapsed = true;
        p.toggle_collapse();
        assert_eq!(p.desired_height(60), EXPANDED_HEIGHT);
    }

    #[test]
    fn collapsed_by_default_even_with_ample_room() {
        // Matches VS Code: the SYSTEM section starts collapsed (just its header)
        // so the metrics don't distract; the user expands it from the header.
        let p = SystemPanel::new();
        assert_eq!(p.desired_height(60), COLLAPSED_HEIGHT);
    }

    #[test]
    fn desired_height_auto_downgrades_when_sidebar_short() {
        let mut p = SystemPanel::new();
        // Clear the collapsed-by-default override to exercise the auto sizing.
        p.user_override = None;
        assert_eq!(p.desired_height(60), EXPANDED_HEIGHT);
        assert_eq!(p.desired_height(20), COLLAPSED_HEIGHT);
        assert_eq!(p.desired_height(10), 0);
    }

    #[test]
    fn user_override_clips_to_available_when_needed() {
        let mut p = SystemPanel::new();
        p.last_effective_collapsed = true;
        p.toggle_collapse();
        let h = p.desired_height(20);
        assert!(
            h > COLLAPSED_HEIGHT,
            "user-expanded must beat auto-collapse, got {h}"
        );
    }

    #[test]
    fn set_hidden_forces_zero_height() {
        let mut p = SystemPanel::new();
        assert!(p.desired_height(60) > 0);
        p.set_hidden(true);
        assert_eq!(p.desired_height(60), 0);
    }

    #[test]
    fn apply_sample_caps_history() {
        let mut p = SystemPanel::new();
        for i in 0..(HISTORY_LEN as u64 + 10) {
            p.apply_sample(SystemSample {
                cpu_pct: (i % 100) as u8,
                mem_pct: 0,
                net_bps: 0,
                disk_pct: 0,
                temp_c: None,
            });
        }
        assert_eq!(p.cpu.len(), HISTORY_LEN);
        assert_eq!(*p.cpu.back().unwrap(), (HISTORY_LEN as u64 + 9) % 100);
    }

    #[test]
    fn fmt_bps_picks_units() {
        assert_eq!(fmt_bps(500), "500 B/s");
        assert_eq!(fmt_bps(2150), "2.1 KB/s");
        assert!(fmt_bps(5 * 1024 * 1024).ends_with("MB/s"));
    }

    #[test]
    fn fmt_temp_handles_missing() {
        assert_eq!(fmt_temp(Some(48)), "48°C");
        assert_eq!(fmt_temp(None), "--°C");
    }

    #[test]
    fn temp_row_sits_just_above_bottom_border() {
        let mut p = SystemPanel::new();
        p.user_override = Some(false); // expand to exercise the full layout
        p.apply_sample(SystemSample {
            cpu_pct: 10,
            mem_pct: 20,
            net_bps: 0,
            disk_pct: 30,
            temp_c: Some(52),
        });
        let area = Rect {
            x: 0,
            y: 5,
            width: 30,
            height: EXPANDED_HEIGHT,
        };
        let mut buf = Buffer::empty(area);
        (&mut p).render(area, &mut buf);
        let bottom_row = area.y + area.height - 1;
        let temp_row = bottom_row - 1;
        let mut temp_text = String::new();
        let mut border_text = String::new();
        for x in 0..area.width {
            temp_text.push(buf[(x, temp_row)].symbol().chars().next().unwrap_or(' '));
            border_text.push(buf[(x, bottom_row)].symbol().chars().next().unwrap_or(' '));
        }
        assert!(
            temp_text.contains("TEMP") && temp_text.contains("52°C"),
            "temp row was {temp_text:?}"
        );
        assert!(
            border_text.chars().any(|c| c == '─'),
            "bottom border row was {border_text:?}"
        );
    }

    #[test]
    fn content_is_indented_to_align_with_box_above() {
        let mut p = SystemPanel::new();
        p.user_override = Some(false); // expand to exercise the full layout
        p.apply_sample(SystemSample {
            cpu_pct: 10,
            mem_pct: 20,
            net_bps: 0,
            disk_pct: 30,
            temp_c: Some(52),
        });
        let area = Rect {
            x: 0,
            y: 0,
            width: 30,
            height: EXPANDED_HEIGHT,
        };
        let mut buf = Buffer::empty(area);
        (&mut p).render(area, &mut buf);
        // The leftmost column stays blank; content begins one column in,
        // matching the bordered activity widget rendered above.
        let header_row = area.y;
        assert_eq!(buf[(0, header_row)].symbol(), " ");
        assert_eq!(buf[(CONTENT_INDENT, header_row)].symbol(), "▾");
    }

    #[test]
    fn hit_header_uses_recorded_row() {
        let mut p = SystemPanel::new();
        p.last_header_row = 5;
        p.last_header_x = 1;
        p.last_header_w = 30;
        assert!(p.hit_header(10, 5));
        assert!(!p.hit_header(10, 4));
        assert!(!p.hit_header(0, 5));
        assert!(!p.hit_header(40, 5));
    }
}
