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
    target: Option<(u16, u16)>,
    last_emitted: Option<(u16, u16)>,
    clear: ClearLatch,
}

impl CellOverlay {
    pub fn set_image(&mut self, image: String) {
        self.image = Some(image);
    }

    pub fn image(&self) -> Option<&str> {
        self.image.as_deref()
    }

    pub fn has_image(&self) -> bool {
        self.image.is_some()
    }

    pub fn set_target(&mut self, target: Option<(u16, u16)>) {
        self.target = target;
    }

    pub fn target(&self) -> Option<(u16, u16)> {
        self.target
    }

    pub fn last_emitted(&self) -> Option<(u16, u16)> {
        self.last_emitted
    }

    pub fn take_last_emitted(&mut self) -> Option<(u16, u16)> {
        self.last_emitted.take()
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

    pub fn should_refresh(&self) -> bool {
        self.dirty
            || self
                .last_emit
                .is_none_or(|t| t.elapsed() >= Self::KEEPALIVE)
    }

    pub fn mark_emitted(&mut self) {
        self.dirty = false;
        self.last_emit = Some(std::time::Instant::now());
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
    pub zoxide_jump_clear: ClearLatch,
    /// Inline-image overlay state, one slot per editor split column
    /// (`[0]` = left pane, `[1]` = right pane). When the editor is not
    /// split only `[0]` is used and `[1]` stays disabled. Keyed by
    /// physical side so the OSC-1337 coordinates stay stable across a
    /// focus swap (a focus swap never moves a group's column).
    pub editor: [ImageOverlay<super::EditorImageLayout>; 2],
    pub welcome: ImageOverlay<super::WelcomeLayout>,
    pub hero: ImageOverlay<super::WelcomeLayout>,
    pub ssh: ImageOverlay<()>,
    pub badge: CellOverlay,
    pub run_debug: CellOverlay,
    pub activity: ActivityOverlay,
}
