use base64::Engine;
use image::{ImageBuffer, Rgba, RgbaImage};

// Activity-bar codicons, kept as their upstream `microsoft/vscode-codicons`
// SVG sources and rasterised at the exact icon pixel size in `compose_icon_svg`
// (see `App::init_graphics`). Shipping the vector source rather than a 192×192
// raster keeps the glyphs crisp at any cell size and avoids decoding a
// heavyweight PNG for a 4×2-cell icon (the Source Control icon previously
// reused the 2.18 MB no-repo illustration, which dominated cold-start time).
pub const EXPLORER_SRC_SVG: &[u8] = include_bytes!("../assets/icons/files.svg");
pub const SEARCH_SRC_SVG: &[u8] = include_bytes!("../assets/icons/search.svg");
pub const SOURCE_CONTROL_SRC_SVG: &[u8] = include_bytes!("../assets/icons/source-control.svg");
pub const REMOTE_SRC_SVG: &[u8] = include_bytes!("../assets/icons/remote-explorer.svg");
/// Codicon `debug-alt` (bug + play triangle), the Run and Debug activity icon.
pub const RUN_DEBUG_SRC_SVG: &[u8] = include_bytes!("../assets/icons/debug-alt.svg");
/// Codicon `extensions` (four squares, one detached), the Extensions activity icon.
pub const EXTENSIONS_SRC_SVG: &[u8] = include_bytes!("../assets/icons/extensions.svg");
/// Codicon `gear`, the bottom-anchored "Manage" button (Color Theme picker).
pub const SETTINGS_GEAR_SRC_SVG: &[u8] = include_bytes!("../assets/icons/settings_gear.svg");
/// VS Code's title-bar layout codicons for the Customize Layout toolbar.
/// `layout-sidebar-left` / `-off` toggle the primary side bar (filled vs.
/// hollow left pane), `layout-panel` / `-off` toggle the bottom panel, and
/// `layout` is the Customize Layout grid. Rasterised at the toolbar cell box so
/// they render twice the size of a font glyph and match the activity bar.
pub const LAYOUT_SIDEBAR_ON_SRC_SVG: &[u8] =
    include_bytes!("../assets/icons/layout-sidebar-left.svg");
pub const LAYOUT_SIDEBAR_OFF_SRC_SVG: &[u8] =
    include_bytes!("../assets/icons/layout-sidebar-left-off.svg");
pub const LAYOUT_PANEL_ON_SRC_SVG: &[u8] = include_bytes!("../assets/icons/layout-panel.svg");
pub const LAYOUT_PANEL_OFF_SRC_SVG: &[u8] = include_bytes!("../assets/icons/layout-panel-off.svg");
pub const LAYOUT_CUSTOMIZE_SRC_SVG: &[u8] = include_bytes!("../assets/icons/layout.svg");
/// 192x192 white-on-transparent rasterisation of `assets/icons/debug-alt.svg`.
/// Still drives the Run and Debug panel's headline illustration (composed via
/// the PNG path); the activity-bar icon now rasterises the SVG directly.
pub const RUN_DEBUG_SRC_PNG: &[u8] = include_bytes!("../assets/icons/run_debug_src.png");
pub const WELCOME_LOGO_PNG: &[u8] = include_bytes!("../assets/logo-tight-removebg-preview.png");
/// Square (1254x1254) croft logo, used as the source for the macOS launcher
/// app-bundle icon (`croft install-launcher` rasterises it into an `.icns`).
pub const APP_ICON_PNG: &[u8] = include_bytes!("../assets/logo.png");
/// Hero illustration shown in the Source Control sidebar when the
/// workspace isn't a git repo: a stylised file silhouette with the Git
/// Y-fork (three blue rings + curved branch) and a dashed circle, framed
/// by decorative `+` and dot motifs. Bundled as a raster so it renders
/// identically across terminals that support OSC-1337 inline images.
pub const NO_REPO_HERO_PNG: &[u8] = include_bytes!("../assets/icons/no_repo_src.png");
/// Illustration shown inside the Remote Explorer panel's SSH section when
/// no Host entries are present: three stylised server units stacked
/// vertically with a dashed connector to a small terminal box, framed by
/// decorative `+` motifs. Same teal/cyan palette as the rest of the
/// empty-state card. Bundled as a raster so we can paint it via OSC-1337
/// instead of fighting box-drawing characters that never look quite right.
pub const SSH_EMPTY_STATE_PNG: &[u8] = include_bytes!("../assets/icons/ssh_empty_state.png");

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
/// `off_y_bias` nudges the composed glyph vertically by a pixel count, on top
/// of the default vertical centering, and is clamped so the glyph can never
/// clip past the canvas edges. Positive shifts down, negative up. View icons
/// pass `0`; the bottom-anchored settings gear passes a small positive bias so
/// it sits a touch lower within its cell block (sub-cell precision the
/// whole-cell block anchor can't express).
pub fn compose_icon(
    src_codicon_png: &[u8],
    canvas_w: u32,
    canvas_h: u32,
    is_active: bool,
    bg: Rgba<u8>,
    off_y_bias: i64,
) -> Result<Vec<u8>, image::ImageError> {
    let codicon =
        image::load_from_memory_with_format(src_codicon_png, image::ImageFormat::Png)?.to_rgba8();
    let (src_w, src_h) = (codicon.width().max(1), codicon.height().max(1));
    let (icon_w, icon_h) = fit_icon_dims(canvas_w, canvas_h, src_w, src_h);
    let scaled = image::imageops::resize(
        &codicon,
        icon_w,
        icon_h,
        image::imageops::FilterType::Lanczos3,
    );
    let tint = if is_active {
        ACTIVE_TINT
    } else {
        INACTIVE_TINT
    };
    let tinted = tint_rgba(&scaled, tint);
    Ok(finish_icon(
        &tinted, canvas_w, canvas_h, is_active, bg, off_y_bias,
    ))
}

/// Visual state of an activity-bar icon, mirroring VS Code's activity bar:
/// `Inactive` is the muted grey resting glyph, `Hovered` brightens it to the
/// active foreground while the pointer is over it (no selection bar), and
/// `Active` is the selected view: bright glyph plus the blue left pill.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum IconState {
    Inactive,
    Hovered,
    Active,
}

impl IconState {
    /// Whether the glyph wears the bright (active-foreground) tint. Both the
    /// hovered and active states do; only the resting state stays muted.
    fn tinted(self) -> bool {
        matches!(self, IconState::Hovered | IconState::Active)
    }

    /// Whether the blue left selection pill is painted. Only the active
    /// (selected-view) state shows it, so a merely-hovered icon stays
    /// distinguishable from the selected one.
    fn pill(self) -> bool {
        matches!(self, IconState::Active)
    }
}

/// Like [`compose_icon`], but rasterises an SVG codicon directly at the target
/// icon pixel size instead of decoding-then-downscaling a raster source. The
/// glyph is rendered crisp at the exact cell size, tinted, and composed onto
/// the bar-bg canvas identically to the raster path. Returns `None` if the SVG
/// fails to parse or a zero-sized pixmap is requested.
pub fn compose_icon_svg(
    src_svg: &[u8],
    canvas_w: u32,
    canvas_h: u32,
    state: IconState,
    bg: Rgba<u8>,
    off_y_bias: i64,
) -> Option<Vec<u8>> {
    let tree = resvg::usvg::Tree::from_data(src_svg, &resvg::usvg::Options::default()).ok()?;
    let size = tree.size();
    let (src_w, src_h) = (
        size.width().ceil().max(1.0) as u32,
        size.height().ceil().max(1.0) as u32,
    );
    let (icon_w, icon_h) = fit_icon_dims(canvas_w, canvas_h, src_w, src_h);
    let mut pixmap = resvg::tiny_skia::Pixmap::new(icon_w, icon_h)?;
    // Independent x/y scale fills the icon rect exactly; because `fit_icon_dims`
    // preserves the source aspect, the two factors are equal up to rounding, so
    // there is no visible distortion.
    let transform = resvg::tiny_skia::Transform::from_scale(
        icon_w as f32 / size.width(),
        icon_h as f32 / size.height(),
    );
    resvg::render(&tree, transform, &mut pixmap.as_mut());
    // Keep only the coverage alpha from the rasterised glyph and stamp the tint
    // colour on top, mirroring `tint_rgba` for the raster path. tiny-skia stores
    // premultiplied RGBA but the alpha byte itself is straight, which is all the
    // alpha-composited overlay below needs.
    let tint = if state.tinted() {
        ACTIVE_TINT
    } else {
        INACTIVE_TINT
    };
    let data = pixmap.data();
    let mut glyph: RgbaImage = ImageBuffer::new(icon_w, icon_h);
    for (i, px) in glyph.pixels_mut().enumerate() {
        *px = Rgba([tint.0[0], tint.0[1], tint.0[2], data[i * 4 + 3]]);
    }
    Some(finish_icon(
        &glyph,
        canvas_w,
        canvas_h,
        state.pill(),
        bg,
        off_y_bias,
    ))
}

/// Size the glyph to a square-ish area inside the canvas while preserving the
/// source aspect ratio: at most 90% of the shorter canvas axis, never taller or
/// wider than the canvas less a 2px margin.
fn fit_icon_dims(canvas_w: u32, canvas_h: u32, src_w: u32, src_h: u32) -> (u32, u32) {
    let target_max = (canvas_w.min(canvas_h).saturating_sub(4) * 9 / 10).max(8);
    let (src_w, src_h) = (src_w.max(1), src_h.max(1));
    if src_w <= src_h {
        let scaled_h = ((target_max * src_h) / src_w).max(1);
        let capped_h = scaled_h.min(canvas_h.saturating_sub(2).max(1));
        let final_w = ((capped_h * src_w) / src_h).max(1);
        (final_w, capped_h)
    } else {
        let scaled_w = ((target_max * src_w) / src_h).max(1);
        let capped_w = scaled_w.min(canvas_w.saturating_sub(2).max(1));
        let final_h = ((capped_w * src_h) / src_w).max(1);
        (capped_w, final_h)
    }
}

/// Centre an already-sized, already-tinted glyph on a bar-bg canvas, apply the
/// optional active blue pill on the left edge, and PNG-encode the result.
/// `off_y_bias` nudges the glyph vertically, clamped so it never clips.
fn finish_icon(
    tinted: &RgbaImage,
    canvas_w: u32,
    canvas_h: u32,
    is_active: bool,
    bg: Rgba<u8>,
    off_y_bias: i64,
) -> Vec<u8> {
    let (icon_w, icon_h) = (tinted.width(), tinted.height());
    let mut canvas: RgbaImage = ImageBuffer::from_pixel(canvas_w, canvas_h, bg);
    let off_x = ((canvas_w.saturating_sub(icon_w)) / 2) as i64;
    let centered_off_y = (canvas_h.saturating_sub(icon_h)) / 2;
    // Clamp the bias so the glyph stays fully inside the canvas: it can travel
    // at most as far as the centering padding in either direction.
    let max_off_y = canvas_h.saturating_sub(icon_h) as i64;
    let off_y = (centered_off_y as i64 + off_y_bias).clamp(0, max_off_y);
    image::imageops::overlay(&mut canvas, tinted, off_x, off_y);
    if is_active {
        let pill_w: u32 = if canvas_w >= 8 { 2 } else { 1 };
        let pill_y_start = off_y.max(0) as u32;
        let pill_y_end = (pill_y_start + icon_h).min(canvas_h);
        for y in pill_y_start..pill_y_end {
            for x in 0..pill_w {
                canvas.put_pixel(x, y, ACTIVE_PILL);
            }
        }
    }
    let mut out = Vec::with_capacity(2048);
    // Encoding a tiny RGBA canvas to PNG is ~0.01ms; the cost that used to
    // dominate startup was decoding the multi-MB raster *source*, not this.
    let _ = image::DynamicImage::ImageRgba8(canvas)
        .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png);
    out
}

/// Activity-bar cells wide the source-control change badge occupies for a given
/// change count. One- and two-digit counts use a square-ish 2-cell pill (a
/// circle on the typical ~1:2 cell); the "99+" overflow needs three.
pub fn count_badge_cells_w(count: usize) -> u16 {
    if count > 99 {
        3
    } else {
        2
    }
}

/// 3×5 pixel-font rows for the digits and '+', one byte per row with bit 2 = the
/// left column. Drawn white over the accent pill so the badge needs no font
/// database (resvg's text support is compiled out, see `Cargo.toml`).
const BADGE_GLYPHS: [(char, [u8; 5]); 11] = [
    ('0', [0b111, 0b101, 0b101, 0b101, 0b111]),
    ('1', [0b010, 0b110, 0b010, 0b010, 0b111]),
    ('2', [0b111, 0b001, 0b111, 0b100, 0b111]),
    ('3', [0b111, 0b001, 0b111, 0b001, 0b111]),
    ('4', [0b101, 0b101, 0b111, 0b001, 0b001]),
    ('5', [0b111, 0b100, 0b111, 0b001, 0b111]),
    ('6', [0b111, 0b100, 0b111, 0b101, 0b111]),
    ('7', [0b111, 0b001, 0b010, 0b010, 0b010]),
    ('8', [0b111, 0b101, 0b111, 0b101, 0b111]),
    ('9', [0b111, 0b101, 0b111, 0b001, 0b111]),
    ('+', [0b000, 0b010, 0b111, 0b010, 0b000]),
];

/// Rasterise the source-control change-count badge: a rounded accent pill with
/// the count in white, sized to `count_badge_cells_w(count)`×1 activity-bar
/// cells. Composited onto `bg` (the bar background) so the iTerm2 path, which
/// can't render alpha, has no transparent edge to ghost. Mirrors VS Code's
/// `activityBarBadge`. Returns PNG bytes, or `None` on a zero-sized request.
pub fn compose_count_badge(
    count: usize,
    cell_w: u32,
    cell_h: u32,
    accent: (u8, u8, u8),
    bg: Rgba<u8>,
) -> Option<Vec<u8>> {
    let label = if count > 99 {
        String::from("99+")
    } else {
        count.to_string()
    };
    let (w, h) = (cell_w * count_badge_cells_w(count) as u32, cell_h);
    if w == 0 || h == 0 {
        return None;
    }
    let mut canvas: RgbaImage = ImageBuffer::from_pixel(w, h, bg);
    // Stadium pill: full height, corner radius = half height (a circle when the
    // pill is roughly square, which the 2-cell width gives on a ~1:2 cell).
    let accent_px = Rgba([accent.0, accent.1, accent.2, 0xff]);
    let r = h as f32 / 2.0;
    for y in 0..h {
        for x in 0..w {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            let cx = fx.clamp(r, (w as f32 - r).max(r));
            let cy = fy.clamp(r, (h as f32 - r).max(r));
            if (fx - cx).hypot(fy - cy) <= r {
                canvas.put_pixel(x, y, accent_px);
            }
        }
    }
    // Count digits, white, nearest-neighbour scaled and centred over the pill.
    let glyphs: Vec<&[u8; 5]> = label
        .chars()
        .filter_map(|c| BADGE_GLYPHS.iter().find(|(g, _)| *g == c).map(|(_, rows)| rows))
        .collect();
    let n = glyphs.len() as u32;
    if n > 0 {
        let grid_w = n * 3 + (n - 1); // 3 cols per glyph + a 1-col gap between
        let scale = ((h * 6 / 10) / 5).min((w * 8 / 10) / grid_w).max(1);
        let off_x = w.saturating_sub(grid_w * scale) / 2;
        let off_y = h.saturating_sub(5 * scale) / 2;
        let white = Rgba([0xff, 0xff, 0xff, 0xff]);
        for (gi, rows) in glyphs.iter().enumerate() {
            let gx = off_x + gi as u32 * 4 * scale;
            for (ry, row) in rows.iter().enumerate() {
                for col in 0..3u32 {
                    if row & (0b100 >> col) == 0 {
                        continue;
                    }
                    let (px0, py0) = (gx + col * scale, off_y + ry as u32 * scale);
                    for dy in 0..scale {
                        for dx in 0..scale {
                            let (px, py) = (px0 + dx, py0 + dy);
                            if px < w && py < h {
                                canvas.put_pixel(px, py, white);
                            }
                        }
                    }
                }
            }
        }
    }
    let mut out = Vec::with_capacity(1024);
    let _ = image::DynamicImage::ImageRgba8(canvas)
        .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png);
    Some(out)
}

fn tint_rgba(src: &RgbaImage, tint: Rgba<u8>) -> RgbaImage {
    let mut out = src.clone();
    for px in out.pixels_mut() {
        let a = px.0[3];
        px.0 = [tint.0[0], tint.0[1], tint.0[2], a];
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum InlineImageProtocol {
    ITerm2,
    Kitty,
    /// DEC sixel graphics. Unlike iTerm2/Kitty there is no env var that
    /// advertises it, so it is detected at runtime via a DA1 probe
    /// (`probe_sixel_support`) rather than by `inline_image_protocol_for`.
    /// Painted as explicit truecolor like Kitty (it is not iTerm2, so no
    /// `SetColors`), but its cells live in the normal cell buffer like the
    /// iTerm2 path: a `\x1b[2J` evicts them and writing spaces erases them, so
    /// it needs neither the Kitty delete-all-on-clear nor delete-by-id.
    Sixel,
    None,
}

#[allow(dead_code)]
pub fn inline_image_protocol_for(
    term_program: Option<&str>,
    term: Option<&str>,
) -> InlineImageProtocol {
    match term_program {
        Some("iTerm.app") | Some("WezTerm") => InlineImageProtocol::ITerm2,
        Some("ghostty") => InlineImageProtocol::Kitty,
        // SSH forwards `TERM` (Ghostty advertises `xterm-ghostty`, Kitty
        // `xterm-kitty`) but not `TERM_PROGRAM` unless the remote sshd opts in
        // via `AcceptEnv`, so a remote croft must key Kitty detection off `TERM`
        // too or it falls back to text glyphs over SSH.
        _ if term.is_some_and(|t| t.contains("kitty") || t.contains("ghostty")) => {
            InlineImageProtocol::Kitty
        }
        _ => InlineImageProtocol::None,
    }
}

/// True when croft should nudge the user to switch to a richer terminal:
/// the resolved inline-image `protocol` is `None` (no iTerm2/Kitty/sixel
/// rendering, so the activity bar and file icons fall back to blank cells)
/// AND this is not Termux. Termux renders no images either, but it is an
/// intentional Android target where the recommended terminals (iTerm2 on
/// macOS, Ghostty on macOS/Linux) don't exist, so it is never warned.
///
/// Must be evaluated AFTER the sixel DA1 probe has promoted `protocol`, so a
/// sixel host (foot, contour, xterm) is correctly treated as supported.
pub fn terminal_warrants_switch_warning(protocol: InlineImageProtocol, termux: bool) -> bool {
    protocol == InlineImageProtocol::None && !termux
}

#[allow(dead_code)]
pub fn detect_inline_image_protocol() -> InlineImageProtocol {
    let term_program = std::env::var("TERM_PROGRAM").ok();
    let term = std::env::var("TERM").ok();
    let protocol = inline_image_protocol_for(term_program.as_deref(), term.as_deref());
    if protocol != InlineImageProtocol::None {
        return protocol;
    }
    if force_inline_images(std::env::var("CROFT_FORCE_INLINE_IMAGES").ok().as_deref()) {
        return InlineImageProtocol::ITerm2;
    }
    InlineImageProtocol::None
}

/// True when a Primary Device Attributes (DA1) reply advertises sixel support.
/// A DA1 reply looks like `\x1b[?62;1;4;6c`; sixel is attribute `4`, declared as
/// a standalone `;`-separated token (so it must not match the `4` inside `14`,
/// `64`, etc.). Bytes outside the `\x1b[?` … `c` envelope are ignored, which
/// tolerates stray input that arrived alongside the reply.
pub fn da1_advertises_sixel(reply: &[u8]) -> bool {
    let text = String::from_utf8_lossy(reply);
    let Some(start) = text.find("\x1b[?") else {
        return false;
    };
    let tail = &text[start + 3..];
    let Some(end) = tail.find('c') else {
        return false;
    };
    tail[..end].split(';').any(|tok| tok.trim() == "4")
}

/// Probe the host terminal for sixel support by issuing Primary Device
/// Attributes (`\x1b[c`) and parsing the reply for attribute `4`. Returns
/// `false` on any timeout, read error, or absent attribute, so a terminal that
/// stays silent (or replies without sixel, like Apple Terminal) falls back
/// cleanly to the text glyphs.
///
/// MUST be called after raw mode is enabled and BEFORE the main loop starts
/// reading stdin (crossterm's event source initialises lazily on first poll, so
/// probing first keeps it from swallowing the reply). Virtually every terminal
/// answers DA1 promptly, so the timeout is only a safety net for a
/// non-responding host and does not slow normal startup.
#[cfg(unix)]
pub fn probe_sixel_support() -> bool {
    use std::io::{Read, Write};
    use std::os::fd::AsRawFd;

    let mut out = std::io::stdout();
    if out.write_all(b"\x1b[c").is_err() || out.flush().is_err() {
        return false;
    }
    let stdin = std::io::stdin();
    let fd = stdin.as_raw_fd();
    let mut buf = Vec::with_capacity(32);
    let mut byte = [0u8; 1];
    // ~250ms total budget: comfortably covers an SSH round-trip while staying
    // imperceptible. DA1 normally returns in a few ms regardless.
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(250);
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        let mut pfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let ms = remaining.as_millis().min(i32::MAX as u128) as libc::c_int;
        let n = unsafe { libc::poll(&mut pfd, 1, ms) };
        if n <= 0 {
            break; // timeout or poll error
        }
        match stdin.lock().read(&mut byte) {
            Ok(1) => {
                buf.push(byte[0]);
                if byte[0] == b'c' || buf.len() >= 64 {
                    break; // DA1 terminator (or runaway guard)
                }
            }
            _ => break,
        }
    }
    da1_advertises_sixel(&buf)
}

/// True when running inside Termux's terminal on Android. Termux does not set
/// `TERM_PROGRAM`, so detection keys off `TERMUX_VERSION` (exported by the
/// Termux app) and the `com.termux` install `PREFIX`.
pub fn is_termux_env(termux_version: Option<&str>, prefix: Option<&str>) -> bool {
    termux_version.is_some_and(|v| !v.is_empty())
        || prefix.is_some_and(|p| p.contains("com.termux"))
}

/// Cached `is_termux_env` over the live environment. Termux-ness is fixed for
/// the lifetime of the process, so the env lookup runs once even on the
/// per-keystroke hot path that consults it. Used only by the keymap (Ctrl
/// stands in for the absent Cmd key on Android); it does NOT enable inline
/// images, since mainline Termux cannot render OSC 1337.
pub fn detect_termux() -> bool {
    static CACHE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *CACHE.get_or_init(|| {
        is_termux_env(
            std::env::var("TERMUX_VERSION").ok().as_deref(),
            std::env::var("PREFIX").ok().as_deref(),
        )
    })
}

/// True when taps in editable areas should auto-raise croft's own on-screen
/// keyboard. Termux's tap-to-show-keyboard path is dead while an app has
/// mouse tracking active (croft always does, for click routing), so touch
/// users there otherwise have no way to type; `CROFT_FORCE_OSK` opts desktop
/// terminals in for testing. Same truthy values as the inline-image override.
pub fn osk_env(termux: bool, force: Option<&str>) -> bool {
    termux || force_inline_images(force)
}

/// Cached `osk_env` over the live environment, mirroring `detect_termux`.
pub fn detect_osk_auto() -> bool {
    static CACHE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *CACHE.get_or_init(|| {
        osk_env(
            detect_termux(),
            std::env::var("CROFT_FORCE_OSK").ok().as_deref(),
        )
    })
}

/// True for terminals that implement iTerm2's proprietary OSC-1337 inline
/// image + `SetColors` sequences. Ghostty is deliberately excluded: it parses
/// those sequences but does not implement them, so it must take the Kitty
/// graphics path and paint its welcome/editor bg with explicit truecolor
/// rather than relying on the ignored `SetColors=bg=srgb:…`.
pub fn is_iterm2_term_program(value: Option<&str>) -> bool {
    matches!(value, Some("iTerm.app") | Some("WezTerm"))
}

pub fn force_inline_images(value: Option<&str>) -> bool {
    value.is_some_and(|v| matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
}

pub fn is_tmux_env(term: Option<&str>, tmux_var: Option<&str>) -> bool {
    tmux_var.is_some_and(|v| !v.is_empty())
        || term.is_some_and(|v| v.starts_with("tmux") || v.starts_with("screen"))
}

pub fn detect_iterm2_inline_support() -> bool {
    if force_inline_images(std::env::var("CROFT_FORCE_INLINE_IMAGES").ok().as_deref()) {
        return true;
    }
    // Termux is deliberately NOT auto-enabled here. Mainline Termux does not
    // render the iTerm2 OSC 1337 inline-image protocol (the termux-app PR that
    // adds it is still unmerged), so emitting it dumps the raw base64 payload
    // to the screen. Termux falls back to the metadata-header line like any
    // other unsupported terminal; users on an OSC 1337 build can opt in with
    // `CROFT_FORCE_INLINE_IMAGES=1`.
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
    let scaled = image::imageops::resize(&img, new_w, new_h, image::imageops::FilterType::Lanczos3);
    let mut canvas: RgbaImage = ImageBuffer::from_pixel(canvas_w_px, canvas_h_px, bg);
    let off_x = ((canvas_w_px as i64) - (new_w as i64)) / 2;
    let off_y = ((canvas_h_px as i64) - (new_h as i64)) / 2;
    image::imageops::overlay(&mut canvas, &scaled, off_x, off_y);
    let mut out = Vec::with_capacity(8192);
    image::DynamicImage::ImageRgba8(canvas)
        .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)?;
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

/// Build a Kitty graphics-protocol transmit-and-display (`a=T`) sequence
/// placing `png` at the cursor across `width_cells` × `height_cells` cells.
/// `image_id` keys the transmitted image data; `placement_id` keys the on-
/// screen placement. Re-sending the same `(image_id, placement_id)` pair
/// REPLACES the existing placement in place (flicker-free per the Kitty spec)
/// rather than stacking a duplicate, which is what lets croft re-emit the
/// image every frame from the same per-overlay slot id. `C=1` keeps the
/// cursor from advancing; `q=2` suppresses the terminal's OK/error reply so
/// it never pollutes the crossterm stdin reader. Base64 is chunked at 4096
/// bytes (a multiple of 4) with `m=1` on every chunk but the last.
pub fn build_inline_image_kitty(
    png: &[u8],
    width_cells: u16,
    height_cells: u16,
    image_id: u32,
    placement_id: u32,
    z: i32,
) -> String {
    let b64 = base64::engine::general_purpose::STANDARD.encode(png);
    let len = b64.len();
    let mut out = String::with_capacity(len + 64);
    let mut start = 0usize;
    let mut first = true;
    // `z=` is only emitted when non-zero so the common above-text placements
    // stay byte-for-byte identical. A deep-negative z (see
    // `KITTY_Z_BELOW_TEXT_AND_BG`) drops the image beneath text AND non-default
    // background cells, letting an opaque tooltip box paint fully on top.
    let z_field = if z == 0 {
        String::new()
    } else {
        format!(",z={z}")
    };
    loop {
        let end = (start + 4096).min(len);
        let more = u8::from(end < len);
        if first {
            out.push_str(&format!(
                "\x1b_Gf=100,a=T,c={width_cells},r={height_cells},C=1,q=2,i={image_id},p={placement_id}{z_field},m={more};"
            ));
            first = false;
        } else {
            out.push_str(&format!("\x1b_Gm={more};"));
        }
        out.push_str(&b64[start..end]);
        out.push_str("\x1b\\");
        start = end;
        if start >= len {
            break;
        }
    }
    out
}

#[allow(dead_code)]
pub fn delete_kitty_image(image_id: u32) -> String {
    format!("\x1b_Ga=d,d=i,i={image_id},q=2\x1b\\")
}

/// Delete every on-screen Kitty placement (`d=a`), quietly (`q=2`). A plain
/// `\x1b[2J` clear evicts iTerm2's cached image cells but leaves Kitty
/// graphics painted on their separate layer, so the Kitty eviction path emits
/// this on the same frames the main loop clears the screen to repaint. The
/// currently-visible overlays then re-emit themselves on the post-draw flush.
pub fn delete_all_kitty_images() -> String {
    "\x1b_Ga=d,d=a,q=2\x1b\\".to_string()
}

/// Encode a straight-RGBA pixel buffer (`width`×`height`, 4 bytes/pixel) as a
/// DEC sixel DCS string. Colours are quantised to sixel's ≤256 palette
/// registers with NeuQuant (`color_quant`, already in-tree for GIF); only the
/// registers that actually appear are declared, which keeps the payload small
/// for the near-two-colour codicons. The image is emitted opaque — callers bake
/// the surrounding pane bg into the source exactly like the OSC/Kitty paths, so
/// no transparency handling is needed. Sixel values are 0–100 *percentages*,
/// not 0–255. Pixels are written in horizontal bands of six rows, one pass per
/// colour, with run-length compression (`!count char`).
pub fn encode_sixel_rgba(rgba: &[u8], width: u32, height: u32) -> String {
    let (w, h) = (width as usize, height as usize);
    if w == 0 || h == 0 || rgba.len() < w * h * 4 {
        return String::new();
    }
    // NeuQuant wants `colors` >= 64 and treats the buffer as RGBA. samplefac=1
    // is the highest quality; our overlays are tiny so the cost is negligible.
    let nq = color_quant::NeuQuant::new(1, 256, &rgba[..w * h * 4]);
    let palette = nq.color_map_rgb(); // 256 * 3 (R,G,B bytes)
    // Pixels below this alpha are left unpainted so the host's existing cell
    // content shows through (P2=1 below). The activity-bar icons bake an opaque
    // bg, so every pixel is painted; only letterboxed overlays (SSH empty state)
    // exercise the transparent path.
    const ALPHA_CUTOFF: u8 = 128;
    let mut indices = vec![0u8; w * h];
    let mut opaque = vec![false; w * h];
    for i in 0..w * h {
        let px = &rgba[i * 4..i * 4 + 4];
        if px[3] >= ALPHA_CUTOFF {
            opaque[i] = true;
            indices[i] = nq.index_of(px) as u8;
        }
    }

    let mut out = String::with_capacity(w * h / 4 + 512);
    // P2=1: positions left unset stay transparent (show prior cell content).
    out.push_str("\x1bP0;1;0q");
    out.push_str(&format!("\"1;1;{width};{height}")); // 1:1 aspect, raster size

    // Declare only the palette registers that painted (opaque) pixels use.
    let mut used = [false; 256];
    for i in 0..w * h {
        if opaque[i] {
            used[indices[i] as usize] = true;
        }
    }
    let scale = |v: u8| -> u32 { (v as u32 * 100 + 127) / 255 };
    for (c, is_used) in used.iter().enumerate() {
        if !is_used {
            continue;
        }
        let (r, g, b) = (palette[c * 3], palette[c * 3 + 1], palette[c * 3 + 2]);
        out.push_str(&format!("#{c};2;{};{};{}", scale(r), scale(g), scale(b)));
    }

    // One band per six rows; within a band, one left-to-right pass per colour.
    let mut band = 0usize;
    while band < h {
        let rows = (h - band).min(6);
        let mut first_color_in_band = true;
        for (c, is_used) in used.iter().enumerate() {
            if !is_used {
                continue;
            }
            // Build this colour's sixel chars for the whole row width.
            let mut row: Vec<u8> = Vec::with_capacity(w);
            let mut any = false;
            for x in 0..w {
                let mut bits = 0u8;
                for r in 0..rows {
                    let p = (band + r) * w + x;
                    if opaque[p] && indices[p] as usize == c {
                        bits |= 1 << r;
                    }
                }
                if bits != 0 {
                    any = true;
                }
                row.push(0x3F + bits);
            }
            if !any {
                continue; // this colour is absent from the band
            }
            if !first_color_in_band {
                out.push('$'); // graphics carriage return: overlay next colour
            }
            first_color_in_band = false;
            out.push_str(&format!("#{c}"));
            // Run-length encode the row.
            let mut x = 0usize;
            while x < w {
                let ch = row[x];
                let mut run = 1usize;
                while x + run < w && row[x + run] == ch {
                    run += 1;
                }
                if run >= 4 {
                    out.push_str(&format!("!{run}"));
                    out.push(ch as char);
                } else {
                    for _ in 0..run {
                        out.push(ch as char);
                    }
                }
                x += run;
            }
        }
        out.push('-'); // graphics new line: advance to the next band
        band += 6;
    }

    out.push_str("\x1b\\"); // ST: terminate the DCS
    out
}

/// Decode a baked PNG back to RGBA and encode it as sixel. The OSC/Kitty paths
/// base64 the PNG as-is; sixel needs raw pixels, so we pay one decode here. The
/// caller has already sized the PNG to an exact cell-pixel multiple, so the
/// sixel lands on the same cells.
pub fn build_inline_image_sixel(png: &[u8]) -> Option<String> {
    let img = image::load_from_memory(png).ok()?.to_rgba8();
    let (w, h) = (img.width(), img.height());
    Some(encode_sixel_rgba(img.as_raw(), w, h))
}

/// Single chokepoint that turns a baked PNG into the right inline-image escape
/// for the host terminal's protocol. `id` is the stable per-overlay slot id
/// used for both the Kitty image and placement ids (see `KITTY_ID_*`); it is
/// ignored on the iTerm2 and sixel paths. Returns `None` when no inline
/// protocol is available, so callers fall back to their text/metadata rendering.
pub fn build_inline_image(
    protocol: InlineImageProtocol,
    png: &[u8],
    width_cells: u16,
    height_cells: u16,
    preserve_aspect: bool,
    id: u32,
) -> Option<String> {
    build_inline_image_z(
        protocol,
        png,
        width_cells,
        height_cells,
        preserve_aspect,
        id,
        0,
    )
}

/// Kitty z-index that places an image below BOTH text glyphs and cells with a
/// non-default background (the spec threshold is INT32_MIN/2 = -1,073,741,824).
/// Used for sidebar illustrations so a tooltip's opaque box paints fully on top
/// while the illustration still shows through the panel's transparent reserved
/// cells. iTerm2's OSC-1337 has no z-index, so on that path the tooltip is
/// re-painted after the image instead (`App::flush_tooltip_over_image`).
pub const KITTY_Z_BELOW_TEXT_AND_BG: i32 = -2_000_000_000;
// The spec threshold for "below non-default background cells" is INT32_MIN/2.
const _: () = assert!(KITTY_Z_BELOW_TEXT_AND_BG < -1_073_741_824);

/// `build_inline_image` plus a Kitty z-index (ignored on the iTerm2/Sixel/None
/// paths, which have no layer ordering control).
pub fn build_inline_image_z(
    protocol: InlineImageProtocol,
    png: &[u8],
    width_cells: u16,
    height_cells: u16,
    preserve_aspect: bool,
    id: u32,
    z: i32,
) -> Option<String> {
    match protocol {
        InlineImageProtocol::ITerm2 => Some(build_inline_image_osc(
            png,
            width_cells,
            height_cells,
            preserve_aspect,
        )),
        InlineImageProtocol::Kitty => Some(build_inline_image_kitty(
            png,
            width_cells,
            height_cells,
            id,
            id,
            z,
        )),
        InlineImageProtocol::Sixel => build_inline_image_sixel(png),
        InlineImageProtocol::None => None,
    }
}

/// Stable Kitty image/placement ids, one per inline-image overlay slot. Re-
/// transmitting an overlay with its fixed id replaces its placement in place
/// instead of stacking duplicates. Namespaced well above the small ids other
/// programs typically use; croft owns the alt-screen so collisions are
/// unlikely regardless.
pub const KITTY_ID_BASE: u32 = 0x00C0_F700; // "croft"
pub const KITTY_ID_EXPLORER: u32 = KITTY_ID_BASE + 1;
pub const KITTY_ID_SEARCH: u32 = KITTY_ID_BASE + 2;
pub const KITTY_ID_SOURCE_CONTROL: u32 = KITTY_ID_BASE + 3;
pub const KITTY_ID_REMOTE: u32 = KITTY_ID_BASE + 4;
pub const KITTY_ID_RUN_DEBUG_BAR: u32 = KITTY_ID_BASE + 5;
pub const KITTY_ID_SETTINGS: u32 = KITTY_ID_BASE + 6;
// (KITTY_ID_BASE + 7 was the welcome-panel Codeberg badge, removed with the
// repo-link row; the slot is left unused rather than renumbering the rest.)
pub const KITTY_ID_RUN_DEBUG_PANEL: u32 = KITTY_ID_BASE + 8;
pub const KITTY_ID_HERO: u32 = KITTY_ID_BASE + 9;
pub const KITTY_ID_SSH: u32 = KITTY_ID_BASE + 10;
pub const KITTY_ID_WELCOME: u32 = KITTY_ID_BASE + 11;
/// Editor image slots, indexed by physical split side (`+0` left, `+1` right),
/// so they occupy `+12` and `+13`.
pub const KITTY_ID_EDITOR_BASE: u32 = KITTY_ID_BASE + 12;
/// Extensions activity-bar icon, past the editor pair to avoid collision.
pub const KITTY_ID_EXTENSIONS: u32 = KITTY_ID_BASE + 14;
/// Customize Layout toolbar icons (top-right of the editor / welcome). One
/// stable placement id each; the side-bar / panel ids swap their on/off image
/// in place, exactly like an activity icon swaps active/inactive.
pub const KITTY_ID_LAYOUT_SIDEBAR: u32 = KITTY_ID_BASE + 15;
pub const KITTY_ID_LAYOUT_PANEL: u32 = KITTY_ID_BASE + 16;
pub const KITTY_ID_LAYOUT_CUSTOMIZE: u32 = KITTY_ID_BASE + 17;
pub const KITTY_ID_MINIMAP: u32 = KITTY_ID_BASE + 18;
/// Source-control change-count badge, emitted at z=1 over the SCM icon.
pub const KITTY_ID_SCM_BADGE: u32 = KITTY_ID_BASE + 19;

/// Apply tmux DCS passthrough wrapping to an inline-image escape when needed.
/// Sixel passes through tmux natively (tmux built with sixel support renders it
/// directly), and the OSC/APC-oriented passthrough wrapper would corrupt it, so
/// sixel is never wrapped. iTerm2/Kitty escapes are wrapped only under tmux.
pub fn maybe_tmux_wrap(protocol: InlineImageProtocol, is_tmux: bool, raw: String) -> String {
    if is_tmux && protocol != InlineImageProtocol::Sixel {
        tmux_passthrough_wrap(&raw)
    } else {
        raw
    }
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
    fn count_badge_widens_only_past_two_digits() {
        assert_eq!(count_badge_cells_w(1), 2);
        assert_eq!(count_badge_cells_w(99), 2);
        assert_eq!(count_badge_cells_w(100), 3);
    }

    #[test]
    fn count_badge_renders_pill_and_digits() {
        let bg = Rgba([0x20, 0x20, 0x20, 0xff]);
        let accent = (0x4e, 0x9a, 0xff);
        let png = compose_count_badge(3, 9, 18, accent, bg).expect("badge png");
        let img = image::load_from_memory(&png).expect("decode").to_rgba8();
        // 1 digit -> 2 cells wide, 1 cell tall.
        assert_eq!((img.width(), img.height()), (18, 18));
        let has = |p: Rgba<u8>| img.pixels().any(|q| *q == p);
        assert!(has(Rgba([accent.0, accent.1, accent.2, 0xff])), "accent pill");
        assert!(has(Rgba([0xff, 0xff, 0xff, 0xff])), "white digit");
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
    fn osk_env_arms_on_termux_or_explicit_force() {
        assert!(osk_env(true, None));
        assert!(osk_env(false, Some("1")));
        assert!(osk_env(false, Some("true")));
        assert!(!osk_env(false, Some("0")));
        assert!(!osk_env(false, None));
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
    fn termux_version_var_detected() {
        assert!(is_termux_env(Some("0.118.0"), None));
    }

    #[test]
    fn termux_prefix_detected() {
        assert!(is_termux_env(None, Some("/data/data/com.termux/files/usr")));
    }

    #[test]
    fn empty_termux_version_is_not_termux() {
        assert!(!is_termux_env(Some(""), None));
    }

    #[test]
    fn non_termux_prefix_is_not_termux() {
        assert!(!is_termux_env(None, Some("/usr")));
    }

    #[test]
    fn missing_termux_signals_is_not_termux() {
        assert!(!is_termux_env(None, None));
    }

    #[test]
    fn termux_does_not_auto_enable_inline_images() {
        // Mainline Termux cannot render the iTerm2 OSC 1337 protocol (the
        // termux-app PR adding it is unmerged), so emitting it would dump raw
        // base64 to the screen. A Termux session with no other signals must
        // resolve to None, falling back to the metadata-header line.
        assert_eq!(
            inline_image_protocol_for(None, None),
            InlineImageProtocol::None
        );
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
    fn compose_icon_positive_off_y_bias_lowers_the_glyph_and_never_clips() {
        // A small opaque square so the composed glyph is distinguishable from
        // the background regardless of where it lands.
        let src = {
            let img: RgbaImage = ImageBuffer::from_pixel(4, 4, Rgba([0xff, 0xff, 0xff, 0xff]));
            let mut buf = Vec::new();
            image::DynamicImage::ImageRgba8(img)
                .write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
                .unwrap();
            buf
        };
        let bg = Rgba([0x1e, 0x22, 0x2e, 0xff]);
        // Rows that contain any non-bg (glyph) pixel, top-most and bottom-most.
        let content_span = |png: &[u8]| -> (u32, u32) {
            let img = image::load_from_memory_with_format(png, image::ImageFormat::Png)
                .unwrap()
                .to_rgba8();
            let (mut top, mut bottom) = (None, None);
            for y in 0..img.height() {
                let has = (0..img.width()).any(|x| img.get_pixel(x, y).0 != bg.0);
                if has {
                    top.get_or_insert(y);
                    bottom = Some(y);
                }
            }
            (top.unwrap(), bottom.unwrap())
        };
        let (canvas_w, canvas_h) = (32u32, 48u32);
        let centered = compose_icon(&src, canvas_w, canvas_h, false, bg, 0).unwrap();
        // A bias far larger than the canvas: clamp must keep the glyph inside.
        let lowered = compose_icon(&src, canvas_w, canvas_h, false, bg, 10_000).unwrap();
        let (c_top, c_bottom) = content_span(&centered);
        let (l_top, l_bottom) = content_span(&lowered);
        assert!(
            l_top > c_top && l_bottom > c_bottom,
            "a positive bias must push the glyph downward (centered span {c_top}..={c_bottom}, lowered {l_top}..={l_bottom})"
        );
        assert!(
            l_bottom < canvas_h,
            "the glyph must stay inside the canvas: bottom row {l_bottom} exceeds {}",
            canvas_h - 1
        );
        assert_eq!(
            l_bottom - l_top,
            c_bottom - c_top,
            "clamping must preserve the glyph height (no clipping), only shift it"
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
        let baked = compose_icon(&src, 32, 16, false, bg, 0).unwrap();
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
    fn source_control_activity_bar_icon_is_the_no_repo_hero_png() {
        let baked = NO_REPO_HERO_PNG;
        let decoded = image::load_from_memory_with_format(baked, image::ImageFormat::Png)
            .expect("the shipped source-control PNG must decode")
            .to_rgba8();
        let (w, h) = (decoded.width(), decoded.height());
        assert!(
            h > w,
            "the asset the user supplied is a portrait Y-fork; if the file gets swapped for a square one the activity-bar render will read squashed"
        );
        let corner = decoded.get_pixel(0, 0).0;
        assert_eq!(
            corner[3], 0x00,
            "the asset must have a transparent background so compose_icon's bg fill stays clean"
        );
    }

    #[test]
    fn compose_icon_preserves_source_aspect_ratio_for_a_portrait_png() {
        let bg = Rgba([0x1e, 0x22, 0x2e, 0xff]);
        let baked = compose_icon(NO_REPO_HERO_PNG, 32, 16, false, bg, 0)
            .expect("compose_icon must accept the portrait source-control PNG");
        let decoded = image::load_from_memory_with_format(&baked, image::ImageFormat::Png)
            .unwrap()
            .to_rgba8();
        assert_eq!(
            (decoded.width(), decoded.height()),
            (32, 16),
            "canvas dimensions must match the requested size regardless of source aspect"
        );
        let left_corner = decoded.get_pixel(0, 0).0;
        let right_corner = decoded.get_pixel(31, 0).0;
        assert_eq!(
            left_corner,
            [bg[0], bg[1], bg[2], bg[3]],
            "a portrait source must leave bar-bg fill on the left of the icon (no squash to fill the square)"
        );
        assert_eq!(
            right_corner,
            [bg[0], bg[1], bg[2], bg[3]],
            "and on the right too"
        );
    }

    #[test]
    fn compose_icon_svg_rasterises_codicon_to_canvas_size() {
        // The activity-bar icons now come from SVG codicons rasterised at the
        // exact icon size. The result must be a PNG of exactly the requested
        // canvas dimensions, carry an opaque glyph, and keep the bar-bg fill in
        // the corners (the codicon never bleeds to the canvas edge).
        let bg = Rgba([0x1e, 0x22, 0x2e, 0xff]);
        let baked = compose_icon_svg(EXPLORER_SRC_SVG, 40, 40, IconState::Inactive, bg, 0)
            .expect("the files codicon SVG must rasterise");
        let decoded = image::load_from_memory_with_format(&baked, image::ImageFormat::Png)
            .unwrap()
            .to_rgba8();
        assert_eq!((decoded.width(), decoded.height()), (40, 40));
        assert_eq!(
            decoded.get_pixel(0, 0).0,
            [bg[0], bg[1], bg[2], bg[3]],
            "corner must stay bar-bg; the glyph sits centred inside a margin"
        );
        let glyph_opaque = decoded
            .pixels()
            .any(|p| p.0 != [bg[0], bg[1], bg[2], bg[3]]);
        assert!(
            glyph_opaque,
            "the rasterised glyph must paint visible pixels"
        );
    }

    #[test]
    fn compose_icon_svg_active_paints_the_blue_pill() {
        let bg = Rgba([0x1e, 0x22, 0x2e, 0xff]);
        let baked = compose_icon_svg(EXPLORER_SRC_SVG, 40, 40, IconState::Active, bg, 0).unwrap();
        let decoded = image::load_from_memory_with_format(&baked, image::ImageFormat::Png)
            .unwrap()
            .to_rgba8();
        // The active variant draws the blue selection pill down the left edge.
        let mut saw_pill = false;
        for y in 0..40 {
            if decoded.get_pixel(0, y).0 == [ACTIVE_PILL[0], ACTIVE_PILL[1], ACTIVE_PILL[2], 0xff] {
                saw_pill = true;
                break;
            }
        }
        assert!(saw_pill, "active icon must carry the left-edge blue pill");
    }

    #[test]
    fn compose_icon_svg_hovered_brightens_glyph_without_the_pill() {
        // The hovered variant wears the active (bright) tint so the user sees
        // the icon light up under the pointer, but it must NOT paint the blue
        // selection pill — that bar is reserved for the actually-selected view.
        let bg = Rgba([0x1e, 0x22, 0x2e, 0xff]);
        let hovered =
            compose_icon_svg(EXPLORER_SRC_SVG, 40, 40, IconState::Hovered, bg, 0).unwrap();
        let active = compose_icon_svg(EXPLORER_SRC_SVG, 40, 40, IconState::Active, bg, 0).unwrap();
        let inactive =
            compose_icon_svg(EXPLORER_SRC_SVG, 40, 40, IconState::Inactive, bg, 0).unwrap();
        let decode = |b: &[u8]| {
            image::load_from_memory_with_format(b, image::ImageFormat::Png)
                .unwrap()
                .to_rgba8()
        };
        let (hov, inact) = (decode(&hovered), decode(&inactive));
        // No blue pill anywhere down the left edge of the hovered glyph.
        let pill = [ACTIVE_PILL[0], ACTIVE_PILL[1], ACTIVE_PILL[2], 0xff];
        assert!(
            (0..40).all(|y| hov.get_pixel(0, y).0 != pill),
            "hovered icon must not carry the selection pill"
        );
        // The brighter tint makes the hovered render differ from the muted
        // resting one, and the (pill-bearing) active render differs from both.
        assert_ne!(
            hovered, inactive,
            "hover must visibly brighten the resting glyph"
        );
        assert_ne!(
            hovered, active,
            "hover (no pill) must stay distinct from the selected state"
        );
        // Sanity: both renders are still the requested canvas size.
        assert_eq!((hov.width(), hov.height()), (40, 40));
        assert_eq!((inact.width(), inact.height()), (40, 40));
    }

    #[test]
    fn run_debug_src_png_is_192x192_with_visible_bug_and_triangle() {
        let decoded =
            image::load_from_memory_with_format(RUN_DEBUG_SRC_PNG, image::ImageFormat::Png)
                .expect("rsvg-convert output must decode as a PNG")
                .to_rgba8();
        assert_eq!(decoded.width(), 192);
        assert_eq!(decoded.height(), 192);
        // Pixel coordinates derived empirically from `rsvg-convert -w 192
        // -h 192 assets/icons/debug-alt.svg` on 2026-05-09. They sit deep
        // inside each subshape rather than on its antialiased edge so a
        // future re-render of the SVG (different rsvg version, slightly
        // different antialias kernel) keeps the assertion meaningful.
        let bug_body = decoded.get_pixel(40, 124).0;
        assert!(
            bug_body[3] >= 0xc0,
            "bug body fill at (40,124) must be (mostly) opaque; got alpha {} — codicon debug-alt's body sits in the lower-left quadrant of a 192x192 render",
            bug_body[3]
        );
        let triangle_stroke = decoded.get_pixel(48, 32).0;
        assert!(
            triangle_stroke[3] >= 0xc0,
            "play-triangle's left stroke at (48,32) must be (mostly) opaque; got alpha {}",
            triangle_stroke[3]
        );
        let corner = decoded.get_pixel(0, 0).0;
        assert_eq!(
            corner[3], 0x00,
            "outside the silhouette must stay transparent so compose_icon's bg fills it"
        );
    }

    #[test]
    fn run_debug_icon_composes_through_icon_pipeline() {
        let bg = Rgba([0x1e, 0x22, 0x2e, 0xff]);
        let baked = compose_icon(RUN_DEBUG_SRC_PNG, 32, 16, false, bg, 0)
            .expect("compose_icon must accept the SVG-rasterised run-debug PNG");
        let decoded = image::load_from_memory_with_format(&baked, image::ImageFormat::Png)
            .unwrap()
            .to_rgba8();
        assert_eq!((decoded.width(), decoded.height()), (32, 16));
    }

    #[test]
    fn portrait_icon_paints_at_least_as_tall_as_a_square_icon_in_the_same_cell() {
        // Activity-bar cells are roughly square (4 cells × 2 cells, with cell
        // pixel ratio ≈ 1:2). Under the old fit-inside-target_max math a 2:3
        // portrait source painted only target_max × (target_max·2/3), so the
        // glyph looked visibly smaller than the square sibling icons. Allow
        // the portrait icon to extend beyond the inner target square (still
        // capped at the canvas itself) so its painted height matches the
        // square baseline.
        let bg = Rgba([0x1e, 0x22, 0x2e, 0xff]);
        let canvas_w = 40u32;
        let canvas_h = 40u32;
        let baked = compose_icon(NO_REPO_HERO_PNG, canvas_w, canvas_h, false, bg, 0)
            .expect("portrait source must compose into the activity-bar cell");
        let decoded = image::load_from_memory_with_format(&baked, image::ImageFormat::Png)
            .unwrap()
            .to_rgba8();
        let opaque_rows: u32 = (0..canvas_h)
            .filter(|&y| {
                (0..canvas_w).any(|x| {
                    let p = decoded.get_pixel(x, y).0;
                    [p[0], p[1], p[2], p[3]] != [bg[0], bg[1], bg[2], bg[3]]
                })
            })
            .count() as u32;
        let target_max = (canvas_w.min(canvas_h).saturating_sub(4) * 9 / 10).max(8);
        let alpha_edge_slack = 2;
        assert!(
            opaque_rows + alpha_edge_slack >= target_max,
            "portrait icon painted {} rows in a {}x{} cell; expected close to target_max ({}) (within {} rows of slack for the Lanczos alpha edges) so it visually matches the square sibling icons",
            opaque_rows,
            canvas_w,
            canvas_h,
            target_max,
            alpha_edge_slack
        );
    }

    #[test]
    fn tmux_passthrough_wrap_doubles_inner_escapes() {
        let inner = "\x1b]1337;File=inline=1:DATA\x07";
        let wrapped = tmux_passthrough_wrap(inner);
        assert!(wrapped.starts_with("\x1bPtmux;"), "DCS prefix");
        assert!(wrapped.ends_with("\x1b\\"), "ST terminator");
        assert!(
            wrapped.contains("\x1b\x1b]1337"),
            "inner ESC must be doubled"
        );
    }

    #[test]
    fn kitty_graphics_starts_with_apc_introducer() {
        let seq = build_inline_image_kitty(b"PNGDATA", 4, 3, 7, 7, 0);
        assert!(seq.starts_with("\x1b_G"), "wrong prefix: {seq:?}");
    }

    #[test]
    fn kitty_graphics_terminates_with_st() {
        let seq = build_inline_image_kitty(b"PNGDATA", 4, 3, 7, 7, 0);
        assert!(seq.ends_with("\x1b\\"), "must end with ST: {seq:?}");
    }

    #[test]
    fn kitty_first_chunk_carries_png_format_action_and_cells() {
        let seq = build_inline_image_kitty(b"PNGDATA", 4, 3, 7, 7, 0);
        assert!(seq.contains("f=100"), "PNG format: {seq:?}");
        assert!(seq.contains("a=T"), "transmit+display: {seq:?}");
        assert!(seq.contains("c=4"), "width in cells: {seq:?}");
        assert!(seq.contains("r=3"), "height in cells: {seq:?}");
        assert!(seq.contains("i=7"), "image id: {seq:?}");
    }

    #[test]
    fn kitty_omits_z_when_zero_but_emits_deep_negative_for_below_text() {
        // z=0 keeps the default above-text placement and emits no z field, so
        // the common icon/logo sequences stay byte-identical.
        let above = build_inline_image_kitty(b"PNGDATA", 4, 3, 7, 7, 0);
        assert!(!above.contains("z="), "no z field when z=0: {above:?}");
        // A sidebar illustration is placed below text AND non-default-bg cells
        // (z below the INT32_MIN/2 spec threshold, guarded at compile time
        // beside the constant) so an opaque tooltip box can paint fully on top.
        let below = build_inline_image_kitty(b"PNGDATA", 4, 3, 7, 7, KITTY_Z_BELOW_TEXT_AND_BG);
        assert!(
            below.contains(&format!("z={KITTY_Z_BELOW_TEXT_AND_BG}")),
            "deep-negative z must be present: {below:?}"
        );
    }

    #[test]
    fn kitty_first_chunk_carries_a_stable_placement_id() {
        // Re-transmitting the same image id with the same placement id replaces
        // the placement in place (flicker-free) instead of stacking a duplicate,
        // which is what lets croft's per-frame re-emit loop work on Kitty.
        let seq = build_inline_image_kitty(b"PNGDATA", 4, 3, 7, 42, 0);
        assert!(
            seq.contains("p=42"),
            "placement id must be present: {seq:?}"
        );
    }

    #[test]
    fn kitty_sets_cursor_and_response_suppression_flags() {
        let seq = build_inline_image_kitty(b"PNGDATA", 4, 3, 7, 7, 0);
        assert!(seq.contains("C=1"), "cursor must not advance: {seq:?}");
        assert!(seq.contains("q=2"), "responses must be suppressed: {seq:?}");
    }

    #[test]
    fn kitty_payload_is_base64_encoded_png() {
        let seq = build_inline_image_kitty(b"PNGDATA", 4, 3, 7, 7, 0);
        assert!(
            seq.contains(";UE5HREFUQQ=="),
            "expected base64 payload after the control-data semicolon: {seq:?}"
        );
    }

    #[test]
    fn kitty_chunks_payload_larger_than_4096_base64_bytes() {
        let png = vec![0xABu8; 4000];
        let seq = build_inline_image_kitty(&png, 4, 3, 7, 7, 0);
        let chunks: Vec<&str> = seq.split("\x1b\\").filter(|s| !s.is_empty()).collect();
        assert!(
            chunks.len() >= 2,
            "4000 raw bytes must span >1 chunk, got {}",
            chunks.len()
        );
        assert!(
            chunks[0].contains("m=1") && chunks[0].contains("f=100"),
            "first chunk carries full control data and m=1: {:?}",
            chunks[0]
        );
        assert!(
            chunks.last().unwrap().contains("m=0"),
            "final chunk must carry m=0: {:?}",
            chunks.last().unwrap()
        );
        for mid in &chunks[1..chunks.len() - 1] {
            assert!(
                mid.contains("m=1") && !mid.contains("f=100"),
                "middle chunks carry only m=, no repeated control data: {mid:?}"
            );
        }
    }

    #[test]
    fn iterm2_and_wezterm_speak_the_iterm2_protocol() {
        assert_eq!(
            inline_image_protocol_for(Some("iTerm.app"), None),
            InlineImageProtocol::ITerm2
        );
        assert_eq!(
            inline_image_protocol_for(Some("WezTerm"), None),
            InlineImageProtocol::ITerm2
        );
    }

    #[test]
    fn ghostty_speaks_the_kitty_protocol_not_iterm2() {
        assert_eq!(
            inline_image_protocol_for(Some("ghostty"), None),
            InlineImageProtocol::Kitty
        );
    }

    #[test]
    fn kitty_term_is_detected_via_term_env() {
        assert_eq!(
            inline_image_protocol_for(None, Some("xterm-kitty")),
            InlineImageProtocol::Kitty
        );
    }

    #[test]
    fn ghostty_term_is_detected_via_term_env_over_ssh() {
        // Over SSH, TERM_PROGRAM is dropped but TERM=xterm-ghostty survives;
        // the remote croft must still resolve the Kitty protocol from it.
        assert_eq!(
            inline_image_protocol_for(None, Some("xterm-ghostty")),
            InlineImageProtocol::Kitty
        );
    }

    #[test]
    fn apple_terminal_and_unknown_have_no_inline_protocol() {
        assert_eq!(
            inline_image_protocol_for(Some("Apple_Terminal"), Some("xterm-256color")),
            InlineImageProtocol::None
        );
        assert_eq!(
            inline_image_protocol_for(None, None),
            InlineImageProtocol::None
        );
    }

    #[test]
    fn switch_warning_fires_only_for_imageless_non_termux_terminals() {
        // Apple Terminal and friends: no protocol, not Termux -> warn.
        assert!(terminal_warrants_switch_warning(
            InlineImageProtocol::None,
            false
        ));
        // Every rendering protocol suppresses the warning.
        assert!(!terminal_warrants_switch_warning(
            InlineImageProtocol::ITerm2,
            false
        ));
        assert!(!terminal_warrants_switch_warning(
            InlineImageProtocol::Kitty,
            false
        ));
        assert!(!terminal_warrants_switch_warning(
            InlineImageProtocol::Sixel,
            false
        ));
        // Termux renders no images but is an intentional Android target where
        // the recommended terminals don't exist, so it is never warned.
        assert!(!terminal_warrants_switch_warning(
            InlineImageProtocol::None,
            true
        ));
    }

    #[test]
    fn da1_sixel_attribute_is_detected_as_standalone_token() {
        // VT340-style reply with sixel (4) present.
        assert!(da1_advertises_sixel(b"\x1b[?62;1;4;6;9;15c"));
        // Konsole-style.
        assert!(da1_advertises_sixel(b"\x1b[?62;4c"));
        // Sixel as the first attribute.
        assert!(da1_advertises_sixel(b"\x1b[?4;6c"));
    }

    #[test]
    fn da1_without_sixel_is_rejected() {
        // Apple Terminal: VT100 with AVO, no sixel.
        assert!(!da1_advertises_sixel(b"\x1b[?1;2c"));
        // The `4` must be a whole token, not a digit inside 14 / 64.
        assert!(!da1_advertises_sixel(b"\x1b[?62;14;64c"));
        // Garbage / no envelope.
        assert!(!da1_advertises_sixel(b""));
        assert!(!da1_advertises_sixel(b"hello"));
    }

    #[test]
    fn encode_sixel_wraps_in_dcs_and_declares_palette() {
        // 2x2 solid red, opaque.
        let rgba = [255u8, 0, 0, 255].repeat(4);
        let seq = encode_sixel_rgba(&rgba, 2, 2);
        assert!(seq.starts_with("\x1bP0;1;0q"), "DCS sixel intro: {seq:?}");
        assert!(seq.contains("\"1;1;2;2"), "raster attrs: {seq:?}");
        assert!(seq.contains('#'), "declares a palette register: {seq:?}");
        assert!(seq.ends_with("\x1b\\"), "ST terminator: {seq:?}");
    }

    #[test]
    fn encode_sixel_rejects_degenerate_input() {
        assert_eq!(encode_sixel_rgba(&[], 0, 0), "");
        // buffer too small for the claimed dimensions
        assert_eq!(encode_sixel_rgba(&[0, 0, 0, 0], 4, 4), "");
    }

    #[test]
    fn build_inline_image_dispatches_sixel() {
        // A 2x2 opaque PNG round-trips through the dispatch into a sixel DCS.
        let mut png = Vec::new();
        let img: RgbaImage = ImageBuffer::from_pixel(2, 2, Rgba([10, 200, 30, 255]));
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let seq = build_inline_image(InlineImageProtocol::Sixel, &png, 2, 2, false, 0)
            .expect("sixel dispatch");
        assert!(seq.starts_with("\x1bP"), "is a sixel DCS: {seq:?}");
        assert!(seq.ends_with("\x1b\\"));
    }

    #[test]
    fn maybe_tmux_wrap_skips_sixel_but_wraps_others() {
        let raw = "\x1bPq#0;2;0;0;0-\x1b\\".to_string();
        // Sixel is never wrapped, even under tmux.
        assert_eq!(
            maybe_tmux_wrap(InlineImageProtocol::Sixel, true, raw.clone()),
            raw
        );
        // iTerm2 under tmux IS wrapped.
        let wrapped = maybe_tmux_wrap(InlineImageProtocol::ITerm2, true, raw.clone());
        assert!(
            wrapped.starts_with("\x1bPtmux;"),
            "tmux-wrapped: {wrapped:?}"
        );
        // Not under tmux: unchanged.
        assert_eq!(
            maybe_tmux_wrap(InlineImageProtocol::ITerm2, false, raw.clone()),
            raw
        );
    }

    #[test]
    fn delete_kitty_image_deletes_by_id_quietly() {
        let seq = delete_kitty_image(7);
        assert!(seq.starts_with("\x1b_G"), "APC prefix: {seq:?}");
        assert!(seq.ends_with("\x1b\\"), "ST terminator: {seq:?}");
        assert!(seq.contains("a=d"), "delete action: {seq:?}");
        assert!(seq.contains("d=i"), "delete by id selector: {seq:?}");
        assert!(seq.contains("i=7"), "target id: {seq:?}");
        assert!(seq.contains("q=2"), "delete must be quiet too: {seq:?}");
    }

    #[test]
    fn delete_all_kitty_images_clears_every_placement_quietly() {
        // `\x1b[2J` evicts iTerm2's image cells but leaves Kitty graphics on
        // their layer, so the Kitty eviction path needs an explicit delete-all
        // (d=a) on the frames croft clears to repaint.
        let seq = delete_all_kitty_images();
        assert!(seq.starts_with("\x1b_G"), "APC prefix: {seq:?}");
        assert!(seq.ends_with("\x1b\\"), "ST terminator: {seq:?}");
        assert!(seq.contains("a=d"), "delete action: {seq:?}");
        assert!(seq.contains("d=a"), "delete-all selector: {seq:?}");
        assert!(seq.contains("q=2"), "delete must be quiet too: {seq:?}");
    }

    #[test]
    fn ghostty_is_not_classified_as_iterm2() {
        // Ghostty parses but ignores the iTerm2 OSC-1337 / SetColors sequences;
        // it must take the Kitty path and the explicit-truecolor welcome bg, so
        // the iTerm2-specific predicate must reject it.
        assert!(!is_iterm2_term_program(Some("ghostty")));
    }

    #[test]
    fn build_inline_image_dispatches_on_protocol() {
        let osc = build_inline_image(InlineImageProtocol::ITerm2, b"PNGDATA", 4, 3, false, 11)
            .expect("iTerm2 must produce a sequence");
        assert!(
            osc.starts_with("\x1b]1337;File="),
            "iTerm2 → OSC-1337: {osc:?}"
        );

        let kitty = build_inline_image(InlineImageProtocol::Kitty, b"PNGDATA", 4, 3, false, 11)
            .expect("Kitty must produce a sequence");
        assert!(
            kitty.starts_with("\x1b_G"),
            "Kitty → APC graphics: {kitty:?}"
        );
        assert!(
            kitty.contains("i=11") && kitty.contains("p=11"),
            "Kitty uses the id for both image and placement: {kitty:?}"
        );

        assert!(
            build_inline_image(InlineImageProtocol::None, b"PNGDATA", 4, 3, false, 11).is_none(),
            "no inline protocol → no sequence"
        );
    }
}
