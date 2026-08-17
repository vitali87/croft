//! SVG file preview rasterisation (#175): parse with usvg, render with
//! resvg into a PNG the existing image-overlay pipeline consumes
//! unchanged. The fontdb behind `<text>` elements is built lazily on the
//! FIRST preview — never at startup, where init_graphics' icon bake is
//! the critical path and codicons contain no text.

use std::sync::OnceLock;

/// Longest raster edge. High enough that any pane (retina cells
/// included) downscales rather than upscales, small enough that encode
/// and overlay bake stay instant. Matches the PDF path's philosophy:
/// one fixed-quality raster per open, no re-render on pane resize.
pub const RASTER_LONG_EDGE: u32 = 1600;

/// Source-size cap: an SVG is XML, and pathological megabyte-scale
/// documents belong in the text editor, not the parser.
pub const MAX_SVG_BYTES: u64 = 20 * 1024 * 1024;

fn fontdb() -> &'static resvg::usvg::fontdb::Database {
    static DB: OnceLock<resvg::usvg::fontdb::Database> = OnceLock::new();
    DB.get_or_init(|| {
        use resvg::usvg::fontdb::{Family, Query};
        let mut db = resvg::usvg::fontdb::Database::new();
        db.load_system_fonts();
        // fontdb's generic-family defaults name fonts (Arial, Times New
        // Roman) that plain Linux boxes rarely install, and an
        // unresolvable family drops the text run entirely. When the
        // generics resolve to nothing, remap them all to whatever face
        // the host actually has — imperfect typography beats invisible
        // text.
        let generics_resolve = db
            .query(&Query {
                families: &[Family::SansSerif, Family::Serif, Family::Monospace],
                ..Default::default()
            })
            .is_some();
        if !generics_resolve {
            let first = db.faces().next().map(|f| f.families[0].0.clone());
            if let Some(name) = first {
                db.set_sans_serif_family(name.clone());
                db.set_serif_family(name.clone());
                db.set_monospace_family(name.clone());
                db.set_cursive_family(name.clone());
                db.set_fantasy_family(name);
            }
        }
        db
    })
}

/// True when the host has any fonts for `<text>` to shape with. Test
/// hook: a fontless container renders text as nothing, which is a host
/// property, not a defect to assert on.
#[cfg(test)]
pub fn has_fonts() -> bool {
    !fontdb().faces().next().is_none()
}

/// Rasterise `svg` to PNG bytes. The output preserves the source aspect
/// ratio, scaled so the longest edge is [`RASTER_LONG_EDGE`] (never
/// upscaled past 8x natural size, so a 16px icon stays a crisp small
/// image instead of a 1600px blur). Returns the PNG plus its pixel
/// dimensions.
pub fn rasterize(svg: &[u8]) -> Result<(Vec<u8>, u32, u32), String> {
    let opts = resvg::usvg::Options {
        fontdb: std::sync::Arc::new(fontdb().clone()),
        // The fallback for text WITHOUT a font-family: usvg's default is
        // "Times New Roman", unresolvable on most Linux hosts; the
        // generic goes through the fontdb mappings fixed up above.
        font_family: String::from("sans-serif"),
        ..Default::default()
    };
    let tree = resvg::usvg::Tree::from_data(svg, &opts).map_err(|e| e.to_string())?;
    let size = tree.size();
    let (nw, nh) = (size.width().max(1.0), size.height().max(1.0));
    let long = nw.max(nh);
    let scale = (RASTER_LONG_EDGE as f32 / long).min(8.0);
    let (w, h) = (
        (nw * scale).round().max(1.0) as u32,
        (nh * scale).round().max(1.0) as u32,
    );
    let mut pixmap = resvg::tiny_skia::Pixmap::new(w, h)
        .ok_or_else(|| String::from("raster dimensions overflow"))?;
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    // tiny-skia stores premultiplied RGBA; the PNG wants straight.
    let mut rgba = pixmap.data().to_vec();
    for px in rgba.chunks_exact_mut(4) {
        let a = px[3] as u16;
        if a > 0 && a < 255 {
            px[0] = (px[0] as u16 * 255 / a).min(255) as u8;
            px[1] = (px[1] as u16 * 255 / a).min(255) as u8;
            px[2] = (px[2] as u16 * 255 / a).min(255) as u8;
        }
    }
    let img = image::RgbaImage::from_raw(w, h, rgba)
        .ok_or_else(|| String::from("raster buffer size mismatch"))?;
    let mut png = std::io::Cursor::new(Vec::new());
    img.write_to(&mut png, image::ImageFormat::Png)
        .map_err(|e| e.to_string())?;
    Ok((png.into_inner(), w, h))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rasterizes_shapes_to_a_decodable_png_at_the_long_edge() {
        let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" width="400" height="200">
            <rect x="0" y="0" width="400" height="200" fill="#ff0000"/>
        </svg>"##;
        let (png, w, h) = rasterize(svg).unwrap();
        assert_eq!((w, h), (RASTER_LONG_EDGE, RASTER_LONG_EDGE / 2));
        let img = image::load_from_memory(&png).unwrap().to_rgba8();
        assert_eq!(img.dimensions(), (w, h));
        let px = img.get_pixel(w / 2, h / 2);
        assert_eq!((px[0], px[1], px[2], px[3]), (255, 0, 0, 255), "solid red");
    }

    #[test]
    fn tiny_svgs_are_not_upscaled_past_8x() {
        let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16">
            <circle cx="8" cy="8" r="8" fill="#00ff00"/>
        </svg>"##;
        let (_, w, h) = rasterize(svg).unwrap();
        assert_eq!((w, h), (128, 128), "16px icon caps at 8x, not 1600px");
    }

    #[test]
    fn text_elements_shape_when_the_host_has_fonts() {
        if !has_fonts() {
            // A fontless container cannot shape text; nothing to assert.
            return;
        }
        let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" width="200" height="60">
            <text x="10" y="40" font-size="40" fill="#000000">Hi</text>
        </svg>"##;
        let (png, w, h) = rasterize(svg).unwrap();
        let img = image::load_from_memory(&png).unwrap().to_rgba8();
        let inked = img.pixels().filter(|p| p[3] > 0).count();
        assert!(
            inked > 100,
            "text must rasterise to visible pixels, got {inked} inked of {}",
            w * h
        );
    }

    #[test]
    fn invalid_svg_reports_an_error_instead_of_panicking() {
        let _ = rasterize(b"<svg"); // truncated: any Result, no panic
        assert!(rasterize(b"not xml at all").is_err());
        assert!(rasterize(b"").is_err());
    }
}
