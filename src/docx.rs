//! docx / odt read-only rich-text preview (#181): both formats are
//! zip + XML. The walker emits MARKDOWN from the document XML -
//! headings, emphasis, lists, tables, and embedded images (extracted
//! into the session scratch directory) - and the proven markdown
//! builder turns that into the preview's (lines, images) state, so
//! styling, wrapping, and the inline-image overlay all reuse. Nothing
//! edits or saves; fidelity is document STRUCTURE, not layout.

use quick_xml::events::Event;
use std::io::Read as _;
use std::path::{Path, PathBuf};

/// Source cap: these parse fully in memory. This bounds the DECOMPRESSED
/// bytes a member may yield, which is why it is generous.
pub const MAX_DOC_BYTES: u64 = 50 * 1024 * 1024;

/// Pre-parse cap on the FILE, checked before `ZipArchive::new` (#506).
///
/// A different limit from [`MAX_DOC_BYTES`], and conflating them is the bug
/// this fixes: that one bounds how much a member may DECOMPRESS to once the
/// archive is open, while the constructor's cost is the central-directory
/// parse it performs to open it at all. That parse allocates per member
/// before the entry count is knowable, so it cannot be bounded from the
/// inside.
///
/// A CRUDE PROXY, and the honest description of it. File size is not a
/// measure of parse cost: media is bytes without members, a crafted archive
/// is members without bytes. Measured against this 8 MB cap:
///
/// | shape | size | members | parse | verdict |
/// |---|---|---|---|---|
/// | crafted, empty members | 8.39 MB | 97,799 | 384 ms | ADMITTED |
/// | document of 600 KB photos | 12.3 MB | 21 | 0.19 ms | REFUSED |
///
/// The most expensive thing it admits costs about 2000x the cheapest thing
/// it blocks. It orders the two cases backwards: uncorrelated with cost
/// benignly, anti-correlated adversarially.
///
/// The benign threshold is a RANGE, not a number. Holding this cap fixed
/// and varying only the media size inside moves it 27x -- 165 members of
/// 50 KB images, 26 of 330 KB, 6 of 2 MB photographs. So do not read a
/// member count out of these rows and tune against it: any such figure
/// describes the fixture it came from, not the format.
///
/// The residual is explicit: a crafted archive of ~98k empty members fits
/// under this cap and still costs ~400 ms on the frame loop. Bounding the
/// real cost needs a member-count limit, which needs a partial
/// central-directory reader.
///
/// It stays at `ZIP_LIST_CAP` rather than being raised toward
/// [`MAX_DOC_BYTES`], although the rows above show size buys little. That
/// sibling's own doc accepts ~800 ms as its bounded worst case at this
/// value, so the ~400 ms this admits is INSIDE the budget #504 chose
/// deliberately for the same constructor. Raising this one would take the
/// same parse several times past that bound on one of its two routes,
/// break the alias that keeps them honest, and trade a recoverable refusal
/// for a longer freeze -- the direction the asymmetry below argues against.
///
/// It ships anyway because file size is the only thing checkable BEFORE the
/// call whose cost we are bounding: `ZipArchive::len()` exists, but only on
/// a constructed archive, so asking how many members there are costs
/// exactly what we are trying to avoid. Bounding the count properly needs a
/// partial central-directory reader, which is a design change rather than a
/// constant.
///
/// This is a TRADE, not an improvement. A large real document that opened
/// under the old 50 MB gate will now be refused. The justification is the
/// asymmetry of the two failures: a refused document is recoverable -- it
/// opens in the hex viewer and says why -- and a frame loop frozen inside
/// the parse is not.
///
/// Aliased to `archive::ZIP_LIST_CAP` rather than repeating its value: it
/// is the same parse reached by a second route, the editor's docx/odt
/// branch, which `croft view` (#362) makes reachable from any pane. One
/// definition means the two cannot drift.
pub const MAX_DOC_LISTING_BYTES: u64 = crate::archive::ZIP_LIST_CAP;

/// Whether the preview declined this file only because of its SIZE (#506),
/// as opposed to not recognising it. The cascade needs the distinction:
/// `archive::kind_from_ext` claims `.zip`/`.jar`/`.whl` and NOT `.docx`
/// or `.odt`, so an over-cap document does not reach the archive route
/// and cannot pick up that route's cap refusal. Without this it falls
/// past every viewer to hex with nothing said, which is the silent dump
/// #504 added `route_note` to prevent.
pub fn is_past_listing_cap(path: &Path) -> bool {
    extension_is_doc(
        path.extension()
            .and_then(|e| e.to_str())
            .unwrap_or_default(),
    ) && std::fs::metadata(path).is_ok_and(|m| m.len() > MAX_DOC_LISTING_BYTES)
}

pub fn extension_is_doc(ext: &str) -> bool {
    matches!(ext.to_ascii_lowercase().as_str(), "docx" | "odt")
}

/// Escape markdown metacharacters in document text (#200 review): the
/// walker SYNTHESISES markdown, so literal #, -, *, _, |, etc. in the
/// document must not become structure.
fn md_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if matches!(
            c,
            '\\' | '`' | '*' | '_' | '#' | '|' | '[' | ']' | '<' | '>' | '~'
        ) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

fn read_member(z: &mut zip::ZipArchive<std::fs::File>, name: &str) -> Option<String> {
    // Cap the DECOMPRESSED bytes (#200 review): the on-disk cap says
    // nothing about what a crafted member inflates to.
    let mut s = String::new();
    let member = z.by_name(name).ok()?;
    let mut limited = member.take(MAX_DOC_BYTES + 1);
    limited.read_to_string(&mut s).ok()?;
    (s.len() as u64 <= MAX_DOC_BYTES).then_some(s)
}

/// Extract an embedded image into `scratch` (hash-named) and return
/// its path.
fn extract_image(
    z: &mut zip::ZipArchive<std::fs::File>,
    member: &str,
    scratch: &Path,
) -> Option<PathBuf> {
    let mut bytes = Vec::new();
    let m = z.by_name(member).ok()?;
    let mut limited = m.take(MAX_DOC_BYTES + 1);
    limited.read_to_end(&mut bytes).ok()?;
    if bytes.len() as u64 > MAX_DOC_BYTES {
        return None;
    }
    image::load_from_memory(&bytes).ok()?;
    use std::hash::{Hash as _, Hasher as _};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut h);
    let ext = Path::new(member)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("png");
    let path = scratch.join(format!("doc-{:016x}.{ext}", h.finish()));
    if !path.is_file() {
        std::fs::create_dir_all(scratch).ok()?;
        std::fs::write(&path, &bytes).ok()?;
    }
    Some(path)
}

/// Convert the document into markdown text plus the scratch dir images
/// resolve against. `None` when the file is not a recognisable
/// docx/odt.
pub fn to_markdown(path: &Path, scratch: &Path) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    // Before the open, not after: the constructor below is where the
    // per-member central-directory cost is paid (#506).
    if meta.len() > MAX_DOC_LISTING_BYTES {
        return None;
    }
    let f = std::fs::File::open(path).ok()?;
    let mut z = zip::ZipArchive::new(f).ok()?;
    if let Some(xml) = read_member(&mut z, "word/document.xml") {
        let rels = read_member(&mut z, "word/_rels/document.xml.rels").unwrap_or_default();
        return Some(docx_to_md(&xml, &rels, &mut z, scratch));
    }
    if let Some(xml) = read_member(&mut z, "content.xml") {
        return Some(odt_to_md(&xml, &mut z, scratch));
    }
    None
}

/// Map r:embed relationship ids to media member names.
fn rel_targets(rels: &str) -> std::collections::HashMap<String, String> {
    let mut out = std::collections::HashMap::new();
    let mut r = quick_xml::Reader::from_str(rels);
    let dec = r.decoder();
    loop {
        match r.read_event() {
            Ok(Event::Empty(e)) | Ok(Event::Start(e))
                if e.local_name().as_ref() == b"Relationship" =>
            {
                let mut id = None;
                let mut target = None;
                for a in e.attributes().flatten() {
                    match a.key.local_name().as_ref() {
                        b"Id" => {
                            id = a
                                .decode_and_unescape_value(dec)
                                .ok()
                                .map(|v| v.into_owned())
                        }
                        b"Target" => {
                            target = a
                                .decode_and_unescape_value(dec)
                                .ok()
                                .map(|v| v.into_owned())
                        }
                        _ => {}
                    }
                }
                if let (Some(id), Some(t)) = (id, target) {
                    out.insert(id, format!("word/{}", t.trim_start_matches("./")));
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    out
}

fn docx_to_md(
    xml: &str,
    rels: &str,
    z: &mut zip::ZipArchive<std::fs::File>,
    scratch: &Path,
) -> String {
    let targets = rel_targets(rels);
    let mut md = String::new();
    let mut r = quick_xml::Reader::from_str(xml);
    let dec = r.decoder();
    let mut para = String::new();
    let mut heading = 0usize;
    let mut listed = false;
    let mut bold = false;
    let mut italic = false;
    let mut in_rpr = false;
    let mut table: Option<Vec<Vec<String>>> = None;
    while let Ok(ev) = r.read_event() {
        match &ev {
            Event::Start(e) | Event::Empty(e) => match e.local_name().as_ref() {
                b"p" => {
                    para.clear();
                    heading = 0;
                    listed = false;
                }
                b"rPr" => in_rpr = true,
                b"b" if in_rpr => bold = true,
                b"i" if in_rpr => italic = true,
                b"pStyle" => {
                    for a in e.attributes().flatten() {
                        if a.key.local_name().as_ref() == b"val"
                            && let Ok(v) = a.decode_and_unescape_value(dec)
                            && let Some(n) = v.strip_prefix("Heading")
                        {
                            heading = n.parse().unwrap_or(0);
                        }
                    }
                }
                b"numPr" => listed = true,
                b"tbl" => table = Some(Vec::new()),
                b"tr" => {
                    if let Some(t) = table.as_mut() {
                        t.push(Vec::new());
                    }
                }
                b"tc" => {
                    if let Some(row) = table.as_mut().and_then(|t| t.last_mut()) {
                        row.push(String::new());
                    }
                }
                b"blip" => {
                    for a in e.attributes().flatten() {
                        if a.key.local_name().as_ref() == b"embed"
                            && let Ok(id) = a.decode_and_unescape_value(dec)
                            && let Some(member) = targets.get(id.as_ref())
                            && let Some(p) = extract_image(z, member, scratch)
                        {
                            para.push_str(&format!("![image]({})", p.display()));
                        }
                    }
                }
                _ => {}
            },
            Event::End(e) => match e.local_name().as_ref() {
                b"rPr" => in_rpr = false,
                b"r" => {
                    bold = false;
                    italic = false;
                }
                b"p" => {
                    let text = para.trim();
                    if let Some(row) = table
                        .as_mut()
                        .and_then(|t| t.last_mut())
                        .and_then(|r| r.last_mut())
                    {
                        if !text.is_empty() {
                            if !row.is_empty() {
                                row.push(' ');
                            }
                            row.push_str(text);
                        }
                    } else if !text.is_empty() {
                        if heading > 0 {
                            md.push_str(&"#".repeat(heading.min(6)));
                            md.push(' ');
                        } else if listed {
                            md.push_str("- ");
                        }
                        md.push_str(text);
                        md.push_str("\n\n");
                    }
                    para.clear();
                }
                b"tbl" => {
                    if let Some(rows) = table.take()
                        && !rows.is_empty()
                    {
                        let cols = rows.iter().map(|r| r.len()).max().unwrap_or(0);
                        for (i, row) in rows.iter().enumerate() {
                            md.push('|');
                            for c in 0..cols {
                                md.push_str(row.get(c).map(String::as_str).unwrap_or(""));
                                md.push('|');
                            }
                            md.push('\n');
                            if i == 0 {
                                md.push('|');
                                for _ in 0..cols {
                                    md.push_str("---|");
                                }
                                md.push('\n');
                            }
                        }
                        md.push('\n');
                    }
                }
                _ => {}
            },
            Event::Text(t) => {
                if let Ok(text) = t.xml_content() {
                    let text = md_escape(&text);
                    let styled = if bold && italic {
                        format!("***{text}***")
                    } else if bold {
                        format!("**{text}**")
                    } else if italic {
                        format!("*{text}*")
                    } else {
                        text
                    };
                    para.push_str(&styled);
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    md
}

fn odt_to_md(xml: &str, z: &mut zip::ZipArchive<std::fs::File>, scratch: &Path) -> String {
    let mut md = String::new();
    let mut r = quick_xml::Reader::from_str(xml);
    let dec = r.decoder();
    let mut para = String::new();
    let mut heading = 0usize;
    let mut list_depth = 0usize;
    while let Ok(ev) = r.read_event() {
        match &ev {
            Event::Start(e) | Event::Empty(e) => match e.local_name().as_ref() {
                b"h" => {
                    heading = 1;
                    for a in e.attributes().flatten() {
                        if a.key.local_name().as_ref() == b"outline-level"
                            && let Ok(v) = a.decode_and_unescape_value(dec)
                        {
                            heading = v.parse().unwrap_or(1);
                        }
                    }
                    para.clear();
                }
                b"p" => para.clear(),
                b"list" if matches!(&ev, Event::Start(_)) => list_depth += 1,
                b"image" => {
                    for a in e.attributes().flatten() {
                        if a.key.local_name().as_ref() == b"href"
                            && let Ok(v) = a.decode_and_unescape_value(dec)
                            && let Some(p) = extract_image(z, v.as_ref(), scratch)
                        {
                            para.push_str(&format!("![image]({})", p.display()));
                        }
                    }
                }
                _ => {}
            },
            Event::End(e) => match e.local_name().as_ref() {
                b"h" => {
                    if !para.trim().is_empty() {
                        md.push_str(&"#".repeat(heading.clamp(1, 6)));
                        md.push(' ');
                        md.push_str(para.trim());
                        md.push_str("\n\n");
                    }
                    heading = 0;
                    para.clear();
                }
                b"p" => {
                    if !para.trim().is_empty() {
                        if list_depth > 0 {
                            md.push_str("- ");
                        }
                        md.push_str(para.trim());
                        md.push_str("\n\n");
                    }
                    para.clear();
                }
                b"list" => list_depth = list_depth.saturating_sub(1),
                _ => {}
            },
            Event::Text(t) => {
                if let Ok(text) = t.xml_content() {
                    para.push_str(&md_escape(&text));
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    md
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    fn docx_fixture(p: &Path) {
        let f = std::fs::File::create(p).unwrap();
        let mut z = zip::ZipWriter::new(f);
        let o = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        z.start_file("word/document.xml", o).unwrap();
        z.write_all(
            br#"<?xml version="1.0"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
<w:body>
<w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>Title Here</w:t></w:r></w:p>
<w:p><w:r><w:t>plain </w:t></w:r><w:r><w:rPr><w:b/></w:rPr><w:t>bold</w:t></w:r></w:p>
<w:p><w:pPr><w:numPr/></w:pPr><w:r><w:t>item one</w:t></w:r></w:p>
<w:tbl><w:tr><w:tc><w:p><w:r><w:t>h1</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>h2</w:t></w:r></w:p></w:tc></w:tr>
<w:tr><w:tc><w:p><w:r><w:t>a</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>b</w:t></w:r></w:p></w:tc></w:tr></w:tbl>
</w:body></w:document>"#,
        )
        .unwrap();
        z.finish().unwrap();
    }

    #[test]
    fn docx_walks_headings_emphasis_lists_and_tables_into_markdown() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("d.docx");
        docx_fixture(&p);
        let md = to_markdown(&p, tmp.path()).expect("recognised");
        assert!(md.contains("# Title Here"), "{md}");
        assert!(md.contains("plain **bold**"), "{md}");
        assert!(md.contains("- item one"), "{md}");
        assert!(md.contains("|h1|h2|"), "{md}");
        assert!(md.contains("|a|b|"), "{md}");
    }

    /// A document whose FILE size clears the listing cap is refused before
    /// `ZipArchive::new` runs, so the crafted many-member layout cannot put
    /// its central directory through the constructor (#506). The fixture is
    /// a valid docx -- it holds a real `word/document.xml` -- so what the
    /// cap refuses is the size, not the shape.
    #[test]
    fn a_document_past_the_listing_cap_is_refused_before_the_parse() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("big.docx");
        docx_fixture(&p);
        // Pad past the cap without building a real many-member archive:
        // the gate reads the file's length, which is what a crafted
        // central directory inflates.
        let f = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
        f.set_len(MAX_DOC_LISTING_BYTES + 1).unwrap();
        // Both bounds, so the refusal can only be the NEW cap. Past the
        // listing cap says the gate under test should fire; still under
        // `MAX_DOC_BYTES` says the old gate cannot be what fired instead.
        // Without the second, raising the listing cap above 50 MB would
        // leave this test green while the old bound did the work -- passing
        // for a reason it does not claim.
        let len = std::fs::metadata(&p).unwrap().len();
        assert!(len > MAX_DOC_LISTING_BYTES, "fixture clears the new cap");
        assert!(
            len <= MAX_DOC_BYTES,
            "and stays under the old one, so only the new cap can refuse it"
        );
        assert!(
            to_markdown(&p, tmp.path()).is_none(),
            "a file past the listing cap must not reach the parse"
        );
        // And the same fixture under the cap still opens, so the refusal
        // above is the size and not the padding.
        let q = tmp.path().join("small.docx");
        docx_fixture(&q);
        assert!(
            std::fs::metadata(&q).unwrap().len() <= MAX_DOC_LISTING_BYTES,
            "fixture premise: the unpadded document is under the cap"
        );
        assert!(to_markdown(&q, tmp.path()).is_some(), "under the cap opens");
    }

    /// An over-cap document does NOT reach the archive route, and that is
    /// why the size refusal has to be named here (#506).
    /// `archive::kind_from_ext` claims `.zip`/`.jar`/`.whl` and not
    /// `.docx`/`.odt`, so the cascade's archive branch is skipped and the
    /// file would otherwise fall to hex with no reason shown. Pinning the
    /// premise here means a later widening of `kind_from_ext` fails this
    /// test rather than silently making the message redundant.
    #[test]
    fn an_over_cap_document_needs_its_own_refusal_because_the_archive_route_skips_it() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("big.docx");
        docx_fixture(&p);
        let f = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
        f.set_len(MAX_DOC_LISTING_BYTES + 1).unwrap();

        assert!(
            to_markdown(&p, tmp.path()).is_none(),
            "the preview declines"
        );
        assert!(
            crate::archive::kind_from_ext(&p).is_none(),
            "premise: the archive route does not claim a .docx, so it cannot \
             supply the refusal reason on this path"
        );
        // Control: the same call DOES claim a plain zip, so the assertion
        // above is about .docx and not a broken call.
        assert!(
            crate::archive::kind_from_ext(&tmp.path().join("x.zip")).is_some(),
            "control: kind_from_ext claims a .zip"
        );
        assert!(
            is_past_listing_cap(&p),
            "so the size refusal must be named by the doc path itself"
        );
    }

    /// The listing cap must stay the tighter of the two (#506). A
    /// `const` block rather than a runtime test: both are compile-time
    /// constants, so an assertion on them has a constant value, which
    /// clippy rejects and which would never have run as a check anyway --
    /// it fails the BUILD if the relationship is ever inverted, which is
    /// strictly earlier than a test could.
    ///
    /// The equality to `archive::ZIP_LIST_CAP` that stood here is gone
    /// deliberately: `MAX_DOC_LISTING_BYTES` is now defined AS that
    /// constant, so asserting they match compares a thing to itself and
    /// can never fail. The alias is what enforces it.
    const _: () = assert!(
        MAX_DOC_LISTING_BYTES < MAX_DOC_BYTES,
        "the pre-parse cap must be tighter than the decompressed-member cap"
    );

    #[test]
    fn odt_walks_headings_and_paragraphs() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("d.odt");
        let f = std::fs::File::create(&p).unwrap();
        let mut z = zip::ZipWriter::new(f);
        let o = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        z.start_file("content.xml", o).unwrap();
        z.write_all(
            br#"<?xml version="1.0"?>
<office:document-content xmlns:office="o" xmlns:text="t">
<office:body><office:text>
<text:h text:outline-level="2">Sub Head</text:h>
<text:p>body words</text:p>
<text:list><text:list-item><text:p>li</text:p></text:list-item></text:list>
</office:text></office:body></office:document-content>"#,
        )
        .unwrap();
        z.finish().unwrap();
        let md = to_markdown(&p, tmp.path()).expect("recognised");
        assert!(md.contains("## Sub Head"), "{md}");
        assert!(md.contains("body words"), "{md}");
        assert!(md.contains("- li"), "{md}");
    }

    #[test]
    fn literal_metacharacters_stay_text_not_structure() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("d.docx");
        let f = std::fs::File::create(&p).unwrap();
        let mut z = zip::ZipWriter::new(f);
        let o = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        z.start_file("word/document.xml", o).unwrap();
        z.write_all(
            br#"<w:document xmlns:w="x"><w:body>
<w:p><w:r><w:t># not a heading</w:t></w:r></w:p>
<w:tbl><w:tr><w:tc><w:p><w:r><w:t>a|b</w:t></w:r></w:p></w:tc></w:tr></w:tbl>
</w:body></w:document>"#,
        )
        .unwrap();
        z.finish().unwrap();
        let md = to_markdown(&p, tmp.path()).unwrap();
        assert!(md.contains("\\# not a heading"), "{md}");
        assert!(md.contains("a\\|b"), "pipes stay inside the cell: {md}");
    }

    #[test]
    fn a_plain_zip_is_not_a_document() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("x.zip");
        let f = std::fs::File::create(&p).unwrap();
        let mut z = zip::ZipWriter::new(f);
        let o = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        z.start_file("readme.txt", o).unwrap();
        z.write_all(b"x").unwrap();
        z.finish().unwrap();
        assert!(to_markdown(&p, tmp.path()).is_none());
    }
}
