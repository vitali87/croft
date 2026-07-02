#[derive(Default)]
pub struct ClearLatch {
    requested: bool,
}

impl ClearLatch {
    pub fn request(&mut self) {
        self.requested = true;
    }

    pub fn consume(&mut self) -> bool {
        let requested = self.requested;
        self.requested = false;
        requested
    }
}

pub struct ImageOverlay<L> {
    image: Option<String>,
    layout: Option<L>,
    displayed: bool,
    dirty: bool,
    clear: ClearLatch,
}

impl<L> Default for ImageOverlay<L> {
    fn default() -> Self {
        Self {
            image: None,
            layout: None,
            displayed: false,
            dirty: false,
            clear: ClearLatch::default(),
        }
    }
}

impl<L: PartialEq> ImageOverlay<L> {
    pub fn layout_matches(&self, desired: &L) -> bool {
        self.layout.as_ref() == Some(desired)
    }

    pub fn layout(&self) -> Option<&L> {
        self.layout.as_ref()
    }

    pub fn set(&mut self, image: String, layout: L) {
        self.image = Some(image);
        self.layout = Some(layout);
    }

    pub fn set_image(&mut self, image: String) {
        self.image = Some(image);
    }

    pub fn set_layout(&mut self, layout: L) {
        self.layout = Some(layout);
    }

    pub fn has_image(&self) -> bool {
        self.image.is_some()
    }

    pub fn image(&self) -> Option<&str> {
        self.image.as_deref()
    }

    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    pub fn mark_hidden(&mut self) {
        if self.displayed {
            self.displayed = false;
            self.dirty = true;
        }
    }

    pub fn hide_and_request_clear(&mut self) {
        if self.displayed {
            self.displayed = false;
            self.clear.request();
        }
    }

    pub fn request_clear(&mut self) {
        self.clear.request();
    }

    pub fn request_clear_if_displayed(&mut self) {
        if self.displayed {
            self.clear.request();
        }
    }

    pub fn invalidate_layout(&mut self) {
        self.layout = None;
    }

    pub fn disable(&mut self) {
        self.request_clear_if_displayed();
        self.image = None;
        self.layout = None;
    }

    pub fn consume_clear(&mut self) -> bool {
        if self.clear.consume() {
            self.displayed = false;
            true
        } else {
            false
        }
    }

    pub fn consume_clear_latch(&mut self) -> bool {
        self.clear.consume()
    }

    pub fn consume_clear_or(&mut self, also: bool) -> bool {
        if self.clear.consume() || (self.displayed && also) {
            self.displayed = false;
            true
        } else {
            false
        }
    }

    pub fn payload(&self) -> Option<(&str, &L)> {
        Some((self.image.as_deref()?, self.layout.as_ref()?))
    }

    pub fn emit_payload(&self) -> Option<(&str, &L)> {
        if self.dirty { self.payload() } else { None }
    }

    pub fn mark_displayed(&mut self) {
        self.displayed = true;
    }

    pub fn mark_emitted(&mut self) {
        self.dirty = false;
        self.displayed = true;
    }

    #[cfg(test)]
    pub fn set_test_state(&mut self, image: Option<String>, layout: Option<L>, displayed: bool) {
        self.image = image;
        self.layout = layout;
        self.displayed = displayed;
    }

    #[cfg(test)]
    pub fn image_is_none(&self) -> bool {
        self.image.is_none()
    }

    #[cfg(test)]
    pub fn layout_is_none(&self) -> bool {
        self.layout.is_none()
    }

    #[cfg(test)]
    pub fn set_displayed(&mut self, v: bool) {
        self.displayed = v;
    }

    #[cfg(test)]
    pub fn set_dirty(&mut self, v: bool) {
        self.dirty = v;
    }

    #[cfg(test)]
    pub fn is_displayed(&self) -> bool {
        self.displayed
    }

    #[cfg(test)]
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    #[cfg(test)]
    pub fn clear_requested(&self) -> bool {
        self.clear.requested
    }
}

#[derive(Default)]
pub struct CellOverlay {
    image: Option<String>,
    last_emitted: Option<(u16, u16)>,
    clear: ClearLatch,
}

impl CellOverlay {
    pub fn set_image(&mut self, image: String) {
        self.image = Some(image);
    }

    pub fn clear_image(&mut self) {
        self.image = None;
    }

    pub fn image(&self) -> Option<&str> {
        self.image.as_deref()
    }

    pub fn has_image(&self) -> bool {
        self.image.is_some()
    }

    pub fn last_emitted(&self) -> Option<(u16, u16)> {
        self.last_emitted
    }

    pub fn mark_emitted_at(&mut self, cell: (u16, u16)) {
        self.last_emitted = Some(cell);
    }

    pub fn clear_emitted(&mut self) {
        self.last_emitted = None;
    }

    pub fn was_emitted(&self) -> bool {
        self.last_emitted.is_some()
    }

    pub fn request_clear(&mut self) {
        self.clear.request();
    }

    pub fn consume_clear(&mut self) -> bool {
        self.clear.consume()
    }
}

#[derive(Default)]
pub struct ActivityOverlay {
    images: Option<super::ActivityBarImages>,
    dirty: bool,
    last_emit: Option<std::time::Instant>,
    /// Cell positions of the icons painted on the most recent emit. Compared
    /// against the next frame's positions so the flush can detect a layout
    /// shift (terminal resize, or any change that recenters the bar) and
    /// evict iTerm2's stale OSC-1337 image layer before re-emitting.
    last_positions: Vec<(u16, u16)>,
    clear: ClearLatch,
}

impl ActivityOverlay {
    const KEEPALIVE: std::time::Duration = std::time::Duration::from_secs(2);

    pub fn set_images(&mut self, images: super::ActivityBarImages) {
        self.images = Some(images);
    }

    pub fn images(&self) -> Option<&super::ActivityBarImages> {
        self.images.as_ref()
    }

    pub fn has_images(&self) -> bool {
        self.images.is_some()
    }

    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    /// Whether the icons should be re-emitted this frame. Always true on a
    /// dirty (view switch, resize). The 2-second keepalive re-emit only applies
    /// when `allow_keepalive` is set: it exists to outlast iTerm2's image-cache
    /// eviction under SGR traffic, but sixel cells live in the normal buffer and
    /// are never overwritten in the untouched activity-bar column, so re-emitting
    /// them just risks a repaint flicker — the sixel path passes `false`.
    pub fn should_refresh(&self, allow_keepalive: bool) -> bool {
        self.dirty
            || (allow_keepalive
                && self
                    .last_emit
                    .is_none_or(|t| t.elapsed() >= Self::KEEPALIVE))
    }

    pub fn mark_emitted(&mut self) {
        self.dirty = false;
        self.last_emit = Some(std::time::Instant::now());
    }

    /// Arm a one-shot `terminal.clear()` (consumed by the main loop's clear
    /// chain) so iTerm2 evicts the cached OSC-1337 icon bitmaps. Needed
    /// whenever a reflow could leave a stale image layer that a plain SGR
    /// re-emit would stack a fresh copy on top of (the doubled-icon ghost).
    pub fn request_clear(&mut self) {
        self.clear.request();
    }

    pub fn consume_clear(&mut self) -> bool {
        self.clear.consume()
    }

    /// True when the icons were emitted before at a *different* set of cells.
    /// A first emit (or one after `forget_positions`) reports `false` so the
    /// caller paints rather than looping on an eviction it doesn't need.
    pub fn positions_moved(&self, positions: &[(u16, u16)]) -> bool {
        !self.last_positions.is_empty() && self.last_positions != positions
    }

    pub fn store_positions(&mut self, positions: Vec<(u16, u16)>) {
        self.last_positions = positions;
    }

    pub fn forget_positions(&mut self) {
        self.last_positions.clear();
    }

    #[cfg(test)]
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }
}

#[derive(Default)]
pub struct OverlayManager {
    pub shortcuts_clear: ClearLatch,
    pub file_finder_clear: ClearLatch,
    pub command_palette_clear: ClearLatch,
    pub process_picker_clear: ClearLatch,
    pub zoxide_jump_clear: ClearLatch,
    pub branch_picker_clear: ClearLatch,
    pub input_prompt_clear: ClearLatch,
    pub list_picker_clear: ClearLatch,
    /// Inline-image overlay state, one slot per editor split column
    /// (`[0]` = left pane, `[1]` = right pane). When the editor is not
    /// split only `[0]` is used and `[1]` stays disabled. Keyed by
    /// physical side so the OSC-1337 coordinates stay stable across a
    /// focus swap (a focus swap never moves a group's column).
    pub editor: [ImageOverlay<super::EditorImageLayout>; 2],
    pub welcome: ImageOverlay<super::WelcomeLayout>,
    pub minimap: ImageOverlay<super::MinimapLayout>,
    pub hero: ImageOverlay<super::WelcomeLayout>,
    pub ssh: ImageOverlay<()>,
    pub run_debug: CellOverlay,
    pub problems_badge: CellOverlay,
    pub activity: ActivityOverlay,
}
