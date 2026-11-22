//! `croft plot`: a chart of what arrives on stdin, printed as an inline image where the
//! terminal shows one and as text otherwise, in the active theme's colours.

use super::*;

/// `croft plot` (#361): parse stdin, draw, and print either an inline image
/// or the text chart. The image path is taken only when the terminal
/// advertises an inline protocol in the environment (iTerm2/Kitty); the
/// sixel DA1 probe needs a raw-mode tty on stdin, and stdin is the data
/// pipe here, so sixel hosts get the text chart. Everything else, and
/// `--text`, gets braille/blocks.
/// Exit 1 with the parse error on bad input rather than drawing a blank.
pub(super) fn plot(
    kind: &str,
    x: Option<&str>,
    y: &[String],
    title: Option<&str>,
    width: u16,
    height: u16,
    text_only: bool,
) -> Result<()> {
    use std::io::{IsTerminal, Read, Write};
    let kind: crate::plot::ChartKind = kind.parse().map_err(|e: String| anyhow::anyhow!(e))?;
    let mut input = String::new();
    std::io::stdin()
        .read_to_string(&mut input)
        .context("reading stdin")?;
    let dataset = crate::plot::parse(&input, x, y).map_err(|e| anyhow::anyhow!(e))?;
    if dataset.is_empty() {
        anyhow::bail!("no rows to plot");
    }
    let rows = if kind == crate::plot::ChartKind::Spark {
        1
    } else {
        height
    };
    let mut out = std::io::stdout();
    if !text_only && out.is_terminal() {
        let protocol = crate::iterm2_inline::detect_inline_image_protocol();
        if protocol != crate::iterm2_inline::InlineImageProtocol::None {
            let palette = plot_palette();
            // Cells are roughly 1:2, so the raster keeps the on-screen aspect.
            let svg = crate::plot::svg(
                &dataset,
                kind,
                title,
                width as u32 * 10,
                rows as u32 * 20,
                &palette,
            );
            let (png, _, _) =
                crate::svg::rasterize(svg.as_bytes()).map_err(|e| anyhow::anyhow!(e))?;
            if let Some(seq) = crate::iterm2_inline::build_inline_image(
                protocol,
                &png,
                width,
                rows,
                true,
                crate::iterm2_inline::KITTY_ID_PLOT,
            ) {
                out.write_all(seq.as_bytes())?;
                // Kitty leaves the cursor on the image's first row (C=1):
                // newlines, not a cursor-down, so an image emitted near the
                // bottom scrolls up instead of the prompt landing on it.
                if protocol == crate::iterm2_inline::InlineImageProtocol::Kitty {
                    out.write_all("\n".repeat(rows as usize).as_bytes())?;
                } else {
                    out.write_all(b"\n")?;
                }
                return out.flush().context("writing the chart");
            }
        }
    }
    out.write_all(crate::plot::text(&dataset, kind, title, width, rows).as_bytes())?;
    out.flush().context("writing the chart")
}

/// The chart palette from the active theme: its editor background and
/// accent, with the rest of the series colours fixed so they stay
/// distinguishable on any background.
pub(super) fn plot_palette() -> crate::plot::Palette {
    let mut palette = crate::plot::Palette::default();
    if let Ok(prefs) = crate::prefs::Prefs::load(&crate::prefs::config_path()) {
        let theme = prefs.theme();
        palette.background = theme.editor_bg_rgb();
        palette.series[0] = theme.accent_rgb();
    }
    palette
}
