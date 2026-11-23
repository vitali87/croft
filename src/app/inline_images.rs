//! Pictures in the terminal: support detection, the background chrome bake, sending
//! and clearing each picture every frame (the activity-bar icons included), the
//! editor, terminal and Markdown overlays, and the minimap.

use super::*;

pub(super) const FALLBACK_CELL_PIXEL: (u32, u32) = (10, 20);

/// One cell's size in pixels, from a window size report of `cols` x `rows`
/// cells over `width` x `height` pixels; `None` when the terminal reports
/// no pixel size.
pub(super) fn cell_pixel_of(cols: u16, rows: u16, width: u16, height: u16) -> Option<(u32, u32)> {
    (cols > 0 && rows > 0 && width > 0 && height > 0)
        .then(|| ((width / cols).max(1) as u32, (height / rows).max(1) as u32))
}

impl ChromeBakeInput {
    /// Whether pictures baked for `shown` must come down while this bake
    /// runs, rather than stay up until it lands: across a theme switch they
    /// carry the old theme's ink and background, and on sixel, where a
    /// picture is sized in pixels, across a cell-size change they outgrow
    /// their cells (#1843). A font zoom on Kitty or iTerm2 only rescales them.
    pub(super) fn outdates(&self, shown: &ChromeBakeInput) -> bool {
        self.protocol != shown.protocol
            || self.icon_bg != shown.icon_bg
            || self.icon_ink != shown.icon_ink
            || self.sponsor_ink != shown.sponsor_ink
            || (self.protocol == crate::iterm2_inline::InlineImageProtocol::Sixel
                && self.cell != shown.cell)
    }
}

/// Which edge of the editor pane the minimap strip sits on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MinimapSide {
    Left,
    Right,
}

/// Width of the minimap strip in cells (the user picked the ~6-cell option).
pub(super) const MINIMAP_WIDTH_CELLS: u16 = 6;

/// Minimum editor-text width that must remain after carving the strip; below
/// this the minimap is suppressed so the code never gets squeezed to nothing.
pub(super) const MINIMAP_MIN_EDITOR_WIDTH: u16 = 44;

/// While only the text changes, the minimap image is re-baked at most this
/// often: each bake is a fresh PNG of the whole strip (#682).
pub(super) const MINIMAP_EDIT_REBAKE: std::time::Duration = std::time::Duration::from_millis(300);

/// Maximum pixel height per source line in the minimap. Only kicks in for very
/// short files: it caps a six-line file to a compact sliver instead of
/// stretching those lines down the whole strip. Any file long enough that its
/// natural fit is already <= this (roughly > a screenful) fills the strip
/// exactly as a plain whole-file fit would, so normal files are unaffected.
pub(super) const MINIMAP_MAX_LINE_PX: u32 = 6;

/// Source characters folded into one braille dot column of the text minimap,
/// so the 6-cell strip spans 48 columns of code, like the image's 1px a char.
pub(super) const MINIMAP_TEXT_CHARS_PER_DOT: u32 = 4;

/// One cell of the text minimap: its braille glyph and ink colour, or `None`
/// for a blank cell.
pub(super) type BrailleCell = Option<(char, (u8, u8, u8))>;

/// Fold a minimap raster into braille cells for terminals that cannot draw
/// images (#1231): each cell is 2x4 dots, each dot `MINIMAP_TEXT_CHARS_PER_DOT`
/// raster columns wide and one raster row tall, lit when any of its pixels is
/// ink (not `bg`). A cell takes the colour of its first lit pixel. Cells come
/// back row-major, `cell_w` to a row.
pub(super) fn braille_minimap(
    rgba: &[u8],
    w: u32,
    h: u32,
    bg: (u8, u8, u8),
    cell_w: u16,
    cell_h: u16,
) -> Vec<BrailleCell> {
    // Braille dot bits by (dot column, dot row), U+2800 + bits.
    const BITS: [[u8; 4]; 2] = [[0x01, 0x02, 0x04, 0x40], [0x08, 0x10, 0x20, 0x80]];
    let xs = MINIMAP_TEXT_CHARS_PER_DOT;
    let ink = |x: u32, y: u32| -> Option<(u8, u8, u8)> {
        if x >= w || y >= h {
            return None;
        }
        let i = ((y * w + x) * 4) as usize;
        let px = (rgba[i], rgba[i + 1], rgba[i + 2]);
        (px != bg).then_some(px)
    };
    let mut cells = Vec::with_capacity(cell_w as usize * cell_h as usize);
    for cy in 0..cell_h as u32 {
        for cx in 0..cell_w as u32 {
            let mut bits = 0u8;
            let mut colour = None;
            for (dx, column) in BITS.iter().enumerate() {
                for (dy, bit) in column.iter().enumerate() {
                    let y = cy * 4 + dy as u32;
                    let x0 = (cx * 2 + dx as u32) * xs;
                    if let Some(px) = (x0..x0 + xs).find_map(|x| ink(x, y)) {
                        bits |= bit;
                        colour.get_or_insert(px);
                    }
                }
            }
            cells.push(colour.map(|c| (char::from_u32(0x2800 + bits as u32).unwrap_or(' '), c)));
        }
    }
    cells
}

pub(super) type MinimapSig = (
    Option<PathBuf>,
    u64,
    u32,
    u32,
    (u8, u8, u8),
    (usize, usize),
    u64,
);

/// Composite the per-frame overlays onto the cached raster: the translucent
/// viewport box (style A) blending the visible-line rows toward white (dark
/// theme) or black (light), plus a thin left-edge accent in the selection color
/// marking the selected rows. The accent is deliberately tiny so the minimap's
/// look and feel is unchanged. Returns a fresh buffer.
pub(super) fn composite_minimap_viewport(
    base: &[u8],
    w: u32,
    h: u32,
    ov: MinimapOverlay,
) -> Vec<u8> {
    let MinimapOverlay {
        top,
        rows,
        total,
        light,
        selection,
        sel_rgb,
        content_h,
    } = ov;
    let mut buf = base.to_vec();
    if total == 0 || w == 0 || h == 0 {
        return buf;
    }
    // Map a 0-based line index to a pixel row, matching `minimap_rgba`'s
    // top-aligned `content_h` scale.
    let row_y = |line: usize| (line as u64 * content_h as u64 / total as u64) as u32;
    // Blend cols `x0..x1` of rows `r0..r1` toward `tint` by alpha `a` (0..=255).
    let band = |buf: &mut [u8], r0: u32, r1: u32, x0: u32, x1: u32, tint: [u16; 3], a: u16| {
        for y in r0..r1.min(h) {
            for x in x0..x1.min(w) {
                let idx = ((y * w + x) * 4) as usize;
                for k in 0..3 {
                    let c = buf[idx + k] as u16;
                    buf[idx + k] = ((c * (255 - a) + tint[k] * a) / 255) as u8;
                }
            }
        }
    };
    // Viewport box: a translucent fill plus a crisp bright border so "where am
    // I" is unmistakable. Clamp the visible range to the line count so an
    // all-visible short file's box covers only its content, not the empty
    // strip below it.
    let y0 = row_y(top);
    let y1 = row_y((top + rows).min(total)).clamp(y0 + 1, h);
    // Fill (toward white on dark, black on light), then the four edges at a
    // much higher alpha so the rectangle outline reads against both the dark
    // strip and the colored code.
    let fill: [u16; 3] = if light { [0, 0, 0] } else { [255, 255, 255] };
    let edge: [u16; 3] = if light { [20, 20, 20] } else { [235, 240, 250] };
    band(&mut buf, y0, y1, 0, w, fill, 46);
    band(&mut buf, y0, y0 + 1, 0, w, edge, 200); // top
    band(&mut buf, y1.saturating_sub(1), y1, 0, w, edge, 200); // bottom
    band(&mut buf, y0, y1, 0, 1, edge, 200); // left
    band(&mut buf, y0, y1, w.saturating_sub(1), w, edge, 200); // right
    // Selection: a thin (~2-3px) accent stripe down the left edge of the
    // selected rows, in the selection color. Tiny on purpose.
    if let Some((s, e)) = selection {
        let r0 = row_y(s);
        let r1 = row_y(e + 1).clamp(r0 + 1, h);
        let aw = (w / 12).clamp(1, 3);
        band(
            &mut buf,
            r0,
            r1,
            0,
            aw,
            [sel_rgb.0 as u16, sel_rgb.1 as u16, sel_rgb.2 as u16],
            210,
        );
    }
    buf
}

/// Encode an RGBA pixel buffer to PNG (the format `build_inline_image` wants).
pub(super) fn rgba_to_png(rgba: Vec<u8>, w: u32, h: u32) -> Option<Vec<u8>> {
    use image::ImageEncoder as _;
    use image::codecs::png::{CompressionType, FilterType, PngEncoder};
    if rgba.len() != (w as usize) * (h as usize) * 4 {
        return None;
    }
    // Best compression with adaptive filters: the minimap is a small strip
    // of flat colour runs, so this costs well under a millisecond and makes
    // each bake about a quarter the size, which is what SSH carries (#682).
    let mut out = Vec::new();
    PngEncoder::new_with_quality(&mut out, CompressionType::Best, FilterType::Adaptive)
        .write_image(&rgba, w, h, image::ExtendedColorType::Rgba8)
        .ok()?;
    Some(out)
}

impl App {
    /// Detect inline-image support via env vars only — no stdin queries, no
    /// raw-mode contention. Queries the terminal cell pixel size via
    /// crossterm's `window_size` (TIOCGWINSZ ioctl, no stdin involvement).
    /// SSH PTYs often report only rows/columns and leave pixel dimensions
    /// as zero, so fall back to a sane 10×20 cell estimate; OSC-1337 still
    /// scales the image to the requested cell rectangle.
    /// The active theme's background as a fully-opaque image pixel. Used to
    /// fill the OSC-1337 icon/hero canvases so the baked PNGs sit on the same
    /// color as the surrounding (session-bg) panes, with no visible seam.
    pub(super) fn theme_bg_pixel(&self) -> image::Rgba<u8> {
        let (r, g, b) = self.theme.editor_bg_rgb();
        image::Rgba([r, g, b, 0xff])
    }

    /// The canvas fill for images that sit on chrome (the activity-bar icons
    /// and every count badge): Croft Loch's chrome tone, the theme
    /// background everywhere else.
    pub(super) fn chrome_bg_pixel(&self) -> image::Rgba<u8> {
        let (r, g, b) = self
            .theme
            .surfaces()
            .map_or(self.theme.editor_bg_rgb(), |s| s.chrome);
        image::Rgba([r, g, b, 0xff])
    }

    /// True when the host terminal can render inline images via either
    /// protocol. Gates the bake/emit path; the iTerm2-only `SetColors` /
    /// `Color::Reset` tricks stay behind `detect_iterm2_inline_support`.
    pub(super) fn inline_images_enabled(&self) -> bool {
        self.inline_protocol != crate::iterm2_inline::InlineImageProtocol::None
    }

    /// Whether evicting stale inline images requires wiping the whole screen.
    /// iTerm2 OSC-1337 and sixel graphics live in the normal cell buffer, so
    /// only a `\x1b[2J` removes their cached cells. Kitty graphics live on
    /// their own layer and are deleted by [`Self::evict_inline_images_on_clear`]
    /// without touching a single text cell - wiping the screen there blanks
    /// the whole app for an SSH round trip on every latch (the "croft blinks
    /// while I type" bug), so the Kitty path must never `terminal.clear()`.
    pub fn image_eviction_needs_screen_wipe(&self) -> bool {
        self.inline_protocol != crate::iterm2_inline::InlineImageProtocol::Kitty
    }

    /// Kitty graphics live on a layer that survives the `\x1b[2J` the main
    /// loop fires to evict iTerm2's image cells, so on the Kitty path that
    /// same clear frame must also delete every placement; the visible
    /// overlays re-emit themselves on the following post-draw flush. No-op on
    /// the iTerm2 path, where the clear already evicted the cached cells.
    pub fn evict_inline_images_on_clear(&self) {
        use std::io::Write;
        if self.inline_protocol == crate::iterm2_inline::InlineImageProtocol::Kitty {
            let mut out = stdout();
            let _ = out.write_all(crate::iterm2_inline::delete_all_kitty_images().as_bytes());
            let _ = out.flush();
        }
    }

    /// One-shot consumer for [`Self::resize_repaint_needed`]: the main loop
    /// answers `true` with an unconditional `terminal.clear()` on every
    /// protocol, Kitty included. Resize is not image eviction: the wipe here
    /// restores text a WINCH (window resize or dtach reattach) invalidated,
    /// and it never fires while the user merely types.
    pub fn consume_resize_repaint(&mut self) -> bool {
        std::mem::take(&mut self.resize_repaint_needed)
    }

    pub fn init_graphics(&mut self) {
        // Env-advertised protocols (iTerm2/Kitty) win; otherwise fall back to a
        // sixel host detected earlier by the DA1 probe. Re-derived here so the
        // theme-switch re-bake keeps the right protocol, and `sixel_supported`
        // carries the probe result across the env re-detect (sixel has none).
        self.inline_protocol = match crate::iterm2_inline::detect_inline_image_protocol() {
            crate::iterm2_inline::InlineImageProtocol::None if self.sixel_supported => {
                crate::iterm2_inline::InlineImageProtocol::Sixel
            }
            other => other,
        };
        if !self.inline_images_enabled() {
            return;
        }
        let Ok(ws) = crossterm::terminal::window_size() else {
            return;
        };
        if ws.columns == 0 || ws.rows == 0 {
            return;
        }
        let (cell_w, cell_h) =
            cell_pixel_of(ws.columns, ws.rows, ws.width, ws.height).unwrap_or(FALLBACK_CELL_PIXEL);
        self.cell_pixel = Some((cell_w, cell_h));
        // OSC-1337 hero will own the no-repo illustration cells; the
        // widget skips its ASCII fallback so resize doesn't flash old
        // text through the image.
        self.source_control.inline_hero_image_active = true;
        // The bar background above is the theme's; so is the ink painted on
        // it. A baked icon can't route its colors through `Theme::ui` the way
        // a styled cell does, so the palette is resolved once here and rides
        // into every compose call (issue #225).
        let input = ChromeBakeInput {
            protocol: self.inline_protocol,
            cell: (cell_w, cell_h),
            is_tmux: crate::iterm2_inline::detect_tmux(),
            icon_bg: self.chrome_bg_pixel(),
            icon_ink: crate::iterm2_inline::IconInk::new(
                self.theme.activity_icon_inactive_rgb(),
                self.theme.activity_icon_active_rgb(),
                self.theme.activity_icon_pill_rgb(),
            )
            .with_hover(self.theme.activity_icon_hover_rgb()),
            sponsor_ink: crate::iterm2_inline::IconInk::new(
                self.theme.activity_sponsor_rgb(),
                self.theme.activity_sponsor_hover_rgb(),
                self.theme.activity_icon_pill_rgb(),
            ),
        };
        self.start_chrome_bake(input);
    }

    /// Bake the activity-bar icons and panel illustrations for `input` on a
    /// thread of its own. Rasterising some fifty icons and two illustrations
    /// is tens of milliseconds, four times that on a Retina canvas, and croft
    /// starts in milliseconds (#1973): `poll_chrome_bake` puts the pictures
    /// up when they land, and until then the activity bar draws its codicon
    /// glyphs. One bake runs at a time: asked again while one runs (arrowing
    /// through the theme picker's previews), the newest ask waits for it and
    /// runs in its place when it lands, so an older one's pictures never go
    /// up. Pictures on screen that `input` outdates come down at once, and
    /// the bar draws its glyphs, in the new theme, meanwhile.
    pub(super) fn start_chrome_bake(&mut self, input: ChromeBakeInput) {
        if self
            .chrome_shown
            .is_some_and(|shown| input.outdates(&shown))
        {
            self.drop_chrome_images();
        }
        if self.chrome_bake.is_some() {
            self.chrome_bake_next = Some(input);
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            // The bar draws its glyphs until the icons land.
            crate::startup_gate::wait();
            let _ = tx.send((input, Self::bake_chrome_images(&input)));
        });
        self.chrome_bake = Some(rx);
    }

    /// Take down the activity-bar icons and panel illustrations, evicting
    /// whatever of them was sent, so the bar falls back to its glyphs.
    pub(super) fn drop_chrome_images(&mut self) {
        self.overlays.activity.clear_images();
        self.overlays.activity.request_clear();
        self.overlays.activity.mark_dirty();
        self.clear_run_debug_icon_if_emitted();
        self.overlays.run_debug.clear_image();
        self.overlays.ssh.disable();
        self.chrome_shown = None;
    }

    /// Put up the activity-bar icons and the panel illustrations once the
    /// bake [`Self::init_graphics`] started lands, or start the one asked for
    /// since. `true` when they went up.
    pub(super) fn poll_chrome_bake(&mut self) -> bool {
        let Some(rx) = self.chrome_bake.as_ref() else {
            return false;
        };
        let baked = match rx.try_recv() {
            Ok(baked) => Some(baked),
            Err(std::sync::mpsc::TryRecvError::Empty) => return false,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => None,
        };
        self.chrome_bake = None;
        if let Some(next) = self.chrome_bake_next.take() {
            self.start_chrome_bake(next);
            return false;
        }
        let Some((input, baked)) = baked else {
            return false;
        };
        self.apply_chrome_images(baked);
        self.chrome_shown = Some(input);
        true
    }

    /// Install a finished [`Self::bake_chrome_images`]: the activity-bar icons,
    /// the Run and Debug headline icon and the SSH empty state.
    pub(super) fn apply_chrome_images(&mut self, baked: ChromeImages) {
        if let Some(images) = baked.activity {
            // New icons over old ones (a theme switch, a font zoom): the old
            // layer is evicted first, as the switch itself does.
            if self.overlays.activity.has_images() {
                self.overlays.activity.request_clear();
            }
            self.overlays.activity.set_images(images);
            self.overlays.activity.mark_dirty();
        }
        if let Some(icon) = baked.run_debug {
            self.overlays.run_debug.set_image(icon);
        }
        if let Some(card) = baked.ssh {
            self.overlays.ssh.set_image(card);
        }
        // The activity-bar count badges ride the same overlay machinery; rebuild
        // them against the freshly-baked icons / new background or cell size. The
        // Explorer badge's per-frame refresh is count-gated, so force a rebuild
        // here by invalidating its cached count first.
        self.refresh_scm_change_badge();
        self.explorer_unsaved_count = usize::MAX;
        self.refresh_explorer_unsaved_badge();
        self.remote_ports_count = usize::MAX;
        self.refresh_remote_ports_badge();
        self.testing_badge_count = usize::MAX;
        self.refresh_testing_badge();
        self.problems_badge_count = usize::MAX;
        self.refresh_problems_badge();
    }

    /// Bake the activity-bar icons (every state of every icon), the Run and
    /// Debug headline icon and the SSH empty state for `input`'s terminal.
    pub(super) fn bake_chrome_images(input: &ChromeBakeInput) -> ChromeImages {
        let ChromeBakeInput {
            protocol,
            cell: (cell_w, cell_h),
            is_tmux,
            icon_bg,
            icon_ink,
            sponsor_ink,
        } = *input;
        let canvas_w = cell_w * ACTIVITY_BAR_WIDTH as u32;
        let canvas_h = cell_h * ACTIVITY_ICON_HEIGHT as u32;
        let w_cells = ACTIVITY_BAR_WIDTH;
        let h_cells = ACTIVITY_ICON_HEIGHT;
        // The activity-bar icons are codicon SVGs, rasterised at the exact icon
        // pixel size (no multi-MB raster decode, crisp at any cell size). `id`
        // is the icon's stable per-slot Kitty image/placement id (shared by its
        // active and inactive variants so swapping one for the other replaces
        // the placement in place); ignored on the iTerm2 path.
        let encode = |src_svg: &[u8],
                      state: crate::iterm2_inline::IconState,
                      ink: crate::iterm2_inline::IconInk,
                      off_y_bias: i64,
                      id: u32|
         -> Option<String> {
            let baked = crate::iterm2_inline::compose_icon_svg(
                src_svg, canvas_w, canvas_h, state, ink, icon_bg, off_y_bias,
            )?;
            // preserveAspectRatio=0: stretch to exactly fill 4×2 cells.
            // Since the canvas was composed at exactly that pixel size,
            // there's no actual scaling and the codicon's square area
            // remains a true square on screen.
            let raw = crate::iterm2_inline::build_inline_image(
                protocol, &baked, w_cells, h_cells, false, id,
            )?;
            Some(crate::iterm2_inline::maybe_tmux_wrap(
                protocol, is_tmux, raw,
            ))
        };
        // View icons render dead-centered in their cell block; the gear gets a
        // downward bias so it bottom-aligns within its (bottom-inset) block,
        // shaving the canvas's bottom padding off the gap above the status bar.
        // The value need only exceed the no-clip ceiling; `compose_icon_svg`
        // clamps it, so this stays correct at any cell size.
        let gear_off_y_bias = canvas_h as i64;
        use crate::iterm2_inline as ki;
        // Bake all three states per icon up front (selected / hovered / resting)
        // so the render path only ever swaps which pre-encoded sequence it emits
        // — a hover never triggers a re-bake, just a different cached string.
        let bake = |src: &[u8], off_y_bias: i64, id: u32| -> Option<(String, String, String)> {
            Some((
                encode(src, ki::IconState::Active, icon_ink, off_y_bias, id)?,
                encode(src, ki::IconState::Inactive, icon_ink, off_y_bias, id)?,
                encode(src, ki::IconState::Hovered, icon_ink, off_y_bias, id)?,
            ))
        };
        // The sponsor heart (#1940) wears its own pink, and is never selected,
        // so it bakes only its resting and hovered states. It shares the gear's
        // bias so the two keep the view icons' spacing between them.
        let sponsor_imgs = (|| {
            let heart = |state| {
                encode(
                    ki::SPONSOR_HEART_SRC_SVG,
                    state,
                    sponsor_ink,
                    gear_off_y_bias,
                    ki::KITTY_ID_SPONSOR,
                )
            };
            Some((
                heart(ki::IconState::Inactive)?,
                heart(ki::IconState::Hovered)?,
            ))
        })();
        // Customize Layout toolbar icons: baked OPAQUE on the editor bg at the
        // 4×2-cell box so they paint IN FRONT of the container's top line
        // (the line sits behind the icon's solid background). `Inactive` is the
        // muted resting grey; `Hovered` is the bright variant emitted while the
        // pointer rests on the icon. The side-bar / panel ids swap their on/off
        // image in place.
        let layout_canvas_w = cell_w * LAYOUT_ICON_CELLS_W as u32;
        let layout_canvas_h = cell_h * LAYOUT_ICON_CELLS_H as u32;
        let bake_layout = |src_svg: &[u8], state: ki::IconState, id: u32| -> Option<String> {
            let baked = crate::iterm2_inline::compose_icon_svg(
                src_svg,
                layout_canvas_w,
                layout_canvas_h,
                state,
                icon_ink,
                icon_bg,
                0,
            )?;
            let raw = crate::iterm2_inline::build_inline_image(
                protocol,
                &baked,
                LAYOUT_ICON_CELLS_W,
                LAYOUT_ICON_CELLS_H,
                false,
                id,
            )?;
            Some(crate::iterm2_inline::maybe_tmux_wrap(
                protocol, is_tmux, raw,
            ))
        };
        let bake_layout_pair = |src: &[u8], id: u32| -> Option<(String, String)> {
            Some((
                bake_layout(src, ki::IconState::Inactive, id)?,
                bake_layout(src, ki::IconState::Hovered, id)?,
            ))
        };
        let layout_imgs = (|| {
            let (so, soh) =
                bake_layout_pair(ki::LAYOUT_SIDEBAR_ON_SRC_SVG, ki::KITTY_ID_LAYOUT_SIDEBAR)?;
            let (sf, sfh) =
                bake_layout_pair(ki::LAYOUT_SIDEBAR_OFF_SRC_SVG, ki::KITTY_ID_LAYOUT_SIDEBAR)?;
            let (po, poh) =
                bake_layout_pair(ki::LAYOUT_PANEL_ON_SRC_SVG, ki::KITTY_ID_LAYOUT_PANEL)?;
            let (pf, pfh) =
                bake_layout_pair(ki::LAYOUT_PANEL_OFF_SRC_SVG, ki::KITTY_ID_LAYOUT_PANEL)?;
            let (cu, cuh) =
                bake_layout_pair(ki::LAYOUT_CUSTOMIZE_SRC_SVG, ki::KITTY_ID_LAYOUT_CUSTOMIZE)?;
            Some((so, soh, sf, sfh, po, poh, pf, pfh, cu, cuh))
        })();
        let activity = if let (
            Some((ea, ei, eh)),
            Some((sa, si, sh)),
            Some((sca, sci, sch)),
            Some((ra, ri, rh)),
            Some((rda, rdi, rdh)),
            Some((exta, exti, exth)),
            Some((tsta, tsti, tsth)),
            Some((cqa, cqi, cqh)),
            Some((spi, sph)),
            Some((gea, gei, geh)),
            Some((lso, lsoh, lsf, lsfh, lpo, lpoh, lpf, lpfh, lcu, lcuh)),
        ) = (
            bake(ki::EXPLORER_SRC_SVG, 0, ki::KITTY_ID_EXPLORER),
            bake(ki::SEARCH_SRC_SVG, 0, ki::KITTY_ID_SEARCH),
            bake(ki::SOURCE_CONTROL_SRC_SVG, 0, ki::KITTY_ID_SOURCE_CONTROL),
            bake(ki::REMOTE_SRC_SVG, 0, ki::KITTY_ID_REMOTE),
            bake(ki::RUN_DEBUG_SRC_SVG, 0, ki::KITTY_ID_RUN_DEBUG_BAR),
            bake(ki::EXTENSIONS_SRC_SVG, 0, ki::KITTY_ID_EXTENSIONS),
            bake(ki::TESTING_SRC_SVG, 0, ki::KITTY_ID_TESTING),
            bake(ki::CODEQL_SRC_SVG, 0, ki::KITTY_ID_CODEQL),
            sponsor_imgs,
            bake(
                ki::SETTINGS_GEAR_SRC_SVG,
                gear_off_y_bias,
                ki::KITTY_ID_SETTINGS,
            ),
            layout_imgs,
        ) {
            Some(ActivityBarImages {
                explorer_active: ea,
                explorer_inactive: ei,
                explorer_hovered: eh,
                search_active: sa,
                search_inactive: si,
                search_hovered: sh,
                source_control_active: sca,
                source_control_inactive: sci,
                source_control_hovered: sch,
                remote_active: ra,
                remote_inactive: ri,
                remote_hovered: rh,
                run_debug_active: rda,
                run_debug_inactive: rdi,
                run_debug_hovered: rdh,
                extensions_active: exta,
                extensions_inactive: exti,
                extensions_hovered: exth,
                testing_active: tsta,
                testing_inactive: tsti,
                testing_hovered: tsth,
                codeql_active: cqa,
                codeql_inactive: cqi,
                codeql_hovered: cqh,
                sponsor_inactive: spi,
                sponsor_hovered: sph,
                settings_active: gea,
                settings_inactive: gei,
                settings_hovered: geh,
                layout_sidebar_on: lso,
                layout_sidebar_on_hover: lsoh,
                layout_sidebar_off: lsf,
                layout_sidebar_off_hover: lsfh,
                layout_panel_on: lpo,
                layout_panel_on_hover: lpoh,
                layout_panel_off: lpf,
                layout_panel_off_hover: lpfh,
                layout_customize: lcu,
                layout_customize_hover: lcuh,
            })
        } else {
            None
        };
        // Run-and-Debug headline icon: the same debug-alt PNG used in the
        // activity bar, fitted to the panel's icon block (RUN_DEBUG_ICON_CELLS_W
        // cells wide × RUN_DEBUG_ICON_CELLS_H cells tall) and tinted at icon-
        // inactive grey so it reads as the same monochrome family as the rest
        // of the activity-bar icons. Encoded once at init; emitted post-frame
        // by `flush_run_debug_icon_overlay` while the panel is visible.
        let rd_icon_w_cells = crate::widgets::run_debug::RUN_DEBUG_ICON_CELLS_W as u32;
        let rd_icon_h_cells = crate::widgets::run_debug::RUN_DEBUG_ICON_CELLS_H as u32;
        let rd_icon_w_px = cell_w * rd_icon_w_cells;
        let rd_icon_h_px = cell_h * rd_icon_h_cells;
        let run_debug = if let Ok(baked) = crate::iterm2_inline::compose_icon(
            crate::iterm2_inline::RUN_DEBUG_SRC_PNG,
            rd_icon_w_px,
            rd_icon_h_px,
            false,
            icon_ink,
            icon_bg,
            0,
        ) && let Some(raw) = crate::iterm2_inline::build_inline_image(
            protocol,
            &baked,
            crate::widgets::run_debug::RUN_DEBUG_ICON_CELLS_W,
            crate::widgets::run_debug::RUN_DEBUG_ICON_CELLS_H,
            false,
            crate::iterm2_inline::KITTY_ID_RUN_DEBUG_PANEL,
        ) {
            Some(crate::iterm2_inline::maybe_tmux_wrap(
                protocol, is_tmux, raw,
            ))
        } else {
            None
        };
        // SSH empty-state illustration baked to a fixed cell box (18 × 8
        // cells). Letterboxed onto the card's dark bg so iTerm2 has no
        // transparent edge to ghost the underlying buffer. Emitted post-
        // draw by `flush_ssh_empty_state_overlay` while the SSH section
        // shows no hosts.
        let ssh_cells_w: u16 = crate::widgets::remote::SSH_EMPTY_STATE_CELLS_W;
        let ssh_cells_h: u16 = crate::widgets::remote::SSH_EMPTY_STATE_CELLS_H;
        let ssh_w_px = cell_w * ssh_cells_w as u32;
        let ssh_h_px = cell_h * ssh_cells_h as u32;
        // Transparent letterbox so the host terminal's session bg shows
        // through the empty padding around the illustration. Any opaque
        // fill here would render as a visible darker rectangle behind the
        // PNG, which is exactly what the user told us to remove.
        let card_bg = ::image::Rgba([0x00, 0x00, 0x00, 0x00]);
        let ssh = if let Ok(baked) = crate::iterm2_inline::fit_image_auto(
            crate::iterm2_inline::SSH_EMPTY_STATE_PNG,
            ssh_w_px,
            ssh_h_px,
            card_bg,
        ) && let Some(raw) = crate::iterm2_inline::build_inline_image_z(
            protocol,
            &baked,
            ssh_cells_w,
            ssh_cells_h,
            false,
            crate::iterm2_inline::KITTY_ID_SSH,
            crate::iterm2_inline::KITTY_Z_BELOW_TEXT_AND_BG,
        ) {
            Some(crate::iterm2_inline::maybe_tmux_wrap(
                protocol, is_tmux, raw,
            ))
        } else {
            None
        };
        ChromeImages {
            activity,
            run_debug,
            ssh,
        }
    }

    /// Emit the OSC-1337 image carrying the run-debug headline icon at the
    /// cell the panel reserved on its most recent render. No-op when the
    /// panel isn't the active sidebar view, when the icon hasn't been
    /// baked (non-iTerm2 host), or when the panel didn't reserve a cell
    /// (too short to fit the icon). Critically, the panel-hidden case
    /// does NOT stamp blanks or any other "wipe" payload here: doing so
    /// after `terminal.clear()` + redraw stomps freshly-painted text in
    /// the next sidebar (this was the bug in the prior space-stamp wipe
    /// attempt). Eviction of iTerm2's cached image cells is handled
    /// exclusively by the `consume_run_debug_image_clear` →
    /// `terminal.clear()` gate in the main loop, the same pipeline the
    /// welcome wordmark and editor preview image use.
    /// Stop showing the Run/Debug empty-state icon: if an OSC-1337 image was
    /// emitted, arm the one-shot `terminal.clear()` latch so the cached image
    /// cells are evicted (text painted over an inline image does not remove
    /// it), then reset the emitted-position tracking so the clear fires exactly
    /// once rather than every frame.
    pub(super) fn clear_run_debug_icon_if_emitted(&mut self) {
        if self.overlays.run_debug.was_emitted() {
            self.overlays.run_debug.request_clear();
        }
        self.overlays.run_debug.clear_emitted();
    }

    pub fn flush_run_debug_icon_overlay(&mut self) {
        use std::io::Write;
        if self.modal_overlay_open() {
            return;
        }
        let panel_visible = self.show_tree
            && self.sidebar_view == SidebarView::RunDebug
            && self.overlays.run_debug.has_image();
        if !panel_visible {
            self.clear_run_debug_icon_if_emitted();
            return;
        }
        // No icon cell this frame means the panel is showing the paused
        // call-stack tree (debug_active) or is too short for the icon. The
        // empty-state icon is an OSC-1337 image; drawing the tree over it does
        // NOT evict it, so an emitted icon must be cleared with the one-shot
        // `\x1b[2J` latch or it ghosts on top of the call stack.
        let Some((cx, cy)) = self.run_debug.last_icon_cell else {
            self.clear_run_debug_icon_if_emitted();
            return;
        };
        // The icon moved since the last emit (terminal resize OR an internal
        // pane-drag, which never fires Event::Resize and so escapes
        // `on_resize`). A plain re-emit at the new cell stacks a fresh
        // OSC-1337 image on top of the one iTerm2 still caches at the old
        // cell — the doubled/garbled headline-icon ghost. Plain SGR
        // overwrites don't evict those cells, so reuse the proven
        // `terminal.clear()` latch: arm one clear, drop the stale emit
        // position, and skip painting this frame. Next frame the main loop's
        // OR chain fires `\x1b[2J`, full-redraws, and this flush re-emits a
        // single clean icon at the new cell.
        if let Some(prev) = self.overlays.run_debug.last_emitted()
            && prev != (cx, cy)
        {
            self.overlays.run_debug.request_clear();
            self.overlays.run_debug.clear_emitted();
            return;
        }
        let Some(osc) = self.overlays.run_debug.image().map(str::to_owned) else {
            return;
        };
        let at = Rect {
            x: cx,
            y: cy,
            width: crate::widgets::run_debug::RUN_DEBUG_ICON_CELLS_W,
            height: crate::widgets::run_debug::RUN_DEBUG_ICON_CELLS_H,
        };
        if !self.chrome_image_due("run_debug", &osc, at) {
            return;
        }
        let mut out = stdout();
        let cursor_on = self.cursor_should_be_visible();
        let _ = write!(out, "\x1b[?25l\x1b[s");
        let _ = write!(out, "\x1b[{};{}H", cy + 1, cx + 1);
        let _ = out.write_all(osc.as_bytes());
        let _ = write!(out, "\x1b[u");
        if cursor_on {
            let _ = write!(out, "\x1b[?25h");
        }
        let _ = out.flush();
        self.overlays.run_debug.mark_emitted_at((cx, cy));
    }

    /// Arm the one-shot `terminal.clear()` latch if the PROBLEMS circle was
    /// emitted, so its cached image cells are evicted when the badge goes away
    /// (text painted over an inline image does not remove it), then reset the
    /// emit position so the clear fires exactly once.
    pub(super) fn clear_problems_badge_if_emitted(&mut self) {
        if self.overlays.problems_badge.was_emitted() {
            self.overlays.problems_badge.request_clear();
        }
        self.overlays.problems_badge.clear_emitted();
    }

    /// Emit the PROBLEMS-tab orange circle as an OSC-1337/Kitty inline image at
    /// the cell `paint_panel_tabs` recorded this frame, re-emitted every frame
    /// (like the activity-bar badges) so heavy SGR traffic can't evict it.
    /// Clears when the panel strip is hidden, the count is zero, or a modal owns
    /// the screen; arms a one-shot clear when the anchor moves so no ghost stacks.
    pub fn flush_problems_badge_overlay(&mut self) {
        use std::io::Write;
        if self.modal_overlay_open() {
            return;
        }
        let Some((cx, cy)) = self.problems_badge_cell.filter(|_| self.show_terminal) else {
            self.clear_problems_badge_if_emitted();
            return;
        };
        if !self.overlays.problems_badge.has_image() {
            self.clear_problems_badge_if_emitted();
            return;
        }
        if let Some(prev) = self.overlays.problems_badge.last_emitted()
            && prev != (cx, cy)
        {
            self.overlays.problems_badge.request_clear();
            self.overlays.problems_badge.clear_emitted();
            return;
        }
        let Some(osc) = self.overlays.problems_badge.image().map(str::to_owned) else {
            return;
        };
        let at = Rect {
            x: cx,
            y: cy,
            width: crate::iterm2_inline::count_badge_cells_w(self.problems_badge_count),
            height: 1,
        };
        if !self.chrome_image_due("problems_badge", &osc, at) {
            return;
        }
        let mut out = stdout();
        let cursor_on = self.cursor_should_be_visible();
        let _ = write!(out, "\x1b[?25l\x1b[s");
        let _ = write!(out, "\x1b[{};{}H", cy + 1, cx + 1);
        let _ = out.write_all(osc.as_bytes());
        let _ = write!(out, "\x1b[u");
        if cursor_on {
            let _ = write!(out, "\x1b[?25h");
        }
        let _ = out.flush();
        self.overlays.problems_badge.mark_emitted_at((cx, cy));
    }

    /// Re-bake (when needed) and re-emit the OSC-1337 inline image for
    /// the no-repo source-control hero. Pure no-op when the sidebar isn't
    /// on Source Control, the workspace IS a git repo, or the host
    /// terminal can't render inline images. Called from the post-draw
    /// flush each frame so iTerm2 keeps the image painted even after
    /// surrounding cells redraw.
    pub fn flush_no_repo_hero_overlay(&mut self) {
        use std::io::Write;
        if self.modal_overlay_open() {
            return;
        }
        let panel_visible = self.show_tree
            && self.sidebar_view == SidebarView::SourceControl
            && !self.source_control.status.in_repo;
        if !panel_visible {
            self.overlays.hero.mark_hidden();
            return;
        }
        let hero = self.source_control.last_hero_area;
        if hero.width == 0 || hero.height == 0 || self.update_card_covers(hero) {
            return;
        }
        // The hero asset carries its own opaque dark backdrop; on a light
        // theme it would sit as a dark slab on the white panel, so the
        // panel keeps its text-only empty state instead.
        if self.theme.is_light() {
            self.overlays.hero.mark_hidden();
            return;
        }
        let Some((cw, ch)) = self.cell_pixel else {
            return;
        };
        let desired = WelcomeLayout {
            cell_x: hero.x,
            cell_y: hero.y,
            cell_w: hero.width,
            cell_h: hero.height,
        };
        if !self.overlays.hero.layout_matches(&desired) || !self.overlays.hero.has_image() {
            let canvas_w = (hero.width as u32) * cw;
            let canvas_h = (hero.height as u32) * ch;
            let bg = self.theme_bg_pixel();
            let protocol = self.inline_protocol;
            if let Ok(baked) = crate::iterm2_inline::fit_image(
                crate::iterm2_inline::NO_REPO_HERO_PNG,
                canvas_w,
                canvas_h,
                bg,
            ) && let Some(raw) = crate::iterm2_inline::build_inline_image_z(
                protocol,
                &baked,
                hero.width,
                hero.height,
                false,
                crate::iterm2_inline::KITTY_ID_HERO,
                crate::iterm2_inline::KITTY_Z_BELOW_TEXT_AND_BG,
            ) {
                let osc = crate::iterm2_inline::maybe_tmux_wrap(
                    protocol,
                    crate::iterm2_inline::detect_tmux(),
                    raw,
                );
                self.overlays.hero.set(osc, desired);
            }
        }
        let Some(osc) = self.overlays.hero.image().map(str::to_owned) else {
            return;
        };
        if !self.chrome_image_due("hero", &osc, hero) {
            return;
        }
        let mut out = stdout();
        let cursor_on = self.cursor_should_be_visible();
        let _ = write!(out, "\x1b[?25l\x1b[s");
        let _ = write!(out, "\x1b[{};{}H", hero.y + 1, hero.x + 1);
        let _ = out.write_all(osc.as_bytes());
        let _ = write!(out, "\x1b[u");
        if cursor_on {
            let _ = write!(out, "\x1b[?25h");
        }
        let _ = out.flush();
        self.overlays.hero.mark_emitted();
    }

    /// Emit the OSC-1337 SSH-empty-state illustration at the cell the
    /// remote panel reserved on its most recent render. When the panel
    /// is no longer the active view (or has hosts, or the shortcuts
    /// modal is open), this arms a one-shot terminal.clear() via
    /// `consume_ssh_empty_state_image_clear()` so iTerm2 evicts the
    /// cached image cells — otherwise the illustration ghosts onto
    /// whatever view (Explorer / Search / …) replaces it.
    pub fn flush_ssh_empty_state_overlay(&mut self) {
        use std::io::Write;
        let should_show = self.shortcuts_modal.is_none()
            && self.file_finder.is_none()
            && self.zoxide_jump.is_none()
            && self.command_history_popup.is_none()
            && self.branch_picker.is_none()
            && self.input_prompt.is_none()
            && self.list_picker.is_none()
            && self.show_tree
            && self.sidebar_view == SidebarView::Remote
            && self.remote.targets.is_empty();
        if !should_show {
            self.overlays.ssh.hide_and_request_clear();
            return;
        }
        let Some((cx, cy)) = self.remote.last_image_cell else {
            return;
        };
        let Some(osc) = self.overlays.ssh.image().map(str::to_owned) else {
            return;
        };
        let at = Rect {
            x: cx,
            y: cy,
            width: crate::widgets::remote::SSH_EMPTY_STATE_CELLS_W,
            height: crate::widgets::remote::SSH_EMPTY_STATE_CELLS_H,
        };
        if self.update_card_covers(at) || !self.chrome_image_due("ssh", &osc, at) {
            return;
        }
        let mut out = stdout();
        let cursor_on = self.cursor_should_be_visible();
        let _ = write!(out, "\x1b[?25l\x1b[s");
        let _ = write!(out, "\x1b[{};{}H", cy + 1, cx + 1);
        let _ = out.write_all(osc.as_bytes());
        let _ = write!(out, "\x1b[u");
        if cursor_on {
            let _ = write!(out, "\x1b[?25h");
        }
        let _ = out.flush();
        self.overlays.ssh.mark_displayed();
    }

    /// One-shot consumer paired with `flush_ssh_empty_state_overlay`. The
    /// main loop ORs the result into its `terminal.clear()` chain so
    /// iTerm2's cached image cells are evicted on view change.
    pub fn consume_ssh_empty_state_image_clear(&mut self) -> bool {
        self.overlays.ssh.consume_clear_latch()
    }

    /// One-shot consumer for the "leaving Run-Debug after the icon was
    /// emitted" flag. The main loop folds the result into the same OR
    /// chain that fires `terminal.clear()` for the welcome wordmark and
    /// editor preview image.
    pub fn consume_run_debug_image_clear(&mut self) -> bool {
        self.overlays.run_debug.consume_clear()
    }

    /// One-shot consumer for the "the PROBLEMS circle went away / moved after it
    /// was emitted" flag. Folded into the main loop's `terminal.clear()` OR chain
    /// so iTerm2 evicts the cached badge bitmap.
    pub fn consume_problems_badge_image_clear(&mut self) -> bool {
        self.overlays.problems_badge.consume_clear()
    }

    /// One-shot consumer for the "a resize may have left stale activity-bar
    /// icon bitmaps in iTerm2's image layer" flag, armed by `on_resize`. The
    /// main loop folds the result into the same OR chain that fires
    /// `terminal.clear()` for the welcome wordmark and editor preview image,
    /// evicting the ghost before the icons re-emit on the next draw.
    pub fn consume_activity_image_clear(&mut self) -> bool {
        self.overlays.activity.consume_clear()
    }

    /// Handle a terminal/pane resize. The alt-screen reflow blanks the SGR
    /// cells but iTerm2's OSC-1337 image layer survives, so a plain re-emit
    /// would stack a fresh icon on top of the stale one (the doubled-icon
    /// ghost). Re-mark the icons dirty so they re-emit, and — in images mode,
    /// the only state the stale layer can exist in — arm a one-shot
    /// `terminal.clear()` so iTerm2 drops the cached bitmaps first.
    pub fn on_resize(&mut self) {
        self.overlays.activity.mark_dirty();
        if self.overlays.activity.has_images() {
            self.overlays.activity.request_clear();
        }
        // A WINCH invalidates the physical screen (window resize reflow, or a
        // dtach reattach against a blank terminal) while ratatui's back buffer
        // still claims every cell is painted; force the next frame to wipe and
        // re-emit everything on every protocol.
        self.resize_repaint_needed = true;
        // A font zoom resizes the cells, not only the grid. Every inline
        // image was baked for the old cell size, and a sixel image is sized
        // in pixels, so it would outgrow its cells and paint over its
        // neighbours: the welcome logo over the version badge (#1843).
        if let Some(cell) = self.cell_pixel
            && let Ok(ws) = crossterm::terminal::window_size()
            && cell_pixel_of(ws.columns, ws.rows, ws.width, ws.height)
                .is_some_and(|now| now != cell)
        {
            self.rebake_inline_images();
        }
    }

    /// One-shot consumer for the "leaving Source Control after the no-repo
    /// hero PNG was emitted" flag. Same shape as
    /// `consume_run_debug_image_clear` - the main loop folds the result
    /// into its `terminal.clear()` chain so iTerm2's cached image cells
    /// are evicted on view change.
    pub fn consume_no_repo_hero_image_clear(&mut self) -> bool {
        self.overlays.hero.consume_clear_latch()
    }

    /// The rect of the sidebar OSC-1337 illustration currently on screen (the
    /// Source Control no-repo hero or the Remote SSH empty-state), or `None`
    /// when neither is showing. Those images composite above ratatui's text
    /// layer, so a tooltip overlapping one must be re-painted last.
    pub(super) fn active_sidebar_image_rect(&self) -> Option<Rect> {
        if !self.show_tree {
            return None;
        }
        let rect = match self.sidebar_view {
            SidebarView::SourceControl if !self.source_control.status.in_repo => {
                self.source_control.last_hero_area
            }
            SidebarView::Remote if self.remote.targets.is_empty() => self.remote.last_image_area,
            _ => return None,
        };
        (rect.width > 0 && rect.height > 0).then_some(rect)
    }

    /// Re-paint the chrome tooltip directly to the terminal AFTER the inline
    /// images have been flushed, so its box sits in the foreground over any
    /// sidebar illustration it overlaps. The hero / SSH image is emitted
    /// post-draw and would otherwise composite over the tooltip's text; since
    /// the last writer wins each cell, re-emitting the box here puts it on top
    /// while the illustration itself stays fully painted everywhere else.
    /// No-op unless a tooltip is showing and actually lands on an illustration.
    pub fn flush_tooltip_over_image(&mut self) {
        use std::io::Write;
        // Only the cell-buffer protocols (iTerm2 OSC-1337, Sixel) need this: a
        // later text write wins those cells. The Kitty path instead places the
        // illustration at a deep-negative z so text already draws on top, and
        // re-blitting would be wasted stdout traffic.
        match self.inline_protocol {
            crate::iterm2_inline::InlineImageProtocol::ITerm2
            | crate::iterm2_inline::InlineImageProtocol::Sixel => {}
            _ => return,
        }
        let Some(area) = self.ui_tooltip_area else {
            return;
        };
        let Some(img) = self.active_sidebar_image_rect() else {
            return;
        };
        if !rects_intersect(area, img) {
            return;
        }
        let Some(popup) = self.ui_tooltip.as_ref() else {
            return;
        };
        // Render the popup into a standalone buffer at its on-screen rect, then
        // blit each cell as an absolutely-positioned SGR write. The box is fully
        // opaque (its block fills the bg), so it cleanly occludes the slice of
        // the illustration it covers, exactly like a floating tooltip.
        let mut buf = ratatui::buffer::Buffer::empty(area);
        ratatui::widgets::Widget::render(popup, area, &mut buf);
        let mut out = stdout();
        let cursor_on = self.cursor_should_be_visible();
        let _ = write!(out, "\x1b[?25l\x1b[s");
        for y in area.top()..area.bottom() {
            // A `surfaces` theme's one-line pill leaves the rows around it
            // unpainted: re-emitting them would blank the illustration.
            if (area.left()..area.right()).all(|x| buf[(x, y)] == ratatui::buffer::Cell::EMPTY) {
                continue;
            }
            let _ = write!(out, "\x1b[{};{}H", y + 1, area.left() + 1);
            for x in area.left()..area.right() {
                let cell = &buf[(x, y)];
                let _ = out.write_all(sgr_for_cell(cell).as_bytes());
                let _ = out.write_all(cell.symbol().as_bytes());
            }
            let _ = write!(out, "\x1b[0m");
        }
        let _ = write!(out, "\x1b[u");
        if cursor_on {
            let _ = write!(out, "\x1b[?25h");
        }
        let _ = out.flush();
    }

    /// Returns the post-frame OSC-1337 escapes to write under the activity
    /// bar, paired with the absolute terminal cell where each one starts
    /// (just past the active-pill column). Empty when image rendering is
    /// disabled or the activity bar hasn't been laid out yet.
    pub fn pending_activity_image_overlays(&self) -> Vec<((u16, u16), &str)> {
        let Some(images) = self.overlays.activity.images() else {
            return Vec::new();
        };
        let exp_block = self.sidebar_areas.activity_icons[ActivityIcon::Explorer];
        let sea_block = self.sidebar_areas.activity_icons[ActivityIcon::Search];
        let scm_block = self.sidebar_areas.activity_icons[ActivityIcon::SourceControl];
        let rem_block = self.sidebar_areas.activity_icons[ActivityIcon::Remote];
        let rdb_block = self.sidebar_areas.activity_icons[ActivityIcon::RunDebug];
        let ext_block = self.sidebar_areas.activity_icons[ActivityIcon::Extensions];
        let tst_block = self.sidebar_areas.activity_icons[ActivityIcon::Testing];
        // Optional: absent while CodeQL is disabled, so not part of the
        // all-or-nothing readiness below.
        let cql_block = self.sidebar_areas.activity_icons[ActivityIcon::CodeQL];
        // The activity bar is all-or-nothing: emit its icons only when every
        // block is laid out (a partial bar would look broken). When it's hidden
        // (Customize Layout / Zen) the blocks are zero-width and skipped, but
        // the layout toolbar icons below still emit independently.
        let activity_ready = exp_block.width > 0
            && sea_block.width > 0
            && scm_block.width > 0
            && rem_block.width > 0
            && rdb_block.width > 0
            && ext_block.width > 0
            && tst_block.width > 0;
        // Three-way per icon: the selected view shows the active (pilled)
        // variant; otherwise the pointer-hovered icon brightens (no pill); the
        // rest stay muted. Selected always wins over hovered.
        let hov = self.hovered_activity_icon;
        let exp_state = if self.activity_view_active(SidebarView::Explorer) {
            &images.explorer_active
        } else if hov == Some(ActivityIcon::Explorer) {
            &images.explorer_hovered
        } else {
            &images.explorer_inactive
        };
        let sea_state = if self.activity_view_active(SidebarView::Search) {
            &images.search_active
        } else if hov == Some(ActivityIcon::Search) {
            &images.search_hovered
        } else {
            &images.search_inactive
        };
        let scm_state = if self.activity_view_active(SidebarView::SourceControl) {
            &images.source_control_active
        } else if hov == Some(ActivityIcon::SourceControl) {
            &images.source_control_hovered
        } else {
            &images.source_control_inactive
        };
        let rem_state = if self.activity_view_active(SidebarView::Remote) {
            &images.remote_active
        } else if hov == Some(ActivityIcon::Remote) {
            &images.remote_hovered
        } else {
            &images.remote_inactive
        };
        let rdb_state = if self.activity_view_active(SidebarView::RunDebug) {
            &images.run_debug_active
        } else if hov == Some(ActivityIcon::RunDebug) {
            &images.run_debug_hovered
        } else {
            &images.run_debug_inactive
        };
        let ext_state = if self.activity_view_active(SidebarView::Extensions) {
            &images.extensions_active
        } else if hov == Some(ActivityIcon::Extensions) {
            &images.extensions_hovered
        } else {
            &images.extensions_inactive
        };
        let tst_state = if self.activity_view_active(SidebarView::Testing) {
            &images.testing_active
        } else if hov == Some(ActivityIcon::Testing) {
            &images.testing_hovered
        } else {
            &images.testing_inactive
        };
        let cql_state = if self.activity_view_active(SidebarView::CodeQL) {
            &images.codeql_active
        } else if hov == Some(ActivityIcon::CodeQL) {
            &images.codeql_hovered
        } else {
            &images.codeql_inactive
        };
        // The heart and the gear are bottom-anchored and may be absent on a
        // short bar (empty block). The heart is never selected; the gear
        // reads "active" while its menu/picker is open.
        let spo_block = self.sidebar_areas.activity_icons[ActivityIcon::Sponsor];
        let spo_state = if hov == Some(ActivityIcon::Sponsor) {
            &images.sponsor_hovered
        } else {
            &images.sponsor_inactive
        };
        let set_block = self.sidebar_areas.activity_icons[ActivityIcon::Settings];
        let set_state = if self.settings_menu_active() {
            &images.settings_active
        } else if hov == Some(ActivityIcon::Settings) {
            &images.settings_hovered
        } else {
            &images.settings_inactive
        };
        // The shortcuts modal and file finder are centered over the editor
        // column, not the far-left activity bar, so the icons stay visible
        // beside them. Suppress an icon only when its block actually
        // intersects an open panel (possible only when the terminal is
        // narrow enough that the centered panel reaches column 0); otherwise
        // a terminal.clear() armed by the modal wipes iTerm2's cached image
        // cells and they never repaint while the panel is up.
        let occluders = [
            self.shortcuts_modal.as_ref().map(|m| m.last_rect),
            self.file_finder.as_ref().map(|f| f.last_rect),
            self.command_palette.as_ref().map(|p| p.last_rect),
            self.go_to_symbol.as_ref().map(|p| p.last_rect),
            self.workspace_symbols.as_ref().map(|p| p.last_rect),
            self.process_picker.as_ref().map(|p| p.last_rect),
        ];
        let visible = |block: Rect| {
            !self.update_card_covers(block)
                && occluders
                    .iter()
                    .flatten()
                    .all(|panel| !block.intersects(*panel))
        };
        let mut blocks: Vec<(Rect, &str)> = Vec::new();
        if activity_ready {
            blocks.extend_from_slice(&[
                (exp_block, exp_state.as_str()),
                (sea_block, sea_state.as_str()),
                (scm_block, scm_state.as_str()),
                (rem_block, rem_state.as_str()),
                (rdb_block, rdb_state.as_str()),
                (ext_block, ext_state.as_str()),
                (tst_block, tst_state.as_str()),
            ]);
            if cql_block.width > 0 {
                blocks.push((cql_block, cql_state.as_str()));
            }
            if spo_block.width > 0 {
                blocks.push((spo_block, spo_state.as_str()));
            }
            if set_block.width > 0 {
                blocks.push((set_block, set_state.as_str()));
            }
            // Change-count badge over the SCM icon's bottom-right corner. Pushed
            // after the SCM icon so the iTerm2 path (last writer wins each cell)
            // paints it on top; the Kitty path baked it at z=1 for the same
            // effect. Dropping it from the list when the count hits zero shifts
            // the emit positions, which arms the overlay's move-detection clear.
            if let Some(badge) = self.scm_change_badge.as_deref() {
                let bw = crate::iterm2_inline::count_badge_cells_w(self.scm_change_badge_count);
                let bx = scm_block.x + scm_block.width.saturating_sub(bw);
                let by = scm_block.y + scm_block.height.saturating_sub(1);
                blocks.push((
                    Rect {
                        x: bx,
                        y: by,
                        width: bw,
                        height: 1,
                    },
                    badge,
                ));
            }
            // Same badge over the Explorer icon for the unsaved-file count.
            if let Some(badge) = self.explorer_unsaved_badge.as_deref() {
                let bw = crate::iterm2_inline::count_badge_cells_w(self.explorer_unsaved_count);
                let bx = exp_block.x + exp_block.width.saturating_sub(bw);
                let by = exp_block.y + exp_block.height.saturating_sub(1);
                blocks.push((
                    Rect {
                        x: bx,
                        y: by,
                        width: bw,
                        height: 1,
                    },
                    badge,
                ));
            }
            // And over the Remote icon for the forwarded-ports count.
            if let Some(badge) = self.remote_ports_badge.as_deref() {
                let bw = crate::iterm2_inline::count_badge_cells_w(self.remote_ports_count);
                let bx = rem_block.x + rem_block.width.saturating_sub(bw);
                let by = rem_block.y + rem_block.height.saturating_sub(1);
                blocks.push((
                    Rect {
                        x: bx,
                        y: by,
                        width: bw,
                        height: 1,
                    },
                    badge,
                ));
            }
            // And the failing-test count over the Testing (beaker) icon.
            if let Some(badge) = self.testing_badge.as_deref() {
                let bw = crate::iterm2_inline::count_badge_cells_w(self.testing_badge_count);
                let bx = tst_block.x + tst_block.width.saturating_sub(bw);
                let by = tst_block.y + tst_block.height.saturating_sub(1);
                blocks.push((
                    Rect {
                        x: bx,
                        y: by,
                        width: bw,
                        height: 1,
                    },
                    badge,
                ));
            }
        }
        // Customize Layout toolbar icons (top-right of the editor / welcome).
        // Their image swaps with the side-bar / panel visibility, exactly like
        // an activity icon swaps active/inactive. Independent of the activity
        // bar, so they still emit when it's hidden.
        let li = self.layout_icon_areas;
        let hovered = |r: Rect| {
            self.pointer_cell
                .is_some_and(|(px, py)| rect_contains(r, px, py))
        };
        let side_img = match (self.show_tree, hovered(li.toggle_side_bar)) {
            (true, false) => &images.layout_sidebar_on,
            (true, true) => &images.layout_sidebar_on_hover,
            (false, false) => &images.layout_sidebar_off,
            (false, true) => &images.layout_sidebar_off_hover,
        };
        let panel_img = match (self.show_terminal, hovered(li.toggle_panel)) {
            (true, false) => &images.layout_panel_on,
            (true, true) => &images.layout_panel_on_hover,
            (false, false) => &images.layout_panel_off,
            (false, true) => &images.layout_panel_off_hover,
        };
        let cust_img = if hovered(li.customize) {
            &images.layout_customize_hover
        } else {
            &images.layout_customize
        };
        blocks.push((li.toggle_side_bar, side_img.as_str()));
        blocks.push((li.toggle_panel, panel_img.as_str()));
        blocks.push((li.customize, cust_img.as_str()));
        blocks
            .into_iter()
            // A zero-width block means the icon is hidden (the activity bar is
            // off in Customize Layout / Zen, or the toolbar isn't laid out):
            // emit nothing so the image can't ghost.
            .filter(|(block, _)| block.width > 0 && visible(*block))
            .map(|(block, state)| ((block.x, block.y), state))
            .collect()
    }

    /// True while the activity-bar gear's menu or theme picker is open, so the
    /// gear renders in its pressed/active state. Derived from the live context
    /// menu's actions rather than a separate flag that could drift out of sync
    /// with the many places the menu is dismissed.
    pub(super) fn settings_menu_active(&self) -> bool {
        self.context_menu.as_ref().is_some_and(|menu| {
            menu.items.iter().any(|entry| {
                matches!(
                    entry.action(),
                    Some(MenuAction::OpenThemePicker | MenuAction::SetTheme(_))
                )
            })
        })
    }

    /// Only iTerm2 evicts cached images, so only there do the activity
    /// icons need re-sending while nothing changed.
    pub(super) fn activity_keepalive_allowed(&self) -> bool {
        self.inline_protocol == crate::iterm2_inline::InlineImageProtocol::ITerm2
    }

    /// Whether a post-draw overlay at this cell rect may be emitted (#513).
    ///
    /// The flush block below writes with absolute CUP straight to stdout,
    /// after ratatui's diff. Two properties make a wrong origin dangerous
    /// rather than merely wrong: it is a raw write, not a buffer index, so
    /// an out-of-range origin cannot panic the way `Buffer` would; and
    /// ratatui has no record of the cells it touched, so nothing repairs
    /// them on the next frame. The corruption persists instead of clearing
    /// on repaint, which is what #510 looked like from the outside.
    ///
    /// So the payload is checked against the screen at EMIT time rather
    /// than trusting whichever cached rect produced the layout. Every
    /// writer in the block goes through it, which is the part #512's shape
    /// could not give: that fixed the terminal carrier by clearing its
    /// stored rect, but each other writer keeps its own rect with its own
    /// lifecycle, so every future site a frame can decline to paint is one
    /// more place to remember the clear.
    ///
    /// What this does NOT do, stated plainly because the bound is easy to
    /// oversell: it removes OFF-SCREEN writes only. It is a screen bound,
    /// not the owning widget's rect, so an overlay anchored on a stale rect
    /// that still lands somewhere on screen passes it untouched -- and that
    /// is the case #510 actually exhibited, a terminal image painted over a
    /// sibling pane, the sidebar and the status bar, all of them mid-screen
    /// (#512 records the landing sites). So this does not subsume #512 and
    /// does not close #513's class; it removes the unbounded half, where a
    /// wild origin can write anywhere in the terminal.
    ///
    /// The bound is the screen because the emitter does not know which
    /// widget a payload belongs to. Making it exact needs each payload to
    /// carry its owning rect, which is the wider change #513 describes.
    pub(super) fn overlay_fits_on_screen(
        &self,
        cell_x: u16,
        cell_y: u16,
        cell_w: u16,
        cell_h: u16,
    ) -> bool {
        let area = self.last_frame_area;
        if area.width == 0 || area.height == 0 {
            // No frame has been drawn yet, so there is no screen to land on.
            return false;
        }
        // A zero-sized payload has nothing to place; treat it as unemittable
        // rather than trivially in-bounds, so a cleared rect cannot slip a
        // bare cursor park through.
        if cell_w == 0 || cell_h == 0 {
            return false;
        }
        cell_x.saturating_add(cell_w) <= area.width && cell_y.saturating_add(cell_h) <= area.height
    }

    /// The editor group occupying physical column `side` (0 = left,
    /// 1 = right). When not split only side 0 exists. `editor` is always
    /// the active group, so which physical column it sits in depends on the
    /// active leaf's depth-first index in `editor_layout`.
    pub(super) fn group_on_side(&self, side: usize) -> Option<&EditorTabs> {
        // Physical column maps directly to depth-first leaf index only in
        // the two-column layout. Every other layout - unsplit, or a grid of
        // three or more groups - bakes the ACTIVE group into slot 0 and
        // leaves slot 1 dark (see the render path), the same clamp
        // `focused_image_side` applies: resolving by DFS position here made
        // the image gate and the layout key read one group while the baked
        // pixels came from another.
        if self.editor_layout.leaf_count() != 2 {
            return (side == 0).then_some(&self.editor);
        }
        // The active group is hoisted into `self.editor`.
        if side == self.editor_layout.active_dfs_index() {
            Some(&self.editor)
        } else {
            self.editor_layout.group_ref_at_dfs(side)
        }
    }

    /// Re-bake both editor image slots on the next render. Used when a
    /// full-screen modal that covered both panes closes.
    pub(super) fn invalidate_editor_image_layouts(&mut self) {
        self.overlays.editor[0].invalidate_layout();
        self.overlays.editor[1].invalidate_layout();
    }

    /// Bake an OSC-1337 inline-image escape sized to one editor column so
    /// the active image tab can be painted on top of ratatui's text
    /// buffer. `side` selects the physical column (and the overlay slot).
    /// Skipped when that column's active tab is text or when the host
    /// terminal can't render inline images (Terminal.app, raw SSH); in
    /// those cases the metadata header line painted by the editor widget
    /// is the entire preview the user sees.
    pub(super) fn update_editor_image_overlay(&mut self, side: usize, editor_area: Rect) {
        // Cheap pre-checks that don't touch the (large) image bytes, so
        // the common per-frame path skips the clone+bake entirely.
        if self.group_on_side(side).is_none_or(|g| g.image.is_none()) {
            self.disable_editor_image(side);
            return;
        }
        let Some(path) = self.group_on_side(side).and_then(|g| g.path.clone()) else {
            self.disable_editor_image(side);
            return;
        };
        if !self.inline_images_enabled() {
            self.disable_editor_image(side);
            return;
        }
        let Some((cw_px, ch_px)) = self.cell_pixel else {
            return;
        };
        // The editor widget paints its own 1-cell border + a 1-row header
        // strip; the EditorTabs widget paints a 1-row tab strip above
        // that. Carve those off before baking the image so the OSC
        // doesn't bleed over the labels.
        let tab_strip = 1u16;
        let border = 1u16;
        let header = 1u16;
        if editor_area.height < tab_strip + 2 * border + header + 2
            || editor_area.width < 2 * border + 4
        {
            self.disable_editor_image(side);
            return;
        }
        let cell_x = editor_area.x + border;
        let cell_y = editor_area.y + tab_strip + border + header;
        let cell_w = editor_area.width.saturating_sub(2 * border);
        let cell_h = editor_area
            .height
            .saturating_sub(tab_strip + 2 * border + header);
        let desired = EditorImageLayout {
            cell_x,
            cell_y,
            cell_w,
            cell_h,
            path,
            page: self
                .group_on_side(side)
                .and_then(|g| g.image.as_ref())
                .and_then(|i| i.pdf.as_ref())
                .map_or(1, |p| p.current_page),
            generation: self
                .group_on_side(side)
                .and_then(|g| g.image.as_ref())
                .map_or(0, |i| i.generation),
        };
        // The cell-buffer protocols (iTerm2 OSC-1337, Sixel) have no
        // z-layer, so the post-frame image blit would overpaint an open
        // context menu (Kitty puts the image at deep-negative z instead).
        // Hold the overlay cleared while a menu overlaps it; the layout
        // was invalidated, so the first frame after the menu closes or
        // moves off re-bakes the picture.
        if self.context_menu_covers_rect(Rect {
            x: cell_x,
            y: cell_y,
            width: cell_w,
            height: cell_h,
        }) {
            self.overlays.editor[side].request_clear_if_displayed();
            self.overlays.editor[side].invalidate_layout();
            return;
        }
        // Skip the bake when nothing about the layout changed - this is
        // the hot path on every frame an image is on screen.
        if self.overlays.editor[side].layout_matches(&desired) {
            return;
        }
        self.overlays.editor[side].request_clear_if_displayed();
        let canvas_w = cell_w as u32 * cw_px;
        let canvas_h = cell_h as u32 * ch_px;
        let bg = self.theme_bg_pixel();
        // The bytes come from the same `group_on_side` resolution the gate
        // and the layout key above used, so all three always describe one
        // group; the borrow ends at the last use inside the bake, before we
        // store into the slot.
        let baked = {
            let Some(image) = self.group_on_side(side).and_then(|g| g.image.as_ref()) else {
                return;
            };
            crate::iterm2_inline::fit_image_auto(&image.bytes, canvas_w, canvas_h, bg)
        };
        if let Ok(baked) = baked
            && let Some(raw) = crate::iterm2_inline::build_inline_image_z(
                self.inline_protocol,
                &baked,
                cell_w,
                cell_h,
                false,
                crate::iterm2_inline::KITTY_ID_EDITOR_BASE + side as u32,
                // Below text and below non-default background cells, so a
                // right-click menu or any other opaque popup paints fully on
                // top. The preview still shows: on Kitty the editor widget
                // keeps its canvas at the DEFAULT bg (`image_canvas_bg`),
                // which this z stays above — an explicit canvas fill would
                // paint over the picture. Same placement the sidebar
                // illustrations use.
                crate::iterm2_inline::KITTY_Z_BELOW_TEXT_AND_BG,
            )
        {
            let osc = crate::iterm2_inline::maybe_tmux_wrap(
                self.inline_protocol,
                crate::iterm2_inline::detect_tmux(),
                raw,
            );
            self.overlays.editor[side].set(osc, desired);
        }
    }

    pub(super) fn disable_editor_image(&mut self, side: usize) {
        self.overlays.editor[side].disable();
    }

    /// Returns true if the cached editor-image cells need to be repainted
    /// by ratatui this frame (because the user closed/switched away from
    /// the image, or the layout changed). Called from the main loop to
    /// force a full redraw before re-emitting. ORs both split slots and
    /// consumes BOTH latches (never short-circuits).
    pub fn consume_editor_image_clear(&mut self) -> bool {
        let left = self.overlays.editor[0].consume_clear();
        let right = self.overlays.editor[1].consume_clear();
        left || right
    }

    pub fn editor_image_payload(&self, side: usize) -> Option<(&str, &EditorImageLayout)> {
        if self.shortcuts_modal.is_some()
            || self.file_finder.is_some()
            || self.command_palette.is_some()
            || self.go_to_symbol.is_some()
            || self.workspace_symbols.is_some()
            || self.process_picker.is_some()
            || self.zoxide_jump.is_some()
            || self.command_history_popup.is_some()
            || self.branch_picker.is_some()
            || self.input_prompt.is_some()
            || self.list_picker.is_some()
        {
            return None;
        }
        self.overlays.editor[side].payload()
    }

    pub fn mark_editor_image_displayed(&mut self, side: usize) {
        self.overlays.editor[side].mark_displayed();
    }

    /// Bake the active terminal pane's newest captured inline image (imgcat
    /// / OSC 1337) into an overlay anchored at its grid row. One slot: older
    /// images scroll away like text, and only the active pane shows its
    /// picture (focus a pane to see what it printed). alacritty never
    /// reserved grid rows for the image, so it floats above any text
    /// printed after it — the honest best without forking the engine.
    pub(super) fn update_terminal_image_overlay(&mut self) {
        if !self.show_terminal
            || self.bottom_panel_tab != BottomPanelTab::Terminal
            || !self.inline_images_enabled()
        {
            self.disable_terminal_image();
            return;
        }
        let Some((cw_px, ch_px)) = self.cell_pixel else {
            return;
        };
        let idx = self.active_terminal;
        let Some(t) = self.terminals.get(idx) else {
            self.disable_terminal_image();
            return;
        };
        let inner = t.last_inner;
        // The floors below reject a pane the last frame did not paint ONLY
        // because the pane-skip site zeroes `last_inner` first (#510). They
        // are magnitude checks, not freshness checks: a pane painted at a
        // real size that then stops being painted keeps a full-size rect and
        // passes both. That was the bug. Delete the clear and these floors
        // will not catch it.
        if t.alt_screen() || inner.width < 6 || inner.height < 3 {
            self.disable_terminal_image();
            return;
        }
        let Some(img) = t.pane_images().pop() else {
            self.disable_terminal_image();
            return;
        };
        // Anchor row in the viewport; off-screen anchors hide the picture.
        let vp = img.line + t.scroll_display_offset();
        if vp < 0 || vp >= inner.height as i32 {
            self.disable_terminal_image();
            return;
        }
        // Image pixels → cells, shrunk (never grown) to fit the pane below
        // the anchor. Reading dimensions only decodes the header.
        let Ok(Some((px_w, px_h))) =
            image::ImageReader::new(std::io::Cursor::new(img.data.as_slice()))
                .with_guessed_format()
                .map(|r| r.into_dimensions().ok())
        else {
            self.disable_terminal_image();
            return;
        };
        let want_w = (px_w.div_ceil(cw_px)).max(1) as f32;
        let want_h = (px_h.div_ceil(ch_px)).max(1) as f32;
        let max_w = (inner.width - 2) as f32;
        let max_h = ((inner.height as i32 - vp).max(1) as f32).min(inner.height as f32 - 1.0);
        let scale = (max_w / want_w).min(max_h / want_h).min(1.0);
        let cell_w = ((want_w * scale) as u16).max(1);
        let cell_h = ((want_h * scale) as u16).max(1);
        let desired = TerminalImageLayout {
            cell_x: inner.x,
            cell_y: inner.y + vp as u16,
            cell_w,
            cell_h,
            seq: img.seq,
            pane: idx,
        };
        // An open right-click menu over the picture wins on the protocols
        // whose post-frame blit would erase the overlapped menu rows
        // (iTerm2/Sixel; Kitty solves this with the below-text z instead).
        // The editor preview applies the same rule.
        if self.context_menu_covers_rect(Rect {
            x: desired.cell_x,
            y: desired.cell_y,
            width: desired.cell_w,
            height: desired.cell_h,
        }) {
            self.overlays.terminal_image.request_clear_if_displayed();
            self.overlays.terminal_image.invalidate_layout();
            return;
        }
        // Hot path: unchanged layout means nothing to rebake and no latch.
        if self.overlays.terminal_image.layout_matches(&desired) {
            return;
        }
        // A pure anchor move (output scrolled, viewport scrolled) keeps the
        // baked payload: same picture, same cell size, same protocol — only
        // the placement row changed. Re-running the decode + resize +
        // encode of a full-resolution photo per scrolled row stalled the
        // render thread for the whole pane height.
        let reusable = self.overlays.terminal_image.has_image()
            && self.overlays.terminal_image.layout().is_some_and(|l| {
                l.seq == desired.seq
                    && l.pane == desired.pane
                    && l.cell_w == desired.cell_w
                    && l.cell_h == desired.cell_h
            });
        self.overlays.terminal_image.request_clear_if_displayed();
        if reusable {
            self.overlays.terminal_image.set_layout(desired);
            return;
        }
        let bg = self.theme_bg_pixel();
        let baked = crate::iterm2_inline::fit_image_auto(
            &img.data,
            cell_w as u32 * cw_px,
            cell_h as u32 * ch_px,
            bg,
        );
        if let Ok(baked) = baked
            && let Some(raw) = crate::iterm2_inline::build_inline_image_z(
                self.inline_protocol,
                &baked,
                cell_w,
                cell_h,
                true,
                crate::iterm2_inline::KITTY_ID_TERMINAL,
                // Below text and non-default background cells: the pane's
                // default-bg cells are Color::Reset so the picture shows,
                // while an opaque popup (the right-click menu) paints fully
                // on top. Same placement the editor preview uses.
                crate::iterm2_inline::KITTY_Z_BELOW_TEXT_AND_BG,
            )
        {
            let osc = crate::iterm2_inline::maybe_tmux_wrap(
                self.inline_protocol,
                crate::iterm2_inline::detect_tmux(),
                raw,
            );
            self.overlays.terminal_image.set(osc, desired);
        }
    }

    pub(super) fn disable_terminal_image(&mut self) {
        self.overlays.terminal_image.disable();
    }

    pub(super) fn disable_markdown_image(&mut self) {
        self.overlays.markdown_image.disable();
    }

    /// See [`Self::consume_editor_image_clear`]; the markdown twin.
    pub fn consume_markdown_image_clear(&mut self) -> bool {
        self.overlays.markdown_image.consume_clear()
    }

    pub fn mark_markdown_image_displayed(&mut self) {
        self.overlays.markdown_image.mark_displayed();
    }

    /// The markdown-preview image escape to flush this frame; same modal
    /// gate as the terminal pane picture.
    pub fn markdown_image_payload(&self) -> Option<(&str, &MarkdownImageLayout)> {
        if self.shortcuts_modal.is_some()
            || self.file_finder.is_some()
            || self.command_palette.is_some()
            || self.go_to_symbol.is_some()
            || self.workspace_symbols.is_some()
            || self.process_picker.is_some()
            || self.zoxide_jump.is_some()
            || self.command_history_popup.is_some()
            || self.branch_picker.is_some()
            || self.input_prompt.is_some()
            || self.list_picker.is_some()
        {
            return None;
        }
        self.overlays.markdown_image.payload()
    }

    /// Bake/refresh the markdown preview's inline image (#176): the
    /// TOPMOST reserved block whose anchor row is on screen paints; a
    /// bake is keyed on (path, mtime, fitted cells) and a pure scroll
    /// reuses the payload (the terminal-pane picture's contract).
    pub(super) fn update_markdown_image_overlay(&mut self) {
        if !self.inline_images_enabled() {
            self.disable_markdown_image();
            return;
        }
        let Some((cw_px, ch_px)) = self.cell_pixel else {
            return;
        };
        let Some(md) = self.editor.markdown_preview.as_ref() else {
            self.disable_markdown_image();
            return;
        };
        let area = md.last_area;
        let viewport_rows = self.editor.last_inner.height;
        if area.width < 6 || viewport_rows < 3 || md.images.is_empty() {
            self.disable_markdown_image();
            return;
        }
        let scroll = md.scroll as usize;
        let Some((img, anchor_row)) = md
            .images
            .iter()
            .zip(md.anchor_rows.iter())
            .find(|&(_, &row)| row >= scroll && row - scroll < viewport_rows as usize)
            .map(|(i, &r)| (i.clone(), r))
        else {
            self.disable_markdown_image();
            return;
        };
        let Ok(meta) = std::fs::metadata(&img.path) else {
            self.disable_markdown_image();
            return;
        };
        let mtime_ms = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let Ok((px_w, px_h)) = image::image_dimensions(&img.path) else {
            self.disable_markdown_image();
            return;
        };
        let vp = (anchor_row - scroll) as u16;
        let want_w = (px_w.div_ceil(cw_px)).max(1) as f32;
        let want_h = (px_h.div_ceil(ch_px)).max(1) as f32;
        let max_w = area.width.saturating_sub(2) as f32;
        let max_h = (img.rows.min(viewport_rows.saturating_sub(vp))).max(1) as f32;
        let scale = (max_w / want_w).min(max_h / want_h).min(1.0);
        let cell_w = ((want_w * scale) as u16).max(1);
        let cell_h = ((want_h * scale) as u16).max(1);
        let desired = MarkdownImageLayout {
            cell_x: area.x,
            cell_y: area.y + vp,
            cell_w,
            cell_h,
            path: img.path.clone(),
            mtime_ms,
        };
        if self.context_menu_covers_rect(Rect {
            x: desired.cell_x,
            y: desired.cell_y,
            width: desired.cell_w,
            height: desired.cell_h,
        }) {
            self.overlays.markdown_image.request_clear_if_displayed();
            self.overlays.markdown_image.invalidate_layout();
            return;
        }
        if self.overlays.markdown_image.layout_matches(&desired) {
            return;
        }
        let reusable = self.overlays.markdown_image.has_image()
            && self.overlays.markdown_image.layout().is_some_and(|l| {
                l.path == desired.path
                    && l.mtime_ms == desired.mtime_ms
                    && l.cell_w == desired.cell_w
                    && l.cell_h == desired.cell_h
            });
        self.overlays.markdown_image.request_clear_if_displayed();
        if reusable {
            self.overlays.markdown_image.set_layout(desired);
            return;
        }
        let Ok(bytes) = std::fs::read(&img.path) else {
            self.disable_markdown_image();
            return;
        };
        let bg = self.theme_bg_pixel();
        let baked = crate::iterm2_inline::fit_image_auto(
            &bytes,
            cell_w as u32 * cw_px,
            cell_h as u32 * ch_px,
            bg,
        );
        if let Ok(baked) = baked
            && let Some(raw) = crate::iterm2_inline::build_inline_image_z(
                self.inline_protocol,
                &baked,
                cell_w,
                cell_h,
                true,
                crate::iterm2_inline::KITTY_ID_MARKDOWN,
                crate::iterm2_inline::KITTY_Z_BELOW_TEXT_AND_BG,
            )
        {
            let osc = crate::iterm2_inline::maybe_tmux_wrap(
                self.inline_protocol,
                crate::iterm2_inline::detect_tmux(),
                raw,
            );
            self.overlays.markdown_image.set(osc, desired);
        }
    }

    /// See [`Self::consume_editor_image_clear`]; the terminal-image twin.
    pub fn consume_terminal_image_clear(&mut self) -> bool {
        self.overlays.terminal_image.consume_clear()
    }

    /// The pane image escape to flush this frame, gated off while any modal
    /// popup is up so the picture never paints over it.
    pub fn terminal_image_payload(&self) -> Option<(&str, &TerminalImageLayout)> {
        if self.shortcuts_modal.is_some()
            || self.file_finder.is_some()
            || self.command_palette.is_some()
            || self.go_to_symbol.is_some()
            || self.workspace_symbols.is_some()
            || self.process_picker.is_some()
            || self.zoxide_jump.is_some()
            || self.command_history_popup.is_some()
            || self.branch_picker.is_some()
            || self.input_prompt.is_some()
            || self.list_picker.is_some()
        {
            return None;
        }
        self.overlays.terminal_image.payload()
    }

    pub fn mark_terminal_image_displayed(&mut self) {
        self.overlays.terminal_image.mark_displayed();
    }

    /// Bake/recomposite the editor minimap for `strip` (the text-aligned image
    /// rect on the chosen edge). Mirrors `update_editor_image_overlay`: the
    /// expensive whole-file raster is cached in `minimap_base` and only rebuilt
    /// when its signature (content / size / theme) changes; scrolling reuses the
    /// raster and just recomposites the viewport box. The per-frame hot path
    /// early-returns at `layout_matches` before any allocation.
    pub(super) fn update_minimap_overlay(&mut self, strip: Rect) {
        if !self.minimap_visible || !self.inline_images_enabled() {
            self.disable_minimap_image();
            return;
        }
        let Some((cw_px, ch_px)) = self.cell_pixel else {
            self.disable_minimap_image();
            return;
        };
        if strip.width == 0 || strip.height == 0 {
            self.disable_minimap_image();
            return;
        }
        // The raster is emitted at Kitty z=0 after ratatui's diff, so anything
        // ratatui paints over the strip gets painted back over every frame.
        // Only a menu that actually reaches the strip may evict it: arming the
        // clear latch for every right-click anywhere would wipe and repaint the
        // whole screen on the cell-buffer protocols. The layout is kept, so the
        // raster comes back without a rebake once the menu closes.
        self.minimap_img_rect = strip;
        if self.menu_covers(strip) {
            self.overlays.minimap.hide_and_request_clear();
            return;
        }
        let cell_w = strip.width;
        let cell_h = strip.height;
        let bg = self.theme.editor_bg_rgb();
        // A symbol tab's minimap draws only its symbol (#369), so rows
        // count from the symbol's first line.
        let span = self.editor.minimap_span();
        let (first, end) = span;
        let top = self.editor.scroll.saturating_sub(first);
        let rows = self.editor.visible_rows();
        let total = end - first;
        let edit_seq = self.editor.edit_seq;
        // A selection is only drawn where it meets the drawn lines.
        let selection = self
            .editor
            .selection_rows()
            .filter(|&(s, e)| s < end && e >= first)
            .map(|(s, e)| (s.max(first) - first, e.min(end - 1) - first));
        let doc = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            self.editor.path.hash(&mut h);
            (h.finish(), span)
        };
        let desired = MinimapLayout {
            cell_x: strip.x,
            cell_y: strip.y,
            cell_w,
            cell_h,
            top,
            rows,
            total,
            edit_seq,
            highlight_generation: self.editor.highlight_generation(),
            side: self.minimap_side,
            bg,
            selection,
            doc,
        };
        if self.overlays.minimap.layout_matches(&desired) {
            return;
        }
        // Only a moved or resized strip needs the full-screen eviction (image
        // cells outside the new rect would ghost). A content-only change
        // (edit, scroll, selection, theme) re-emits over the SAME cells - the
        // fixed Kitty id / same-cell OSC-1337 emit replaces the image in
        // place - so it must NOT arm the clear: that `2J` + full redraw is a
        // whole-app blink on every keystroke over an SSH round trip.
        let placement_moved = self.overlays.minimap.layout().is_none_or(|prev| {
            (prev.cell_x, prev.cell_y, prev.cell_w, prev.cell_h)
                != (
                    desired.cell_x,
                    desired.cell_y,
                    desired.cell_w,
                    desired.cell_h,
                )
        });
        // Typing re-bakes the whole strip image, several kilobytes a key
        // over SSH (#682). While only the text changed, the strip catches up
        // at most every `MINIMAP_EDIT_REBAKE`; `tick_minimap` redraws once
        // the wait is over so the last edit always lands.
        let text_only = !placement_moved
            && self.overlays.minimap.layout().is_some_and(|prev| {
                (
                    prev.top,
                    prev.rows,
                    prev.side,
                    prev.bg,
                    prev.selection,
                    prev.doc,
                ) == (
                    desired.top,
                    desired.rows,
                    desired.side,
                    desired.bg,
                    desired.selection,
                    desired.doc,
                )
            });
        if text_only
            && self
                .minimap_baked_at
                .is_some_and(|t| t.elapsed() < MINIMAP_EDIT_REBAKE)
        {
            self.minimap_edit_pending = true;
            return;
        }
        self.minimap_edit_pending = false;
        self.minimap_baked_at = Some(std::time::Instant::now());
        if placement_moved {
            self.overlays.minimap.request_clear_if_displayed();
        }
        // Cold path only: clone the path into the rebake signature.
        let canvas_w = cell_w as u32 * cw_px;
        let canvas_h = cell_h as u32 * ch_px;
        // Cap each line's height so a short file stays a compact sliver at the
        // top rather than stretching down the whole strip; a file long enough
        // to fill the strip naturally reaches `canvas_h` and is unaffected.
        let content_h = (total as u32 * MINIMAP_MAX_LINE_PX).min(canvas_h).max(1);
        self.minimap_content_h = content_h;
        // Luminance picks the default text color and the viewport tint so the
        // box reads on either theme.
        let light = 0.299 * bg.0 as f32 + 0.587 * bg.1 as f32 + 0.114 * bg.2 as f32 > 140.0;
        let sig = (
            self.editor.path.clone(),
            edit_seq,
            canvas_w,
            canvas_h,
            bg,
            span,
            self.editor.highlight_generation(),
        );
        if self.minimap_base.as_ref().map(|b| &b.sig) != Some(&sig) {
            let fg = if light {
                (0x38, 0x3a, 0x41)
            } else {
                (0xc5, 0xcd, 0xd9)
            };
            let rgba = self
                .editor
                .minimap_rgba(canvas_w, canvas_h, content_h, bg, fg);
            self.minimap_base = Some(MinimapBase {
                rgba,
                w: canvas_w,
                h: canvas_h,
                sig,
            });
        }
        let Some(base) = self.minimap_base.as_ref() else {
            return;
        };
        let composited = composite_minimap_viewport(
            &base.rgba,
            base.w,
            base.h,
            MinimapOverlay {
                top,
                rows,
                total,
                light,
                selection,
                sel_rgb: self.theme.selection_rgb(),
                content_h,
            },
        );
        let (bw, bh) = (base.w, base.h);
        let Some(png) = rgba_to_png(composited, bw, bh) else {
            return;
        };
        if let Some(raw) = crate::iterm2_inline::build_inline_image(
            self.inline_protocol,
            &png,
            cell_w,
            cell_h,
            false,
            crate::iterm2_inline::KITTY_ID_MINIMAP,
        ) {
            let osc = crate::iterm2_inline::maybe_tmux_wrap(
                self.inline_protocol,
                crate::iterm2_inline::detect_tmux(),
                raw,
            );
            self.overlays.minimap.set(osc, desired);
        }
    }

    /// True once a deferred minimap bake is due, so the loop redraws and
    /// the strip shows the last edit. Consumes the pending flag: that redraw
    /// bakes (the wait is over), and if the minimap was hidden meanwhile
    /// nothing is left pending to redraw for, forever.
    pub fn tick_minimap(&mut self) -> bool {
        let due = self.minimap_edit_pending
            && self
                .minimap_baked_at
                .is_none_or(|t| t.elapsed() >= MINIMAP_EDIT_REBAKE);
        if due {
            self.minimap_edit_pending = false;
        }
        due
    }

    pub(super) fn disable_minimap_image(&mut self) {
        self.overlays.minimap.disable();
        self.minimap_img_rect = Rect::default();
        self.minimap_text = false;
    }

    /// Draw the minimap into `strip`: the raster where the terminal draws
    /// images, else braille text (#1231), so it shows in tmux, Alacritty,
    /// GNOME Terminal and plain xterm too.
    pub(super) fn paint_minimap(&mut self, frame: &mut ratatui::Frame, strip: Rect) {
        if self.inline_images_enabled() && self.cell_pixel.is_some() {
            self.minimap_text = false;
            self.update_minimap_overlay(strip);
        } else {
            self.overlays.minimap.disable();
            self.render_text_minimap(frame.buffer_mut(), strip);
        }
    }

    /// The braille minimap (#1231): the same raster the image path bakes, at
    /// one dot row a line (folded to fit a longer file), with the visible
    /// lines tinted like the image's viewport box.
    pub(super) fn render_text_minimap(&mut self, buf: &mut ratatui::buffer::Buffer, strip: Rect) {
        self.minimap_img_rect = strip;
        self.minimap_text = true;
        if strip.width == 0 || strip.height == 0 {
            return;
        }
        let (first, end) = self.editor.minimap_span();
        let total = end - first;
        let w = strip.width as u32 * 2 * MINIMAP_TEXT_CHARS_PER_DOT;
        let h = strip.height as u32 * 4;
        let content_h = (total as u32).clamp(1, h);
        self.minimap_content_h = content_h;
        let bg = self.theme.editor_bg_rgb();
        let light = 0.299 * bg.0 as f32 + 0.587 * bg.1 as f32 + 0.114 * bg.2 as f32 > 140.0;
        let fg = if light {
            (0x38, 0x3a, 0x41)
        } else {
            (0xc5, 0xcd, 0xd9)
        };
        let sig = (
            self.editor.path.clone(),
            self.editor.edit_seq,
            w,
            h,
            bg,
            (first, end),
            self.editor.highlight_generation(),
        );
        if self.minimap_text_cache.as_ref().map(|(k, _)| k) != Some(&sig) {
            let rgba = self.editor.minimap_rgba(w, h, content_h, bg, fg);
            let cells = braille_minimap(&rgba, w, h, bg, strip.width, strip.height);
            self.minimap_text_cache = Some((sig, cells));
        }
        let Some((_, cells)) = self.minimap_text_cache.as_ref() else {
            return;
        };
        // The visible lines, in cell rows, get a faint wash of the text colour.
        let top = self.editor.scroll.saturating_sub(first);
        let rows = self.editor.visible_rows();
        let to_cell = |line: usize| line * content_h as usize / total.max(1) / 4;
        let (view_top, view_end) = (to_cell(top), to_cell(top + rows.max(1) - 1));
        let mix = |a: u8, b: u8| ((a as u16 * 85 + b as u16 * 15) / 100) as u8;
        let wash = Color::Rgb(mix(bg.0, fg.0), mix(bg.1, fg.1), mix(bg.2, fg.2));
        for (i, cell) in cells.iter().enumerate() {
            let (cx, cy) = (i % strip.width as usize, i / strip.width as usize);
            let back = if total > 0 && (view_top..=view_end).contains(&cy) {
                wash
            } else {
                Color::Rgb(bg.0, bg.1, bg.2)
            };
            let Some(out) = buf.cell_mut((strip.x + cx as u16, strip.y + cy as u16)) else {
                continue;
            };
            match cell {
                Some((glyph, (r, g, b))) => {
                    out.set_char(*glyph).set_fg(Color::Rgb(*r, *g, *b));
                }
                None => {
                    out.set_char(' ');
                }
            }
            out.set_bg(back);
        }
    }

    /// Show/hide the editor minimap (⌥⌘M, the minimap right-click menu, the
    /// Customize Layout menu, and the "View: Toggle Minimap" palette command
    /// all route here). Hiding evicts the cached strip image so it can't ghost.
    pub(super) fn toggle_minimap(&mut self) {
        self.minimap_visible = !self.minimap_visible;
        if !self.minimap_visible {
            self.disable_minimap_image();
        }
        self.persist_layout();
        self.status = if self.minimap_visible && !self.minimap_room {
            format!(
                "Minimap on: it shows once the editor is {} columns wide",
                MINIMAP_WIDTH_CELLS + MINIMAP_MIN_EDITOR_WIDTH
            )
        } else if self.minimap_visible {
            String::from("Minimap on")
        } else {
            String::from("Minimap off")
        };
    }

    /// Carve the minimap strip off `editor_area` (the active group's rect in a
    /// split) when the minimap is on and the editor has room. Returns
    /// `(editor_rect, full_strip, image_rect)`: the editor renders into
    /// `editor_rect`, the whole `full_strip` column is bg-filled, and the
    /// raster image occupies `image_rect` (aligned to the editor's text rows).
    pub(super) fn carve_minimap(&mut self, editor_area: Rect) -> Option<(Rect, Rect, Rect)> {
        self.minimap_room = editor_area.width >= MINIMAP_WIDTH_CELLS + MINIMAP_MIN_EDITOR_WIDTH;
        if !self.minimap_visible || !self.minimap_room {
            return None;
        }
        let w = MINIMAP_WIDTH_CELLS;
        let (ed_x, strip_x) = match self.minimap_side {
            MinimapSide::Right => (editor_area.x, editor_area.x + editor_area.width - w),
            MinimapSide::Left => (editor_area.x + w, editor_area.x),
        };
        let ed_rect = Rect {
            x: ed_x,
            y: editor_area.y,
            width: editor_area.width - w,
            height: editor_area.height,
        };
        let full_strip = Rect {
            x: strip_x,
            y: editor_area.y,
            width: w,
            height: editor_area.height,
        };
        // Align the image to the editor's text rows (tab strip + border +
        // header above, border below) so it tracks the text vertically.
        let (tab, border, header) = (1u16, 1u16, 1u16);
        let img = if editor_area.height > tab + 2 * border + header {
            Rect {
                x: strip_x,
                y: editor_area.y + tab + border + header,
                width: w,
                height: editor_area.height - (tab + 2 * border + header),
            }
        } else {
            full_strip
        };
        Some((ed_rect, full_strip, img))
    }

    /// The minimap column's share of the editor header on a `surfaces`
    /// theme, painted after the editor: the tab-strip row in the strip's
    /// chrome, so the strip (and the layout icons on it) runs to the edge
    /// instead of stopping a canvas-black column short, and the seam under
    /// the breadcrumbs carried across the column. The editor's rule has
    /// eased into the hairline by its far end, so the column takes that.
    pub(super) fn paint_minimap_header(&self, buf: &mut ratatui::buffer::Buffer, column: Rect) {
        let Some(t) = crate::widgets::surface::tones(self.theme) else {
            return;
        };
        let column = column.intersection(buf.area);
        if column.height == 0 {
            return;
        }
        let strip_row = Rect {
            height: 1,
            ..column
        };
        buf.set_style(strip_row, Style::default().bg(self.theme.tab_strip_bg()));
        let body = self.editor.last_body;
        if body.height >= 2 && body.y > column.y && body.y < column.bottom() {
            for x in column.left()..column.right() {
                let cell = &mut buf[(x, body.y)];
                if cell.symbol() == " " {
                    cell.set_symbol("\u{2594}");
                    cell.set_fg(t.hairline());
                }
            }
        }
    }

    /// Whether the open context menu (or its submenu, which floats further
    /// out) overlaps `r`. Menus are clamped to the frame, not snapped to it, so
    /// a right-click far from the minimap leaves the strip untouched.
    pub(super) fn menu_covers(&self, r: Rect) -> bool {
        if r.width == 0 || r.height == 0 {
            return false;
        }
        [self.menu_rect(), self.submenu_rect()]
            .into_iter()
            .flatten()
            .any(|m| m.intersects(r))
    }

    pub fn minimap_image_payload(&self) -> Option<(&str, &MinimapLayout)> {
        if self.shortcuts_modal.is_some()
            || self.file_finder.is_some()
            || self.command_palette.is_some()
            || self.go_to_symbol.is_some()
            || self.workspace_symbols.is_some()
            || self.process_picker.is_some()
            || self.zoxide_jump.is_some()
            || self.command_history_popup.is_some()
            || self.branch_picker.is_some()
            || self.input_prompt.is_some()
            || self.list_picker.is_some()
            || self.menu_covers(self.minimap_img_rect)
        {
            return None;
        }
        self.overlays.minimap.payload()
    }

    pub fn consume_minimap_image_clear(&mut self) -> bool {
        self.overlays.minimap.consume_clear()
    }

    pub fn mark_minimap_image_displayed(&mut self) {
        self.overlays.minimap.mark_displayed();
    }

    /// Map a click row inside the minimap strip to a document line and jump
    /// there, centered. The vertical mapping uses the raster's `content_h`
    /// (the lines occupy only the top `content_h` px for a short file), so a
    /// click lands on the line actually under the cursor.
    pub(super) fn minimap_scroll_to_row(&mut self, row: u16) {
        let r = self.minimap_img_rect;
        let (first, end) = self.editor.minimap_span();
        let total = end - first;
        if r.height == 0 || total == 0 {
            return;
        }
        // The text strip maps four dot rows to a cell (#1231).
        let ch_px = if self.minimap_text {
            4
        } else {
            self.cell_pixel.map(|(_, h)| h).unwrap_or(1).max(1)
        };
        let dy_px = (row.saturating_sub(r.y)) as u32 * ch_px;
        let content_h = self.minimap_content_h.max(1);
        let line = first + (dy_px as usize * total / content_h as usize).min(total - 1);
        self.editor.goto_line_centered(line);
        self.poke_cursor();
    }

    /// Right-click on the minimap strip: a compact menu mirroring VS Code's
    /// (Minimap toggle + Side submenu).
    pub(super) fn open_minimap_menu(&mut self, col: u16, row: u16) {
        let check = |on: bool| if on { "\u{2713} " } else { "  " };
        let items = vec![
            MenuEntry::Item {
                label: format!("{}Minimap", check(self.minimap_visible)),
                action: MenuAction::ToggleMinimap,
            },
            MenuEntry::Submenu {
                label: String::from("Side"),
                items: vec![
                    MenuEntry::Item {
                        label: format!("{}Left", check(self.minimap_side == MinimapSide::Left)),
                        action: MenuAction::SetMinimapSide(MinimapSide::Left),
                    },
                    MenuEntry::Item {
                        label: format!("{}Right", check(self.minimap_side == MinimapSide::Right)),
                        action: MenuAction::SetMinimapSide(MinimapSide::Right),
                    },
                ],
            },
        ];
        self.context_menu = Some(ContextMenu {
            crumb: None,
            scroll: Default::default(),
            submenu_scroll: Default::default(),
            origin: (col, row),
            items,
            selected: 0,
            open_submenu: None,
            submenu_selected: 0,
            target_dir: self.tree.root.clone(),
        });
    }
}

/// The SGR prefix (reset + truecolor fg/bg) for one ratatui cell, used when
/// blitting a buffer region straight to the terminal in
/// `flush_tooltip_over_image`. The tooltip only uses RGB and reset colors, so
/// other `Color` variants fall through to the terminal default.
pub(super) fn sgr_for_cell(cell: &ratatui::buffer::Cell) -> String {
    let mut s = String::from("\x1b[0m");
    if let Color::Rgb(r, g, b) = cell.fg {
        s.push_str(&format!("\x1b[38;2;{r};{g};{b}m"));
    }
    if let Color::Rgb(r, g, b) = cell.bg {
        s.push_str(&format!("\x1b[48;2;{r};{g};{b}m"));
    }
    s
}

/// True when two cell rectangles share at least one cell. Used to decide
/// whether a text overlay (the chrome tooltip) lands on top of an OSC-1337
/// inline image; since terminal images composite ABOVE text cells, the
/// overlapping tooltip must be re-painted last to win those cells.
pub(super) fn rects_intersect(a: Rect, b: Rect) -> bool {
    a.width > 0
        && a.height > 0
        && b.width > 0
        && b.height > 0
        && a.x < b.x.saturating_add(b.width)
        && b.x < a.x.saturating_add(a.width)
        && a.y < b.y.saturating_add(b.height)
        && b.y < a.y.saturating_add(a.height)
}

/// A fingerprint of the cells of `buf` in the given rectangle: symbols,
/// colours and modifiers, so any write ratatui makes there changes it.
pub(super) fn cells_fingerprint(
    buf: &ratatui::buffer::Buffer,
    x: u16,
    y: u16,
    w: u16,
    h: u16,
) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    let area = buf.area;
    let (x0, y0) = (x.max(area.x), y.max(area.y));
    let x1 = x.saturating_add(w).min(area.x + area.width);
    let y1 = y.saturating_add(h).min(area.y + area.height);
    (x0, y0, x1, y1).hash(&mut hasher);
    for row in y0..y1 {
        for col in x0..x1 {
            let cell = &buf[(col, row)];
            cell.symbol().hash(&mut hasher);
            (cell.fg, cell.bg, cell.modifier).hash(&mut hasher);
        }
    }
    hasher.finish()
}

impl App {
    /// This frame's [`ImageUnderlays`], read from the buffer just drawn.
    pub(super) fn image_underlays(&self, buf: &ratatui::buffer::Buffer) -> ImageUnderlays {
        // A one-cell ring around each icon's block (the widest is 4x2).
        let activity: Vec<u64> = self
            .overlays
            .activity
            .last_positions()
            .iter()
            .map(|&(x, y)| cells_fingerprint(buf, x.saturating_sub(1), y.saturating_sub(1), 6, 4))
            .collect();
        // Kitty keeps pictures on their own layer: text written into the
        // cells never deletes a placement (only a delete command or a screen
        // clear does, and both forget what was sent). So on Kitty the cells
        // beneath do not matter, and streaming output beside an image sends
        // nothing.
        if self.inline_protocol == crate::iterm2_inline::InlineImageProtocol::Kitty {
            return ImageUnderlays {
                activity,
                editor: [0, 0],
                terminal: 0,
                markdown: 0,
                minimap: 0,
            };
        }
        let o = &self.overlays;
        let editor = |side: usize| {
            o.editor[side].layout().map_or(0, |l| {
                cells_fingerprint(buf, l.cell_x, l.cell_y, l.cell_w, l.cell_h)
            })
        };
        ImageUnderlays {
            activity,
            editor: [editor(0), editor(1)],
            terminal: o.terminal_image.layout().map_or(0, |l| {
                cells_fingerprint(buf, l.cell_x, l.cell_y, l.cell_w, l.cell_h)
            }),
            markdown: o.markdown_image.layout().map_or(0, |l| {
                cells_fingerprint(buf, l.cell_x, l.cell_y, l.cell_w, l.cell_h)
            }),
            minimap: o.minimap.layout().map_or(0, |l| {
                cells_fingerprint(buf, l.cell_x, l.cell_y, l.cell_w, l.cell_h)
            }),
        }
    }

    /// Keep the buffer ratatui just drew for [`Self::chrome_image_due`], on
    /// the terminals it reads it for: every one but Kitty's protocol, which
    /// keeps pictures on a layer of their own. On Kitty (Ghostty, kitty) the
    /// copy was a whole screen of cells cloned after every frame and never
    /// read, so none is kept there.
    pub(super) fn keep_drawn_buffer(&mut self, drawn: &ratatui::buffer::Buffer) {
        if self.inline_protocol == crate::iterm2_inline::InlineImageProtocol::Kitty {
            self.drawn_buffer = None;
            return;
        }
        match self.drawn_buffer.as_mut() {
            // Into the cells of the last copy, so a frame reuses them
            // rather than allocating a screen's worth anew.
            Some(kept) => {
                kept.area = drawn.area;
                kept.content.clone_from(&drawn.content);
            }
            None => self.drawn_buffer = Some(drawn.clone()),
        }
    }

    /// Whether a small chrome image must be sent this frame (#682). These
    /// used to go out after every redraw, so a keystroke re-sent each one
    /// over SSH. Now one goes out when it is new, changed or moved, and on
    /// iTerm2 and Sixel also when the cells around it were repainted (iTerm2
    /// evicts an image under neighbouring traffic, and a cell-buffer picture
    /// is erased by writes into it). Kitty keeps images on their own layer.
    pub(super) fn chrome_image_due(&mut self, key: &'static str, image: &str, at: Rect) -> bool {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        image.hash(&mut h);
        (at.x, at.y, at.width, at.height).hash(&mut h);
        if self.inline_protocol != crate::iterm2_inline::InlineImageProtocol::Kitty
            && let Some(buf) = &self.drawn_buffer
        {
            cells_fingerprint(
                buf,
                at.x.saturating_sub(1),
                at.y.saturating_sub(1),
                at.width.saturating_add(2),
                at.height.saturating_add(2),
            )
            .hash(&mut h);
        }
        let sig = h.finish();
        self.chrome_seen.insert(key);
        self.chrome_sent.insert(key, sig) != Some(sig)
    }

    /// Drop the record of chrome images that were not on screen this frame,
    /// so they are sent again when they come back.
    pub(super) fn end_chrome_flush(&mut self) {
        let seen = std::mem::take(&mut self.chrome_seen);
        self.chrome_sent.retain(|k, _| seen.contains(k));
    }

    /// The screen was wiped or its images evicted: every large image is
    /// sent again on the next flush.
    pub(super) fn forget_sent_images(&mut self) {
        self.chrome_sent.clear();
        self.overlays.activity.forget_sent();
        for side in 0..2 {
            self.overlays.editor[side].forget_sent();
        }
        self.overlays.terminal_image.forget_sent();
        self.overlays.markdown_image.forget_sent();
        self.overlays.minimap.forget_sent();
    }
}
