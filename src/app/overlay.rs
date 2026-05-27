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
pub struct OverlayManager {
    pub shortcuts_clear: ClearLatch,
    pub file_finder_clear: ClearLatch,
    pub editor: ImageOverlay<super::EditorImageLayout>,
    pub welcome: ImageOverlay<super::WelcomeLayout>,
    pub hero: ImageOverlay<super::WelcomeLayout>,
    pub ssh: ImageOverlay<()>,
}
