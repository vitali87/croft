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

#[derive(Default)]
pub struct OverlayManager {
    pub shortcuts_clear: ClearLatch,
    pub file_finder_clear: ClearLatch,
}
