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
    /// Memory readout (#694), sampled only while the HUD shows: the
    /// process's resident set, and what terminal rewind holds of its budget.
    memory: Option<MemorySample>,
}

/// One reading of where croft's memory is, for the HUD.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct MemorySample {
    /// `VmRSS` in KiB, where the platform has `/proc` to read it from.
    rss_kb: Option<u64>,
    rewind_held: usize,
    rewind_total: usize,
    rewind_panes: usize,
}

impl MemorySample {
    fn take() -> Self {
        let budget = crate::rewind::budget();
        Self {
            rss_kb: std::fs::read_to_string("/proc/self/status")
                .ok()
                .as_deref()
                .and_then(parse_vm_rss_kb),
            rewind_held: budget.held_bytes(),
            rewind_total: budget.total(),
            rewind_panes: budget.pane_count(),
        }
    }

    fn label(&self) -> String {
        const MIB: usize = 1024 * 1024;
        let rewind = format!(
            "rewind {}/{}M×{}",
            self.rewind_held.div_ceil(MIB),
            self.rewind_total / MIB,
            self.rewind_panes
        );
        match self.rss_kb {
            Some(kb) => format!("rss {}M {rewind}", kb / 1024),
            None => rewind,
        }
    }
}

/// The `VmRSS:` figure from a `/proc/<pid>/status` document, in KiB.
fn parse_vm_rss_kb(status: &str) -> Option<u64> {
    status
        .lines()
        .find_map(|l| l.strip_prefix("VmRSS:"))?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

impl PerfHud {
    pub fn toggle(&mut self) {
        self.visible = !self.visible;
        // Sample at once rather than showing nothing until the next window.
        self.memory = self.visible.then(MemorySample::take);
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
        if self.visible {
            self.memory = Some(MemorySample::take());
        }
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
        let memory = self
            .memory
            .map(|m| format!("{} ", m.label()))
            .unwrap_or_default();
        Some(Span::styled(
            format!(
                " ⚡d{}µs w{}µs p{}µs {}ips {}kps {memory}",
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vm_rss_is_read_from_a_status_document() {
        let status = "Name:\tcroft\nVmPeak:\t 900000 kB\nVmRSS:\t  812345 kB\nThreads:\t9\n";
        assert_eq!(parse_vm_rss_kb(status), Some(812_345));
        assert_eq!(parse_vm_rss_kb("Name:\tcroft\n"), None);
    }

    /// #694: the HUD attributes memory to rewind without a profiler.
    #[test]
    fn the_memory_label_names_rss_and_the_rewind_budget() {
        let m = MemorySample {
            rss_kb: Some(812 * 1024),
            rewind_held: 12 * 1024 * 1024 + 1,
            rewind_total: 256 * 1024 * 1024,
            rewind_panes: 4,
        };
        assert_eq!(m.label(), "rss 812M rewind 13/256M×4");
        let no_proc = MemorySample { rss_kb: None, ..m };
        assert_eq!(no_proc.label(), "rewind 13/256M×4");
    }
}
