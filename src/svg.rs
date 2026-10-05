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

/// Deepest element nesting the preview will parse. usvg's XML parser
/// recurses per level with no limit of its own, and running out of stack
/// aborts the whole process (#1135); real SVGs nest a few dozen levels.
pub const MAX_SVG_DEPTH: usize = 256;

/// Entity references the XML parser expands inside one another before
/// it gives up (roxmltree's loop guard): the most times one entity
/// value's markup can stack on a single path.
const ENTITY_EXPANSION_DEPTH: usize = 10;

/// The deepest element nesting the parser can reach in `svg`, from a
/// linear scan that never recurses per level. Comments, CDATA,
/// processing instructions and declarations open no element; attribute
/// values are skipped whole, so a `>` or `/>` quoted inside one ends
/// nothing. Markup quoted in a declaration (an entity value) is counted
/// as if every expansion stacked it, since the parser expands `&name;`
/// in place. Stops early once the depth passes `limit`, so a refused
/// file costs no more than its first `limit` levels.
pub fn nesting_depth(svg: &[u8], limit: usize) -> usize {
    let (mut depth, mut max, mut i) = (0usize, 0usize, 0usize);
    let mut quoted = 0usize;
    let worst = |max: usize, quoted: usize| {
        max.saturating_add(quoted.saturating_mul(ENTITY_EXPANSION_DEPTH))
    };
    while i < svg.len() && worst(max, quoted) <= limit {
        if svg[i] != b'<' {
            i += 1;
            continue;
        }
        let rest = &svg[i..];
        if rest.starts_with(b"<!--") {
            i = find(svg, i + 4, b"-->");
        } else if rest.starts_with(b"<![CDATA[") {
            i = find(svg, i + 9, b"]]>");
        } else if rest.starts_with(b"<?") {
            i = find(svg, i + 2, b"?>");
        } else if rest.starts_with(b"<!") {
            let (end, deepest) = skip_declaration(svg, i + 2, limit);
            quoted = quoted.max(deepest);
            i = end;
        } else if rest.starts_with(b"</") {
            depth = depth.saturating_sub(1);
            i = find(svg, i + 2, b">");
        } else {
            let mut j = i + 1;
            let mut quote = None;
            while j < svg.len() {
                match (quote, svg[j]) {
                    (Some(q), c) if c == q => quote = None,
                    (None, b'"' | b'\'') => quote = Some(svg[j]),
                    (None, b'>') => break,
                    _ => {}
                }
                j += 1;
            }
            if svg.get(j.wrapping_sub(1)) != Some(&b'/') || quote.is_some() {
                depth += 1;
                max = max.max(depth);
            }
            i = j + 1;
        }
    }
    worst(max, quoted)
}

/// The index just past the first `pat` at or after `from`, or the end.
fn find(svg: &[u8], from: usize, pat: &[u8]) -> usize {
    svg.get(from..)
        .and_then(|rest| rest.windows(pat.len()).position(|w| w == pat))
        .map_or(svg.len(), |i| from + i + pat.len())
}

/// Skips a `<!...>` declaration whose body starts at `from`. Quoted
/// literals are skipped whole, and only an unquoted `[` opens a DOCTYPE's
/// internal subset, which ends at its unquoted `]` (comments and
/// processing instructions inside it skipped). A subset that never ends
/// resumes the scan right after its `[`, so nothing after it goes
/// uncounted. Returns where the scan resumes and the deepest nesting
/// written inside any literal.
fn skip_declaration(svg: &[u8], from: usize, limit: usize) -> (usize, usize) {
    let (mut j, mut deepest, mut subset) = (from, 0usize, None);
    while j < svg.len() {
        let rest = &svg[j..];
        match svg[j] {
            q @ (b'"' | b'\'') => {
                let close = rest[1..].iter().position(|&c| c == q);
                let end = close.map_or(svg.len(), |k| j + 1 + k);
                deepest = deepest.max(nesting_depth(&svg[j + 1..end], limit));
                j = end + 1;
                continue;
            }
            b'<' if subset.is_some() && rest.starts_with(b"<!--") => {
                j = find(svg, j + 4, b"-->");
                continue;
            }
            b'<' if subset.is_some() && rest.starts_with(b"<?") => {
                j = find(svg, j + 2, b"?>");
                continue;
            }
            b'[' if subset.is_none() => subset = Some(j + 1),
            b']' if subset.is_some() => {
                return (find(svg, j + 1, b">"), deepest);
            }
            b'>' if subset.is_none() => return (j + 1, deepest),
            _ => {}
        }
        j += 1;
    }
    (subset.unwrap_or(svg.len()), deepest)
}

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
    fontdb().faces().next().is_some()
}

/// Rasterise `svg` to PNG bytes. The output preserves the source aspect
/// ratio, scaled so the longest edge is [`RASTER_LONG_EDGE`] (never
/// upscaled past 8x natural size, so a 16px icon stays a crisp small
/// image instead of a 1600px blur). Returns the PNG plus its pixel
/// dimensions.
pub fn rasterize(svg: &[u8]) -> Result<(Vec<u8>, u32, u32), String> {
    if nesting_depth(svg, MAX_SVG_DEPTH) > MAX_SVG_DEPTH {
        return Err(format!(
            "SVG nested too deeply to render (over {MAX_SVG_DEPTH} levels)"
        ));
    }
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

    fn nested(levels: usize) -> Vec<u8> {
        let mut s =
            String::from(r#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10">"#);
        s.push_str(&"<g>".repeat(levels));
        s.push_str(r##"<rect width="5" height="5" fill="#0000ff"/>"##);
        s.push_str(&"</g>".repeat(levels));
        s.push_str("</svg>");
        s.into_bytes()
    }

    #[test]
    fn a_deeply_nested_svg_is_refused_instead_of_overflowing_the_stack() {
        // #1135: the XML parser recurses per level with no limit, and the
        // overflow aborts the process (no panic hook, no hot exit).
        let err = rasterize(&nested(50_000)).unwrap_err();
        assert!(err.contains("nested too deeply"), "{err}");
    }

    #[test]
    fn ordinary_nesting_still_renders() {
        // Negative: real SVGs nest a few dozen levels at most.
        let (png, _, _) = rasterize(&nested(100)).unwrap();
        let img = image::load_from_memory(&png).unwrap().to_rgba8();
        assert!(
            img.pixels().any(|p| p[2] == 255 && p[3] == 255),
            "the rect draws"
        );
    }

    #[test]
    fn nesting_depth_skips_markup_that_opens_no_element() {
        assert_eq!(
            nesting_depth(b"<svg><g><g/></g><g></g></svg>", usize::MAX),
            2
        );
        assert_eq!(
            nesting_depth(
                br#"<?xml version="1.0"?><!DOCTYPE svg [<!ENTITY a "x>y">]><svg></svg>"#,
                usize::MAX
            ),
            1
        );
        assert_eq!(
            nesting_depth(
                b"<svg><!-- <g><g><g> --><![CDATA[<g><g>]]></svg>",
                usize::MAX
            ),
            1
        );
        assert_eq!(
            nesting_depth(br#"<svg><g a="/>" b='>'><rect/></g></svg>"#, usize::MAX),
            2
        );
        assert_eq!(
            nesting_depth(b"<svg><g></svg>", usize::MAX),
            2,
            "unbalanced input still counts"
        );
    }

    fn wrapped(prolog: &str, levels: usize) -> Vec<u8> {
        let mut s = String::from(prolog);
        s.push_str(&String::from_utf8(nested(levels)).unwrap());
        s.into_bytes()
    }

    #[test]
    fn a_bracket_quoted_in_the_doctype_does_not_hide_the_nesting() {
        // A `[` inside the DOCTYPE's quoted system id opens no internal
        // subset; treating it as one skipped the whole document.
        let svg = wrapped(r#"<!DOCTYPE svg SYSTEM "a[b.dtd">"#, 50_000);
        assert!(nesting_depth(&svg, MAX_SVG_DEPTH) > MAX_SVG_DEPTH);
        let err = rasterize(&svg).unwrap_err();
        assert!(err.contains("nested too deeply"), "{err}");
    }

    #[test]
    fn an_unterminated_internal_subset_does_not_hide_the_nesting() {
        let svg = wrapped("<!DOCTYPE svg [ <!ENTITY a 'b'> >", 50_000);
        assert!(nesting_depth(&svg, MAX_SVG_DEPTH) > MAX_SVG_DEPTH);
    }

    #[test]
    fn a_quoted_subset_end_does_not_hide_the_nesting() {
        // `]>` inside an entity value does not close the internal subset.
        let svg = wrapped(r#"<!DOCTYPE svg [ <!ENTITY a "]>"> ]>"#, 50_000);
        assert!(nesting_depth(&svg, MAX_SVG_DEPTH) > MAX_SVG_DEPTH);
    }

    #[test]
    fn nesting_hidden_in_an_entity_is_refused() {
        // The parser expands `&deep;` in place, so the markup in its value
        // nests as deep as if it were written out.
        let mut deep = "<g>".repeat(50_000);
        deep.push_str(&"</g>".repeat(50_000));
        let mut s = format!(r#"<!DOCTYPE svg [ <!ENTITY deep "{deep}"> ]>"#);
        s.push_str(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10">&deep;</svg>"#,
        );
        assert!(nesting_depth(s.as_bytes(), MAX_SVG_DEPTH) > MAX_SVG_DEPTH);
        let err = rasterize(s.as_bytes()).unwrap_err();
        assert!(err.contains("nested too deeply"), "{err}");
    }

    #[test]
    fn markup_in_an_entity_counts_once_per_possible_expansion() {
        let svg = br#"<!DOCTYPE svg [<!ENTITY a "<g><g>x</g></g>">]><svg>&a;</svg>"#;
        assert_eq!(
            nesting_depth(svg, usize::MAX),
            1 + 2 * ENTITY_EXPANSION_DEPTH
        );
    }

    #[test]
    fn the_scan_stops_once_the_limit_is_passed() {
        // Only "more than the limit" matters, so a huge file is not
        // walked to its end.
        assert_eq!(
            nesting_depth(&nested(50_000), MAX_SVG_DEPTH),
            MAX_SVG_DEPTH + 1
        );
        assert_eq!(nesting_depth(&nested(3), MAX_SVG_DEPTH), 4);
    }

    #[test]
    fn an_svg_with_plain_entities_still_renders() {
        // Negative: Illustrator exports declare namespace entities in an
        // internal subset; those add no nesting.
        let svg = wrapped(
            r#"<?xml version="1.0"?>
<!DOCTYPE svg PUBLIC "-//W3C//DTD SVG 1.1//EN" "http://www.w3.org/Graphics/SVG/1.1/DTD/svg11.dtd" [
  <!ENTITY ns_svg "http://www.w3.org/2000/svg">
  <!-- a comment with ]> in it -->
]>
"#,
            100,
        );
        assert_eq!(nesting_depth(&svg, usize::MAX), 101);
        let (png, _, _) = rasterize(&svg).unwrap();
        let img = image::load_from_memory(&png).unwrap().to_rgba8();
        assert!(img.pixels().any(|p| p[2] == 255 && p[3] == 255));
    }

    #[test]
    fn a_doctype_without_a_subset_still_counts_what_follows() {
        // Negative: a DOCTYPE ends at its first unquoted `>`.
        let svg = wrapped(r#"<!DOCTYPE svg SYSTEM "x>y.dtd">"#, 3);
        assert_eq!(nesting_depth(&svg, usize::MAX), 4);
    }

    #[test]
    fn invalid_svg_reports_an_error_instead_of_panicking() {
        let _ = rasterize(b"<svg"); // truncated: any Result, no panic
        assert!(rasterize(b"not xml at all").is_err());
        assert!(rasterize(b"").is_err());
    }
}
