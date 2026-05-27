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
    clear: ClearLatch,
}

impl<L> Default for ImageOverlay<L> {
    fn default() -> Self {
        Self {
            image: None,
            layout: None,
            displayed: false,
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

    pub fn payload(&self) -> Option<(&str, &L)> {
        Some((self.image.as_deref()?, self.layout.as_ref()?))
    }

    pub fn mark_displayed(&mut self) {
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
}

#[derive(Default)]
pub struct OverlayManager {
    pub shortcuts_clear: ClearLatch,
    pub file_finder_clear: ClearLatch,
    pub editor: ImageOverlay<super::EditorImageLayout>,
}
