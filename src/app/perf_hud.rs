use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;

#[derive(Default)]
pub struct PerfHud {
    visible: bool,
    bytes: Option<Arc<AtomicUsize>>,
    last_draw_us: u128,
    last_frame_bytes: usize,
    last_fps: u32,
    last_kps: u32,
    last_ips: u32,
    last_work_us: u128,
    last_poll_us: u128,
}

impl PerfHud {
    pub fn toggle(&mut self) {
        self.visible = !self.visible;
    }

    pub fn attach_counter(&mut self, counter: Arc<AtomicUsize>) {
        self.bytes = Some(counter);
    }

    pub fn record_window(&mut self, fps: u32, kps: u32, ips: u32, work_us: u128, poll_us: u128) {
        self.last_fps = fps;
        self.last_kps = kps;
        self.last_ips = ips;
        self.last_work_us = work_us;
        self.last_poll_us = poll_us;
    }

    pub fn record_draw(&mut self, draw_us: u128) {
        self.last_draw_us = draw_us;
        if let Some(counter) = self.bytes.as_ref() {
            self.last_frame_bytes = counter.swap(0, Ordering::Relaxed);
        }
    }

    pub fn status_span(&self) -> Option<Span<'static>> {
        if !self.visible {
            return None;
        }
        Some(Span::styled(
            format!(
                " ⚡d{}µs w{}µs p{}µs {}ips {}kps ",
                self.last_draw_us,
                self.last_work_us,
                self.last_poll_us,
                self.last_ips,
                self.last_kps
            ),
            Style::default()
                .bg(Color::Rgb(0x7a, 0x1f, 0x1f))
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ))
    }
}
