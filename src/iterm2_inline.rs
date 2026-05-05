use base64::Engine;
use image::{ImageBuffer, Rgba, RgbaImage};

pub const EXPLORER_SRC_PNG: &[u8] =
    include_bytes!("../assets/icons/explorer_src.png");
pub const SEARCH_SRC_PNG: &[u8] =
    include_bytes!("../assets/icons/search_src.png");
pub const REMOTE_SRC_PNG: &[u8] =
    include_bytes!("../assets/icons/remote_src.png");
pub const WELCOME_LOGO_PNG: &[u8] =
    include_bytes!("../assets/logo-tight-removebg-preview.png");

const ACTIVE_PILL: Rgba<u8> = Rgba([0x4e, 0x9a, 0xff, 0xff]);
const ACTIVE_TINT: Rgba<u8> = Rgba([0xff, 0xff, 0xff, 0xff]);
const INACTIVE_TINT: Rgba<u8> = Rgba([0x9d, 0xa5, 0xb4, 0xff]);

/// Compose a runtime-sized icon canvas matching the iTerm2 cell viewport
/// exactly: width × height in physical pixels, bar-bg fill, codicon scaled
/// to a square area centred inside the canvas, optional active blue pill on
/// the left edge. The codicon stays visually square because we sit it in a
/// `min(w,h)` square sub-area; the canvas extra space along the longer
/// axis is filled with bar bg, so the rendered cell area shows zero
/// terminal-default-bg leftover.
pub fn compose_icon(
    src_codicon_png: &[u8],
    canvas_w: u32,
    canvas_h: u32,
    is_active: bool,
    bg: Rgba<u8>,
) -> Result<Vec<u8>, image::ImageError> {
    let codicon = image::load_from_memory_with_format(
        src_codicon_png,
        image::ImageFormat::Png,
    )?
    .to_rgba8();
    let icon_size = (canvas_w.min(canvas_h).saturating_sub(4) * 9 / 10).max(8);
    let scaled =
        image::imageops::resize(&codicon, icon_size, icon_size, image::imageops::FilterType::Lanczos3);
    let tint = if is_active { ACTIVE_TINT } else { INACTIVE_TINT };
    let tinted = tint_rgba(&scaled, tint);
    let mut canvas: RgbaImage =
        ImageBuffer::from_pixel(canvas_w, canvas_h, bg);
    let off_x = ((canvas_w - icon_size) / 2) as i64;
    let off_y: u32 = 1;
    image::imageops::overlay(&mut canvas, &tinted, off_x, off_y as i64);
    if is_active {
        let pill_w: u32 = if canvas_w >= 8 { 2 } else { 1 };
        // Pill height = the codicon's vertical extent so the pill visually
        // brackets the icon and doesn't drift to the empty bottom-canvas
        // bg the way a centered pill would.
        let pill_y_start = off_y;
        let pill_y_end = (off_y + icon_size).min(canvas_h);
        for y in pill_y_start..pill_y_end {
            for x in 0..pill_w {
                canvas.put_pixel(x, y, ACTIVE_PILL);
            }
        }
    }
    let mut out = Vec::with_capacity(2048);
    image::DynamicImage::ImageRgba8(canvas).write_to(
        &mut std::io::Cursor::new(&mut out),
        image::ImageFormat::Png,
    )?;
    Ok(out)
}

fn tint_rgba(src: &RgbaImage, tint: Rgba<u8>) -> RgbaImage {
    let mut out = src.clone();
    for px in out.pixels_mut() {
        let a = px.0[3];
        px.0 = [tint.0[0], tint.0[1], tint.0[2], a];
    }
    out
}

pub fn is_iterm2_term_program(value: Option<&str>) -> bool {
    matches!(value, Some("iTerm.app") | Some("WezTerm") | Some("ghostty"))
}

pub fn force_inline_images(value: Option<&str>) -> bool {
    value.is_some_and(|v| {
        matches!(
            v.to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

pub fn is_tmux_env(term: Option<&str>, tmux_var: Option<&str>) -> bool {
    tmux_var.is_some_and(|v| !v.is_empty())
        || term.is_some_and(|v| v.starts_with("tmux") || v.starts_with("screen"))
}

pub fn detect_iterm2_inline_support() -> bool {
    if force_inline_images(std::env::var("CROFT_FORCE_INLINE_IMAGES").ok().as_deref()) {
        return true;
    }
    let term_program = std::env::var("TERM_PROGRAM").ok();
    is_iterm2_term_program(term_program.as_deref())
}

pub fn detect_tmux() -> bool {
    let term = std::env::var("TERM").ok();
    let tmux = std::env::var("TMUX").ok();
    is_tmux_env(term.as_deref(), tmux.as_deref())
}

/// Bake an opaque PNG that exactly fills `(canvas_w_px, canvas_h_px)`. The
/// source image is scaled with Lanczos3 (preserving aspect ratio) and
/// overlaid on a solid `bg` fill. iTerm2 decodes the PNG as sRGB, so pass
/// the *sRGB-equivalent* of the surrounding SGR-painted pane bg here (see
/// `generic_rgb_to_srgb`) — that way the welcome image bg and the editor
/// pane bg display as the same physical pixel.
pub fn fit_image(
    src_png: &[u8],
    canvas_w_px: u32,
    canvas_h_px: u32,
    bg: Rgba<u8>,
) -> Result<Vec<u8>, image::ImageError> {
    fit_image_auto(src_png, canvas_w_px, canvas_h_px, bg)
}

/// Same as `fit_image` but accepts any format `image` can decode (PNG,
/// JPEG, GIF first frame, BMP, WebP). Bakes the result back to a PNG sized
/// to the supplied canvas with `bg` as the letterbox fill.
pub fn fit_image_auto(
    src: &[u8],
    canvas_w_px: u32,
    canvas_h_px: u32,
    bg: Rgba<u8>,
) -> Result<Vec<u8>, image::ImageError> {
    let img = image::load_from_memory(src)?.to_rgba8();
    let (sw, sh) = (img.width(), img.height());
    let scale = f64::min(
        canvas_w_px as f64 / sw as f64,
        canvas_h_px as f64 / sh as f64,
    );
    let new_w = ((sw as f64 * scale).round() as u32).max(1);
    let new_h = ((sh as f64 * scale).round() as u32).max(1);
    let scaled = image::imageops::resize(
        &img,
        new_w,
        new_h,
        image::imageops::FilterType::Lanczos3,
    );
    let mut canvas: RgbaImage = ImageBuffer::from_pixel(canvas_w_px, canvas_h_px, bg);
    let off_x = ((canvas_w_px as i64) - (new_w as i64)) / 2;
    let off_y = ((canvas_h_px as i64) - (new_h as i64)) / 2;
    image::imageops::overlay(&mut canvas, &scaled, off_x, off_y);
    let mut out = Vec::with_capacity(8192);
    image::DynamicImage::ImageRgba8(canvas).write_to(
        &mut std::io::Cursor::new(&mut out),
        image::ImageFormat::Png,
    )?;
    Ok(out)
}

pub fn build_inline_image_osc(
    png: &[u8],
    width_cells: u16,
    height_cells: u16,
    preserve_aspect: bool,
) -> String {
    let b64 = base64::engine::general_purpose::STANDARD.encode(png);
    let aspect = if preserve_aspect { 1 } else { 0 };
    let size = png.len();
    format!(
        "\x1b]1337;File=inline=1;size={size};width={width_cells};height={height_cells};preserveAspectRatio={aspect}:{b64}\x07"
    )
}

pub fn tmux_passthrough_wrap(seq: &str) -> String {
    let mut out = String::with_capacity(seq.len() + 8);
    out.push_str("\x1bPtmux;");
    for ch in seq.chars() {
        if ch == '\x1b' {
            out.push_str("\x1b\x1b");
        } else {
            out.push(ch);
        }
    }
    out.push_str("\x1b\\");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iterm_app_is_iterm2() {
        assert!(is_iterm2_term_program(Some("iTerm.app")));
    }

    #[test]
    fn wezterm_is_iterm2_compatible() {
        assert!(is_iterm2_term_program(Some("WezTerm")));
    }

    #[test]
    fn apple_terminal_is_not_iterm2() {
        assert!(!is_iterm2_term_program(Some("Apple_Terminal")));
    }

    #[test]
    fn missing_term_program_is_not_iterm2() {
        assert!(!is_iterm2_term_program(None));
    }

    #[test]
    fn force_inline_images_accepts_true_values() {
        assert!(force_inline_images(Some("1")));
        assert!(force_inline_images(Some("true")));
        assert!(force_inline_images(Some("yes")));
        assert!(!force_inline_images(Some("0")));
        assert!(!force_inline_images(None));
    }

    #[test]
    fn tmux_env_var_detected() {
        assert!(is_tmux_env(None, Some("/tmp/tmux-501/default,1234,0")));
    }

    #[test]
    fn empty_tmux_var_is_not_tmux() {
        assert!(!is_tmux_env(None, Some("")));
    }

    #[test]
    fn screen_term_is_tmux_like() {
        assert!(is_tmux_env(Some("screen-256color"), None));
    }

    #[test]
    fn xterm_is_not_tmux() {
        assert!(!is_tmux_env(Some("xterm-256color"), None));
    }

    #[test]
    fn osc1337_starts_with_iterm2_introducer() {
        let seq = build_inline_image_osc(b"PNGDATA", 4, 3, true);
        assert!(seq.starts_with("\x1b]1337;File="), "wrong prefix: {seq:?}");
    }

    #[test]
    fn osc1337_terminates_with_bel() {
        let seq = build_inline_image_osc(b"PNGDATA", 4, 3, true);
        assert!(seq.ends_with('\x07'), "must end with BEL: {seq:?}");
    }

    #[test]
    fn osc1337_carries_inline_flag_and_size_in_cells() {
        let seq = build_inline_image_osc(b"PNGDATA", 4, 3, true);
        assert!(seq.contains("inline=1"));
        assert!(seq.contains("width=4"));
        assert!(seq.contains("height=3"));
        assert!(seq.contains("preserveAspectRatio=1"));
    }

    #[test]
    fn osc1337_payload_is_base64_encoded_png() {
        let seq = build_inline_image_osc(b"PNGDATA", 4, 3, true);
        // PNGDATA → UE5HREFUQQ== in standard base64.
        assert!(
            seq.contains(":UE5HREFUQQ=="),
            "expected base64 payload after colon: {seq:?}"
        );
    }

    #[test]
    fn compose_icon_canvas_corners_equal_caller_supplied_bg() {
        // 1x1 transparent PNG: avoids needing a real codicon asset and lets
        // us verify the canvas fill is exactly the caller-provided sRGB bg.
        let src = {
            let img: RgbaImage = ImageBuffer::from_pixel(1, 1, Rgba([0, 0, 0, 0]));
            let mut buf = Vec::new();
            image::DynamicImage::ImageRgba8(img)
                .write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
                .unwrap();
            buf
        };
        let bg = Rgba([0x1e, 0x22, 0x2e, 0xff]);
        let baked = compose_icon(&src, 32, 16, false, bg).unwrap();
        let decoded = image::load_from_memory_with_format(&baked, image::ImageFormat::Png)
            .unwrap()
            .to_rgba8();
        let (w, h) = (decoded.width(), decoded.height());
        for (x, y) in [(0, 0), (w - 1, 0), (0, h - 1), (w - 1, h - 1)] {
            let px = decoded.get_pixel(x, y).0;
            assert_eq!(
                (px[0], px[1], px[2], px[3]),
                (bg.0[0], bg.0[1], bg.0[2], bg.0[3]),
                "corner ({x},{y}) must equal caller bg"
            );
        }
    }

    #[test]
    fn tmux_passthrough_wrap_doubles_inner_escapes() {
        let inner = "\x1b]1337;File=inline=1:DATA\x07";
        let wrapped = tmux_passthrough_wrap(inner);
        assert!(wrapped.starts_with("\x1bPtmux;"), "DCS prefix");
        assert!(wrapped.ends_with("\x1b\\"), "ST terminator");
        assert!(wrapped.contains("\x1b\x1b]1337"), "inner ESC must be doubled");
    }
}
