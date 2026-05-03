use anyhow::{Context, Result};
use image::DynamicImage;

pub const EXPLORER_ACTIVE_PNG: &[u8] =
    include_bytes!("../assets/icons/explorer_active.png");
pub const EXPLORER_INACTIVE_PNG: &[u8] =
    include_bytes!("../assets/icons/explorer_inactive.png");
pub const SEARCH_ACTIVE_PNG: &[u8] =
    include_bytes!("../assets/icons/search_active.png");
pub const SEARCH_INACTIVE_PNG: &[u8] =
    include_bytes!("../assets/icons/search_inactive.png");

pub fn decode(png_bytes: &[u8]) -> Result<DynamicImage> {
    image::load_from_memory_with_format(png_bytes, image::ImageFormat::Png)
        .context("decode codicon PNG")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explorer_active_png_decodes_to_nonempty_image() {
        let img = decode(EXPLORER_ACTIVE_PNG).expect("PNG decodes");
        assert!(img.width() > 0 && img.height() > 0);
    }

    #[test]
    fn explorer_inactive_png_decodes_to_nonempty_image() {
        let img = decode(EXPLORER_INACTIVE_PNG).expect("PNG decodes");
        assert!(img.width() > 0 && img.height() > 0);
    }

    #[test]
    fn search_active_png_decodes_to_nonempty_image() {
        let img = decode(SEARCH_ACTIVE_PNG).expect("PNG decodes");
        assert!(img.width() > 0 && img.height() > 0);
    }

    #[test]
    fn search_inactive_png_decodes_to_nonempty_image() {
        let img = decode(SEARCH_INACTIVE_PNG).expect("PNG decodes");
        assert!(img.width() > 0 && img.height() > 0);
    }

    #[test]
    fn icons_have_alpha_channel_for_transparent_background() {
        let img = decode(EXPLORER_ACTIVE_PNG).expect("PNG decodes");
        assert!(
            matches!(img, DynamicImage::ImageRgba8(_) | DynamicImage::ImageRgba16(_)),
            "icons must have alpha so the activity-bar background shows through, got {:?}",
            img.color()
        );
    }
}
