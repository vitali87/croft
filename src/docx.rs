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

/// Source cap: these parse fully in memory.
pub const MAX_DOC_BYTES: u64 = 50 * 1024 * 1024;

pub fn extension_is_doc(ext: &str) -> bool {
    matches!(ext.to_ascii_lowercase().as_str(), "docx" | "odt")
}

fn read_member(z: &mut zip::ZipArchive<std::fs::File>, name: &str) -> Option<String> {
    let mut s = String::new();
    z.by_name(name).ok()?.read_to_string(&mut s).ok()?;
    Some(s)
}

/// Extract an embedded image into `scratch` (hash-named) and return
/// its path.
fn extract_image(
    z: &mut zip::ZipArchive<std::fs::File>,
    member: &str,
    scratch: &Path,
) -> Option<PathBuf> {
    let mut bytes = Vec::new();
    z.by_name(member).ok()?.read_to_end(&mut bytes).ok()?;
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
    if meta.len() > MAX_DOC_BYTES {
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
                    let styled = if bold && italic {
                        format!("***{text}***")
                    } else if bold {
                        format!("**{text}**")
                    } else if italic {
                        format!("*{text}*")
                    } else {
                        text.into_owned()
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
                b"list" => list_depth += 1,
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
                    para.push_str(&text);
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
