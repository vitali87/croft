//! Pictures a program prints into the pane: keeping them and placing them on the grid.

use super::*;

/// A pane inline image surfaced to the app: its per-pane id, the raw image
/// bytes, and the anchor's CURRENT grid line (negative = scrolled into
/// history).
#[derive(Clone)]
pub struct PaneImage {
    pub seq: u64,
    pub data: std::sync::Arc<Vec<u8>>,
    pub line: i32,
}

/// Most inline images kept per pane; older ones scroll away like text.
pub(super) const IMAGES_MAX: usize = 4;

/// Set once the DA1 probe finds a sixel host, which no variable advertises.
pub static SIXEL_HOST: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Whether this croft draws a pane's captured images (#1453): the outer
/// terminal speaks some image protocol. A pane opened before the sixel probe
/// answers says no, which costs a picture, never a blank chart.
pub(super) fn pane_images_shown() -> bool {
    crate::iterm2_inline::detect_inline_image_protocol()
        != crate::iterm2_inline::InlineImageProtocol::None
        || SIXEL_HOST.load(std::sync::atomic::Ordering::Relaxed)
}

impl PtyTerminal {
    /// Inline images captured from the pane's output, oldest first, each
    /// with its anchor's CURRENT grid line (drift model identical to command
    /// marks). Images whose anchor scrolled past the scrollback floor are
    /// garbage-collected here.
    pub fn pane_images(&self) -> Vec<PaneImage> {
        let mut term = self.term.lock();
        let now = self.clock.lock().unwrap().tick(&mut term);
        let floor = term.grid().topmost_line().0;
        drop(term);
        let mut imgs = self.images.lock().unwrap();
        imgs.retain(|m| m.line_rec - (now - m.clock_rec) as i32 >= floor);
        imgs.iter()
            .map(|m| PaneImage {
                seq: m.seq,
                data: m.data.clone(),
                line: m.line_rec - (now - m.clock_rec) as i32,
            })
            .collect()
    }

    /// Test-only: record an inline image as the reader thread would,
    /// anchored at the grid's CURRENT cursor row.
    #[cfg(test)]
    pub fn push_image_for_test(&self, data: Vec<u8>) {
        let mut term = self.term.lock();
        let line_rec = term.grid().cursor.point.line.0;
        let clock_rec = self.clock.lock().unwrap().tick(&mut term);
        drop(term);
        let mut imgs = self.images.lock().unwrap();
        let seq = imgs.last().map_or(0, |m| m.seq) + 1;
        imgs.push(StoredImage {
            seq,
            data: std::sync::Arc::new(data),
            line_rec,
            clock_rec,
        });
    }
}
